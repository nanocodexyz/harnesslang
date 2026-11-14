import { createContext, Script } from "node:vm";

export function stepFromJson(rt, json) {
  const pair = Object.entries(json);
  if (pair.length !== 1) throw new Error(`a traced step has one tag: ${rt.encodeJson(json)}`);
  const [kind, value] = pair[0];
  switch (kind) {
    case "call":
      return { kind, id: Number(value.id), name: value.name, input: value.input };
    case "timeout":
      return { kind, id: Number(value.id), delayMs: value.delay_ms };
    case "bytes":
      return { kind, length: value.length };
    case "done":
      return { kind, output: value.output };
    case "threw":
      return { kind, error: value.error };
    default:
      return { kind };
  }
}

export function inputJson(rt, input) {
  const o = rt.object;
  switch (input.kind) {
    case "issued":
      return o([["issued", null]]);
    case "refused":
      return o([["refused", o([["error", input.error]])]]);
    case "result":
      return o([["result", o([["encoded", input.encoded], ["id", BigInt(input.id)], ["success", input.success]])]]);
    case "timer":
      return o([["timer", o([["id", BigInt(input.id)]])]]);
    case "clock":
    case "monotonic":
      return o([[input.kind, o([["value", input.value]])]]);
    case "random":
      return o([["random", o([["value", new rt.Fraction(input.value)]])]]);
    case "bytes":
      return o([["bytes", o([["data", input.data.map((byte) => BigInt(byte))]])]]);
    default:
      throw new Error(`unknown engine input ${input.kind}`);
  }
}

export function eventJson(rt, event) {
  const o = rt.object;
  switch (event.kind) {
    case "delivered":
      return o([["delivered", o([["cell", BigInt(event.cell)], ["id", BigInt(event.id)]])]]);
    case "diverged":
      return o([["diverged", o([["cell", BigInt(event.cell)], ["reason", event.reason]])]]);
    case "finished": {
      const output = rt.cellOutcomeOutput(event.outcome);
      const json = output.tag === "ok" ? o([["ok", output.value]]) : o([["failed", output.error]]);
      return o([["finished", o([["cell", BigInt(event.cell)], ["output", json]])]]);
    }
    default:
      throw new Error(`unknown cell event ${event.kind}`);
  }
}

export class DurableCells {
  constructor(rt, lines) {
    this.cells = new Map();
    this.nested = new Set();
    this.entries = new Map();
    for (const line of lines) {
      const record = rt.decodeLine(line, false);
      if (record.kind === "cell") {
        const cell = this.cells.get(record.cell) ?? { header: false, finished: false };
        cell.header ||= record.fresh;
        cell.finished ||= record.update.finished;
        this.cells.set(record.cell, cell);
        this.entries.set(record.cell, [...(this.entries.get(record.cell) ?? []), ...record.update.entries]);
      }
      if (record.kind === "nested_call") this.nested.add(`${record.turn}/${record.seq}/code-${record.nested}`);
    }
  }

  header(cell) {
    return this.cells.get(cell)?.header ?? false;
  }

  finished(cell) {
    return this.cells.get(cell)?.finished ?? false;
  }
}

export class TracedCells {
  constructor(seed, check) {
    this.seed = seed;
    this.check = check;
    this.memory = new Map();
    this.replaying = new Map();
    this.contexts = new Map();
  }

  key(cell) {
    return `${this.seed.stats.runs}/${cell}`;
  }

  exchange(cell, kind, detail, live, raw) {
    const key = this.key(cell);
    const remembered = this.memory.get(key) ?? [];
    const at = this.replaying.get(key);
    if (at !== undefined && at < remembered.length) {
      const before = remembered[at];
      this.check.same(`re-executed cell ${cell} exchange ${at}`, before.detail, detail);
      this.replaying.set(key, at + 1);
      return before.answer;
    }
    this.replaying.delete(key);
    const answer = live();
    remembered.push({ kind, detail, answer, raw });
    this.memory.set(key, remembered);
    return answer;
  }

  start(cell, script, context) {
    const { rt, cursor, same } = this.check;
    this.contexts.set(this.key(cell), { script, context });
    if (this.check.durable().finished(cell)) throw this.check.failure("cell_finishes_once", `the engine started on cell ${cell}, whose finished update is durable`);
    const key = this.key(cell);
    if (this.memory.has(key)) this.replaying.set(key, 0);
    return this.exchange(cell, "start", rt.object([["script", script]]), () => {
      this.check.checks.started(this.seed.stats.runs, cell, this.check.durable().entries.get(cell) ?? []);
      const event = cursor.next("cell_start", `cell ${cell}`);
      same("cell start cell", event.cell, BigInt(cell));
      same("cell start script", event.script, script);
      this.seed.stats.cells += 1;
      const step = stepFromJson(rt, event.step);
      this.check.checks.stepped(this.seed.stats.runs, cell, step);
      return step;
    }, null);
  }

  resume(cell, input) {
    const { rt, cursor, same } = this.check;
    const json = inputJson(rt, input);
    return this.exchange(cell, "resume", json, () => {
      const event = cursor.next("cell_resume", `cell ${cell} ${rt.encodeJson(json)}`);
      same("cell resume cell", event.cell, BigInt(cell));
      same("cell resume input", event.input, json);
      const step = stepFromJson(rt, event.step);
      this.check.checks.stepped(this.seed.stats.runs, cell, step);
      return step;
    }, input);
  }

  observe(event) {
    const { rt, cursor, same } = this.check;
    const json = eventJson(rt, event);
    this.exchange(event.cell, "observe", json, () => {
      const traced = cursor.next("cell", rt.encodeJson(json));
      same("cell event", traced.observed, json);
      if (event.kind === "diverged") this.seed.stats.divergences += 1;
      if (event.kind === "delivered") this.check.checks.delivered(this.seed.stats.runs, event.cell, event.id);
      return null;
    }, event);
  }
}

const SYNC_KINDS = ["clock", "monotonic", "random", "bytes"];
const SCRIPTED_SYNTAX_ERROR = "SyntaxError: the scripted engine cannot run line";

function stepText(rt, step) {
  const shown = { ...step };
  delete shown.value;
  delete shown.data;
  return rt.encodeJson(rt.object(Object.entries(shown).map(([key, value]) => [key, typeof value === "number" ? BigInt(value) : value])));
}

export async function engineDifferential(rt, cells, totals) {
  for (const [key, exchanges] of cells.memory) {
    if (exchanges.length === 0 || exchanges[0].kind !== "start") continue;
    if (exchanges.some((exchange) => exchange.kind === "observe" && exchange.raw.kind === "diverged")) continue;
    const first = exchanges[0].answer;
    if (first.kind === "threw" && first.error.startsWith(SCRIPTED_SYNTAX_ERROR)) {
      totals.unparsed += 1;
      continue;
    }
    const { script, context } = cells.contexts.get(key);
    const engineSteps = exchanges.filter((exchange) => exchange.kind !== "observe");
    const values = engineSteps.filter((exchange) => exchange.kind === "resume" && SYNC_KINDS.includes(exchange.raw.kind)).map((exchange) => exchange.raw);
    const take = (kind) => {
      const next = values.shift();
      if (next === undefined) return kind === "bytes" ? [] : 0n;
      if (next.kind !== kind) throw new Error(`cell ${key}: the script engine read ${kind} where the scripted engine read ${next?.kind ?? "nothing"}`);
      return kind === "bytes" ? next.data : next.value;
    };
    let run;
    try {
      run = new Script(`(async function cell(${rt.CELL_SCRIPT_PARAMETERS.join(", ")}) {\n${script}\n})`).runInContext(createContext(Object.create(null)));
    } catch (error) {
      run = async () => {
        throw error;
      };
    }
    const engine = new rt.ScriptEngine(run, {
      limits: context.limits,
      replay: [],
      platform: { clock: () => take("clock"), monotonic: () => take("monotonic"), random: () => take("random"), bytes: () => take("bytes") },
      macrotask: () => new Promise((resolve) => setImmediate(resolve)),
    });
    let step = await engine.start();
    for (let index = 0; index < engineSteps.length; index++) {
      const scripted = engineSteps[index].answer;
      const next = engineSteps[index + 1];
      if (scripted.kind === "timeout" && next !== undefined && next.raw.kind === "refused") {
        index += 1;
        const after = engineSteps[index].answer;
        if (stepText(rt, step) !== stepText(rt, after)) throw new Error(`cell ${key} step ${index}: the scripted engine threw ${stepText(rt, after)} after a refused timer, the script engine took ${stepText(rt, step)}`);
        totals.steps += 1;
        break;
      }
      if (stepText(rt, step) !== stepText(rt, scripted)) {
        throw new Error(`cell ${key} step ${index}: the scripted engine took ${stepText(rt, scripted)}, the script engine took ${stepText(rt, step)}`);
      }
      totals.steps += 1;
      if (next === undefined || step.kind === "done" || step.kind === "threw") break;
      step = await engine.resume(next.raw);
    }
    totals.cells += 1;
  }
}

const OUTPUT_STEP_KINDS = ["call", "timeout", "clock", "monotonic", "random", "bytes"];

function stepMatchesEntry(rt, step, entry) {
  if (step.kind !== entry.kind) return false;
  if (step.kind === "bytes") return BigInt(entry.data.length) === step.length;
  return rt.sameStep(entry, { atMs: 0n, ...step });
}

export class CellChecks {
  constructor(rt, failure) {
    this.rt = rt;
    this.failure = failure;
    this.outputs = new Map();
    this.divergedCells = new Set();
    this.running = new Map();
    this.issued = new Map();
    this.armed = new Map();
    this.dispatchedOutputs = new Map();
  }

  started(run, cell, durableEntries) {
    this.outputs.set(`${run}/${cell}`, { journal: durableEntries.filter((entry) => !this.rt.entryIsInput(entry)), taken: 0 });
    this.divergedCells.delete(`${run}/${cell}`);
  }

  stepped(run, cell, step) {
    const key = `${run}/${cell}`;
    const outputs = this.outputs.get(key);
    if (outputs === undefined || !OUTPUT_STEP_KINDS.includes(step.kind)) return;
    const index = outputs.taken;
    outputs.taken += 1;
    if (index < outputs.journal.length && !stepMatchesEntry(this.rt, step, outputs.journal[index])) this.divergedCells.add(key);
  }

  delivered(run, cell, id) {
    this.running.get(`${run}/${cell}`)?.delete(id);
  }

  dispatched(run, nested, request, output) {
    const { cell, id, code } = nested;
    const key = `${run}/${cell}`;
    const callId = `${request.turn}/${request.seq}/code-${id}`;
    if (this.divergedCells.has(key)) throw this.failure("cell_replay_equal", `nested call ${callId} was dispatched after cell ${cell} diverged from its journal`);
    const bounds = this.rt.PROGRAM.codes[code];
    const issued = this.issued.get(cell) ?? new Set();
    issued.add(id);
    this.issued.set(cell, issued);
    if (BigInt(issued.size) > bounds.calls.value) {
      throw this.failure("cell_bounded", `cell ${cell} dispatched ${issued.size} nested calls, past calls (${bounds.calls.name} = ${bounds.calls.value})`);
    }
    const running = this.running.get(key) ?? new Set();
    running.add(id);
    this.running.set(key, running);
    if (BigInt(running.size) > bounds.parallel.value) {
      throw this.failure("cell_bounded", `cell ${cell} runs ${running.size} nested calls at once, past parallel (${bounds.parallel.name} = ${bounds.parallel.value})`);
    }
    const outputs = this.dispatchedOutputs.get(callId) ?? [];
    outputs.push(output);
    this.dispatchedOutputs.set(callId, outputs);
  }

  waited(wait, code) {
    const bounds = this.rt.PROGRAM.codes[code];
    const armed = this.armed.get(wait.cell) ?? new Set();
    armed.add(wait.timer);
    this.armed.set(wait.cell, armed);
    if (BigInt(armed.size) > bounds.timers.value) {
      throw this.failure("cell_bounded", `cell ${wait.cell} waits on ${armed.size} timers, past timers (${bounds.timers.name} = ${bounds.timers.value})`);
    }
    if (wait.delayMs > bounds.timerMs.value) {
      throw this.failure("cell_bounded", `cell ${wait.cell} waits ${wait.delayMs} ms, past timer_ms (${bounds.timerMs.name} = ${bounds.timerMs.value})`);
    }
  }

  flushed(lines) {
    const seen = new Set();
    for (const line of lines) {
      const record = this.rt.decodeLine(line, false);
      if (record.kind !== "nested_call") continue;
      const callId = `${record.turn}/${record.seq}/code-${record.nested}`;
      if (seen.has(callId)) throw this.failure("seq_unique", `nested call ${callId} has two call records`);
      seen.add(callId);
    }
  }

  ended(lines) {
    const rt = this.rt;
    const reported = new Set();
    const tools = new Map();
    for (const line of lines) {
      const record = rt.decodeLine(line, false);
      if (record.kind === "recovery") for (const [seq, nested] of record.nested) reported.add(`${record.turn}/${seq}/code-${nested}`);
      if (record.kind === "nested_result") {
        const callId = `${record.turn}/${record.seq}/code-${record.nested}`;
        const outputs = this.dispatchedOutputs.get(callId) ?? [];
        const recorded = rt.encodeJson(record.output.tag === "ok" ? rt.object([["ok", record.output.value]]) : rt.object([["failed", record.output.error]]));
        if (!outputs.some((output) => rt.encodeJson(output) === recorded)) {
          throw this.failure("unknown_nested_reported", `nested call ${callId} has result ${recorded}, which its tool never returned`);
        }
      }
      if (record.kind === "cell") this.cellEnded(record, reported, tools);
    }
  }

  cellEnded(record, reported, tools) {
    const rt = this.rt;
    const origin = record.update.header?.originCallId;
    if (origin !== undefined) tools.set(record.cell, { origin, names: new Map() });
    const cell = tools.get(record.cell);
    for (const entry of record.update.entries) {
      if (entry.kind === "call") cell.names.set(entry.id, entry.name);
      if (entry.kind !== "result" || entry.success) continue;
      const json = rt.decodeJson(entry.encoded);
      if (!(rt.isObject(json) && json.outcome === "unknown")) continue;
      const callId = `${cell.origin}/code-${entry.id}`;
      const tool = rt.PROGRAM.tools.find((candidate) => candidate.name === cell.names.get(entry.id));
      if (tool !== undefined && tool.effect !== "external") throw this.failure("nested_known", `${tool.effect} nested call ${callId} ended with the unknown outcome`);
      if (!reported.has(callId)) throw this.failure("unknown_nested_reported", `nested call ${callId} was given the unknown outcome without a recovery record listing it`);
    }
  }
}
