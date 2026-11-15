#!/usr/bin/env node
import { mkdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { performance } from "node:perf_hooks";
import { Platform, startHost } from "./platform.mjs";

const HTTP_OK = 200;
const TOOL_CALLS_PER_REPLY = 2;
const { values } = parseArgs({
  options: {
    program: { type: "string" },
    module: { type: "string" },
    wasm: { type: "string" },
    dir: { type: "string" },
    turns: { type: "string", default: "50" },
    handler: { type: "string" },
    arguments: { type: "string" },
  },
});
if (!values.program || !values.module || !values.dir || !values.handler || !values.arguments) {
  console.error("usage: node bench.mjs --program FILE.hl --module WORKER.mjs [--wasm PROGRAM.wasm] --dir DIR --handler NAME --arguments JSON [--turns N]");
  process.exit(2);
}

function toolInputs(source) {
  const inputs = new Map();
  for (const match of source.matchAll(/^tool ([a-z_]+)\(([^)]*)\)/gm)) {
    const input = {};
    for (const param of match[2].split(",").map((text) => text.trim()).filter((text) => text !== "")) input[param.split(":")[0].trim()] = "bench";
    inputs.set(match[1], input);
  }
  return inputs;
}

const inputs = toolInputs(readFileSync(values.program, "utf8"));
const tools = [...inputs.keys()];
const answer = (path, body) => {
  const request = JSON.parse(body);
  if (path === "/tool") return JSON.stringify({ ok: "bench-" + request.tool });
  if (request.seq > 1 || tools.length === 0) return JSON.stringify({ answer: "done" });
  const calls = [];
  for (let index = 0; index < TOOL_CALLS_PER_REPLY; index++) {
    const tool = tools[index % tools.length];
    calls.push({ id: "c" + request.turn + "-" + index, input: inputs.get(tool), tool });
  }
  return JSON.stringify({ calls });
};

const dir = resolve(values.dir);
rmSync(dir, { recursive: true, force: true });
mkdirSync(dir, { recursive: true });
const host = await startHost(async (path, body) => answer(path, body));
const platform = new Platform(dir, resolve(values.module), host.port, values.wasm === undefined ? null : resolve(values.wasm));
const started = performance.now();
await platform.start("none", null);
const listening = performance.now() - started;
const turns = Number(values.turns);
const times = [];
for (let turn = 1; turn <= turns; turn++) {
  const body = '{"arguments":' + values.arguments + ',"handler":' + JSON.stringify(values.handler) + ',"turn":' + turn + "}";
  const sent = performance.now();
  const response = await platform.turn(body);
  times.push(performance.now() - sent);
  if (response.status !== HTTP_OK) throw new Error("turn " + turn + " answered " + response.status + ": " + response.body);
}
await platform.kill();
host.server.close();
const steady = times.slice(1).sort((left, right) => left - right);
const bytes = statSync(values.module).size + (values.wasm === undefined ? 0 : statSync(values.wasm).size);
console.log(
  "bench program=" + values.program + " format=" + (values.wasm === undefined ? "source" : "wasm") + " deploy_bytes=" + bytes +
    (values.wasm === undefined ? "" : " wasm_bytes=" + statSync(values.wasm).size) + " listen_ms=" + listening.toFixed(0) +
    " first_turn_ms=" + times[0].toFixed(1) + " turn_ms_median=" + steady[Math.floor(steady.length / 2)].toFixed(2) +
    " turn_ms_mean=" + (steady.reduce((sum, time) => sum + time, 0) / steady.length).toFixed(2) + " rows=" + platform.rows().length,
);
