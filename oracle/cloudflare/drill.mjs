#!/usr/bin/env node
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { Failure } from "./platform.mjs";
import { killSession } from "./kill.mjs";
import { parseTrace, traceSeed } from "./trace.mjs";

const { values } = parseArgs({
  options: {
    mode: { type: "string" },
    module: { type: "string" },
    dir: { type: "string" },
    trace: { type: "string" },
    seeds_max: { type: "string", default: "0" },
    sessions: { type: "string", default: "8" },
    turns: { type: "string", default: "6" },
    seed: { type: "string", default: "0" },
    delay_ms_max: { type: "string", default: "40" },
    jobs: { type: "string", default: "4" },
    tools: { type: "string", default: "" },
    profile: { type: "string", default: "all" },
  },
});
if (!["kill", "trace"].includes(values.mode) || !values.module || !values.dir || !values.trace) {
  console.error("usage: node drill.mjs --mode kill|trace --module WORKER.mjs --dir DIR --trace TRACE.jsonl [--tools a,b] [--sessions N] [--turns N] [--seed S] [--jobs N] [--seeds_max N]");
  process.exit(2);
}
const module = resolve(values.module);
const rt = (await import(pathToFileURL(module).href)).harness.runtime;
const options = {
  module,
  dir: resolve(values.dir),
  sessions: Number(values.sessions),
  turns: Number(values.turns),
  seed: Number(values.seed),
  delayMsMax: Number(values.delay_ms_max),
  requests: [],
  profile: values.profile,
  tools: values.tools === "" ? [] : values.tools.split(","),
};

async function pool(items, jobs, work) {
  const results = new Array(items.length);
  let next = 0;
  let failure = null;
  const worker = async () => {
    while (failure === null && next < items.length) {
      const index = next;
      next += 1;
      try {
        results[index] = await work(items[index]);
      } catch (error) {
        failure ??= { item: items[index], error };
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(jobs, items.length) }, worker));
  return { results, failure };
}

function fail(what, error) {
  const invariant = error instanceof Failure ? error.invariant : "panic";
  const detail = error instanceof Failure ? error.message : error?.stack ?? String(error);
  console.log(`FAIL invariant=${invariant} ${what}: ${detail}`);
  console.log("drill: FAILED");
  process.exit(1);
}

function histogram(map) {
  const counts = new Map();
  for (const rows of map.values()) counts.set(rows, (counts.get(rows) ?? 0) + 1);
  return [...counts.entries()].sort((a, b) => a[0] - b[0]).map(([rows, turns]) => `${rows}:${turns}`).join(",");
}

async function killMode() {
  options.requests = parseTrace(rt, values.trace).flatMap((seed) => seed.events.filter((event) => event.event === "turn"));
  const indexes = Array.from({ length: options.sessions }, (_, index) => index + 1);
  const started = Date.now();
  const { results, failure } = await pool(indexes, Number(values.jobs), (index) => killSession(rt, options, index));
  if (failure !== null) fail(`session=${failure.item} seed=${options.seed + failure.item}`, failure.error);
  const totals = {};
  const rowsPerTurn = new Map();
  const shapes = new Map();
  for (const result of results) {
    const { rowsPerTurn: perTurn, shapes: perShape, seed, ...numbers } = result;
    for (const [turn, shape] of perShape) {
      const key = `model_replies=${shape.models} calls=${shape.calls}`;
      const rows = shapes.get(key) ?? new Map();
      rows.set(perTurn.get(turn), (rows.get(perTurn.get(turn)) ?? 0) + 1);
      shapes.set(key, rows);
    }
    console.log(`session seed=${seed} ${Object.entries(numbers).map(([key, value]) => `${key}=${value}`).join(" ")} rows_per_turn=${histogram(perTurn)}`);
    for (const [key, value] of Object.entries(numbers)) totals[key] = (totals[key] ?? 0) + value;
    for (const [turn, rows] of perTurn) rowsPerTurn.set(`${seed}/${turn}`, rows);
  }
  if (options.profile === "clean") {
    for (const [key, rows] of [...shapes.entries()].sort()) console.log(`shape ${key} rows:turns=${[...rows.entries()].map(([count, turns]) => `${count}:${turns}`).join(",")}`);
  }
  console.log(`totals ${Object.entries(totals).map(([key, value]) => `${key}=${value}`).join(" ")} rows_per_turn=${histogram(rowsPerTurn)} ms=${Date.now() - started}`);
  console.log(`drill: ${options.sessions} sessions of ${options.turns} turns on workerd survived ${totals.kills} kill -9s, ${totals.storageFaults} storage faults and ${totals.duplicates} duplicate requests: external_once, unknown_reported, idempotent_known, finish_once and seq_unique hold on the object's SQL`);
}

async function traceMode() {
  const seeds = parseTrace(rt, values.trace);
  const limit = Number(values.seeds_max);
  const run = limit > 0 ? seeds.slice(0, limit) : seeds;
  const started = Date.now();
  const { results, failure } = await pool(run, Number(values.jobs), (seed) => traceSeed(rt, options, seed));
  if (failure !== null) fail(`seed=${failure.item.seed} profile=${failure.item.profile}`, failure.error);
  const totals = { seeds: results.length, rows: 0, runs: 0, crashes: 0, alarmRuns: 0, requests: 0, comparisons: 0, starts: 0 };
  for (const result of results) for (const key of Object.keys(totals)) if (key !== "seeds") totals[key] += result[key];
  console.log(`trace ${Object.entries(totals).map(([key, value]) => `${key}=${value}`).join(" ")} ms=${Date.now() - started}`);
  console.log(`drill: ${results.length} traced seeds ran on workerd with the interpreter's journal, dispatches and acknowledgements`);
}

if (values.mode === "kill") await killMode();
else await traceMode();
