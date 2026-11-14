use crc::{CRC_32_ISO_HDLC, Crc};
use serde::Deserialize;
use serde_json::value::RawValue;

const LINE_END: u8 = b'\n';
const CHECKSUM: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);
const CRC32_CHECK_INPUT: &[u8] = b"123456789";
const CRC32_CHECK: u32 = 0xcbf4_3926;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    crc: u32,
    #[serde(borrow)]
    record: &'a RawValue,
}

pub enum Lines {
    Records(Vec<String>),
    Invalid(String),
}

fn checked(line: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(line).ok()?;
    let envelope: Envelope<'_> = serde_json::from_str(text).ok()?;
    let body = envelope.record.get();
    (CHECKSUM.checksum(body.as_bytes()) == envelope.crc).then(|| body.to_owned())
}

pub fn records(bytes: &[u8], checksum: bool) -> Lines {
    assert_eq!(
        CHECKSUM.checksum(CRC32_CHECK_INPUT),
        CRC32_CHECK,
        "the crc crate's CRC-32/ISO-HDLC matches its catalogued check value"
    );
    let mut records = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let Some(length) = rest.iter().position(|byte| *byte == LINE_END) else {
            if checksum {
                break;
            }
            return Lines::Invalid(format!("line {} is not terminated", records.len() + 1));
        };
        let line = &rest[..length];
        rest = &rest[length + 1..];
        if checksum {
            match checked(line) {
                Some(body) => records.push(body),
                None => break,
            }
            continue;
        }
        match std::str::from_utf8(line) {
            Ok(text) if serde_json::from_str::<&RawValue>(text).is_ok() => {
                records.push(text.to_owned());
            }
            _ => return Lines::Invalid(format!("line {} is not JSON", records.len() + 1)),
        }
    }
    Lines::Records(records)
}
