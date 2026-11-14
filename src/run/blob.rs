use std::collections::BTreeMap;

use crate::ir::{self, Bound, Program, Type};
use crate::run::journal::Output;
use crate::run::json::Json;
use crate::run::session::{Session, SiteLog};
use crate::run::value::Value;

pub const BLOB_KEY: &str = "blob";
pub const DATA_KEY: &str = "data";
pub const DIGEST_BYTES: usize = 32;
pub const DIGEST_HEX_LEN: usize = 2 * DIGEST_BYTES;
pub const OMITTED_REFERENCE_PREFIX: &str = "sha256:";

const SHA256_BLOCK_BYTES: usize = 64;
const SHA256_LENGTH_BYTES: usize = 8;
const SHA256_ROUNDS: usize = 64;
const SHA256_INITIAL: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];
const SHA256_ROUND_CONSTANTS: [u32; SHA256_ROUNDS] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];
pub const SHA256_CHECK_INPUT: &[u8] = b"abc";
pub const SHA256_CHECK: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn sha256_block(hash: &mut [u32; 8], block: &[u8]) {
    assert_eq!(
        block.len(),
        SHA256_BLOCK_BYTES,
        "sha-256 compresses whole blocks"
    );
    let mut words = [0_u32; SHA256_ROUNDS];
    for (index, chunk) in block.chunks_exact(4).enumerate() {
        words[index] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for index in 16..SHA256_ROUNDS {
        let early = words[index - 15];
        let late = words[index - 2];
        let small0 = early.rotate_right(7) ^ early.rotate_right(18) ^ (early >> 3);
        let small1 = late.rotate_right(17) ^ late.rotate_right(19) ^ (late >> 10);
        words[index] = words[index - 16]
            .wrapping_add(small0)
            .wrapping_add(words[index - 7])
            .wrapping_add(small1);
    }
    let mut state = *hash;
    for index in 0..SHA256_ROUNDS {
        let [a, b, c, d, e, f, g, h] = state;
        let big1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choice = (e & f) ^ (!e & g);
        let first = h
            .wrapping_add(big1)
            .wrapping_add(choice)
            .wrapping_add(SHA256_ROUND_CONSTANTS[index])
            .wrapping_add(words[index]);
        let big0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let second = big0.wrapping_add(majority);
        state = [
            first.wrapping_add(second),
            a,
            b,
            c,
            d.wrapping_add(first),
            e,
            f,
            g,
        ];
    }
    for (slot, value) in hash.iter_mut().zip(state) {
        *slot = slot.wrapping_add(value);
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hash = SHA256_INITIAL;
    let mut blocks = bytes.chunks_exact(SHA256_BLOCK_BYTES);
    for block in blocks.by_ref() {
        sha256_block(&mut hash, block);
    }
    let rest = blocks.remainder();
    let mut tail = Vec::with_capacity(2 * SHA256_BLOCK_BYTES);
    tail.extend_from_slice(rest);
    tail.push(0x80);
    while tail.len() % SHA256_BLOCK_BYTES != SHA256_BLOCK_BYTES - SHA256_LENGTH_BYTES {
        tail.push(0);
    }
    let bits = (bytes.len() as u64).wrapping_mul(8);
    tail.extend_from_slice(&bits.to_be_bytes());
    for block in tail.chunks_exact(SHA256_BLOCK_BYTES) {
        sha256_block(&mut hash, block);
    }
    let mut out = String::with_capacity(DIGEST_HEX_LEN);
    for word in hash {
        for byte in word.to_be_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64_PAD: u8 = b'=';
const BASE64_GROUP_BYTES: usize = 3;
const BASE64_GROUP_CHARS: usize = 4;

pub fn base64_len(bytes: u64) -> Option<u64> {
    bytes
        .div_ceil(BASE64_GROUP_BYTES as u64)
        .checked_mul(BASE64_GROUP_CHARS as u64)
}

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out =
        String::with_capacity(bytes.len().div_ceil(BASE64_GROUP_BYTES) * BASE64_GROUP_CHARS);
    for group in bytes.chunks(BASE64_GROUP_BYTES) {
        let mut padded = [0_u8; BASE64_GROUP_BYTES];
        padded[..group.len()].copy_from_slice(group);
        let joined = u32::from(padded[0]) << 16 | u32::from(padded[1]) << 8 | u32::from(padded[2]);
        for index in 0..BASE64_GROUP_CHARS {
            if index <= group.len() {
                let sextet = (joined >> (18 - 6 * index)) & 0x3f;
                out.push(char::from(BASE64_ALPHABET[sextet as usize]));
            } else {
                out.push(char::from(BASE64_PAD));
            }
        }
    }
    out
}

fn sextet(byte: u8) -> Option<u32> {
    BASE64_ALPHABET
        .iter()
        .position(|candidate| *candidate == byte)
        .map(|index| index as u32)
}

pub fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if bytes.len() % BASE64_GROUP_CHARS != 0 {
        return Err(format!(
            "{} characters, not a multiple of {BASE64_GROUP_CHARS}",
            bytes.len()
        ));
    }
    let mut out = Vec::with_capacity(bytes.len() / BASE64_GROUP_CHARS * BASE64_GROUP_BYTES);
    let groups = bytes.len() / BASE64_GROUP_CHARS;
    for (index, group) in bytes.chunks_exact(BASE64_GROUP_CHARS).enumerate() {
        let padding = group
            .iter()
            .rev()
            .take_while(|byte| **byte == BASE64_PAD)
            .count();
        if padding > 2 || (padding > 0 && index + 1 != groups) {
            return Err("padding inside the text".to_owned());
        }
        let mut joined = 0_u32;
        for byte in &group[..BASE64_GROUP_CHARS - padding] {
            let value =
                sextet(*byte).ok_or_else(|| format!("character {:?}", char::from(*byte)))?;
            joined = joined << 6 | value;
        }
        joined <<= 6 * padding;
        let decoded = joined.to_be_bytes();
        let kept = BASE64_GROUP_BYTES - padding;
        if decoded[1 + kept..].iter().any(|byte| *byte != 0) {
            return Err("nonzero bits after the last byte".to_owned());
        }
        out.extend_from_slice(&decoded[1..=kept]);
    }
    Ok(out)
}

pub fn is_digest(text: &str) -> bool {
    text.len() == DIGEST_HEX_LEN
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn value_json(digest: &str) -> Json {
    Json::object([(BLOB_KEY, Json::text(digest))])
}

pub fn digest_of(json: &Json) -> Result<&str, String> {
    let Json::Object(map) = json else {
        return Err(format!("expected a blob, found {}", json.encode()));
    };
    let digest = map
        .get(BLOB_KEY)
        .and_then(Json::as_str)
        .filter(|digest| is_digest(digest))
        .ok_or_else(|| format!("expected a blob's sha-256 digest, found {}", json.encode()))?;
    if map.keys().any(|key| key != BLOB_KEY && key != DATA_KEY) {
        return Err(format!(
            "a blob holds its digest and data, found {}",
            json.encode()
        ));
    }
    Ok(digest)
}

pub fn returned_bound<'p>(program: &'p Program, tool: &str) -> Option<&'p Bound> {
    match &program.tool(tool)?.1.returns {
        Type::Blob(bytes) => Some(bytes),
        _ => None,
    }
}

pub fn checked(tool: &str, bytes: &Bound, output: Output) -> Output {
    let Output::Ok(json) = &output else {
        return output;
    };
    let Some(text) = json.as_str() else {
        return Output::Failed(format!(
            "{tool} returned {}, which is not a blob: blobs are base64 text",
            json.encode()
        ));
    };
    match base64_decode(text) {
        Err(error) => Output::Failed(format!(
            "{tool} returned text that is not base64 ({error}), which is not a blob"
        )),
        Ok(decoded) if decoded.len() as u64 > bytes.value => Output::Failed(format!(
            "{tool} returned a blob of {} bytes, over its bound {} ({})",
            decoded.len(),
            bytes.name,
            bytes.value
        )),
        Ok(_) => output,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blobs {
    held: BTreeMap<String, Vec<u8>>,
}

impl Blobs {
    pub fn get(&self, digest: &str) -> Option<&[u8]> {
        self.held.get(digest).map(Vec::as_slice)
    }

    pub fn holds(&self, digest: &str) -> bool {
        self.held.contains_key(digest)
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    pub fn journaled(&mut self, base64: &str, with_data: bool) -> Json {
        let bytes = base64_decode(base64).expect("a checked blob is base64");
        let digest = sha256_hex(&bytes);
        let fresh = !self.held.contains_key(&digest);
        let journaled = if fresh && with_data {
            Json::object([
                (BLOB_KEY, Json::text(&digest)),
                (DATA_KEY, Json::text(base64)),
            ])
        } else {
            value_json(&digest)
        };
        self.held.entry(digest).or_insert(bytes);
        journaled
    }

    pub fn read(&mut self, json: &Json) -> Result<String, String> {
        let digest = digest_of(json)?.to_owned();
        let Json::Object(map) = json else {
            unreachable!("digest_of accepts only objects");
        };
        match map.get(DATA_KEY) {
            Some(data) => {
                let data = data
                    .as_str()
                    .ok_or_else(|| format!("blob {digest} has data {}, not text", data.encode()))?;
                let bytes = base64_decode(data).map_err(|error| {
                    format!("blob {digest} has data that is not base64: {error}")
                })?;
                let hashed = sha256_hex(&bytes);
                if hashed != digest {
                    return Err(format!("blob {digest} holds data whose digest is {hashed}"));
                }
                self.held.entry(digest.clone()).or_insert(bytes);
            }
            None if !self.held.contains_key(&digest) => {
                return Err(format!(
                    "blob {digest} is journaled without its data, and no earlier record holds it"
                ));
            }
            None => {}
        }
        Ok(digest)
    }

    pub fn of_session(program: &Program, session: &Session) -> Result<Self, String> {
        let mut blobs = Self::default();
        if program.blobs.is_none() {
            return Ok(blobs);
        }
        for (turn, log) in &session.turns {
            for (seq, site) in &log.sites {
                let SiteLog::Done(call, Output::Ok(json)) = site else {
                    continue;
                };
                if returned_bound(program, &call.tool).is_none() {
                    continue;
                }
                blobs
                    .read(json)
                    .map_err(|error| format!("result {turn}/{seq}: {error}"))?;
            }
        }
        Ok(blobs)
    }

    pub fn check_state(&self, program: &Program, state: &[Value]) -> Result<(), String> {
        let mut digests = Vec::new();
        for value in state {
            collect(value, &mut digests);
        }
        match digests.into_iter().find(|digest| !self.holds(digest)) {
            Some(digest) => Err(format!(
                "the state holds blob {digest}, which no result of the journal holds"
            )),
            None => {
                assert!(
                    program.blobs.is_some() || self.is_empty(),
                    "only a program with blob tools holds blobs"
                );
                Ok(())
            }
        }
    }
}

pub fn collect(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Blob(digest) => out.push(digest.clone()),
        Value::List(items) => items.iter().for_each(|item| collect(item, out)),
        Value::Variant(_, fields) => fields.iter().for_each(|field| collect(field, out)),
        Value::U64(_) | Value::Bool(_) | Value::Text(_) => {}
    }
}

pub fn evicted(total: u64, kept: u64) -> u64 {
    if total <= kept {
        return 0;
    }
    let batch = kept.div_ceil(2).max(1);
    ((total - kept).div_ceil(batch) * batch).min(total)
}

pub fn omitted(kept: u64, digest: &str) -> String {
    format!(
        "[image omitted from this request, which keeps only the newest {kept} images: {OMITTED_REFERENCE_PREFIX}{digest}]"
    )
}

pub fn keep_blobs(items: &mut [Value], attached: ir::VariantId, kept: u64, evict: bool) {
    let total = items
        .iter()
        .filter(|item| matches!(item, Value::Variant(variant, _) if *variant == attached))
        .count() as u64;
    let mut dropped = if evict { evicted(total, kept) } else { 0 };
    for item in items.iter_mut() {
        if dropped == 0 {
            break;
        }
        let Value::Variant(variant, fields) = item else {
            panic!("a context holds entries, found {item:?}");
        };
        if *variant != attached {
            continue;
        }
        let [call, Value::Blob(digest)] = fields.as_slice() else {
            panic!("an attached entry holds its call and its blob, found {fields:?}");
        };
        *item = Value::Variant(
            ir::ENTRY_RESULT,
            vec![call.clone(), Value::Text(omitted(kept, digest))],
        );
        dropped -= 1;
    }
    let left = items
        .iter()
        .filter(|item| matches!(item, Value::Variant(variant, _) if *variant == attached))
        .count() as u64;
    assert!(
        !evict || left <= kept,
        "keep blobs leaves at most its count of blobs"
    );
}

pub fn digested(output: Output) -> Output {
    match &output {
        Output::Ok(Json::Object(map)) if map.contains_key(DATA_KEY) => {
            let digest = map
                .get(BLOB_KEY)
                .and_then(Json::as_str)
                .expect("a journaled blob holds its digest");
            Output::Ok(value_json(digest))
        }
        _ => output,
    }
}

pub fn expected(returns: &Type, output: Output) -> Output {
    match (returns, &output) {
        (Type::Blob(_), Output::Ok(Json::String(data))) => {
            let bytes = base64_decode(data).expect("a checked blob is base64");
            Output::Ok(value_json(&sha256_hex(&bytes)))
        }
        _ => output,
    }
}
