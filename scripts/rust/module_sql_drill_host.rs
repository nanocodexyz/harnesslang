#[path = "sql_drill_checks.rs"]
mod checks;
#[path = "postgres_store.rs"]
mod postgres_store;
#[path = "sqlite_store.rs"]
mod sqlite_store;

mod harness {
    pub use harnesslang::effect::EffectClass;
    pub use harnesslang::run::cell_journal::NESTED_ID_INFIX;
    pub use harnesslang::run::journal::{Record, call_id};
    pub use harnesslang::run::session::{Session, fold};

    pub mod program {
        pub fn code_of(_tool: &str) -> Option<usize> {
            None
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ParamType {
        Text,
        Integer,
        Json,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Param {
        pub name: &'static str,
        pub ty: ParamType,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Statement {
        pub name: &'static str,
        pub sql: &'static str,
        pub params: &'static [Param],
        pub returns: &'static [&'static str],
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum SqlValue {
        Text(String),
        Integer(u64),
        Json(String),
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum SqlError {
        Fenced,
        Other(String),
    }

    pub trait Sql {
        fn query(
            &mut self,
            statement: &Statement,
            values: &[SqlValue],
        ) -> Result<Vec<Vec<Option<String>>>, SqlError>;
        fn rollback(&mut self);
    }

    pub const FENCED_ERROR: &str =
        "SQLITE_CONSTRAINT_NOTNULL: NOT NULL constraint failed: hl_journal.epoch";
}

use std::collections::BTreeMap;

use harness::{EffectClass, Param, ParamType, Sql, SqlError, SqlValue, Statement};
use harnesslang::emit::sql as catalogue;
use harnesslang::target::Target;
use harnesslang::wasm::js::Json;
use harnesslang::wasm::native::{self, Store, World};
use postgres_store::PostgresStore;
use sqlite_store::SqliteStore;

const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
const SPLITMIX_MIX_FIRST: u64 = 0xbf58_476d_1ce4_e5b9;
const SPLITMIX_MIX_SECOND: u64 = 0x94d0_49bb_1331_11eb;
const PPM: u64 = 1_000_000;
const ANSWER_PPM: u64 = 350_000;
const UNDECLARED_TOOL_PPM: u64 = 250_000;
const TOOL_FAILURE_PPM: u64 = 150_000;
const UNDECLARED_TOOLS: [&str; 2] = ["web_fetch", "spawn_agent"];
const DECLARED_TOOLS: [&str; 3] = ["read", "edit", "bash"];
const EXIT_HELD: i32 = 3;
const EXIT_FENCED: i32 = 4;
const EXIT_STORE_LOST: i32 = 5;
const EXIT_PAUSE_BOUND: i32 = 6;
const EXIT_USAGE: i32 = 2;
const BUSY_TIMEOUT_MS: i32 = 10_000;
const PAUSE_POLL_MS: u64 = 2;
const PAUSE_POLLS_MAX: u64 = 30_000;
const POSTGRES_USER: &str = "postgres";
const POSTGRES_FENCED_ERROR: &str = "SQLSTATE 23502 not_null_violation on hl_journal.epoch";
const INTEGRITY_OK: &str = "ok";
const INTEGRITY_CHECK: Statement = Statement {
    name: "integrity_check",
    sql: "PRAGMA integrity_check",
    params: &[],
    returns: &["integrity_check"],
};
const LINE_END: char = '\n';

struct Rng(u64);

impl Rng {
    fn draw(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(SPLITMIX_GAMMA);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(SPLITMIX_MIX_FIRST);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(SPLITMIX_MIX_SECOND);
        mixed ^ (mixed >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "a draw is below a positive bound");
        ((u128::from(self.draw()) * u128::from(bound)) >> 64) as u64
    }

    fn chance(&mut self, ppm: u64) -> bool {
        self.below(PPM) < ppm
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pause {
    Never,
    FirstFlush,
    External(u64),
}

struct Pauser {
    owner: String,
    dir: String,
    tag: String,
    waiting: Pause,
}

impl Pauser {
    fn pause_here(&mut self) {
        let paused = format!("{}/paused.{}", self.dir, self.owner);
        let resume = format!("{}/resume.{}", self.dir, self.owner);
        std::fs::write(&paused, &self.tag).unwrap_or_else(|error| panic!("{paused}: {error}"));
        self.waiting = Pause::Never;
        for _ in 0..PAUSE_POLLS_MAX {
            if std::path::Path::new(&resume).exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(PAUSE_POLL_MS));
        }
        eprintln!(
            "pause: {resume} did not appear within PAUSE_POLLS_MAX ({PAUSE_POLLS_MAX}) polls"
        );
        std::process::exit(EXIT_PAUSE_BOUND);
    }
}

struct Drill<'p> {
    ledger: std::fs::File,
    acks: std::fs::File,
    rng: Rng,
    clock: u64,
    tag: String,
    externals: u64,
    pauser: &'p std::cell::RefCell<Pauser>,
}

fn number(json: &Json, what: &str) -> Result<u64, String> {
    json.bigint()
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| format!("{what} is not a number: {}", json.encode()))
}

fn object(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

impl World for Drill<'_> {
    fn tool(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a tool request's turn")?;
        let seq = number(request.get("seq"), "a tool request's seq")?;
        if !matches!(request.get("nested"), Json::Null | Json::Undefined) {
            return Err("the drill program runs no code cells".to_owned());
        }
        let Json::Text(tool) = request.get("tool") else {
            return Err("a tool request names its tool".to_owned());
        };
        let Json::Text(effect) = request.get("effect") else {
            return Err("a tool request names its effect".to_owned());
        };
        let effect =
            EffectClass::parse(effect).ok_or_else(|| format!("unknown effect {effect}"))?;
        let id = harness::call_id(turn, seq);
        let line = checks::dispatch_line(&id, tool, effect, &self.tag);
        checks::append_synced(&mut self.ledger, &line);
        if effect == EffectClass::External {
            self.externals += 1;
            let mut pauser = self.pauser.borrow_mut();
            if pauser.waiting == Pause::External(self.externals) {
                pauser.pause_here();
            }
        }
        if self.rng.chance(TOOL_FAILURE_PPM) {
            return Ok(object(vec![(
                "failed",
                Json::text(&format!("{tool} failed at {id}")),
            )]));
        }
        Ok(object(vec![("ok", Json::text(&format!("{tool}-{id}")))]))
    }

    fn model(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a model request's turn")?;
        let seq = number(request.get("seq"), "a model request's seq")?;
        let calls_max = number(request.get("calls_max"), "a model request's calls_max")?;
        if self.rng.chance(ANSWER_PPM) {
            return Ok(object(vec![(
                "answer",
                Json::text(&format!("done-{turn}-{seq}")),
            )]));
        }
        let count = 1 + self.rng.below(calls_max);
        let mut calls = Vec::new();
        for index in 0..count {
            let tool = if self.rng.chance(UNDECLARED_TOOL_PPM) {
                UNDECLARED_TOOLS[self.rng.below(UNDECLARED_TOOLS.len() as u64) as usize]
            } else {
                DECLARED_TOOLS[self.rng.below(DECLARED_TOOLS.len() as u64) as usize]
            };
            calls.push(object(vec![
                ("id", Json::text(&format!("c{turn}-{seq}-{index}"))),
                ("tool", Json::text(tool)),
                ("input", Json::text(&typed_input(tool, index))),
            ]));
        }
        Ok(object(vec![("calls", Json::Array(calls))]))
    }

    fn now(&mut self) -> Result<u64, String> {
        self.clock += 1;
        Ok(self.clock)
    }

    fn random(&mut self) -> Result<u64, String> {
        Ok(self.rng.draw())
    }

    fn world(&mut self, request: &Json) -> Result<Json, String> {
        Err(format!(
            "the drill program makes no world request, found {}",
            request.encode()
        ))
    }

    fn acknowledge(&mut self, finish: &Json) -> Result<(), String> {
        let turn = number(finish.get("turn"), "a finish's turn")?;
        let Json::Text(outcome) = finish.get("outcome") else {
            return Err("a finish names its outcome".to_owned());
        };
        checks::append_synced(
            &mut self.acks,
            &format!("{turn} {outcome} {}\n", finish.get("result").encode()),
        );
        Ok(())
    }
}

fn leak(statement: &catalogue::Statement) -> Statement {
    let params: Vec<Param> = statement
        .params
        .iter()
        .map(|param| Param {
            name: param.name,
            ty: match param.ty {
                catalogue::ParamType::Text => ParamType::Text,
                catalogue::ParamType::Integer => ParamType::Integer,
                catalogue::ParamType::Json => ParamType::Json,
            },
        })
        .collect();
    Statement {
        name: statement.name,
        sql: Box::leak(statement.sql().into_boxed_str()),
        params: Box::leak(params.into_boxed_slice()),
        returns: Box::leak(statement.returns.clone().into_boxed_slice()),
    }
}

struct Statements {
    setup: Vec<Statement>,
    acquire: Statement,
    load: Statement,
    commit: Vec<Statement>,
}

fn statements(target: Target, mutation: catalogue::Mutation) -> Statements {
    let catalogue = catalogue::catalogue(target, mutation).unwrap_or_else(|error| usage(&error));
    Statements {
        setup: catalogue
            .connect
            .iter()
            .chain(&catalogue.schema)
            .map(leak)
            .collect(),
        acquire: leak(&catalogue.acquire),
        load: leak(&catalogue.load),
        commit: catalogue.commit.iter().map(leak).collect(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    Fenced,
    Lost,
}

struct Session {
    session: String,
    owner: String,
    lease_ms: u64,
}

impl Session {
    fn value(&self, param: &Param, epoch: u64, head: u64, batch: &str) -> SqlValue {
        match param.name {
            "session" => SqlValue::Text(self.session.clone()),
            "owner" => SqlValue::Text(self.owner.clone()),
            "lease_ms" => SqlValue::Integer(self.lease_ms),
            "epoch" => SqlValue::Integer(epoch),
            "head" => SqlValue::Integer(head),
            "batch" => SqlValue::Json(batch.to_owned()),
            other => panic!("the catalogue names only journal parameters, found {other}"),
        }
    }

    fn values(&self, statement: &Statement, epoch: u64, head: u64, batch: &str) -> Vec<SqlValue> {
        statement
            .params
            .iter()
            .map(|param| self.value(param, epoch, head, batch))
            .collect()
    }
}

fn column(row: &[Option<String>], statement: &Statement, name: &str) -> Result<String, String> {
    let index = statement
        .returns
        .iter()
        .position(|returned| *returned == name)
        .expect("the statement returns the column");
    row.get(index)
        .and_then(|value| value.clone())
        .ok_or_else(|| format!("{} returned no {name}", statement.name))
}

fn load_lines(sql: &mut dyn Sql, load: &Statement, session: &Session) -> Result<String, SqlError> {
    let rows = sql.query(load, &session.values(load, 0, 0, ""))?;
    let mut text = String::new();
    for (index, row) in rows.iter().enumerate() {
        let seq = column(row, load, "seq").map_err(SqlError::Other)?;
        assert_eq!(
            seq,
            (index + 1).to_string(),
            "rows load in seq order without gaps"
        );
        let record = column(row, load, "record").map_err(SqlError::Other)?;
        assert!(!record.contains(LINE_END), "a row holds one journal line");
        text.push_str(&record);
        text.push(LINE_END);
    }
    Ok(text)
}

struct SqlStore<'s, 'p> {
    sql: &'s mut dyn Sql,
    statements: &'s Statements,
    session: Session,
    epoch: u64,
    head: u64,
    read: u64,
    failure: Option<Failure>,
    pauser: &'p std::cell::RefCell<Pauser>,
}

impl SqlStore<'_, '_> {
    fn batch(&self, bytes: &[u8]) -> Result<(String, u64), String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "a flush is UTF-8".to_owned())?;
        let lines: Vec<&str> = text.split_terminator(LINE_END).collect();
        assert!(
            !lines.is_empty() && text.ends_with(LINE_END),
            "a flush appends whole lines"
        );
        let mut rows = Vec::with_capacity(lines.len());
        for (index, line) in lines.iter().enumerate() {
            let record = harnesslang::run::journal::decode_line(line, false)
                .map_err(|error| format!("a flushed line does not decode: {error}"))?;
            rows.push(object(vec![
                ("kind", Json::text(catalogue::record_kind(&record))),
                ("record", Json::text(line)),
                ("seq", Json::int(self.head + 1 + index as u64)),
                ("turn", Json::int(catalogue::record_turn(&record))),
            ]));
        }
        Ok((Json::Array(rows).encode(), lines.len() as u64))
    }
}

impl Store for SqlStore<'_, '_> {
    fn read(&mut self) -> Result<Vec<u8>, String> {
        let text = load_lines(self.sql, &self.statements.load, &self.session).map_err(|error| {
            self.failure = Some(Failure::Lost);
            format!("load: {error:?}")
        })?;
        let rows = text.matches(LINE_END).count() as u64;
        if rows != self.head {
            return Err(format!(
                "the session's head is {} but {rows} rows load",
                self.head
            ));
        }
        self.read = text.len() as u64;
        Ok(text.into_bytes())
    }

    fn append(&mut self, epoch: i64, bytes: &[u8]) -> Result<(), String> {
        {
            let mut pauser = self.pauser.borrow_mut();
            if pauser.waiting == Pause::FirstFlush {
                pauser.pause_here();
            }
        }
        let epoch =
            u64::try_from(epoch).map_err(|_| "a flush names a negative epoch".to_owned())?;
        let (batch, rows) = self.batch(bytes)?;
        for statement in &self.statements.commit {
            let values = self.session.values(statement, epoch, self.head, &batch);
            match self.sql.query(statement, &values) {
                Ok(_) => {}
                Err(SqlError::Fenced) => {
                    self.sql.rollback();
                    self.failure = Some(Failure::Fenced);
                    return Err(format!("fenced: epoch {epoch} no longer owns the session"));
                }
                Err(SqlError::Other(error)) => {
                    self.sql.rollback();
                    self.failure = Some(Failure::Lost);
                    return Err(format!("store lost during a flush: {error}"));
                }
            }
        }
        self.head += rows;
        self.read += bytes.len() as u64;
        Ok(())
    }

    fn keep(&mut self, length: u64) -> Result<(), String> {
        if length == self.read {
            return Ok(());
        }
        Err(format!(
            "an SQL journal commits whole batches, so it has no torn tail to drop ({length} of {} bytes kept)",
            self.read
        ))
    }

    fn epoch(&self) -> i64 {
        self.epoch as i64
    }
}

enum Database {
    Sqlite(SqliteStore),
    Postgres(PostgresStore),
}

impl Database {
    fn sql(&mut self) -> &mut dyn Sql {
        match self {
            Self::Sqlite(store) => store,
            Self::Postgres(store) => store,
        }
    }
}

fn target_of(options: &Options) -> Target {
    let name = options.text("target");
    match Target::parse(name) {
        Some(target @ (Target::NativeSqlite | Target::ServerPostgres)) => target,
        _ => usage(&format!(
            "target={name} is not native-sqlite or server-postgres"
        )),
    }
}

fn open_database(
    target: Target,
    location: &str,
    application: &str,
    pid_file: Option<&str>,
) -> Result<Database, String> {
    if target == Target::NativeSqlite {
        return SqliteStore::open(location, BUSY_TIMEOUT_MS).map(Database::Sqlite);
    }
    assert_eq!(
        catalogue::Dialect::Postgres.fenced_error(),
        POSTGRES_FENCED_ERROR,
        "the adapter maps the fence the catalogue names"
    );
    let (address, database) = location
        .split_once('/')
        .ok_or_else(|| format!("{location} is not host:port/database"))?;
    let store = PostgresStore::connect(address, POSTGRES_USER, database, application)?;
    if let Some(path) = pid_file {
        let partial = format!("{path}.partial");
        std::fs::write(&partial, store.backend_pid.to_string())
            .map_err(|error| format!("{partial}: {error}"))?;
        std::fs::rename(&partial, path).map_err(|error| format!("{path}: {error}"))?;
    }
    Ok(Database::Postgres(store))
}

struct Options(BTreeMap<String, String>);

impl Options {
    fn parse(args: &[String]) -> Self {
        let pairs = args.iter().map(|arg| {
            let (key, value) = arg
                .split_once('=')
                .unwrap_or_else(|| usage(&format!("{arg} is not key=value")));
            (key.to_owned(), value.to_owned())
        });
        Self(pairs.collect())
    }

    fn text(&self, key: &str) -> &str {
        self.0
            .get(key)
            .map_or_else(|| usage(&format!("missing {key}=")), String::as_str)
    }

    fn number(&self, key: &str) -> u64 {
        let text = self.text(key);
        text.parse()
            .unwrap_or_else(|_| usage(&format!("{key}={text} is not a number")))
    }

    fn pause(&self) -> Pause {
        match self.0.get("pause").map(String::as_str) {
            None | Some("never") => Pause::Never,
            Some("flush") => Pause::FirstFlush,
            Some(external) => match external.strip_prefix("external:").map(str::parse) {
                Some(Ok(n)) if n > 0 => Pause::External(n),
                _ => usage(&format!(
                    "pause={external} is not never, flush or external:N"
                )),
            },
        }
    }

    fn mutation(&self) -> catalogue::Mutation {
        match self.0.get("mutation").map(String::as_str) {
            None | Some("none") => catalogue::Mutation::None,
            Some("no_fence") => catalogue::Mutation::NoFencePredicate,
            Some(other) => usage(&format!("mutation={other} is not none or no_fence")),
        }
    }
}

fn usage(problem: &str) -> ! {
    eprintln!("{problem}");
    eprintln!(
        "usage: module_sql_drill_host run target=T module=M store=S session=S owner=O lease_ms=N ledger=F acks=F seed=N turns=N [mutation=none|no_fence] [pause=never|flush|external:N pause_dir=D] [pid_file=F]\n       module_sql_drill_host verify target=T store=S session=S ledger=F acks=F turns=N taken_over=N"
    );
    std::process::exit(EXIT_USAGE);
}

fn turn_request(seed: u64, turn: u64) -> Json {
    object(vec![
        (
            "arguments",
            object(vec![("input", Json::text(&format!("drill-{seed}-{turn}")))]),
        ),
        ("handler", Json::text("prompt")),
        ("turn", Json::int(turn)),
    ])
}

fn acknowledged_last(acks: &str) -> u64 {
    let text = std::fs::read_to_string(acks).unwrap_or_default();
    let mut last = 0;
    for line in text.lines() {
        let turn = line
            .split(' ')
            .next()
            .and_then(|turn| turn.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("ack line {line:?} does not start with its turn"));
        last = last.max(turn);
    }
    last
}

fn store_lost(error: &str) -> i32 {
    eprintln!("store lost: {error}");
    EXIT_STORE_LOST
}

fn run(options: &Options) -> i32 {
    let target = target_of(options);
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_nanos() as u64;
    let module = options.text("module");
    let bytes =
        std::fs::read(module).unwrap_or_else(|error| panic!("cannot read {module}: {error}"));
    let loaded = harnesslang::wasm::sim::load(&bytes).unwrap_or_else(|error| panic!("{error}"));
    let tag = checks::run_tag(options.text("owner"));
    let pause = options.pause();
    let pauser = std::cell::RefCell::new(Pauser {
        owner: options.text("owner").to_owned(),
        dir: if pause == Pause::Never {
            String::new()
        } else {
            options.text("pause_dir").to_owned()
        },
        tag: tag.clone(),
        waiting: pause,
    });
    let mut drill = Drill {
        ledger: checks::open_append(options.text("ledger")),
        acks: checks::open_append(options.text("acks")),
        rng: Rng(options.number("seed") ^ started),
        clock: 0,
        tag: tag.clone(),
        externals: 0,
        pauser: &pauser,
    };
    checks::append_synced(&mut drill.ledger, &checks::run_line(&tag));
    let mut database = match open_database(
        target,
        options.text("store"),
        &tag,
        options.0.get("pid_file").map(String::as_str),
    ) {
        Ok(database) => database,
        Err(error) => return store_lost(&error),
    };
    let statements = statements(target, options.mutation());
    let sql = database.sql();
    for statement in &statements.setup {
        if let Err(error) = sql.query(statement, &[]) {
            return store_lost(&format!("{}: {error:?}", statement.name));
        }
    }
    let session = Session {
        session: options.text("session").to_owned(),
        owner: options.text("owner").to_owned(),
        lease_ms: options.number("lease_ms"),
    };
    let rows = match sql.query(
        &statements.acquire,
        &session.values(&statements.acquire, 0, 0, ""),
    ) {
        Ok(rows) => rows,
        Err(error) => return store_lost(&format!("acquire: {error:?}")),
    };
    let [row] = rows.as_slice() else {
        assert!(rows.is_empty(), "acquire returns at most one row");
        return EXIT_HELD;
    };
    let read = |name: &str| -> u64 {
        column(row, &statements.acquire, name)
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or_else(|| panic!("acquire returned no number for {name}"))
    };
    let (epoch, head) = (read("epoch"), read("head"));
    checks::append_synced(&mut drill.ledger, &checks::acquired_line(&tag, epoch));
    let mut store = SqlStore {
        sql,
        statements: &statements,
        session,
        epoch,
        head,
        read: 0,
        failure: None,
        pauser: &pauser,
    };
    let seed = options.number("seed");
    let first = acknowledged_last(options.text("acks")) + 1;
    for turn in first..=options.number("turns") {
        match native::run(&loaded, &mut store, &turn_request(seed, turn), &mut drill) {
            Ok(exit) => match exit.get("exit") {
                Json::Text(kind) if kind == "finished" => {}
                Json::Text(kind) if kind == "already_finished" => drill
                    .acknowledge(exit.get("finish"))
                    .unwrap_or_else(|error| panic!("{error}")),
                _ => panic!(
                    "turn {turn}: the drill program finishes every turn, found {}",
                    exit.encode()
                ),
            },
            Err(error) => match store.failure {
                Some(Failure::Fenced) => {
                    checks::append_synced(&mut drill.ledger, &checks::taken_over_line(&tag));
                    return EXIT_FENCED;
                }
                Some(Failure::Lost) => return store_lost(&format!("turn {turn}: {error}")),
                None => panic!("turn {turn}: {error}"),
            },
        }
    }
    checks::append_synced(&mut drill.ledger, &checks::finished_line(&tag));
    0
}

fn integrity(database: &mut Database) -> &'static str {
    let Database::Sqlite(sqlite) = database else {
        return "not-sqlite";
    };
    let rows = sqlite
        .query(&INTEGRITY_CHECK, &[])
        .unwrap_or_else(|error| panic!("integrity_check: {error:?}"));
    let first = rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| value.as_deref());
    assert_eq!(
        first,
        Some(INTEGRITY_OK),
        "the database passes integrity_check"
    );
    INTEGRITY_OK
}

fn verify(options: &Options) -> i32 {
    let target = target_of(options);
    let mut database = match open_database(target, options.text("store"), "verify", None) {
        Ok(database) => database,
        Err(error) => return store_lost(&error),
    };
    let integrity = integrity(&mut database);
    let statements = statements(target, catalogue::Mutation::None);
    let session = Session {
        session: options.text("session").to_owned(),
        owner: String::new(),
        lease_ms: 0,
    };
    let text = match load_lines(database.sql(), &statements.load, &session) {
        Ok(text) => text,
        Err(error) => return store_lost(&format!("load: {error:?}")),
    };
    let decoded = harnesslang::run::journal::decode_lines(text.as_bytes(), false)
        .unwrap_or_else(|error| panic!("the journal decodes: {error}"));
    assert_eq!(decoded.dropped_bytes, 0, "an SQL journal has no torn tail");
    let turns = options.number("turns");
    let taken_over = options.number("taken_over") as usize;
    let verified = checks::check_all(
        &decoded.records,
        options.text("ledger"),
        options.text("acks"),
        turns,
        taken_over,
    );
    println!(
        "verified turns={turns} runs={} records={} external={} unknown={} other={} dispatches={} other_dispatches_lost={} acks={} acquires={} takeovers={} taken_over={taken_over} integrity={integrity}",
        verified.runs,
        decoded.records.len(),
        verified.external,
        verified.unknown,
        verified.other,
        verified.dispatches,
        verified.other_lost,
        verified.acks,
        verified.fence.acquires,
        verified.fence.takeovers,
    );
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        usage("missing command");
    };
    let options = Options::parse(rest);
    let status = match command.as_str() {
        "run" => run(&options),
        "verify" => verify(&options),
        "version" => {
            println!(
                "target={} sqlite={}",
                target_of(&options).name(),
                sqlite_store::version()
            );
            0
        }
        other => usage(&format!("unknown command {other}")),
    };
    std::process::exit(status);
}

fn typed_input(tool: &str, index: u64) -> String {
    let fields = match tool {
        "read" => vec![("path", Json::text(&format!("input-{index}")))],
        "edit" => vec![
            ("path", Json::text(&format!("input-{index}"))),
            ("body", Json::text(&format!("body-{index}"))),
        ],
        "bash" => vec![("command", Json::text(&format!("input-{index}")))],
        _ => return format!("input-{index}"),
    };
    object(fields).encode()
}
