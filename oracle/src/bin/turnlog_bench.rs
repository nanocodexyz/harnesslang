#[path = "../journal.rs"]
mod journal;

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use nanocode_durability_core::{StoreError, StoreFuture};
use nanocode_turnlog::{EffectClass, LogHost, Phase, Point, Record, TurnLog};

const SCOPE: &str = "bench";
const SNAPSHOT_TAG: char = 's';
const ROW_SEPARATOR: char = ' ';
const USAGE_EXIT: u8 = 2;
const FAILURE_EXIT: u8 = 1;
const NANOS_PER_MILLI: f64 = 1_000_000.0;

const RUSAGE_SELF: i32 = 0;
const RUSAGE_WORDS: usize = 18;
const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_MILLI: f64 = 1_000.0;

#[repr(C)]
struct Rusage {
    words: [i64; RUSAGE_WORDS],
}

unsafe extern "C" {
    fn getrusage(who: i32, usage: *mut Rusage) -> i32;
}

fn cpu_ms() -> f64 {
    let mut usage = Rusage {
        words: [0; RUSAGE_WORDS],
    };
    let status = unsafe { getrusage(RUSAGE_SELF, &mut usage) };
    assert_eq!(status, 0, "getrusage of this process succeeds");
    let [user_seconds, user_micros, system_seconds, system_micros, ..] = usage.words;
    let micros = (user_seconds + system_seconds) * MICROS_PER_SECOND + user_micros + system_micros;
    micros as f64 / MICROS_PER_MILLI
}

fn ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the file host and an uncontended lease complete without waiting"),
    }
}

fn milliseconds(elapsed: Duration) -> f64 {
    elapsed.as_nanos() as f64 / NANOS_PER_MILLI
}

struct FileLog {
    path: PathBuf,
    file: Mutex<std::fs::File>,
    syncs: AtomicU64,
    appends: AtomicU64,
}

fn backend(path: &Path, error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(format!("{}: {error}", path.display()))
}

impl FileLog {
    fn open(path: &Path) -> Result<Self, String> {
        let created = !path.exists();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let mut syncs = 0;
        if created {
            let parent = match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("."),
            };
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("{}: {error}", parent.display()))?;
            syncs += 1;
        }
        Ok(Self {
            path: path.to_owned(),
            file: Mutex::new(file),
            syncs: AtomicU64::new(syncs),
            appends: AtomicU64::new(0),
        })
    }

    fn rows(&self) -> Result<Vec<(u64, String)>, StoreError> {
        let text =
            std::fs::read_to_string(&self.path).map_err(|error| backend(&self.path, error))?;
        let mut rows = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let Some((seq, body)) = line.split_once(ROW_SEPARATOR) else {
                return Err(backend(
                    &self.path,
                    format!("line {} has no seq", index + 1),
                ));
            };
            let seq = seq
                .parse()
                .map_err(|error| backend(&self.path, format!("line {}: {error}", index + 1)))?;
            rows.push((seq, body.to_owned()));
        }
        Ok(rows)
    }
}

impl LogHost for FileLog {
    fn scan<'a>(
        &'a self,
        scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        assert_eq!(scope, SCOPE, "the bench log holds one scope");
        let rows = self.rows().map(|rows| {
            rows.into_iter()
                .filter(|(seq, _)| (from..to).contains(seq))
                .collect()
        });
        Box::pin(async move { rows })
    }

    fn load<'a>(
        &'a self,
        scope: &'a str,
    ) -> StoreFuture<'a, Result<Vec<(u64, String)>, StoreError>> {
        assert_eq!(scope, SCOPE, "the bench log holds one scope");
        let rows = self.rows().map(|mut rows| {
            let start = rows
                .iter()
                .rposition(|(_, body)| body.starts_with(SNAPSHOT_TAG))
                .unwrap_or(0);
            rows.split_off(start)
        });
        Box::pin(async move { rows })
    }

    fn append<'a>(
        &'a self,
        scope: &'a str,
        rows: &'a [(u64, String)],
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        assert_eq!(scope, SCOPE, "the bench log holds one scope");
        let mut lines = String::new();
        for (seq, body) in rows {
            assert!(!body.contains('\n'), "a turn log row is one line");
            lines.push_str(&format!("{seq}{ROW_SEPARATOR}{body}\n"));
        }
        let mut file = self
            .file
            .lock()
            .expect("the bench log's lock is not poisoned");
        let written = file
            .write_all(lines.as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|error| backend(&self.path, error));
        self.appends.fetch_add(1, Ordering::Relaxed);
        self.syncs.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move { written })
    }
}

fn ends_flush(record: &Record) -> bool {
    match record {
        Record::Call { effect, .. } => *effect == EffectClass::External,
        Record::Checkpoint { .. } => true,
        _ => false,
    }
}

fn point(flush: &[Record]) -> Point {
    let has = |wanted: fn(&Record) -> bool| flush.iter().any(wanted);
    if has(|record| matches!(record, Record::Status { .. })) {
        Point::Finish
    } else if has(|record| matches!(record, Record::Recovery { .. })) {
        Point::Recovery
    } else if has(|record| matches!(record, Record::Admitted { .. })) {
        Point::Admission
    } else {
        Point::Call
    }
}

fn read_records(path: &Path) -> Result<Vec<Record>, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let bodies = match journal::records(&bytes, true) {
        journal::Lines::Records(bodies) => bodies,
        journal::Lines::Invalid(error) => return Err(format!("{}: {error}", path.display())),
    };
    let mut records = Vec::with_capacity(bodies.len());
    for (index, body) in bodies.iter().enumerate() {
        let record = serde_json::from_str::<Record>(body)
            .map_err(|error| format!("{} record {}: {error}", path.display(), index + 1))?;
        records.push(record);
    }
    if records.is_empty() {
        return Err(format!("{}: the journal holds no records", path.display()));
    }
    Ok(records)
}

fn derived_flushes(records: &[Record]) -> Result<Vec<usize>, String> {
    let mut flushes = Vec::new();
    let mut open = 0;
    for record in records {
        open += 1;
        if ends_flush(record) {
            flushes.push(open);
            open = 0;
        }
    }
    if open != 0 {
        return Err(format!(
            "the journal ends with {open} records after its last durability point"
        ));
    }
    Ok(flushes)
}

fn count_text(count: Option<&usize>) -> String {
    count.map_or_else(|| "no".to_owned(), usize::to_string)
}

fn read_flushes(path: &Path, records: &[Record]) -> Result<Vec<usize>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut sidecar = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let count: usize = line
            .parse()
            .map_err(|error| format!("{} line {}: {error}", path.display(), index + 1))?;
        if count == 0 {
            return Err(format!(
                "{} line {}: a flush carries no records",
                path.display(),
                index + 1
            ));
        }
        sidecar.push(count);
    }
    let carried: usize = sidecar.iter().sum();
    if carried != records.len() {
        return Err(format!(
            "{}: the flushes carry {carried} records, the journal holds {}",
            path.display(),
            records.len()
        ));
    }
    let derived = derived_flushes(records)?;
    let first = sidecar
        .iter()
        .zip(&derived)
        .position(|(said, point)| said != point);
    if let Some(index) =
        first.or((sidecar.len() != derived.len()).then_some(sidecar.len().min(derived.len())))
    {
        return Err(format!(
            "{}: flush {} carries {} records where the journal's durability points put {} ({} flushes against {})",
            path.display(),
            index + 1,
            count_text(sidecar.get(index)),
            count_text(derived.get(index)),
            sidecar.len(),
            derived.len()
        ));
    }
    Ok(sidecar)
}

fn run(journal_path: &Path, flushes_path: &Path, out: &Path, turns: u64) -> Result<(), String> {
    let records = read_records(journal_path)?;
    let flushes = read_flushes(flushes_path, &records)?;
    if out.exists() {
        return Err(format!("{}: the bench starts from no log", out.display()));
    }
    let cpu_started = cpu_ms();
    let started = Instant::now();
    let host = Arc::new(FileLog::open(out)?);
    let log = ready(TurnLog::open(host.clone(), SCOPE)).map_err(|error| error.to_string())?;
    let mut rest = records.as_slice();
    for count in &flushes {
        let (flush, after) = rest.split_at(*count);
        rest = after;
        let mut lease = ready(log.lease());
        for record in flush {
            lease
                .push(record.clone())
                .map_err(|error| error.to_string())?;
        }
        ready(lease.commit(point(flush))).map_err(|error| error.to_string())?;
    }
    let wall = started.elapsed();
    let cpu = cpu_ms() - cpu_started;
    assert!(rest.is_empty(), "the flushes cover every record");
    let stats = ready(log.stats());
    println!(
        "run turns={turns} wall_ms={:.3} cpu_ms={cpu:.3} records={} flushes={} flush_ms={:.3} syncs_counted={} appends={} snapshots={} boundaries={} row_bytes={} snapshot_bytes={} plain_bytes={} retired={}",
        milliseconds(wall),
        records.len(),
        flushes.len(),
        milliseconds(wall),
        host.syncs.load(Ordering::Relaxed),
        host.appends.load(Ordering::Relaxed),
        stats.snapshots,
        stats.boundaries,
        stats.row_bytes,
        stats.snapshot_bytes,
        stats.plain_bytes,
        stats.retired
    );
    Ok(())
}

fn open(out: &Path, turns: u64) -> Result<(), String> {
    if !out.exists() {
        return Err(format!("{}: no log to open", out.display()));
    }
    let cpu_started = cpu_ms();
    let started = Instant::now();
    let host = Arc::new(FileLog::open(out)?);
    let log = ready(TurnLog::open(host, SCOPE)).map_err(|error| error.to_string())?;
    let (finished, state) = ready(log.read(|fold| {
        let finished = (1..=turns)
            .filter(|turn| {
                fold.turn(&turn.to_string())
                    .is_some_and(|turn| matches!(turn.phase, Phase::Finished { .. }))
            })
            .count() as u64;
        let state = fold
            .checkpoints()
            .map(|(_, checkpoint)| serde_json::from_str::<serde_json::Value>(&checkpoint.payload))
            .collect::<Result<Vec<_>, _>>();
        (finished, state)
    }));
    let elapsed = started.elapsed();
    let cpu = cpu_ms() - cpu_started;
    let state = state.map_err(|error| format!("a checkpoint payload is not JSON: {error}"))?;
    if finished != turns {
        return Err(format!(
            "{finished} of {turns} turns are finished in the log"
        ));
    }
    let collected = ready(log.stats()).collected;
    println!(
        "open open_ms={:.3} open_cpu_ms={cpu:.3} checkpoints={} state_bytes={} collected={collected}",
        milliseconds(elapsed),
        state.len(),
        state
            .iter()
            .map(|value| value.to_string().len())
            .sum::<usize>()
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let number = |text: &str| {
        text.parse::<u64>()
            .map_err(|error| format!("{text}: {error}"))
    };
    let result = match words.as_slice() {
        ["run", journal, flushes, out, turns] => number(turns).and_then(|turns| {
            run(
                Path::new(journal),
                Path::new(flushes),
                Path::new(out),
                turns,
            )
        }),
        ["open", out, turns] => number(turns).and_then(|turns| open(Path::new(out), turns)),
        _ => {
            eprintln!("usage: turnlog_bench run JOURNAL FLUSHES OUT TURNS | open OUT TURNS");
            return ExitCode::from(USAGE_EXIT);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("turnlog_bench: error: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}
