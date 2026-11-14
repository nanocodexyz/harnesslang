pub const PAGE_BYTES: u64 = 4096;
const CHECKPOINT_OPENING: &[u8] = b"{\"checkpoint\":";
const CELL_OPENING: &[u8] = b",\"record\":{\"cell\":{\"cell\":";

pub trait Disk {
    fn length(&mut self) -> std::io::Result<u64>;
    fn read_at(&mut self, offset: u64, length: u64) -> std::io::Result<Vec<u8>>;
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    fn sync(&mut self) -> std::io::Result<()>;
    fn truncate(&mut self, length: u64) -> std::io::Result<()>;
}

#[derive(Debug)]
pub enum OpenError {
    Io(std::io::Error),
    Corrupt(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "journal: {error}"),
            Self::Corrupt(error) => write!(formatter, "journal: {error}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub start: Start,
    pub records: Vec<Record>,
    pub dropped_bytes: usize,
}

pub fn jsonl_open(disk: &mut dyn Disk) -> Result<Opened, OpenError> {
    disk.sync().map_err(OpenError::Io)?;
    let length = disk.length().map_err(OpenError::Io)?;
    let bytes = disk.read_at(0, length).map_err(OpenError::Io)?;
    jsonl_keep(disk, 0, decode_checksummed_lines(&bytes), Start::Beginning)
}

pub fn jsonl_open_tail(disk: &mut dyn Disk) -> Result<Opened, OpenError> {
    if program::HOLDS_BLOBS {
        return jsonl_open(disk);
    }
    disk.sync().map_err(OpenError::Io)?;
    let length = disk.length().map_err(OpenError::Io)?;
    let mut offset = length - length.min(PAGE_BYTES);
    let mut bytes = disk
        .read_at(offset, length - offset)
        .map_err(OpenError::Io)?;
    loop {
        match trusted_checkpoint(&bytes, offset == 0) {
            Scan::Found(start) => {
                let decoded = decode_checksummed_lines(&bytes[start..]);
                return match decoded.records.first() {
                    Some(Record::Checkpoint { revision, .. }) => {
                        let revision = *revision;
                        let next_cell = next_cell_before(disk, offset + start as u64)?;
                        jsonl_keep(
                            disk,
                            offset + start as u64,
                            decoded,
                            Start::Checkpoint { revision, next_cell },
                        )
                    }
                    _ => jsonl_open(disk),
                };
            }
            Scan::Absent => {
                assert_eq!(
                    offset, 0,
                    "a journal without a trusted checkpoint is read whole"
                );
                return jsonl_keep(disk, 0, decode_checksummed_lines(&bytes), Start::Beginning);
            }
            Scan::Wider => {
                assert!(offset > 0, "a wider read has more of the journal to read");
                let wider = offset - offset.min(length - offset);
                let mut earlier = disk.read_at(wider, offset - wider).map_err(OpenError::Io)?;
                earlier.extend_from_slice(&bytes);
                bytes = earlier;
                offset = wider;
            }
        }
    }
}

pub fn jsonl_open_for(disk: &mut dyn Disk, turn: u64) -> Result<Opened, OpenError> {
    let opened = jsonl_open_tail(disk)?;
    match opened.start {
        Start::Checkpoint { revision, .. } if turn <= revision => jsonl_open(disk),
        Start::Checkpoint { .. } | Start::Beginning => Ok(opened),
    }
}

fn jsonl_keep(
    disk: &mut dyn Disk,
    offset: u64,
    decoded: Decoded,
    start: Start,
) -> Result<Opened, OpenError> {
    if decoded.dropped_bytes > 0 {
        disk.truncate(offset + decoded.valid_bytes as u64)
            .map_err(OpenError::Io)?;
        disk.sync().map_err(OpenError::Io)?;
    }
    if let Start::Checkpoint { revision, .. } = start {
        let first = match decoded.records.first() {
            Some(Record::Checkpoint { revision, .. }) => Some(*revision),
            _ => None,
        };
        assert_eq!(
            first,
            Some(revision),
            "a tail starts with the checkpoint found for it"
        );
    }
    Ok(Opened {
        start,
        records: decoded.records,
        dropped_bytes: decoded.dropped_bytes,
    })
}

enum CellScan {
    Found(u64),
    Absent,
    Wider,
}

fn next_cell_before(disk: &mut dyn Disk, end: u64) -> Result<u64, OpenError> {
    if program::CODES.is_empty() {
        return Ok(FIRST_CELL);
    }
    let mut start = end - end.min(PAGE_BYTES);
    let mut bytes = disk.read_at(start, end - start).map_err(OpenError::Io)?;
    loop {
        match last_cell(&bytes, start == 0)? {
            CellScan::Found(cell) => return Ok(cell + 1),
            CellScan::Absent => return Ok(FIRST_CELL),
            CellScan::Wider => {
                assert!(start > 0, "a wider read has more of the journal to read");
                let wider = start - start.min(end - start);
                let mut earlier = disk.read_at(wider, start - wider).map_err(OpenError::Io)?;
                earlier.extend_from_slice(&bytes);
                bytes = earlier;
                start = wider;
            }
        }
    }
}

fn last_cell(bytes: &[u8], from_start: bool) -> Result<CellScan, OpenError> {
    let found = bytes
        .windows(CELL_OPENING.len())
        .rposition(|window| window == CELL_OPENING);
    let Some(at) = found else {
        return Ok(if from_start { CellScan::Absent } else { CellScan::Wider });
    };
    let line_start = match bytes[..at].iter().rposition(|byte| *byte == LINE_END) {
        Some(newline) => newline + 1,
        None if from_start => 0,
        None => return Ok(CellScan::Wider),
    };
    let corrupt = |what: String| OpenError::Corrupt(format!("the cell line before the checkpoint {what}"));
    let length = bytes[at..]
        .iter()
        .position(|byte| *byte == LINE_END)
        .ok_or_else(|| corrupt("has no end".to_owned()))?;
    let line = std::str::from_utf8(&bytes[line_start..at + length]).map_err(|_| corrupt("is not utf-8".to_owned()))?;
    match decode_line(line, true) {
        Ok(Record::Cell { cell, .. }) => Ok(CellScan::Found(cell)),
        Ok(other) => Err(corrupt(format!("decodes as a {} record", other.tag()))),
        Err(error) => Err(corrupt(error)),
    }
}

enum Scan {
    Found(usize),
    Absent,
    Wider,
}

fn trusted_checkpoint(bytes: &[u8], from_start: bool) -> Scan {
    let Some(mut end) = bytes.iter().rposition(|byte| *byte == LINE_END) else {
        return if from_start {
            Scan::Absent
        } else {
            Scan::Wider
        };
    };
    loop {
        let start = match bytes[..end].iter().rposition(|byte| *byte == LINE_END) {
            Some(newline) => newline + 1,
            None if from_start => 0,
            None => return Scan::Wider,
        };
        if end + 1 < bytes.len() && checkpoint_line(&bytes[start..end]) {
            return Scan::Found(start);
        }
        if start == 0 {
            return Scan::Absent;
        }
        end = start - 1;
    }
}

fn checkpoint_line(line: &[u8]) -> bool {
    let Some(rest) = line.strip_prefix(LINE_PREFIX.as_bytes()) else {
        return false;
    };
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    let body = rest[digits..]
        .strip_prefix(LINE_MIDDLE.as_bytes())
        .and_then(|body| body.strip_suffix(LINE_SUFFIX.as_bytes()));
    let Some(body) = body.filter(|body| body.starts_with(CHECKPOINT_OPENING)) else {
        return false;
    };
    let crc = std::str::from_utf8(&rest[..digits])
        .ok()
        .and_then(|digits| digits.parse::<u32>().ok());
    crc == Some(crc32(body))
}

pub fn jsonl_self_check() {
    let record = Record::Checkpoint {
        revision: FIRST_TURN,
        state: program::HARNESS_NAME.to_owned(),
        payload: program::state_json(&program::State::default()).encode(),
    };
    let line = encode_line(&record, true);
    assert!(
        checkpoint_line(line.as_bytes()),
        "a checkpoint line is found by its opening"
    );
    let status = Record::Status {
        turn: FIRST_TURN,
        last: FIRST_SEQ,
        outcome: Outcome::Completed,
        result: Json::Null,
    };
    assert!(
        !checkpoint_line(encode_line(&status, true).as_bytes()),
        "a status line is not taken for a checkpoint"
    );
    let mut torn = line.into_bytes();
    torn.pop();
    assert!(
        !checkpoint_line(&torn),
        "a checkpoint line without its envelope's end is not trusted"
    );
}

pub fn jsonl_flush(disk: &mut dyn Disk, records: &[Record]) -> Result<(), Stop> {
    assert!(!records.is_empty(), "a flush carries at least one record");
    let bytes = encode_lines(records, true);
    disk.append(&bytes).map_err(|_| Stop::Crash)?;
    disk.sync().map_err(|_| Stop::Crash)?;
    Ok(())
}

pub struct FileDisk {
    file: std::fs::File,
    pub syncs: u64,
    pub appends: u64,
    pub bytes_read: u64,
}

impl FileDisk {
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        let created = !path.exists();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        if created {
            let parent = match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => std::path::Path::new("."),
            };
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(Self {
            file,
            syncs: 0,
            appends: 0,
            bytes_read: 0,
        })
    }
}

impl Disk for FileDisk {
    fn length(&mut self) -> std::io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    fn read_at(&mut self, offset: u64, length: u64) -> std::io::Result<Vec<u8>> {
        use std::io::{Read, Seek};
        let mut bytes = vec![0; length as usize];
        self.file.seek(std::io::SeekFrom::Start(offset))?;
        self.file.read_exact(&mut bytes)?;
        self.bytes_read += bytes.len() as u64;
        Ok(bytes)
    }

    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        self.appends += 1;
        self.file.write_all(bytes)
    }

    fn sync(&mut self) -> std::io::Result<()> {
        self.syncs += 1;
        self.file.sync_data()
    }

    fn truncate(&mut self, length: u64) -> std::io::Result<()> {
        self.file.set_len(length)
    }
}

pub struct JsonlWorld<'a, H: Host> {
    pub host: &'a mut H,
    pub disk: &'a mut dyn Disk,
    pub flushed: &'a mut Vec<Record>,
}

impl<H: Host> World for JsonlWorld<'_, H> {
    fn hook(&mut self, point: Point) -> Result<(), Stop> {
        self.host.hook(point)
    }

    fn flush(&mut self, _epoch: u64, records: &[Record]) -> Result<(), Stop> {
        jsonl_flush(self.disk, records)?;
        self.flushed.extend_from_slice(records);
        Ok(())
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        self.host.tool(request)
    }

    fn listing(&mut self, request: &ListingRequest<'_>) -> Output {
        self.host.listing(request)
    }

    fn tools(&mut self, requests: &[ToolRequest<'_>], limit: Option<u64>) -> Result<Vec<(usize, Output)>, Stop> {
        host_tools(self.host, requests, limit)
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        self.host.model(request)
    }

    fn now(&mut self) -> u64 {
        self.host.now()
    }

    fn random(&mut self) -> u64 {
        self.host.random()
    }

    fn acknowledge(&mut self, finish: &Finish) {
        self.host.acknowledge(finish)
    }

    fn cell_start(&mut self, cell: u64, script: &str) -> Step {
        host_cell_start(self.host, cell, script)
    }

    fn cell_resume(&mut self, cell: u64, input: Input) -> Step {
        host_cell_resume(self.host, cell, input)
    }

    fn wait(&mut self, wait: &Wait) -> Result<(), Stop> {
        host_wait(self.host, wait)
    }

    fn observe(&mut self, _event: &CellEvent) {}

    fn inbox(&mut self, turn: u64) -> Vec<Delivery> {
        self.host.inbox(turn)
    }

    fn answer(&mut self, turn: u64, position: u64, answers: &[(String, EventAnswer)]) {
        self.host.answer(turn, position, answers)
    }

    fn clock(&mut self) -> u64 {
        self.host.clock()
    }

    fn sleep(&mut self, turn: u64, seq: u64, until_ms: u64) -> Result<u64, Stop> {
        self.host.sleep(turn, seq, until_ms)
    }

    fn resume(&mut self, suspension: &Suspension) -> Option<Resume> {
        self.host.resume(suspension)
    }

    fn resumed(&mut self, suspension: &Suspension, resume: &Resume, matched: bool) {
        self.host.resumed(suspension, resume, matched)
    }
}

#[derive(Debug)]
pub enum RunError {
    Open(OpenError),
    Exit(Exit),
}

pub struct JsonlJournal<'d> {
    disk: &'d mut dyn Disk,
    start: Start,
    records: Vec<Record>,
    pub dropped_bytes: usize,
}

impl<'d> JsonlJournal<'d> {
    pub fn open(disk: &'d mut dyn Disk) -> Result<Self, OpenError> {
        let opened = jsonl_open_tail(disk)?;
        Ok(Self {
            disk,
            start: opened.start,
            records: opened.records,
            dropped_bytes: opened.dropped_bytes,
        })
    }

    pub fn run<H: Host>(
        mut self,
        host: &mut H,
        request: &TurnRequest,
    ) -> Result<(Self, Ran), RunError> {
        if let Start::Checkpoint { revision, .. } = self.start
            && request.turn <= revision
        {
            let opened = jsonl_open(&mut *self.disk).map_err(RunError::Open)?;
            self.start = opened.start;
            self.records = opened.records;
        }
        let mut flushed = Vec::new();
        let mut world = JsonlWorld {
            host: &mut *host,
            disk: &mut *self.disk,
            flushed: &mut flushed,
        };
        let ran = run_turn_from(&mut world, 0, self.start, &self.records, request)
            .map_err(RunError::Exit)?;
        match &ran {
            Ran::AlreadyFinished(finish) => host.acknowledge(finish),
            Ran::Finished(finish) => self.advance(finish.turn, flushed),
            Ran::Suspended(_) => self.records.extend(flushed),
            Ran::NotDue(_) => assert!(flushed.is_empty(), "an idle turn that is not due writes nothing"),
        }
        Ok((self, ran))
    }

    pub fn idle_due(&self) -> Result<Option<u64>, Exit> {
        idle_due(self.start, &self.records)
    }

    pub fn next_turn(&self) -> Result<u64, Exit> {
        next_turn(self.start, &self.records)
    }

    fn advance(&mut self, turn: u64, mut flushed: Vec<Record>) {
        if program::HOLDS_BLOBS {
            assert_eq!(self.start, Start::Beginning, "a journal of blobs is read whole, so every turn finds the blobs before it");
            self.records.extend(flushed);
            return;
        }
        let checkpoint = flushed.pop().expect("a finished turn flushed its records");
        let Record::Checkpoint { revision, .. } = &checkpoint else {
            panic!(
                "a finished turn flushes its checkpoint last, found {}",
                checkpoint.tag()
            );
        };
        assert_eq!(
            *revision, turn,
            "a finished turn checkpoints its own revision"
        );
        let next_cell = next_cell_after(next_cell_after(self.start.first_cell(), &self.records), &flushed);
        self.start = Start::Checkpoint {
            revision: turn,
            next_cell,
        };
        self.records = vec![checkpoint];
    }
}

pub fn run_jsonl<H: Host>(
    host: &mut H,
    disk: &mut dyn Disk,
    request: &TurnRequest,
) -> Result<Ran, RunError> {
    let journal = JsonlJournal::open(disk).map_err(RunError::Open)?;
    let (_, ran) = journal.run(host, request)?;
    Ok(ran)
}
