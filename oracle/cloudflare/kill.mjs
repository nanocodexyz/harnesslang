import { join } from "node:path";
import { mkdirSync, rmSync } from "node:fs";
import { Failure, Platform, UNCAUGHT_HEADER, assert, sleep, startHost } from "./platform.mjs";
import { callKey, statusOf, verifyJournal } from "./verify.mjs";

const SPLITMIX_GAMMA = 0x9e3779b97f4a7c15n;
const SPLITMIX_MIX_FIRST = 0xbf58476d1ce4e5b9n;
const SPLITMIX_MIX_SECOND = 0x94d049bb133111ebn;
const U64_MASK = (1n << 64n) - 1n;
const PPM = 1_000_000;
const ANSWER_PPM = 350_000;
const UNDECLARED_TOOL_PPM = 250_000;
const TOOL_FAILURE_PPM = 150_000;
const UNDECLARED_TOOLS = ["web_fetch", "spawn_agent"];
const HTTP_OK = 200;
const ALARM_POLL_MS = 25;
const ALARM_WAIT_MS_MAX = 20000;
const ATTEMPTS_PER_TURN_MAX = 400;
const INJECTED = "hl-drill injected";

const FAULTS = {
  killBeforeRunPpm: 60_000,
  killAfterRunPpm: 60_000,
  killTimerPpm: 300_000,
  storageFaultPpm: 250_000,
  alarmResumePpm: 500_000,
  duplicatePpm: 200_000,
  insertsFaultRange: 12,
  alarmsFaultRange: 2,
};

class Rng {
  constructor(seed) {
    this.state = BigInt(seed) & U64_MASK;
  }

  draw() {
    this.state = (this.state + SPLITMIX_GAMMA) & U64_MASK;
    let mixed = this.state;
    mixed = ((mixed ^ (mixed >> 30n)) * SPLITMIX_MIX_FIRST) & U64_MASK;
    mixed = ((mixed ^ (mixed >> 27n)) * SPLITMIX_MIX_SECOND) & U64_MASK;
    return mixed ^ (mixed >> 31n);
  }

  below(bound) {
    assert(bound > 0, "a draw is below a positive bound");
    return Number((this.draw() * BigInt(bound)) >> 64n);
  }

  chance(ppm) {
    return this.below(PPM) < ppm;
  }
}

class KillWorld {
  constructor(rt, options, seed) {
    this.rt = rt;
    this.options = options;
    this.rng = new Rng(seed);
    this.platform = null;
    this.ledger = new Map();
    this.failure = null;
    this.run = 0;
    this.stats = { killsBeforeRun: 0, killsAfterRun: 0, killsTimer: 0, storageFaults: 0, injectedErrors: 0, alarmResumes: 0, duplicates: 0, gateChecks: 0, attempts: 0 };
  }

  fail(failure) {
    this.failure ??= failure;
    return this.platform.kill().then(() => null);
  }

  gate(request) {
    const id = callKey(Number(request.turn), Number(request.seq));
    const rows = this.platform.rows();
    const committed = rows.some((row) => {
      const record = this.rt.decodeLine(row.record, false);
      return record.kind === "call" && callKey(record.turn, record.seq) === id && record.tool === request.tool && record.effect === request.effect;
    });
    this.stats.gateChecks += 1;
    if (!committed) return new Failure("external_once", `external call ${id} (${request.tool}) reached the tool host before its call row was committed, so a reset object runs it again`);
    if (!this.platform.alarm()) return new Failure("liveness", `external call ${id} reached the tool host with no alarm committed, so a reset object never resumes`);
    return null;
  }

  async handle(path, body) {
    try {
      return await this.answer(path, this.rt.decodeJson(body));
    } catch (error) {
      return this.fail(error instanceof Failure ? error : new Failure("panic", error?.stack ?? String(error)));
    }
  }

  async answer(path, request) {
    if (path === "/tool" && request.effect === "external") {
      const failure = this.gate(request);
      if (failure !== null) return this.fail(failure);
    }
    if (this.rng.chance(FAULTS.killBeforeRunPpm)) {
      this.stats.killsBeforeRun += 1;
      await this.platform.kill();
      return null;
    }
    const answer = path === "/tool" ? this.tool(request) : this.model(request);
    if (this.rng.chance(FAULTS.killAfterRunPpm)) {
      this.stats.killsAfterRun += 1;
      await this.platform.kill();
      return null;
    }
    return this.rt.encodeJson(answer);
  }

  tool(request) {
    const id = callKey(Number(request.turn), Number(request.seq));
    const dispatches = this.ledger.get(id) ?? [];
    dispatches.push({ tool: request.tool, effect: request.effect, run: this.run });
    this.ledger.set(id, dispatches);
    if (request.effect === "external" && dispatches.filter((dispatch) => dispatch.effect === "external").length > 1) {
      throw new Failure("external_once", `external call ${id} (${request.tool}) was dispatched twice`);
    }
    if (this.rng.chance(TOOL_FAILURE_PPM)) return { failed: `${request.tool} failed at ${id}` };
    if (request.returns === "u64") return { ok: this.rng.draw() >> 32n };
    if (request.returns === "bool") return { ok: (this.rng.draw() & 1n) === 1n };
    return { ok: `${request.tool}-${id}` };
  }

  model(request) {
    if (this.rng.chance(ANSWER_PPM)) return { answer: `done-${request.turn}-${request.seq}` };
    const declared = this.options.tools;
    const count = 1 + this.rng.below(Number(request.calls_max));
    const calls = [];
    for (let index = 0; index < count; index++) {
      const undeclared = declared.length === 0 || this.rng.chance(UNDECLARED_TOOL_PPM);
      const tool = undeclared ? UNDECLARED_TOOLS[this.rng.below(UNDECLARED_TOOLS.length)] : declared[this.rng.below(declared.length)];
      calls.push({ id: `c${request.turn}-${request.seq}-${index}`, input: `input-${index}`, tool });
    }
    return { calls };
  }

  fault() {
    if (!this.rng.chance(FAULTS.storageFaultPpm)) return "none";
    this.stats.storageFaults += 1;
    if (this.rng.chance(PPM / 2)) return `alarm ${1 + this.rng.below(FAULTS.alarmsFaultRange)}`;
    return `insert ${1 + this.rng.below(FAULTS.insertsFaultRange)}`;
  }
}

function turnBody(rt, options, seed, turn) {
  const args = Object.assign(Object.create(null), { [options.arg]: `drill-${seed}-${turn}` });
  return rt.encodeJson(Object.assign(Object.create(null), { arguments: args, handler: options.handler, turn: BigInt(turn) }));
}

async function waitAlarm(world, turn) {
  const started = Date.now();
  for (;;) {
    if (world.failure !== null || !world.platform.alive()) return false;
    if (statusOf(world.rt, world.platform.rows(), turn) !== null) return true;
    if (Date.now() - started > ALARM_WAIT_MS_MAX) {
      throw new Failure("liveness", `turn ${turn}: the committed alarm did not finish it within ALARM_WAIT_MS_MAX (${ALARM_WAIT_MS_MAX} ms)`);
    }
    await sleep(ALARM_POLL_MS);
  }
}

function settleResponse(world, turn, settled, acks) {
  if (settled.status === "rejected") return false;
  const response = settled.value;
  if (response.status !== HTTP_OK) {
    if (response.body.includes(INJECTED)) {
      world.stats.injectedErrors += 1;
      return false;
    }
    const uncaught = response.headers[UNCAUGHT_HEADER] === "1" ? " (uncaught in the object)" : "";
    throw new Failure("liveness", `turn ${turn}: the object answered ${response.status}${uncaught}: ${response.body}`);
  }
  const finish = world.rt.decodeJson(response.body);
  const ack = { turn: Number(finish.turn), outcome: finish.outcome, result: finish.result };
  const status = statusOf(world.rt, world.platform.rows(), turn);
  if (status === null || status.outcome !== ack.outcome || world.rt.encodeJson(status.result) !== world.rt.encodeJson(ack.result)) {
    throw new Failure("finish_once", `turn ${turn} was acknowledged before its status row was committed`);
  }
  acks.push(ack);
  return true;
}

async function attempt(world, body, turn, acks, killed) {
  const platform = world.platform;
  if (!platform.alive()) await platform.start(world.fault(), false);
  world.run += 1;
  world.stats.attempts += 1;
  if (killed && platform.alarm() && world.rng.chance(FAULTS.alarmResumePpm)) {
    world.stats.alarmResumes += 1;
    if (!(await waitAlarm(world, turn))) return false;
  }
  const kills = platform.kills;
  let timer = null;
  if (world.rng.chance(FAULTS.killTimerPpm)) {
    const delay = world.rng.below(world.options.delayMsMax + 1);
    timer = setTimeout(() => {
      if (platform.kills === kills && platform.alive()) {
        world.stats.killsTimer += 1;
        platform.kill();
      }
    }, delay);
  }
  const requests = [platform.turn(body)];
  if (world.rng.chance(FAULTS.duplicatePpm)) {
    world.stats.duplicates += 1;
    const delay = world.rng.below(world.options.delayMsMax + 1);
    requests.push(sleep(delay).then(() => (platform.alive() ? platform.turn(body) : Promise.reject(new Error("killed")))));
  }
  const settled = await Promise.allSettled(requests);
  clearTimeout(timer);
  if (world.failure !== null) throw world.failure;
  const finished = settled.map((each) => settleResponse(world, turn, each, acks));
  return finished.some((each) => each);
}

async function killSession(rt, options, index) {
  const seed = options.seed + index;
  const dir = join(options.dir, `session-${index}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  const world = new KillWorld(rt, options, seed);
  const host = await startHost((path, body) => world.handle(path, body));
  world.platform = new Platform(dir, options.module, host.port);
  const acks = [];
  try {
    for (let turn = 1; turn <= options.turns; turn++) {
      const body = turnBody(rt, options, seed, turn);
      let done = false;
      let killed = false;
      for (let tries = 0; !done; tries++) {
        if (tries >= ATTEMPTS_PER_TURN_MAX) throw new Failure("liveness", `turn ${turn} did not finish in ATTEMPTS_PER_TURN_MAX (${ATTEMPTS_PER_TURN_MAX}) attempts`);
        const kills = world.platform.kills;
        done = await attempt(world, body, turn, acks, killed);
        killed = world.platform.kills > kills;
      }
    }
    if (!world.platform.alive()) await world.platform.start("none", false);
    const verified = verifyJournal(rt, world.platform.rows(), world.ledger, acks, options.turns);
    return { seed, ...world.stats, kills: world.platform.kills, starts: world.platform.starts, acks: acks.length, ...verified };
  } finally {
    await world.platform.kill();
    host.server.close();
  }
}

export { killSession };
