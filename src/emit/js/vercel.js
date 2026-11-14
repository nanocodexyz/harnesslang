import { createContext, Script } from "node:vm";
const VERCEL_TARGET = { ...TARGET_MODEL, inbound: "after_flush" };
const VERCEL_EPOCH = 0;
const STEPS_PER_EFFECT_MAX = 2;
const STEPS_PER_SUSPENSION_MAX = 2;
const STEPS_PER_TURN_FIXED = 2;

assert(VERCEL_TARGET.recovery === "in_step", "the Vercel adapter recovers external effects in their step");
assert(!VERCEL_TARGET.gateBeforeOutbound, "a Vercel step has no output gate");

class StepWait {
  constructor(wait) {
    this.wait = wait;
  }
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
  constructor(context, waited, resume) {
    this.context = context;
    this.waitedTimers = waited;
    this.delivered = resume;
    this.resuming = null;
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

  async flush(epoch, records) {
    assert(epoch === VERCEL_EPOCH, "a workflow run has one owner");
    throw new StepEnd(records);
  }

  async attempt() {
    const attempt = this.context.attempt;
    assert(Number.isSafeInteger(attempt) && attempt >= 1, "a step's attempt counts from 1");
    return attempt;
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
  assert(input.kind === "run", "a step acknowledges or runs");
  const records = input.lines.map((line) => decodeOrCorrupt(() => decodeLine(line, false)));
  const request = decodeOrCorrupt(() => turnRequestFromJson(decodeJson(input.request)));
  const setup = { target: VERCEL_TARGET, mutation: context.mutation ?? "none" };
  const waited = (input.waited ?? []).map((text) => decodeOrCorrupt(() => waitFrom(decodeJson(text))));
  const delivered = input.resume === undefined || input.resume === null ? null : decodeOrCorrupt(() => resumeFrom(decodeJson(input.resume)));
  const world = new StepWorld(context, waited, delivered);
  try {
    const ran = await runTurn(setup, world, VERCEL_EPOCH, records, request);
    if (ran.ran === "suspended") return { lines: [], suspended: encodeJson(suspensionJson(ran.suspension)) };
    throw new Panic(`a step ends at a flush, found the turn ${ran.ran}`);
  } catch (error) {
    if (error instanceof StepEnd) {
      const output = { lines: error.records.map((record) => encodeLine(record, false)) };
      const answered = world.resuming;
      if (answered !== null && error.records.some((record) => record.kind === "result" && record.turn === answered.suspension.turn && record.seq === answered.suspension.seq)) {
        output.resumed = encodeJson(object([["resume", resumeJson(answered.resume)], ["suspension", suspensionJson(answered.suspension)]]));
      }
      return output;
    }
    if (error instanceof StepWait) return { lines: [], wait: encodeJson(waitJson(error.wait)) };
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

async function session(platform) {
  const lines = [];
  const folded = fold([]);
  for (let index = 0; index < platform.turnsMax; index++) {
    const requestText = await platform.request(index);
    await driveTurn(platform, lines, folded, requestText);
  }
}

async function driveTurn(platform, lines, folded, requestText) {
  const request = decodeOrCorrupt(() => turnRequestFromJson(decodeJson(requestText)));
  const handler = handlerNamed(request.handler);
  const stepsMax = (STEPS_PER_EFFECT_MAX + STEPS_PER_SUSPENSION_MAX) * handler.effectsMax + STEPS_PER_TURN_FIXED;
  let live = false;
  const waited = [];
  let resume = null;
  for (let step = 0; step < stepsMax; step++) {
    const log = folded.turns.get(request.turn);
    if (log !== undefined && log.finished !== null) {
      if (live) await platform.hook(POINT_BEFORE_ACKNOWLEDGE);
      const finish = { turn: request.turn, outcome: log.finished.outcome, result: log.finished.result };
      const acknowledged = await platform.step(`acknowledge ${request.turn}`, { kind: "acknowledge", finish: encodeJson(finishJson(finish)) });
      if (eventsTaken()) await closeTurnInbox(platform, request.turn, log, acknowledged.live);
      return;
    }
    const done = await platform.step(`turn ${request.turn} step ${step}`, { kind: "run", lines: lines.slice(), request: requestText, waited: waited.slice(), resume });
    resume = null;
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
    const ran = folded.turns.get(request.turn);
    if (eventsTaken() && ran.finished === null) await recordEvents(platform, lines, folded, request.turn, live);
    if (live && done.output.resumed !== undefined) {
      const answered = decodeJson(done.output.resumed);
      await platform.resumed(decodeOrCorrupt(() => suspensionFrom(answered.suspension)), decodeOrCorrupt(() => resumeFrom(answered.resume)), true);
    }
  }
  throw new Panic(`turn ${request.turn} did not finish within its ${stepsMax} steps`);
}

async function recordEvents(platform, lines, folded, turn, live) {
  const arrived = await platform.inbox(turn, live);
  if (arrived.length === 0) return;
  const log = folded.turns.get(turn);
  const position = log.lastSeq;
  const events = log.events.slice();
  const answers = [];
  const recorded = [];
  for (const delivery of arrived) {
    const admitted = admitEvent(turn, events, position, delivery);
    if (admitted.record !== null) recorded.push(encodeLine(admitted.record, false));
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
  const arrived = await platform.inbox(turn, live);
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
  "    now: () => Date.now(),",
  "    random: harness.platformRandom,",
  "  });",
  "}",
  "",
  "export async function harnessStep(input) {",
  "  \"use step\";",
  "  return harness.runStep(input, {",
  "    attempt: getStepMetadata().attempt,",
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
  "export async function harnessSide(lines, request) {",
  "  return harness.side(lines, request, host());",
  "}",
  "",
  "export async function harnessSession(sessionId, turnsMax) {",
  "  \"use workflow\";",
  "  const inboxes = new Map();",
  "  const suspended = new Map();",
  "  return harness.session({",
  "    turnsMax,",
  "    async request(index) {",
  "      const hook = createHook({ token: sessionId + \":\" + index });",
  "      const payload = await hook;",
  "      return payload.request;",
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
  runStep,
  platformRandom,
  processCells: () => new ProcessCells(),
  workflowBinding: WORKFLOW_BINDING,
};

export { harness };
