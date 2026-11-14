#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

const TOOLS_URL = "https://tools.simulation.invalid/call";
const MODEL_URL = "https://model.simulation.invalid/infer";
const LISTINGS_URL = "https://listings.simulation.invalid/list";
const TURN_URL = "https://harness.simulation.invalid/turn";
const DETAIL_CHARACTERS_MAX = 400;
const HTTP_OK = 200;

const { values } = parseArgs({
  options: {
    target: { type: "string" },
    module: { type: "string" },
    trace: { type: "string" },
    mutation: { type: "string", default: "none" },
  },
});
if (!values.target || !values.module || !values.trace) {
  console.error("usage: node scripts/js-sim.mjs --target cloudflare|vercel --module FILE.mjs --trace TRACE.jsonl [--mutation NAME]");
  process.exit(2);
}
const loaded = await import(pathToFileURL(values.module).href);
const harness = loaded.harness;
const rt = harness.runtime;
if (harness.target !== values.target) {
  console.error(`${values.module} is built for ${harness.target}, not ${values.target}`);
  process.exit(2);
}

class Failure extends Error {
  constructor(invariant, detail) {
    super(detail);
    this.invariant = invariant;
  }
}

class SimReset {}

class Suspend {}

function short(text) {
  return text.length > DETAIL_CHARACTERS_MAX ? text.slice(0, DETAIL_CHARACTERS_MAX) + "..." : text;
}

function differ(what, expected, actual) {
  return new Failure("backend_equal", short(`${what}: the interpreter has ${expected}, the generated code has ${actual}`));
}

function hexText(hex) {
  return Buffer.from(hex, "hex").toString("utf8");
}

function parseTrace(path) {
  const seeds = [];
  let current = null;
  for (const line of readFileSync(path, "utf8").split("\n")) {
    if (line === "") continue;
    const event = rt.decodeJson(line);
    if (rt.encodeJson(event) !== line) throw new Error(`the JavaScript codec does not round-trip trace line ${short(line)}`);
    if (event.event === "seed") {
      current = { seed: event.seed, profile: event.profile, events: [], acks: [], turns: 0 };
      seeds.push(current);
    }
    if (event.event === "ack") current.acks.push(event);
    else current.events.push(event);
    if (event.event === "turn") current.turns += 1;
  }
  return seeds;
}

class Cursor {
  constructor(events) {
    this.events = events;
    this.index = 0;
  }

  peek() {
    return this.events[this.index];
  }

  next(kind, detail) {
    const event = this.events[this.index];
    if (event === undefined || event.event !== kind) {
      const found = event === undefined ? "nothing more" : rt.encodeJson(event);
      throw new Failure("backend_equal", short(`event ${this.index}: the generated code made a ${kind} call (${detail}) where the interpreter made ${found}`));
    }
    this.index += 1;
    return event;
  }
}

function same(what, expected, actual) {
  const left = rt.encodeJson(expected);
  const right = rt.encodeJson(actual);
  if (left !== right) throw differ(what, left, right);
}

class Seed {
  constructor(seed) {
    this.seed = seed;
    this.cursor = new Cursor(seed.events);
    this.opened = "";
    this.acks = [];
    this.dispatched = new Map();
    this.stats = { turns: 0, runs: 0, crashes: 0, dispatches: 0, steps: 0 };
  }

  hook(point) {
    const event = this.cursor.next("hook", rt.encodeJson(point));
    same("hook point", event.point, point.point);
    if (point.kind !== undefined) {
      same("hook site", event.kind ?? null, point.kind);
      same("hook effect", event.effect ?? null, point.effect);
    }
    if (event.answer === "crash") {
      this.stats.crashes += 1;
      throw new SimReset();
    }
    if (event.answer !== "ok") throw new Failure("backend_equal", `hook answer ${event.answer} has no meaning on ${values.target}`);
  }

  flushed(bytes) {
    const event = this.cursor.next("flush", short(bytes));
    if (event.bytes !== bytes) throw differ("flushed records", event.bytes, bytes);
    if (event.answer !== "ok") throw new Failure("backend_equal", `flush answer ${event.answer} has no meaning on ${values.target}`);
  }

  opening(medium) {
    const event = this.cursor.next("run", "open the journal");
    const expected = this.opened.slice(0, Number(event.keep)) + hexText(event.tail);
    if (expected !== medium) throw differ("journal at open", JSON.stringify(short(expected)), JSON.stringify(short(medium)));
    this.opened = expected;
    this.stats.runs += 1;
  }

  exited(kind) {
    const event = this.cursor.next("exit", kind);
    same("run exit", event.result, kind);
  }

  tool(request, durable) {
    const callId = `${request.turn}/${request.seq}`;
    const external = request.effect === "external" ? 1 : 0;
    const count = (this.dispatched.get(callId) ?? 0) + external;
    this.dispatched.set(callId, count);
    this.stats.dispatches += 1;
    if (request.effect === "external" && count > 1) {
      throw new Failure("external_once", `external call ${callId} (${request.tool}) was dispatched ${count} times`);
    }
    if (request.effect === "external" && durable !== null) durable(callId);
    const event = this.cursor.next("tool", short(rt.encodeJson(request)));
    same("tool call turn", event.turn, request.turn);
    same("tool call seq", event.seq, request.seq);
    same("tool call name", event.tool, request.tool);
    same("tool call class", event.effect, request.effect);
    same("tool call input", event.input, request.input);
    same("tool call admission", event.admitted, request.admitted ?? null);
    return event.output;
  }

  listing(request) {
    const event = this.cursor.next("listing", `${request.turn}/${request.seq}`);
    same("listing turn", event.turn, request.turn);
    same("listing seq", event.seq, request.seq);
    same("listing name", event.listing, request.listing);
    return event.output;
  }

  async tools(world, requests, limit) {
    const event = this.cursor.next("batch", "a batch of " + requests.length + " calls");
    same("batch limit", event.limit ?? null, limit === null ? null : BigInt(limit));
    const order = event.order.map((index) => Number(index));
    if (order.length !== requests.length) throw differ("batch size", order.length, requests.length);
    const width = limit === null ? requests.length : Math.min(limit, requests.length);
    const outputs = new Array(requests.length);
    const completed = [];
    let started = 0;
    const start = async () => {
      await world.hook({ point: "before_run", kind: "dispatch", effect: requests[started].effect });
      outputs[started] = await world.tool(requests[started]);
      started += 1;
    };
    while (started < width) await start();
    for (const index of order) {
      if (index >= started || outputs[index] === undefined) throw new Failure("backend_equal", "batch call " + index + " completes before it started");
      await world.hook({ point: "after_run", kind: "dispatch", effect: requests[index].effect });
      completed.push([index, outputs[index]]);
      outputs[index] = undefined;
      if (started < requests.length) await start();
    }
    return completed;
  }

  model(request) {
    const event = this.cursor.next("model", `${request.turn}/${request.seq}`);
    same("model call turn", event.turn, request.turn);
    same("model call seq", event.seq, request.seq);
    same("model calls_max", event.calls_max, request.calls_max);
    same("model context crc", event.context_crc, BigInt(rt.crc32(rt.encodeJson(request.context))));
    return event.response;
  }

  now() {
    return this.cursor.next("now", "now").answer;
  }

  random() {
    return this.cursor.next("random", "random").answer;
  }

  acknowledged(finish, durableStatus) {
    const expected = this.seed.acks[this.acks.length];
    const actual = { turn: BigInt(finish.turn), outcome: finish.outcome, result: finish.result };
    if (expected === undefined) throw new Failure("finish_once", short(`turn ${finish.turn} was acknowledged again: ${rt.encodeJson(actual)}`));
    same("acknowledgement", { turn: expected.turn, outcome: expected.outcome, result: expected.result }, actual);
    const status = durableStatus(finish.turn);
    if (status === null || status.outcome !== finish.outcome || rt.encodeJson(status.result) !== rt.encodeJson(finish.result)) {
      throw new Failure("finish_once", `turn ${finish.turn} was acknowledged before its status was durable`);
    }
    this.acks.push(actual);
  }

  ended(medium) {
    const event = this.cursor.next("end", "end of seed");
    same("journal bytes", event.journal_bytes, BigInt(Buffer.byteLength(medium)));
    same("journal crc", event.journal_crc, BigInt(rt.crc32(medium)));
    same("acknowledgements", event.acks, BigInt(this.acks.length));
    if (event.result !== "ok") throw new Failure("backend_equal", `the interpreter failed this seed: ${event.result}`);
  }

  host(fetchCheck) {
    const seed = this;
    const fakeFetch = async (url, init) => {
      const body = rt.decodeJson(init.body);
      if (url === TOOLS_URL) {
        const request = { turn: body.turn, seq: body.seq, tool: body.tool, effect: body.effect, input: body.input, admitted: body.admitted ?? null };
        fetchCheck(request);
        const output = seed.tool(request, fetchCheck.durable ?? null);
        return new Response(rt.encodeJson(output), { status: HTTP_OK });
      }
      if (url === LISTINGS_URL) {
        fetchCheck(null);
        const output = seed.listing({ turn: body.turn, seq: body.seq, listing: body.listing });
        return new Response(rt.encodeJson(output), { status: HTTP_OK });
      }
      if (url === MODEL_URL) {
        fetchCheck(null);
        const response = seed.model({ turn: body.turn, seq: body.seq, calls_max: body.calls_max, context: body.context });
        return new Response(rt.encodeJson(response), { status: HTTP_OK });
      }
      throw new Error(`unexpected fetch of ${url}`);
    };
    return rt.defaultHost({ fetch: fakeFetch, toolsUrl: TOOLS_URL, listingsUrl: LISTINGS_URL, modelUrl: MODEL_URL, now: () => seed.now(), random: () => seed.random() });
  }
}

function statusIn(lines, turn) {
  let found = null;
  for (const line of lines) {
    const record = rt.decodeLine(line, false);
    if (record.kind === "status" && record.turn === turn) found = record;
  }
  return found;
}

class FakeStorage {
  constructor(statements) {
    this.statements = statements;
    this.rows = [];
    this.pendingRows = [];
    this.alarm = null;
    this.pendingAlarm = undefined;
    this.sql = { exec: (query, ...bindings) => this.exec(query, bindings) };
  }

  exec(query, bindings) {
    const cursor = (rows) => ({ toArray: () => rows.map((row) => ({ ...row })) });
    if (query === this.statements.schema) return cursor([]);
    if (query === this.statements.load) return cursor([...this.rows, ...this.pendingRows]);
    if (query === this.statements.insert) {
      const [position, record] = bindings;
      if ([...this.rows, ...this.pendingRows].some((row) => row.position === position)) {
        throw new Error(`UNIQUE constraint failed: hl_journal.position ${position}`);
      }
      this.pendingRows.push({ position, record });
      return cursor([]);
    }
    throw new Error(`unexpected statement ${query}`);
  }

  setAlarm(time) {
    this.pendingAlarm = time;
    return Promise.resolve();
  }

  deleteAlarm() {
    this.pendingAlarm = null;
    return Promise.resolve();
  }

  getAlarm() {
    return Promise.resolve(this.pendingAlarm !== undefined ? this.pendingAlarm : this.alarm);
  }

  sync() {
    return Promise.resolve();
  }

  pendingBytes() {
    return this.pendingRows.map((row) => row.record + "\n").join("");
  }

  commit() {
    this.rows.push(...this.pendingRows);
    this.pendingRows = [];
    if (this.pendingAlarm !== undefined) this.alarm = this.pendingAlarm;
    this.pendingAlarm = undefined;
  }

  reset() {
    this.pendingRows = [];
    this.pendingAlarm = undefined;
  }

  lines() {
    return this.rows.map((row) => row.record);
  }

  medium() {
    return this.rows.map((row) => row.record + "\n").join("");
  }
}

async function cloudflareSeed(seed) {
  const storage = new FakeStorage(harness.statements);
  const ctx = { storage };
  const fetchCheck = (request) => {
    if (storage.pendingRows.length > 0) {
      throw new Failure("backend_equal", "writes were pending at an outbound request, outside any flush the interpreter made");
    }
  };
  fetchCheck.durable = (callId) => {
    const durable = storage.lines().some((line) => {
      const record = rt.decodeLine(line, false);
      return record.kind === "call" && `${record.turn}/${record.seq}` === callId;
    });
    if (!durable) throw new Failure("external_once", `external call ${callId} left before its call record was committed, so a reset re-dispatches it`);
    if (storage.alarm === null) throw new Failure("liveness", `external call ${callId} left with no alarm committed, so a reset object never resumes`);
  };
  const simulation = {
    mutation: values.mutation,
    hook: (point) => seed.hook(point),
    tools: (world, requests, limit) => seed.tools(world, requests, limit),
    gate: (records) => {
      const bytes = storage.pendingBytes();
      if (rt.encodeLines(records, false) !== bytes) throw differ("rows inserted before the gate", rt.encodeLines(records, false), bytes);
      seed.flushed(bytes);
      storage.commit();
    },
  };
  const env = { HL_SIMULATION: simulation, HL_HOST: seed.host(fetchCheck) };
  let instance = null;
  const durableStatus = (turn) => statusIn(storage.lines(), turn);
  while (seed.cursor.peek()?.event === "turn") {
    const turn = seed.cursor.next("turn", "next turn");
    seed.stats.turns += 1;
    const body = rt.encodeJson(Object.assign(Object.create(null), { arguments: turn.arguments, handler: turn.handler, turn: turn.turn }));
    let first = true;
    for (;;) {
      seed.opening(storage.medium());
      instance ??= new loaded.HarnessObject(ctx, env);
      const byAlarm = !first && storage.alarm !== null;
      first = false;
      let kind;
      try {
        let driven;
        if (byAlarm) {
          driven = await instance.alarm();
          storage.alarm = null;
        } else {
          const response = await instance.fetch(new Request(TURN_URL, { method: "POST", body }));
          if (response.status !== HTTP_OK) throw new Failure("liveness", `turn ${turn.turn}: the object answered ${response.status}: ${await response.text()}`);
          driven = { ran: response.headers.get(harness.ranHeader), finish: rt.decodeJson(await response.text()) };
          driven.finish = { turn: Number(driven.finish.turn), outcome: driven.finish.outcome, result: driven.finish.result };
        }
        if (storage.pendingRows.length > 0) throw new Failure("backend_equal", "rows were written after the turn's last flush");
        storage.commit();
        if (driven.ran !== "finished" && driven.ran !== "already_finished") throw new Failure("liveness", `the alarm found ${driven.ran}`);
        seed.acknowledged(driven.finish, durableStatus);
        kind = driven.ran;
      } catch (error) {
        if (!(error instanceof SimReset)) throw error;
        storage.reset();
        instance = null;
        kind = "crash";
      }
      seed.exited(kind);
      if (kind !== "crash") break;
    }
  }
  seed.ended(storage.medium());
}

class SimWorkflow {
  constructor(seed, state) {
    this.seed = seed;
    this.state = state;
    this.turnsMax = seed.seed.turns;
    this.stepIndex = 0;
    this.statusLive = false;
  }

  async request(index) {
    if (index < this.state.requests.length) return this.state.requests[index];
    throw new Suspend();
  }

  async hook(point) {
    this.seed.hook(point);
  }

  async step(name, input) {
    const index = this.stepIndex;
    this.stepIndex += 1;
    const log = this.state.log;
    if (index < log.length) {
      if (log[index].name !== name) throw differ(`workflow step ${index} on replay`, log[index].name, name);
      return { output: log[index].output, live: false };
    }
    const attempts = (this.state.attempts.get(index) ?? 0) + 1;
    this.state.attempts.set(index, attempts);
    this.seed.stats.steps += 1;
    const seed = this.seed;
    const context = {
      get attempt() {
        const event = seed.cursor.next("attempt", `step ${index} attempt ${attempts}`);
        same("step attempt", event.answer, BigInt(attempts));
        return attempts;
      },
      hook: (point) => seed.hook(point),
      tools: (world, requests, limit) => seed.tools(world, requests, limit),
      host: this.state.host,
      mutation: values.mutation,
      acknowledge: async (finish) => seed.acknowledged(finish, (turn) => statusIn(this.state.lines(), turn)),
    };
    const output = await harness.runStep(input, context);
    if (input.kind === "run") {
      this.seed.flushed(output.lines.map((line) => line + "\n").join(""));
      const request = rt.decodeJson(input.request);
      if (output.lines.some((line) => {
        const record = rt.decodeLine(line, false);
        return record.kind === "status" && BigInt(record.turn) === request.turn;
      })) this.statusLive = true;
    }
    log.push({ name, output });
    return { output, live: true };
  }
}

async function vercelSeed(seed) {
  const state = {
    requests: [],
    log: [],
    attempts: new Map(),
    host: seed.host(() => {}),
    lines() {
      return this.log.flatMap((entry) => entry.output.lines);
    },
  };
  while (seed.cursor.peek()?.event === "turn") {
    const turn = seed.cursor.next("turn", "next turn");
    seed.stats.turns += 1;
    state.requests.push(rt.encodeJson(Object.assign(Object.create(null), { arguments: turn.arguments, handler: turn.handler, turn: turn.turn })));
    for (;;) {
      seed.opening(state.lines().map((line) => line + "\n").join(""));
      const workflow = new SimWorkflow(seed, state);
      let kind;
      try {
        await harness.session(workflow);
        kind = workflow.statusLive ? "finished" : "already_finished";
      } catch (error) {
        if (error instanceof Suspend) kind = workflow.statusLive ? "finished" : "already_finished";
        else if (error instanceof SimReset) kind = "crash";
        else throw error;
      }
      seed.exited(kind);
      if (kind !== "crash") break;
    }
  }
  seed.ended(state.lines().map((line) => line + "\n").join(""));
}

const seeds = parseTrace(values.trace);
const totals = { seeds: 0, turns: 0, runs: 0, crashes: 0, dispatches: 0, steps: 0 };
const started = performance.now();
for (const traced of seeds) {
  const seed = new Seed(traced);
  try {
    seed.cursor.next("seed", "start of seed");
    if (values.target === "cloudflare") await cloudflareSeed(seed);
    else await vercelSeed(seed);
  } catch (error) {
    const invariant = error instanceof Failure ? error.invariant : "panic";
    const detail = error instanceof Failure ? error.message : (error?.stack ?? String(error));
    if (process.env.HL_JS_SIM_CONTEXT) {
      const from = Math.max(0, seed.cursor.index - Number(process.env.HL_JS_SIM_CONTEXT));
      for (let index = from; index < seed.cursor.index + 3 && index < traced.events.length; index++) {
        console.log(`${index === seed.cursor.index ? ">" : " "} ${index} ${short(rt.encodeJson(traced.events[index]))}`);
      }
    }
    console.log(`FAIL invariant=${invariant} seed=${traced.seed} target=${values.target} profile=${traced.profile} mutation=${values.mutation}: ${detail}`);
    process.exit(1);
  }
  totals.seeds += 1;
  for (const key of ["turns", "runs", "crashes", "dispatches", "steps"]) totals[key] += seed.stats[key];
}
const elapsed = Math.round(performance.now() - started);
console.log(`ok target=${values.target} module=${values.module} mutation=${values.mutation} seeds=${totals.seeds} turns=${totals.turns} runs=${totals.runs} crashes=${totals.crashes} dispatches=${totals.dispatches} steps=${totals.steps} ms=${elapsed}`);
