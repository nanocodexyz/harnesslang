const VERCEL_TARGET = { recovery: "in_step", gateBeforeOutbound: false };
const VERCEL_EPOCH = 0;
const STEPS_PER_EFFECT_MAX = 2;
const STEPS_PER_TURN_FIXED = 2;

class StepWorld {
  constructor(context) {
    this.context = context;
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
  try {
    const ran = await runTurn(setup, new StepWorld(context), VERCEL_EPOCH, records, request);
    throw new Panic(`a step ends at a flush, found the turn ${ran.ran}`);
  } catch (error) {
    if (error instanceof StepEnd) return { lines: error.records.map((record) => encodeLine(record, false)) };
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
  for (let step = 0; step < stepsMax; step++) {
    const log = folded.turns.get(request.turn);
    if (log !== undefined && log.finished !== null) {
      if (live) await platform.hook(POINT_BEFORE_ACKNOWLEDGE);
      const finish = { turn: request.turn, outcome: log.finished.outcome, result: log.finished.result };
      await platform.step(`acknowledge ${request.turn}`, { kind: "acknowledge", finish: encodeJson(finishJson(finish)) });
      return;
    }
    const done = await platform.step(`turn ${request.turn} step ${step}`, { kind: "run", lines: lines.slice(), request: requestText });
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
  "import { createHook, getStepMetadata } from \"workflow\";",
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
  workflowBinding: WORKFLOW_BINDING,
};

export { harness };
