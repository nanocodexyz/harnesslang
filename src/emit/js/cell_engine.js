const CELL_FRACTION_SCALE = 2 ** 32;
const CELL_JSON_NUMBER_MAX = Number.MAX_SAFE_INTEGER;
const CELL_SCRIPT_PARAMETERS = ["tools", "setTimeout", "Date", "performance", "Math", "crypto", "console"];

class CellRefusal extends Error {}

function cellErrorText(error) {
  if (error instanceof Error) return error.message;
  return String(error);
}

function cellInputJson(value, depth) {
  if (depth > JSON_DEPTH_MAX) throw new TypeError(`a nested call's input is nested deeper than json_depth_max (${JSON_DEPTH_MAX})`);
  if (value === null || typeof value === "boolean" || typeof value === "string") return value;
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || value < 0) throw new TypeError(`a nested call's input holds ${value}, which is not an unsigned integer`);
    return BigInt(value);
  }
  if (typeof value === "bigint") return value;
  if (Array.isArray(value)) return value.map((item) => cellInputJson(item, depth + 1));
  if (typeof value === "object") return object(Object.keys(value).map((key) => [key, cellInputJson(value[key], depth + 1)]));
  throw new TypeError(`a nested call's input holds a ${typeof value}`);
}

function cellOutputValue(json) {
  if (typeof json === "bigint") return json <= BigInt(CELL_JSON_NUMBER_MAX) ? Number(json) : json;
  if (json instanceof Fraction) return Number(json.numerator) / CELL_FRACTION_SCALE;
  if (Array.isArray(json)) return json.map(cellOutputValue);
  if (isObject(json)) return Object.fromEntries(Object.keys(json).map((key) => [key, cellOutputValue(json[key])]));
  return json;
}

class ScriptEngine {
  constructor(run, config) {
    this.run = run;
    this.limits = config.limits;
    this.replay = config.replay;
    this.platform = config.platform;
    this.macrotask = config.macrotask;
    this.queue = [];
    this.steps = 0;
    this.nextCall = 0;
    this.nextTimer = 0;
    this.armed = 0n;
    this.calls = new Map();
    this.timers = new Map();
    this.printed = [];
    this.ended = null;
    this.awaiting = null;
  }

  emit(step) {
    this.steps += 1;
    this.queue.push(step);
  }

  recorded(kind) {
    const entry = this.replay[this.steps];
    return entry !== undefined && entry.kind === kind ? entry : null;
  }

  read(kind, live) {
    const entry = this.recorded(kind);
    const value = entry === null ? live() : (kind === "bytes" ? entry.data : entry.value);
    return value;
  }

  scope() {
    const engine = this;
    const tools = new Proxy(Object.create(null), {
      get(_, name) {
        if (typeof name !== "string") return undefined;
        return (input) => engine.call(name, input);
      },
    });
    const clock = () => {
      const value = engine.read("clock", () => engine.platform.clock());
      engine.emit({ kind: "clock", value });
      return Number(value);
    };
    class CellDate extends Date {
      constructor(...args) {
        if (args.length === 0) super(clock());
        else super(...args);
      }

      static now() {
        return clock();
      }
    }
    const performance = Object.freeze({
      now() {
        const value = engine.read("monotonic", () => engine.platform.monotonic());
        engine.emit({ kind: "monotonic", value });
        return Number(value);
      },
    });
    const math = Object.create(Math);
    math.random = () => {
      const value = engine.read("random", () => engine.platform.random());
      engine.emit({ kind: "random", value });
      return Number(value) / CELL_FRACTION_SCALE;
    };
    const crypto = Object.freeze({
      getRandomValues(array) {
        if (!(array instanceof Uint8Array)) throw new TypeError("getRandomValues fills a Uint8Array here");
        const data = engine.read("bytes", () => engine.platform.bytes(array.length));
        if (data.length !== array.length) throw new TypeError("random bytes of another length");
        engine.emit({ kind: "bytes", length: BigInt(array.length), data: data.slice() });
        array.set(data);
        return array;
      },
    });
    const console = Object.freeze({ log: (...items) => engine.printed.push(items.map((item) => (typeof item === "string" ? item : String(item))).join(" ")) });
    const setTimeout = (callback, delay) => engine.arm(callback, delay);
    return [tools, setTimeout, CellDate, performance, Object.freeze(math), crypto, console];
  }

  call(name, input) {
    this.nextCall += 1;
    const id = this.nextCall;
    const json = cellInputJson(input === undefined ? null : input, 0);
    this.emit({ kind: "call", id, name, input: json });
    return new Promise((resolve, reject) => this.calls.set(id, { resolve, reject }));
  }

  arm(callback, delay) {
    if (typeof callback !== "function") throw new TypeError("setTimeout takes a function");
    const delayMs = BigInt(Math.max(0, Math.trunc(Number(delay ?? 0))));
    const limits = this.limits;
    if (!limits.unchecked && this.armed >= limits.timers.value) {
      throw new CellRefusal(`setTimeout: the cell armed its timers bound (${limits.timers.name} = ${limits.timers.value})`);
    }
    if (!limits.unchecked && delayMs > limits.timerMs.value) {
      throw new CellRefusal(`setTimeout: ${delayMs} ms is past timer_ms (${limits.timerMs.name} = ${limits.timerMs.value})`);
    }
    this.nextTimer += 1;
    const id = this.nextTimer;
    this.armed += 1n;
    this.timers.set(id, callback);
    this.emit({ kind: "timeout", id, delayMs });
    return id;
  }

  async start() {
    assert(this.ended === null && this.steps === 0, "a script engine starts once");
    const promise = this.run(...this.scope());
    promise.then(
      (value) => {
        if (typeof value === "string") this.printed.push(value);
        this.ended = { kind: "done", output: this.printed.join("\n") };
      },
      (error) => {
        this.ended = { kind: "threw", error: cellErrorText(error) };
      },
    );
    return this.next();
  }

  async next() {
    if (this.queue.length === 0) await this.macrotask();
    if (this.queue.length > 0) return this.queue.shift();
    if (this.ended !== null) return this.ended;
    return { kind: "idle" };
  }

  async resume(input) {
    switch (input.kind) {
      case "issued":
        break;
      case "result":
        this.deliver(input);
        break;
      case "timer": {
        const callback = this.timers.get(input.id);
        if (callback === undefined) return { kind: "threw", error: `the engine has no timer ${input.id}` };
        this.timers.delete(input.id);
        callback();
        break;
      }
      case "clock":
      case "monotonic":
      case "random":
      case "bytes":
        break;
      case "refused":
        return { kind: "threw", error: input.error };
      default:
        throw new Panic(`unknown engine input ${input.kind}`);
    }
    return this.next();
  }

  deliver(input) {
    const pending = this.calls.get(input.id);
    assert(pending !== undefined, "a result answers a nested call the script made");
    this.calls.delete(input.id);
    let json;
    try {
      json = decodeJson(input.encoded);
    } catch (error) {
      if (!(error instanceof DecodeError)) throw error;
      json = input.encoded;
    }
    if (input.success) {
      pending.resolve(cellOutputValue(json));
      return;
    }
    const message = isObject(json) && typeof json.error === "string" ? json.error : input.encoded;
    const error = new Error(message);
    error.outcome = cellOutputValue(json);
    pending.reject(error);
  }
}

function scriptEngineModule(script) {
  return `export default async function cell(${CELL_SCRIPT_PARAMETERS.join(", ")}) {\n${script}\n}\n`;
}

