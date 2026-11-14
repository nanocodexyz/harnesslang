#!/usr/bin/env node
import { execFileSync, spawn } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKERD = join(HERE, "node_modules", ".bin", "workerd");
const COMPATIBILITY_DATE = "2026-11-01";
const UNIQUE_KEY = "harnesslang-cell-facts";
const LOOPBACK = "127.0.0.1";
const READY_POLL_MS = 10;
const READY_WAIT_MS_MAX = 30000;
const SUBREQUESTS_PAID_DEFAULT = 10000;
const RPC_CALLS = SUBREQUESTS_PAID_DEFAULT + 50;
const NO_INTERNET = "This worker is not permitted to access the internet via global functions like fetch()";
const NAMESPACE_NOT_SERIALIZABLE = 'Could not serialize object of type "DurableObjectNamespace"';
const TARGET_NOT_SERIALIZABLE = "Remote RPC references can only be serialized for RPC";
const EXPERIMENTAL_REQUIRED = "Worker loader bindings are an experimental feature";
const NESTED_CHARGES = 20;
const QUIET_MS = 300;

const { values } = parseArgs({ options: { dir: { type: "string" } } });
if (!values.dir) {
  console.error("usage: node loader.mjs --dir DIR");
  process.exit(2);
}
const dir = resolve(values.dir);
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
const seen = [];
let log = "";
const failures = [];

function freePort() {
  return new Promise((done, fail) => {
    const server = net.createServer();
    server.once("error", fail);
    server.listen(0, LOOPBACK, () => {
      const port = server.address().port;
      server.close(() => done(port));
    });
  });
}

function journalRows() {
  const folder = join(dir, "storage", UNIQUE_KEY);
  if (!existsSync(folder)) return [];
  const file = readdirSync(folder).find((name) => name.endsWith(".sqlite") && name !== "metadata.sqlite");
  if (file === undefined) return [];
  const output = execFileSync("sqlite3", ["-json", "-readonly", join(folder, file), "SELECT seq, entry FROM cell_journal ORDER BY seq"], { encoding: "utf8" });
  return output.trim() === "" ? [] : JSON.parse(output);
}

function startHost() {
  return new Promise((done) => {
    const server = http.createServer((request, response) => {
      let body = "";
      request.on("data", (chunk) => (body += chunk));
      request.on("end", () => {
        const rows = journalRows();
        seen.push({ path: request.url, body, rows: rows.length, entries: rows.map((row) => JSON.parse(row.entry)) });
        response.end(request.url === "/charge" ? `charged:${JSON.parse(body).seq}` : "escaped");
      });
    });
    server.listen(0, LOOPBACK, () => done({ server, port: server.address().port }));
  });
}

function config(port, hostPort) {
  return [
    'using Workerd = import "/workerd/workerd.capnp";',
    "const config :Workerd.Config = (",
    "  services = [",
    '    (name = "main", worker = .cells),',
    '    (name = "internet", network = (allow = ["public", "private", "local"])),',
    '    (name = "storage", disk = (path = "storage", writable = true)),',
    "  ],",
    `  sockets = [(name = "http", address = "${LOOPBACK}:${port}", http = (), service = "main")],`,
    ");",
    "const cells :Workerd.Worker = (",
    '  modules = [(name = "loader-object.mjs", esModule = embed "loader-object.mjs")],',
    `  compatibilityDate = "${COMPATIBILITY_DATE}",`,
    `  durableObjectNamespaces = [(className = "CellObject", uniqueKey = "${UNIQUE_KEY}", enableSql = true)],`,
    '  durableObjectStorage = (localDisk = "storage"),',
    "  bindings = [",
    '    (name = "CELL", durableObjectNamespace = "CellObject"),',
    '    (name = "LOADER", workerLoader = (id = "cells")),',
    `    (name = "HOST_URL", text = "http://${LOOPBACK}:${hostPort}"),`,
    "  ],",
    ");",
    "",
  ].join("\n");
}

async function run(port, query, code) {
  let response;
  try {
    response = await fetch(`http://${LOOPBACK}:${port}/?${query}`, { method: "POST", body: code });
  } catch (error) {
    return { status: "no response", error: String(error?.cause?.message ?? error?.message ?? error) };
  }
  const text = await response.text();
  if (response.status !== 200) return { status: response.status, error: text };
  return JSON.parse(text);
}

function expect(fact, condition, detail) {
  console.log(`${condition ? "fact" : "FAIL"} ${fact}: ${detail}`);
  if (!condition) failures.push(fact);
}

async function facts(port, hostPort) {
  const namespace = await run(port, "id=namespace&via=namespace", "return await tools.count();");
  const target = await run(port, "id=target&via=target", "return await tools.count();");
  const stub = await run(port, "id=stub&via=stub", "return await tools.count();");
  const exported = await run(port, "id=exports&via=exports", "return await tools.count();");
  expect("namespace_binding_refused", namespace.status === 500 && log.includes(NAMESPACE_NOT_SERIALIZABLE), `a DurableObjectNamespace in WorkerCode.env is refused: ${NAMESPACE_NOT_SERIALIZABLE}`);
  expect("rpc_target_refused", target.status === 500 && log.includes(TARGET_NOT_SERIALIZABLE), `an RpcTarget of the object in WorkerCode.env is refused: ${TARGET_NOT_SERIALIZABLE}`);
  expect("object_stub_and_entrypoint_accepted", stub.ok === 1 && exported.ok === 2, `an object stub and a ctx.exports entrypoint in WorkerCode.env reach the object: ${JSON.stringify(stub)} ${JSON.stringify(exported)}`);

  const charge = await run(port, "id=charge&via=stub", 'return await tools.charge("order-1");');
  const chargeSeen = seen.find((request) => request.path === "/charge");
  expect("rpc_into_object", charge.ok === "charged:1", `a cell's tools.charge RPC ran in the object and returned ${JSON.stringify(charge)}`);
  expect("gate_holds_nested_outbound", chargeSeen !== undefined && chargeSeen.entries.some((entry) => entry.call === "charge"), `the object's fetch for the nested call reached the host with ${chargeSeen?.rows} committed journal rows: ${JSON.stringify(chargeSeen?.entries)}`);

  const blocked = await run(port, "id=null-fetch&via=stub", `await fetch(host + "/escape-null"); return "sent";`);
  await sleep(QUIET_MS);
  expect("null_outbound_blocks_fetch", String(blocked.error).includes(NO_INTERNET) && !seen.some((request) => request.path === "/escape-null"), `fetch from a globalOutbound: null isolate: ${JSON.stringify(blocked)}`);
  const socket = await run(port, "id=null-connect&via=stub", `const { connect } = await import("cloudflare:sockets"); const s = connect("${LOOPBACK}:${hostPort}"); await s.opened; return "opened";`);
  expect("null_outbound_blocks_connect", String(socket.error).includes(NO_INTERNET), `connect() from a globalOutbound: null isolate: ${JSON.stringify(socket)}`);
  const inherit = await run(port, "id=inherit&via=stub&outbound=inherit", `await fetch(host + "/escape-inherit"); return "sent";`);
  expect("omitted_outbound_inherits_network", inherit.ok === "sent" && seen.some((request) => request.path === "/escape-inherit"), `a loaded isolate without globalOutbound reaches the network: ${JSON.stringify(inherit)}`);

  const before = seen.filter((request) => request.path === "/charge").length;
  const nested = await run(port, "id=nested&via=stub", `const outputs = []; for (let i = 0; i < ${NESTED_CHARGES}; i++) outputs.push(await tools.charge("n" + i)); return outputs.length;`);
  const charges = seen.filter((request) => request.path === "/charge").slice(before);
  const allCommitted = charges.every((request) => request.entries.some((entry) => entry.call === "charge" && entry.input === JSON.parse(request.body).input));
  expect("gate_holds_every_nested_outbound", nested.ok === NESTED_CHARGES && charges.length === NESTED_CHARGES && allCommitted, `${charges.length} nested charges, each reaching the host after its call row committed: ${allCommitted}`);

  for (const via of ["stub", "exports"]) {
    const started = performance.now();
    const volume = await run(port, `id=volume-${via}&via=${via}`, `for (let i = 0; i < ${RPC_CALLS}; i++) await tools.count(); return ${RPC_CALLS};`);
    const elapsed = performance.now() - started;
    expect(`rpc_volume_${via}`, volume.ok === RPC_CALLS && volume.rpc_calls === RPC_CALLS, `${RPC_CALLS} RPC calls (over the Paid plan's default of ${SUBREQUESTS_PAID_DEFAULT} subrequests) from the cell into the object through ${via} completed in ${Math.round(elapsed)} ms (${((elapsed * 1000) / RPC_CALLS).toFixed(1)} us each): local workerd does not enforce a subrequest limit`);
  }

  const isolated = await run(port, "id=isolated&via=stub", "globalThis.cellMarker = 1; return typeof globalThis.cellMarker;");
  expect("separate_global_scope", isolated.ok === "number" && isolated.isolate_global === null, `a global set in the cell is not seen by the object: ${JSON.stringify(isolated)}`);
}

async function serve(host, experimental) {
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(join(dir, "storage"), { recursive: true });
  copyFileSync(join(HERE, "loader-object.mjs"), join(dir, "loader-object.mjs"));
  const port = await freePort();
  writeFileSync(join(dir, "config.capnp"), config(port, host.port));
  const args = ["serve", "config.capnp", ...(experimental ? ["--experimental"] : [])];
  log = "";
  const child = spawn(WORKERD, args, { cwd: dir, stdio: ["ignore", "pipe", "pipe"] });
  child.stdout.on("data", (chunk) => (log += chunk));
  child.stderr.on("data", (chunk) => (log += chunk));
  const exited = new Promise((done) => child.once("exit", (code) => done(code)));
  const started = Date.now();
  let ready = false;
  while (!ready && child.exitCode === null && Date.now() - started < READY_WAIT_MS_MAX) {
    ready = await new Promise((done) => {
      const socket = net.connect(port, LOOPBACK);
      socket.once("connect", () => {
        socket.destroy();
        done(true);
      });
      socket.once("error", () => done(false));
    });
    if (!ready) await sleep(READY_POLL_MS);
  }
  const stop = async () => {
    child.kill("SIGKILL");
    await exited;
  };
  return { port, ready, args, stop };
}

async function main() {
  const host = await startHost();
  const version = execFileSync(WORKERD, ["--version"], { encoding: "utf8" }).trim();
  try {
    const plain = await serve(host, false);
    await sleep(QUIET_MS);
    await plain.stop();
    expect("worker_loader_needs_experimental", log.includes(EXPERIMENTAL_REQUIRED), `${version} ${plain.args.join(" ")}: ${log.trim().split("\n").find((line) => line.includes(EXPERIMENTAL_REQUIRED)) ?? log.trim().slice(0, 200)}`);
    const served = await serve(host, true);
    console.log(`loader facts on ${version}, ${served.args.join(" ")}, compatibilityDate ${COMPATIBILITY_DATE}`);
    try {
      expect("worker_loader_available", served.ready, `workerd listens with a workerLoader binding: ${served.ready}`);
      if (served.ready) await facts(served.port, host.port);
    } finally {
      await served.stop();
    }
  } finally {
    host.server.close();
  }
  if (failures.length > 0) {
    console.log(log.trim().split("\n").slice(0, 12).join("\n"));
    console.log(`loader facts: FAILED ${failures.join(", ")}`);
    process.exit(1);
  }
  console.log("loader facts: ok");
}

await main();
