import { createContext, Script } from "node:vm";
const VERCEL_TARGET = { ...TARGET_MODEL, inbound: "after_flush" };
const VERCEL_EPOCH = 0;
const STEPS_PER_EFFECT_MAX = 3;
const STEPS_PER_SUSPENSION_MAX = 2;
const STEPS_PER_TURN_FIXED = 2;

assert(VERCEL_TARGET.recovery === "in_step", "the Vercel adapter recovers external effects in their step");
assert(!VERCEL_TARGET.gateBeforeOutbound, "a Vercel step has no output gate");

class StepWait {
  constructor(wait) {
    this.wait = wait;
  }
}

function ranJson(turn, ran) {
  const outputs = ran.outputs.map(([position, output]) => [BigInt(position), ran.model ? output : outputJson(output)]);
  return object([
    ["events", BigInt(ran.events)],
    ["model", ran.model],
    ["outputs", outputs],
    ["position", BigInt(ran.position)],
    ["preempted", ran.preempted],
    ["seq", BigInt(ran.seq)],
    ["started", ran.started],
    ["turn", BigInt(turn)],
  ]);
}

function ranFrom(json) {
  if (!isObject(json)) throw new DecodeError("a ran effect is an object");
  const fields = new Fields("ran", json);
  fields.exact(["events", "model", "outputs", "position", "preempted", "seq", "started", "turn"]);
  const model = fields.boolean("model");
  const outputs = fields.get("outputs");
  const preempted = fields.get("preempted");
  const started = fields.get("started");
  if (!Array.isArray(outputs) || !Array.isArray(preempted) || !Array.isArray(started)) throw new DecodeError("a ran effect lists its outputs, preemptions and started jobs");
  if (preempted.some((one) => typeof one !== "boolean")) throw new DecodeError("a ran effect's preemptions are bools");
  if (started.some((pair) => !Array.isArray(pair) || pair.length !== 2 || pair.some((text) => typeof text !== "string"))) throw new DecodeError("a ran effect's started jobs are key and tool pairs");
  return {
    turn: fields.number("turn"),
    seq: fields.number("seq"),
    position: fields.number("position"),
    events: fields.number("events"),
    model,
    preempted,
    started,
    outputs: outputs.map((pair) => {
      if (!Array.isArray(pair) || pair.length !== 2) throw new DecodeError("a ran effect's output is a position and its output");
      return [smallNumber(pair[0], "a ran effect's position"), model ? pair[1] : outputFrom(pair[1])];
    }),
  };
}

function waitJson(wait) {
  return object([["cell", BigInt(wait.cell)], ["code", BigInt(wait.code)], ["delay_ms", wait.delayMs], ["timer", BigInt(wait.timer)], ["until_ms", wait.untilMs]]);
}

function waitFrom(json) {
  if (!isObject(json)) throw new DecodeError("a wait is an object");
  const fields = new Fields("wait", json);
  fields.exact(["cell", "code", "delay_ms", "timer", "until_ms"]);
  const big = (key) => {
    const value = fields.get(key);
    if (typeof value !== "bigint") throw new DecodeError(`a wait's '${key}' is not a number`);
    return value;
  };
  return { cell: fields.number("cell"), code: fields.number("code"), timer: fields.number("timer"), delayMs: big("delay_ms"), untilMs: big("until_ms") };
}

class StepWorld {
  constructor(context, waited, resume, carried) {
    this.context = context;
    this.waitedTimers = waited;
    this.delivered = resume;
    this.resuming = null;
    this.carried = carried;
    this.deadlines = [];
  }

  async cellDeadline(turn, site, cell) {
    const key = `${turn}/${site}/${cell}`;
    const carried = this.carried.deadlines.find(([held]) => held === key);
    if (carried !== undefined) return BigInt(carried[1]);
    const clock = BigInt(await this.now());
    this.deadlines.push([key, clock.toString()]);
    return clock;
  }

  async ran(turn, seq) {
    const ran = this.carried.ran;
    return ran !== null && ran.turn === turn && ran.seq === seq ? ran : null;
  }

  async undispatched(turn, seq) {
    if (this.carried.turn !== turn || !this.carried.undispatched.includes(seq)) return false;
    const attempt = this.context.attemptQuiet;
    assert(Number.isSafeInteger(attempt) && attempt >= 1, "a step's attempt counts from 1");
    return attempt === 1;
  }

  recover(request) {
    return this.context.host.recover(request);
  }

  async jobs() {
    return this.carried.lost;
  }

  hold(turn, seq, untilMs) {
    return this.context.host.hold(turn, seq, untilMs);
  }

  clock() {
    return this.context.host.clock();
  }

  sleep(turn, seq, untilMs) {
    return this.context.host.sleep(turn, seq, untilMs);
  }

  async resume(suspension) {
    const resume = this.context.resume !== undefined ? await this.context.resume(suspension) : this.delivered;
    if (resume !== null) this.resuming = { suspension, resume };
    return resume;
  }

  async resumed(suspension, resume, matched) {
    assert(!matched, "a matched resume's flush ends the step before it is answered");
    if (this.context.resumed !== undefined) await this.context.resumed(suspension, resume, matched);
  }

  append() {}

  async hook(point) {
    await this.context.hook(point);
  }

  async beforeExternal() {}

  async flush(epoch, records, position) {
    assert(epoch === VERCEL_EPOCH, "a workflow run has one owner");
    throw new StepEnd(records, position);
  }

  async attempt(turn, seq, attempted) {
    const attempt = this.context.stepAttempt !== undefined ? this.context.stepAttempt(attempted) : this.context.attempt;
    assert(Number.isSafeInteger(attempt) && attempt >= 1, "a step's attempt counts from 1");
    return attempted && attempt === 1 ? attempt + 1 : attempt;
  }

  async nestedAttempt(turn, seq, nested) {
    const attempt = await this.context.nestedAttempt(turn, seq, nested);
    assert(Number.isSafeInteger(attempt) && attempt >= 1, "a step's attempt counts from 1");
    return attempt;
  }

  cellStart(cell, script, context) {
    return this.context.cells.start(cell, script, context);
  }

  cellResume(cell, input) {
    return this.context.cells.resume(cell, input);
  }

  async waited(wait) {
    const answer = this.waitedTimers.some((done) => done.cell === wait.cell && done.timer === wait.timer);
    if (this.context.observeWaited !== undefined) await this.context.observeWaited(wait, answer);
    return answer;
  }

  async wait(wait) {
    throw new StepWait(wait);
  }

  async observe(event) {
    await this.context.host.observe(event);
  }

  tool(request) {
    return this.context.host.tool(request);
  }

  listing(request) {
    return this.context.host.listing(request);
  }

  async tools(requests, limit) {
    const simulated = this.context.tools;
    if (simulated !== undefined) return simulated(this, requests, limit);
    return dispatchConcurrently(this, requests, limit);
  }

  model(request) {
    return this.context.host.model(request);
  }

  now() {
    return this.context.host.now();
  }

  random() {
    return this.context.host.random();
  }

  async acknowledge() {
    throw new Panic("a step ends at its flush, before the turn is acknowledged");
  }

  async inbox() {
    throw new Panic("a step takes no events: the workflow function records them between steps");
  }

  async answer() {
    throw new Panic("a step answers no events: the workflow function does, by an answer step");
  }

  async side(turn, seq) {
    if (this.context.side === undefined) return null;
    return this.context.side(turn, seq);
  }

  async sided(request, answer) {
    if (this.context.sided !== undefined) await this.context.sided(request, answer);
  }
}

class ProcessCells {
  constructor() {
    this.engines = new Map();
  }

  async start(cell, script, context) {
    let run;
    try {
      run = new Script(`(async function cell(${CELL_SCRIPT_PARAMETERS.join(", ")}) {\n${script}\n})`).runInContext(createContext(Object.create(null)));
    } catch (error) {
      return { kind: "threw", error: cellErrorText(error) };
    }
    const engine = new ScriptEngine(run, {
      limits: context.limits,
      replay: context.replay,
      platform: platformCellValues(),
      macrotask: () => new Promise((resolve) => setTimeout(resolve, 0)),
    });
    this.engines.set(cell, engine);
    return engine.start();
  }

  resume(cell, input) {
    const engine = this.engines.get(cell);
    assert(engine !== undefined, "a cell resumes after it started");
    return engine.resume(input);
  }
}

async function runStep(input, context) {
  if (input.kind === "acknowledge") {
    await context.acknowledge(finishFromJson(decodeJson(input.finish)));
    return { lines: [] };
  }
  if (input.kind === "answer") {
    await context.answer(input.turn, input.position, input.answers);
    return { lines: [] };
  }
  if (input.kind === "idle") {
    const records = input.lines.map((line) => decodeOrCorrupt(() => decodeLine(line, false)));
    const due = await idleDue({ target: VERCEL_TARGET, mutation: context.mutation ?? "none" }, records);
    return { lines: [], due: due === null ? null : due.toString() };
  }
  assert(input.kind === "run", "a step acknowledges, answers, reads the idle due or runs");
  const records = input.lines.map((line) => decodeOrCorrupt(() => decodeLine(line, false)));
  const request = decodeOrCorrupt(() => turnRequestFromJson(decodeJson(input.request)));
  const setup = { target: VERCEL_TARGET, mutation: context.mutation ?? "none" };
  const waited = (input.waited ?? []).map((text) => decodeOrCorrupt(() => waitFrom(decodeJson(text))));
  const delivered = input.resume === undefined || input.resume === null ? null : decodeOrCorrupt(() => resumeFrom(decodeJson(input.resume)));
  const carried = {
    turn: request.turn,
    ran: input.ran === undefined || input.ran === null ? null : decodeOrCorrupt(() => ranFrom(decodeJson(input.ran))),
    undispatched: input.undispatched ?? [],
    lost: input.lost ?? [],
    deadlines: input.deadlines ?? [],
  };
  const world = new StepWorld(context, waited, delivered, carried);
  try {
    const ran = await runTurn(setup, world, VERCEL_EPOCH, records, request);
    if (ran.ran === "suspended") return { lines: [], suspended: encodeJson(suspensionJson(ran.suspension)) };
    if (ran.ran === "not_due") return { lines: [], notDue: true };
    throw new Panic(`a step ends at a flush, found the turn ${ran.ran}`);
  } catch (error) {
    if (error instanceof StepEnd) {
      const output = { lines: error.records.map((record) => encodeLine(record, false)), position: error.position, deadlines: world.deadlines };
      const answered = world.resuming;
      if (answered !== null && error.records.some((record) => record.kind === "result" && record.turn === answered.suspension.turn && record.seq === answered.suspension.seq)) {
        output.resumed = encodeJson(object([["resume", resumeJson(answered.resume)], ["suspension", suspensionJson(answered.suspension)]]));
      }
      return output;
    }
    if (error instanceof StepWait) return { lines: [], wait: encodeJson(waitJson(error.wait)), deadlines: world.deadlines };
    if (error instanceof StepRan) return { lines: [], ran: encodeJson(ranJson(request.turn, error.ran)), deadlines: world.deadlines };
    throw error;
  }
}

async function side(lines, requestText, host) {
  const records = lines.map((line) => decodeOrCorrupt(() => decodeLine(line, false)));
  const request = decodeOrCorrupt(() => sideRequestFromJson(decodeJson(requestText)));
  const world = { tool: (call) => host.tool(call), model: (call) => host.model(call) };
  const answer = await answerSide({ target: VERCEL_TARGET, mutation: "none" }, world, records, request);
  return encodeJson(sideAnswerJson(answer));
}

async function fork(lines, at, origin) {
  const records = lines.map((line) => decodeOrCorrupt(() => decodeLine(line, false)));
  const forked = await forkEntry({ target: VERCEL_TARGET, mutation: "none" }, records, at, origin);
  if (forked.refused !== undefined) return { refused: forked.refused };
  return { lines: forked.records.map((record) => encodeLine(record, false)) };
}

async function session(platform) {
  const lines = (platform.entry ?? []).slice();
  const folded = fold(lines.map((line) => decodeOrCorrupt(() => decodeLine(line, false))));
  for (let index = 0; index < platform.turnsMax; index++) {
    const requestText = PROGRAM.idle === null ? await platform.request(index) : await idleOrRequest(platform, lines, folded, index);
    await driveTurn(platform, lines, folded, requestText);
  }
}

async function idleOrRequest(platform, lines, folded, index) {
  const done = await platform.step(`idle ${index}`, { kind: "idle", lines: lines.slice() });
  if (done.output.due === null) return platform.request(index);
  const idle = encodeJson(object([["arguments", object([])], ["handler", PROGRAM.idle.handler], ["turn", BigInt(folded.lastTurn + 1)]]));
  return platform.requestOrIdle(index, BigInt(done.output.due), idle);
}

async function driveTurn(platform, lines, folded, requestText) {
  const request = decodeOrCorrupt(() => turnRequestFromJson(decodeJson(requestText)));
  const handler = handlerNamed(request.handler);
  const stepsMax = (STEPS_PER_EFFECT_MAX + STEPS_PER_SUSPENSION_MAX) * handler.effectsMax + STEPS_PER_TURN_FIXED;
  let live = false;
  const waited = [];
  const deadlines = [];
  let resume = null;
  let ran = null;
  let undispatched = [];
  let asked = PROGRAM.events.jobs === null;
  const lost = [];
  for (let step = 0; step < stepsMax; step++) {
    const log = folded.turns.get(request.turn);
    if (log !== undefined && log.finished !== null) {
      if (live) await platform.hook(POINT_BEFORE_ACKNOWLEDGE);
      const finish = { turn: request.turn, outcome: log.finished.outcome, result: log.finished.result };
      const acknowledged = await platform.step(`acknowledge ${request.turn}`, { kind: "acknowledge", finish: encodeJson(finishJson(finish)) });
      if (eventsTaken()) await closeTurnInbox(platform, request.turn, log, acknowledged.live);
      return;
    }
    if (!asked) {
      const answer = await platform.jobs(request.turn, step, [...folded.jobs.keys()].sort());
      if (answer !== null) {
        asked = answer.live;
        lost.splice(0, lost.length, ...answer.lost.filter((key) => folded.jobs.has(key)).map((key) => lostDelivery(key)));
      }
    }
    const done = await platform.step(`turn ${request.turn} step ${step}`, { kind: "run", lines: lines.slice(), request: requestText, waited: waited.slice(), deadlines: deadlines.slice(), resume, ran, undispatched, lost: lost.map((delivery) => delivery.key) });
    deadlines.push(...(done.output.deadlines ?? []));
    resume = null;
    ran = null;
    undispatched = [];
    if (done.output.ran !== undefined) {
      assert(done.output.lines.length === 0, "a step that ran an effect to drain events after it flushed before it ran");
      const carried = decodeOrCorrupt(() => ranFrom(decodeJson(done.output.ran)));
      if (eventsTaken()) await recordEvents(platform, lines, folded, request.turn, done.live, { position: carried.position, lost, started: carried.started });
      if (done.live) ran = done.output.ran;
      continue;
    }
    if (done.output.notDue === true) return;
    if (done.output.suspended !== undefined) {
      const suspension = decodeOrCorrupt(() => suspensionFrom(decodeJson(done.output.suspended)));
      resume = await platform.resume(`turn ${request.turn} resume ${suspension.seq} at step ${step}`, suspension);
      continue;
    }
    if (done.output.wait !== undefined) {
      assert(done.output.lines.length === 0, "a step that waits flushed before it waited");
      await platform.sleep(`turn ${request.turn} sleep ${step}`, decodeOrCorrupt(() => waitFrom(decodeJson(done.output.wait))));
      waited.push(done.output.wait);
      continue;
    }
    assert(done.output.lines.length > 0, "a run step ends at a flush of at least one record");
    for (const line of done.output.lines) {
      lines.push(line);
      applyRecord(folded, decodeOrCorrupt(() => decodeLine(line, false)));
    }
    live = done.live;
    if (live) await platform.hook(POINT_AFTER_FLUSH);
    if (live) undispatched = undispatchedIn(done.output.lines, request.turn);
    const running = folded.turns.get(request.turn);
    if (eventsTaken() && running.finished === null) await recordEvents(platform, lines, folded, request.turn, live, { position: done.output.position ?? running.lastSeq, lost, started: [] });
    if (live && done.output.resumed !== undefined) {
      const answered = decodeJson(done.output.resumed);
      await platform.resumed(decodeOrCorrupt(() => suspensionFrom(answered.suspension)), decodeOrCorrupt(() => resumeFrom(answered.resume)), true);
    }
  }
  throw new Panic(`turn ${request.turn} did not finish within its ${stepsMax} steps`);
}

function undispatchedIn(lines, turn) {
  const seqs = [];
  for (const line of lines) {
    const record = decodeOrCorrupt(() => decodeLine(line, false));
    if (record.kind === "recovery" && record.turn === turn) seqs.push(...record.undispatched);
  }
  return seqs;
}

async function recordEvents(platform, lines, folded, turn, live, drained) {
  const inbox = await platform.inbox(turn, live);
  if (inbox === null) return;
  const queued = drained.lost.splice(0);
  const arrived = [...queued, ...inbox];
  if (arrived.length === 0) return;
  const log = folded.turns.get(turn);
  const position = drained.position;
  const events = log.events.slice();
  const open = new Map(folded.jobs);
  for (const [key, tool] of drained.started) open.set(key, tool);
  const unrecorded = new Set(drained.started.map(([key]) => key));
  const jobs = { open, recorded: (key) => !unrecorded.has(key) && jobRecorded(log.sites, turn, key) };
  const answers = [];
  const recorded = [];
  for (let index = 0; index < arrived.length; index++) {
    const delivery = arrived[index];
    const admitted = admitEvent(turn, events, position, delivery, jobs);
    if (admitted.record !== null) recorded.push(encodeLine(admitted.record, false));
    if (index < queued.length) {
      if (admitted.answer === "full") drained.lost.push(delivery);
      continue;
    }
    answers.push([delivery.id, admitted.answer]);
  }
  if (recorded.length > 0) {
    await platform.record(recorded);
    for (const line of recorded) {
      lines.push(line);
      applyRecord(folded, decodeLine(line, false));
    }
  }
  await platform.step(`events ${turn} at ${lines.length}`, { kind: "answer", turn, position, answers });
}

async function closeTurnInbox(platform, turn, log, live) {
  const arrived = ((await platform.inbox(turn, live)) ?? []).filter((delivery) => delivery.kind !== "job");
  if (arrived.length === 0) return;
  const answers = closingAnswers(log.events, arrived);
  await platform.step(`close ${turn}`, { kind: "answer", turn, position: log.finished.last, answers });
}

const WORKFLOW_BINDING = [
  "import { createHook, getStepMetadata, sleep } from \"workflow\";",
  "import { harness } from \"./harness.mjs\";",
  "",
  "function host() {",
  "  return harness.runtime.defaultHost({",
  "    fetch: (url, init) => fetch(url, init),",
  "    toolsUrl: process.env.HL_TOOLS_URL,",
  "    listingsUrl: process.env.HL_LISTINGS_URL,",
  "    modelUrl: process.env.HL_MODEL_URL,",
  "    recoverUrl: process.env.HL_RECOVER_URL,",
  "    jobsUrl: process.env.HL_JOBS_URL,",
  "    now: () => Date.now(),",
  "    random: harness.platformRandom,",
  "  });",
  "}",
  "",
  "export async function harnessStep(input) {",
  "  \"use step\";",
  "  return harness.runStep(input, {",
  "    attempt: getStepMetadata().attempt,",
  "    attemptQuiet: getStepMetadata().attempt,",
  "    async nestedAttempt() {",
  "      return getStepMetadata().attempt;",
  "    },",
  "    cells: harness.processCells(),",
  "    hook() {},",
  "    host: host(),",
  "    async acknowledge(finish) {",
  "      const body = harness.runtime.encodeJson(harness.runtime.finishJson(finish));",
  "      const response = await fetch(process.env.HL_ACKNOWLEDGE_URL, { method: \"POST\", body });",
  "      if (!response.ok) throw new Error(\"acknowledgement answered \" + response.status);",
  "    },",
  "    async answer(turn, position, answers) {",
  "      const body = JSON.stringify({ turn, position, answers });",
  "      const response = await fetch(process.env.HL_EVENTS_URL, { method: \"POST\", body });",
  "      if (!response.ok) throw new Error(\"event answers answered \" + response.status);",
  "    },",
  "  });",
  "}",
  "",
  "export async function harnessJobs(turn, open) {",
  "  \"use step\";",
  "  return host().jobs(turn, open);",
  "}",
  "",
  "export async function harnessSide(lines, request) {",
  "  return harness.side(lines, request, host());",
  "}",
  "",
  "export async function harnessSession(sessionId, turnsMax, entry) {",
  "  \"use workflow\";",
  "  const inboxes = new Map();",
  "  const suspended = new Map();",
  "  let prompts = 0;",
  "  let prompt = null;",
  "  const nextPrompt = () => {",
  "    if (prompt === null) {",
  "      const hook = createHook({ token: sessionId + \":\" + prompts });",
  "      prompt = (async () => {",
  "        const payload = await hook;",
  "        prompts += 1;",
  "        prompt = null;",
  "        return payload.request;",
  "      })();",
  "    }",
  "    return prompt;",
  "  };",
  "  return harness.session({",
  "    turnsMax,",
  "    entry: entry ?? [],",
  "    async request() {",
  "      return nextPrompt();",
  "    },",
  "    async requestOrIdle(index, dueMs, idleRequest) {",
  "      return Promise.race([nextPrompt(), sleep(new Date(Number(dueMs))).then(() => idleRequest)]);",
  "    },",
  "    async step(name, input) {",
  "      return { output: await harnessStep(input), live: true };",
  "    },",
  "    async sleep(name, wait) {",
  "      await sleep(new Date(Number(wait.untilMs)));",
  "    },",
  "    async resume(name, suspension) {",
  "      let resumes = suspended.get(suspension.turn + \"/\" + suspension.seq);",
  "      if (resumes === undefined) {",
  "        resumes = createHook({ token: sessionId + \":\" + suspension.turn + \":\" + suspension.seq + \":resume\" })[Symbol.asyncIterator]();",
  "        suspended.set(suspension.turn + \"/\" + suspension.seq, resumes);",
  "      }",
  "      const next = await resumes.next();",
  "      return harness.runtime.encodeJson(harness.runtime.resumeJson(harness.runtime.resumeFrom(harness.runtime.decodeJson(next.value.resume))));",
  "    },",
  "    async resumed() {},",
  "    async hook() {},",
  "    async inbox(turn) {",
  "      let inbox = inboxes.get(turn);",
  "      if (inbox === undefined) {",
  "        inbox = { pending: [], hook: createHook({ token: sessionId + \":\" + turn + \":events\" }) };",
  "        inboxes.set(turn, inbox);",
  "        (async () => {",
  "          for await (const payload of inbox.hook) inbox.pending.push(harness.runtime.deliveryFrom(harness.runtime.decodeJson(payload.event)));",
  "        })();",
  "      }",
  "      return inbox.pending.splice(0);",
  "    },",
  "    async record() {},",
  "    async jobs(turn, step, open) {",
  "      if (step !== 0) return null;",
  "      return { lost: open.length === 0 ? [] : await harnessJobs(turn, open), live: true };",
  "    },",
  "  });",
  "}",
  "",
].join("\n");

const harness = {
  program: PROGRAM.name,
  target: "vercel",
  runtime,
  session,
  side,
  fork,
  runStep,
  platformRandom,
  processCells: () => new ProcessCells(),
  workflowBinding: WORKFLOW_BINDING,
};

export { harness };
