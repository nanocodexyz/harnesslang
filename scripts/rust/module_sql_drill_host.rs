#[path = "sql_drill_checks.rs"]
mod checks;
#[path = "../../hosts/native/src/sql.rs"]
mod sql;
#[path = "../../hosts/native/src/store_postgres.rs"]
mod store_postgres;
#[path = "../../hosts/native/src/store_sqlite.rs"]
mod store_sqlite;

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
}

use std::collections::BTreeMap;

use harness::EffectClass;
use harnesslang::emit::sql as catalogue;
use harnesslang::target::Target;
use harnesslang::wasm::js::Json;
use harnesslang::wasm::native::{self, World};
use sql::{Database, Failure, Lease, Opened, Sql, Statement};

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
const PAUSE_POLL_MS: u64 = 2;
const PAUSE_POLLS_MAX: u64 = 30_000;
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

fn target_of(options: &Options) -> Target {
    let name = options.text("target");
    match Target::parse(name) {
        Some(target @ (Target::NativeSqlite | Target::ServerPostgres)) => target,
        _ => usage(&format!(
            "target={name} is not native-sqlite or server-postgres"
        )),
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
    let lease = Lease {
        session: options.text("session").to_owned(),
        owner: options.text("owner").to_owned(),
        lease_ms: options.number("lease_ms"),
    };
    let opened = sql::open_journal(
        target,
        options.text("store"),
        &tag,
        lease,
        options.mutation(),
    );
    let mut store = match opened {
        Ok(Opened::Owned(store)) => store,
        Ok(Opened::Held) => return EXIT_HELD,
        Err(error) => return store_lost(&error),
    };
    if let (Some(path), Database::Postgres(postgres)) = (options.0.get("pid_file"), &store.db) {
        let partial = format!("{path}.partial");
        std::fs::write(&partial, postgres.backend_pid.to_string())
            .unwrap_or_else(|error| panic!("{partial}: {error}"));
        std::fs::rename(&partial, path).unwrap_or_else(|error| panic!("{path}: {error}"));
    }
    let pause_hook = || {
        let mut pauser = pauser.borrow_mut();
        if pauser.waiting == Pause::FirstFlush {
            pauser.pause_here();
        }
    };
    store.before_append = Some(&pause_hook);
    checks::append_synced(&mut drill.ledger, &checks::acquired_line(&tag, store.epoch));
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
    let mut database = match sql::connect(target, options.text("store"), "verify") {
        Ok(database) => database,
        Err(error) => return store_lost(&error),
    };
    let integrity = integrity(&mut database);
    let text = match sql::read_lines(target, options.text("store"), options.text("session")) {
        Ok(text) => text,
        Err(error) => return store_lost(&error),
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
                store_sqlite::version().unwrap_or_else(|error| panic!("{error}"))
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
