#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { CellChecks, DurableCells, TracedCells, engineDifferential } from "./js-sim-cell.mjs";

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

class ParkEnd {}

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
    this.nestedDispatched = new Map();
    this.stats = { turns: 0, runs: 0, crashes: 0, dispatches: 0, steps: 0, cells: 0, nested: 0, divergences: 0, waits: 0, sleeps: 0, resumes: 0 };
    this.durableLines = () => [];
    this.checks = new CellChecks(rt, (invariant, detail) => new Failure(invariant, detail));
    this.cells = new TracedCells(this, {
      rt,
      cursor: this.cursor,
      same,
      failure: (invariant, detail) => new Failure(invariant, detail),
      durable: () => new DurableCells(rt, this.durableLines()),
      checks: this.checks,
    });
  }

  durableCells() {
    return new DurableCells(rt, this.durableLines());
  }

  hook(point) {
    const event = this.cursor.next("hook", rt.encodeJson(point));
    same(`hook point at event ${this.cursor.index - 1}`, event.point, point.point);
    if (point.kind !== undefined) same("hook site", event.kind ?? null, point.kind);
    if (point.effect !== undefined) same("hook effect", event.effect ?? null, point.effect);
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

  nestedTool(request) {
    const callId = `${request.turn}/${request.seq}/code-${request.nested.id}`;
    const external = request.effect === "external" ? 1 : 0;
    const count = (this.nestedDispatched.get(callId) ?? 0) + external;
    this.nestedDispatched.set(callId, count);
    this.stats.dispatches += 1;
    this.stats.nested += 1;
    if (count > 1) throw new Failure("nested_external_once", `nested external call ${callId} (${request.tool}) was dispatched ${count} times`);
    if (external === 1) {
      const durable = this.durableCells();
      if (!durable.header(request.nested.cell) || !durable.nested.has(callId)) {
        throw new Failure("nested_external_once", `nested external call ${callId} was dispatched before C1 made durable its cell header (${durable.header(request.nested.cell)}) and call record (${durable.nested.has(callId)})`);
      }
    }
  }

  tool(request, durable) {
    if (request.nested === null) return this.topTool(request, durable);
    if (durable !== null && durable.alarmed !== undefined) durable.alarmed(`${request.turn}/${request.seq}/code-${request.nested.id}`, request.effect);
    const event = this.cursor.next("tool", short(rt.encodeJson({ ...request, nested: BigInt(request.nested.id) })));
    same("tool call turn", event.turn, request.turn);
    same("tool call seq", event.seq, request.seq);
    same("tool call nested", event.nested, BigInt(request.nested.id));
    same("tool call name", event.tool, request.tool);
    same("tool call class", event.effect, request.effect);
    same("tool call input", event.input, request.input);
    this.checks.dispatched(this.stats.runs, request.nested, request, event.output);
    return event.output;
  }

  topTool(request, durable) {
    const callId = `${request.turn}/${request.seq}`;
    const external = request.effect === "external" ? 1 : 0;
    const count = (this.dispatched.get(callId) ?? 0) + external;
    this.dispatched.set(callId, count);
    this.stats.dispatches += 1;
    if (request.effect === "external" && count > 1) {
      throw new Failure("external_once", `external call ${callId} (${request.tool}) was dispatched ${count} times`);
    }
    if (request.effect === "external" && durable !== null) durable.durable(callId);
    const event = this.cursor.next("tool", short(rt.encodeJson(request)));
    same("tool call turn", event.turn, request.turn);
    same("tool call seq", event.seq, request.seq);
    same("tool call nested", event.nested ?? null, null);
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

  wait(wait) {
    this.checks.waited(wait, wait.code);
    if (!this.durableCells().header(wait.cell)) throw new Failure("header_before_wait", `cell ${wait.cell} waits on timer ${wait.timer} before its header is durable`);
    const event = this.cursor.next("wait", `cell ${wait.cell} timer ${wait.timer}`);
    same("wait cell", event.cell, BigInt(wait.cell));
    same("wait timer", event.timer, BigInt(wait.timer));
    same("wait until", event.until, wait.untilMs);
    this.stats.waits += 1;
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
    same("model mode", event.mode ?? null, request.mode);
    same("model context crc", event.context_crc, BigInt(rt.crc32(rt.encodeJson(request.context))));
    return event.response;
  }

  now() {
    return this.cursor.next("now", "now").answer;
  }

  clock() {
    return this.cursor.next("clock", "clock").answer;
  }

  sleep(turn, seq, untilMs) {
    const recorded = this.durableLines().map((line) => rt.decodeLine(line, false)).find((record) => record.kind === "call" && record.turn === turn && record.seq === seq);
    const durable = recorded === undefined || !rt.isObject(recorded.input) ? undefined : recorded.input.until;
    if (durable !== untilMs) throw new Failure("wait_until", `wait ${turn}/${seq} sleeps until ${untilMs} while its durable call record holds ${durable}`);
    const event = this.cursor.next("sleep", `${turn}/${seq}`);
    same("sleep turn", event.turn, BigInt(turn));
    same("sleep seq", event.seq, BigInt(seq));
    same("sleep until", event.until, untilMs);
    this.stats.sleeps += 1;
    if (event.answer === "crash") {
      this.stats.crashes += 1;
      throw new SimReset();
    }
    return event.clock;
  }

  resume(suspension) {
    if (this.cursor.peek()?.event !== "resume") return null;
    const event = this.cursor.next("resume", `${suspension.turn}/${suspension.seq}`);
    same("resume turn", event.turn, BigInt(suspension.turn));
    same("resume seq", event.seq, BigInt(suspension.seq));
    return { key: event.key, answer: event.answer };
  }

  resumed(suspension, resume, matched) {
    const event = this.cursor.next("resumed", `${suspension.turn}/${suspension.seq}`);
    same("resumed turn", event.turn, BigInt(suspension.turn));
    same("resumed seq", event.seq, BigInt(suspension.seq));
    same("resumed match", event.matched, matched);
    this.stats.resumes += 1;
  }

  inbox(turn) {
    const event = this.cursor.next("inbox", `turn ${turn}`);
    same("inbox turn", event.turn, BigInt(turn));
    return event.answer.map((json) => rt.deliveryFrom(json));
  }

  answered(turn, position, answers) {
    const event = this.cursor.next("answer", `turn ${turn}`);
    same("answer turn", event.turn, BigInt(turn));
    same("answer position", event.position, BigInt(position));
    same("event answers", event.answers, answers.map(([id, answer]) => ({ answer, id })));
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
    this.checks.ended(this.durableLines());
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
        const request = { turn: body.turn, seq: body.seq, tool: body.tool, effect: body.effect, input: body.input, admitted: body.admitted ?? null, nested: body.nested === null ? null : { id: Number(body.nested), cell: seed.nestedCell, code: seed.nestedCode } };
        if (request.nested !== null) seed.nestedTool(request);
        fetchCheck(request);
        const output = seed.tool(request, fetchCheck.durable === undefined ? null : fetchCheck);
        return new Response(rt.encodeJson(output), { status: HTTP_OK });
      }
      if (url === LISTINGS_URL) {
        fetchCheck(null);
        const output = seed.listing({ turn: body.turn, seq: body.seq, listing: body.listing });
        return new Response(rt.encodeJson(output), { status: HTTP_OK });
      }
      if (url === MODEL_URL) {
        fetchCheck(null);
        const response = seed.model({ turn: body.turn, seq: body.seq, calls_max: body.calls_max, mode: body.mode ?? null, context: body.context });
        return new Response(rt.encodeJson(response), { status: HTTP_OK });
      }
      throw new Error(`unexpected fetch of ${url}`);
    };
    const host = rt.defaultHost({ fetch: fakeFetch, toolsUrl: TOOLS_URL, listingsUrl: LISTINGS_URL, modelUrl: MODEL_URL, now: () => seed.now(), random: () => seed.random() });
    const tool = host.tool;
    return {
      ...host,
      async tool(request) {
        seed.nestedCell = request.nested?.cell ?? null;
        seed.nestedCode = request.nested?.code ?? null;
        return tool(request);
      },
      async wait(wait) {
        seed.wait(wait);
      },
      clock() {
        return seed.clock();
      },
      async sleep(turn, seq, untilMs) {
        return seed.sleep(turn, seq, untilMs);
      },
      async observe(event) {
        seed.cells.observe(event);
      },
    };
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
    inbox: (turn) => seed.inbox(turn),
    answer: (turn, position, answers) => seed.answered(turn, position, answers),
    resume: (suspension) => seed.resume(suspension),
    resumed: (suspension, resume, matched) => seed.resumed(suspension, resume, matched),
    tools: (world, requests, limit) => seed.tools(world, requests, limit),
    gate: (records) => {
      const bytes = storage.pendingBytes();
      if (rt.encodeLines(records, false) !== bytes) throw differ("rows inserted before the gate", rt.encodeLines(records, false), bytes);
      seed.flushed(bytes);
      storage.commit();
      seed.checks.flushed(storage.lines());
    },
  };
  fetchCheck.alarmed = (callId, effect) => {
    if (effect === "external" && storage.alarm === null) throw new Failure("liveness", `nested external call ${callId} left with no alarm committed, so a reset object never resumes`);
  };
  seed.durableLines = () => storage.lines();
  const env = { HL_SIMULATION: simulation, HL_HOST: seed.host(fetchCheck), HL_CELLS: seed.cells };
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
      const idles = turn.handler === rt.PROGRAM.idle?.handler;
      const admitted = storage.lines().some((line) => {
        const record = rt.decodeLine(line, false);
        return record.kind === "admitted" && Number(record.turn) === Number(turn.turn);
      });
      const byAlarm = idles ? first || storage.alarm !== null : !first && storage.alarm !== null && (admitted || rt.PROGRAM.idle === null);
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
          if (driven.ran !== "suspended") driven.finish = { turn: Number(driven.finish.turn), outcome: driven.finish.outcome, result: driven.finish.result };
        }
        if (storage.pendingRows.length > 0) throw new Failure("backend_equal", "rows were written after the turn's last flush");
        storage.commit();
        if (driven.ran !== "finished" && driven.ran !== "already_finished" && driven.ran !== "suspended" && driven.ran !== "not_due") throw new Failure("liveness", `the alarm found ${driven.ran}`);
        if (driven.ran !== "suspended" && driven.ran !== "not_due") seed.acknowledged(driven.finish, durableStatus);
        kind = driven.ran;
      } catch (error) {
        if (!(error instanceof SimReset)) throw error;
        storage.reset();
        instance = null;
        kind = "crash";
      }
      seed.exited(kind);
      if (kind !== "crash" && kind !== "suspended") break;
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
    this.notDueLive = false;
    this.slept = new Set();
    this.lastInbox = null;
  }

  async inbox(turn, live) {
    const index = this.stepIndex;
    const log = this.state.log;
    const name = `inbox ${turn}`;
    if (index < log.length && log[index].name === name) {
      this.stepIndex += 1;
      this.lastInbox = { entry: log[index], live: false };
      return log[index].deliveries;
    }
    if (!live) return [];
    if (index < log.length) throw differ(`workflow step ${index} on replay`, log[index].name, name);
    this.stepIndex += 1;
    const entry = { name, output: { lines: [] }, deliveries: this.seed.inbox(turn) };
    log.push(entry);
    this.lastInbox = { entry, live: true };
    return entry.deliveries;
  }

  async record(lines) {
    const { entry, live } = this.lastInbox;
    if (!live) return;
    this.seed.flushed(lines.map((line) => line + "\n").join(""));
    entry.output.lines.push(...lines);
  }

  async request(index) {
    if (index < this.state.requests.length) return this.state.requests[index];
    throw new Suspend();
  }

  async requestOrIdle(index) {
    return this.request(index);
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
      async nestedAttempt(turn, seq, nested) {
        const event = seed.cursor.next("attempt", `nested ${turn}/${seq}/code-${nested} step ${index} attempt ${attempts}`);
        same("nested attempt turn", event.turn, BigInt(turn));
        same("nested attempt seq", event.seq, BigInt(seq));
        same("nested attempt call", event.nested ?? null, BigInt(nested));
        same("step attempt", event.answer, BigInt(attempts));
        return attempts;
      },
      cells: seed.cells,
      observeWaited: async (wait, answer) => {
        if (answer && this.slept.has(`${wait.cell}/${wait.timer}`)) return;
        const event = seed.cursor.next("waited", `cell ${wait.cell} timer ${wait.timer}`);
        same("waited cell", event.cell, BigInt(wait.cell));
        same("waited timer", event.timer, BigInt(wait.timer));
        same("waited answer", event.answer, answer);
      },
      hook: (point) => seed.hook(point),
      tools: (world, requests, limit) => seed.tools(world, requests, limit),
      resume: (suspension) => seed.resume(suspension),
      resumed: (suspension, resume, matched) => seed.resumed(suspension, resume, matched),
      host: this.state.host,
      mutation: values.mutation,
      acknowledge: async (finish) => seed.acknowledged(finish, (turn) => statusIn(this.state.lines(), turn)),
      answer: async (turn, position, answers) => seed.answered(turn, position, answers),
    };
    const output = await harness.runStep(input, context);
    if (input.kind === "run" && output.notDue === true) this.notDueLive = true;
    if (input.kind === "run" && output.wait === undefined && output.suspended === undefined && output.notDue === undefined) {
      this.seed.flushed(output.lines.map((line) => line + "\n").join(""));
      this.seed.checks.flushed([...this.state.lines(), ...output.lines]);
      const request = rt.decodeJson(input.request);
      if (output.lines.some((line) => {
        const record = rt.decodeLine(line, false);
        return record.kind === "status" && BigInt(record.turn) === request.turn;
      })) this.statusLive = true;
    }
    log.push({ name, output });
    return { output, live: true };
  }

  async resume(name, suspension) {
    const index = this.stepIndex;
    this.stepIndex += 1;
    const log = this.state.log;
    if (index < log.length) {
      if (log[index].name !== name) throw differ(`workflow resume ${index} on replay`, log[index].name, name);
      return null;
    }
    log.push({ name, output: { lines: [] } });
    throw new ParkEnd();
  }

  async resumed(suspension, resume, matched) {
    this.seed.resumed(suspension, resume, matched);
  }

  async sleep(name, wait) {
    const index = this.stepIndex;
    this.stepIndex += 1;
    const log = this.state.log;
    if (index < log.length) {
      if (log[index].name !== name) throw differ(`workflow sleep ${index} on replay`, log[index].name, name);
      return;
    }
    this.seed.wait(wait);
    this.slept.add(`${wait.cell}/${wait.timer}`);
    log.push({ name, output: { lines: [] } });
  }
}

async function vercelSeed(seed) {
  seed.durableLines = () => state.lines();
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
        kind = workflow.statusLive ? "finished" : workflow.notDueLive ? "not_due" : "already_finished";
      } catch (error) {
        if (error instanceof Suspend) kind = workflow.statusLive ? "finished" : workflow.notDueLive ? "not_due" : "already_finished";
        else if (error instanceof ParkEnd) kind = "suspended";
        else if (error instanceof SimReset) kind = "crash";
        else throw error;
      }
      seed.exited(kind);
      if (kind !== "crash" && kind !== "suspended") break;
    }
  }
  seed.ended(state.lines().map((line) => line + "\n").join(""));
}

const seeds = parseTrace(values.trace);
const totals = { seeds: 0, turns: 0, runs: 0, crashes: 0, dispatches: 0, steps: 0, cells: 0, nested: 0, divergences: 0, waits: 0, sleeps: 0, resumes: 0 };
const engineTotals = { cells: 0, steps: 0, unparsed: 0 };
const started = performance.now();
for (const traced of seeds) {
  const seed = new Seed(traced);
  try {
    seed.cursor.next("seed", "start of seed");
    if (values.target === "cloudflare") await cloudflareSeed(seed);
    else await vercelSeed(seed);
    try {
      await engineDifferential(rt, seed.cells, engineTotals);
    } catch (error) {
      throw new Failure("engine_equal", short(error.message));
    }
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
  for (const key of ["turns", "runs", "crashes", "dispatches", "steps", "cells", "nested", "divergences", "waits", "sleeps", "resumes"]) totals[key] += seed.stats[key];
}
const elapsed = Math.round(performance.now() - started);
console.log(`ok target=${values.target} module=${values.module} mutation=${values.mutation} seeds=${totals.seeds} turns=${totals.turns} runs=${totals.runs} crashes=${totals.crashes} dispatches=${totals.dispatches} steps=${totals.steps}${totals.cells > 0 ? ` cells=${totals.cells} nested=${totals.nested} divergences=${totals.divergences} waits=${totals.waits} engine_cells=${engineTotals.cells} engine_steps=${engineTotals.steps} engine_unparsed=${engineTotals.unparsed}` : ""}${totals.sleeps + totals.resumes > 0 ? ` sleeps=${totals.sleeps} resumes=${totals.resumes}` : ""} ms=${elapsed}`);
