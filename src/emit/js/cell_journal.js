const CELL_JOURNAL_VERSION = 1n;
const JOURNAL_LIMIT_BYTES = 32n * 1024n * 1024n;
const ENTRY_BYTES = 32n;
const CALL_INTERRUPTED = "CODE_MODE_CALL_INTERRUPTED";
const CALL_LIMIT = "call_limit";
const UNKNOWN_OUTCOME = "This nested call was running when its cell moved to another host; its outcome is unknown. Do not assume it did not run or automatically repeat it.";
const UNKNOWN_KEY = "unknown";
const BYTE_MAX = 255n;
const INPUT_ENTRY_KINDS = ["result", "timer"];

class CellFields {
  constructor(what, map) {
    this.what = what;
    this.map = map;
  }

  get(key) {
    if (!Object.hasOwn(this.map, key)) throw new DecodeError(`${this.what} lacks '${key}'`);
    return this.map[key];
  }

  number(key) {
    return smallNumber(this.get(key), `${this.what}'s '${key}'`);
  }

  big(key) {
    const value = this.get(key);
    if (typeof value !== "bigint") throw new DecodeError(`${this.what}'s '${key}' is not a number`);
    return value;
  }

  text(key) {
    const value = this.get(key);
    if (typeof value !== "string") throw new DecodeError(`${this.what}'s '${key}' is not text`);
    return value;
  }

  boolean(key) {
    const value = this.get(key);
    if (typeof value !== "boolean") throw new DecodeError(`${this.what}'s '${key}' is not a bool`);
    return value;
  }

  noHost() {
    if (this.boolean("host")) throw new DecodeError(`${this.what} runs on a host, which harnesslang never routes to`);
  }

  list(key, item) {
    const value = this.get(key);
    if (!Array.isArray(value)) throw new DecodeError(`${this.what}'s '${key}' is not a list`);
    return value.map(item);
  }

  exact(keys) {
    const present = Object.keys(this.map);
    if (present.length !== keys.length || keys.some((key) => !Object.hasOwn(this.map, key))) {
      throw new DecodeError(`${this.what} has fields ${JSON.stringify(present.sort())}, expected ${JSON.stringify(keys)}`);
    }
  }
}

function cellMap(json, what) {
  if (!isObject(json) || json instanceof Fraction) throw new DecodeError(`${what} is an object`);
  return json;
}

function entryJson(entry) {
  switch (entry.kind) {
    case "call":
      return object([["host", false], ["id", BigInt(entry.id)], ["input", entry.input], ["kind", "call"], ["name", entry.name]]);
    case "result":
      return object([["encoded", entry.encoded], ["id", BigInt(entry.id)], ["kind", "result"], ["success", entry.success]]);
    case "timeout":
      return object([["at_ms", entry.atMs], ["delay_ms", entry.delayMs], ["id", BigInt(entry.id)], ["kind", "timeout"]]);
    case "timer":
      return object([["id", BigInt(entry.id)], ["kind", "timer"]]);
    case "clock":
    case "monotonic":
      return object([["kind", entry.kind], ["value", entry.value]]);
    case "random":
      return object([["kind", "random"], ["value", new Fraction(entry.value)]]);
    case "bytes":
      return object([["data", entry.data.map((byte) => BigInt(byte))], ["kind", "bytes"]]);
    default:
      throw new Panic(`unknown cell entry kind ${entry.kind}`);
  }
}

function entryFrom(json) {
  const fields = new CellFields("entry", cellMap(json, "a cell journal entry"));
  const kind = fields.text("kind");
  switch (kind) {
    case "call":
      fields.exact(["host", "id", "input", "kind", "name"]);
      fields.noHost();
      return { kind, id: fields.number("id"), name: fields.text("name"), input: fields.get("input") };
    case "result":
      fields.exact(["encoded", "id", "kind", "success"]);
      return { kind, id: fields.number("id"), encoded: fields.text("encoded"), success: fields.boolean("success") };
    case "timeout":
      fields.exact(["at_ms", "delay_ms", "id", "kind"]);
      return { kind, id: fields.number("id"), delayMs: fields.big("delay_ms"), atMs: fields.big("at_ms") };
    case "timer":
      fields.exact(["id", "kind"]);
      return { kind, id: fields.number("id") };
    default:
      return scalarEntryFrom(fields, kind);
  }
}

function scalarEntryFrom(fields, kind) {
  fields.exact(["kind", kind === "bytes" ? "data" : "value"]);
  switch (kind) {
    case "clock":
    case "monotonic":
      return { kind, value: fields.big("value") };
    case "random": {
      const value = fields.get("value");
      if (!(value instanceof Fraction)) throw new DecodeError(`random entry value ${encodeJson(value)} is not a fraction`);
      return { kind, value: value.numerator };
    }
    case "bytes":
      return {
        kind,
        data: fields.list("data", (item) => {
          if (typeof item !== "bigint" || item > BYTE_MAX) throw new DecodeError("bytes entry holds a value that is not a byte");
          return Number(item);
        }),
      };
    default:
      throw new DecodeError(`unknown cell journal entry kind '${kind}'`);
  }
}

function entryIsInput(entry) {
  return INPUT_ENTRY_KINDS.includes(entry.kind);
}

function sameStep(left, right) {
  if (left.kind !== right.kind) return false;
  switch (left.kind) {
    case "timeout":
      return left.id === right.id && left.delayMs === right.delayMs;
    case "clock":
    case "monotonic":
    case "random":
      return true;
    case "bytes":
      return left.data.length === right.data.length;
    default:
      return jsonEqual(entryJson(left), entryJson(right));
  }
}

function describeEntry(entry) {
  switch (entry.kind) {
    case "call":
      return `call ${entry.name} (nested call ${entry.id})`;
    case "result":
      return `the result of nested call ${entry.id}`;
    case "timeout":
      return `timer ${entry.id} set for ${entry.delayMs} ms`;
    case "timer":
      return `timer ${entry.id} firing`;
    case "clock":
      return "a clock read";
    case "monotonic":
      return "a performance.now() read";
    case "random":
      return "a random number";
    case "bytes":
      return `${entry.data.length} random bytes`;
    default:
      throw new Panic(`unknown cell entry kind ${entry.kind}`);
  }
}

function entryBytes(entry) {
  return BigInt(textEncoder.encode(encodeJson(entryJson(entry))).length) + ENTRY_BYTES;
}

function journaledCallJson(call) {
  let state;
  if (call.state.kind === "completed") {
    state = object([["encoded", call.state.encoded], ["state", "completed"], ["success", call.state.success]]);
  } else {
    state = object([["state", call.state.kind]]);
  }
  return object([
    ["call_id", call.callId],
    ["host", false],
    ["id", BigInt(call.id)],
    ["input", call.input],
    ["name", call.name],
    ["state", state],
  ]);
}

function journaledCallFrom(json) {
  const fields = new CellFields("call", cellMap(json, "a journaled call"));
  fields.exact(["call_id", "host", "id", "input", "name", "state"]);
  fields.noHost();
  const stateFields = new CellFields("call state", cellMap(fields.get("state"), "a journaled call's state"));
  const kind = stateFields.text("state");
  let state;
  if (kind === "queued" || kind === "running") {
    stateFields.exact(["state"]);
    state = { kind };
  } else if (kind === "completed") {
    stateFields.exact(["encoded", "state", "success"]);
    state = { kind, encoded: stateFields.text("encoded"), success: stateFields.boolean("success") };
  } else {
    throw new DecodeError(`unknown call state '${kind}'`);
  }
  return { id: fields.number("id"), callId: fields.text("call_id"), name: fields.text("name"), input: fields.get("input"), state };
}

function headerJson(header) {
  return object([
    ["max_output_tokens", null],
    ["origin_call_id", header.originCallId],
    ["prelude", false],
    ["script", header.script],
    ["stored", object([])],
    ["tools", header.tools],
    ["version", CELL_JOURNAL_VERSION],
    ["yield_time_ms", header.yieldTimeMs],
  ]);
}

function headerFrom(json) {
  const fields = new CellFields("header", cellMap(json, "a cell header"));
  fields.exact(["max_output_tokens", "origin_call_id", "prelude", "script", "stored", "tools", "version", "yield_time_ms"]);
  if (fields.big("version") !== CELL_JOURNAL_VERSION) {
    throw new DecodeError(`cell header version ${fields.big("version")} is not ${CELL_JOURNAL_VERSION}`);
  }
  if (fields.get("max_output_tokens") !== null || fields.get("prelude") !== false || !jsonEqual(fields.get("stored"), object([]))) {
    throw new DecodeError("cell header carries stored values, a prelude or max_output_tokens");
  }
  const tools = fields.get("tools");
  if (!Array.isArray(tools)) throw new DecodeError("cell header tools is not a list");
  return { originCallId: fields.text("origin_call_id"), script: fields.text("script"), tools, yieldTimeMs: fields.big("yield_time_ms") };
}

function cellUpdateJson(update) {
  const entries = [
    ["session_id", update.sessionId],
    ["cell_id", BigInt(update.cellId)],
    ["seq", BigInt(update.seq)],
    ["entries_from", BigInt(update.entriesFrom)],
    ["entries", update.entries.map(entryJson)],
    ["calls", update.calls.map(journaledCallJson)],
    ["content_returned", 0n],
  ];
  if (update.header !== null) entries.push(["header", headerJson(update.header)]);
  if (update.finished) entries.push(["finished", true]);
  return object(entries);
}

function cellUpdateFrom(json) {
  const map = cellMap(json, "a cell update");
  const fields = new CellFields("cell update", map);
  const keys = ["calls", "cell_id", "content_returned", "entries", "entries_from", "seq", "session_id"];
  let header = null;
  if (Object.hasOwn(map, "header")) {
    keys.push("header");
    header = headerFrom(map.header);
  }
  let finished = false;
  if (Object.hasOwn(map, "finished")) {
    if (map.finished !== true) throw new DecodeError("cell update's finished is written only when true");
    keys.push("finished");
    finished = true;
  }
  keys.sort();
  fields.exact(keys);
  if (fields.big("content_returned") !== 0n) throw new DecodeError("cell update returns content, which harnesslang never writes");
  return {
    sessionId: fields.text("session_id"),
    cellId: fields.number("cell_id"),
    seq: fields.number("seq"),
    header,
    entriesFrom: fields.number("entries_from"),
    entries: fields.list("entries", entryFrom),
    calls: fields.list("calls", journaledCallFrom),
    finished,
  };
}

function cellLogStart(update) {
  if (update.header === null) throw new Corrupt(`cell ${update.cellId} starts without a header`);
  if (update.seq !== 0 || update.entriesFrom !== 0) {
    throw new Corrupt(`cell ${update.cellId}'s header update has seq ${update.seq} and entries_from ${update.entriesFrom}`);
  }
  const log = { cell: update.cellId, header: update.header, entries: [], calls: new Map(), finished: false, nextSeq: 0 };
  cellLogApply(log, update);
  return log;
}

function cellLogApply(log, update) {
  const cell = log.cell;
  if (update.cellId !== cell) throw new Corrupt(`cell ${cell} folds an update of cell ${update.cellId}`);
  if (log.finished) throw new Corrupt(`cell ${cell} has an update after it finished`);
  if (update.seq !== log.nextSeq) throw new Corrupt(`cell ${cell} update seq ${update.seq} follows seq ${log.nextSeq - 1}`);
  if (update.entriesFrom !== log.entries.length) {
    throw new Corrupt(`cell ${cell} update starts at entry ${update.entriesFrom} after ${log.entries.length} entries`);
  }
  if ((update.header !== null) !== (update.seq === 0)) {
    throw new Corrupt(`cell ${cell} carries its header on an update other than its first`);
  }
  log.nextSeq += 1;
  log.entries.push(...update.entries);
  for (const call of update.calls) log.calls.set(call.id, call);
  log.finished = update.finished;
}

function cellLogStopped(log, session) {
  assert(!log.finished, "only a running cell is stopped");
  return { sessionId: session, cellId: log.cell, seq: log.nextSeq, header: null, entriesFrom: log.entries.length, entries: [], calls: [], finished: true };
}

function cellLogBytes(log) {
  return log.entries.reduce((sum, entry) => sum + entryBytes(entry), 0n);
}

function cellLogClone(log) {
  return { ...log, entries: log.entries.slice(), calls: new Map(log.calls) };
}

function nestedKey(seq, nested) {
  return `${seq}/${nested}`;
}

function nestedCallFold(log, record) {
  const site = log.sites.get(record.seq);
  const cell = log.cells.get(record.seq);
  const lastSeq = Math.max(...log.sites.keys());
  const continued = cell !== undefined && !cell.finished && log.sites.get(lastSeq).kind === "open";
  if (!continued && (site === undefined || site.kind !== "open")) {
    throw new Corrupt(`nested call ${record.seq}/${record.nested} of turn ${record.turn} comes from a site that is not running`);
  }
  const last = log.lastNested.get(record.seq);
  if (last !== undefined && last >= record.nested) {
    throw new Corrupt(`turn ${record.turn} records nested call ${record.seq}/${record.nested} after ${record.seq}/${last}`);
  }
  log.lastNested.set(record.seq, record.nested);
  log.nested.set(nestedKey(record.seq, record.nested), { kind: "open", call: { tool: record.tool, effect: record.effect, input: record.input } });
}

function nestedResultFold(log, record) {
  const key = nestedKey(record.seq, record.nested);
  const site = log.nested.get(key);
  if (site === undefined) throw new Corrupt(`result for nested call ${key} of turn ${record.turn}, which has no call`);
  if (site.kind !== "open") throw new Corrupt(`result for nested call ${key} of turn ${record.turn}, which is not open`);
  log.nested.set(key, { kind: "done", call: site.call, output: record.output });
}

function nestedRecoverFold(log, turn, seq, nested) {
  const key = nestedKey(seq, nested);
  const site = log.nested.get(key);
  if (site === undefined) throw new Corrupt(`recovery of nested call ${key} of turn ${turn}, which has no call`);
  if (site.kind !== "open") throw new Corrupt(`recovery of nested call ${key} of turn ${turn}, which is not open`);
  if (!mayBeUnknown(site.call.effect)) throw new Corrupt(`recovery reports ${site.call.effect} nested call ${key} of turn ${turn} unknown`);
  log.nested.set(key, { kind: "unknown", call: site.call });
}

function nextCell(session) {
  let last = 0;
  for (const cell of session.cellOrigins.keys()) last = Math.max(last, cell);
  return last + 1;
}

function cellFold(session, record) {
  const { cell, update } = record;
  if (!record.fresh) {
    const origin = session.cellOrigins.get(cell);
    if (origin === undefined) throw new Corrupt(`cell ${cell} has an update before its header`);
    const log = runningLog(session, origin[0], "a cell update");
    const folded = log.cells.get(origin[1]);
    if (folded === undefined) throw new Corrupt(`cell ${cell} has lost its origin`);
    cellLogApply(folded, update);
    return;
  }
  if (cell < nextCell(session)) throw new Corrupt(`cell ${cell} starts after cell ${nextCell(session) - 1}`);
  if (update.header === null) throw new Corrupt(`fresh cell ${cell} lacks its header`);
  const [turn, seq, nested] = decodeOrCorrupt(() => parseAnyCallId(update.header.originCallId));
  if (nested !== null) throw new Corrupt(`cell ${cell} starts from a nested call`);
  const log = runningLog(session, turn, "a cell");
  const site = log.sites.get(seq);
  if (site === undefined || site.kind !== "open") throw new Corrupt(`cell ${cell} starts from ${turn}/${seq}, which is not a running site`);
  if (log.cells.has(seq)) throw new Corrupt(`cell ${cell} starts from ${turn}/${seq}, which already has a cell`);
  log.cells.set(seq, cellLogStart(update));
  session.cellOrigins.set(cell, [turn, seq]);
}

function unknownOutcome() {
  return object([["code", CALL_INTERRUPTED], ["error", UNKNOWN_OUTCOME], ["outcome", "unknown"]]);
}

function divergenceText(cell, reason) {
  return `This cell moved to another host and could not be resumed (${reason}), so it stopped and its outcome is unknown: nested calls it made may have run. Cell ${cell} no longer exists: don't wait on it again.`;
}

