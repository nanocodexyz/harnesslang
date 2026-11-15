const CRC_POLYNOMIAL = 0xedb88320;
const BYTE_VALUES = 256;
const BITS_PER_BYTE = 8;
const LINE_PREFIX = '{"crc":';
const LINE_MIDDLE = ',"record":';
const LINE_SUFFIX = "}";
const NESTED_ID_INFIX = "/code-";
const DECIMAL = /^\+?[0-9]+$/;

class DecodeError extends Error {}

const CRC_TABLE = (() => {
  const table = new Uint32Array(BYTE_VALUES);
  for (let byte = 0; byte < BYTE_VALUES; byte++) {
    let word = byte;
    for (let bit = 0; bit < BITS_PER_BYTE; bit++) word = word & 1 ? (word >>> 1) ^ CRC_POLYNOMIAL : word >>> 1;
    table[byte] = word >>> 0;
  }
  return table;
})();

const encoder = new TextEncoder();

function crc32(text) {
  let crc = 0xffffffff;
  for (const byte of encoder.encode(text)) crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> BITS_PER_BYTE);
  return (crc ^ 0xffffffff) >>> 0;
}

function decodeJson(text) {
  try {
    return JSON.parse(text, (key, value, context) => (typeof value === "number" && context !== undefined && /^-?[0-9]+$/.test(context.source) ? BigInt(context.source) : value));
  } catch (error) {
    if (error instanceof SyntaxError) throw new DecodeError(error.message);
    throw error;
  }
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function encodeJson(value) {
  if (typeof value === "bigint") return value.toString();
  if (Array.isArray(value)) return "[" + value.map(encodeJson).join(",") + "]";
  if (isObject(value)) return "{" + Object.keys(value).sort().map((key) => JSON.stringify(key) + ":" + encodeJson(value[key])).join(",") + "}";
  return JSON.stringify(value);
}

function decimal(text, what) {
  if (typeof text !== "string" || !DECIMAL.test(text) || BigInt(text) > BigInt(Number.MAX_SAFE_INTEGER)) throw new DecodeError(what);
  return Number(text);
}

function number(value, what) {
  if (typeof value !== "bigint" || value < 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) throw new DecodeError(what + " is not a number");
  return Number(value);
}

function callId(text) {
  if (typeof text !== "string") throw new DecodeError("a call id is not text");
  const infix = text.indexOf(NESTED_ID_INFIX);
  const outer = infix < 0 ? text : text.slice(0, infix);
  const slash = outer.indexOf("/");
  if (slash < 0) throw new DecodeError("call id " + JSON.stringify(text) + " is not turn/seq");
  const turn = decimal(outer.slice(0, slash), "call id " + JSON.stringify(text) + " has no turn number");
  const seq = decimal(outer.slice(slash + 1), "call id " + JSON.stringify(text) + " has no sequence number");
  const nested = infix < 0 ? null : decimal(text.slice(infix + NESTED_ID_INFIX.length), "call id " + JSON.stringify(text) + " has no nested number");
  return { turn, seq, nested };
}

function single(json, what) {
  if (!isObject(json)) throw new DecodeError(what + " is not an object with one tag");
  const keys = Object.keys(json);
  if (keys.length !== 1) throw new DecodeError(what + " is not an object with one tag");
  return [keys[0], json[keys[0]]];
}

function output(json) {
  const [tag, body] = single(json, "an output");
  if (tag === "ok") return { tag, value: body };
  if (tag === "failed" && typeof body === "string") return { tag, error: body };
  throw new DecodeError("output " + encodeJson(json) + " is neither ok nor failed");
}

function record(json) {
  const [kind, body] = single(json, "a record");
  if (!isObject(body)) throw new DecodeError("a " + kind + " record is not an object");
  switch (kind) {
    case "call": {
      const id = callId(body.call_id);
      if (decimal(body.turn, "a call record's turn") !== id.turn || number(body.seq, "a call record's seq") !== id.seq) throw new DecodeError("a call record disagrees with its call id");
      const call = { turn: id.turn, seq: id.seq, tool: body.tool, effect: body.effect, input: body.input };
      return id.nested === null ? { kind: "call", ...call } : { kind: "nested_call", ...call, nested: id.nested };
    }
    case "result": {
      const id = callId(body.call_id);
      if (number(body.seq, "a result record's seq") !== id.seq) throw new DecodeError("a result record disagrees with its call id");
      const result = { turn: id.turn, seq: id.seq, output: output(body.output) };
      return id.nested === null ? { kind: "result", ...result } : { kind: "nested_result", ...result, nested: id.nested };
    }
    case "model": {
      const id = callId(body.call_id);
      return { kind: "model", turn: id.turn, seq: id.seq };
    }
    case "recovery": {
      const turn = decimal(body.turn, "a recovery record's turn");
      if (!Array.isArray(body.unknown)) throw new DecodeError("a recovery record's unknown is not a list");
      const ids = body.unknown.map(callId);
      if (ids.some((id) => id.turn !== turn)) throw new DecodeError("a recovery lists a call of another turn");
      return { kind: "recovery", turn, attempt: number(body.attempt, "a recovery record's attempt"), unknown: ids.filter((id) => id.nested === null).map((id) => id.seq) };
    }
    case "status": {
      const [tag, finished] = single(body.status, "a status");
      if (tag !== "finished" || !isObject(finished)) throw new DecodeError("a status record is not finished");
      if (finished.outcome !== "completed" && finished.outcome !== "failed") throw new DecodeError("unknown outcome " + JSON.stringify(finished.outcome));
      return { kind: "status", turn: decimal(body.turn, "a status record's turn"), outcome: finished.outcome, result: finished.result };
    }
    case "checkpoint":
      return { kind: "checkpoint", revision: number(body.revision, "a checkpoint's revision") };
    case "admitted":
    case "event":
      return { kind, turn: decimal(body.turn, "an " + kind + " record's turn") };
    case "cell":
    case "host":
      return { kind };
    default:
      throw new DecodeError("unknown record " + JSON.stringify(kind));
  }
}

function decodeLine(line, checksum) {
  if (!checksum) return record(decodeJson(line));
  if (!line.startsWith(LINE_PREFIX) || !line.endsWith(LINE_SUFFIX)) throw new DecodeError("a line is not a checksummed envelope");
  const rest = line.slice(LINE_PREFIX.length, line.length - LINE_SUFFIX.length);
  const middle = rest.indexOf(LINE_MIDDLE);
  if (middle < 0) throw new DecodeError("a line has no record after its checksum");
  const body = rest.slice(middle + LINE_MIDDLE.length);
  if (decimal(rest.slice(0, middle), "a line's checksum is not a number") !== crc32(body)) throw new DecodeError("a line's checksum does not match its record");
  return record(decodeJson(body));
}

function fold(records) {
  const turns = new Map();
  for (const each of records) {
    if (each.kind === "admitted") {
      if (turns.has(each.turn)) throw new DecodeError("turn " + each.turn + " is admitted twice");
      turns.set(each.turn, { finished: null });
    }
    if (each.kind === "status") {
      const log = turns.get(each.turn) ?? { finished: null };
      turns.set(each.turn, log);
      if (log.finished !== null) throw new DecodeError("turn " + each.turn + " finishes twice");
      log.finished = { outcome: each.outcome, result: each.result };
    }
  }
  return { turns };
}

const journal = { DecodeError, crc32, decodeJson, encodeJson, isObject, decodeLine, fold };

export { journal };
