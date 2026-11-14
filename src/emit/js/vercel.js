import { createContext, Script } from "node:vm";
const VERCEL_TARGET = TARGET_MODEL;
const VERCEL_EPOCH = 0;
const STEPS_PER_EFFECT_MAX = 2;
const STEPS_PER_TURN_FIXED = 2;

assert(VERCEL_TARGET.recovery === "in_step", "the Vercel adapter recovers external effects in their step");
assert(!VERCEL_TARGET.gateBeforeOutbound, "a Vercel step has no output gate");

class StepWait {
  constructor(wait) {
    this.wait = wait;
  }
}

function waitJson(wait) {
  return object([["cell", BigInt(wait.cell)], ["delay_ms", wait.delayMs], ["timer", BigInt(wait.timer)], ["until_ms", wait.untilMs]]);
}

function waitFrom(json) {
  if (!isObject(json)) throw new DecodeError("a wait is an object");
  const fields = new Fields("wait", json);
  fields.exact(["cell", "delay_ms", "timer", "until_ms"]);
  const big = (key) => {
    const value = fields.get(key);
    if (typeof value !== "bigint") throw new DecodeError(`a wait's '${key}' is not a number`);
    return value;
  };
  return { cell: fields.number("cell"), timer: fields.number("timer"), delayMs: big("delay_ms"), untilMs: big("until_ms") };
}

class StepWorld {
  constructor(context, waited) {
    this.context = context;
    this.waitedTimers = waited;
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
    return this.waitedTimers.some((done) => done.cell === wait.cell && done.timer === wait.timer);
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
  assert(input.kind === "run", "a step acknowledges or runs");
  const records = input.lines.map((line) => decodeOrCorrupt(() => decodeLine(line, false)));
  const request = decodeOrCorrupt(() => turnRequestFromJson(decodeJson(input.request)));
  const setup = { target: VERCEL_TARGET, mutation: context.mutation ?? "none" };
  const waited = (input.waited ?? []).map((text) => decodeOrCorrupt(() => waitFrom(decodeJson(text))));
  try {
    const ran = await runTurn(setup, new StepWorld(context, waited), VERCEL_EPOCH, records, request);
    throw new Panic(`a step ends at a flush, found the turn ${ran.ran}`);
  } catch (error) {
    if (error instanceof StepEnd) return { lines: error.records.map((record) => encodeLine(record, false)) };
    if (error instanceof StepWait) return { lines: [], wait: encodeJson(waitJson(error.wait)) };
    throw error;
  }
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
  const stepsMax = STEPS_PER_EFFECT_MAX * handler.effectsMax + STEPS_PER_TURN_FIXED;
  let live = false;
  const waited = [];
  for (let step = 0; step < stepsMax; step++) {
    const log = folded.turns.get(request.turn);
    if (log !== undefined && log.finished !== null) {
      if (live) await platform.hook(POINT_BEFORE_ACKNOWLEDGE);
      const finish = { turn: request.turn, outcome: log.finished.outcome, result: log.finished.result };
      await platform.step(`acknowledge ${request.turn}`, { kind: "acknowledge", finish: encodeJson(finishJson(finish)) });
      return;
    }
    const done = await platform.step(`turn ${request.turn} step ${step}`, { kind: "run", lines: lines.slice(), request: requestText, waited: waited.slice() });
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
  }
  throw new Panic(`turn ${request.turn} did not finish within its ${stepsMax} steps`);
}

const WORKFLOW_BINDING = [
  "import { createHook, getStepMetadata, sleep } from \"workflow\";",
  "import { harness } from \"./harness.mjs\";",
  "",
  "function host() {",
  "  return harness.runtime.defaultHost({",
  "    fetch: (url, init) => fetch(url, init),",
  "    toolsUrl: process.env.HL_TOOLS_URL,",
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
  "  });",
  "}",
  "",
  "export async function harnessSession(sessionId, turnsMax) {",
  "  \"use workflow\";",
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
  "    async hook() {},",
  "  });",
  "}",
  "",
].join("\n");

const harness = {
  program: PROGRAM.name,
  target: "vercel",
  runtime,
  session,
  runStep,
  platformRandom,
  processCells: () => new ProcessCells(),
  workflowBinding: WORKFLOW_BINDING,
};

export { harness };
