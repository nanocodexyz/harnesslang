use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::Bench;
use crate::harness::{self, Disk, FileDisk, JsonlJournal, Ran, TurnRequest};

const SIDECAR_SUFFIX: &str = "flushes";

#[derive(Default)]
struct Counts {
    pending: Option<Instant>,
    flushes: Vec<u64>,
    flush_time: Duration,
    open_syncs: u64,
    syncs: u64,
}

struct Counted {
    file: FileDisk,
    counts: Rc<RefCell<Counts>>,
}

impl Disk for Counted {
    fn length(&mut self) -> std::io::Result<u64> {
        self.file.length()
    }

    fn read_at(&mut self, offset: u64, length: u64) -> std::io::Result<Vec<u8>> {
        self.file.read_at(offset, length)
    }

    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut counts = self.counts.borrow_mut();
        assert!(
            counts.pending.is_none(),
            "a flush syncs before the next one appends"
        );
        counts.pending = Some(Instant::now());
        let lines = bytes
            .iter()
            .filter(|byte| **byte == harness::LINE_END)
            .count();
        assert!(lines > 0, "a flush appends whole lines");
        counts.flushes.push(lines as u64);
        self.file.append(bytes)
    }

    fn sync(&mut self) -> std::io::Result<()> {
        let synced = self.file.sync();
        let mut counts = self.counts.borrow_mut();
        counts.syncs += 1;
        match counts.pending.take() {
            Some(started) => counts.flush_time += started.elapsed(),
            None => counts.open_syncs += 1,
        }
        synced
    }

    fn truncate(&mut self, length: u64) -> std::io::Result<()> {
        self.file.truncate(length)
    }
}

pub struct Session {
    journal: Option<JsonlJournal<'static>>,
    counts: Rc<RefCell<Counts>>,
    directory_syncs: u64,
    opens: u64,
}

impl Session {
    pub fn create(path: &std::path::Path) -> Self {
        assert!(!path.exists(), "the bench starts from no journal");
        let file = FileDisk::open(path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
        let counts = Rc::new(RefCell::new(Counts::default()));
        let disk: &'static mut Counted = Box::leak(Box::new(Counted {
            file,
            counts: Rc::clone(&counts),
        }));
        let journal = JsonlJournal::open(disk).unwrap_or_else(|error| panic!("{error}"));
        Self {
            journal: Some(journal),
            counts,
            directory_syncs: 1,
            opens: 1,
        }
    }

    pub fn turn(&mut self, bench: &mut Bench, request: &TurnRequest) -> Ran {
        let journal = self
            .journal
            .take()
            .expect("the journal is held between turns");
        let (journal, ran) = journal
            .run(bench, request)
            .unwrap_or_else(|error| panic!("turn {}: {error:?}", request.turn));
        self.journal = Some(journal);
        ran
    }

    pub fn report(&self, path: &std::path::Path) -> String {
        let counts = self.counts.borrow();
        assert!(counts.pending.is_none(), "every flush synced");
        let sidecar: String = counts
            .flushes
            .iter()
            .map(|count| format!("{count}\n"))
            .collect();
        let sidecar_path = path.with_extension(SIDECAR_SUFFIX);
        std::fs::write(&sidecar_path, sidecar)
            .unwrap_or_else(|error| panic!("{sidecar_path:?}: {error}"));
        let records: u64 = counts.flushes.iter().sum();
        format!(
            "records={records} flushes={} flush_ms={:.3} opens={} open_syncs={} directory_syncs={} syncs_counted={}",
            counts.flushes.len(),
            crate::milliseconds(counts.flush_time),
            self.opens,
            counts.open_syncs,
            self.directory_syncs,
            counts.syncs + self.directory_syncs
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
    let opened = harness::jsonl_open_tail(&mut disk).unwrap_or_else(|error| panic!("{error}"));
    let session = harness::fold_from(opened.start, &opened.records)
        .unwrap_or_else(|error| panic!("the journal's tail folds: {error}"));
    Opened {
        session,
        records: opened.records.len(),
        report: format!(
            "dropped_bytes={} bytes_read={}",
            opened.dropped_bytes, disk.bytes_read
        ),
    }
}
