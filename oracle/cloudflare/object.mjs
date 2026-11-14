import worker, { HarnessObject as GeneratedObject, harness } from "./worker.mjs";

const INJECTED = "hl-drill injected";
const FAULT_ERRORS = {
  insert: "SQLITE_FULL: database or disk is full",
  alarm: "Durable Object storage operation exceeded timeout which caused object to be reset",
};
const NO_FAULT = "none";
const NO_CRASH = "none";
const CRASH_MARKER = "hl-drill crash";
const ALARM_MARKER = "hl-drill alarm";
const UNCAUGHT_HEADER = "hl-drill-uncaught";
const HTTP_INTERNAL_ERROR = 500;
const counts = { insert: 0, alarm: 0 };
const traced = { hooks: 0, clock: 0 };

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
    sync() {
      return storage.sync();
    },
  };
}

function spinUntilKilled(point) {
  console.log(`${CRASH_MARKER} ${harness.runtime.encodeJson(point)}`);
  for (;;) {}
}

function traceSimulation(env) {
  const crashAt = env.HL_DRILL_CRASH_HOOK === NO_CRASH ? null : Number(env.HL_DRILL_CRASH_HOOK);
  return {
    mutation: "none",
    hook(point) {
      traced.hooks += 1;
      if (traced.hooks === crashAt) spinUntilKilled(point);
    },
    gate() {},
  };
}

function traceHost(env) {
  const clock = harness.runtime.decodeJson(env.HL_DRILL_CLOCK);
  const base = harness.runtime.defaultHost({
    fetch: (url, init) => fetch(url, init),
    toolsUrl: env.HL_TOOLS_URL,
    modelUrl: env.HL_MODEL_URL,
    now: () => 0,
    random: () => 0n,
  });
  const next = (kind) => {
    const answer = clock[traced.clock];
    if (answer === undefined || answer.kind !== kind) throw new Error(`the trace has no ${kind} answer at clock position ${traced.clock}`);
    traced.clock += 1;
    return answer.value;
  };
  return { tool: base.tool, model: base.model, now: () => next("now"), random: () => next("random") };
}

class HarnessObject extends GeneratedObject {
  constructor(ctx, env) {
    const plan = faultPlan(env.HL_DRILL_FAULT);
    const storage = plan === null ? ctx.storage : faultyStorage(ctx.storage, plan);
    const tracing = env.HL_DRILL_CLOCK !== "";
    const drillEnv = tracing ? { ...env, HL_HOST: traceHost(env), HL_SIMULATION: traceSimulation(env) } : env;
    super({ storage }, drillEnv);
  }

  async fetch(request) {
    try {
      return await super.fetch(request);
    } catch (error) {
      return new Response(`${error?.name}: ${error?.message}`, { status: HTTP_INTERNAL_ERROR, headers: { [UNCAUGHT_HEADER]: "1" } });
    }
  }

  async alarm() {
    const result = await super.alarm();
    const finish = result.finish === undefined ? null : harness.runtime.finishJson(result.finish);
    console.log(`${ALARM_MARKER} ${harness.runtime.encodeJson(Object.assign(Object.create(null), { finish, ran: result.ran }))}`);
    return result;
  }
}

export { HarnessObject };
export default worker;
