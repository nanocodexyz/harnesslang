import worker, { HarnessObject as GeneratedObject, harness } from "./worker.mjs";

const INJECTED = "hl-drill injected";
const FAULT_ERRORS = {
  insert: "SQLITE_FULL: database or disk is full",
  alarm: "Durable Object storage operation exceeded timeout which caused object to be reset",
};
const NO_FAULT = "none";
const UNCAUGHT_HEADER = "hl-drill-uncaught";
const HTTP_INTERNAL_ERROR = 500;
const counts = { insert: 0, alarm: 0 };

function faultPlan(text) {
  if (text === NO_FAULT) return null;
  const [kind, at] = text.split(" ");
  if (FAULT_ERRORS[kind] === undefined || !/^[0-9]+$/.test(at)) throw new Error(`bad HL_DRILL_FAULT '${text}'`);
  return { kind, at: Number(at) };
}

function injected(plan, kind) {
  counts[kind] += 1;
  return plan.kind === kind && counts[kind] === plan.at;
}

function faultyStorage(storage, plan) {
  return {
    sql: {
      exec(query, ...bindings) {
        if (query === harness.statements.insert && injected(plan, "insert")) throw new Error(`${INJECTED} ${FAULT_ERRORS.insert}`);
        return storage.sql.exec(query, ...bindings);
      },
    },
    setAlarm(time) {
      if (injected(plan, "alarm")) return Promise.reject(new Error(`${INJECTED} ${FAULT_ERRORS.alarm}`));
      return storage.setAlarm(time);
    },
    deleteAlarm() {
      return storage.deleteAlarm();
    },
    getAlarm() {
      return storage.getAlarm();
    },
  };
}

function worldHost(env) {
  const base = harness.runtime.defaultHost({
    fetch: (url, init) => fetch(url, init),
    toolsUrl: env.HL_TOOLS_URL,
    modelUrl: env.HL_MODEL_URL,
    now: () => 0,
    random: () => 0n,
  });
  const ask = async (path) => {
    const response = await fetch(env.HL_DRILL_WORLD_URL + path, { method: "POST" });
    return BigInt(await response.text());
  };
  return { tool: base.tool, model: base.model, now: () => ask("/now"), random: () => ask("/random") };
}

class HarnessObject extends GeneratedObject {
  constructor(ctx, env) {
    const plan = faultPlan(env.HL_DRILL_FAULT);
    const storage = plan === null ? ctx.storage : faultyStorage(ctx.storage, plan);
    const drillEnv = env.HL_DRILL_WORLD_URL === "" ? env : { ...env, HL_HOST: worldHost(env) };
    super({ storage }, drillEnv);
  }

  async fetch(request) {
    try {
      return await super.fetch(request);
    } catch (error) {
      return new Response(`${error?.name}: ${error?.message}`, { status: HTTP_INTERNAL_ERROR, headers: { [UNCAUGHT_HEADER]: "1" } });
    }
  }
}

export { HarnessObject };
export default worker;
