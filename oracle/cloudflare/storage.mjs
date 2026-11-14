#!/usr/bin/env node
import { execFileSync, spawn } from "node:child_process";
import { copyFileSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { WORKERD, objectFile, readRows } from "./local.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const COMPATIBILITY_DATE = "2026-11-01";
const UNIQUE_KEY = "harnesslang-storage-facts";
const LOOPBACK = "127.0.0.1";
const READY_POLL_MS = 10;
const READY_WAIT_MS_MAX = 30000;
const MARKER_WAIT_MS_MAX = 30000;
const SPIN_MARKER = "hl-storage spin ";
const ALARM_MARKER = "hl-storage alarm";
const ALARM_DELAY_MS = 1000;
const GATE_TRIALS = 10;
const GATE_ROWS = 2000;
const GATE_ROW_BYTES = 1024;
const SPIN_TRIALS = 3;

const { values } = parseArgs({ options: { dir: { type: "string" } } });
if (!values.dir) {
  console.error("usage: node storage.mjs --dir DIR");
  process.exit(2);
}
const root = resolve(values.dir);
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
const failures = [];
const host = { handle: null };

function expect(fact, condition, detail) {
  console.log(`${condition ? "fact" : "FAIL"} ${fact}: ${detail}`);
  if (!condition) failures.push(fact);
}

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

function connects(port) {
  return new Promise((done) => {
    const socket = net.connect(port, LOOPBACK);
    socket.once("connect", () => {
      socket.destroy();
      done(true);
    });
    socket.once("error", () => done(false));
  });
}

function rows(dir, sql) {
  const file = objectFile(join(dir, "storage", UNIQUE_KEY));
  return file === null ? [] : readRows(file, sql);
}

function facts(dir) {
  return rows(dir, "SELECT what FROM facts ORDER BY id").map((row) => row.what);
}

function startHost() {
  return new Promise((done) => {
    const server = http.createServer((request, response) => {
      let body = "";
      request.on("data", (chunk) => (body += chunk));
      request.on("end", async () => response.end(await host.handle(request.url, body)));
    });
    server.listen(0, LOOPBACK, () => done({ server, port: server.address().port }));
  });
}

function config(port, hostPort) {
  return [
    'using Workerd = import "/workerd/workerd.capnp";',
    "const config :Workerd.Config = (",
    "  services = [",
    '    (name = "main", worker = .facts),',
    '    (name = "internet", network = (allow = ["public", "private", "local"])),',
    '    (name = "storage", disk = (path = "storage", writable = true)),',
    "  ],",
    `  sockets = [(name = "http", address = "${LOOPBACK}:${port}", http = (), service = "main")],`,
    ");",
    "const facts :Workerd.Worker = (",
    '  modules = [(name = "storage-object.mjs", esModule = embed "storage-object.mjs")],',
    `  compatibilityDate = "${COMPATIBILITY_DATE}",`,
    `  durableObjectNamespaces = [(className = "FactObject", uniqueKey = "${UNIQUE_KEY}", enableSql = true)],`,
    '  durableObjectStorage = (localDisk = "storage"),',
    "  bindings = [",
    '    (name = "FACTS", durableObjectNamespace = "FactObject"),',
    `    (name = "HOST_URL", text = "http://${LOOPBACK}:${hostPort}"),`,
    "  ],",
    ");",
    "",
  ].join("\n");
}

function prepare(name) {
  const dir = join(root, name);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(join(dir, "storage"), { recursive: true });
  copyFileSync(join(HERE, "storage-object.mjs"), join(dir, "storage-object.mjs"));
  return dir;
}

async function serve(dir, hostPort) {
  const port = await freePort();
  writeFileSync(join(dir, "config.capnp"), config(port, hostPort));
  const child = spawn(WORKERD, ["serve", "config.capnp", "--experimental"], { cwd: dir, stdio: ["ignore", "pipe", "pipe"] });
  const lines = [];
  let log = "";
  child.stdout.on("data", (chunk) => {
    log += chunk;
    lines.push(...String(chunk).split("\n"));
  });
  child.stderr.on("data", (chunk) => (log += chunk));
  const exited = new Promise((done) => child.once("exit", () => done()));
  const started = Date.now();
  while (child.exitCode === null && !(await connects(port))) {
    if (Date.now() - started > READY_WAIT_MS_MAX) throw new Error(`workerd did not listen within READY_WAIT_MS_MAX (${READY_WAIT_MS_MAX} ms): ${log}`);
    await sleep(READY_POLL_MS);
  }
  const marker = async (prefix) => {
    const waited = Date.now();
    while (!lines.some((line) => line.startsWith(prefix))) {
      if (Date.now() - waited > MARKER_WAIT_MS_MAX) return false;
      await sleep(READY_POLL_MS);
    }
    return true;
  };
  const kill = async () => {
    child.kill("SIGKILL");
    await exited;
  };
  return { port, marker, kill, log: () => log };
}

function send(port, query) {
  return fetch(`http://${LOOPBACK}:${port}/?${query}`, { method: "POST" }).then(
    async (response) => ({ status: response.status, text: await response.text() }),
    (error) => ({ status: "no response", text: String(error?.cause?.message ?? error?.message ?? error) }),
  );
}

async function spinFact(hostPort, fact, expected) {
  const kept = [];
  for (let trial = 0; trial < SPIN_TRIALS; trial++) {
    const dir = prepare(`${fact}-${trial}`);
    const workerd = await serve(dir, hostPort);
    const answer = send(workerd.port, `fact=${fact}`);
    const spun = await workerd.marker(SPIN_MARKER + fact);
    await workerd.kill();
    await answer;
    kept.push(spun ? facts(dir).length : -1);
  }
  return { kept, held: kept.every((count) => count === expected) };
}

async function gateTrials(hostPort, unconfirmed) {
  const dir = prepare(unconfirmed ? "unconfirmed" : "gate");
  const seen = [];
  const sql = unconfirmed ? "SELECT count(*) AS committed FROM _cf_KV" : "SELECT count(*) AS committed FROM facts";
  host.handle = async () => {
    seen.push(rows(dir, sql)[0]?.committed ?? 0);
    return "seen";
  };
  const workerd = await serve(dir, hostPort);
  const answers = [];
  for (let trial = 0; trial < GATE_TRIALS; trial++) {
    answers.push(await send(workerd.port, `fact=batch_then_fetch&trial=${trial}&rows=${GATE_ROWS}&bytes=${GATE_ROW_BYTES}&unconfirmed=${unconfirmed ? 1 : 0}`));
  }
  await workerd.kill();
  const expected = Array.from({ length: GATE_TRIALS }, (_, trial) => (trial + 1) * GATE_ROWS);
  const answered = answers.every((answer) => answer.status === 200);
  return { seen, expected, answered, behind: seen.filter((count, trial) => count < expected[trial]).length };
}

async function alarmFact(hostPort) {
  const dir = prepare("alarm");
  host.handle = async () => "unused";
  const first = await serve(dir, hostPort);
  const armed = await send(first.port, `fact=arm&ms=${ALARM_DELAY_MS}`);
  await first.kill();
  const before = facts(dir);
  const second = await serve(dir, hostPort);
  const ran = await second.marker(ALARM_MARKER);
  await second.kill();
  return { armed: armed.text, before, after: facts(dir), ran };
}

async function interleaveFact(hostPort) {
  const dir = prepare("interleave");
  let release = null;
  let reached = null;
  const slowReached = new Promise((done) => (reached = done));
  host.handle = (url) => {
    reached();
    return new Promise((done) => (release = () => done("released")));
  };
  const workerd = await serve(dir, hostPort);
  const slow = send(workerd.port, "fact=slow");
  await slowReached;
  const quick = await send(workerd.port, "fact=quick");
  const during = facts(dir);
  release();
  const slowAnswer = await slow;
  await workerd.kill();
  return { quick: quick.text, slow: slowAnswer.text, during, after: facts(dir) };
}

async function main() {
  const hostServer = await startHost();
  const version = execFileSync(WORKERD, ["--version"], { encoding: "utf8" }).trim();
  console.log(`storage facts on ${version}, compatibilityDate ${COMPATIBILITY_DATE}, SQLite-backed Durable Object, localDisk storage, SIGKILL`);
  try {
    host.handle = async () => "unused";
    const atomic = await spinFact(hostServer.port, "two_inserts_then_spin", 0);
    expect("event_commits_after_it_yields", atomic.held, `two inserts then a synchronous spin, SIGKILL: rows kept ${JSON.stringify(atomic.kept)} of 2 in each of ${SPIN_TRIALS} trials`);
    const microtask = await spinFact(hostServer.port, "insert_microtask_spin", 0);
    expect("microtask_await_does_not_commit", microtask.held, `an insert, await Promise.resolve(), then a spin, SIGKILL: rows kept ${JSON.stringify(microtask.kept)}`);
    const synced = await spinFact(hostServer.port, "insert_sync_spin", 1);
    expect("sync_commits", synced.held, `an insert, await ctx.storage.sync(), then a spin, SIGKILL: rows kept ${JSON.stringify(synced.kept)}`);
    const started = performance.now();
    const gate = await gateTrials(hostServer.port, false);
    const gateMs = performance.now() - started;
    expect("gate_holds_outbound_fetch", gate.answered && gate.behind === 0, `${GATE_ROWS} inserts of ${GATE_ROW_BYTES} bytes then fetch() without awaiting a commit: the host counted ${JSON.stringify(gate.seen)} committed rows on receipt, expected ${JSON.stringify(gate.expected)} (${Math.round(gateMs / GATE_TRIALS)} ms per trial)`);
    const unconfirmed = await gateTrials(hostServer.port, true);
    console.log(`note gate_not_isolated: the same with put(..., { allowUnconfirmed: true }), which the gate does not hold: the host counted ${JSON.stringify(unconfirmed.seen)}, behind in ${unconfirmed.behind} of ${GATE_TRIALS} trials; local workerd commits before the fetch leaves either way, so this run is consistent with the gate but does not isolate it`);
    const alarm = await alarmFact(hostServer.port);
    expect("alarm_runs_after_reset", alarm.ran && alarm.after.includes("alarm"), `setAlarm(now + ${ALARM_DELAY_MS} ms) committed, SIGKILL before it fired (rows ${JSON.stringify(alarm.before)}), restart: alarm() ran: ${alarm.ran}, rows ${JSON.stringify(alarm.after)}`);
    const interleave = await interleaveFact(hostServer.port);
    expect("event_enters_at_await", interleave.quick === "quick" && interleave.during.includes("quick") && !interleave.during.some((what) => what.startsWith("slow finished")), `a second request ran to completion while the first awaited fetch(): rows then ${JSON.stringify(interleave.during)}, at the end ${JSON.stringify(interleave.after)}`);
  } finally {
    hostServer.server.close();
  }
  if (failures.length > 0) {
    console.log(`storage facts: FAILED ${failures.join(", ")}`);
    process.exit(1);
  }
  console.log("storage facts: ok");
}

await main();

