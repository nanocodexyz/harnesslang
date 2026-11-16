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
    case "yielded":
      return o([["yielded", o([["cell", BigInt(event.cell)]])]]);
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

function doubles(rt, json) {
  if (json instanceof rt.Fraction) return "double " + Number(json.text);
  if (Array.isArray(json)) return json.map((item) => doubles(rt, item));
  if (json === null || typeof json !== "object") return json;
  return Object.fromEntries(Object.entries(json).map(([key, value]) => [key, doubles(rt, value)]));
}

export class TracedCells {
  constructor(seed, check) {
    this.seed = seed;
    this.check = check;
    this.memory = new Map();
    this.replaying = new Map();
    this.contexts = new Map();
    this.generations = new Map();
  }

  key(cell) {
    const base = `${this.seed.stats.runs}/${cell}`;
    const generation = this.generations.get(base) ?? 0;
    return generation === 0 ? base : `${base}#${generation}`;
  }

  continued(cell) {
    const next = this.check.cursor.peek();
    if (next?.event !== "cell_start" || next.cell !== BigInt(cell) || !this.memory.has(this.key(cell))) return;
    const base = `${this.seed.stats.runs}/${cell}`;
    this.generations.set(base, (this.generations.get(base) ?? 0) + 1);
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
    this.continued(cell);
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
      this.check.checks.stepped(this.seed.stats.runs, cell, stepFromJson(rt, event.step));
      return event.step;
    }, null);
  }

  resume(cell, json) {
    const { rt, cursor, same } = this.check;
    return this.exchange(cell, "resume", json, () => {
      const event = cursor.next("cell_resume", `cell ${cell} ${rt.encodeJson(json)}`);
      same("cell resume cell", event.cell, BigInt(cell));
      same("cell resume input", doubles(rt, event.input), doubles(rt, json));
      this.check.checks.stepped(this.seed.stats.runs, cell, stepFromJson(rt, event.step));
      return event.step;
    }, json);
  }

  observe(cell, json) {
    const { rt, cursor, same } = this.check;
    this.exchange(cell, "observe", json, () => {
      const traced = cursor.next("cell", rt.encodeJson(json));
      same("cell event", traced.observed, json);
      if (json.diverged !== undefined) this.seed.stats.divergences += 1;
      return null;
    }, json);
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

  dispatched(run, nested, request, output) {
    const { cell, id } = nested;
    const key = `${run}/${cell}`;
    const callId = `${request.turn}/${request.seq}/code-${id}`;
    if (this.divergedCells.has(key)) throw this.failure("cell_replay_equal", `nested call ${callId} was dispatched after cell ${cell} diverged from its journal`);
    const outputs = this.dispatchedOutputs.get(callId) ?? [];
    outputs.push(output);
    this.dispatchedOutputs.set(callId, outputs);
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
      if (!reported.has(callId)) throw this.failure("unknown_nested_reported", `nested call ${callId} was given the unknown outcome without a recovery record listing it`);
    }
  }
}
