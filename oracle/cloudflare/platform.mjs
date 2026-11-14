import { execFileSync, spawn } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, openSync, readdirSync, writeFileSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKERD = join(HERE, "node_modules", ".bin", "workerd");
const OBJECT_MODULE = join(HERE, "object.mjs");
const COMPATIBILITY_DATE = "2026-11-01";
const UNIQUE_KEY = "harnesslang-drill";
const METADATA_FILE = "metadata.sqlite";
const ALARM_METADATA_KEY = 1;
const SESSION_NAME = "drill";
const LOOPBACK = "127.0.0.1";
const READY_POLL_MS = 10;
const READY_WAIT_MS_MAX = 30000;
const SQLITE_OUTPUT_BYTES_MAX = 1 << 30;
const NO_SUCH_TABLE = "no such table";
const UNCAUGHT_HEADER = "hl-drill-uncaught";

class Failure extends Error {
  constructor(invariant, detail) {
    super(detail);
    this.invariant = invariant;
  }
}

function assert(condition, message) {
  if (!condition) throw new Error(`drill assertion failed: ${message}`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, LOOPBACK, () => {
      const port = server.address().port;
      server.close(() => resolve(port));
    });
  });
}

function connects(port) {
  return new Promise((resolve) => {
    const socket = net.connect(port, LOOPBACK);
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("error", () => resolve(false));
  });
}

function capnpText(text) {
  assert(!/["\\\n]/.test(text), "a config text needs no escaping");
  return `"${text}"`;
}

function post(port, path, body) {
  return new Promise((resolve, reject) => {
    const request = http.request({ host: LOOPBACK, port, path, method: "POST", agent: false, headers: { "content-type": "application/json" } }, (response) => {
      let text = "";
      response.setEncoding("utf8");
      response.on("data", (chunk) => (text += chunk));
      response.on("end", () => resolve({ status: response.statusCode, headers: response.headers, body: text }));
      response.on("error", reject);
    });
    request.on("error", reject);
    request.end(body);
  });
}

class Platform {
  constructor(dir, module, hostPort) {
    this.dir = dir;
    this.storage = join(dir, "storage");
    this.hostPort = hostPort;
    this.port = null;
    this.child = null;
    this.exited = null;
    this.starts = 0;
    this.kills = 0;
    mkdirSync(this.storage, { recursive: true });
    copyFileSync(module, join(dir, "worker.mjs"));
    copyFileSync(OBJECT_MODULE, join(dir, "object.mjs"));
  }

  config(fault, worldUrl) {
    const host = `http://${LOOPBACK}:${this.hostPort}`;
    return [
      'using Workerd = import "/workerd/workerd.capnp";',
      "const config :Workerd.Config = (",
      "  services = [",
      "    (name = \"main\", worker = .harness),",
      '    (name = "internet", network = (allow = ["public", "private", "local"])),',
      `    (name = "storage", disk = (path = ${capnpText(this.storage)}, writable = true)),`,
      "  ],",
      `  sockets = [(name = "http", address = ${capnpText(`${LOOPBACK}:${this.port}`)}, http = (), service = "main")],`,
      ");",
      "const harness :Workerd.Worker = (",
      '  modules = [(name = "object.mjs", esModule = embed "object.mjs"), (name = "worker.mjs", esModule = embed "worker.mjs")],',
      `  compatibilityDate = ${capnpText(COMPATIBILITY_DATE)},`,
      `  durableObjectNamespaces = [(className = "HarnessObject", uniqueKey = ${capnpText(UNIQUE_KEY)}, enableSql = true)],`,
      '  durableObjectStorage = (localDisk = "storage"),',
      "  bindings = [",
      '    (name = "HARNESS", durableObjectNamespace = "HarnessObject"),',
      `    (name = "HL_TOOLS_URL", text = ${capnpText(`${host}/tool`)}),`,
      `    (name = "HL_MODEL_URL", text = ${capnpText(`${host}/model`)}),`,
      `    (name = "HL_DRILL_WORLD_URL", text = ${capnpText(worldUrl ? host : "")}),`,
      `    (name = "HL_DRILL_FAULT", text = ${capnpText(fault)}),`,
      "  ],",
      ");",
      "",
    ].join("\n");
  }

  alive() {
    return this.child !== null;
  }

  async start(fault, worldUrl) {
    assert(this.child === null, "one workerd runs per drill session");
    if (this.exited !== null) await this.exited;
    this.port ??= await freePort();
    writeFileSync(join(this.dir, "config.capnp"), this.config(fault, worldUrl));
    const log = openSync(join(this.dir, "workerd.log"), "a");
    const child = spawn(WORKERD, ["serve", "config.capnp", "--experimental"], { cwd: this.dir, stdio: ["ignore", log, log] });
    this.exited = new Promise((resolve) => child.once("exit", (code, signal) => resolve({ code, signal })));
    this.child = child;
    this.starts += 1;
    const started = Date.now();
    while (!(await connects(this.port))) {
      if (Date.now() - started > READY_WAIT_MS_MAX) throw new Failure("liveness", `workerd did not listen within READY_WAIT_MS_MAX (${READY_WAIT_MS_MAX} ms)`);
      await sleep(READY_POLL_MS);
    }
  }

  async kill() {
    if (this.child === null) return false;
    const child = this.child;
    this.child = null;
    child.kill("SIGKILL");
    await this.exited;
    this.kills += 1;
    return true;
  }

  objectFile() {
    const dir = join(this.storage, UNIQUE_KEY);
    if (!existsSync(dir)) return null;
    const files = readdirSync(dir).filter((name) => name.endsWith(".sqlite") && name !== METADATA_FILE);
    assert(files.length <= 1, "a drill session has one object");
    return files.length === 0 ? null : join(dir, files[0]);
  }

  query(sql) {
    const file = this.objectFile();
    if (file === null) return [];
    let output;
    try {
      output = execFileSync("sqlite3", ["-json", "-readonly", file, sql], { encoding: "utf8", maxBuffer: SQLITE_OUTPUT_BYTES_MAX, stdio: ["ignore", "pipe", "pipe"] });
    } catch (error) {
      if (String(error.stderr).includes(NO_SUCH_TABLE)) return [];
      throw error;
    }
    return output.trim() === "" ? [] : JSON.parse(output);
  }

  rows() {
    return this.query("SELECT position, record FROM hl_journal ORDER BY position");
  }

  alarm() {
    const rows = this.query(`SELECT value FROM _cf_METADATA WHERE key = ${ALARM_METADATA_KEY}`);
    return rows.length === 1 && rows[0].value !== null;
  }

  turn(body) {
    assert(this.port !== null, "a turn is posted to a started workerd");
    return post(this.port, `/sessions/${SESSION_NAME}/turn`, body);
  }
}

function startHost(handle) {
  return new Promise((resolve) => {
    const server = http.createServer((request, response) => {
      let body = "";
      request.setEncoding("utf8");
      request.on("data", (chunk) => (body += chunk));
      request.on("end", async () => {
        const answer = await handle(request.url, body);
        if (answer === null) {
          response.destroy();
          return;
        }
        response.writeHead(200, { "content-type": "application/json" });
        response.end(answer);
      });
    });
    server.listen(0, LOOPBACK, () => resolve({ server, port: server.address().port }));
  });
}

export { Failure, Platform, UNCAUGHT_HEADER, assert, sleep, startHost };
