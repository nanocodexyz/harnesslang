pub mod command;
pub mod drill;
pub mod listing;

use crate::run::journal::{self, Outcome, Record};
use crate::run::json::Json;
use crate::target::Target;

pub const FIRST_EPOCH: u64 = 1;
pub const EMPTY_HEAD: u64 = 0;
pub const FIRST_ROW_SEQ: u64 = 1;
pub const MILLISECONDS_PER_SECOND: u64 = 1_000;
pub const MILLISECONDS_PER_DAY: u64 = 86_400_000;
pub const UNIX_EPOCH_JULIAN_DAY: &str = "2440587.5";
pub const RECORD_KINDS: [&str; 8] = [
    "admitted",
    "call",
    "result",
    "model",
    "recovery",
    "status",
    "checkpoint",
    "cell",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
    DurableObject,
}

impl Dialect {
    pub fn of(target: Target) -> Result<Self, String> {
        match target {
            Target::NativeSqlite => Ok(Self::Sqlite),
            Target::ServerPostgres => Ok(Self::Postgres),
            Target::Cloudflare => Ok(Self::DurableObject),
            Target::NativeJsonl | Target::Vercel => Err(format!(
                "target {} keeps no SQL journal; SQL targets are native-sqlite, server-postgres and cloudflare",
                target.name()
            )),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::DurableObject => "durable-object-sqlite",
        }
    }

    const fn integer(self) -> &'static str {
        match self {
            Self::Postgres => "BIGINT",
            Self::Sqlite | Self::DurableObject => "INTEGER",
        }
    }

    fn now_ms(self) -> String {
        match self {
            Self::Postgres => format!(
                "CAST(floor(extract(epoch FROM clock_timestamp()) * {MILLISECONDS_PER_SECOND}) AS BIGINT)"
            ),
            Self::Sqlite | Self::DurableObject => format!(
                "CAST((julianday('now') - {UNIX_EPOCH_JULIAN_DAY}) * {MILLISECONDS_PER_DAY} AS INTEGER)"
            ),
        }
    }

    pub const fn fenced_error(self) -> &'static str {
        match self {
            Self::Postgres => "SQLSTATE 23502 not_null_violation on hl_journal.epoch",
            Self::Sqlite | Self::DurableObject => {
                "SQLITE_CONSTRAINT_NOTNULL: NOT NULL constraint failed: hl_journal.epoch"
            }
        }
    }

    pub const fn duplicate_error(self) -> &'static str {
        match self {
            Self::Postgres => "SQLSTATE 23505 unique_violation on hl_journal_pkey",
            Self::Sqlite | Self::DurableObject => {
                "SQLITE_CONSTRAINT_PRIMARYKEY: UNIQUE constraint failed: hl_journal.session, hl_journal.seq"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mutation {
    None,
    NoFencePredicate,
    SeqNotUnique,
}

impl Mutation {
    pub const ALL: [Self; 3] = [Self::None, Self::NoFencePredicate, Self::SeqNotUnique];

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::NoFencePredicate => "no_fence_predicate",
            Self::SeqNotUnique => "seq_not_unique",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    Text,
    Integer,
    Json,
}

impl ParamType {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Json => "json",
        }
    }

    const fn postgres_cast(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Integer => "bigint",
            Self::Json => "json",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: &'static str,
    pub ty: ParamType,
}

const SESSION: Param = Param {
    name: "session",
    ty: ParamType::Text,
};
const OWNER: Param = Param {
    name: "owner",
    ty: ParamType::Text,
};
const LEASE_MS: Param = Param {
    name: "lease_ms",
    ty: ParamType::Integer,
};
const EPOCH: Param = Param {
    name: "epoch",
    ty: ParamType::Integer,
};
const HEAD: Param = Param {
    name: "head",
    ty: ParamType::Integer,
};
const BATCH: Param = Param {
    name: "batch",
    ty: ParamType::Json,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Literal {
    Text(String),
    Integer(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub name: &'static str,
    pub dialect: Dialect,
    template: String,
    pub params: Vec<Param>,
    pub returns: Vec<&'static str>,
}

const SLOT: char = '@';

fn slot_name(rest: &str) -> &str {
    let end = rest
        .find(|character: char| !(character.is_ascii_lowercase() || character == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

impl Statement {
    fn new(
        name: &'static str,
        dialect: Dialect,
        template: String,
        params: &[Param],
        returns: &[&'static str],
    ) -> Self {
        let statement = Self {
            name,
            dialect,
            template,
            params: params.to_vec(),
            returns: returns.to_vec(),
        };
        let mut used = vec![false; params.len()];
        statement.fill(|index, _| {
            used[index] = true;
            String::new()
        });
        assert!(
            used.iter().all(|used| *used),
            "statement {name} uses every parameter it declares"
        );
        statement
    }

    fn fill(&self, mut slot: impl FnMut(usize, &Param) -> String) -> String {
        let mut out = String::with_capacity(self.template.len());
        let mut rest = self.template.as_str();
        while let Some(at) = rest.find(SLOT) {
            out.push_str(&rest[..at]);
            let name = slot_name(&rest[at + 1..]);
            let index = self
                .params
                .iter()
                .position(|param| param.name == name)
                .unwrap_or_else(|| panic!("statement {} declares parameter {name}", self.name));
            out.push_str(&slot(index, &self.params[index]));
            rest = &rest[at + 1 + name.len()..];
        }
        out.push_str(rest);
        out
    }

    pub fn sql(&self) -> String {
        let dialect = self.dialect;
        self.fill(|index, param| match dialect {
            Dialect::Postgres => format!("${}::{}", index + 1, param.ty.postgres_cast()),
            Dialect::Sqlite | Dialect::DurableObject => format!("?{}", index + 1),
        })
    }

    pub fn render(&self, values: &[Literal]) -> String {
        assert_eq!(
            values.len(),
            self.params.len(),
            "one literal per parameter of {}",
            self.name
        );
        let dialect = self.dialect;
        self.fill(|index, param| {
            let literal = match (&values[index], param.ty) {
                (Literal::Integer(value), ParamType::Integer) => value.to_string(),
                (Literal::Text(text), ParamType::Text | ParamType::Json) => {
                    format!("'{}'", text.replace('\'', "''"))
                }
                (literal, ty) => panic!(
                    "statement {} parameter {} is {}, given {literal:?}",
                    self.name,
                    param.name,
                    ty.name()
                ),
            };
            match dialect {
                Dialect::Postgres => format!("{literal}::{}", param.ty.postgres_cast()),
                Dialect::Sqlite | Dialect::DurableObject => literal,
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalogue {
    pub target: Target,
    pub dialect: Dialect,
    pub mutation: Mutation,
    pub connect: Vec<Statement>,
    pub schema: Vec<Statement>,
    pub acquire: Statement,
    pub load: Statement,
    pub commit: Vec<Statement>,
}

pub fn catalogue(target: Target, mutation: Mutation) -> Result<Catalogue, String> {
    let dialect = Dialect::of(target)?;
    Ok(Catalogue {
        target,
        dialect,
        mutation,
        connect: connect(dialect),
        schema: schema(dialect, mutation),
        acquire: acquire(dialect),
        load: load(dialect),
        commit: commit(dialect, mutation),
    })
}

fn connect(dialect: Dialect) -> Vec<Statement> {
    match dialect {
        Dialect::Sqlite => vec![
            Statement::new(
                "journal_mode",
                dialect,
                "PRAGMA journal_mode = WAL".to_owned(),
                &[],
                &["journal_mode"],
            ),
            Statement::new(
                "synchronous",
                dialect,
                "PRAGMA synchronous = FULL".to_owned(),
                &[],
                &[],
            ),
        ],
        Dialect::Postgres | Dialect::DurableObject => Vec::new(),
    }
}

fn schema(dialect: Dialect, mutation: Mutation) -> Vec<Statement> {
    let integer = dialect.integer();
    let strict = match dialect {
        Dialect::Postgres => "",
        Dialect::Sqlite | Dialect::DurableObject => " STRICT",
    };
    let kinds = RECORD_KINDS
        .iter()
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let (primary, journal_options) = match (mutation, dialect) {
        (Mutation::SeqNotUnique, _) => ("", strict),
        (_, Dialect::Postgres) => (", PRIMARY KEY (session, seq)", ""),
        (_, Dialect::Sqlite | Dialect::DurableObject) => {
            (", PRIMARY KEY (session, seq)", " WITHOUT ROWID, STRICT")
        }
    };
    let sessions = format!(
        "CREATE TABLE IF NOT EXISTS hl_sessions (session TEXT PRIMARY KEY, owner TEXT NOT NULL, epoch {integer} NOT NULL CHECK (epoch >= {FIRST_EPOCH}), lease_expires {integer} NOT NULL, head {integer} NOT NULL CHECK (head >= {EMPTY_HEAD}), finished {integer} NOT NULL, checkpoint {integer} NOT NULL){strict}"
    );
    let journal = format!(
        "CREATE TABLE IF NOT EXISTS hl_journal (session TEXT NOT NULL, seq {integer} NOT NULL CHECK (seq >= {FIRST_ROW_SEQ}), epoch {integer} NOT NULL, turn {integer} NOT NULL, kind TEXT NOT NULL CHECK (kind IN ({kinds})), record TEXT NOT NULL{primary}){journal_options}"
    );
    vec![
        Statement::new("create_sessions", dialect, sessions, &[], &[]),
        Statement::new("create_journal", dialect, journal, &[], &[]),
    ]
}

fn acquire(dialect: Dialect) -> Statement {
    let now = dialect.now_ms();
    let template = format!(
        "INSERT INTO hl_sessions (session, owner, epoch, lease_expires, head, finished, checkpoint) VALUES (@session, @owner, {FIRST_EPOCH}, {now} + @lease_ms, {EMPTY_HEAD}, {EMPTY_HEAD}, {EMPTY_HEAD}) ON CONFLICT (session) DO UPDATE SET owner = excluded.owner, epoch = hl_sessions.epoch + 1, lease_expires = excluded.lease_expires WHERE hl_sessions.lease_expires <= {now} OR hl_sessions.owner = excluded.owner RETURNING CAST(epoch AS TEXT) AS epoch, CAST(head AS TEXT) AS head"
    );
    Statement::new(
        "acquire",
        dialect,
        template,
        &[SESSION, OWNER, LEASE_MS],
        &["epoch", "head"],
    )
}

fn load(dialect: Dialect) -> Statement {
    Statement::new(
        "load",
        dialect,
        "SELECT CAST(hl_journal.seq AS TEXT) AS seq, hl_journal.record FROM hl_journal WHERE hl_journal.session = @session ORDER BY hl_journal.seq".to_owned(),
        &[SESSION],
        &["seq", "record"],
    )
}

fn owner_predicate(table: &str, mutation: Mutation) -> String {
    if mutation == Mutation::NoFencePredicate {
        format!("{table}.session = @session")
    } else {
        format!("{table}.session = @session AND {table}.epoch = @epoch")
    }
}

fn without(params: &[Param], dropped: &[&str]) -> Vec<Param> {
    params
        .iter()
        .copied()
        .filter(|param| !dropped.contains(&param.name))
        .collect()
}

fn postgres_commit(mutation: Mutation, params: &[Param]) -> Statement {
    let now = Dialect::Postgres.now_ms();
    let owner = owner_predicate("hl_sessions", mutation);
    let template = format!(
        "WITH batch AS (SELECT b.seq, b.turn, b.kind, b.record FROM json_to_recordset(@batch) AS b(seq BIGINT, turn BIGINT, kind TEXT, record TEXT)), fence AS (UPDATE hl_sessions SET head = @head + (SELECT count(*) FROM batch), lease_expires = {now} + @lease_ms, finished = COALESCE((SELECT max(batch.turn) FROM batch WHERE batch.kind = 'status'), hl_sessions.finished), checkpoint = COALESCE((SELECT max(batch.seq) FROM batch WHERE batch.kind = 'checkpoint'), hl_sessions.checkpoint) WHERE {owner} AND hl_sessions.head = @head AND (SELECT min(batch.seq) FROM batch) = @head + {FIRST_ROW_SEQ} AND (SELECT max(batch.seq) FROM batch) = @head + (SELECT count(*) FROM batch) RETURNING hl_sessions.epoch) INSERT INTO hl_journal (session, seq, epoch, turn, kind, record) SELECT @session, batch.seq, (SELECT fence.epoch FROM fence), batch.turn, batch.kind, batch.record FROM batch"
    );
    let dropped: &[&str] = if mutation == Mutation::NoFencePredicate {
        &[EPOCH.name]
    } else {
        &[]
    };
    Statement::new(
        "commit",
        Dialect::Postgres,
        template,
        &without(params, dropped),
        &[],
    )
}

fn commit(dialect: Dialect, mutation: Mutation) -> Vec<Statement> {
    let params = [SESSION, EPOCH, HEAD, LEASE_MS, BATCH];
    if dialect == Dialect::Postgres {
        return vec![postgres_commit(mutation, &params)];
    }
    let now = dialect.now_ms();
    let rows = "json_each(@batch) AS b";
    let field = |name: &str| format!("json_extract(b.value, '$.{name}')");
    let (turn, kind, seq, record) = (field("turn"), field("kind"), field("seq"), field("record"));
    let conflict = match dialect {
        Dialect::Sqlite => "INSERT OR ROLLBACK INTO",
        Dialect::DurableObject | Dialect::Postgres => "INSERT INTO",
    };
    let fenced_owner = owner_predicate("s", mutation);
    let insert = format!(
        "{conflict} hl_journal (session, seq, epoch, turn, kind, record) SELECT @session, {seq}, (SELECT s.epoch FROM hl_sessions AS s WHERE {fenced_owner} AND s.head = @head AND (SELECT min({seq}) FROM {rows}) = @head + {FIRST_ROW_SEQ} AND (SELECT max({seq}) FROM {rows}) = @head + json_array_length(@batch)), {turn}, {kind}, {record} FROM {rows} ORDER BY b.key"
    );
    let owner = owner_predicate("hl_sessions", mutation);
    let advance = format!(
        "UPDATE hl_sessions SET head = @head + json_array_length(@batch), lease_expires = {now} + @lease_ms, finished = COALESCE((SELECT max({turn}) FROM {rows} WHERE {kind} = 'status'), hl_sessions.finished), checkpoint = COALESCE((SELECT max({seq}) FROM {rows} WHERE {kind} = 'checkpoint'), hl_sessions.checkpoint) WHERE {owner} AND hl_sessions.head = @head AND (SELECT count(*) FROM hl_journal AS j WHERE j.session = @session AND j.seq > @head) = json_array_length(@batch)"
    );
    let unfenced: &[&str] = if mutation == Mutation::NoFencePredicate {
        &[EPOCH.name]
    } else {
        &[]
    };
    let mut insert_dropped = unfenced.to_vec();
    insert_dropped.push(LEASE_MS.name);
    let mut statements = Vec::new();
    if dialect == Dialect::Sqlite {
        statements.push(Statement::new(
            "begin",
            dialect,
            "BEGIN IMMEDIATE".to_owned(),
            &[],
            &[],
        ));
    }
    statements.push(Statement::new(
        "insert",
        dialect,
        insert,
        &without(&params, &insert_dropped),
        &[],
    ));
    statements.push(Statement::new(
        "advance",
        dialect,
        advance,
        &without(&params, unfenced),
        &[],
    ));
    if dialect == Dialect::Sqlite {
        statements.push(Statement::new(
            "end",
            dialect,
            "COMMIT".to_owned(),
            &[],
            &[],
        ));
    }
    statements
}

pub fn record_kind(record: &Record) -> &'static str {
    let index = match record {
        Record::Admitted { .. } => 0,
        Record::Call { .. } | Record::NestedCall { .. } => 1,
        Record::Result { .. } | Record::NestedResult { .. } => 2,
        Record::Model { .. } => 3,
        Record::Recovery { .. } => 4,
        Record::Status { .. } => 5,
        Record::Checkpoint { .. } => 6,
        Record::Cell { .. } => 7,
    };
    RECORD_KINDS[index]
}

pub fn record_turn(record: &Record) -> u64 {
    match record {
        Record::Checkpoint { revision, .. } => *revision,
        Record::Cell { .. } => {
            panic!("emit refuses programs with code tools, so no cell record reaches sql")
        }
        other => other
            .turn()
            .expect("every record but a checkpoint has a turn"),
    }
}

pub fn batch_json(records: &[Record], head: u64) -> String {
    assert!(
        !records.is_empty(),
        "a durability point commits at least one record"
    );
    let mut rows = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let text = journal::to_json(record).encode();
        assert_eq!(
            journal::decode_line(&text, false).as_ref(),
            Ok(record),
            "a row's record decodes back"
        );
        rows.push(Json::object([
            ("kind", Json::text(record_kind(record))),
            ("record", Json::String(text)),
            ("seq", Json::Number(head + FIRST_ROW_SEQ + index as u64)),
            ("turn", Json::Number(record_turn(record))),
        ]));
    }
    Json::Array(rows).encode()
}

pub fn render_all(statements: &[Statement], named: &[(&str, Literal)]) -> Result<String, String> {
    let mut out = String::new();
    for statement in statements {
        let mut values = Vec::with_capacity(statement.params.len());
        for param in &statement.params {
            let value = named
                .iter()
                .find(|(name, _)| *name == param.name)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| {
                    format!(
                        "statement {} needs parameter {}",
                        statement.name, param.name
                    )
                })?;
            values.push(value);
        }
        out.push_str(&statement.render(&values));
        out.push_str(";\n");
    }
    Ok(out)
}

pub fn finished_turn_records(turn: u64, state: &str) -> Vec<Record> {
    vec![
        Record::Admitted {
            turn,
            handler: state.to_owned(),
            arguments: Json::object([]),
        },
        Record::Status {
            turn,
            last: EMPTY_HEAD,
            outcome: Outcome::Completed,
            result: Json::Null,
        },
        Record::Checkpoint {
            revision: turn,
            state: state.to_owned(),
            payload: Json::object([]).encode(),
        },
    ]
}
