import { mkdirSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Failure, Platform, UNCAUGHT_HEADER, assert, sleep, startHost } from "./platform.mjs";
import { statusOf } from "./verify.mjs";

const HTTP_OK = 200;
const OUTBOUND_KINDS = ["tool", "dispatch", "model"];
const RUN_POINTS = ["before_run", "after_run"];
const STATUS_KEY = '{"status":';
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

function reproducible(seed) {
  const events = seed.events;
  for (let index = 0; index < events.length; index++) {
    const event = events[index];
    if (event.event !== "hook" || event.answer !== "crash") continue;
    if (RUN_POINTS.includes(event.point) && OUTBOUND_KINDS.includes(event.kind)) continue;
    const flushed = events[index - 1];
    if (event.point === "after_flush" && flushed?.event === "flush" && !flushed.bytes.includes(STATUS_KEY)) continue;
    return { ok: false, point: `${event.point}${event.kind ? ` ${event.kind}` : ""}` };
  }
  return { ok: true };
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
    this.stats = { runs: 0, crashes: 0, alarmRuns: 0, requests: 0, comparisons: 0 };
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
      if (event?.event === "hook" && event.answer === "ok") {
        this.index += 1;
        continue;
      }
      return;
    }
  }

  committed() {
    return this.platform.rows().map((row) => `${row.record}\n`).join("");
  }

  same(what, expected, actual) {
    this.stats.comparisons += 1;
    if (expected !== actual) throw new Failure("backend_equal", short(`${what}: the interpreter has ${expected}, workerd has ${actual}`));
  }

  sameJson(what, expected, actual) {
    this.same(what, this.rt.encodeJson(expected), this.rt.encodeJson(actual));
  }

  async handle(path, body) {
    try {
      return await this.answer(path, body === "" ? null : this.rt.decodeJson(body));
    } catch (error) {
      this.failure ??= error instanceof Failure ? error : new Failure("panic", error?.stack ?? String(error));
      await this.platform.kill();
      return null;
    }
  }

  crashAt() {
    const event = this.peek();
    if (event?.event !== "hook" || event.answer !== "crash") return false;
    this.index += 1;
    this.stats.crashes += 1;
    return true;
  }

  async answer(path, request) {
    this.stats.requests += 1;
    this.quiet();
    if (path === "/now" || path === "/random") return String(this.take(path.slice(1), path).answer);
    if (this.crashAt()) {
      await this.platform.kill();
      return null;
    }
    const kind = path === "/model" ? "model" : "tool";
    const event = this.take(kind, short(this.rt.encodeJson(request)));
    this.compare(kind, event, request);
    this.same("committed rows at the outbound request", JSON.stringify(short(this.expected)), JSON.stringify(short(this.committed())));
    const answer = kind === "model" ? event.response : event.output;
    if (this.crashAt()) {
      await this.platform.kill();
      return null;
    }
    return this.rt.encodeJson(answer);
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
}

function hexText(hex) {
  return Buffer.from(hex, "hex").toString("utf8");
}

async function alarmRun(world, turn) {
  const started = Date.now();
  for (;;) {
    if (world.failure !== null || !world.platform.alive()) return null;
    const status = statusOf(world.rt, world.platform.rows(), turn);
    if (status !== null && !world.platform.alarm()) return { ran: "finished", finish: { turn, outcome: status.outcome, result: status.result } };
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
    const run = world.take("run", "open the journal");
    world.opened = world.opened.slice(0, Number(run.keep)) + hexText(run.tail);
    world.expected = world.opened;
    if (!world.platform.alive()) await world.platform.start("none", true);
    world.same("journal at open", JSON.stringify(short(world.opened)), JSON.stringify(short(world.committed())));
    world.stats.runs += 1;
    const byAlarm = !first && world.platform.alarm();
    if (byAlarm) world.stats.alarmRuns += 1;
    const driven = byAlarm ? await alarmRun(world, turn) : await postRun(world, body, turn);
    if (world.failure !== null) throw world.failure;
    if (driven === null) {
      world.same("run exit", "crash", world.take("exit", "crash").result);
      continue;
    }
    world.quiet();
    world.same("run exit", world.take("exit", driven.ran).result, driven.ran);
    acknowledged(world, driven.finish);
    world.same("committed rows after the run", JSON.stringify(short(world.expected)), JSON.stringify(short(world.committed())));
    return;
  }
}

async function traceSeed(rt, options, seed) {
  const dir = join(options.dir, `seed-${seed.seed}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  const world = new TraceWorld(rt, seed);
  const host = await startHost((path, body) => world.handle(path, body));
  world.platform = new Platform(dir, options.module, host.port);
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

export { parseTrace, reproducible, traceSeed };
