const SIDE_TURN = 0;
const CRC_POLYNOMIAL = 0xedb88320;
const CRC_CHECK_INPUT = "123456789";
const CRC_CHECK = 0xcbf43926;
const JSON_DEPTH_MAX = 64;
const RECOVERED_ANSWERS = ["not_dispatched", "unknown"];
const RECOVERED_REPLY = "reply";
const INPUT_ENTRY_KINDS = ["result", "timer"];
const DELIVERY_FIELDS = { cancel: ["id"], steer: ["id", "message"], job: ["done", "id", "key", "notice"] };
const textEncoder = new TextEncoder();

class DecodeError extends Error {}

class Fraction {
  constructor(text) {
    if (typeof text !== "string" || !/^0\.[0-9]+$/.test(text)) throw new DecodeError("a fraction is written 0.digits, found " + text);
    this.text = text;
  }
}

function assert(condition, message) {
  if (!condition) throw new Error("assertion failed: " + message);
}

function revive(key, value, context) {
  if (typeof value !== "number") return value;
  const source = context.source;
  if (/^[0-9]+$/.test(source)) return BigInt(source);
  if (/^0\.[0-9]+$/.test(source)) return new Fraction(source);
  throw new DecodeError("only unsigned integers and fractions below one are numbers here, found " + source);
}

function decodeJson(text) {
  try {
    return JSON.parse(text, revive);
  } catch (error) {
    if (error instanceof SyntaxError) throw new DecodeError(error.message);
    throw error;
  }
}

function compareKeys(left, right) {
  const a = textEncoder.encode(left);
  const b = textEncoder.encode(right);
  const shared = Math.min(a.length, b.length);
  for (let index = 0; index < shared; index++) {
    if (a[index] !== b[index]) return a[index] - b[index];
  }
  return a.length - b.length;
}

function quote(text) {
  let out = "\"";
  for (let index = 0; index < text.length; index++) {
    const character = text[index];
    const code = text.charCodeAt(index);
    if (character === "\"") out += "\\\"";
    else if (character === "\\") out += "\\\\";
    else if (character === "\n") out += "\\n";
    else if (character === "\r") out += "\\r";
    else if (character === "\t") out += "\\t";
    else if (code < 0x20) out += "\\u" + code.toString(16).padStart(4, "0");
    else out += character;
  }
  return out + "\"";
}

function writeJson(json, out, depth) {
  assert(depth <= JSON_DEPTH_MAX, "values we encode are nested at most json_depth_max deep");
  if (json === null) out.push("null");
  else if (json === true) out.push("true");
  else if (json === false) out.push("false");
  else if (typeof json === "bigint") out.push(json.toString());
  else if (typeof json === "number") {
    assert(Number.isSafeInteger(json) && json >= 0, "a plain number we encode is a small unsigned integer");
    out.push(String(json));
  } else if (json instanceof Fraction) out.push(json.text);
  else if (typeof json === "string") out.push(quote(json));
  else if (Array.isArray(json)) {
    out.push("[");
    json.forEach((item, index) => {
      if (index > 0) out.push(",");
      writeJson(item, out, depth + 1);
    });
    out.push("]");
  } else {
    assert(isObject(json), "a json value, found " + typeof json);
    const keys = Object.keys(json).sort(compareKeys);
    out.push("{");
    keys.forEach((key, index) => {
      if (index > 0) out.push(",");
      out.push(quote(key), ":");
      writeJson(json[key], out, depth + 1);
    });
    out.push("}");
  }
}

function encodeJson(json) {
  const out = [];
  writeJson(json, out, 0);
  return out.join("");
}

function isObject(json) {
  return json !== null && typeof json === "object" && !Array.isArray(json) && !(json instanceof Fraction);
}

function object(entries) {
  const result = Object.create(null);
  for (const [key, value] of entries) {
    assert(!Object.hasOwn(result, key), "an object's keys are distinct");
    result[key] = value;
  }
  return result;
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let index = 0; index < 256; index++) {
    let value = index;
    for (let bit = 0; bit < 8; bit++) value = value & 1 ? (value >>> 1) ^ CRC_POLYNOMIAL : value >>> 1;
    table[index] = value >>> 0;
  }
  return table;
})();

function crc32(text) {
  let crc = 0xffffffff;
  for (const byte of textEncoder.encode(text)) crc = (crc >>> 8) ^ CRC_TABLE[(crc ^ byte) & 0xff];
  return (crc ^ 0xffffffff) >>> 0;
}

assert(crc32(CRC_CHECK_INPUT) === CRC_CHECK, "CRC-32 matches its catalogued check value");

function tagged(json) {
  if (!isObject(json)) return null;
  const keys = Object.keys(json);
  return keys.length === 1 ? [keys[0], json[keys[0]]] : null;
}

function small(value, what) {
  const number = typeof value === "string" && /^[0-9]+$/.test(value) ? BigInt(value) : value;
  if (typeof number !== "bigint" || number > BigInt(Number.MAX_SAFE_INTEGER)) throw new DecodeError(what + " is not a number");
  return Number(number);
}

function callIdParts(text) {
  const match = /^([0-9]+)\/([0-9]+)(?:\/code-([0-9]+))?$/.exec(text);
  if (match === null) throw new DecodeError("'" + text + "' is not a call id");
  return [Number(match[1]), Number(match[2]), match[3] === undefined ? null : Number(match[3])];
}

function outputFrom(json) {
  const pair = tagged(json);
  if (pair !== null && pair[0] === "ok") return { tag: "ok", value: pair[1] };
  if (pair !== null && pair[0] === "failed") return { tag: "failed", error: pair[1] };
  throw new DecodeError("an output is ok or failed, found " + encodeJson(json));
}

function entryFrom(json) {
  if (!isObject(json) || typeof json.kind !== "string") throw new DecodeError("a cell journal entry names its kind");
  const entry = { kind: json.kind };
  if (json.id !== undefined) entry.id = small(json.id, "an entry's id");
  if (json.name !== undefined) entry.name = json.name;
  if (json.input !== undefined) entry.input = json.input;
  if (json.encoded !== undefined) entry.encoded = json.encoded;
  if (json.success !== undefined) entry.success = json.success;
  if (json.delay_ms !== undefined) entry.delayMs = json.delay_ms;
  if (json.at_ms !== undefined) entry.atMs = json.at_ms;
  if (json.data !== undefined) entry.data = json.data.map((byte) => small(byte, "a byte"));
  if (json.value !== undefined) entry.value = json.value;
  return entry;
}

function cellFrom(body) {
  const update = body.update;
  if (!isObject(update) || !Array.isArray(update.entries)) throw new DecodeError("a cell record carries an update with entries");
  const header = update.header === undefined ? null : { originCallId: update.header.origin_call_id, script: update.header.script };
  if ((header !== null) !== body.fresh) throw new DecodeError("a cell record is fresh exactly when its update carries the header");
  return { kind: "cell", cell: small(body.cell, "a cell record's cell"), fresh: body.fresh, session: body.session, update: { header, finished: update.finished === true, entries: update.entries.map(entryFrom) } };
}

function recoveryFrom(body) {
  const turn = small(body.turn, "a recovery's turn");
  const unknown = [];
  const nested = [];
  for (const item of body.unknown) {
    const [callTurn, seq, inner] = callIdParts(item);
    if (callTurn !== turn) throw new DecodeError("a recovery lists a call of another turn");
    if (inner === null) unknown.push(seq);
    else nested.push([seq, inner]);
  }
  return { kind: "recovery", turn, attempt: small(body.attempt, "a recovery's attempt"), unknown, nested };
}

function recordFrom(json) {
  const pair = tagged(json);
  if (pair === null || !isObject(pair[1])) throw new DecodeError("a record is an object with one tag");
  const [tag, body] = pair;
  switch (tag) {
    case "admitted":
      return { kind: "admitted", turn: small(body.turn, "an admission's turn"), handler: body.digest, arguments: body.prompt };
    case "call": {
      const [turn, seq, nested] = callIdParts(body.call_id);
      const call = { turn, seq, tool: body.tool, effect: body.effect, input: body.input };
      return nested === null ? { kind: "call", ...call } : { kind: "nested_call", ...call, nested };
    }
    case "result": {
      const [turn, seq, nested] = callIdParts(body.call_id);
      const output = outputFrom(body.output);
      return nested === null ? { kind: "result", turn, seq, output } : { kind: "nested_result", turn, seq, nested, output };
    }
    case "status": {
      const finished = tagged(body.status);
      if (finished === null || finished[0] !== "finished") throw new DecodeError("a status record is finished");
      return { kind: "status", turn: small(body.turn, "a status's turn"), last: small(finished[1].last, "a status's last"), outcome: finished[1].outcome, result: finished[1].result };
    }
    case "cell":
      return cellFrom(body);
    case "recovery":
      return recoveryFrom(body);
    default:
      return { kind: tag, body };
  }
}

function decodeLine(line) {
  return recordFrom(decodeJson(line));
}

function deliveryFrom(json) {
  const pair = tagged(json);
  const fields = pair === null ? undefined : DELIVERY_FIELDS[pair[0]];
  if (fields === undefined || !isObject(pair[1]) || encodeJson(Object.keys(pair[1]).sort()) !== encodeJson(fields)) {
    throw new DecodeError("event " + encodeJson(json) + " is not a cancel, steer or job");
  }
  return { kind: pair[0], id: pair[1].id, json };
}

function recoveredFrom(json) {
  if (typeof json === "string" && RECOVERED_ANSWERS.includes(json)) return { kind: json };
  const pair = tagged(json);
  if (pair !== null && pair[0] === RECOVERED_REPLY) return { kind: "reply", output: outputFrom(pair[1]) };
  throw new DecodeError("a recovery answer is not_dispatched, unknown or a reply, found " + encodeJson(json));
}

function entryIsInput(entry) {
  return INPUT_ENTRY_KINDS.includes(entry.kind);
}

function sameStep(left, right) {
  if (left.kind !== right.kind) return false;
  switch (left.kind) {
    case "call":
      return left.id === right.id && left.name === right.name && encodeJson(left.input) === encodeJson(right.input);
    case "timeout":
      return left.id === right.id && left.delayMs === right.delayMs;
    case "bytes":
      return left.data.length === right.data.length;
    default:
      return true;
  }
}

function cellOutcomeOutput(outcome) {
  if (outcome.kind === "done") return { tag: "ok", value: outcome.text };
  if (outcome.kind === "threw") return { tag: "failed", error: outcome.text };
  return { tag: "ok", value: object([[outcome.kind, outcome.text]]) };
}

export { DecodeError, Fraction, SIDE_TURN, cellOutcomeOutput, crc32, decodeJson, decodeLine, deliveryFrom, encodeJson, entryIsInput, isObject, object, recoveredFrom, sameStep };
