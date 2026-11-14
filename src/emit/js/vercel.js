const VERCEL_TARGET = { recovery: "in_step", gateBeforeOutbound: false, inbound: "after_flush" };
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

  async inbox() {
    throw new Panic("a step takes no events: the workflow function records them between steps");
  }

  async answer() {
    throw new Panic("a step answers no events: the workflow function does, by an answer step");
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
      const acknowledged = await platform.step(`acknowledge ${request.turn}`, { kind: "acknowledge", finish: encodeJson(finishJson(finish)) });
      if (eventsTaken()) await closeTurnInbox(platform, request.turn, log, acknowledged.live);
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
    const ran = folded.turns.get(request.turn);
    if (eventsTaken() && ran.finished === null) await recordEvents(platform, lines, folded, request.turn, live);
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
  "    async answer(turn, position, answers) {",
  "      const body = JSON.stringify({ turn, position, answers });",
  "      const response = await fetch(process.env.HL_EVENTS_URL, { method: \"POST\", body });",
  "      if (!response.ok) throw new Error(\"event answers answered \" + response.status);",
  "    },",
  "  });",
  "}",
  "",
  "export async function harnessSession(sessionId, turnsMax) {",
  "  \"use workflow\";",
  "  const inboxes = new Map();",
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
  runStep,
  platformRandom,
  workflowBinding: WORKFLOW_BINDING,
};

export { harness };
