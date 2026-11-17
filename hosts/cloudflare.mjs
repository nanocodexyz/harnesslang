import program from "./program.wasm";

const EPOCH = 0n;
const JOURNAL_SCHEMA = "CREATE TABLE IF NOT EXISTS hl_journal (position INTEGER PRIMARY KEY, record TEXT NOT NULL)";
const JOURNAL_INSERT = "INSERT INTO hl_journal (position, record) VALUES (?, ?)";
const JOURNAL_LOAD = "SELECT position, record FROM hl_journal ORDER BY position";
const ALARM_RESUME_DELAY_MS = 1000;
const HARNESS_BINDING = "HARNESS";
const TURN_PATH = "/turn";
const EVENT_PATH = "/event";
const RESUME_PATH = "/resume";
const SIDE_PATH = "/side";
const SESSION_PATH = /^\/sessions\/([^/]+)\/(turn|event|resume|side)$/;
const SESSION_PATHS = { turn: TURN_PATH, event: EVENT_PATH, resume: RESUME_PATH, side: SIDE_PATH };
const RAN_HEADER = "hl-ran";
const HTTP_BAD_REQUEST = 400;
const HTTP_NOT_FOUND = 404;
const HTTP_INTERNAL_ERROR = 500;
const HTTP_SERVICE_UNAVAILABLE = 503;
const REQUESTS_QUEUED_MAX = 16;
const LINE_END = "\n";
const WORD_BITS = 32n;
const LOW_WORD = 0xffffffffn;
const U64_MAX = 0xffffffffffffffffn;
const POINTS = ["before_flush", "after_flush", "before_run", "after_run", "before_acknowledge", "cell_before_header", "cell_after_header", "cell_entry", "cell_before_nested", "cell_after_nested", "cell_waiting", "cell_before_finished", "cell_after_finished"];
const RUN_POINTS = ["before_run", "after_run"];
const NESTED_POINTS = ["cell_before_nested", "cell_after_nested"];
const KINDS = ["tool", "dispatch", "model", "now", "random", "listing", "cell", "wait", "hold", "suspend", "deny"];
const EFFECTS = ["none", "idempotent", "external"];
const DELIVERY_FIELDS = { cancel: ["id"], steer: ["id", "message"], job: ["done", "id", "key", "notice"] };
const FINISHED_EXITS = ["finished", "already_finished"];
const NO_SIMULATION = { mutation: "none", hook() {}, gate() {}, tools: undefined };
const WIRE_OPEN = "${";
const WIRE_DEFAULT_MARK = ":-";

class Panic extends Error {}
class Corrupt extends Error {}
class DecodeError extends Error {}
class HostError extends Error {}
class ModuleFailure extends Error {}

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
  const json = decodeJson(line);
  assert(isObject(json) && Object.keys(json).length === 1, "a record is an object with one tag");
  const tag = Object.keys(json)[0];
  return { tag, body: json[tag] };
}

function deliveryChecked(json) {
  if (!isObject(json) || Object.keys(json).length !== 1) throw new DecodeError("event " + encodeJson(json) + " is not a tagged object");
  const kind = Object.keys(json)[0];
  const fields = DELIVERY_FIELDS[kind];
  const body = json[kind];
  if (fields === undefined) throw new DecodeError("unknown event '" + kind + "'");
  if (!isObject(body) || encodeJson(Object.keys(body).sort()) !== encodeJson(fields)) throw new DecodeError("a " + kind + " event has exactly " + fields.join(", "));
  if (typeof body.id !== "string") throw new DecodeError("a " + kind + " event's id is text");
  if (kind === "steer" && typeof body.message !== "string") throw new DecodeError("a steer's message is text");
  if (kind === "job" && (typeof body.key !== "string" || typeof body.notice !== "string" || typeof body.done !== "boolean")) throw new DecodeError("a job event carries its key, notice and done");
  return { kind, id: body.id, json };
}

function deliveryJson(delivery) {
  if (delivery.json !== undefined) return delivery.json;
  const fields = { id: delivery.id };
  if (delivery.kind === "steer") fields.message = delivery.message;
  if (delivery.kind === "job") Object.assign(fields, { done: delivery.done, key: delivery.key, notice: delivery.notice });
  return { [delivery.kind]: fields };
}

function resumeFrom(json) {
  if (!isObject(json) || typeof json.key !== "string" || typeof json.answer !== "string") throw new DecodeError("a resume carries a text key and a text answer");
  return { key: json.key, answer: json.answer };
}

function finishJson(finish) {
  return { outcome: finish.outcome, result: finish.result, turn: BigInt(finish.turn) };
}

function finishFrom(text) {
  const json = decodeJson(text);
  return { turn: smallNumber(json.turn, "a finish's turn"), outcome: json.outcome, result: json.result, text };
}

function pointOf(point, kind, effect) {
  const name = POINTS[point];
  assert(name !== undefined, "a hook names a point");
  if (NESTED_POINTS.includes(name)) return { point: name, effect: EFFECTS[effect] };
  if (!RUN_POINTS.includes(name)) return { point: name };
  return { point: name, kind: KINDS[kind], effect: EFFECTS[effect] };
}

class Session {
  constructor(rows) {
    this.turns = new Map();
    this.lastTurn = 0;
    for (const row of rows) this.apply(tagged(row));
  }

  apply({ tag, body }) {
    if (tag === "admitted") {
      const turn = Number(BigInt(body.turn));
      this.turns.set(turn, { handler: body.digest, arguments: body.prompt, finished: false, events: [], calls: new Map() });
      this.lastTurn = Math.max(this.lastTurn, turn);
      return;
    }
    const log = body !== null && typeof body === "object" && body.turn !== undefined ? this.turns.get(Number(BigInt(body.turn))) : undefined;
    if (log === undefined) return;
    if (tag === "call") log.calls.set(Number(body.seq), body);
    if (tag === "status") log.finished = true;
    if (tag === "event" && isObject(body.event)) log.events.push(Object.values(body.event)[0]?.id);
  }
}

function wireExpand(text, lookup) {
  let out = "";
  let rest = text;
  for (let start = rest.indexOf(WIRE_OPEN); start >= 0; start = rest.indexOf(WIRE_OPEN)) {
    out += rest.slice(0, start);
    const after = rest.slice(start + WIRE_OPEN.length);
    const end = after.indexOf("}");
    if (end < 0) throw new HostError("an unclosed " + WIRE_OPEN + " in " + JSON.stringify(text));
    const inner = after.slice(0, end);
    const mark = inner.indexOf(WIRE_DEFAULT_MARK);
    const name = mark < 0 ? inner : inner.slice(0, mark);
    if (!/^[A-Za-z0-9_]+$/.test(name)) throw new HostError(WIRE_OPEN + inner + "} does not name an environment variable");
    const value = lookup(name);
    if (typeof value === "string" && value !== "") out += value;
    else if (mark >= 0) out += inner.slice(mark + WIRE_DEFAULT_MARK.length);
    else throw new HostError("the environment has no " + name);
    rest = after.slice(end + 1);
  }
  return out + rest;
}

function wireText(value, what) {
  if (typeof value !== "string") throw new HostError(what + " is not text: " + encodeJson(value ?? null));
  return value;
}

async function wireCall(fetcher, lookup, http) {
  let response;
  let body;
  try {
    const listed = http.headers;
    if (!Array.isArray(listed)) throw new HostError("a wire request's headers are not a list: " + encodeJson(listed ?? null));
    const headers = listed.map((pair) => {
      if (!Array.isArray(pair) || pair.length !== 2) throw new HostError("a header is not a [name, value] pair: " + encodeJson(pair));
      return [wireExpand(wireText(pair[0], "a header's name"), lookup), wireExpand(wireText(pair[1], "a header's value"), lookup)];
    });
    const url = wireExpand(wireText(http.url, "a wire request's url"), lookup);
    const init = { method: wireText(http.method, "a wire request's method"), headers, body: wireText(http.body, "a wire request's body") };
    response = await fetcher(url, init);
    body = await response.text();
  } catch (error) {
    if (error instanceof Panic) throw error;
    return { transport: String(error?.message ?? error) };
  }
  return { body, headers: [...response.headers], status: BigInt(response.status) };
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
    async model(text) {
      const request = decodeJson(text);
      if (!isObject(request) || request.http === undefined) return post(config.modelUrl, text);
      return encodeJson(await wireCall(config.fetch, (name) => config.env?.[name], request.http));
    },
    recover(text) {
      return config.recoverUrl === undefined ? '"unknown"' : post(config.recoverUrl, text);
    },
    jobs(text) {
      return config.jobsUrl === undefined ? "[]" : post(config.jobsUrl, text);
    },
    cell(text) {
      if (config.cellsUrl === undefined) throw new Panic("this module runs code cells, and HL_CELLS_URL names no cell engine");
      return post(config.cellsUrl, text);
    },
    wait(cell, timer, untilMs) {
      return this.sleep(0, 0, untilMs);
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
    hold(turn, seq, untilMs) {
      return this.sleep(turn, seq, untilMs);
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
    this.exports = new WebAssembly.Instance(program, { hl }).exports;
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
    return this.taken(await run(Number(requestAt >> WORD_BITS), Number(requestAt & LOW_WORD), Number(journalAt >> WORD_BITS), Number(journalAt & LOW_WORD), EPOCH));
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

class CloudflareWorld {
  constructor(object) {
    this.object = object;
    this.deferred = [];
    this.finish = null;
    this.resuming = null;
    this.asked = null;
    this.sideAnswer = null;
  }

  hook(point) {
    return this.object.simulation.hook(point);
  }

  beforeExternal() {
    if (this.object.mutation !== "cloudflare_no_alarm") this.object.arm();
  }

  append(line) {
    const { tag, body } = tagged(line);
    const external = tag === "call" && body.effect === "external";
    if (external) this.beforeExternal();
    const late = this.object.mutation === "cloudflare_fetch_before_insert" && external;
    if (late) {
      this.deferred.push(line);
      return;
    }
    this.object.insert(line);
    if (tag === "status") this.object.disarm();
  }

  async flush(epoch, text) {
    assert(epoch === EPOCH, "a durable object has one owner");
    for (const line of linesOf(text)) this.append(line);
    await this.object.settle();
    await this.object.simulation.gate(text);
  }

  keep() {
    throw new Panic("a Durable Object's rows are whole records, so no journal read keeps a partial tail");
  }

  async tool(text) {
    const output = await this.object.host.tool(text);
    for (const line of this.deferred.splice(0)) this.object.insert(line);
    return output;
  }

  model(text) {
    return this.object.host.model(text);
  }

  now() {
    return this.object.host.now();
  }

  random() {
    return this.object.host.random();
  }

  attempt() {
    throw new Panic("cloudflare recovers at load and counts no step attempts");
  }

  acknowledge(text) {
    assert(this.finish === null, "a run acknowledges its turn once");
    this.finish = finishFrom(text);
  }

  async run(request, journal) {
    return decodeJson(await new Instance(this).run(request, journal));
  }

  async request(request) {
    const simulation = this.object.simulation;
    const host = this.object.host;
    switch (request.event) {
      case "batch":
        return this.batch(request);
      case "inbox": {
        const turn = Number(request.turn);
        const arrived = simulation.inbox !== undefined ? (await simulation.inbox(turn)).map(deliveryJson) : this.object.arrived(turn);
        return encodeJson({ answer: arrived });
      }
      case "answer": {
        const answers = request.answers.map((answer) => [answer.id, answer.answer]);
        if (simulation.answer !== undefined) await simulation.answer(Number(request.turn), Number(request.position), answers);
        else this.object.answered(Number(request.turn), answers);
        return "{}";
      }
      case "clock":
        return encodeJson({ answer: u64(await host.clock(), "the clock") });
      case "sleep":
        return encodeJson({ clock: u64(await host.sleep(Number(request.turn), Number(request.seq), request.until), "the clock") });
      case "hold":
        return encodeJson({ clock: u64(await host.hold(Number(request.turn), Number(request.seq), request.until), "the clock") });
      case "cell_start":
      case "cell_resume":
      case "cell":
        return host.cell(encodeJson(request));
      case "tool":
        return '{"output":' + (await host.tool(encodeJson({ effect: request.effect, input: request.input, nested: request.nested, seq: request.seq, tool: request.tool, turn: request.turn }))) + "}";
      case "wait":
        await host.wait(Number(request.cell), Number(request.timer), request.until);
        return "{}";
      case "resume": {
        const suspension = { turn: Number(request.turn), seq: Number(request.seq) };
        const resume = simulation.resume !== undefined ? await simulation.resume(suspension) : this.object.pendingResume(suspension);
        if (resume === null) return "null";
        this.resuming = { suspension, resume };
        return encodeJson({ answer: resume.answer, key: resume.key });
      }
      case "resumed": {
        assert(this.resuming !== null, "a resume is answered after it is taken");
        const { suspension, resume } = this.resuming;
        if (simulation.resumed !== undefined) await simulation.resumed(suspension, resume, request.matched);
        else this.object.answerResume(suspension, request.matched);
        return "{}";
      }
      case "listing":
        return '{"output":' + (await host.listing(encodeJson({ listing: request.listing, seq: request.seq, turn: request.turn }))) + "}";
      case "recover":
        return '{"answer":' + (await host.recover(encodeJson({ input: request.input, seq: request.seq, tool: request.tool, turn: request.turn }))) + "}";
      case "jobs":
        return '{"answer":' + (await host.jobs(encodeJson({ open: request.open, turn: request.turn }))) + "}";
      case "side":
        return this.side(request);
      case "side_tool":
        return '{"output":' + (await host.tool(encodeJson({ input: request.input, nested: null, seq: request.seq, tool: request.tool, turn: 0n }))) + "}";
      case "side_answer":
        return this.sided(request);
      default:
        throw new Panic("the module made an unknown world request '" + request.event + "'");
    }
  }

  async side(request) {
    if (request.turn === null) {
      assert(this.asked !== null, "a side entry has its question");
      return encodeJson(this.asked);
    }
    const asked = this.object.simulation.side === undefined ? null : await this.object.simulation.side(Number(request.turn), Number(request.seq));
    this.asked = asked;
    this.askedInRun = asked !== null;
    return asked === null ? "null" : encodeJson(asked);
  }

  async sided(answer) {
    const { event, ...fields } = answer;
    this.sideAnswer = fields;
    if (this.object.simulation.sided !== undefined && this.askedInRun === true) await this.object.simulation.sided(this.asked, fields);
    return "{}";
  }

  async batch(request) {
    const limit = request.limit === null ? null : Number(request.limit);
    const requests = request.requests;
    const simulated = this.object.simulation.tools;
    const world = { hook: (point) => this.hook(point), tool: (call) => this.tool(encodeJson(call)) };
    const completed = simulated !== undefined ? await simulated(world, requests, limit) : await dispatchConcurrently(world, requests, limit);
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

class HarnessObject {
  constructor(ctx, env) {
    this.storage = ctx.storage;
    this.simulation = env.HL_SIMULATION ?? NO_SIMULATION;
    this.mutation = this.simulation.mutation;
    this.host = env.HL_HOST ?? defaultHost({
      fetch: (url, init) => fetch(url, init),
      toolsUrl: env.HL_TOOLS_URL,
      listingsUrl: env.HL_LISTINGS_URL,
      modelUrl: env.HL_MODEL_URL,
      recoverUrl: env.HL_RECOVER_URL,
      jobsUrl: env.HL_JOBS_URL,
      cellsUrl: env.HL_CELLS_URL,
      env,
      now: () => Date.now(),
      random: platformRandom,
    });
    this.name = ctx.id === undefined ? "session" : String(ctx.id);
    this.facts = facts();
    this.storage.sql.exec(JOURNAL_SCHEMA);
    this.position = 0;
    this.armed = false;
    this.running = false;
    this.queued = 0;
    this.tail = Promise.resolve();
    this.storageWrites = [];
    this.runningTurn = null;
    this.waiting = [];
    this.resumes = [];
  }

  pendingResume(suspension) {
    const entry = this.resumes.find((waiting) => waiting.turn === suspension.turn);
    return entry === undefined ? null : entry.resume;
  }

  answerResume(suspension, matched) {
    const index = this.resumes.findIndex((waiting) => waiting.turn === suspension.turn);
    if (index >= 0) this.resumes.splice(index, 1)[0].resolve(matched ? "recorded" : "unmatched");
  }

  async deliverResume(request) {
    let turn;
    let resume;
    try {
      const body = decodeJson(await request.text());
      turn = smallNumber(body.turn, "a resume's turn");
      resume = resumeFrom(body);
    } catch (error) {
      if (error instanceof DecodeError) return new Response(error.message, { status: HTTP_BAD_REQUEST });
      throw error;
    }
    if (this.resumes.length > 0) return new Response("a resume is already being delivered", { status: HTTP_SERVICE_UNAVAILABLE });
    const answered = new Promise((resolve) => this.resumes.push({ turn, resume, resolve }));
    const driven = await this.serialized(() => this.resume());
    const index = this.resumes.findIndex((waiting) => waiting.resume === resume);
    if (index >= 0) this.resumes.splice(index, 1)[0].resolve(driven === null ? "busy" : "not_waiting");
    const answer = await answered;
    const status = answer === "recorded" ? 200 : HTTP_SERVICE_UNAVAILABLE;
    return new Response(encodeJson({ answer }), { status, headers: { "content-type": "application/json" } });
  }

  arrived(turn) {
    return this.waiting.filter((entry) => entry.turn === turn || entry.delivery.kind === "job").map((entry) => entry.delivery.json);
  }

  answered(turn, answers) {
    for (const [id, answer] of answers) {
      const index = this.waiting.findIndex((entry) => entry.turn === turn && entry.delivery.id === id);
      if (index >= 0) this.waiting.splice(index, 1)[0].resolve(answer);
      else {
        const job = this.waiting.findIndex((entry) => entry.delivery.kind === "job" && entry.delivery.id === id);
        if (job >= 0) this.waiting.splice(job, 1)[0].resolve(answer);
      }
    }
  }

  async deliver(request) {
    let turn;
    let delivery;
    try {
      const body = decodeJson(await request.text());
      delivery = deliveryChecked(body.event);
      turn = delivery.kind === "job" && body.turn === undefined ? null : smallNumber(body.turn, "an event's turn");
    } catch (error) {
      if (error instanceof DecodeError) return new Response(error.message, { status: HTTP_BAD_REQUEST });
      throw error;
    }
    const answered = (answer) => new Response(encodeJson({ answer }), { headers: { "content-type": "application/json" } });
    if (delivery.kind === "job") return this.deliverJob(turn, delivery, answered);
    if (this.runningTurn !== turn) {
      const log = new Session(this.load()).turns.get(turn);
      if (log === undefined || !log.finished) return new Response("not_running", { status: HTTP_SERVICE_UNAVAILABLE });
      return answered(log.events.includes(delivery.id) ? "duplicate" : "finished");
    }
    if (BigInt(this.waiting.length) >= this.facts.events_max) {
      return new Response(this.waiting.length + " events already wait for turn " + turn, { status: HTTP_SERVICE_UNAVAILABLE });
    }
    const answer = await new Promise((resolve) => this.waiting.push({ turn, delivery, resolve }));
    if (answer === "not_running") return new Response(answer, { status: HTTP_SERVICE_UNAVAILABLE });
    return answered(answer);
  }

  async deliverJob(turn, delivery, answered) {
    if (BigInt(this.waiting.length) >= this.facts.events_max) {
      return new Response(this.waiting.length + " events already wait for this object", { status: HTTP_SERVICE_UNAVAILABLE });
    }
    const answer = await new Promise((resolve) => this.waiting.push({ turn, delivery, resolve }));
    return answered(answer);
  }

  async serialized(work) {
    if (this.queued >= REQUESTS_QUEUED_MAX) return null;
    this.queued += 1;
    const previous = this.tail;
    let release;
    this.tail = new Promise((resolve) => {
      release = resolve;
    });
    try {
      await previous;
      return await work();
    } finally {
      this.queued -= 1;
      release();
    }
  }

  load() {
    const rows = this.storage.sql.exec(JOURNAL_LOAD).toArray();
    const records = rows.map((row, index) => {
      if (Number(row.position) !== index + 1) throw new Corrupt("journal row " + (index + 1) + " has position " + row.position);
      return row.record;
    });
    this.position = records.length;
    return records;
  }

  journal() {
    return this.load().map((record) => record + LINE_END).join("");
  }

  async side(request) {
    let question;
    try {
      question = decodeJson(await request.text());
      if (!isObject(question) || typeof question.handler !== "string") throw new DecodeError("a side question names its side handler");
      if (!this.facts.sides.includes(question.handler)) throw new DecodeError("no side handler '" + question.handler + "'");
    } catch (error) {
      if (error instanceof DecodeError) return new Response(error.message, { status: HTTP_BAD_REQUEST });
      throw error;
    }
    try {
      const world = new CloudflareWorld(this);
      world.asked = { arguments: question.arguments ?? null, handler: question.handler };
      await new Instance(world).side(this.journal());
      const answer = world.sideAnswer;
      assert(answer !== null, "a side question is answered");
      if (answer.error !== undefined) return new Response(answer.error, { status: HTTP_INTERNAL_ERROR });
      return new Response(encodeJson({ outcome: answer.outcome, result: answer.result }), { headers: { "content-type": "application/json" } });
    } catch (error) {
      if (error instanceof Corrupt) return new Response(error.message, { status: HTTP_INTERNAL_ERROR });
      throw error;
    }
  }

  insert(record) {
    this.position += 1;
    this.storage.sql.exec(JOURNAL_INSERT, this.position, record);
  }

  arm() {
    if (this.armed) return;
    this.armed = true;
    this.storageWrites.push(this.storage.setAlarm(Date.now() + ALARM_RESUME_DELAY_MS));
  }

  disarm() {
    this.armed = false;
    this.storageWrites.push(this.storage.deleteAlarm());
  }

  async settle() {
    await Promise.all(this.storageWrites.splice(0));
    await this.storage.sync();
  }

  suspension(exit, records) {
    const turn = Number(exit.turn);
    const seq = Number(exit.seq);
    const call = new Session(records).turns.get(turn)?.calls.get(seq);
    const key = call !== undefined && isObject(call.input) && typeof call.input.key === "string" ? call.input.key : "";
    return { key, seq: BigInt(seq), turn: BigInt(turn) };
  }

  async drive(request) {
    assert(!this.running, "one turn runs at a time in an object");
    this.running = true;
    this.runningTurn = request.turn;
    try {
      const world = new CloudflareWorld(this);
      const exit = await world.run(request.text, this.journal());
      if (exit.exit === "corrupt") throw new Corrupt(exit.error);
      if (exit.exit === "suspended") {
        this.disarm();
        await this.settle();
        return { ran: exit.exit, suspension: this.suspension(exit, this.load()) };
      }
      if (exit.exit === "not_due") {
        await this.settle();
        return { ran: exit.exit };
      }
      assert(FINISHED_EXITS.includes(exit.exit), "a run finishes, finds its turn finished, suspends, is not due or finds the journal corrupt");
      if (exit.exit === "already_finished") world.acknowledge(encodeJson(exit.finish));
      await this.settle();
      assert(world.finish !== null, "a driven turn is acknowledged");
      await this.armIdle();
      return { ran: exit.exit, finish: world.finish };
    } catch (error) {
      this.armed = false;
      await Promise.allSettled(this.storageWrites.splice(0));
      throw error;
    } finally {
      this.running = false;
      this.runningTurn = null;
      const jobs = this.waiting.filter((entry) => entry.delivery.kind === "job");
      for (const entry of this.waiting.splice(0)) {
        if (entry.delivery.kind !== "job") entry.resolve("not_running");
      }
      this.waiting.push(...jobs);
    }
  }

  turnRequest(text) {
    const checked = new Instance(QUIET_WORLD).check(text);
    if (checked.error !== undefined) throw new DecodeError(checked.error);
    return { text, turn: smallNumber(decodeJson(text).turn, "a turn request's turn") };
  }

  async fetch(request) {
    const url = new URL(request.url);
    if (request.method === "POST" && url.pathname === EVENT_PATH) return this.deliver(request);
    if (request.method === "POST" && url.pathname === RESUME_PATH) return this.deliverResume(request);
    if (request.method === "POST" && url.pathname === SIDE_PATH) return this.side(request);
    if (request.method !== "POST" || url.pathname !== TURN_PATH) return new Response("not found", { status: HTTP_NOT_FOUND });
    let turn;
    try {
      turn = this.turnRequest(await request.text());
    } catch (error) {
      if (error instanceof DecodeError) return new Response(error.message, { status: HTTP_BAD_REQUEST });
      throw error;
    }
    try {
      const driven = await this.serialized(() => this.drive(turn));
      if (driven === null) return new Response("REQUESTS_QUEUED_MAX (" + REQUESTS_QUEUED_MAX + ") requests already wait for this object", { status: HTTP_SERVICE_UNAVAILABLE });
      const body = driven.ran === "suspended" ? encodeJson(driven.suspension) : driven.finish.text;
      return new Response(body, { headers: { "content-type": "application/json", [RAN_HEADER]: driven.ran } });
    } catch (error) {
      if (error instanceof Corrupt) return new Response(error.message, { status: HTTP_INTERNAL_ERROR });
      throw error;
    }
  }

  async armIdle() {
    if (this.facts.idle === null) return;
    const idle = new Instance(QUIET_WORLD).idle(this.journal());
    if (idle.exit === "corrupt") throw new Corrupt(idle.error);
    if (idle.due === null) return;
    this.storageWrites.push(this.storage.setAlarm(Number(idle.due)));
    await this.settle();
  }

  async woken() {
    const resumed = await this.resume();
    if (resumed.ran !== "idle" || this.facts.idle === null) return resumed;
    try {
      const session = new Session(this.load());
      const text = encodeJson({ arguments: {}, handler: this.facts.idle, turn: BigInt(session.lastTurn + 1) });
      return await this.drive({ text, turn: session.lastTurn + 1 });
    } catch (error) {
      if (error instanceof Corrupt) return { ran: "corrupt", message: error.message };
      throw error;
    }
  }

  async alarm() {
    const resumed = await this.serialized(() => this.woken());
    if (resumed !== null) return resumed;
    await this.storage.setAlarm(Date.now() + ALARM_RESUME_DELAY_MS);
    return { ran: "busy" };
  }

  async resume() {
    try {
      const session = new Session(this.load());
      const log = session.turns.get(session.lastTurn);
      if (log === undefined || log.finished) return { ran: "idle" };
      if (!this.facts.handlers.some((handler) => handler.name === log.handler)) throw new Corrupt("turn " + session.lastTurn + " names handler '" + log.handler + "', which the program lacks");
      const text = encodeJson({ arguments: log.arguments, handler: log.handler, turn: BigInt(session.lastTurn) });
      return await this.drive({ text, turn: session.lastTurn });
    } catch (error) {
      if (error instanceof Corrupt) return { ran: "corrupt", message: error.message };
      throw error;
    }
  }
}

const worker = {
  async fetch(request, env) {
    const url = new URL(request.url);
    const match = SESSION_PATH.exec(url.pathname);
    if (request.method !== "POST" || match === null) return new Response("not found", { status: HTTP_NOT_FOUND });
    const namespace = env[HARNESS_BINDING];
    const stub = namespace.get(namespace.idFromName(decodeURIComponent(match[1])));
    const path = SESSION_PATHS[match[2]];
    return stub.fetch(new URL(path, url.origin), { method: "POST", body: await request.text(), headers: { "content-type": "application/json" } });
  },
};

const harness = {
  target: "cloudflare",
  format: "wasm",
  runtime: { Panic, Corrupt, DecodeError, HostError, ModuleFailure, encodeJson, decodeJson, finishJson, defaultHost, deliveryJson, wireCall },
  facts,
  statements: { schema: JOURNAL_SCHEMA, insert: JOURNAL_INSERT, load: JOURNAL_LOAD },
  ranHeader: RAN_HEADER,
  turnPath: TURN_PATH,
  eventPath: EVENT_PATH,
  resumePath: RESUME_PATH,
  sidePath: SIDE_PATH,
};

export { HarnessObject, harness };
export default worker;
