#[path = "sql_drill_checks.rs"]
mod checks;

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

use checks::{append_synced, open_append};
use harness::EffectClass;
use harnesslang::wasm::js::Json;
use harnesslang::wasm::native::{self, Journal, World};

const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
const SPLITMIX_MIX_FIRST: u64 = 0xbf58_476d_1ce4_e5b9;
const SPLITMIX_MIX_SECOND: u64 = 0x94d0_49bb_1331_11eb;
const PPM: u64 = 1_000_000;
const ANSWER_PPM: u64 = 350_000;
const UNDECLARED_TOOL_PPM: u64 = 250_000;
const TOOL_FAILURE_PPM: u64 = 150_000;
const UNDECLARED_TOOLS: [&str; 2] = ["web_fetch", "spawn_agent"];
const DECLARED_TOOLS: [&str; 3] = ["read", "edit", "bash"];
const RUN_OWNER: &str = "jsonl";
const JOURNAL_CHECKSUMS: bool = true;

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

struct Drill {
    ledger: std::fs::File,
    acks: std::fs::File,
    rng: Rng,
    clock: u64,
    tag: String,
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

impl World for Drill {
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
        append_synced(&mut self.ledger, &line);
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
        append_synced(
            &mut self.acks,
            &format!("{turn} {outcome} {}\n", finish.get("result").encode()),
        );
        Ok(())
    }
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

fn run(module: &str, journal: &str, ledger: &str, acks: &str, seed: u64, turns: u64) {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_nanos() as u64;
    let bytes =
        std::fs::read(module).unwrap_or_else(|error| panic!("cannot read {module}: {error}"));
    let loaded = harnesslang::wasm::sim::load(&bytes).unwrap_or_else(|error| panic!("{error}"));
    let mut drill = Drill {
        ledger: open_append(ledger),
        acks: open_append(acks),
        rng: Rng(seed ^ started),
        clock: 0,
        tag: checks::run_tag(RUN_OWNER),
    };
    append_synced(&mut drill.ledger, &checks::run_line(&drill.tag));
    let first = acknowledged_last(acks) + 1;
    let mut file =
        Journal::open(std::path::Path::new(journal)).unwrap_or_else(|error| panic!("{error}"));
    for turn in first..=turns {
        let exit = native::run(&loaded, &mut file, &turn_request(seed, turn), &mut drill)
            .unwrap_or_else(|error| panic!("turn {turn}: {error}"));
        match exit.get("exit") {
            Json::Text(kind) if kind == "finished" => {}
            Json::Text(kind) if kind == "already_finished" => drill
                .acknowledge(exit.get("finish"))
                .unwrap_or_else(|error| panic!("{error}")),
            _ => panic!(
                "turn {turn}: the drill program finishes every turn, found {}",
                exit.encode()
            ),
        }
    }
    append_synced(&mut drill.ledger, &checks::finished_line(&drill.tag));
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

fn verify(journal: &str, ledger: &str, acks: &str, turns: u64) {
    let bytes =
        std::fs::read(journal).unwrap_or_else(|error| panic!("cannot read {journal}: {error}"));
    let decoded = harnesslang::run::journal::decode_lines(&bytes, JOURNAL_CHECKSUMS)
        .unwrap_or_else(|error| panic!("the journal decodes: {error}"));
    assert_eq!(
        decoded.dropped_bytes, 0,
        "a finished run leaves no torn tail"
    );
    let verified = checks::check_all(&decoded.records, ledger, acks, turns, 0);
    println!(
        "verified turns={turns} runs={} records={} external={} unknown={} other={} dispatches={} other_dispatches_lost_in_a_kill={} acks={}",
        verified.runs,
        decoded.records.len(),
        verified.external,
        verified.unknown,
        verified.other,
        verified.dispatches,
        verified.other_lost,
        verified.acks
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let number = |text: &str| -> u64 {
        text.parse()
            .unwrap_or_else(|_| panic!("{text} is not a number"))
    };
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["run", module, journal, ledger, acks, seed, turns] => {
            run(module, journal, ledger, acks, number(seed), number(turns))
        }
        ["verify", journal, ledger, acks, turns] => verify(journal, ledger, acks, number(turns)),
        _ => {
            eprintln!(
                "usage: module_drill_host run MODULE JOURNAL LEDGER ACKS SEED TURNS | verify JOURNAL LEDGER ACKS TURNS"
            );
            std::process::exit(2);
        }
    }
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
