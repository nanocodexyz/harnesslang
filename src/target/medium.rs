use std::collections::BTreeMap;

use crate::run::journal::{self, LINE_END, Record};
use crate::run::mutation::Mutation;
use crate::target::{BLOCK_DEVICE_SECTOR_BYTES, Target};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JsonlFile {
    pub written: Vec<u8>,
    pub synced: usize,
    pub checksum: bool,
}

impl JsonlFile {
    pub fn write(&mut self, bytes: &[u8]) {
        self.written.extend_from_slice(bytes);
    }

    pub fn sync(&mut self) {
        self.synced = self.written.len();
    }

    pub fn unsynced(&self) -> usize {
        assert!(
            self.synced <= self.written.len(),
            "synced bytes were written"
        );
        self.written.len() - self.synced
    }

    pub fn lose_unsynced_prefix(&mut self, kept: usize) {
        assert!(
            kept <= self.unsynced(),
            "a power loss keeps at most the unsynced bytes"
        );
        self.written.truncate(self.synced + kept);
    }

    pub fn unsynced_sectors(&self) -> usize {
        let first = self.synced / BLOCK_DEVICE_SECTOR_BYTES;
        let end = self.written.len().div_ceil(BLOCK_DEVICE_SECTOR_BYTES);
        end.saturating_sub(first)
    }

    pub fn lose_unsynced_sectors(&mut self, persisted: &[bool]) {
        assert_eq!(
            persisted.len(),
            self.unsynced_sectors(),
            "one decision per unsynced sector"
        );
        let first = self.synced / BLOCK_DEVICE_SECTOR_BYTES;
        let mut length = self.synced;
        for (index, kept) in persisted.iter().enumerate() {
            let start = ((first + index) * BLOCK_DEVICE_SECTOR_BYTES).max(self.synced);
            let end = ((first + index + 1) * BLOCK_DEVICE_SECTOR_BYTES).min(self.written.len());
            if *kept {
                length = end;
            } else {
                self.written[start..end].fill(0);
            }
        }
        self.written.truncate(length);
    }

    pub fn open(&mut self) -> Result<Vec<Record>, String> {
        self.sync();
        let decoded = journal::decode_lines(&self.written, self.checksum)?;
        self.written.truncate(decoded.valid_bytes);
        self.sync();
        Ok(decoded.records)
    }

    pub fn durable(&self) -> Result<Vec<Record>, String> {
        let decoded = journal::decode_lines(&self.written[..self.synced], self.checksum)?;
        Ok(decoded.records)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    pub committed: Vec<String>,
    pub epoch: u64,
    pub attempts: BTreeMap<(u64, u64), u64>,
    pub nested_attempts: BTreeMap<(u64, u64, u64), u64>,
}

impl Rows {
    pub fn commit(&mut self, records: &[Record]) {
        for record in records {
            self.committed.push(journal::encode_line(record, false));
        }
    }

    pub fn fenced(&self, epoch: u64) -> bool {
        epoch != self.epoch
    }

    pub fn take_over(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    pub fn attempt(&mut self, turn: u64, seq: u64) -> u64 {
        let attempts = self.attempts.entry((turn, seq)).or_insert(0);
        *attempts += 1;
        *attempts
    }

    pub fn nested_attempt(&mut self, turn: u64, seq: u64, nested: u64) -> u64 {
        let attempts = self.nested_attempts.entry((turn, seq, nested)).or_insert(0);
        *attempts += 1;
        *attempts
    }

    pub fn open(&self) -> Result<Vec<Record>, String> {
        let mut records = Vec::with_capacity(self.committed.len());
        for (index, line) in self.committed.iter().enumerate() {
            records.push(
                journal::decode_line(line, false)
                    .map_err(|error| format!("row {}: {error}", index + 1))?,
            );
        }
        Ok(records)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Medium {
    Jsonl(JsonlFile),
    Rows(Rows),
}

impl Medium {
    pub fn new(target: Target, mutation: Mutation) -> Self {
        match target {
            Target::NativeJsonl => Self::Jsonl(JsonlFile {
                checksum: mutation != Mutation::JsonlNoChecksum,
                ..JsonlFile::default()
            }),
            Target::NativeSqlite | Target::ServerPostgres | Target::Cloudflare | Target::Vercel => {
                Self::Rows(Rows::default())
            }
        }
    }

    pub fn open(&mut self) -> Result<Vec<Record>, String> {
        match self {
            Self::Jsonl(file) => file.open(),
            Self::Rows(rows) => rows.open(),
        }
    }

    pub fn durable(&self) -> Result<Vec<Record>, String> {
        match self {
            Self::Jsonl(file) => file.durable(),
            Self::Rows(rows) => rows.open(),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Self::Jsonl(file) => file.written.clone(),
            Self::Rows(rows) => {
                let mut bytes = Vec::new();
                for line in &rows.committed {
                    bytes.extend_from_slice(line.as_bytes());
                    bytes.push(LINE_END);
                }
                bytes
            }
        }
    }
}
