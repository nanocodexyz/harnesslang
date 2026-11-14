#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { performance } from "node:perf_hooks";

const DETAIL_CHARACTERS_MAX = 400;
const LOW_WORD = 0xffffffffn;
const WORD_BITS = 32n;
const CRC32_POLYNOMIAL_REFLECTED = 0xedb88320;
const POINTS = ["before_flush", "after_flush", "before_run", "after_run", "before_acknowledge"];
const KINDS = ["tool", "dispatch", "model", "now", "random"];
const EFFECTS = ["none", "idempotent", "external"];
const POINT_BEFORE_RUN = 2;
const POINT_AFTER_RUN = 3;

const { values } = parseArgs({
  options: {
    target: { type: "string" },
    module: { type: "string" },
    trace: { type: "string" },
  },
});
if (!values.target || !values.module || !values.trace) {
  console.error("usage: node scripts/wasm-sim.mjs --target TARGET --module FILE.wasm --trace TRACE.jsonl");
  process.exit(2);
}

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
const moduleBytes = readFileSync(values.module);
const compiled = performance.now();
const module = new WebAssembly.Module(moduleBytes);
const compileMs = performance.now() - compiled;

class Failure extends Error {}
class Stopped {
  constructor(exit) {
    this.exit = exit;
  }
}

function short(text) {
  return text.length > DETAIL_CHARACTERS_MAX ? text.slice(0, DETAIL_CHARACTERS_MAX) + "..." : text;
}

function parseJson(text) {
  return JSON.parse(text, (key, value, context) => (typeof value === "number" && context && /^-?\d+$/.test(context.source) ? BigInt(context.source) : value));
}

function encodeJson(value) {
  if (typeof value === "bigint") return value.toString();
  if (Array.isArray(value)) return "[" + value.map(encodeJson).join(",") + "]";
  if (value !== null && typeof value === "object") {
    return "{" + Object.keys(value).sort().map((key) => JSON.stringify(key) + ":" + encodeJson(value[key])).join(",") + "}";
  }
  return JSON.stringify(value);
}

function same(what, expected, actual) {
  const left = encodeJson(expected);
  const right = encodeJson(actual);
  if (left !== right) throw new Failure(short(what + ": the interpreter has " + left + ", the wasm module has " + right));
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) crc = crc & 1 ? (crc >>> 1) ^ CRC32_POLYNOMIAL_REFLECTED : crc >>> 1;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

const seeds = [];
for (const line of readFileSync(values.trace, "utf8").split("\n")) {
  if (line === "") continue;
  const event = parseJson(line);
  if (event.event === "seed") seeds.push({ seed: event, events: [] });
  else seeds[seeds.length - 1].events.push(event);
}

const stats = { seeds: 0, events: 0, runs: 0, owners: 0, flushes: 0, hooks: 0, instantiate_ms: 0, run_ms: 0 };

class Seed {
  constructor(events) {
    this.events = events;
    this.index = 0;
    this.opened = new Uint8Array(0);
    this.request = null;
    this.acks = 0;
  }

  peek() {
    return this.events[this.index];
  }

  next(kind, detail) {
    const event = this.events[this.index];
    if (event === undefined || event.event !== kind) {
      const found = event === undefined ? "nothing more" : encodeJson(event);
      throw new Failure(short("event " + this.index + ": the wasm module made a " + kind + " call (" + detail + ") where the interpreter made " + found));
    }
    this.index += 1;
    stats.events += 1;
    return event;
  }

  openedBytes(event) {
    const keep = Number(event.keep);
    if (keep > this.opened.length) throw new Failure("a run keeps more bytes than the last open read");
    const tail = Buffer.from(event.tail, "hex");
    const bytes = new Uint8Array(keep + tail.length);
    bytes.set(this.opened.subarray(0, keep));
    bytes.set(tail, keep);
    this.opened = bytes;
    return bytes;
  }

  run(event) {
    const journal = this.openedBytes(event);
    const started = performance.now();
    const instance = new Instance(this);
    stats.instantiate_ms += performance.now() - started;
    stats.runs += 1;
    const ran = performance.now();
    let exit;
    try {
      exit = instance.run(this.request, journal, event.epoch);
    } catch (error) {
      if (!(error instanceof Stopped)) throw error;
      exit = { exit: error.exit };
    }
    stats.run_ms += performance.now() - ran;
    if (exit.exit === "corrupt") throw new Failure("the wasm module found the journal corrupt: " + exit.error);
    const done = this.next("exit", exit.exit);
    same("run exit", done.result, exit.exit);
    if (exit.exit === "already_finished") this.acknowledged(exit.finish);
  }

  acknowledged(finish) {
    const event = this.next("ack", encodeJson(finish));
    same("acknowledged turn", event.turn, finish.turn);
    same("acknowledged outcome", event.outcome, finish.outcome);
    same("acknowledged result", event.result, finish.result);
    this.acks += 1;
  }

  replay() {
    for (;;) {
      const event = this.peek();
      if (event === undefined) throw new Failure("the trace ended inside a seed");
      if (event.event === "turn") {
        this.next("turn", "");
        this.request = encodeJson({ arguments: event.arguments, handler: event.handler, turn: event.turn });
      } else if (event.event === "run") {
        this.next("run", "");
        this.run(event);
      } else if (event.event === "end") {
        this.next("end", "");
        if (event.result !== "ok") throw new Failure("the interpreter's seed failed: " + event.result);
        same("acknowledgements", event.acks, BigInt(this.acks));
        return;
      } else {
        throw new Failure("event " + this.index + ": the interpreter made " + event.event + ", the wasm host expected a turn, run or end");
      }
    }
  }
}

class Instance {
  constructor(seed) {
    this.seed = seed;
    this.instance = new WebAssembly.Instance(module, { hl: this.imports() });
    this.exports = this.instance.exports;
  }

  bytes(at, length) {
    return new Uint8Array(this.exports.memory.buffer, at, length);
  }

  text(at, length) {
    return decoder.decode(this.bytes(at, length));
  }

  give(bytes) {
    const at = this.exports.hl_alloc(bytes.length);
    this.bytes(at, bytes.length).set(bytes);
    return (BigInt(at) << WORD_BITS) | BigInt(bytes.length);
  }

  giveText(text) {
    return this.give(encoder.encode(text));
  }

  run(request, journal, epoch) {
    const requestAt = this.giveText(request);
    const journalAt = this.give(journal);
    const packed = this.exports.hl_run(Number(requestAt >> WORD_BITS), Number(requestAt & LOW_WORD), Number(journalAt >> WORD_BITS), Number(journalAt & LOW_WORD), BigInt(epoch));
    return parseJson(this.text(Number(packed >> WORD_BITS), Number(packed & LOW_WORD)));
  }

  imports() {
    const seed = this.seed;
    return {
      fail: (at, length) => {
        throw new Failure("the wasm module failed: " + this.text(at, length));
      },
      hook: (point, kind, effect) => {
        const name = POINTS[point];
        const event = seed.next("hook", name);
        same("hook point at event " + (seed.index - 1), event.point, name);
        if (point === POINT_BEFORE_RUN || point === POINT_AFTER_RUN) {
          same("hook kind", event.kind, KINDS[kind]);
          same("hook effect", event.effect, EFFECTS[effect]);
        }
        stats.hooks += 1;
        if (event.answer === "crash") throw new Stopped("crash");
        if (event.answer === "owner") {
          const owner = seed.next("owner", "a second owner");
          stats.owners += 1;
          seed.run(owner);
          return;
        }
        if (event.answer !== "ok") throw new Failure("unknown hook answer " + event.answer);
      },
      flush: (epoch, at, length) => {
        const bytes = this.text(at, length);
        const event = seed.next("flush", short(bytes));
        same("flush epoch", event.epoch, epoch);
        if (event.bytes !== bytes) throw new Failure(short("flushed records: the interpreter has " + JSON.stringify(event.bytes) + ", the wasm module has " + JSON.stringify(bytes)));
        stats.flushes += 1;
        if (event.answer === "crash") throw new Stopped("crash");
        if (event.answer === "fenced") throw new Stopped("fenced");
        if (event.answer !== "ok") throw new Failure("unknown flush answer " + event.answer);
      },
      keep: () => {},
      tool: (at, length) => {
        const request = parseJson(this.text(at, length));
        const event = seed.next("tool", short(encodeJson(request)));
        same("tool call turn", event.turn, request.turn);
        same("tool call seq", event.seq, request.seq);
        same("tool call name", event.tool, request.tool);
        same("tool call class", event.effect, request.effect);
        same("tool call input", event.input, request.input);
        return this.giveText(encodeJson(event.output));
      },
      model: (at, length) => {
        const request = parseJson(this.text(at, length));
        const event = seed.next("model", request.turn + "/" + request.seq);
        same("model call turn", event.turn, request.turn);
        same("model call seq", event.seq, request.seq);
        same("model calls_max", event.calls_max, request.calls_max);
        same("model context crc", event.context_crc, BigInt(crc32(encoder.encode(encodeJson(request.context)))));
        return this.giveText(encodeJson(event.response));
      },
      now: () => BigInt(seed.next("now", "now").answer),
      random: () => BigInt(seed.next("random", "random").answer),
      attempt: (turn, seq) => {
        const event = seed.next("attempt", turn + "/" + seq);
        same("attempt turn", event.turn, turn);
        same("attempt seq", event.seq, seq);
        return BigInt(event.answer);
      },
      acknowledge: (at, length) => {
        seed.acknowledged(parseJson(this.text(at, length)));
      },
    };
  }
}

for (const { seed: header, events } of seeds) {
  if (header.target !== values.target) throw new Error("the trace is for " + header.target + ", not " + values.target);
  const seed = new Seed(events);
  try {
    seed.replay();
  } catch (error) {
    console.error("seed " + header.seed + ": " + (error instanceof Failure ? "backend_equal: " : "") + (error.stack ?? error));
    process.exit(1);
  }
  stats.seeds += 1;
}
console.log(
  "ok backend=wasm target=" + values.target + " seeds=" + stats.seeds + " events=" + stats.events + " runs=" + stats.runs + " owners=" + stats.owners +
    " flushes=" + stats.flushes + " hooks=" + stats.hooks + " module_bytes=" + moduleBytes.length + " compile_ms=" + compileMs.toFixed(1) +
    " instantiate_us_per_run=" + ((stats.instantiate_ms * 1000) / Math.max(stats.runs, 1)).toFixed(1) + " run_ms=" + stats.run_ms.toFixed(1),
);
