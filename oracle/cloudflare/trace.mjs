import { mkdirSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Failure, Platform, UNCAUGHT_HEADER, assert, sleep, startHost } from "./platform.mjs";
import { statusOf } from "./verify.mjs";

const HTTP_OK = 200;
const CLOCK_EVENTS = ["now", "random"];
const ALARM_POLL_MS = 25;
const ALARM_WAIT_MS_MAX = 20000;
const DETAIL_CHARACTERS_MAX = 300;

function short(text) {
  return text.length > DETAIL_CHARACTERS_MAX ? `${text.slice(0, DETAIL_CHARACTERS_MAX)}...` : text;
}

function parseTrace(rt, path) {
  const seeds = [];
  for (const line of readFileSync(path, "utf8").split("\n")) {
    if (line === "") continue;
    const event = rt.decodeJson(line);
    if (event.event === "seed") seeds.push({ seed: Number(event.seed), profile: event.profile, events: [], acks: [] });
    const current = seeds[seeds.length - 1];
    if (event.event === "ack") current.acks.push(event);
    else current.events.push(event);
  }
  return seeds;
}

class TraceWorld {
  constructor(rt, seed) {
    this.rt = rt;
    this.seed = seed;
    this.events = seed.events;
    this.index = 0;
    this.opened = "";
    this.expected = "";
    this.platform = null;
    this.failure = null;
    this.ackIndex = 0;
    this.lastRun = 0;
    this.stats = { runs: 0, crashes: 0, alarmRuns: 0, requests: 0, comparisons: 0, starts: 0 };
  }

  peek() {
    return this.events[this.index];
  }

  take(kind, detail) {
    const event = this.events[this.index];
    if (event === undefined || event.event !== kind) {
      const found = event === undefined ? "nothing more" : this.rt.encodeJson(event);
      throw new Failure("backend_equal", short(`event ${this.index}: workerd made a ${kind} step (${detail}) where the interpreter made ${found}`));
    }
    this.index += 1;
    return event;
  }

  quiet() {
    for (;;) {
      const event = this.peek();
      if (event?.event === "flush") {
        assert(event.answer === "ok", "cloudflare flushes always succeed in the trace");
        this.expected += event.bytes;
        this.index += 1;
        continue;
      }
      if ((event?.event === "hook" && event.answer === "ok") || CLOCK_EVENTS.includes(event?.event)) {
        this.index += 1;
        continue;
      }
      return;
    }
  }

  plan() {
    let hooks = 0;
    const clock = [];
    for (let index = this.index; index < this.events.length; index++) {
      const event = this.events[index];
      if (CLOCK_EVENTS.includes(event.event)) clock.push(Object.assign(Object.create(null), { kind: event.event, value: event.answer }));
      if (event.event !== "hook") continue;
      hooks += 1;
      if (event.answer === "crash") return { crashHook: hooks, clock: this.rt.encodeJson(clock) };
    }
    return { crashHook: null, clock: this.rt.encodeJson(clock) };
  }

  async start() {
    await this.platform.start("none", this.plan());
    this.stats.starts += 1;
  }

  committed() {
    return this.platform.rows().map((row) => `${row.record}\n`).join("");
  }

  same(what, expected, actual) {
    this.stats.comparisons += 1;
    if (expected !== actual) throw new Failure("backend_equal", short(`event ${this.index}: ${what}: the interpreter has ${expected}, workerd has ${actual}`));
  }

  sameRows(what, expected, actual) {
    this.stats.comparisons += 1;
    if (expected === actual) return;
    const left = expected.split("\n");
    const right = actual.split("\n");
    let row = 0;
    while (left[row] === right[row]) row += 1;
    const shown = (line) => (line === undefined ? "no row" : JSON.stringify(short(line)));
    throw new Failure("backend_equal", `event ${this.index} (run at event ${this.lastRun}): ${what} differ at row ${row + 1} of ${left.length - 1} and ${right.length - 1}: the interpreter has ${shown(left[row])}, workerd has ${shown(right[row])}`);
  }

  sameJson(what, expected, actual) {
    this.same(what, this.rt.encodeJson(expected), this.rt.encodeJson(actual));
  }

  async handle(path, body) {
    try {
      return await this.answer(path, this.rt.decodeJson(body));
    } catch (error) {
      this.failure ??= error instanceof Failure ? error : new Failure("panic", error?.stack ?? String(error));
      await this.platform.kill();
      return null;
    }
  }

  async answer(path, request) {
    this.stats.requests += 1;
    this.quiet();
    const kind = path === "/model" ? "model" : "tool";
    const event = this.take(kind, short(this.rt.encodeJson(request)));
    this.compare(kind, event, request);
    this.sameRows("committed rows at the outbound request", this.expected, this.committed());
    return this.rt.encodeJson(kind === "model" ? event.response : event.output);
  }

  compare(kind, event, request) {
    this.sameJson(`${kind} turn`, event.turn, request.turn);
    this.sameJson(`${kind} seq`, event.seq, request.seq);
    if (kind === "model") {
      this.sameJson("model calls_max", event.calls_max, request.calls_max);
      this.sameJson("model context crc", event.context_crc, BigInt(this.rt.crc32(this.rt.encodeJson(request.context))));
      return;
    }
    this.sameJson("tool name", event.tool, request.tool);
    this.sameJson("tool class", event.effect, request.effect);
    this.sameJson("tool input", event.input, request.input);
  }

  crashed() {
    if (this.platform.crashed === null) throw new Failure("liveness", "workerd stopped without reaching the trace's crash point");
    this.quiet();
    const event = this.take("hook", "the crash point");
    this.same("crash answer", "crash", event.answer);
    const point = this.rt.decodeJson(this.platform.crashed);
    this.sameJson("crash point", [event.point, event.kind ?? null, event.effect ?? null], [point.point, point.kind ?? null, point.effect ?? null]);
    this.stats.crashes += 1;
  }
}

function hexText(hex) {
  return Buffer.from(hex, "hex").toString("utf8");
}

async function alarmRun(world, turn) {
  const started = Date.now();
  for (;;) {
    if (world.failure !== null || !world.platform.alive()) return null;
    const line = world.platform.alarms.shift();
    const result = line === undefined ? null : world.rt.decodeJson(line);
    if (result !== null && result.ran !== "idle") {
      if (result.finish === null) throw new Failure("liveness", `turn ${turn}: the alarm found ${result.ran}`);
      return { ran: result.ran, finish: { turn: Number(result.finish.turn), outcome: result.finish.outcome, result: result.finish.result } };
    }
    if (Date.now() - started > ALARM_WAIT_MS_MAX) throw new Failure("liveness", `turn ${turn}: the alarm did not finish it within ALARM_WAIT_MS_MAX (${ALARM_WAIT_MS_MAX} ms)`);
    await sleep(ALARM_POLL_MS);
  }
}

async function postRun(world, body, turn) {
  let response;
  try {
    response = await world.platform.turn(body);
  } catch (error) {
    if (!world.platform.alive()) return null;
    throw error;
  }
  if (!world.platform.alive()) return null;
  if (response.status !== HTTP_OK) {
    const uncaught = response.headers[UNCAUGHT_HEADER] === "1" ? " (uncaught in the object)" : "";
    throw new Failure("liveness", `turn ${turn}: the object answered ${response.status}${uncaught}: ${response.body}`);
  }
  const finish = world.rt.decodeJson(response.body);
  return { ran: response.headers["hl-ran"], finish: { turn: Number(finish.turn), outcome: finish.outcome, result: finish.result } };
}

function acknowledged(world, finish) {
  const expected = world.seed.acks[world.ackIndex];
  if (expected === undefined) throw new Failure("finish_once", `turn ${finish.turn} was acknowledged again`);
  world.ackIndex += 1;
  world.sameJson("acknowledgement", { outcome: expected.outcome, result: expected.result, turn: expected.turn }, { outcome: finish.outcome, result: finish.result, turn: BigInt(finish.turn) });
  const status = statusOf(world.rt, world.platform.rows(), finish.turn);
  if (status === null) throw new Failure("finish_once", `turn ${finish.turn} was acknowledged before its status row was committed`);
}

async function traceTurn(world, turnEvent) {
  const turn = Number(turnEvent.turn);
  const body = world.rt.encodeJson(Object.assign(Object.create(null), { arguments: turnEvent.arguments, handler: turnEvent.handler, turn: turnEvent.turn }));
  for (let first = true; ; first = false) {
    world.lastRun = world.index;
    const run = world.take("run", "open the journal");
    world.opened = world.opened.slice(0, Number(run.keep)) + hexText(run.tail);
    world.expected = world.opened;
    world.sameRows("journal at open", world.opened, world.committed());
    const byAlarm = !first && world.platform.alarm();
    if (!world.platform.alive()) await world.start();
    world.stats.runs += 1;
    if (byAlarm) world.stats.alarmRuns += 1;
    const driven = byAlarm ? await alarmRun(world, turn) : await postRun(world, body, turn);
    if (world.failure !== null) throw world.failure;
    if (driven === null) {
      world.crashed();
      world.same("run exit", "crash", world.take("exit", "crash").result);
      continue;
    }
    world.quiet();
    world.same("run exit", world.take("exit", driven.ran).result, driven.ran);
    acknowledged(world, driven.finish);
    world.sameRows("committed rows after the run", world.expected, world.committed());
    return;
  }
}

async function traceSeed(rt, options, seed) {
  const dir = join(options.dir, `seed-${seed.seed}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  const world = new TraceWorld(rt, seed);
  const host = await startHost((path, body) => world.handle(path, body));
  world.platform = new Platform(dir, options.module, host.port, options.wasm);
  try {
    world.take("seed", "start of seed");
    while (world.peek()?.event === "turn") await traceTurn(world, world.take("turn", "next turn"));
    const end = world.take("end", "end of seed");
    const medium = world.committed();
    world.sameJson("journal bytes", end.journal_bytes, BigInt(Buffer.byteLength(medium)));
    world.sameJson("journal crc", end.journal_crc, BigInt(rt.crc32(medium)));
    world.sameJson("acknowledgements", end.acks, BigInt(world.ackIndex));
    if (end.result !== "ok") throw new Failure("backend_equal", `the interpreter failed this seed: ${end.result}`);
    return { seed: seed.seed, profile: seed.profile, rows: world.platform.rows().length, ...world.stats };
  } finally {
    await world.platform.kill();
    host.server.close();
  }
}

export { parseTrace, traceSeed };
