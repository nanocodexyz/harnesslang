const U64_MAX = (1n << 64n) - 1n;
const JSON_DEPTH_MAX = 128;
const FRACTION_BITS = 32n;
const FRACTION_DIGITS = 32;
const FRACTION_SCALE = 5n ** FRACTION_BITS;
const FRACTION_NUMERATOR_MAX = (1n << FRACTION_BITS) - 1n;
const FIRST_SEQ = 1;
const CRC32_POLYNOMIAL_REFLECTED = 0xedb88320;
const CRC32_CHECK_INPUT = "123456789";
const CRC32_CHECK = 0xcbf43926;
const BYTE_VALUES = 256;
const BITS_PER_BYTE = 8;
const LINE_END = "\n";
const NESTED_ID_INFIX = "/code-";
const LINE_PREFIX = "{\"crc\":";
const LINE_MIDDLE = ",\"record\":";
const LINE_SUFFIX = "}";
const SUM_CALL = 0;
const SUM_REPLY = 1;
const SUM_ENTRY = 2;
const CALL_FIELDS = ["id", "tool", "input"];
const CALL_ID = 0;
const CALL_TOOL = 1;
const CALL_INPUT = 2;
const VARIANT_OK = 0;
const VARIANT_FAILED = 1;
const VARIANT_UNKNOWN = 2;
const CONTEXT_TYPE = { list: { sum: SUM_ENTRY } };
const POINT_BEFORE_FLUSH = { point: "before_flush" };
const POINT_AFTER_FLUSH = { point: "after_flush" };
const POINT_BEFORE_ACKNOWLEDGE = { point: "before_acknowledge" };
const INTERRUPTED_REASON = "Tool execution interrupted; outcome unknown. Do not assume it did not run or automatically repeat it.";
const ATTEMPT_INTERRUPTED = "interrupted";
const JOB_KEY = "job";
const JOB_DONE = "done";
const JOB_KEY_FIELD = "key";
const JOB_NOTICE = "notice";
const LOST_PREFIX = "lost:";
const RECOVERY_REPLIED = "replied";
const RECOVERY_UNDISPATCHED = "not_dispatched";
const NO_INTERRUPTS = { cancel: false, steer: false };
const INTERRUPTED_BY = "interrupted_by";
const RECOVERED_REPLY = "reply";
const RECOVERED_ANSWERS = ["not_dispatched", "unknown"];

class Panic extends Error {}

class Trap {
  constructor(message) {
    this.message = message;
  }
}

class Corrupt extends Error {}

class DecodeError extends Error {}

class Fraction {
  constructor(numerator) {
    assert(typeof numerator === "bigint" && numerator >= 0n && numerator <= FRACTION_NUMERATOR_MAX, "a fraction's numerator fits in fraction_bits");
    this.numerator = numerator;
  }
}

function fractionText(numerator) {
  const digits = (numerator * FRACTION_SCALE).toString().padStart(FRACTION_DIGITS, "0");
  assert(digits.length === FRACTION_DIGITS, "a fraction below one has fraction_digits decimals");
  const trimmed = digits.replace(/0+$/, "");
  return "0." + (trimmed === "" ? "0" : trimmed);
}

function fractionFrom(digits) {
  if (digits.length === 0 || digits.length > FRACTION_DIGITS) return null;
  const scaled = BigInt(digits.padEnd(FRACTION_DIGITS, "0"));
  if (scaled % FRACTION_SCALE !== 0n) return null;
  const numerator = scaled / FRACTION_SCALE;
  return numerator <= FRACTION_NUMERATOR_MAX ? new Fraction(numerator) : null;
}

class StepEnd {
  constructor(records, position) {
    this.records = records;
    this.position = position;
  }
}

function assert(condition, message) {
  if (!condition) {
    throw new Panic(message);
  }
}

function object(entries) {
  const result = Object.create(null);
  for (const [key, value] of entries) {
    assert(!Object.hasOwn(result, key), "an object's keys are distinct");
    result[key] = value;
  }
  return result;
}

function isObject(json) {
  return json !== null && typeof json === "object" && !Array.isArray(json);
}

function tagged(json) {
  if (!isObject(json)) {
    return null;
  }
  const keys = Object.keys(json);
  if (keys.length !== 1) {
    return null;
  }
  return [keys[0], json[keys[0]]];
}

const MODEL_FAILED = "failed";

function modelOutcome(response) {
  const pair = tagged(response);
  if (pair !== null && pair[0] === MODEL_FAILED && typeof pair[1] === "string") {
    return variant(VARIANT_FAILED, [pair[1]]);
  }
  try {
    return variant(VARIANT_OK, [fromJson({ sum: SUM_REPLY }, response)]);
  } catch (error) {
    if (!(error instanceof DecodeError)) throw error;
    return variant(VARIANT_FAILED, [`model reply: ${error.message}`]);
  }
}

const ATTEMPT_OK = "ok";
const ATTEMPT_REJECTED = 3;
const ATTEMPT_USAGE = 2;
const USAGE_FIELDS = ["input", "output", "cache_read", "cache_write"];

function usageOver(response, window) {
  const pair = tagged(response);
  if (pair === null || pair[0] !== ATTEMPT_OK || !Array.isArray(pair[1])) return null;
  for (let index = 0; index < USAGE_FIELDS.length; index += 1) {
    const used = pair[1][ATTEMPT_USAGE + index];
    if (typeof used === "bigint" && used > window) {
      return `model reply: ${USAGE_FIELDS[index]} usage of ${used} tokens exceeds the window of ${window} tokens`;
    }
  }
  return null;
}

function attemptOutcome(response) {
  const window = PROGRAM.model.window;
  let attempt;
  try {
    attempt = fromJson({ sum: window.attempt }, response);
  } catch (error) {
    if (!(error instanceof DecodeError)) throw error;
    return variant(ATTEMPT_REJECTED, [`model reply: ${error.message}`]);
  }
  const over = usageOver(response, window.tokens);
  return over === null ? attempt : variant(ATTEMPT_REJECTED, [over]);
}

const UTF16_UNITS_PER_TOKEN = 4n;
const TRUNCATION_OPEN = "\u2026";
const TRUNCATION_CLOSE = " tokens truncated\u2026";

function truncateMiddle(text, tokensMax) {
  const total = (BigInt(text.length) + UTF16_UNITS_PER_TOKEN - 1n) / UTF16_UNITS_PER_TOKEN;
  if (total <= tokensMax) return text;
  const side = tokensMax * (UTF16_UNITS_PER_TOKEN / 2n);
  const characters = Array.from(text);
  let units = 0n;
  let head = text.length;
  let offset = 0;
  for (const character of characters) {
    units += BigInt(character.length);
    if (units > side) {
      head = offset;
      break;
    }
    offset += character.length;
  }
  units = 0n;
  let tail = 0;
  offset = text.length;
  for (let index = characters.length - 1; index >= 0; index -= 1) {
    const character = characters[index];
    units += BigInt(character.length);
    if (units > side) {
      tail = offset;
      break;
    }
    offset -= character.length;
  }
  if (tail < head) tail = head;
  return `${text.slice(0, head)}${TRUNCATION_OPEN}${total - tokensMax}${TRUNCATION_CLOSE}${text.slice(tail)}`;
}

const textEncoder = new TextEncoder();
const ASCII_ONLY = /^[\x00-\x7f]*$/;

function compareKeys(left, right) {
  if (ASCII_ONLY.test(left) && ASCII_ONLY.test(right)) {
    return left < right ? -1 : left > right ? 1 : 0;
  }
  const a = textEncoder.encode(left);
  const b = textEncoder.encode(right);
  const shared = Math.min(a.length, b.length);
  for (let index = 0; index < shared; index++) {
    if (a[index] !== b[index]) {
      return a[index] - b[index];
    }
  }
  return a.length - b.length;
}

const NEEDS_ESCAPE = /["\\\x00-\x1f]/;

function quote(text) {
  if (!NEEDS_ESCAPE.test(text)) {
    return "\"" + text + "\"";
  }
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

function encodeJson(json) {
  const out = [];
  writeJson(json, out, 0);
  return out.join("");
}

function writeJson(json, out, depth) {
  assert(depth <= JSON_DEPTH_MAX, "values we encode are nested at most json_depth_max deep");
  if (json === null) out.push("null");
  else if (json === true) out.push("true");
  else if (json === false) out.push("false");
  else if (typeof json === "bigint") {
    assert(json >= 0n && json <= U64_MAX, "a json number is a u64");
    out.push(json.toString());
  } else if (json instanceof Fraction) out.push(fractionText(json.numerator));
  else if (typeof json === "string") out.push(quote(json));
  else if (Array.isArray(json)) {
    out.push("[");
    for (let index = 0; index < json.length; index++) {
      if (index > 0) out.push(",");
      writeJson(json[index], out, depth + 1);
    }
    out.push("]");
  } else {
    assert(isObject(json), `a json value, found ${typeof json}`);
    const keys = Object.keys(json).sort(compareKeys);
    out.push("{");
    for (let index = 0; index < keys.length; index++) {
      if (index > 0) out.push(",");
      out.push(quote(keys[index]), ":");
      writeJson(json[keys[index]], out, depth + 1);
    }
    out.push("}");
  }
}

function jsonEqual(left, right) {
  if (left === right) return true;
  if (left instanceof Fraction || right instanceof Fraction) {
    return left instanceof Fraction && right instanceof Fraction && left.numerator === right.numerator;
  }
  if (Array.isArray(left)) {
    return Array.isArray(right) && left.length === right.length && left.every((item, index) => jsonEqual(item, right[index]));
  }
  if (!isObject(left) || !isObject(right)) return false;
  const keys = Object.keys(left);
  if (keys.length !== Object.keys(right).length) return false;
  return keys.every((key) => Object.hasOwn(right, key) && jsonEqual(left[key], right[key]));
}

class JsonParser {
  constructor(text) {
    this.text = text;
    this.offset = 0;
  }

  fail(what) {
    throw new DecodeError(`${what} at offset ${this.offset}`);
  }

  whitespace() {
    while (this.offset < this.text.length && " \t\n\r".includes(this.text[this.offset])) {
      this.offset += 1;
    }
  }

  literal(word, value) {
    if (this.text.startsWith(word, this.offset)) {
      this.offset += word.length;
      return value;
    }
    return this.fail("invalid literal");
  }

  value(depth) {
    if (depth > JSON_DEPTH_MAX) this.fail(`nesting deeper than json_depth_max (${JSON_DEPTH_MAX})`);
    const character = this.text[this.offset];
    if (character === undefined) return this.fail("unexpected end");
    if (character === "n") return this.literal("null", null);
    if (character === "t") return this.literal("true", true);
    if (character === "f") return this.literal("false", false);
    if (character === "\"") return this.string();
    if (character === "[") return this.array(depth);
    if (character === "{") return this.object(depth);
    if (character >= "0" && character <= "9") return this.number();
    return this.fail("unexpected byte");
  }

  number() {
    const start = this.offset;
    while (this.offset < this.text.length && this.text[this.offset] >= "0" && this.text[this.offset] <= "9") {
      this.offset += 1;
    }
    const digits = this.text.slice(start, this.offset);
    const value = BigInt(digits);
    if (value > U64_MAX) throw new DecodeError(`number does not fit in u64 at offset ${start}`);
    if (digits.length > 1 && digits[0] === "0") throw new DecodeError(`number has a leading zero at offset ${start}`);
    if (digits === "0" && this.text[this.offset] === ".") return this.fraction();
    if (".eE-".includes(this.text[this.offset] ?? "x")) this.fail("only unsigned integers are numbers here");
    return value;
  }

  fraction() {
    this.offset += 1;
    const start = this.offset;
    while (this.offset < this.text.length && this.text[this.offset] >= "0" && this.text[this.offset] <= "9") {
      this.offset += 1;
    }
    if ("eE".includes(this.text[this.offset] ?? "x")) this.fail("a fraction has no exponent here");
    const fraction = fractionFrom(this.text.slice(start, this.offset));
    if (fraction === null) this.fail(`a fraction here is a multiple of 2^-${FRACTION_BITS} below one`);
    return fraction;
  }

  hex4() {
    const digits = this.text.slice(this.offset, this.offset + 4);
    if (digits.length !== 4) throw new DecodeError("truncated \\u escape");
    if (!/^[0-9a-fA-F]{4}$/.test(digits)) this.fail("invalid \\u escape");
    this.offset += 4;
    return parseInt(digits, 16);
  }

  escape() {
    const character = this.text[this.offset];
    if (character === undefined) return this.fail("truncated escape");
    this.offset += 1;
    const simple = { "\"": "\"", "\\": "\\", "/": "/", n: "\n", r: "\r", t: "\t", b: "\b", f: "\f" };
    if (Object.hasOwn(simple, character)) return simple[character];
    if (character !== "u") return this.fail("unknown escape");
    const high = this.hex4();
    if (high >= 0xd800 && high < 0xdc00) {
      if (!this.text.startsWith("\\u", this.offset)) this.fail("unpaired surrogate");
      this.offset += 2;
      const low = this.hex4();
      if (low < 0xdc00 || low >= 0xe000) this.fail("invalid low surrogate");
      return String.fromCodePoint(0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00));
    }
    if (high >= 0xdc00 && high < 0xe000) this.fail("invalid code point");
    return String.fromCodePoint(high);
  }

  string() {
    this.offset += 1;
    let out = "";
    for (;;) {
      const start = this.offset;
      while (this.offset < this.text.length) {
        const code = this.text.charCodeAt(this.offset);
        if (code === 0x22 || code === 0x5c || code < 0x20) break;
        this.offset += 1;
      }
      out += this.text.slice(start, this.offset);
      const character = this.text[this.offset];
      if (character === "\"") {
        this.offset += 1;
        return out;
      }
      if (character === "\\") {
        this.offset += 1;
        out += this.escape();
      } else if (character === undefined) {
        return this.fail("unterminated string");
      } else {
        return this.fail("control character in string");
      }
    }
  }

  array(depth) {
    this.offset += 1;
    const items = [];
    this.whitespace();
    if (this.text[this.offset] === "]") {
      this.offset += 1;
      return items;
    }
    for (;;) {
      this.whitespace();
      items.push(this.value(depth + 1));
      this.whitespace();
      const character = this.text[this.offset];
      if (character === "]") {
        this.offset += 1;
        return items;
      }
      if (character !== ",") return this.fail("expected ',' or ']'");
      this.offset += 1;
    }
  }

  object(depth) {
    this.offset += 1;
    const map = Object.create(null);
    this.whitespace();
    if (this.text[this.offset] === "}") {
      this.offset += 1;
      return map;
    }
    for (;;) {
      this.whitespace();
      if (this.text[this.offset] !== "\"") this.fail("expected a key");
      const key = this.string();
      this.whitespace();
      if (this.text[this.offset] !== ":") this.fail("expected ':'");
      this.offset += 1;
      this.whitespace();
      const value = this.value(depth + 1);
      if (Object.hasOwn(map, key)) throw new DecodeError(`duplicate key ${JSON.stringify(key)}`);
      map[key] = value;
      this.whitespace();
      const character = this.text[this.offset];
      if (character === "}") {
        this.offset += 1;
        return map;
      }
      if (character !== ",") return this.fail("expected ',' or '}'");
      this.offset += 1;
    }
  }
}

function decodeJson(text) {
  const parser = new JsonParser(text);
  parser.whitespace();
  const value = parser.value(0);
  parser.whitespace();
  if (parser.offset !== text.length) {
    throw new DecodeError(`trailing bytes at offset ${parser.offset}`);
  }
  return value;
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(BYTE_VALUES);
  for (let index = 0; index < BYTE_VALUES; index++) {
    let value = index;
    for (let bit = 0; bit < BITS_PER_BYTE; bit++) {
      value = value & 1 ? (value >>> 1) ^ CRC32_POLYNOMIAL_REFLECTED : value >>> 1;
    }
    table[index] = value >>> 0;
  }
  return table;
})();

function crc32Bytes(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc = (crc >>> BITS_PER_BYTE) ^ CRC_TABLE[(crc ^ byte) & 0xff];
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function crc32(text) {
  return crc32Bytes(textEncoder.encode(text));
}

assert(crc32(CRC32_CHECK_INPUT) === CRC32_CHECK, "CRC-32 matches its catalogued check value");

function callId(turn, seq) {
  return `${turn}/${seq}`;
}

function nestedId(turn, seq, nested) {
  return `${callId(turn, seq)}${NESTED_ID_INFIX}${nested}`;
}

function recoveryIds(record) {
  const keyed = record.unknown.map((seq) => [seq, 0, callId(record.turn, seq)]);
  for (const [seq, nested] of record.nested) keyed.push([seq, nested, nestedId(record.turn, seq, nested)]);
  keyed.sort((left, right) => left[0] - right[0] || left[1] - right[1]);
  return keyed.map((item) => item[2]);
}

function mayBeUnknown(effect) {
  return effect === "external";
}

function outputJson(output) {
  return output.tag === "ok" ? object([["ok", output.value]]) : object([["failed", output.error]]);
}

function recordJson(record) {
  const turn = String(record.turn);
  switch (record.kind) {
    case "admitted":
      return object([["admitted", object([
        ["digest", record.handler],
        ["first", BigInt(FIRST_SEQ)],
        ["hand", null],
        ["metadata", object([["handler", record.handler]])],
        ["prompt", record.arguments],
        ["turn", turn],
      ])]]);
    case "call":
      return object([["call", object([
        ["call_id", callId(record.turn, record.seq)],
        ["effect", record.effect],
        ["input", record.input],
        ["seq", BigInt(record.seq)],
        ["tool", record.tool],
        ["turn", turn],
      ])]]);
    case "result":
      return object([["result", object([
        ["call_id", callId(record.turn, record.seq)],
        ["output", outputJson(record.output)],
        ["seq", BigInt(record.seq)],
      ])]]);
    case "model": {
      const entries = [
        ["call_id", callId(record.turn, record.seq)],
        ["response", record.response],
        ["seq", BigInt(record.seq)],
        ["turn", turn],
      ];
      if (record.mode !== null) entries.push([MODEL_MODE, record.mode]);
      return object([["model", object(entries)]]);
    }
    case "recovery": {
      const entries = [
        ["attempt", BigInt(record.attempt)],
        ["turn", turn],
        ["unknown", recoveryIds(record)],
      ];
      if (record.replied.length > 0) entries.push([RECOVERY_REPLIED, record.replied.map((seq) => callId(record.turn, seq))]);
      if (record.undispatched.length > 0) entries.push([RECOVERY_UNDISPATCHED, record.undispatched.map((seq) => callId(record.turn, seq))]);
      return object([["recovery", object(entries)]]);
    }
    case "nested_call":
      return object([["call", object([
        ["call_id", nestedId(record.turn, record.seq, record.nested)],
        ["effect", record.effect],
        ["input", record.input],
        ["seq", BigInt(record.seq)],
        ["tool", record.tool],
        ["turn", turn],
      ])]]);
    case "nested_result":
      return object([["result", object([
        ["call_id", nestedId(record.turn, record.seq, record.nested)],
        ["output", outputJson(record.output)],
        ["seq", BigInt(record.seq)],
      ])]]);
    case "cell":
      return object([["cell", object([
        ["cell", BigInt(record.cell)],
        ["fresh", record.fresh],
        ["session", record.session],
        ["update", cellUpdateJson(record.update)],
      ])]]);
    case "status":
      return object([["status", object([
        ["status", object([["finished", object([
          ["last", BigInt(record.last)],
          ["outcome", record.outcome],
          ["result", record.result],
        ])]])],
        ["turn", turn],
      ])]]);
    case "checkpoint":
      return object([["checkpoint", object([
        ["payload", record.payload],
        ["revision", BigInt(record.revision)],
        ["state", record.state],
      ])]]);
    case "event":
      return object([["event", object([
        ["at", BigInt(record.at)],
        ["event", deliveryJson(record.delivery)],
        ["seq", BigInt(record.seq)],
        ["turn", turn],
      ])]]);
    default:
      throw new Panic(`unknown record kind ${record.kind}`);
  }
}

function deliveryJson(delivery) {
  const fields = [["id", delivery.id]];
  if (delivery.kind === "steer") fields.push(["message", delivery.message]);
  if (delivery.kind === "job") fields.push([JOB_DONE, delivery.done], [JOB_KEY_FIELD, delivery.key], [JOB_NOTICE, delivery.notice]);
  return object([[delivery.kind, object(fields)]]);
}

function lostNotice(key) {
  return `<task-notification>\n<task-id>${key}</task-id>\n<status>killed</status>\n<summary>Background command ${key} was still running when the harness that started it ended, so its completion was never reported and never will be. It was most likely stopped then; its outcome is unknown.</summary>\n</task-notification>\nRead its output file and check its effects before you rely on it or run it again.`;
}

function lostDelivery(key) {
  return { id: LOST_PREFIX + key, kind: "job", key, notice: lostNotice(key), done: true };
}

function jobStarted(output) {
  if (output.tag !== "ok" || !isObject(output.value)) return null;
  const keys = Object.keys(output.value);
  if (keys.length !== 1 || keys[0] !== JOB_KEY || typeof output.value[JOB_KEY] !== "string") return null;
  return output.value[JOB_KEY];
}

function interruptedResponse() {
  return object([[ATTEMPT_INTERRUPTED, []]]);
}

function deliveryFrom(json) {
  const pair = tagged(json);
  if (pair === null || !isObject(pair[1])) throw new DecodeError(`event ${encodeJson(json)} is not a tagged object`);
  const fields = new Fields(pair[0], pair[1]);
  const id = fields.text("id");
  if (pair[0] === "cancel") {
    fields.exact(["id"]);
    return { id, kind: "cancel" };
  }
  if (pair[0] === "steer") {
    fields.exact(["id", "message"]);
    return { id, kind: "steer", message: fields.text("message") };
  }
  if (pair[0] === "job") {
    fields.exact([JOB_DONE, "id", JOB_KEY_FIELD, JOB_NOTICE]);
    return { id, kind: "job", key: fields.text(JOB_KEY_FIELD), notice: fields.text(JOB_NOTICE), done: fields.boolean(JOB_DONE) };
  }
  throw new DecodeError(`unknown event '${pair[0]}'`);
}

function smallNumber(value, what) {
  if (typeof value !== "bigint" || value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new DecodeError(`${what} is not a number`);
  }
  return Number(value);
}

function parseDecimal(text, what) {
  if (typeof text !== "string" || !/^\+?[0-9]+$/.test(text)) {
    throw new DecodeError(what);
  }
  const value = BigInt(text);
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new DecodeError(what);
  }
  return Number(value);
}

function parseAnyCallId(text) {
  const infix = text.indexOf(NESTED_ID_INFIX);
  if (infix < 0) return [...parseCallId(text), null];
  const [turn, seq] = parseCallId(text.slice(0, infix));
  const nested = parseDecimal(text.slice(infix + NESTED_ID_INFIX.length), `call id ${JSON.stringify(text)} has no nested call number`);
  return [turn, seq, nested];
}

function parseCallId(text) {
  const slash = text.indexOf("/");
  if (slash < 0) throw new DecodeError(`call id ${JSON.stringify(text)} is not turn/seq`);
  const turn = parseDecimal(text.slice(0, slash), `call id ${JSON.stringify(text)} has no turn number`);
  const seq = parseDecimal(text.slice(slash + 1), `call id ${JSON.stringify(text)} has no sequence number`);
  return [turn, seq];
}

class Fields {
  constructor(tag, map) {
    this.tag = tag;
    this.map = map;
  }

  get(key) {
    if (!Object.hasOwn(this.map, key)) throw new DecodeError(`${this.tag} record lacks '${key}'`);
    return this.map[key];
  }

  number(key) {
    return smallNumber(this.get(key), `${this.tag} record's '${key}'`);
  }

  text(key) {
    const value = this.get(key);
    if (typeof value !== "string") throw new DecodeError(`${this.tag} record's '${key}' is not text`);
    return value;
  }

  turn() {
    return parseDecimal(this.text("turn"), `${this.tag} record's turn is not a number`);
  }

  call() {
    const [turn, seq, nested] = parseAnyCallId(this.text("call_id"));
    if (this.number("seq") !== seq) throw new DecodeError(`${this.tag} record's seq disagrees with its call id`);
    return [turn, seq, nested];
  }

  boolean(key) {
    const value = this.get(key);
    if (typeof value !== "boolean") throw new DecodeError(`${this.tag} record's '${key}' is not a bool`);
    return value;
  }

  exact(keys) {
    const present = Object.keys(this.map);
    if (present.length !== keys.length || keys.some((key) => !Object.hasOwn(this.map, key))) {
      throw new DecodeError(`${this.tag} record has fields ${JSON.stringify(present.sort())}, expected ${JSON.stringify(keys)}`);
    }
  }
}

function outputFrom(json) {
  const pair = tagged(json);
  if (pair !== null && pair[0] === "ok") return { tag: "ok", value: pair[1] };
  if (pair !== null && pair[0] === "failed" && typeof pair[1] === "string") return { tag: "failed", error: pair[1] };
  throw new DecodeError(`output ${encodeJson(json)} is neither ok nor failed`);
}

const EFFECT_CLASSES = ["none", "idempotent", "external"];

function recordFrom(json) {
  const pair = tagged(json);
  if (pair === null || !isObject(pair[1])) throw new DecodeError("a record is an object with one tag");
  const fields = new Fields(pair[0], pair[1]);
  switch (pair[0]) {
    case "admitted":
      fields.exact(["digest", "first", "hand", "metadata", "prompt", "turn"]);
      return { kind: "admitted", turn: fields.turn(), handler: fields.text("digest"), arguments: fields.get("prompt") };
    case "call":
      return callFrom(fields);
    case "result": {
      fields.exact(["call_id", "output", "seq"]);
      const [turn, seq, nested] = fields.call();
      const output = outputFrom(fields.get("output"));
      if (nested === null) return { kind: "result", turn, seq, output };
      return { kind: "nested_result", turn, seq, nested, output };
    }
    case "model": {
      let mode = null;
      if (Object.hasOwn(fields.map, MODEL_MODE)) {
        fields.exact(["call_id", MODEL_MODE, "response", "seq", "turn"]);
        mode = fields.text(MODEL_MODE);
      } else {
        fields.exact(["call_id", "response", "seq", "turn"]);
      }
      const [turn, seq, nested] = fields.call();
      if (nested !== null) throw new DecodeError("a model reply has a nested call id");
      return { kind: "model", turn, seq, response: fields.get("response"), mode };
    }
    case "cell":
      return cellFrom(fields);
    case "recovery":
      return recoveryFrom(fields);
    case "status":
      return statusFrom(fields);
    case "checkpoint":
      fields.exact(["payload", "revision", "state"]);
      return { kind: "checkpoint", payload: fields.text("payload"), revision: fields.number("revision"), state: fields.text("state") };
    case "event":
      fields.exact(["at", "event", "seq", "turn"]);
      return { kind: "event", turn: fields.turn(), seq: fields.number("seq"), at: fields.number("at"), delivery: deliveryFrom(fields.get("event")) };
    default:
      throw new DecodeError(`unknown record '${pair[0]}'`);
  }
}

function callFrom(fields) {
  fields.exact(["call_id", "effect", "input", "seq", "tool", "turn"]);
  const [turn, seq, nested] = fields.call();
  if (fields.turn() !== turn) throw new DecodeError("call record's turn disagrees with its call id");
  const effect = fields.text("effect");
  if (!EFFECT_CLASSES.includes(effect)) throw new DecodeError("call record has an unknown effect class");
  const call = { turn, seq, tool: fields.text("tool"), effect, input: fields.get("input") };
  if (nested === null) return { kind: "call", ...call };
  return { kind: "nested_call", ...call, nested };
}

function cellFrom(fields) {
  fields.exact(["cell", "fresh", "session", "update"]);
  const cell = fields.number("cell");
  const fresh = fields.boolean("fresh");
  const session = fields.text("session");
  const update = cellUpdateFrom(fields.get("update"));
  if (update.cellId !== cell || update.sessionId !== session) {
    throw new DecodeError("cell record's cell or session disagrees with its update");
  }
  if ((update.header !== null) !== fresh) {
    throw new DecodeError("cell record is fresh exactly when its update carries the header");
  }
  return { kind: "cell", cell, fresh, session, update };
}

function attestedSeqs(fields, key, turn) {
  if (!Object.hasOwn(fields.map, key)) return [];
  const items = fields.map[key];
  if (!Array.isArray(items)) throw new DecodeError(`recovery record's ${key} is not a list`);
  const seqs = items.map((item) => {
    if (typeof item !== "string") throw new DecodeError(`recovery's ${key} lists a call id that is not text`);
    const [callTurn, seq] = parseCallId(item);
    if (callTurn !== turn) throw new DecodeError(`recovery's ${key} lists a call of another turn`);
    return seq;
  });
  if (seqs.length === 0 || seqs.some((seq, index) => index > 0 && seqs[index - 1] >= seq)) {
    throw new DecodeError(`recovery's ${key} lists no calls, or lists them out of order`);
  }
  return seqs;
}

function recoveryFrom(fields) {
  fields.exact(["attempt", "turn", "unknown", ...[RECOVERY_UNDISPATCHED, RECOVERY_REPLIED].filter((key) => Object.hasOwn(fields.map, key))].sort());
  const turn = fields.turn();
  const items = fields.get("unknown");
  if (!Array.isArray(items)) throw new DecodeError("recovery record's unknown is not a list");
  const unknown = [];
  const nested = [];
  for (const item of items) {
    if (typeof item !== "string") throw new DecodeError("recovery lists a call id that is not text");
    const [callTurn, seq, inner] = parseAnyCallId(item);
    if (callTurn !== turn) throw new DecodeError("recovery lists a call of another turn");
    if (inner === null) unknown.push(seq);
    else nested.push([seq, inner]);
  }
  unknown.sort((left, right) => left - right);
  nested.sort((left, right) => left[0] - right[0] || left[1] - right[1]);
  if (unknown.some((seq, index) => index > 0 && unknown[index - 1] === seq)
    || nested.some((pair, index) => index > 0 && nested[index - 1][0] === pair[0] && nested[index - 1][1] === pair[1])) {
    throw new DecodeError("recovery lists a call twice");
  }
  const record = { kind: "recovery", turn, attempt: fields.number("attempt"), unknown, nested, replied: attestedSeqs(fields, RECOVERY_REPLIED, turn), undispatched: attestedSeqs(fields, RECOVERY_UNDISPATCHED, turn) };
  if (!jsonEqual(recoveryIds(record), items)) throw new DecodeError("recovery lists its calls out of order");
  return record;
}

function statusFrom(fields) {
  fields.exact(["status", "turn"]);
  const pair = tagged(fields.get("status"));
  if (pair === null || pair[0] !== "finished" || !isObject(pair[1])) throw new DecodeError("status record is not finished");
  const inner = new Fields("finished", pair[1]);
  inner.exact(["last", "outcome", "result"]);
  const outcome = inner.text("outcome");
  if (outcome !== "completed" && outcome !== "failed") throw new DecodeError(`unknown outcome '${outcome}'`);
  return { kind: "status", turn: fields.turn(), last: inner.number("last"), outcome, result: inner.get("result") };
}

function encodeLine(record, checksum) {
  const body = encodeJson(recordJson(record));
  const line = checksum ? `${LINE_PREFIX}${crc32(body)}${LINE_MIDDLE}${body}${LINE_SUFFIX}` : body;
  const decoded = decodeLine(line, checksum);
  assert(encodeJson(recordJson(decoded)) === body, "a record survives its encoding");
  return line;
}

function decodeLine(line, checksum) {
  if (!checksum) return recordFrom(decodeJson(line));
  if (!line.startsWith(LINE_PREFIX)) throw new DecodeError("line does not start with its checksum");
  const rest = line.slice(LINE_PREFIX.length);
  const middle = rest.indexOf(LINE_MIDDLE);
  if (middle < 0) throw new DecodeError("line has no record after its checksum");
  const crcText = rest.slice(0, middle);
  const tail = rest.slice(middle + LINE_MIDDLE.length);
  if (!tail.endsWith(LINE_SUFFIX)) throw new DecodeError("line does not end its envelope");
  const body = tail.slice(0, tail.length - LINE_SUFFIX.length);
  if (!/^\+?[0-9]+$/.test(crcText) || BigInt(crcText) > 0xffffffffn) throw new DecodeError("line's checksum is not a number");
  if (crc32(body) !== Number(crcText)) throw new DecodeError("line's checksum does not match its record");
  return recordFrom(decodeJson(body));
}

function encodeLines(records, checksum) {
  return records.map((record) => encodeLine(record, checksum) + LINE_END).join("");
}

function decodeLines(text, checksum) {
  const records = [];
  let offset = 0;
  while (offset < text.length) {
    const end = text.indexOf(LINE_END, offset);
    if (end < 0) {
      if (!checksum) throw new DecodeError(`line ${records.length + 1} is not terminated`);
      break;
    }
    try {
      records.push(decodeLine(text.slice(offset, end), checksum));
    } catch (error) {
      if (!(error instanceof DecodeError) || !checksum) throw error;
      break;
    }
    offset = end + 1;
  }
  return { records, validLength: offset };
}

function newTurnLog(handler, args) {
  return { handler, arguments: args, sites: new Map(), lastSeq: 0, attempts: 0, finished: null, nested: new Map(), lastNested: new Map(), cells: new Map(), events: [], undispatched: new Set() };
}

function runningLog(session, turn, what) {
  const log = session.turns.get(turn);
  if (log === undefined) throw new Corrupt(`${what} for turn ${turn}, which was not admitted`);
  if (log.finished !== null) throw new Corrupt(`${what} for turn ${turn}, which already finished`);
  return log;
}

function nextSite(log, turn, seq, site) {
  if (log.sites.size > 0 && seq <= log.lastSeq) {
    throw new Corrupt(`turn ${turn} records seq ${seq} after seq ${log.lastSeq}`);
  }
  log.sites.set(seq, site);
  log.lastSeq = seq;
}

function fold(records) {
  const session = { turns: new Map(), lastTurn: 0, checkpoints: new Map(), cellOrigins: new Map(), lastEvent: 0, jobs: new Map() };
  for (const record of records) {
    applyRecord(session, record);
  }
  return session;
}

function applyRecord(session, record) {
  switch (record.kind) {
    case "admitted": {
      if (session.turns.size > 0) {
        const last = session.lastTurn;
        if (record.turn !== last + 1) throw new Corrupt(`turn ${record.turn} is admitted after turn ${last}`);
        if (session.turns.get(last).finished === null) throw new Corrupt(`turn ${record.turn} is admitted before turn ${last} finished`);
      }
      session.turns.set(record.turn, newTurnLog(record.handler, record.arguments));
      session.lastTurn = record.turn;
      return;
    }
    case "call": {
      const log = runningLog(session, record.turn, "a call");
      nextSite(log, record.turn, record.seq, { kind: "open", call: { tool: record.tool, effect: record.effect, input: record.input } });
      return;
    }
    case "result": {
      const log = runningLog(session, record.turn, "a result");
      const site = log.sites.get(record.seq);
      if (site === undefined) throw new Corrupt(`result for seq ${record.seq} of turn ${record.turn}, which has no call`);
      if (site.kind !== "open") throw new Corrupt(`result for seq ${record.seq} of turn ${record.turn}, which is not open`);
      log.sites.set(record.seq, { kind: "done", call: site.call, output: record.output });
      if (jobStarted(record.output) !== null) session.jobs.set(spawnKey(record.turn, record.seq), site.call.tool);
      return;
    }
    case "model": {
      const log = runningLog(session, record.turn, "a model reply");
      nextSite(log, record.turn, record.seq, { kind: "reply", response: record.response, mode: record.mode });
      return;
    }
    case "nested_call":
      nestedCallFold(runningLog(session, record.turn, "a nested call"), record);
      return;
    case "nested_result":
      nestedResultFold(runningLog(session, record.turn, "a nested result"), record);
      return;
    case "cell":
      cellFold(session, record);
      return;
    case "recovery":
      applyRecovery(session, record);
      return;
    case "status": {
      const log = runningLog(session, record.turn, "a status");
      log.finished = { outcome: record.outcome, result: record.result, last: record.last };
      return;
    }
    case "event":
      applyEvent(session, record);
      return;
    case "checkpoint": {
      const log = session.turns.get(record.revision);
      if (log === undefined || log.finished === null) {
        throw new Corrupt(`checkpoint at revision ${record.revision}, which is not a finished turn`);
      }
      session.checkpoints.set(record.revision, record.payload);
      return;
    }
    default:
      throw new Panic(`unknown record kind ${record.kind}`);
  }
}

function applyEvent(session, record) {
  if (record.seq <= session.lastEvent) {
    throw new Corrupt(`event ${record.seq} of turn ${record.turn} follows event ${session.lastEvent}`);
  }
  const log = runningLog(session, record.turn, "an event");
  const last = log.events.at(-1);
  if (last !== undefined && record.at < last.at) {
    throw new Corrupt(`event ${record.seq} of turn ${record.turn} is at seq ${record.at}, before the event at seq ${last.at}`);
  }
  if (log.events.some((event) => event.delivery.id === record.delivery.id)) {
    throw new Corrupt(`event '${record.delivery.id}' of turn ${record.turn} is recorded twice`);
  }
  const cancelled = log.events.some((event) => event.delivery.kind === "cancel");
  if (cancelled && record.delivery.kind === "cancel") throw new Corrupt(`turn ${record.turn} records a second cancel`);
  if (record.delivery.kind === "job") {
    if (!session.jobs.has(record.delivery.key)) throw new Corrupt(`event '${record.delivery.id}' of turn ${record.turn} notes job ${record.delivery.key}, which is not running`);
    if (record.delivery.done) session.jobs.delete(record.delivery.key);
  }
  log.events.push({ at: record.at, delivery: record.delivery });
  session.lastEvent = record.seq;
}

function eventsTaken() {
  return PROGRAM.events.cancel || PROGRAM.events.steers !== null || PROGRAM.events.jobs !== null;
}

function spawnKey(turn, seq) {
  return String(turn) + SPAWN_KEY_SEPARATOR + String(seq);
}

function interruptsOf(kind, tool) {
  if (kind === "model") return PROGRAM.model === null ? NO_INTERRUPTS : PROGRAM.model.interrupts;
  if (kind !== "tool" && kind !== "dispatch") return NO_INTERRUPTS;
  const declared = PROGRAM.tools.find((candidate) => candidate.name === tool);
  return declared === undefined ? NO_INTERRUPTS : declared.interrupts;
}

function anyInterrupt(interrupts) {
  return interrupts.cancel || interrupts.steer;
}

function interruptedBy(delivery, by) {
  return (delivery.kind === "cancel" && by.cancel) || (delivery.kind === "steer" && by.steer);
}

function interruptNames(by) {
  return [...(by.cancel ? ["cancel"] : []), ...(by.steer ? ["steer"] : [])];
}

function recoverable(tool) {
  return PROGRAM.tools.some((candidate) => candidate.name === tool && candidate.recoverable);
}

function jobsOf(tool) {
  const declared = PROGRAM.tools.find((candidate) => candidate.name === tool);
  return declared === undefined ? null : declared.jobs;
}

function jobRecorded(sites, turn, key) {
  const prefix = String(turn) + SPAWN_KEY_SEPARATOR;
  if (!key.startsWith(prefix) || !/^[0-9]+$/.test(key.slice(prefix.length))) return true;
  return sites.get(Number(key.slice(prefix.length)))?.kind === "done";
}

function recoveredFrom(json) {
  if (typeof json === "string" && RECOVERED_ANSWERS.includes(json)) return { kind: json };
  const pair = tagged(json);
  if (pair !== null && pair[0] === RECOVERED_REPLY) return { kind: "reply", output: outputFrom(pair[1]) };
  throw new DecodeError(`a recovery answer is not_dispatched, unknown or a reply, found ${encodeJson(json)}`);
}

function jobCount(events) {
  return events.filter((event) => event.delivery.kind === "job").length;
}

function eventSeq(turn, index) {
  assert(index >= 1, "an event's index in its turn counts from 1");
  const seq = BigInt(turn - 1) * PROGRAM.events.recordsMax + BigInt(index);
  return seq > BigInt(Number.MAX_SAFE_INTEGER) ? null : Number(seq);
}

function steerCount(events) {
  return events.filter((event) => event.delivery.kind === "steer").length;
}

function admitEvent(turn, events, position, delivery, jobs) {
  if (events.some((event) => event.delivery.id === delivery.id)) return { answer: "duplicate", record: null };
  if (delivery.kind === "cancel") {
    if (!PROGRAM.events.cancel) return { answer: "undeclared", record: null };
    if (events.some((event) => event.delivery.kind === "cancel")) return { answer: "duplicate", record: null };
  } else if (delivery.kind === "job") {
    if (PROGRAM.events.jobs === null) return { answer: "undeclared", record: null };
    if (!jobs.open.has(delivery.key)) return { answer: "duplicate", record: null };
    if (!jobs.recorded(delivery.key)) return { answer: "full", record: null };
    if (BigInt(jobCount(events)) >= PROGRAM.events.jobs.max) return { answer: "full", record: null };
  } else {
    if (PROGRAM.events.steers === null) return { answer: "undeclared", record: null };
    if (BigInt(steerCount(events)) >= PROGRAM.events.steers.max) return { answer: "full", record: null };
  }
  const seq = eventSeq(turn, events.length + 1);
  if (seq === null) return { answer: "full", record: null };
  events.push({ at: position, delivery });
  if (delivery.kind === "job" && delivery.done) jobs.open.delete(delivery.key);
  return { answer: "recorded", record: { kind: "event", turn, seq, at: position, delivery } };
}

function closingAnswers(events, arrived) {
  return arrived.map((delivery) => [delivery.id, events.some((event) => event.delivery.id === delivery.id) ? "duplicate" : "finished"]);
}

function checkEvents(turn, log) {
  for (const event of log.events) {
    const taken = event.delivery.kind === "cancel" ? PROGRAM.events.cancel : event.delivery.kind === "job" ? PROGRAM.events.jobs !== null : PROGRAM.events.steers !== null;
    if (!taken) throw new Corrupt(`turn ${turn} records a ${event.delivery.kind} event, which the program does not take`);
  }
  const jobs = jobCount(log.events);
  if (PROGRAM.events.jobs !== null && BigInt(jobs) > PROGRAM.events.jobs.max) {
    throw new Corrupt(`turn ${turn} records ${jobs} job notices, more than ${PROGRAM.events.jobs.name} (${PROGRAM.events.jobs.max})`);
  }
  const steers = steerCount(log.events);
  if (PROGRAM.events.steers !== null && BigInt(steers) > PROGRAM.events.steers.max) {
    throw new Corrupt(`turn ${turn} records ${steers} steers, more than ${PROGRAM.events.steers.name} (${PROGRAM.events.steers.max})`);
  }
}

function applyRecovery(session, record) {
  const log = runningLog(session, record.turn, "a recovery");
  for (const [seq, nested] of record.nested) nestedRecoverFold(log, record.turn, seq, nested);
  if (record.attempt <= log.attempts) {
    throw new Corrupt(`recovery attempt ${record.attempt} of turn ${record.turn} follows attempt ${log.attempts}`);
  }
  log.attempts = record.attempt;
  for (const seq of record.undispatched) log.undispatched.add(seq);
  for (const seq of [...record.replied, ...record.undispatched]) {
    const site = log.sites.get(seq);
    if (site === undefined || site.kind !== "open" || !mayBeUnknown(site.call.effect) || record.unknown.includes(seq)) {
      throw new Corrupt(`recovery attests seq ${seq} of turn ${record.turn}, which is not an open external call`);
    }
  }
  for (const seq of record.unknown) {
    const site = log.sites.get(seq);
    if (site === undefined) throw new Corrupt(`recovery of seq ${seq} of turn ${record.turn}, which has no call`);
    if (site.kind !== "open") throw new Corrupt(`recovery of seq ${seq} of turn ${record.turn}, which is not open`);
    if (!mayBeUnknown(site.call.effect)) {
      throw new Corrupt(`recovery reports ${site.call.effect} call seq ${seq} of turn ${record.turn} unknown`);
    }
    log.sites.set(seq, { kind: "unknown", call: site.call });
  }
}

function variant(index, fields) {
  return { v: index, f: fields };
}

function typeVariants(type) {
  if (type.sum !== undefined) return PROGRAM.sums[type.sum].variants;
  if (type.known !== undefined) return outcomeVariants(type.known, false);
  if (type.maybe !== undefined) return outcomeVariants(type.maybe, true);
  return null;
}

function outcomeVariants(inner, unknown) {
  const variants = [{ name: "ok", fields: [inner] }, { name: "failed", fields: ["text"] }];
  if (unknown) variants.push({ name: "unknown", fields: [] });
  return variants;
}

function typeName(type) {
  if (typeof type === "string") return type;
  if (type.sum !== undefined) return PROGRAM.sums[type.sum].name;
  if (type.list !== undefined) return `list<${typeName(type.list)}, ${type.bound}>`;
  if (type.blob !== undefined) return `blob<${type.bound}>`;
  if (type.known !== undefined) return `known<${typeName(type.known)}>`;
  return `maybe<${typeName(type.maybe)}>`;
}

function toJson(type, value) {
  if (type === "u64") {
    assert(typeof value === "bigint", "a u64 value is a bigint");
    return value;
  }
  if (type === "bool") {
    assert(typeof value === "boolean", "a bool value is a boolean");
    return value;
  }
  if (type === "text") {
    assert(typeof value === "string", "a text value is a string");
    return value;
  }
  if (type.list !== undefined) {
    assert(Array.isArray(value), "a list value is an array");
    return value.map((item) => toJson(type.list, item));
  }
  if (type.blob !== undefined) {
    assert(typeof value === "string", "a blob value is its digest");
    return blobJson(value);
  }
  assert(value !== null && typeof value === "object" && Number.isInteger(value.v), "a sum value is a variant");
  if (type.sum === SUM_CALL) {
    return object(CALL_FIELDS.map((name, index) => [name, value.f[index]]));
  }
  const definition = typeVariants(type)[value.v];
  assert(definition.fields.length === value.f.length, "a variant value carries its fields");
  if (value.f.length === 0) return definition.name;
  if (value.f.length === 1) return object([[definition.name, toJson(definition.fields[0], value.f[0])]]);
  return object([[definition.name, definition.fields.map((field, index) => toJson(field, value.f[index]))]]);
}

function fromJson(type, json) {
  if (type === "u64") {
    if (typeof json !== "bigint") throw new DecodeError(`expected u64, found ${encodeJson(json)}`);
    return json;
  }
  if (type === "bool") {
    if (typeof json !== "boolean") throw new DecodeError(`expected bool, found ${encodeJson(json)}`);
    return json;
  }
  if (type === "text") {
    if (typeof json !== "string") throw new DecodeError(`expected text, found ${encodeJson(json)}`);
    return json;
  }
  if (type.list !== undefined) {
    if (!Array.isArray(json)) throw new DecodeError(`expected a list, found ${encodeJson(json)}`);
    if (BigInt(json.length) > type.capacity) throw new DecodeError(`bound(${type.bound})`);
    return json.map((item) => fromJson(type.list, item));
  }
  if (type.blob !== undefined) return digestOf(json);
  if (type.sum === SUM_CALL) {
    const fields = CALL_FIELDS.map((name) => {
      const field = isObject(json) && Object.hasOwn(json, name) ? json[name] : undefined;
      if (typeof field !== "string") throw new DecodeError(`call lacks text field '${name}'`);
      return field;
    });
    if (Object.keys(json).length !== CALL_FIELDS.length) throw new DecodeError("call has fields other than id, tool and input");
    return variant(0, fields);
  }
  return variantFromJson(type, json);
}

function variantFromJson(type, json) {
  const variants = typeVariants(type);
  let name;
  let payload;
  if (typeof json === "string") {
    name = json;
  } else {
    const pair = tagged(json);
    if (pair === null) throw new DecodeError(`expected a variant of ${typeName(type)}, found ${encodeJson(json)}`);
    [name, payload] = pair;
  }
  const index = variants.findIndex((candidate) => candidate.name === name);
  if (index < 0) throw new DecodeError(`'${name}' is not a variant of ${typeName(type)}`);
  const fields = variants[index].fields;
  if (fields.length === 0 && payload === undefined) return variant(index, []);
  if (fields.length === 1 && payload !== undefined) return variant(index, [fromJson(fields[0], payload)]);
  if (fields.length > 1 && Array.isArray(payload) && payload.length === fields.length) {
    return variant(index, fields.map((field, position) => fromJson(field, payload[position])));
  }
  throw new DecodeError(`variant '${name}' has ${fields.length} field(s)`);
}

function valueEqual(left, right) {
  if (left === right) return true;
  if (Array.isArray(left)) {
    return Array.isArray(right) && left.length === right.length && left.every((item, index) => valueEqual(item, right[index]));
  }
  if (left === null || right === null || typeof left !== "object" || typeof right !== "object") return false;
  return left.v === right.v && valueEqual(left.f, right.f);
}

function stateJson(state) {
  assert(state.length === PROGRAM.state.length, "the state has one value per field");
  return object(PROGRAM.state.map((field, index) => [field.name, toJson(field.ty, state[index])]));
}

function stateFromJson(json) {
  if (!isObject(json)) throw new DecodeError("state is not an object");
  const count = Object.keys(json).length;
  if (count !== PROGRAM.state.length) {
    throw new DecodeError(`state has ${count} fields, the program has ${PROGRAM.state.length}`);
  }
  return PROGRAM.state.map((field) => {
    if (!Object.hasOwn(json, field.name)) throw new DecodeError(`state lacks field '${field.name}'`);
    return fromJson(field.ty, json[field.name]);
  });
}

function zeroValue(type) {
  if (type === "u64") return 0n;
  if (type === "bool") return false;
  if (type === "text") return "";
  if (type.sum !== undefined) {
    assert(PROGRAM.sums[type.sum].text === true, "the checker admits only text enums among sum state fields");
    return variant(0, []);
  }
  assert(type.list !== undefined, "the checker admits only state fields with a zero value");
  return [];
}

function returnedName(type) {
  if (type === "u64") return "a u64";
  if (type === "bool") return "a bool";
  if (type === "text") return "text";
  assert(type.sum !== undefined && PROGRAM.sums[type.sum].text === true, "the checker admits only scalar or text enum tool results");
  return `a value of ${PROGRAM.sums[type.sum].name}`;
}

function checkedOutput(tool, returns, output) {
  const bound = blobReturned(tool);
  if (bound !== null && (returns.blob !== undefined || (output.tag === "ok" && isObject(output.value)))) return blobChecked(tool, bound, output);
  if (output.tag !== "ok") return output;
  try {
    fromJson(returns, output.value);
    return output;
  } catch (error) {
    if (!(error instanceof DecodeError)) throw error;
  }
  return { tag: "failed", error: `${tool} returned ${encodeJson(output.value)}, which is not ${returnedName(returns)}` };
}

function initialState() {
  return PROGRAM.state.map((field) => zeroValue(field.ty));
}

function argumentsJson(handler, values) {
  return object(handler.params.map((param, index) => [param.name, toJson(param.ty, values[index])]));
}

function argumentsFromJson(handler, json) {
  return handler.params.map((param) => {
    if (!isObject(json) || !Object.hasOwn(json, param.name)) throw new DecodeError(`arguments lack '${param.name}'`);
    return fromJson(param.ty, json[param.name]);
  });
}

function handlerNamed(name) {
  return PROGRAM.handlers.find((handler) => handler.name === name) ?? null;
}

const LISTED_TOOL_SEPARATOR = "__";

function listed(tool) {
  const at = tool.indexOf(LISTED_TOOL_SEPARATOR);
  if (at < 0) return null;
  const prefix = tool.slice(0, at);
  const name = tool.slice(at + LISTED_TOOL_SEPARATOR.length);
  if (name === "") return null;
  const listing = PROGRAM.listings.findIndex((candidate) => candidate.name === prefix);
  return listing < 0 ? null : { listing, name };
}

function waitsNamed(tool) {
  const index = PROGRAM.codes.findIndex((code) => code.waits !== null && code.waits.name === tool);
  return index < 0 ? null : index;
}

function cellsYield() {
  return PROGRAM.codes.some((code) => code.waits !== null);
}

function dispatchClass(tool) {
  const declared = PROGRAM.tools.find((candidate) => candidate.name === tool);
  if (declared !== undefined) return declared.effect;
  if (waitsNamed(tool) !== null) return "none";
  const found = listed(tool);
  return found === null ? "external" : PROGRAM.listings[found.listing].effect;
}

const MODEL_MODE = "mode";

function inputDefaults(tool) {
  const declared = PROGRAM.tools.find((candidate) => candidate.name === tool);
  if (declared !== undefined) return declared.defaults;
  const waits = waitsNamed(tool);
  if (waits !== null) return PROGRAM.codes[waits].waits.defaults;
  const found = listed(tool);
  return found === null ? null : PROGRAM.listings[found.listing].defaults;
}

function inputParams(tool) {
  const declared = PROGRAM.tools.find((candidate) => candidate.name === tool);
  if (declared !== undefined) return declared.params;
  const waits = waitsNamed(tool);
  if (waits !== null) return PROGRAM.codes[waits].waits.params;
  const found = listed(tool);
  return found === null ? null : PROGRAM.listings[found.listing].params;
}

function inputFits(params, input) {
  if (!isObject(input)) return "not a JSON object";
  for (const param of params) {
    if (!Object.hasOwn(input, param.name)) return `missing '${param.name}'`;
    try {
      fromJson(param.ty, input[param.name]);
    } catch (error) {
      if (!(error instanceof DecodeError)) throw error;
      return `'${param.name}': ${error.message}`;
    }
  }
  const extra = Object.keys(input).sort(compareKeys).find((key) => !params.some((param) => param.name === key));
  return extra === undefined ? null : `'${extra}' is not a parameter`;
}

function toolInput(tool, raw) {
  const params = inputParams(tool);
  if (params === null) return { kind: "untyped" };
  let input = null;
  try {
    input = decodeJson(raw);
  } catch (error) {
    if (!(error instanceof DecodeError)) throw error;
  }
  if (isObject(input)) {
    const defaults = inputDefaults(tool);
    params.forEach((param, index) => {
      if (defaults[index] !== null && !Object.hasOwn(input, param.name)) input[param.name] = defaults[index];
    });
  }
  const error = isObject(input) ? inputFits(params, input) : "not a JSON object";
  if (error !== null) return { kind: "invalid", error: `input of ${tool}: ${error}` };
  return { kind: "typed", input };
}

function notAdmitted(tool, listing, name) {
  return `${tool} was not admitted: listing ${listing} did not list ${name} in this turn`;
}

function admittedTools(json) {
  if (!isObject(json)) throw new DecodeError(`a listing is an object, found ${encodeJson(json)}`);
  const tools = new Map();
  for (const name of Object.keys(json).sort(compareKeys)) {
    const fingerprint = json[name];
    if (typeof fingerprint !== "string") {
      throw new DecodeError(`listed tool '${name}' has fingerprint ${encodeJson(fingerprint)}, not text`);
    }
    tools.set(name, fingerprint);
  }
  return tools;
}

function checkedListing(listing, output) {
  if (output.tag !== "ok") return output;
  const json = output.value;
  const fits = isObject(json) && Object.keys(json).every((name) => name !== "" && typeof json[name] === "string");
  if (!fits) {
    return { tag: "failed", error: `listing ${listing.name} returned ${encodeJson(json)}, which is not an object of fingerprints` };
  }
  const count = BigInt(Object.keys(json).length);
  if (count > listing.bound) {
    return { tag: "failed", error: `listing ${listing.name} returned ${count} tools, over its bound ${listing.boundName} (${listing.bound})` };
  }
  return output;
}

function outbound(kind) {
  return kind === "tool" || kind === "listing" || kind === "dispatch" || kind === "model";
}

function flushes(durability, resolved) {
  if (durability === "rides_next_flush") return false;
  if (durability === "flush_before_run") return true;
  return resolved === "external";
}

function runPoint(name, kind, effect) {
  return { point: name, kind, effect };
}

function add(left, right) {
  const result = left + right;
  if (result > U64_MAX) throw new Trap("u64 overflow in '+'");
  return result;
}

function subtract(left, right) {
  const result = left - right;
  if (result < 0n) throw new Trap("u64 underflow in '-'");
  return result;
}

function multiply(left, right) {
  const result = left * right;
  if (result > U64_MAX) throw new Trap("u64 overflow in '*'");
  return result;
}

const TEXT_WHITESPACE = /^[ \t\n\f\r]+|[ \t\n\f\r]+$/g;

function text_contains(subject, separator) {
  return subject.includes(separator);
}

function text_before(subject, separator) {
  const at = subject.indexOf(separator);
  return at < 0 ? subject : subject.slice(0, at);
}

function text_after(subject, separator) {
  const at = subject.indexOf(separator);
  return at < 0 ? "" : subject.slice(at + separator.length);
}

function text_trim(subject) {
  return subject.replace(TEXT_WHITESPACE, "");
}

function both(left, right) {
  return left && right;
}

function either(left, right) {
  return left || right;
}

function batchWidth(count, limit) {
  if (limit === null) return count;
  assert(Number.isSafeInteger(limit) && limit > 0, "a concurrency limit admits at least one request");
  return Math.min(limit, count);
}

async function dispatchConcurrently(world, requests, limit) {
  const completed = [];
  let next = 0;
  const worker = async () => {
    for (let taken = 0; taken < requests.length; taken++) {
      const index = next;
      if (index >= requests.length) return;
      next += 1;
      await world.hook(runPoint("before_run", "dispatch", requests[index].effect));
      const output = await world.tool(requests[index]);
      await world.hook(runPoint("after_run", "dispatch", requests[index].effect));
      completed.push([index, output]);
    }
  };
  const workers = [];
  for (let slot = 0; slot < batchWidth(requests.length, limit); slot++) workers.push(worker());
  await Promise.all(workers);
  assert(completed.length === requests.length, "every request of a batch completes once");
  return completed;
}

function within(items, capacity) {
  assert(BigInt(items.length) <= capacity, "a list holds at most its capacity");
  return items;
}
