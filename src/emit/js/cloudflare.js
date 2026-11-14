const CLOUDFLARE_TARGET = { recovery: "at_load", gateBeforeOutbound: true };
const CLOUDFLARE_EPOCH = 0;
const JOURNAL_SCHEMA = "CREATE TABLE IF NOT EXISTS hl_journal (position INTEGER PRIMARY KEY, record TEXT NOT NULL)";
const JOURNAL_INSERT = "INSERT INTO hl_journal (position, record) VALUES (?, ?)";
const JOURNAL_LOAD = "SELECT position, record FROM hl_journal ORDER BY position";
const ALARM_RESUME_DELAY_MS = 1000;
const HARNESS_BINDING = "HARNESS";
const TURN_PATH = "/turn";
const SESSION_TURN_PATH = /^\/sessions\/([^/]+)\/turn$/;
const RAN_HEADER = "hl-ran";
const HTTP_BAD_REQUEST = 400;
const HTTP_NOT_FOUND = 404;
const HTTP_INTERNAL_ERROR = 500;
const NO_SIMULATION = { mutation: "none", hook() {}, gate() {} };

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

  async tool(request) {
    const output = await this.object.host.tool(request);
    for (const record of this.deferred.splice(0)) this.object.insert(record);
    return output;
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
}

class HarnessObject {
  constructor(ctx, env) {
    this.storage = ctx.storage;
    this.simulation = env.HL_SIMULATION ?? NO_SIMULATION;
    this.mutation = this.simulation.mutation;
    this.host = env.HL_HOST ?? defaultHost({
      fetch: (url, init) => fetch(url, init),
      toolsUrl: env.HL_TOOLS_URL,
      modelUrl: env.HL_MODEL_URL,
      now: () => Date.now(),
      random: platformRandom,
    });
    this.storage.sql.exec(JOURNAL_SCHEMA);
    this.position = 0;
    this.armed = false;
    this.running = false;
    this.storageWrites = [];
  }

  load() {
    const rows = this.storage.sql.exec(JOURNAL_LOAD).toArray();
    const records = rows.map((row, index) => {
      if (Number(row.position) !== index + 1) throw new Corrupt(`journal row ${index + 1} has position ${row.position}`);
      return decodeOrCorrupt(() => decodeLine(row.record, false));
    });
    this.position = rows.length;
    return records;
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
  }

  async drive(request) {
    assert(!this.running, "one turn runs at a time in an object");
    this.running = true;
    try {
      const records = this.load();
      const world = new CloudflareWorld(this);
      const ran = await runTurn({ target: CLOUDFLARE_TARGET, mutation: this.mutation }, world, CLOUDFLARE_EPOCH, records, request);
      if (ran.ran === "already_finished") await world.acknowledge(ran.finish);
      await this.settle();
      assert(world.finish !== null, "a driven turn is acknowledged");
      return { ran: ran.ran, finish: world.finish };
    } finally {
      this.running = false;
    }
  }

  async fetch(request) {
    const url = new URL(request.url);
    if (request.method !== "POST" || url.pathname !== TURN_PATH) return new Response("not found", { status: HTTP_NOT_FOUND });
    let turn;
    try {
      turn = turnRequestFromJson(decodeJson(await request.text()));
    } catch (error) {
      if (error instanceof DecodeError) return new Response(error.message, { status: HTTP_BAD_REQUEST });
      throw error;
    }
    try {
      const driven = await this.drive(turn);
      return new Response(encodeJson(finishJson(driven.finish)), { headers: { "content-type": "application/json", [RAN_HEADER]: driven.ran } });
    } catch (error) {
      if (error instanceof Corrupt) return new Response(error.message, { status: HTTP_INTERNAL_ERROR });
      throw error;
    }
  }

  async alarm() {
    if (this.running) {
      await this.storage.setAlarm(Date.now() + ALARM_RESUME_DELAY_MS);
      return { ran: "busy" };
    }
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

const worker = {
  async fetch(request, env) {
    const url = new URL(request.url);
    const match = SESSION_TURN_PATH.exec(url.pathname);
    if (request.method !== "POST" || match === null) return new Response("not found", { status: HTTP_NOT_FOUND });
    const namespace = env[HARNESS_BINDING];
    const stub = namespace.get(namespace.idFromName(decodeURIComponent(match[1])));
    return stub.fetch(new URL(TURN_PATH, url.origin), { method: "POST", body: await request.text(), headers: { "content-type": "application/json" } });
  },
};

const harness = {
  program: PROGRAM.name,
  target: "cloudflare",
  runtime,
  statements: { schema: JOURNAL_SCHEMA, insert: JOURNAL_INSERT, load: JOURNAL_LOAD },
  ranHeader: RAN_HEADER,
  turnPath: TURN_PATH,
};

export { HarnessObject, harness };
export default worker;
