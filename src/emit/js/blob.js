const BLOB_KEY = "blob";
const BLOB_DATA_KEY = "data";
const DIGEST_BYTES = 32;
const DIGEST_HEX = /^[0-9a-f]{64}$/;
const OMITTED_REFERENCE_PREFIX = "sha256:";
const ENTRY_RESULT = 2;
const SEARCH_SEPARATOR = "\n";
const DISPATCHED_BLOB = 3;
const MODEL_BLOBS = "blobs";
const SHA256_BLOCK_BYTES = 64;
const SHA256_LENGTH_BYTES = 8;
const SHA256_ROUNDS = 64;
const SHA256_SCHEDULE_WORDS = 16;
const SHA256_INITIAL = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
const SHA256_ROUND_CONSTANTS = [
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];
const SHA256_CHECK_INPUT = "abc";
const SHA256_CHECK = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64_PAD = 0x3d;
const BASE64_GROUP_BYTES = 3;
const BASE64_GROUP_CHARS = 4;
const BASE64_PADDING_MAX = 2;
const BASE64_SEXTET_BITS = 6;
const BASE64_SEXTETS = (() => {
  const table = new Int16Array(BYTE_VALUES).fill(-1);
  for (let index = 0; index < BASE64_ALPHABET.length; index++) table[BASE64_ALPHABET.charCodeAt(index)] = index;
  return table;
})();
const RUST_DEBUG_ESCAPES = new Map([[0x00, "\\0"], [0x09, "\\t"], [0x0a, "\\n"], [0x0d, "\\r"], [0x5c, "\\\\"]]);
const RUST_UNPRINTABLE_LAST = 0x1f;
const RUST_UNPRINTABLE_HIGH_FIRST = 0x7f;
const RUST_UNPRINTABLE_HIGH_LAST = 0xa0;
const RUST_SOFT_HYPHEN = 0xad;

function rotateRight(word, count) {
  return (word >>> count) | (word << (32 - count));
}

function sha256Block(hash, bytes, offset, words) {
  for (let index = 0; index < SHA256_SCHEDULE_WORDS; index++) {
    const at = offset + 4 * index;
    words[index] = (bytes[at] << 24) | (bytes[at + 1] << 16) | (bytes[at + 2] << 8) | bytes[at + 3];
  }
  for (let index = SHA256_SCHEDULE_WORDS; index < SHA256_ROUNDS; index++) {
    const early = words[index - 15];
    const late = words[index - 2];
    const small0 = rotateRight(early, 7) ^ rotateRight(early, 18) ^ (early >>> 3);
    const small1 = rotateRight(late, 17) ^ rotateRight(late, 19) ^ (late >>> 10);
    words[index] = (words[index - 16] + small0 + words[index - 7] + small1) | 0;
  }
  let [a, b, c, d, e, f, g, h] = hash;
  for (let index = 0; index < SHA256_ROUNDS; index++) {
    const big1 = rotateRight(e, 6) ^ rotateRight(e, 11) ^ rotateRight(e, 25);
    const choice = (e & f) ^ (~e & g);
    const first = (h + big1 + choice + SHA256_ROUND_CONSTANTS[index] + words[index]) | 0;
    const big0 = rotateRight(a, 2) ^ rotateRight(a, 13) ^ rotateRight(a, 22);
    const majority = (a & b) ^ (a & c) ^ (b & c);
    const second = (big0 + majority) | 0;
    h = g;
    g = f;
    f = e;
    e = (d + first) | 0;
    d = c;
    c = b;
    b = a;
    a = (first + second) | 0;
  }
  const state = [a, b, c, d, e, f, g, h];
  for (let index = 0; index < hash.length; index++) hash[index] = (hash[index] + state[index]) | 0;
}

function sha256Hex(bytes) {
  const hash = SHA256_INITIAL.slice();
  const words = new Int32Array(SHA256_ROUNDS);
  const whole = bytes.length - (bytes.length % SHA256_BLOCK_BYTES);
  for (let offset = 0; offset < whole; offset += SHA256_BLOCK_BYTES) sha256Block(hash, bytes, offset, words);
  const rest = bytes.length - whole;
  const tailLength = rest + 1 + SHA256_LENGTH_BYTES <= SHA256_BLOCK_BYTES ? SHA256_BLOCK_BYTES : 2 * SHA256_BLOCK_BYTES;
  const tail = new Uint8Array(tailLength);
  tail.set(bytes.subarray(whole));
  tail[rest] = 0x80;
  const view = new DataView(tail.buffer);
  view.setBigUint64(tailLength - SHA256_LENGTH_BYTES, BigInt(bytes.length) * BigInt(BITS_PER_BYTE));
  for (let offset = 0; offset < tailLength; offset += SHA256_BLOCK_BYTES) sha256Block(hash, tail, offset, words);
  const hex = hash.map((word) => (word >>> 0).toString(16).padStart(8, "0")).join("");
  assert(hex.length === 2 * DIGEST_BYTES, "a sha-256 digest has digest_bytes bytes");
  return hex;
}

assert(sha256Hex(textEncoder.encode(SHA256_CHECK_INPUT)) === SHA256_CHECK, "SHA-256 matches its catalogued check value");

function rustDebugByte(byte, quote) {
  if (RUST_DEBUG_ESCAPES.has(byte)) return RUST_DEBUG_ESCAPES.get(byte);
  if (byte === quote) return "\\" + String.fromCharCode(byte);
  const unprintable = byte <= RUST_UNPRINTABLE_LAST || (byte >= RUST_UNPRINTABLE_HIGH_FIRST && byte <= RUST_UNPRINTABLE_HIGH_LAST) || byte === RUST_SOFT_HYPHEN;
  return unprintable ? `\\u{${byte.toString(16)}}` : String.fromCharCode(byte);
}

function rustDebugText(text) {
  let out = "\"";
  for (const character of text) {
    const code = character.codePointAt(0);
    out += code < BYTE_VALUES ? rustDebugByte(code, 0x22) : character;
  }
  return out + "\"";
}

function base64Decode(text) {
  const bytes = textEncoder.encode(text);
  if (bytes.length % BASE64_GROUP_CHARS !== 0) return { error: `${bytes.length} characters, not a multiple of ${BASE64_GROUP_CHARS}` };
  const groups = bytes.length / BASE64_GROUP_CHARS;
  const out = new Uint8Array(groups * BASE64_GROUP_BYTES);
  let written = 0;
  for (let group = 0; group < groups; group++) {
    const start = group * BASE64_GROUP_CHARS;
    let padding = 0;
    while (padding < BASE64_GROUP_CHARS && bytes[start + BASE64_GROUP_CHARS - 1 - padding] === BASE64_PAD) padding += 1;
    if (padding > BASE64_PADDING_MAX || (padding > 0 && group + 1 !== groups)) return { error: "padding inside the text" };
    let joined = 0;
    for (let index = 0; index < BASE64_GROUP_CHARS - padding; index++) {
      const byte = bytes[start + index];
      const sextet = BASE64_SEXTETS[byte];
      if (sextet < 0) return { error: `character '${rustDebugByte(byte, 0x27)}'` };
      joined = (joined << BASE64_SEXTET_BITS) | sextet;
    }
    joined <<= BASE64_SEXTET_BITS * padding;
    const kept = BASE64_GROUP_BYTES - padding;
    const decoded = [(joined >>> 16) & 0xff, (joined >>> 8) & 0xff, joined & 0xff];
    if (decoded.slice(kept).some((byte) => byte !== 0)) return { error: "nonzero bits after the last byte" };
    for (let index = 0; index < kept; index++) out[written++] = decoded[index];
  }
  return { bytes: out.subarray(0, written) };
}

function blobJson(digest) {
  return object([[BLOB_KEY, digest]]);
}

function digestOf(json) {
  if (!isObject(json)) throw new DecodeError(`expected a blob, found ${encodeJson(json)}`);
  const digest = Object.hasOwn(json, BLOB_KEY) ? json[BLOB_KEY] : undefined;
  if (typeof digest !== "string" || !DIGEST_HEX.test(digest)) throw new DecodeError(`expected a blob's sha-256 digest, found ${encodeJson(json)}`);
  if (Object.keys(json).some((key) => key !== BLOB_KEY && key !== BLOB_DATA_KEY)) {
    throw new DecodeError(`a blob holds its digest and data, found ${encodeJson(json)}`);
  }
  return digest;
}

function blobReturned(tool) {
  const declared = PROGRAM.tools.find((candidate) => candidate.name === tool);
  if (declared === undefined) return null;
  if (declared.returns.blob !== undefined) return declared.returns;
  return declared.attaches ?? null;
}

function hostData(json) {
  if (!isObject(json) || Object.keys(json).length !== 1 || typeof json[BLOB_DATA_KEY] !== "string") return null;
  return json[BLOB_DATA_KEY];
}

function blobChecked(tool, returns, output) {
  if (output.tag !== "ok") return output;
  const data = hostData(output.value);
  if (data === null) {
    return { tag: "failed", error: `${tool} returned ${encodeJson(output.value)}, which is not a blob: a host returns a blob as {"data": base64}` };
  }
  const decoded = base64Decode(data);
  if (decoded.error !== undefined) return { tag: "failed", error: `${tool} returned text that is not base64 (${decoded.error}), which is not a blob` };
  if (BigInt(decoded.bytes.length) > returns.blob) {
    return { tag: "failed", error: `${tool} returned a blob of ${decoded.bytes.length} bytes, over its bound ${returns.bound} (${returns.blob})` };
  }
  return output;
}

class Blobs {
  constructor() {
    this.held = new Map();
  }

  journaled(data, withData) {
    const decoded = base64Decode(data);
    assert(decoded.error === undefined, "a checked blob is base64");
    const digest = sha256Hex(decoded.bytes);
    const fresh = !this.held.has(digest);
    if (fresh) this.held.set(digest, data);
    if (fresh && withData) return object([[BLOB_KEY, digest], [BLOB_DATA_KEY, data]]);
    return blobJson(digest);
  }

  read(json) {
    let digest;
    try {
      digest = digestOf(json);
    } catch (error) {
      if (!(error instanceof DecodeError)) throw error;
      return { error: error.message };
    }
    if (!Object.hasOwn(json, BLOB_DATA_KEY)) {
      if (this.held.has(digest)) return { digest };
      return { error: `blob ${digest} is journaled without its data, and no earlier record holds it` };
    }
    const data = json[BLOB_DATA_KEY];
    if (typeof data !== "string") return { error: `blob ${digest} has data ${encodeJson(data)}, not text` };
    const decoded = base64Decode(data);
    if (decoded.error !== undefined) return { error: `blob ${digest} has data that is not base64: ${decoded.error}` };
    const hashed = sha256Hex(decoded.bytes);
    if (hashed !== digest) return { error: `blob ${digest} holds data whose digest is ${hashed}` };
    if (!this.held.has(digest)) this.held.set(digest, data);
    return { digest };
  }

  json() {
    return object([...this.held.keys()].sort(compareKeys).map((digest) => [digest, this.held.get(digest)]));
  }

  checkState(state) {
    const digests = [];
    PROGRAM.state.forEach((field, index) => collectBlobs(field.ty, state[index], digests));
    const missing = digests.find((digest) => !this.held.has(digest));
    if (missing !== undefined) throw new Corrupt(`the state holds blob ${missing}, which no result of the journal holds`);
    assert(PROGRAM.blobs !== undefined || this.held.size === 0, "only a program with blob tools holds blobs");
  }
}

function sessionBlobs(session) {
  const blobs = new Blobs();
  if (PROGRAM.blobs === undefined) return blobs;
  const turns = [...session.turns.keys()].sort((left, right) => left - right);
  for (const turn of turns) {
    const log = session.turns.get(turn);
    const seqs = [...log.sites.keys()].sort((left, right) => left - right);
    for (const seq of seqs) {
      const site = log.sites.get(seq);
      if (site.kind !== "done" || site.output.tag !== "ok" || !isObject(site.output.value) || blobReturned(site.call.tool) === null) continue;
      const read = blobs.read(site.output.value);
      if (read.error !== undefined) throw new Corrupt(`result ${turn}/${seq}: ${read.error}`);
    }
  }
  return blobs;
}

function collectBlobs(type, value, out) {
  if (typeof type === "string") return;
  if (type.blob !== undefined) {
    out.push(value);
    return;
  }
  if (type.list !== undefined) {
    for (const item of value) collectBlobs(type.list, item, out);
    return;
  }
  if (type.sum === SUM_CALL) return;
  const variants = typeVariants(type);
  if (variants === null) return;
  const fields = variants[value.v].fields;
  fields.forEach((field, index) => collectBlobs(field, value.f[index], out));
}

function blobsEvicted(total, kept) {
  if (total <= kept) return 0n;
  const half = (kept + 1n) / 2n;
  const batch = half > 1n ? half : 1n;
  const evicted = ((total - kept + batch - 1n) / batch) * batch;
  return evicted < total ? evicted : total;
}

function blobOmitted(kept, digest) {
  return `[image omitted from this request, which keeps only the newest ${kept} images: ${OMITTED_REFERENCE_PREFIX}${digest}]`;
}

function keptBlobs(items, attached, kept) {
  const total = BigInt(items.filter((item) => item.v === attached).length);
  let dropped = blobsEvicted(total, kept);
  const out = items.slice();
  for (let index = 0; index < out.length && dropped > 0n; index++) {
    const item = out[index];
    if (item.v !== attached) continue;
    assert(item.f.length === 2, "an attached entry holds its call and its blob");
    out[index] = variant(ENTRY_RESULT, [item.f[0], blobOmitted(kept, item.f[1])]);
    dropped -= 1n;
  }
  assert(BigInt(out.filter((item) => item.v === attached).length) <= kept, "keep blobs leaves at most its count of blobs");
  return out;
}

function foundNames(text) {
  return text.split(SEARCH_SEPARATOR).filter((line) => line !== "");
}

function searchOf(tool) {
  const declared = PROGRAM.tools.find((candidate) => candidate.name === tool);
  return declared === undefined || declared.finds === undefined ? null : declared.finds;
}

function deferredListing(listing) {
  return PROGRAM.tools.some((tool) => tool.finds === listing);
}

function notDiscovered(tool, listing) {
  return `${tool} was not discovered: no search of listing ${listing} found it in this turn`;
}

function notFound(tool, found, listing) {
  return `${tool} found ${rustDebugText(found)}, which listing ${listing} did not list in this turn`;
}

function checkedSearch(tool, listing, admitted, output) {
  if (output.tag !== "ok" || typeof output.value !== "string") return output;
  const declared = PROGRAM.listings[listing];
  const names = foundNames(output.value);
  if (BigInt(names.length) > declared.bound) {
    return { tag: "failed", error: `${tool} found ${names.length} tools, over listing ${declared.name}'s bound ${declared.boundName} (${declared.bound})` };
  }
  for (const found of names) {
    const named = listed(found);
    const ok = named !== null && named.listing === listing && admitted !== undefined && admitted.has(named.name);
    if (!ok) return { tag: "failed", error: notFound(tool, found, declared.name) };
  }
  return output;
}

function listingNotice(listing, before, after) {
  const named = (name) => `${listing}${LISTED_TOOL_SEPARATOR}${name}`;
  const added = [...after.keys()].filter((name) => !before.has(name)).map(named);
  const changed = [...after.keys()].filter((name) => before.has(name) && before.get(name) !== after.get(name)).map(named);
  const removed = [...before.keys()].filter((name) => !after.has(name)).map(named);
  const parts = [["added", added], ["changed", changed], ["removed", removed]]
    .filter(([, names]) => names.length > 0)
    .map(([what, names]) => `${what} ${names.join(", ")}`);
  if (parts.length === 0) return "";
  return `tools of listing ${listing} changed: ${parts.join("; ")}`;
}

function reportsUnknown(type) {
  return type.maybe !== undefined || (PROGRAM.blobs !== undefined && type.sum === PROGRAM.blobs.dispatched);
}

function dispatchedReturns(tool) {
  const declared = PROGRAM.tools.find((candidate) => candidate.name === tool);
  if (PROGRAM.blobs !== undefined && declared !== undefined && declared.returns.blob !== undefined) return declared.returns;
  return "text";
}
