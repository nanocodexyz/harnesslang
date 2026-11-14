#[path = "sql_drill_checks.rs"]
mod checks;
#[path = "harness.rs"]
mod harness;
#[path = "postgres_store.rs"]
mod postgres_store;
#[path = "sqlite_store.rs"]
mod sqlite_store;

use std::collections::BTreeMap;

use harness::program::{self, Arguments};
use harness::{
    EffectClass, Finish, Host, Json, ModelRequest, OpenError, Output, Point, Ran, Returns,
    RunError, Sql, SqlError, SqlJournal, SqlValue, Statement, Stop, ToolRequest, TurnRequest,
    call_id,
};
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
const DIALECT_SQLITE: &str = "sqlite";
const DIALECT_POSTGRES: &str = "postgres";
const INTEGRITY_OK: &str = "ok";
const INTEGRITY_CHECK: Statement = Statement {
    name: "integrity_check",
    sql: "PRAGMA integrity_check",
    params: &[],
    returns: &["integrity_check"],
};

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pause {
    Never,
    External(u64),
    FirstFlush,
}

enum Paused {
    Waiting(Pause),
    Done,
}

struct Drill {
    ledger: std::fs::File,
    acks: std::fs::File,
    rng: Rng,
    clock: u64,
    tag: String,
    owner: String,
    pause_dir: String,
    paused: Paused,
    externals: u64,
}

impl Drill {
    fn pause_here(&mut self) {
        let paused = format!("{}/paused.{}", self.pause_dir, self.owner);
        let resume = format!("{}/resume.{}", self.pause_dir, self.owner);
        std::fs::write(&paused, &self.tag).unwrap_or_else(|error| panic!("{paused}: {error}"));
        self.paused = Paused::Done;
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

impl Host for Drill {
    fn hook(&mut self, point: Point) -> Result<(), Stop> {
        if point == Point::BeforeFlush && matches!(self.paused, Paused::Waiting(Pause::FirstFlush))
        {
            self.pause_here();
        }
        Ok(())
    }

    fn listing(&mut self, request: &harness::ListingRequest<'_>) -> Output {
        let id = call_id(request.turn, request.seq);
        let line = checks::dispatch_line(&id, request.listing, EffectClass::None, &self.tag);
        checks::append_synced(&mut self.ledger, &line);
        Output::Ok(Json::Object(std::collections::BTreeMap::new()))
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        let id = call_id(request.turn, request.seq);
        let line = checks::dispatch_line(&id, request.tool, request.effect, &self.tag);
        checks::append_synced(&mut self.ledger, &line);
        if request.effect == EffectClass::External {
            self.externals += 1;
            if matches!(self.paused, Paused::Waiting(Pause::External(n)) if n == self.externals) {
                self.pause_here();
            }
        }
        if self.rng.chance(TOOL_FAILURE_PPM) {
            return Output::Failed(format!("{} failed at {id}", request.tool));
        }
        Output::Ok(match request.returns {
            Returns::U64 => Json::Number(self.rng.draw() >> 32),
            Returns::Bool => Json::Bool(self.rng.draw() & 1 == 1),
            Returns::Text => Json::String(format!("{}-{id}", request.tool)),
            Returns::Values { values, .. } => {
                Json::String(values[(self.rng.draw() % values.len() as u64) as usize].to_owned())
            }
            Returns::Blob { .. } => harness::host_blob_bytes(format!("{}-{id}", request.tool).as_bytes()),
        })
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        if self.rng.chance(ANSWER_PPM) {
            let answer = format!("done-{}-{}", request.turn, request.seq);
            return Json::object(vec![("answer", Json::String(answer))]);
        }
        let count = 1 + self.rng.below(request.calls_max);
        let mut calls = Vec::new();
        for index in 0..count {
            let tool = if self.rng.chance(UNDECLARED_TOOL_PPM) {
                UNDECLARED_TOOLS[self.rng.below(UNDECLARED_TOOLS.len() as u64) as usize]
            } else {
                DECLARED_TOOLS[self.rng.below(DECLARED_TOOLS.len() as u64) as usize]
            };
            calls.push(Json::object(vec![
                (
                    "id",
                    Json::String(format!("c{}-{}-{index}", request.turn, request.seq)),
                ),
                ("tool", Json::text(tool)),
                ("input", Json::String(typed_input(tool, index))),
            ]));
        }
        Json::object(vec![("calls", Json::Array(calls))])
    }

    fn now(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn random(&mut self) -> u64 {
        self.rng.draw()
    }

    fn acknowledge(&mut self, finish: &Finish) {
        let line = format!(
            "{} {} {}\n",
            finish.turn,
            finish.outcome.name(),
            finish.result.encode()
        );
        checks::append_synced(&mut self.acks, &line);
    }
}

struct Recorded<'a> {
    inner: &'a mut dyn Sql,
    ledger: std::fs::File,
    tag: String,
}

impl Sql for Recorded<'_> {
    fn query(
        &mut self,
        statement: &Statement,
        values: &[SqlValue],
    ) -> Result<Vec<Vec<Option<String>>>, SqlError> {
        let rows = self.inner.query(statement, values)?;
        if statement.name == harness::ACQUIRE.name
            && let [row] = rows.as_slice()
        {
            let index = statement
                .returns
                .iter()
                .position(|name| *name == "epoch")
                .expect("acquire returns the epoch");
            if let Some(epoch) = row
                .get(index)
                .and_then(|value| value.as_deref())
                .and_then(|text| text.parse().ok())
            {
                checks::append_synced(&mut self.ledger, &checks::acquired_line(&self.tag, epoch));
            }
        }
        Ok(rows)
    }

    fn rollback(&mut self) {
        self.inner.rollback();
    }
}

enum Store {
    Sqlite(SqliteStore),
    Postgres(PostgresStore),
}

impl Store {
    fn sql(&mut self) -> &mut dyn Sql {
        match self {
            Self::Sqlite(store) => store,
            Self::Postgres(store) => store,
        }
    }
}

fn open_store(location: &str, application: &str, pid_file: Option<&str>) -> Result<Store, String> {
    match harness::DIALECT {
        DIALECT_SQLITE => SqliteStore::open(location, BUSY_TIMEOUT_MS).map(Store::Sqlite),
        DIALECT_POSTGRES => {
            assert_eq!(
                harness::FENCED_ERROR,
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
            Ok(Store::Postgres(store))
        }
        other => panic!("the drill host has no store for dialect {other}"),
    }
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
}

fn usage(problem: &str) -> ! {
    eprintln!("{problem}");
    eprintln!(
        "usage: sql_drill_host run store=S session=S owner=O lease_ms=N ledger=F acks=F seed=N turns=N [pause=never|flush|external:N pause_dir=D] [pid_file=F]\n       sql_drill_host verify store=S session=S ledger=F acks=F turns=N taken_over=N"
    );
    std::process::exit(EXIT_USAGE);
}

fn request(seed: u64, turn: u64) -> TurnRequest {
    TurnRequest {
        turn,
        arguments: Arguments::Prompt(format!("drill-{seed}-{turn}")),
    }
}

fn exit_for(turn: u64, error: RunError) -> i32 {
    match error {
        RunError::Open(OpenError::Held) => EXIT_HELD,
        RunError::Open(OpenError::Sql(error)) => {
            eprintln!("turn {turn}: store lost: {error}");
            EXIT_STORE_LOST
        }
        RunError::Open(OpenError::Corrupt(error)) => panic!("turn {turn}: {error}"),
        RunError::Exit(harness::Exit::Stop(Stop::Fenced)) => EXIT_FENCED,
        RunError::Exit(harness::Exit::Stop(Stop::Crash)) => {
            eprintln!("turn {turn}: store lost during a flush");
            EXIT_STORE_LOST
        }
        RunError::Exit(harness::Exit::Corrupt(error)) => panic!("turn {turn}: {error}"),
    }
}

fn drill(options: &Options, tag: &str) -> Drill {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_nanos() as u64;
    let pause = options.pause();
    Drill {
        ledger: checks::open_append(options.text("ledger")),
        acks: checks::open_append(options.text("acks")),
        rng: Rng(options.number("seed") ^ started),
        clock: 0,
        tag: tag.to_owned(),
        owner: options.text("owner").to_owned(),
        pause_dir: if pause == Pause::Never {
            String::new()
        } else {
            options.text("pause_dir").to_owned()
        },
        paused: Paused::Waiting(pause),
        externals: 0,
    }
}

fn run(options: &Options) -> i32 {
    let tag = checks::run_tag(options.text("owner"));
    let mut host = drill(options, &tag);
    checks::append_synced(&mut host.ledger, &checks::run_line(&tag));
    let mut store = match open_store(
        options.text("store"),
        &tag,
        options.0.get("pid_file").map(String::as_str),
    ) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("store lost: {error}");
            return EXIT_STORE_LOST;
        }
    };
    if let Err(error) = harness::sql_prepare(store.sql()) {
        eprintln!("store lost: {error}");
        return EXIT_STORE_LOST;
    }
    let mut journal = SqlJournal {
        session: options.text("session").to_owned(),
        owner: options.text("owner").to_owned(),
        lease_ms: options.number("lease_ms"),
        head: 0,
        last_turn: 0,
    };
    let ledger = host.ledger.try_clone().expect("the ledger handle clones");
    let mut sql = Recorded {
        inner: store.sql(),
        ledger,
        tag: tag.clone(),
    };
    let seed = options.number("seed");
    for turn in 1..=options.number("turns") {
        match harness::run_sql(&mut host, &mut sql, &mut journal, &request(seed, turn)) {
            Ok(Ran::Finished(_) | Ran::AlreadyFinished(_)) => {}
            Ok(ran @ (Ran::Suspended(_) | Ran::NotDue(_))) => panic!("turn {turn}: the drill program neither suspends nor idles, found {ran:?}"),
            Err(error) => {
                let status = exit_for(turn, error);
                if status == EXIT_FENCED {
                    checks::append_synced(&mut host.ledger, &checks::taken_over_line(&tag));
                }
                return status;
            }
        }
    }
    checks::append_synced(&mut host.ledger, &checks::finished_line(&tag));
    0
}

fn load(sql: &mut dyn Sql, session: &str) -> Result<Vec<harness::Record>, SqlError> {
    let values = [SqlValue::Text(session.to_owned())];
    let rows = sql.query(&harness::LOAD, &values)?;
    let seq_index = harness::LOAD
        .returns
        .iter()
        .position(|name| *name == "seq")
        .expect("load returns seq");
    let record_index = harness::LOAD
        .returns
        .iter()
        .position(|name| *name == "record")
        .expect("load returns record");
    let mut records = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let seq = row.get(seq_index).and_then(|value| value.as_deref());
        assert_eq!(
            seq,
            Some((index + 1).to_string().as_str()),
            "rows load in seq order without gaps"
        );
        let text = row
            .get(record_index)
            .and_then(|value| value.as_deref())
            .expect("a row has a record");
        let record = harness::decode_line(text, false)
            .unwrap_or_else(|error| panic!("row {}: {error}", index + 1));
        records.push(record);
    }
    Ok(records)
}

fn integrity(store: &mut Store) -> &'static str {
    let Store::Sqlite(sqlite) = store else {
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
    let mut store = match open_store(options.text("store"), "verify", None) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("store lost: {error}");
            return EXIT_STORE_LOST;
        }
    };
    let integrity = integrity(&mut store);
    let records = match load(store.sql(), options.text("session")) {
        Ok(records) => records,
        Err(error) => {
            eprintln!("store lost: load: {error:?}");
            return EXIT_STORE_LOST;
        }
    };
    let turns = options.number("turns");
    let taken_over = options.number("taken_over") as usize;
    let verified = checks::check_all(
        &records,
        options.text("ledger"),
        options.text("acks"),
        turns,
        taken_over,
    );
    println!(
        "verified turns={turns} runs={} records={} external={} unknown={} other={} dispatches={} other_dispatches_lost={} acks={} acquires={} takeovers={} taken_over={taken_over} integrity={integrity}",
        verified.runs,
        records.len(),
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
    harness::self_check();
    let _ = program::HARNESS_NAME;
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
                "dialect={} sqlite={}",
                harness::DIALECT,
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
        "read" => vec![("path", Json::String(format!("input-{index}")))],
        "edit" => vec![("path", Json::String(format!("input-{index}"))), ("body", Json::String(format!("body-{index}")))],
        "bash" => vec![("command", Json::String(format!("input-{index}")))],
        _ => return format!("input-{index}"),
    };
    Json::object(fields).encode()
}
