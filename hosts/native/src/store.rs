use std::path::Path;

use harnesslang::emit::sql::Mutation;
use harnesslang::run::journal::{Record, decode_lines};
use harnesslang::target::Target;
use harnesslang::wasm::native::{Journal, Store};

use crate::sql::{self, Lease, SqlJournal};

pub const LEASE_MS: u64 = 60_000;
const OWNER: &str = "hl-host";
const JSONL_CHECKSUMS: bool = true;
const SQL_CHECKSUMS: bool = false;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spec {
    Jsonl,
    Sql {
        target: Target,
        location: String,
        session: String,
    },
}

pub fn parse(text: &str, session: &str) -> Result<Spec, String> {
    let sql = |target: Target, location: &str| {
        if location.is_empty() {
            return Err(format!("--store {text} names no location"));
        }
        Ok(Spec::Sql {
            target,
            location: location.to_owned(),
            session: session.to_owned(),
        })
    };
    match text.split_once(':') {
        None if text == "jsonl" => Ok(Spec::Jsonl),
        Some(("sqlite", location)) => sql(Target::NativeSqlite, location),
        Some(("postgres", location)) => sql(Target::ServerPostgres, location),
        _ => Err(format!(
            "--store {text} is not jsonl, sqlite:<file> or postgres:<host:port>/<database>"
        )),
    }
}

pub enum Opened {
    Jsonl(Journal),
    Sql(SqlJournal<'static>),
}

pub fn open(spec: &Spec, journal: &Path) -> Result<Opened, String> {
    let Spec::Sql {
        target,
        location,
        session,
    } = spec
    else {
        return Journal::open(journal).map(Opened::Jsonl);
    };
    let lease = Lease {
        session: session.clone(),
        owner: OWNER.to_owned(),
        lease_ms: LEASE_MS,
    };
    match sql::open_journal(*target, location, OWNER, lease, Mutation::None)? {
        sql::Opened::Owned(journal) => Ok(Opened::Sql(journal)),
        sql::Opened::Held => Err(format!("session {session} in {location}: leased elsewhere")),
    }
}

pub fn records(spec: &Spec, journal: &Path) -> Result<Vec<Record>, String> {
    let (bytes, checksums) = match spec {
        Spec::Jsonl if !journal.exists() => return Ok(Vec::new()),
        Spec::Jsonl => (
            std::fs::read(journal).map_err(|error| format!("{}: {error}", journal.display()))?,
            JSONL_CHECKSUMS,
        ),
        Spec::Sql {
            target,
            location,
            session,
        } => (
            sql::read_lines(*target, location, session)?.into_bytes(),
            SQL_CHECKSUMS,
        ),
    };
    decode_lines(&bytes, checksums)
        .map(|decoded| decoded.records)
        .map_err(|error| format!("{}: {error}", journal.display()))
}

impl Store for Opened {
    fn read(&mut self) -> Result<Vec<u8>, String> {
        match self {
            Self::Jsonl(journal) => Store::read(journal),
            Self::Sql(journal) => journal.read(),
        }
    }

    fn append(&mut self, epoch: i64, bytes: &[u8]) -> Result<(), String> {
        match self {
            Self::Jsonl(journal) => Store::append(journal, epoch, bytes),
            Self::Sql(journal) => journal.append(epoch, bytes),
        }
    }

    fn keep(&mut self, length: u64) -> Result<(), String> {
        match self {
            Self::Jsonl(journal) => Store::keep(journal, length),
            Self::Sql(journal) => journal.keep(length),
        }
    }

    fn epoch(&self) -> i64 {
        match self {
            Self::Jsonl(journal) => Store::epoch(journal),
            Self::Sql(journal) => journal.epoch(),
        }
    }
}
