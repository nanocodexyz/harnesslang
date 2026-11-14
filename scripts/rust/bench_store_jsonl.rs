use std::time::{Duration, Instant};

use crate::Bench;
use crate::harness::{self, Disk, FileDisk, Ran, TurnRequest};

const SIDECAR_SUFFIX: &str = "flushes";

struct Counted {
    file: FileDisk,
    pending: Option<Instant>,
    flushes: Vec<u64>,
    flush_time: Duration,
    open_syncs: u64,
}

impl Disk for Counted {
    fn contents(&mut self) -> std::io::Result<Vec<u8>> {
        self.file.contents()
    }

    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        assert!(
            self.pending.is_none(),
            "a flush syncs before the next one appends"
        );
        self.pending = Some(Instant::now());
        let lines = bytes
            .iter()
            .filter(|byte| **byte == harness::LINE_END)
            .count();
        assert!(lines > 0, "a flush appends whole lines");
        self.flushes.push(lines as u64);
        self.file.append(bytes)
    }

    fn sync(&mut self) -> std::io::Result<()> {
        let synced = self.file.sync();
        match self.pending.take() {
            Some(started) => self.flush_time += started.elapsed(),
            None => self.open_syncs += 1,
        }
        synced
    }

    fn truncate(&mut self, length: u64) -> std::io::Result<()> {
        self.file.truncate(length)
    }
}

pub struct Session {
    disk: Counted,
    directory_syncs: u64,
    opens: u64,
}

impl Session {
    pub fn create(path: &std::path::Path) -> Self {
        assert!(!path.exists(), "the bench starts from no journal");
        let file = FileDisk::open(path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
        Self {
            disk: Counted {
                file,
                pending: None,
                flushes: Vec::new(),
                flush_time: Duration::ZERO,
                open_syncs: 0,
            },
            directory_syncs: 1,
            opens: 0,
        }
    }

    pub fn turn(&mut self, bench: &mut Bench, request: &TurnRequest) -> Ran {
        self.opens += 1;
        harness::run_jsonl(bench, &mut self.disk, request)
            .unwrap_or_else(|error| panic!("turn {}: {error:?}", request.turn))
    }

    pub fn report(&self, path: &std::path::Path) -> String {
        let disk = &self.disk;
        assert!(disk.pending.is_none(), "every flush synced");
        let sidecar: String = disk
            .flushes
            .iter()
            .map(|count| format!("{count}\n"))
            .collect();
        let sidecar_path = path.with_extension(SIDECAR_SUFFIX);
        std::fs::write(&sidecar_path, sidecar)
            .unwrap_or_else(|error| panic!("{sidecar_path:?}: {error}"));
        let records: u64 = disk.flushes.iter().sum();
        format!(
            "records={records} flushes={} flush_ms={:.3} opens={} open_syncs={} directory_syncs={} syncs_counted={}",
            disk.flushes.len(),
            crate::milliseconds(disk.flush_time),
            self.opens,
            disk.open_syncs,
            self.directory_syncs,
            disk.file.syncs + self.directory_syncs
        )
    }
}

pub struct Opened {
    pub session: harness::Session,
    pub records: usize,
    pub report: String,
}

pub fn open(path: &std::path::Path) -> Opened {
    assert!(path.exists(), "the journal to open exists");
    let mut disk = FileDisk::open(path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    let opened = harness::jsonl_open(&mut disk).unwrap_or_else(|error| panic!("{error}"));
    let session =
        harness::fold(&opened.records).unwrap_or_else(|error| panic!("the journal folds: {error}"));
    Opened {
        session,
        records: opened.records.len(),
        report: format!("dropped_bytes={}", opened.dropped_bytes),
    }
}
