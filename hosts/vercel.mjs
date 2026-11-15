import programBase64 from "./program.mjs";

const EPOCH = 0n;
const STEPS_PER_EFFECT_MAX = 3;
const STEPS_PER_SUSPENSION_MAX = 2;
const STEPS_PER_TURN_FIXED = 2;
const LINE_END = "\n";
const WORD_BITS = 32n;
const LOW_WORD = 0xffffffffn;
const U64_MAX = 0xffffffffffffffffn;
const POINTS = ["before_flush", "after_flush", "before_run", "after_run", "before_acknowledge"];
const RUN_POINTS = ["before_run", "after_run"];
const KINDS = ["tool", "dispatch", "model", "now", "random", "listing", "cell", "wait", "hold", "suspend", "deny"];
const EFFECTS = ["none", "idempotent", "external"];
const POINT_AFTER_FLUSH = { point: "after_flush" };
const POINT_BEFORE_ACKNOWLEDGE = { point: "before_acknowledge" };

class Panic extends Error {}
class Corrupt extends Error {}
class DecodeError extends Error {}
class HostError extends Error {}
class ModuleFailure extends Error {}

class StepEnd {
  constructor(lines) {
    this.lines = lines;
  }
}

function assert(condition, message) {
  if (!condition) throw new Panic("assertion failed: " + message);
}

function decodeJson(text) {
  try {
    return JSON.parse(text, (key, value, context) => (typeof value === "number" && context !== undefined && /^-?[0-9]+$/.test(context.source) ? BigInt(context.source) : value));
  } catch (error) {
    if (error instanceof SyntaxError) throw new DecodeError(error.message);
    throw error;
  }
}

function encodeJson(value) {
  if (typeof value === "bigint") return value.toString();
  if (Array.isArray(value)) return "[" + value.map(encodeJson).join(",") + "]";
  if (value !== null && typeof value === "object") {
    return "{" + Object.keys(value).sort().map((key) => JSON.stringify(key) + ":" + encodeJson(value[key])).join(",") + "}";
  }
  return JSON.stringify(value);
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function smallNumber(value, what) {
  if (typeof value !== "bigint" || value < 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) throw new DecodeError(what + " is not a number");
  return Number(value);
}

function u64(value, what) {
  const number = BigInt(value);
  assert(number >= 0n && number <= U64_MAX, what + " is a u64");
  return number;
}

function linesOf(text) {
  assert(text.endsWith(LINE_END), "a flush ends at a line end");
  return text.slice(0, -LINE_END.length).split(LINE_END);
}

function tagged(line) {
  let json;
  try {
    json = decodeJson(line);
  } catch (error) {
    if (error instanceof DecodeError) throw new Corrupt("a journal line is not JSON: " + error.message);
    throw error;
  }
  if (!isObject(json) || Object.keys(json).length !== 1) throw new Corrupt("a record is an object with one tag");
  const tag = Object.keys(json)[0];
  return { tag, body: json[tag] };
}

function resumeFrom(json) {
  if (!isObject(json) || typeof json.key !== "string" || typeof json.answer !== "string") throw new DecodeError("a resume carries a text key and a text answer");
  return { key: json.key, answer: json.answer };
}

function resumeJson(resume) {
  return { answer: resume.answer, key: resume.key };
}

function suspensionFrom(json) {
  if (!isObject(json) || typeof json.key !== "string") throw new DecodeError("a suspension names its key");
  return { turn: smallNumber(json.turn, "a suspension's turn"), seq: smallNumber(json.seq, "a suspension's seq"), key: json.key };
}

function suspensionJson(suspension) {
  return { key: suspension.key, seq: BigInt(suspension.seq), turn: BigInt(suspension.turn) };
}

function finishJson(finish) {
  return { outcome: finish.outcome, result: finish.result, turn: BigInt(finish.turn) };
}

function finishFromJson(json) {
  return { turn: smallNumber(json.turn, "a finish's turn"), outcome: json.outcome, result: json.result };
}

function pointOf(point, kind, effect) {
  const name = POINTS[point];
  assert(name !== undefined, "a hook names a point");
  if (!RUN_POINTS.includes(name)) return { point: name };
  return { point: name, kind: KINDS[kind], effect: EFFECTS[effect] };
}

function callId(turn, seq) {
  return turn + "/" + seq;
}

class Session {
  constructor() {
    this.turns = new Map();
    this.lastTurn = 0;
  }

  apply(line) {
    const { tag, body } = tagged(line);
    if (tag === "admitted") {
      const turn = Number(BigInt(body.turn));
      this.turns.set(turn, { handler: body.digest, arguments: body.prompt, finished: null, calls: new Map() });
      this.lastTurn = Math.max(this.lastTurn, turn);
      return;
    }
    const log = isObject(body) && body.turn !== undefined ? this.turns.get(Number(BigInt(body.turn))) : undefined;
    if (log === undefined) return;
    if (tag === "call") log.calls.set(Number(body.seq), body);
    if (tag === "status") {
      const finished = isObject(body.status) ? body.status.finished : undefined;
      if (!isObject(finished)) throw new Corrupt("status record is not finished");
      log.finished = { outcome: finished.outcome, result: finished.result };
    }
  }
}

function folded(lines) {
  const session = new Session();
  for (const line of lines) session.apply(line);
  return session;
}

function defaultHost(config) {
  const post = async (url, text, signal) => {
    const response = await config.fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: text, signal });
    const body = await response.text();
    if (!response.ok) throw new HostError(url + " answered " + response.status + ": " + body);
    decodeJson(body);
    return body;
  };
  return {
    tool(text) {
      return post(config.toolsUrl, text);
    },
    listing(text) {
      return post(config.listingsUrl, text);
    },
    model(text) {
      return post(config.modelUrl, text);
    },
    now() {
      return BigInt(config.now());
    },
    clock() {
      return BigInt(config.now());
    },
    async sleep(turn, seq, untilMs) {
      const left = Number(untilMs) - Number(config.now());
      if (left > 0) await new Promise((resolve) => setTimeout(resolve, left));
      const clock = BigInt(config.now());
      return clock > untilMs ? clock : untilMs;
    },
    random() {
      return u64(config.random(), "random");
    },
  };
}

function platformRandom() {
  return crypto.getRandomValues(new BigUint64Array(1))[0];
}

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
let programModule = null;

function program() {
  programModule ??= new WebAssembly.Module(Uint8Array.from(atob(programBase64), (character) => character.charCodeAt(0)));
  return programModule;
}

class Instance {
  constructor(world) {
    const suspending = (run) => new WebAssembly.Suspending(run);
    const hl = {
      fail: (at, length) => {
        throw new ModuleFailure("the module failed: " + this.text(at, length));
      },
      hook: suspending((point, kind, effect) => world.hook(pointOf(point, kind, effect))),
      flush: suspending((epoch, at, length) => world.flush(epoch, this.text(at, length))),
      keep: (offset) => world.keep(offset),
      tool: suspending(async (at, length) => this.give(await world.tool(this.text(at, length)))),
      model: suspending(async (at, length) => this.give(await world.model(this.text(at, length)))),
      now: suspending(async () => u64(await world.now(), "now")),
      random: suspending(async () => u64(await world.random(), "random")),
      attempt: suspending(async (turn, seq) => u64(await world.attempt(turn, seq), "an attempt")),
      acknowledge: suspending((at, length) => world.acknowledge(this.text(at, length))),
      world: suspending(async (at, length) => this.give(await world.request(decodeJson(this.text(at, length))))),
    };
    this.exports = new WebAssembly.Instance(program(), { hl }).exports;
  }

  text(at, length) {
    return decoder.decode(new Uint8Array(this.exports.memory.buffer, at, length));
  }

  give(text) {
    const bytes = encoder.encode(text);
    const at = this.exports.hl_alloc(bytes.length);
    new Uint8Array(this.exports.memory.buffer, at, bytes.length).set(bytes);
    return (BigInt(at) << WORD_BITS) | BigInt(bytes.length);
  }

  taken(packed) {
    return this.text(Number(packed >> WORD_BITS), Number(packed & LOW_WORD));
  }

  async run(request, journal) {
    const requestAt = this.give(request);
    const journalAt = this.give(journal);
    const run = WebAssembly.promising(this.exports.hl_run);
    return decodeJson(this.taken(await run(Number(requestAt >> WORD_BITS), Number(requestAt & LOW_WORD), Number(journalAt >> WORD_BITS), Number(journalAt & LOW_WORD), EPOCH)));
  }

  async side(journal) {
    const journalAt = this.give(journal);
    const side = WebAssembly.promising(this.exports.hl_side);
    await side(Number(journalAt >> WORD_BITS), Number(journalAt & LOW_WORD), EPOCH);
  }

  idle(journal) {
    const journalAt = this.give(journal);
    return decodeJson(this.taken(this.exports.hl_idle(Number(journalAt >> WORD_BITS), Number(journalAt & LOW_WORD), EPOCH)));
  }

  check(request) {
    const requestAt = this.give(request);
    return decodeJson(this.taken(this.exports.hl_check(Number(requestAt >> WORD_BITS), Number(requestAt & LOW_WORD))));
  }

  facts() {
    return decodeJson(this.taken(this.exports.hl_facts()));
  }
}

const QUIET_WORLD = {};
let programFacts = null;

function facts() {
  programFacts ??= new Instance(QUIET_WORLD).facts();
  return programFacts;
}

function journalOf(lines) {
  return lines.map((line) => line + LINE_END).join("");
}

function checkedRequest(text) {
  const checked = new Instance(QUIET_WORLD).check(text);
  if (checked.error !== undefined) throw new Corrupt(checked.error);
  const json = decodeJson(text);
  return { text, turn: smallNumber(json.turn, "a turn request's turn"), handler: json.handler };
}

function suspensionIn(lines, exit) {
  const turn = Number(exit.turn);
  const seq = Number(exit.seq);
  const call = folded(lines).turns.get(turn)?.calls.get(seq);
  const key = call !== undefined && isObject(call.input) && typeof call.input.key === "string" ? call.input.key : "";
  return { turn, seq, key };
}

class StepWorld {
  constructor(context, delivered) {
    this.context = context;
    this.delivered = delivered;
    this.resuming = null;
    this.asked = null;
    this.sideAnswer = null;
  }

  hook(point) {
    return this.context.hook(point);
  }

  flush(epoch, text) {
    assert(epoch === EPOCH, "a workflow run has one owner");
    throw new StepEnd(linesOf(text));
  }

  keep() {
    throw new Panic("a workflow's journal is whole lines, so no journal read keeps a partial tail");
  }

  tool(text) {
    return this.context.host.tool(text);
  }

  model(text) {
    return this.context.host.model(text);
  }

  now() {
    return this.context.host.now();
  }

  random() {
    return this.context.host.random();
  }

  attempt() {
    const attempt = this.context.stepAttempt !== undefined ? this.context.stepAttempt(false) : this.context.attempt;
    assert(Number.isSafeInteger(attempt) && attempt >= 1, "a step's attempt counts from 1");
    return attempt;
  }

  acknowledge() {
    throw new Panic("a step ends at its flush, before the turn is acknowledged");
  }

  async request(request) {
    const host = this.context.host;
    switch (request.event) {
      case "batch":
        return this.batch(request);
      case "clock":
        return encodeJson({ answer: u64(await host.clock(), "the clock") });
      case "sleep":
        return encodeJson({ clock: u64(await host.sleep(Number(request.turn), Number(request.seq), request.until), "the clock") });
      case "resume": {
        const suspension = { turn: Number(request.turn), seq: Number(request.seq) };
        const resume = this.context.resume !== undefined ? await this.context.resume(suspension) : this.delivered;
        if (resume === null) return "null";
        this.resuming = { suspension, resume };
        return encodeJson(resumeJson(resume));
      }
      case "resumed": {
        assert(!request.matched, "a matched resume's flush ends the step before it is answered");
        assert(this.resuming !== null, "a resume is answered after it is taken");
        if (this.context.resumed !== undefined) await this.context.resumed(this.resuming.suspension, this.resuming.resume, request.matched);
        return "{}";
      }
      case "listing":
        return '{"output":' + (await host.listing(encodeJson({ listing: request.listing, seq: request.seq, turn: request.turn }))) + "}";
      case "side":
        return this.side(request);
      case "side_tool":
        return '{"output":' + (await host.tool(encodeJson({ input: request.input, nested: null, seq: request.seq, tool: request.tool, turn: 0n }))) + "}";
      case "side_model":
        throw new Panic("a side question's model request carries the context's checksum, not the context a model host needs");
      case "side_answer":
        return this.sided(request);
      case "inbox":
      case "answer":
        throw new Panic("a step takes and answers no events: the module asks for them inside its step, which a Vercel step cannot read");
      default:
        throw new Panic("the module made an unknown world request '" + request.event + "'");
    }
  }

  async side(request) {
    if (request.turn === null) {
      assert(this.asked !== null, "a side entry has its question");
      return encodeJson(this.asked);
    }
    const asked = this.context.side === undefined ? null : await this.context.side(Number(request.turn), Number(request.seq));
    this.asked = asked;
    return asked === null ? "null" : encodeJson(asked);
  }

  async sided(answer) {
    const { event, ...fields } = answer;
    this.sideAnswer = fields;
    if (this.context.sided !== undefined && this.asked !== null) await this.context.sided(this.asked, fields);
    return "{}";
  }

  async batch(request) {
    const limit = request.limit === null ? null : Number(request.limit);
    const requests = request.requests;
    const world = { hook: (point) => this.hook(point), tool: (call) => this.tool(encodeJson(call)) };
    const completed = this.context.tools !== undefined ? await this.context.tools(world, requests, limit) : await dispatchConcurrently(world, requests, limit);
    assert(completed.length === requests.length, "every request of a batch completes once");
    return '{"completed":[' + completed.map(([index, output]) => "[" + index + "," + output + "]").join(",") + "]}";
  }
}

async function dispatchConcurrently(world, requests, limit) {
  const completed = [];
  let next = 0;
  const worker = async () => {
    for (let taken = 0; taken < requests.length; taken++) {
      const index = next;
      if (index >= requests.length) return;
      next += 1;
      await world.hook({ point: "before_run", kind: "dispatch", effect: requests[index].effect });
      const output = await world.tool(requests[index]);
      await world.hook({ point: "after_run", kind: "dispatch", effect: requests[index].effect });
      completed.push([index, output]);
    }
  };
  const width = limit === null ? requests.length : Math.min(limit, requests.length);
  assert(width > 0, "a batch runs at least one request");
  const workers = [];
  for (let slot = 0; slot < width; slot++) workers.push(worker());
  await Promise.all(workers);
  return completed;
}

async function runStep(input, context) {
  if (input.kind === "acknowledge") {
    await context.acknowledge(finishFromJson(decodeJson(input.finish)));
    return { lines: [] };
  }
  if (input.kind === "idle") {
    const idle = new Instance(QUIET_WORLD).idle(journalOf(input.lines));
    if (idle.exit === "corrupt") throw new Corrupt(idle.error);
    return { lines: [], due: idle.due === null ? null : idle.due.toString() };
  }
  assert(input.kind === "run", "a step acknowledges, reads the idle due or runs");
  const request = checkedRequest(input.request);
  const delivered = input.resume === undefined || input.resume === null ? null : resumeFrom(decodeJson(input.resume));
  const world = new StepWorld(context, delivered);
  let exit;
  try {
    exit = await new Instance(world).run(request.text, journalOf(input.lines));
  } catch (error) {
    if (!(error instanceof StepEnd)) throw error;
    const output = { lines: error.lines, deadlines: [] };
    const answered = world.resuming;
    if (answered !== null && error.lines.some((line) => {
      const { tag, body } = tagged(line);
      return tag === "result" && body.call_id === callId(answered.suspension.turn, answered.suspension.seq);
    })) {
      const suspension = suspensionIn(input.lines, answered.suspension);
      output.resumed = encodeJson({ resume: resumeJson(answered.resume), suspension: suspensionJson(suspension) });
    }
    return output;
  }
  if (exit.exit === "corrupt") throw new Corrupt(exit.error);
  if (exit.exit === "suspended") return { lines: [], suspended: encodeJson(suspensionJson(suspensionIn(input.lines, exit))) };
  if (exit.exit === "not_due") return { lines: [], notDue: true };
  throw new Panic("a step ends at a flush, found the turn " + exit.exit);
}

async function side(lines, requestText, host) {
  const question = decodeJson(requestText);
  if (!isObject(question) || typeof question.handler !== "string" || !facts().sides.includes(question.handler)) throw new Corrupt("a side question names a side handler of the program");
  const world = new StepWorld({ host, hook() {} }, null);
  world.asked = { arguments: question.arguments ?? null, handler: question.handler };
  await new Instance(world).side(journalOf(lines));
  const answer = world.sideAnswer;
  assert(answer !== null, "a side question is answered");
  if (answer.error !== undefined) throw new Corrupt(answer.error);
  return encodeJson({ outcome: answer.outcome, result: answer.result });
}

async function session(platform) {
  const known = facts();
  if (known.events) throw new Panic("this module takes events inside its step, which the fixed Vercel host cannot deliver");
  const lines = (platform.entry ?? []).slice();
  const state = folded(lines);
  for (let index = 0; index < platform.turnsMax; index++) {
    const requestText = known.idle === null ? await platform.request(index) : await idleOrRequest(platform, lines, state, index, known.idle);
    await driveTurn(platform, lines, state, requestText, known);
  }
}

async function idleOrRequest(platform, lines, state, index, idle) {
  const done = await platform.step("idle " + index, { kind: "idle", lines: lines.slice() });
  if (done.output.due === null) return platform.request(index);
  const request = encodeJson({ arguments: {}, handler: idle, turn: BigInt(state.lastTurn + 1) });
  return platform.requestOrIdle(index, BigInt(done.output.due), request);
}

async function driveTurn(platform, lines, state, requestText, known) {
  const request = checkedRequest(requestText);
  const handler = known.handlers.find((each) => each.name === request.handler);
  assert(handler !== undefined, "a checked request names a handler of the program");
  const stepsMax = (STEPS_PER_EFFECT_MAX + STEPS_PER_SUSPENSION_MAX) * Number(handler.effects_max) + STEPS_PER_TURN_FIXED;
  let live = false;
  let resume = null;
  for (let step = 0; step < stepsMax; step++) {
    const log = state.turns.get(request.turn);
    if (log !== undefined && log.finished !== null) {
      if (live) await platform.hook(POINT_BEFORE_ACKNOWLEDGE);
      const finish = { turn: request.turn, outcome: log.finished.outcome, result: log.finished.result };
      await platform.step("acknowledge " + request.turn, { kind: "acknowledge", finish: encodeJson(finishJson(finish)) });
      return;
    }
    const done = await platform.step("turn " + request.turn + " step " + step, { kind: "run", lines: lines.slice(), request: requestText, resume });
    resume = null;
    if (done.output.notDue === true) return;
    if (done.output.suspended !== undefined) {
      const suspension = suspensionFrom(decodeJson(done.output.suspended));
      resume = await platform.resume("turn " + request.turn + " resume " + suspension.seq + " at step " + step, suspension);
      continue;
    }
    assert(done.output.lines.length > 0, "a run step ends at a flush of at least one record");
    for (const line of done.output.lines) {
      lines.push(line);
      state.apply(line);
    }
    live = done.live;
    if (live) await platform.hook(POINT_AFTER_FLUSH);
    if (live && done.output.resumed !== undefined) {
      const answered = decodeJson(done.output.resumed);
      await platform.resumed(suspensionFrom(answered.suspension), resumeFrom(answered.resume), true);
    }
  }
  throw new Panic("turn " + request.turn + " did not finish within its " + stepsMax + " steps");
}

const harness = {
  target: "vercel",
  format: "wasm",
  runtime: { Panic, Corrupt, DecodeError, HostError, ModuleFailure, encodeJson, decodeJson, finishJson, defaultHost, resumeJson, resumeFrom },
  facts,
  session,
  side,
  runStep,
  platformRandom,
};

export { harness };
