mod children;
mod codex;
mod control;
mod host;
mod jobs;
mod json;
mod ledger;
mod tools;
mod transport;
mod wire;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use harnesslang::run::journal::Record;
use harnesslang::wasm::js::{Json, parse};
use harnesslang::wasm::sim::Loaded;

use host::{Ran, Session, Setup, Spawn};

pub const EXIT_HARNESS_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
const USAGE: &str = "usage: hl-host run --module <file.wasm> --journal <dir> --handler <name> --args <json> [--turn N] [--cwd <dir>] [--module-dir <dir>] [--spawn <tool>=<program>:<handler>]...\n       hl-host run --instruction <file> --cwd <dir> --model <id> --journal <dir> [--harness claude|codex] [--max-turns N] [--effort <level>] [--reviewer-relay <dir>] [--finish-checks N] [--keep-warm N] [--turn N] [--module-dir <dir>]\n       hl-host steer --journal <dir> --text <message>\n       hl-host cancel --journal <dir>";
const LABEL: &str = "hl-host";
const HARNESS_CLAUDE: &str = "claude";
const HARNESS_CODEX: &str = "codex";
const CLAUDE_MODEL_PREFIX: &str = "claude";
const PROGRAM_CLAUDE: &str = "claude_code";
const PROGRAM_CLAUDE_CHILD: &str = "claude_code_child";
const PROGRAM_CODEX: &str = "codex_agent";
const CLAUDE_SPAWN_TOOL: &str = "spawn_agent";
const CLAUDE_SPAWN_HANDLER: &str = "delegated";
const PROMPT_HANDLER: &str = "prompt";
const REFRESH_HANDLER: &str = "refresh";
const EFFORT_DEFAULT: &str = "default";
const FINISH_CHECKS_DEFAULT: u64 = 2;
const ROUNDS_DEFAULT: u64 = u32::MAX as u64;
const KEEP_WARM_MAX: u64 = 64;
const IDLE_REQUESTS_PER_REFRESH_MAX: u64 = 2;
const FIRST_TURN: u64 = 1;

pub fn now_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970");
    u64::try_from(elapsed.as_millis()).expect("milliseconds since 1970 fit in u64")
}

pub fn random() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970")
        .subsec_nanos();
    u64::from(nanos) ^ now_ms().rotate_left(u32::BITS)
}

fn fail_usage(message: &str) -> ! {
    eprintln!("{LABEL}: {message}\n{USAGE}");
    std::process::exit(EXIT_USAGE)
}

fn fail(message: &str) -> ! {
    eprintln!("{LABEL}: {message}");
    std::process::exit(EXIT_HARNESS_ERROR)
}

fn number_flag(flag: &str, value: &str) -> u64 {
    value
        .parse()
        .unwrap_or_else(|_| fail_usage(&format!("{flag} {value} is not a number")))
}

#[derive(Default)]
struct Flags {
    module: Option<PathBuf>,
    module_dir: Option<PathBuf>,
    journal: Option<PathBuf>,
    handler: Option<String>,
    args: Option<String>,
    turn: Option<u64>,
    cwd: Option<PathBuf>,
    spawns: Vec<Spawn>,
    instruction: Option<PathBuf>,
    model: Option<String>,
    harness: Option<String>,
    max_turns: Option<u64>,
    effort: Option<String>,
    reviewer: Option<String>,
    finish_checks: Option<u64>,
    keep_warm: u64,
    text: Option<String>,
}

fn spawn_flag(value: &str) -> Spawn {
    let parsed = value.split_once('=').and_then(|(tool, rest)| {
        let (program, handler) = rest.split_once(':')?;
        Some(Spawn {
            tool: tool.to_owned(),
            program: program.to_owned(),
            handler: handler.to_owned(),
        })
    });
    parsed.unwrap_or_else(|| {
        fail_usage(&format!(
            "--spawn {value} is not <tool>=<program>:<handler>"
        ))
    })
}

fn flags(args: &[String]) -> Flags {
    let mut flags = Flags::default();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .unwrap_or_else(|| fail_usage(&format!("{flag} needs a value")))
            .clone();
        match flag.as_str() {
            "--module" => flags.module = Some(PathBuf::from(value)),
            "--module-dir" => flags.module_dir = Some(PathBuf::from(value)),
            "--journal" => flags.journal = Some(PathBuf::from(value)),
            "--handler" => flags.handler = Some(value),
            "--args" => flags.args = Some(value),
            "--turn" => flags.turn = Some(number_flag(flag, &value)),
            "--cwd" => flags.cwd = Some(PathBuf::from(value)),
            "--spawn" => flags.spawns.push(spawn_flag(&value)),
            "--instruction" => flags.instruction = Some(PathBuf::from(value)),
            "--model" => flags.model = Some(value),
            "--harness" => flags.harness = Some(value),
            "--max-turns" => flags.max_turns = Some(number_flag(flag, &value)),
            "--effort" => flags.effort = Some(value),
            "--reviewer-relay" => flags.reviewer = Some(value),
            "--finish-checks" => flags.finish_checks = Some(number_flag(flag, &value)),
            "--keep-warm" => flags.keep_warm = number_flag(flag, &value),
            "--text" => flags.text = Some(value),
            other => fail_usage(&format!("unknown flag {other}")),
        }
    }
    if flags.turn == Some(0) {
        fail_usage("--turn counts from 1");
    }
    if flags.keep_warm > KEEP_WARM_MAX {
        fail_usage(&format!(
            "--keep-warm is at most KEEP_WARM_MAX ({KEEP_WARM_MAX})"
        ));
    }
    flags
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or_default();
    let flags = flags(args.get(1..).unwrap_or_default());
    match command {
        "steer" | "cancel" => send_event(command, &flags),
        "run" if flags.instruction.is_some() => front(flags),
        "run" => generic(flags),
        _ => fail_usage("the commands are run, steer and cancel"),
    }
}

fn send_event(command: &str, flags: &Flags) -> ! {
    let journal = flags
        .journal
        .as_ref()
        .unwrap_or_else(|| fail_usage("--journal is required"));
    let steer = match command {
        "steer" => Some(
            flags
                .text
                .as_deref()
                .unwrap_or_else(|| fail_usage("steer needs --text")),
        ),
        _ => None,
    };
    match control::send(journal, steer) {
        Ok(name) => {
            println!("{name}");
            std::process::exit(0)
        }
        Err(error) => fail(&error),
    }
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn open(flags: &Flags, module_dir: PathBuf, spawns: Vec<Spawn>) -> (PathBuf, Session) {
    let journal = flags
        .journal
        .clone()
        .unwrap_or_else(|| fail_usage("--journal is required"));
    std::fs::create_dir_all(&journal)
        .unwrap_or_else(|error| fail(&format!("{}: {error}", journal.display())));
    let cwd = flags.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
    let cwd = std::fs::canonicalize(&cwd)
        .unwrap_or_else(|error| fail_usage(&format!("--cwd {}: {error}", cwd.display())));
    let setup = Setup {
        module_dir,
        cwd,
        spawns,
    };
    let cancel = control::directory(&journal);
    let session =
        Session::open(setup, &journal, LABEL, &cancel).unwrap_or_else(|error| fail(&error));
    (journal, session)
}

fn generic(flags: Flags) {
    let module = flags
        .module
        .clone()
        .unwrap_or_else(|| fail_usage("run needs --module or --instruction"));
    let handler = flags
        .handler
        .clone()
        .unwrap_or_else(|| fail_usage("--handler is required"));
    let arguments = parse(flags.args.as_deref().unwrap_or("{}"))
        .unwrap_or_else(|error| fail_usage(&format!("--args: {error}")));
    let module_dir = flags.module_dir.clone().unwrap_or_else(|| {
        module
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let loaded = host::load(&module).unwrap_or_else(|error| fail(&error));
    let (journal, mut session) = open(&flags, module_dir, flags.spawns.clone());
    let turn = flags.turn.unwrap_or_else(|| {
        planned_turn(&session.journal_path(), &handler, &arguments)
            .unwrap_or_else(|error| fail(&error))
    });
    if handler == host::CONFIGURE_HANDLER {
        session
            .remember_configure(&arguments)
            .unwrap_or_else(|error| fail(&error));
    }
    session.ledger.mark("start", now_ms());
    let ran = host::run_turn(
        &loaded,
        &mut session,
        &host::request(&handler, turn, arguments),
    );
    session.close();
    session.ledger.mark("end", now_ms());
    let model = text_argument(flags.args.as_deref(), "model");
    let (finish, code) = report(&ran, true);
    if let Err(error) = ledger::write_usage(&journal, &model, &finish) {
        fail(&error);
    }
    std::process::exit(code);
}

fn text_argument(args: Option<&str>, name: &str) -> String {
    args.and_then(|text| parse(text).ok())
        .and_then(|json| match json.get(name) {
            Json::Text(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn report(ran: &Result<Ran, String>, raw: bool) -> (String, i32) {
    match ran {
        Ok(Ran::Finished(finish)) if finish.completed => {
            let (name, text) = host::outcome_of(&finish.result);
            if raw {
                println!("{}", finish.result.encode());
            } else {
                println!("{text}");
            }
            (name, 0)
        }
        Ok(Ran::Finished(finish)) => {
            eprintln!(
                "{LABEL}: the program trapped in turn {}: {}",
                finish.turn,
                finish.result.encode()
            );
            ("trap".to_owned(), EXIT_HARNESS_ERROR)
        }
        Ok(Ran::Suspended { turn, seq }) => {
            eprintln!(
                "{LABEL}: turn {turn} waits on suspend {seq}, which this host could not resume"
            );
            ("suspended".to_owned(), EXIT_HARNESS_ERROR)
        }
        Ok(Ran::NotDue) => {
            eprintln!("{LABEL}: the turn was refused as an idle turn");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
        Err(error) => {
            eprintln!("{LABEL}: {error}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
    }
}

struct Last {
    turn: u64,
    handler: String,
    arguments: harnesslang::run::json::Json,
    finished: bool,
}

fn last_turn(path: &Path) -> Result<Option<Last>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let mut last: Option<Last> = None;
    for record in ledger::records(path)? {
        match record {
            Record::Admitted {
                turn,
                handler,
                arguments,
            } => {
                last = Some(Last {
                    turn,
                    handler,
                    arguments,
                    finished: false,
                })
            }
            Record::Status { turn, .. } => {
                if let Some(held) = last.as_mut().filter(|held| held.turn == turn) {
                    held.finished = true;
                }
            }
            _ => {}
        }
    }
    Ok(last)
}

fn same(held: &Last, handler: &str, arguments: &Json) -> bool {
    held.handler == handler
        && harnesslang::run::json::decode(&arguments.encode())
            .ok()
            .as_ref()
            == Some(&held.arguments)
}

fn planned_turn(path: &Path, handler: &str, arguments: &Json) -> Result<u64, String> {
    Ok(match last_turn(path)? {
        None => FIRST_TURN,
        Some(held) if !held.finished && same(&held, handler, arguments) => held.turn,
        Some(held) => held.turn + 1,
    })
}

fn front(flags: Flags) {
    let model = flags
        .model
        .clone()
        .unwrap_or_else(|| fail_usage("--model is required"));
    let cwd = flags
        .cwd
        .clone()
        .unwrap_or_else(|| fail_usage("--cwd is required"));
    let harness = flags.harness.clone().unwrap_or_else(|| {
        if model.starts_with(CLAUDE_MODEL_PREFIX) {
            HARNESS_CLAUDE.to_owned()
        } else {
            HARNESS_CODEX.to_owned()
        }
    });
    let instruction_path = flags
        .instruction
        .clone()
        .expect("the front runs with --instruction");
    let instruction = std::fs::read_to_string(&instruction_path)
        .unwrap_or_else(|error| fail_usage(&format!("{}: {error}", instruction_path.display())));
    let cwd_text = std::fs::canonicalize(&cwd)
        .unwrap_or_else(|error| fail_usage(&format!("--cwd {}: {error}", cwd.display())))
        .display()
        .to_string();
    let effort = flags
        .effort
        .clone()
        .unwrap_or_else(|| EFFORT_DEFAULT.to_owned());
    let (program, configure, spawns) = match harness.as_str() {
        HARNESS_CLAUDE => {
            if flags.reviewer.is_some() || flags.finish_checks.is_some() {
                fail_usage("--reviewer-relay and --finish-checks are for the Codex harness");
            }
            let configure = host::object(vec![
                ("cwd", Json::text(&cwd_text)),
                ("effort", Json::text(&effort)),
                ("model", Json::text(&model)),
            ]);
            let spawn = Spawn {
                tool: CLAUDE_SPAWN_TOOL.to_owned(),
                program: PROGRAM_CLAUDE_CHILD.to_owned(),
                handler: CLAUDE_SPAWN_HANDLER.to_owned(),
            };
            (PROGRAM_CLAUDE, configure, vec![spawn])
        }
        HARNESS_CODEX => {
            if flags.keep_warm > 0 {
                fail_usage("--keep-warm is for the Claude harness");
            }
            let configure = host::object(vec![
                ("cwd", Json::text(&cwd_text)),
                ("effort", Json::text(&effort)),
                (
                    "finish_checks",
                    Json::int(flags.finish_checks.unwrap_or(FINISH_CHECKS_DEFAULT)),
                ),
                ("model", Json::text(&model)),
                (
                    "reviewer",
                    Json::text(flags.reviewer.as_deref().unwrap_or_default()),
                ),
            ]);
            (PROGRAM_CODEX, configure, Vec::new())
        }
        other => fail_usage(&format!("--harness {other} is not claude or codex")),
    };
    let module_dir = flags.module_dir.clone().unwrap_or_else(exe_dir);
    let module = host::module_path(&module_dir, program);
    let loaded = host::load(&module).unwrap_or_else(|error| fail(&error));
    let (journal, mut session) = open(&flags, module_dir, spawns);
    let prompt = host::object(vec![
        ("input", Json::text(&instruction)),
        (
            "rounds",
            Json::int(flags.max_turns.unwrap_or(ROUNDS_DEFAULT)),
        ),
    ]);
    let path = session.journal_path();
    let held = last_turn(&path).unwrap_or_else(|error| fail(&error));
    let (configure_turn, prompt_turn) = match (flags.turn, held) {
        (Some(turn), _) => (Some(turn), turn + 1),
        (None, Some(held)) if !held.finished && same(&held, PROMPT_HANDLER, &prompt) => {
            (None, held.turn)
        }
        (None, Some(held))
            if !held.finished && same(&held, host::CONFIGURE_HANDLER, &configure) =>
        {
            (Some(held.turn), held.turn + 1)
        }
        (None, Some(held)) => (Some(held.turn + 1), held.turn + 2),
        (None, None) => (Some(FIRST_TURN), FIRST_TURN + 1),
    };
    session
        .remember_configure(&configure)
        .unwrap_or_else(|error| fail(&error));
    session.ledger.mark("start", now_ms());
    if let Some(turn) = configure_turn {
        let request = host::request(host::CONFIGURE_HANDLER, turn, configure);
        match host::run_turn(&loaded, &mut session, &request) {
            Ok(Ran::Finished(finish)) if finish.completed => {
                let (name, reason) = host::outcome_of(&finish.result);
                if name == "failed" {
                    session.close();
                    fail_usage(&reason);
                }
            }
            other => {
                session.close();
                let (_, _) = report(&other, false);
                fail("the configure turn did not complete");
            }
        }
    }
    let request = host::request(PROMPT_HANDLER, prompt_turn, prompt);
    let ran = host::run_turn(&loaded, &mut session, &request);
    if flags.keep_warm > 0 && matches!(&ran, Ok(Ran::Finished(_))) {
        keep_warm(&loaded, &mut session, flags.keep_warm);
    }
    session.close();
    session.ledger.mark("end", now_ms());
    let (finish, code) = report(&ran, false);
    if let Err(error) = ledger::write_usage(&journal, &model, &finish) {
        fail(&error);
    }
    std::process::exit(code);
}

fn keep_warm(loaded: &Loaded, session: &mut Session, refreshes: u64) {
    let mut refreshed = 0;
    for _ in 0..refreshes * IDLE_REQUESTS_PER_REFRESH_MAX {
        if refreshed == refreshes {
            return;
        }
        let due = match host::idle_due(loaded, session) {
            Ok(Some(due)) => due,
            Ok(None) => {
                eprintln!("{LABEL}: no idle handler is armed");
                return;
            }
            Err(error) => {
                eprintln!("{LABEL}: journal: {error}");
                return;
            }
        };
        let turn = match host::next_turn(&session.journal_path()) {
            Ok(turn) => turn,
            Err(error) => {
                eprintln!("{LABEL}: journal: {error}");
                return;
            }
        };
        let now = now_ms();
        if due > now {
            eprintln!(
                "{LABEL}: idle {} ms until the idle turn due at {due}",
                due - now
            );
            std::thread::sleep(std::time::Duration::from_millis(due - now));
        }
        let request = host::request(REFRESH_HANDLER, turn, Json::Object(Vec::new()));
        match host::run_once(loaded, session, &request) {
            Ok(Ran::Finished(finish)) => {
                refreshed += 1;
                eprintln!("{LABEL}: idle turn {turn}: {}", finish.result.encode());
            }
            Ok(Ran::NotDue) => eprintln!("{LABEL}: idle turn {turn} is not due"),
            Ok(Ran::Suspended { seq, .. }) => {
                eprintln!("{LABEL}: idle turn {turn} suspended at {seq}");
                return;
            }
            Err(error) => {
                eprintln!("{LABEL}: idle turn {turn}: {error}");
                return;
            }
        }
    }
}
