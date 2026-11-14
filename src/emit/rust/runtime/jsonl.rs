pub trait Disk {
    fn contents(&mut self) -> std::io::Result<Vec<u8>>;
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    fn sync(&mut self) -> std::io::Result<()>;
    fn truncate(&mut self, length: u64) -> std::io::Result<()>;
}

#[derive(Debug)]
pub enum OpenError {
    Io(std::io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "journal: {error}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub records: Vec<Record>,
    pub dropped_bytes: usize,
}

pub fn jsonl_open(disk: &mut dyn Disk) -> Result<Opened, OpenError> {
    disk.sync().map_err(OpenError::Io)?;
    let bytes = disk.contents().map_err(OpenError::Io)?;
    let decoded = decode_checksummed_lines(&bytes);
    if decoded.dropped_bytes > 0 {
        disk.truncate(decoded.valid_bytes as u64).map_err(OpenError::Io)?;
        disk.sync().map_err(OpenError::Io)?;
    }
    Ok(Opened {
        records: decoded.records,
        dropped_bytes: decoded.dropped_bytes,
    })
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
        })
    }
}

impl Disk for FileDisk {
    fn contents(&mut self) -> std::io::Result<Vec<u8>> {
        use std::io::{Read, Seek};
        let mut bytes = Vec::new();
        self.file.seek(std::io::SeekFrom::Start(0))?;
        self.file.read_to_end(&mut bytes)?;
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
}

impl<H: Host> World for JsonlWorld<'_, H> {
    fn hook(&mut self, point: Point) -> Result<(), Stop> {
        self.host.hook(point)
    }

    fn flush(&mut self, _epoch: u64, records: &[Record]) -> Result<(), Stop> {
        jsonl_flush(self.disk, records)
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        self.host.tool(request)
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
}

#[derive(Debug)]
pub enum RunError {
    Open(OpenError),
    Exit(Exit),
}

pub fn run_jsonl<H: Host>(host: &mut H, disk: &mut dyn Disk, request: &TurnRequest) -> Result<Ran, RunError> {
    let opened = jsonl_open(disk).map_err(RunError::Open)?;
    let mut world = JsonlWorld { host, disk };
    let ran = run_turn(&mut world, 0, &opened.records, request).map_err(RunError::Exit)?;
    if let Ran::AlreadyFinished(finish) = &ran {
        world.host.acknowledge(finish);
    }
    Ok(ran)
}
