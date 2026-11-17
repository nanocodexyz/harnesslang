import { createHook, getStepMetadata, sleep } from "workflow";
import { harness } from "./vercel.mjs";

function host() {
  return harness.runtime.defaultHost({
    fetch: (url, init) => fetch(url, init),
    toolsUrl: process.env.HL_TOOLS_URL,
    listingsUrl: process.env.HL_LISTINGS_URL,
    modelUrl: process.env.HL_MODEL_URL,
    env: process.env,
    recoverUrl: process.env.HL_RECOVER_URL,
    jobsUrl: process.env.HL_JOBS_URL,
    cellsUrl: process.env.HL_CELLS_URL,
    now: () => Date.now(),
    random: harness.platformRandom,
  });
}

export async function harnessStep(input) {
  "use step";
  return harness.runStep(input, {
    attempt: getStepMetadata().attempt,
    nestedAttempt: () => getStepMetadata().attempt,
    hook() {},
    observeWaited() {},
    host: host(),
    async acknowledge(finish) {
      const body = harness.runtime.encodeJson(harness.runtime.finishJson(finish));
      const response = await fetch(process.env.HL_ACKNOWLEDGE_URL, { method: "POST", body });
      if (!response.ok) throw new Error("acknowledgement answered " + response.status);
    },
  });
}

export async function harnessSide(lines, request) {
  return harness.side(lines, request, host());
}

export async function harnessSession(sessionId, turnsMax, entry) {
  "use workflow";
  const suspended = new Map();
  let prompts = 0;
  let prompt = null;
  const nextPrompt = () => {
    if (prompt === null) {
      const hook = createHook({ token: sessionId + ":" + prompts });
      prompt = (async () => {
        const payload = await hook;
        prompts += 1;
        prompt = null;
        return payload.request;
      })();
    }
    return prompt;
  };
  return harness.session({
    turnsMax,
    entry: entry ?? [],
    async request() {
      return nextPrompt();
    },
    async requestOrIdle(index, dueMs, idleRequest) {
      return Promise.race([nextPrompt(), sleep(new Date(Number(dueMs))).then(() => idleRequest)]);
    },
    async step(name, input) {
      return { output: await harnessStep(input), live: true };
    },
    async resume(name, suspension) {
      let resumes = suspended.get(suspension.turn + "/" + suspension.seq);
      if (resumes === undefined) {
        resumes = createHook({ token: sessionId + ":" + suspension.turn + ":" + suspension.seq + ":resume" })[Symbol.asyncIterator]();
        suspended.set(suspension.turn + "/" + suspension.seq, resumes);
      }
      const next = await resumes.next();
      return harness.runtime.encodeJson(harness.runtime.resumeJson(harness.runtime.resumeFrom(harness.runtime.decodeJson(next.value.resume))));
    },
    async resumed() {},
    async sleep(name, wait) {
      await sleep(new Date(Number(wait.untilMs)));
    },
    async hook() {},
  });
}
