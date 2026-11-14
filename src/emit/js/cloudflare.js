const CLOUDFLARE_TARGET = { ...TARGET_MODEL, inbound: "after_run" };
assert(CLOUDFLARE_TARGET.recovery === "at_load", "a Durable Object recovers open external calls when it loads");
assert(CLOUDFLARE_TARGET.gateBeforeOutbound, "a Durable Object's output gate holds outbound requests until writes commit");
const CLOUDFLARE_EPOCH = 0;
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
const NO_SIMULATION = { mutation: "none", hook() {}, gate() {}, tools: undefined };

class CloudflareWorld {
  constructor(object) {
    this.object = object;
    this.deferred = [];
    this.finish = null;
  }

  append(record) {
    const late = this.object.mutation === "cloudflare_fetch_before_insert" && record.kind === "call" && mayBeUnknown(record.effect);
    if (late) {
      this.deferred.push(record);
      return;
    }
    this.object.insert(record);
    if (record.kind === "status") this.object.disarm();
  }

  async hook(point) {
    await this.object.simulation.hook(point);
  }

  async beforeExternal() {
    if (this.object.mutation !== "cloudflare_no_alarm") this.object.arm();
  }

  async flush(epoch, records) {
    assert(epoch === CLOUDFLARE_EPOCH, "a durable object has one owner");
    await this.object.settle();
    await this.object.simulation.gate(records);
  }

  async attempt() {
    throw new Panic("cloudflare recovers at load and counts no step attempts");
  }

  async nestedAttempt() {
    throw new Panic("cloudflare recovers nested calls at load and counts no step attempts");
  }

  cellStart(cell, script, context) {
    return this.object.cells.start(cell, script, context);
  }

  cellResume(cell, input) {
    return this.object.cells.resume(cell, input);
  }

  async waited() {
    return false;
  }

  async wait(wait) {
    await this.object.host.wait(wait);
  }

  async observe(event) {
    await this.object.host.observe(event);
  }

  async tool(request) {
    const output = await this.object.host.tool(request);
    for (const record of this.deferred.splice(0)) this.object.insert(record);
    return output;
  }

  async listing(request) {
    const output = await this.object.host.listing(request);
    for (const record of this.deferred.splice(0)) this.object.insert(record);
    return output;
  }

  async tools(requests, limit) {
    const simulated = this.object.simulation.tools;
    if (simulated !== undefined) return simulated(this, requests, limit);
    return dispatchConcurrently(this, requests, limit);
  }

  model(request) {
    return this.object.host.model(request);
  }

  now() {
    return this.object.host.now();
  }

  random() {
    return this.object.host.random();
  }

  async acknowledge(finish) {
    assert(this.finish === null, "a run acknowledges its turn once");
    this.finish = finish;
  }

  async inbox(turn) {
    if (this.object.simulation.inbox !== undefined) return this.object.simulation.inbox(turn);
    return this.object.arrived(turn);
  }

  clock() {
    return this.object.host.clock();
  }

  sleep(turn, seq, untilMs) {
    return this.object.host.sleep(turn, seq, untilMs);
  }

  async resume(suspension) {
    if (this.object.simulation.resume !== undefined) return this.object.simulation.resume(suspension);
    return this.object.pendingResume(suspension);
  }

  async resumed(suspension, resume, matched) {
    if (this.object.simulation.resumed !== undefined) {
      await this.object.simulation.resumed(suspension, resume, matched);
      return;
    }
    this.object.answerResume(suspension, matched);
  }

  async answer(turn, position, answers) {
    if (this.object.simulation.answer !== undefined) {
      await this.object.simulation.answer(turn, position, answers);
      return;
    }
    this.object.answered(turn, answers);
  }

  async side(turn, seq) {
    if (this.object.simulation.side === undefined) return null;
    return this.object.simulation.side(turn, seq);
  }

  async sided(request, answer) {
    if (this.object.simulation.sided !== undefined) await this.object.simulation.sided(request, answer);
  }
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
      now: () => Date.now(),
      random: platformRandom,
    });
    this.name = ctx.id === undefined ? "session" : String(ctx.id);
    this.cells = env.HL_CELLS ?? new IsolateCells(env[LOADER_BINDING], this);
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
    return new Response(encodeJson(object([["answer", answer]])), { status, headers: { "content-type": "application/json" } });
  }

  arrived(turn) {
    return this.waiting.filter((entry) => entry.turn === turn).map((entry) => entry.delivery);
  }

  answered(turn, answers) {
    for (const [id, answer] of answers) {
      const index = this.waiting.findIndex((entry) => entry.turn === turn && entry.delivery.id === id);
      if (index >= 0) this.waiting.splice(index, 1)[0].resolve(answer);
    }
  }

  async deliver(request) {
    let turn;
    let delivery;
    try {
      const body = decodeJson(await request.text());
      turn = smallNumber(body.turn, "an event's turn");
      delivery = deliveryFrom(body.event);
    } catch (error) {
      if (error instanceof DecodeError) return new Response(error.message, { status: HTTP_BAD_REQUEST });
      throw error;
    }
    const answered = (answer) => new Response(encodeJson(object([["answer", answer]])), { headers: { "content-type": "application/json" } });
    if (this.runningTurn !== turn) {
      const log = fold(this.load()).turns.get(turn);
      if (log === undefined || log.finished === null) return new Response("not_running", { status: HTTP_SERVICE_UNAVAILABLE });
      return answered(closingAnswers(log.events, [delivery])[0][1]);
    }
    if (BigInt(this.waiting.length) >= PROGRAM.events.recordsMax) {
      return new Response(`${this.waiting.length} events already wait for turn ${turn}`, { status: HTTP_SERVICE_UNAVAILABLE });
    }
    const answer = await new Promise((resolve) => this.waiting.push({ turn, delivery, resolve }));
    if (answer === "not_running") return new Response(answer, { status: HTTP_SERVICE_UNAVAILABLE });
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

  rows() {
    const rows = this.storage.sql.exec(JOURNAL_LOAD).toArray();
    return rows.map((row, index) => {
      if (Number(row.position) !== index + 1) throw new Corrupt(`journal row ${index + 1} has position ${row.position}`);
      return decodeOrCorrupt(() => decodeLine(row.record, false));
    });
  }

  load() {
    const records = this.rows();
    this.position = records.length;
    return records;
  }

  async side(request) {
    let question;
    try {
      question = sideRequestFromJson(decodeJson(await request.text()));
    } catch (error) {
      if (error instanceof DecodeError) return new Response(error.message, { status: HTTP_BAD_REQUEST });
      throw error;
    }
    try {
      const answer = await answerSide({ target: CLOUDFLARE_TARGET, mutation: "none" }, new CloudflareWorld(this), this.rows(), question);
      return new Response(encodeJson(sideAnswerJson(answer)), { headers: { "content-type": "application/json" } });
    } catch (error) {
      if (error instanceof Corrupt) return new Response(error.message, { status: HTTP_INTERNAL_ERROR });
      throw error;
    }
  }

  insert(record) {
    this.position += 1;
    this.storage.sql.exec(JOURNAL_INSERT, this.position, encodeLine(record, false));
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

  async drive(request) {
    assert(!this.running, "one turn runs at a time in an object");
    this.running = true;
    this.runningTurn = request.turn;
    try {
      const records = this.load();
      const world = new CloudflareWorld(this);
      const ran = await runTurn({ target: CLOUDFLARE_TARGET, mutation: this.mutation }, world, CLOUDFLARE_EPOCH, records, request);
      if (ran.ran === "suspended") {
        this.disarm();
        await this.settle();
        return { ran: ran.ran, suspension: ran.suspension };
      }
      if (ran.ran === "not_due") {
        await this.settle();
        return { ran: ran.ran };
      }
      if (ran.ran === "already_finished") await world.acknowledge(ran.finish);
      await this.settle();
      assert(world.finish !== null, "a driven turn is acknowledged");
      await this.armIdle();
      return { ran: ran.ran, finish: world.finish };
    } catch (error) {
      this.armed = false;
      await Promise.allSettled(this.storageWrites.splice(0));
      throw error;
    } finally {
      this.running = false;
      this.runningTurn = null;
      for (const entry of this.waiting.splice(0)) entry.resolve("not_running");
    }
  }

  async fetch(request) {
    const url = new URL(request.url);
    if (request.method === "POST" && url.pathname === EVENT_PATH) return this.deliver(request);
    if (request.method === "POST" && url.pathname === RESUME_PATH) return this.deliverResume(request);
    if (request.method === "POST" && url.pathname === SIDE_PATH) return this.side(request);
    if (request.method !== "POST" || url.pathname !== TURN_PATH) return new Response("not found", { status: HTTP_NOT_FOUND });
    let turn;
    try {
      turn = turnRequestFromJson(decodeJson(await request.text()));
    } catch (error) {
      if (error instanceof DecodeError) return new Response(error.message, { status: HTTP_BAD_REQUEST });
      throw error;
    }
    try {
      const driven = await this.serialized(() => this.drive(turn));
      if (driven === null) return new Response(`REQUESTS_QUEUED_MAX (${REQUESTS_QUEUED_MAX}) requests already wait for this object`, { status: HTTP_SERVICE_UNAVAILABLE });
      const body = driven.ran === "suspended" ? suspensionJson(driven.suspension) : finishJson(driven.finish);
      return new Response(encodeJson(body), { headers: { "content-type": "application/json", [RAN_HEADER]: driven.ran } });
    } catch (error) {
      if (error instanceof Corrupt) return new Response(error.message, { status: HTTP_INTERNAL_ERROR });
      throw error;
    }
  }

  async armIdle() {
    const due = await idleDue({ target: CLOUDFLARE_TARGET, mutation: this.mutation }, this.load());
    if (due === null) return;
    this.storageWrites.push(this.storage.setAlarm(Number(due)));
    await this.settle();
  }

  async woken() {
    const resumed = await this.resume();
    if (resumed.ran !== "idle" || PROGRAM.idle === null) return resumed;
    try {
      const session = fold(this.load());
      return await this.drive({ turn: session.lastTurn + 1, handler: PROGRAM.idle.handler, arguments: [] });
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
      const session = fold(this.load());
      const log = session.turns.get(session.lastTurn);
      if (log === undefined || log.finished !== null) return { ran: "idle" };
      const handler = handlerNamed(log.handler);
      if (handler === null) throw new Corrupt(`turn ${session.lastTurn} names handler '${log.handler}', which the program lacks`);
      const args = decodeOrCorrupt(() => argumentsFromJson(handler, log.arguments));
      return await this.drive({ turn: session.lastTurn, handler: log.handler, arguments: args });
    } catch (error) {
      if (error instanceof Corrupt) return { ran: "corrupt", message: error.message };
      throw error;
    }
  }
}

const LOADER_BINDING = "LOADER";
const CELL_COMPATIBILITY_DATE = "2025-09-01";
const CELL_MAIN_MODULE = "main.js";
const CELL_SCRIPT_MODULE = "script.js";
const TERMINAL_STEPS = ["done", "threw"];

class StepChannel {
  constructor() {
    this.items = [];
    this.waiting = null;
    this.pending = null;
  }

  push(text) {
    return new Promise((resolve) => this.deliver({ text, resolve }));
  }

  deliver(item) {
    if (this.waiting === null) {
      this.items.push(item);
      return;
    }
    const waiting = this.waiting;
    this.waiting = null;
    waiting(item);
  }

  async next() {
    const item = this.items.length > 0 ? this.items.shift() : await new Promise((resolve) => {
      this.waiting = resolve;
    });
    const step = decodeOrCorrupt(() => engineStepFrom(decodeJson(item.text)));
    if (TERMINAL_STEPS.includes(step.kind)) item.resolve(null);
    else this.pending = item.resolve;
    return step;
  }

  answer(input) {
    assert(this.pending !== null, "an engine input answers a step the isolate is waiting on");
    const resolve = this.pending;
    this.pending = null;
    resolve(encodeJson(engineInputJson(input)));
  }
}

class IsolateCells {
  constructor(loader, object) {
    this.loader = loader;
    this.object = object;
    this.channels = new Map();
    this.generation = 0;
  }

  async start(cell, script, context) {
    if (this.loader === undefined) return { kind: "threw", error: `the object has no ${LOADER_BINDING} Worker Loader binding to run cells in` };
    this.generation += 1;
    const name = `${this.object.name}-cell-${cell}-${this.generation}`;
    const worker = this.loader.get(name, () => ({
      compatibilityDate: CELL_COMPATIBILITY_DATE,
      mainModule: CELL_MAIN_MODULE,
      modules: { [CELL_MAIN_MODULE]: CELL_ISOLATE_SOURCE, [CELL_SCRIPT_MODULE]: scriptEngineModule(script) },
      globalOutbound: null,
    }));
    const channel = new StepChannel();
    this.channels.set(cell, channel);
    const contextText = encodeJson(cellContextJson(context, cellStepsMax(PROGRAM.codes[context.code])));
    worker.getEntrypoint().run(contextText, (text) => channel.push(text)).catch((error) => {
      channel.deliver({ text: encodeJson(engineStepJson({ kind: "threw", error: cellErrorText(error) })), resolve() {} });
    });
    return channel.next();
  }

  resume(cell, input) {
    const channel = this.channels.get(cell);
    assert(channel !== undefined, "a cell resumes after it started");
    channel.answer(input);
    return channel.next();
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
  program: PROGRAM.name,
  target: "cloudflare",
  runtime,
  statements: { schema: JOURNAL_SCHEMA, insert: JOURNAL_INSERT, load: JOURNAL_LOAD },
  ranHeader: RAN_HEADER,
  turnPath: TURN_PATH,
  eventPath: EVENT_PATH,
  resumePath: RESUME_PATH,
  sidePath: SIDE_PATH,
};

export { HarnessObject, harness };
export default worker;
