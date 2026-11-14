import { createContext, Script } from "node:vm";
const CATCH = ".catch((error) => error)";
const TOOLS_PREFIX = "tools.";
const SLEEP_PREFIX = "await new Promise((resolve) => setTimeout(resolve, ";
const SLEEP_SUFFIX = "));";

export function scriptedEngine(rt) {
  const decoded = (text) => {
    try {
      return rt.decodeJson(text);
    } catch (error) {
      if (error instanceof rt.DecodeError) return undefined;
      throw error;
    }
  };

  const parseText = (text) => {
    const json = decoded(text);
    return typeof json === "string" ? json : null;
  };

  const between = (line, prefix, suffix) => (line.startsWith(prefix) && line.endsWith(suffix) && line.length >= prefix.length + suffix.length ? line.slice(prefix.length, line.length - suffix.length) : null);

  const parseCall = (text) => {
    const trimmed = text.trim();
    if (!trimmed.startsWith(TOOLS_PREFIX)) return null;
    const rest = trimmed.slice(TOOLS_PREFIX.length);
    const open = rest.indexOf("(");
    if (open < 0) return null;
    const inner = between(rest.slice(open + 1), "", ")" + CATCH);
    if (inner === null) return null;
    const input = decoded(inner);
    return input === undefined ? null : [rest.slice(0, open), input];
  };

  const afterBinding = (line) => {
    if (!line.startsWith("const ")) return null;
    const at = line.indexOf(" = ");
    if (at < 0 || !line.endsWith(";")) return null;
    return line.slice(at + 3, line.length - 1);
  };

  const parseLine = (line) => {
    const ms = between(line, SLEEP_PREFIX, SLEEP_SUFFIX);
    if (ms !== null) return /^[0-9]+$/.test(ms) ? { op: "sleep", ms: BigInt(ms) } : null;
    for (const [prefix, suffix, op] of [["console.log(", ");", "print"], ["throw new Error(", ");", "throw"], ["return ", ";", "return"]]) {
      const text = between(line, prefix, suffix);
      if (text !== null) {
        const value = parseText(text);
        return value === null ? null : { op, text: value };
      }
    }
    const value = afterBinding(line);
    if (value === null) return null;
    if (value === "Date.now()") return { op: "clock" };
    if (value === "performance.now()") return { op: "monotonic" };
    if (value === "Math.random()") return { op: "random" };
    const length = between(value, "crypto.getRandomValues(new Uint8Array(", "))");
    if (length !== null) return /^[0-9]+$/.test(length) ? { op: "bytes", length: BigInt(length) } : null;
    const items = between(value, "await Promise.all([", "])");
    if (items !== null) {
      const calls = [];
      let rest = items;
      while (rest.length > 0) {
        const end = rest.indexOf(CATCH);
        const piece = end < 0 ? rest : rest.slice(0, end + CATCH.length);
        rest = end < 0 ? "" : rest.slice(end + CATCH.length);
        const call = parseCall(piece.replace(/^(, )+/, ""));
        if (call === null) return null;
        calls.push(call);
      }
      return calls.length > 0 ? { op: "calls", calls } : null;
    }
    if (!value.startsWith("await ")) return null;
    const call = parseCall(value.slice("await ".length));
    return call === null ? null : { op: "calls", calls: [call] };
  };

  const parse = (script) => {
    const ops = [];
    const body = script.endsWith("\n") ? script.slice(0, -1) : script;
    const lines = body === "" ? [] : body.split("\n").map((line) => (line.endsWith("\r") ? line.slice(0, -1) : line));
    for (let index = 0; index < lines.length; index++) {
      const op = parseLine(lines[index]);
      if (op === null) return { error: `SyntaxError: the scripted engine cannot run line ${index + 1}` };
      ops.push(op);
    }
    return { ops };
  };

  class Machine {
    constructor(ops, divergeAt) {
      this.ops = ops;
      this.at = 0;
      this.nextCall = 0;
      this.nextTimer = 0;
      this.printed = [];
      this.state = { kind: "value" };
      this.steps = 0n;
      this.divergeAt = divergeAt;
      this.diverged = false;
    }

    output(step) {
      const index = this.steps;
      this.steps += 1n;
      if (this.divergeAt !== index) return step;
      this.diverged = true;
      switch (step.kind) {
        case "call":
          return { ...step, input: rt.object([["diverged", true]]) };
        case "timeout":
          return { ...step, delayMs: step.delayMs + 1n };
        case "clock":
          return { kind: "monotonic" };
        case "monotonic":
        case "random":
          return { kind: "clock" };
        case "bytes":
          return { kind: "bytes", length: step.length + 1n };
        default:
          return step;
      }
    }

    issue(name, input) {
      this.nextCall += 1;
      const id = this.nextCall;
      return [id, this.output({ kind: "call", id, name, input })];
    }

    next() {
      while (this.at < this.ops.length) {
        const op = this.ops[this.at];
        this.at += 1;
        switch (op.op) {
          case "calls": {
            const left = op.calls.slice();
            const [name, input] = left.shift();
            const [id, step] = this.issue(name, input);
            this.state = { kind: "issuing", left, awaiting: new Set([id]) };
            return step;
          }
          case "sleep": {
            this.nextTimer += 1;
            const id = this.nextTimer;
            this.state = { kind: "arming", id };
            return this.output({ kind: "timeout", id, delayMs: op.ms });
          }
          case "clock":
          case "monotonic":
          case "random":
            this.state = { kind: "value" };
            return this.output({ kind: op.op });
          case "bytes":
            this.state = { kind: "value" };
            return this.output({ kind: "bytes", length: op.length });
          case "print":
            this.printed.push(op.text);
            continue;
          case "throw":
            this.state = { kind: "ended" };
            return { kind: "threw", error: op.text };
          case "return":
            this.printed.push(op.text);
            this.state = { kind: "ended" };
            return { kind: "done", output: this.printed.join("\n") };
          default:
            throw new Error(`unknown scripted op ${op.op}`);
        }
      }
      this.state = { kind: "ended" };
      return { kind: "done", output: this.printed.join("\n") };
    }

    resume(input) {
      const state = this.state;
      this.state = { kind: "ended" };
      if (state.kind === "issuing" && input.kind === "issued") {
        if (state.left.length > 0) {
          const [name, callInput] = state.left.shift();
          const [id, step] = this.issue(name, callInput);
          state.awaiting.add(id);
          this.state = state;
          return step;
        }
        this.state = { kind: "calls", awaiting: state.awaiting };
        return { kind: "idle" };
      }
      if (state.kind === "calls" && input.kind === "result") {
        state.awaiting.delete(input.id);
        if (state.awaiting.size === 0) return this.next();
        this.state = state;
        return { kind: "idle" };
      }
      if (state.kind === "arming" && input.kind === "issued") {
        this.state = { kind: "timer", id: state.id };
        return { kind: "idle" };
      }
      if (state.kind === "arming" && input.kind === "refused") return { kind: "threw", error: input.error };
      if (state.kind === "timer" && input.kind === "timer" && input.id === state.id) return this.next();
      if (state.kind === "value" && ["clock", "monotonic", "random", "bytes"].includes(input.kind)) return this.next();
      return { kind: "threw", error: `the scripted engine got ${input.kind} while in ${state.kind}` };
    }
  }

  return { parse, Machine };
}

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
    for (const line of lines) {
      const record = rt.decodeLine(line, false);
      if (record.kind === "cell") {
        const cell = this.cells.get(record.cell) ?? { header: false, finished: false };
        cell.header ||= record.fresh;
        cell.finished ||= record.update.finished;
        this.cells.set(record.cell, cell);
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
      const event = cursor.next("cell_start", `cell ${cell}`);
      same("cell start cell", event.cell, BigInt(cell));
      same("cell start script", event.script, script);
      this.seed.stats.cells += 1;
      return stepFromJson(rt, event.step);
    }, null);
  }

  resume(cell, input) {
    const { rt, cursor, same } = this.check;
    const json = inputJson(rt, input);
    return this.exchange(cell, "resume", json, () => {
      const event = cursor.next("cell_resume", `cell ${cell} ${rt.encodeJson(json)}`);
      same("cell resume cell", event.cell, BigInt(cell));
      same("cell resume input", event.input, json);
      return stepFromJson(rt, event.step);
    }, input);
  }

  observe(event) {
    const { rt, cursor, same } = this.check;
    const json = eventJson(rt, event);
    this.exchange(event.cell, "observe", json, () => {
      const traced = cursor.next("cell", rt.encodeJson(json));
      same("cell event", traced.observed, json);
      if (event.kind === "diverged") this.seed.stats.divergences += 1;
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
      if (next === undefined || next.kind !== kind) throw new Error(`cell ${key}: the script engine read ${kind} where the scripted engine read ${next?.kind ?? "nothing"}`);
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
