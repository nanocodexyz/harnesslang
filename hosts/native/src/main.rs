mod children;
mod codex;
mod control;
mod host;
mod jobs;
mod json;
mod ledger;
mod sql;
mod store;
mod store_postgres;
mod store_sqlite;
mod texts;
mod tools;
mod transport;
mod wire;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use harnesslang::run::journal::Record;
use harnesslang::wasm::js::{Json, parse};
use harnesslang::wasm::sim::Loaded;

use host::{Ran, Session, Setup, Spawn};
use texts::Texts;

pub const EXIT_HARNESS_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
const USAGE: &str = "usage: hl-host run --module <file.wasm> --journal <dir> [--store jsonl|sqlite:<file>|postgres:<host:port>/<database>] [--session <key>] --handler <name> --args <json> [--turn N] [--cwd <dir>] [--module-dir <dir>] [--spawn <tool>=<program>:<handler>]...\n       hl-host run --journal <dir> [--store <store>] [--session <key>] [--cwd <dir>] [--turn N] [--module-dir <dir>] [--<flag> <value>]...\n       hl-host wire --request <file.json> --scratch <dir>\n       hl-host steer --journal <dir> --text <message>\n       hl-host cancel --journal <dir>";
const LABEL: &str = "hl-host";
const ENTRY_PROGRAM: &str = "entry";
const ENTRY_HANDLER: &str = "launch";
const ENTRY_LAUNCH: &str = "launch";
const ENTRY_REFUSED: &str = "refused";
const FILE_KEY: &str = "$file";
const NUMBER_KEY: &str = "$number";
const STEPS_MAX: usize = 8;
const IDLE_TURNS_MAX: u64 = 64;
const IDLE_REQUESTS_PER_TURN_MAX: u64 = 2;
const FIRST_TURN: u64 = 1;
const NO_HANDLER: &str = "no handler";
const ARGUMENTS_REFUSED: &str = "a turn request has a turn, a handler and arguments";

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
    text: Option<String>,
    request: Option<PathBuf>,
    scratch: Option<PathBuf>,
    store: Option<String>,
    session: Option<String>,
    rest: Vec<(String, String)>,
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
            "--text" => flags.text = Some(value),
            "--request" => flags.request = Some(PathBuf::from(value)),
            "--scratch" => flags.scratch = Some(PathBuf::from(value)),
            "--store" => flags.store = Some(value),
            "--session" => flags.session = Some(value),
            other if other.starts_with("--") => {
                flags.rest.retain(|(name, _)| name != other);
                flags.rest.push((other.to_owned(), value));
            }
            other => fail_usage(&format!("{other} is not a flag")),
        }
    }
    if flags.turn == Some(0) {
        fail_usage("--turn counts from 1");
    }
    flags
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or_default();
    let flags = flags(args.get(1..).unwrap_or_default());
    match command {
        "steer" | "cancel" => send_event(command, &flags),
        "wire" => wire_once(&flags),
        "run" if flags.module.is_some() => generic(flags),
        "run" => launch(flags),
        _ => fail_usage("the commands are run, wire, steer and cancel"),
    }
}

fn refuse_rest(flags: &Flags, known: &[String]) {
    if let Some((name, _)) = flags.rest.iter().find(|(name, _)| !known.contains(name)) {
        fail_usage(&format!("unknown flag {name}"));
    }
}

fn send_event(command: &str, flags: &Flags) -> ! {
    refuse_rest(flags, &[]);
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

fn wire_once(flags: &Flags) -> ! {
    refuse_rest(flags, &[]);
    let path = flags
        .request
        .as_ref()
        .unwrap_or_else(|| fail_usage("wire needs --request"));
    let scratch = flags
        .scratch
        .as_ref()
        .unwrap_or_else(|| fail_usage("wire needs --scratch"));
    std::fs::create_dir_all(scratch)
        .unwrap_or_else(|error| fail(&format!("{}: {error}", scratch.display())));
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| fail_usage(&format!("{}: {error}", path.display())));
    let http =
        parse(&text).unwrap_or_else(|error| fail_usage(&format!("{}: {error}", path.display())));
    println!("{}", wire::call(&http, scratch, &|| false).encode());
    std::process::exit(0)
}

fn canonical_cwd(flags: &Flags) -> PathBuf {
    let cwd = flags.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
    std::fs::canonicalize(&cwd)
        .unwrap_or_else(|error| fail_usage(&format!("--cwd {}: {error}", cwd.display())))
}

fn open(
    flags: &Flags,
    module_dir: Option<PathBuf>,
    spawns: Vec<Spawn>,
    texts: Texts,
) -> (PathBuf, Session) {
    let journal = flags
        .journal
        .clone()
        .unwrap_or_else(|| fail_usage("--journal is required"));
    std::fs::create_dir_all(&journal)
        .unwrap_or_else(|error| fail(&format!("{}: {error}", journal.display())));
    let journal = std::fs::canonicalize(&journal)
        .unwrap_or_else(|error| fail(&format!("{}: {error}", journal.display())));
    let setup = Setup {
        texts,
        module_dir,
        cwd: canonical_cwd(flags),
        spawns,
    };
    let cancel = control::directory(&journal);
    let mut session =
        Session::open(setup, &journal, LABEL, &cancel).unwrap_or_else(|error| fail(&error));
    let key = flags
        .session
        .clone()
        .unwrap_or_else(|| journal.display().to_string());
    session.store = store::parse(flags.store.as_deref().unwrap_or("jsonl"), &key)
        .unwrap_or_else(|error| fail_usage(&error));
    (journal, session)
}

struct Step {
    handler: String,
    arguments: Json,
    refused: String,
}

struct Plan {
    texts: Texts,
    model: String,
    raw: bool,
    steps: Vec<Step>,
    idle: Option<(String, u64)>,
}

fn generic(flags: Flags) {
    refuse_rest(&flags, &[]);
    let module = flags.module.clone().expect("generic runs with --module");
    let handler = flags
        .handler
        .clone()
        .unwrap_or_else(|| fail_usage("--handler is required"));
    let arguments = parse(flags.args.as_deref().unwrap_or("{}"))
        .unwrap_or_else(|error| fail_usage(&format!("--args: {error}")));
    let module_dir = flags
        .module_dir
        .clone()
        .or_else(|| module.parent().map(Path::to_path_buf));
    let loaded = host::load(&module).unwrap_or_else(|error| fail(&error));
    let plan = Plan {
        texts: Texts::default(),
        model: text_of(arguments.get("model"))
            .unwrap_or_default()
            .to_owned(),
        raw: true,
        steps: vec![Step {
            handler,
            arguments,
            refused: String::new(),
        }],
        idle: None,
    };
    let spawns = flags.spawns.clone();
    run_plan(&flags, &loaded, module_dir, spawns, &plan);
}

fn text_of(json: &Json) -> Option<&str> {
    match json {
        Json::Text(text) => Some(text),
        _ => None,
    }
}

fn load_entry(module_dir: Option<&Path>) -> Loaded {
    let own = module_dir
        .map(|dir| host::module_path(dir, ENTRY_PROGRAM))
        .filter(|path| path.exists());
    let loaded = match own {
        Some(path) => host::load(&path),
        None => host::load_program(None, ENTRY_PROGRAM),
    };
    loaded.unwrap_or_else(|error| fail(&error))
}

fn entry_flags(flags: &Flags, cwd: Option<&Path>) -> String {
    let mut text = String::new();
    let cwd = cwd.map(|cwd| ("--cwd".to_owned(), cwd.display().to_string()));
    for (name, value) in cwd.iter().chain(flags.rest.iter()) {
        if value.contains('\n') {
            fail_usage(&format!("{name} holds a newline"));
        }
        text.push('\n');
        text.push_str(name);
        text.push('\n');
        text.push_str(value);
    }
    text.push('\n');
    text
}

fn manifest(flags: &Flags) -> Json {
    let cwd = flags.cwd.as_ref().map(|_| canonical_cwd(flags));
    let entry = load_entry(flags.module_dir.as_deref());
    let scratch = std::env::temp_dir().join(format!(
        "{LABEL}-{ENTRY_PROGRAM}-{}-{:x}",
        std::process::id(),
        random()
    ));
    let setup = Setup {
        texts: Texts::default(),
        module_dir: None,
        cwd: cwd.clone().unwrap_or_else(|| PathBuf::from(".")),
        spawns: Vec::new(),
    };
    let arguments = host::object(vec![(
        "flags",
        Json::Text(entry_flags(flags, cwd.as_deref())),
    )]);
    let ran = Session::open(setup, &scratch, LABEL, &control::directory(&scratch)).and_then(
        |mut session| {
            let request = host::request(ENTRY_HANDLER, FIRST_TURN, arguments);
            let ran = host::run_turn(&entry, &mut session, &request);
            session.close();
            ran
        },
    );
    if let Err(error) = std::fs::remove_dir_all(&scratch) {
        eprintln!("{LABEL}: {}: {error}", scratch.display());
    }
    let finish = match ran {
        Ok(Ran::Finished(finish)) if finish.completed => finish,
        other => {
            let (_, _) = report(&other, true);
            fail("the entry program did not launch");
        }
    };
    match host::outcome_of(&finish.result) {
        (name, text) if name == ENTRY_LAUNCH => parse(&text)
            .unwrap_or_else(|error| fail(&format!("the launch manifest is not JSON: {error}"))),
        (name, reason) if name == ENTRY_REFUSED => fail_usage(&reason),
        (name, text) => fail(&format!("the entry program answered {name}: {text}")),
    }
}

fn texts_list(json: &Json, what: &str) -> Vec<String> {
    match json {
        Json::Array(items) => items
            .iter()
            .map(|item| {
                text_of(item)
                    .unwrap_or_else(|| fail(&format!("the manifest's {what} are texts")))
                    .to_owned()
            })
            .collect(),
        _ => fail(&format!("the manifest has no {what}")),
    }
}

fn entries<'a>(json: &'a Json, what: &str) -> &'a [(String, Json)] {
    match json {
        Json::Object(fields) => fields,
        Json::Undefined => &[],
        _ => fail(&format!("the manifest's {what} are an object")),
    }
}

fn texts(manifest: &Json) -> Texts {
    let mut texts = Texts::default();
    for (name, value) in entries(manifest.get("texts"), "texts") {
        let template = text_of(value).unwrap_or_else(|| fail(&format!("text {name} is a text")));
        texts.templates.insert(name.clone(), template.to_owned());
    }
    for (name, value) in entries(manifest.get("numbers"), "numbers") {
        let number = value
            .bigint()
            .ok()
            .and_then(|number| u64::try_from(number).ok())
            .unwrap_or_else(|| fail(&format!("number {name} is a number")));
        texts.numbers.insert(name.clone(), number);
    }
    for (name, value) in entries(manifest.get("lists"), "lists") {
        texts.lists.insert(name.clone(), texts_list(value, name));
    }
    texts
}

fn field(json: &Json, key: &str) -> String {
    text_of(json.get(key))
        .unwrap_or_else(|| fail(&format!("the manifest has no text {key}")))
        .to_owned()
}

fn resolve(json: &Json) -> Json {
    match json {
        Json::Object(fields) if fields.len() == 1 && fields[0].0 == FILE_KEY => {
            let path = text_of(&fields[0].1).unwrap_or_else(|| fail("a $file names a path"));
            Json::Text(
                std::fs::read_to_string(path)
                    .unwrap_or_else(|error| fail_usage(&format!("{path}: {error}"))),
            )
        }
        Json::Object(fields) if fields.len() == 1 && fields[0].0 == NUMBER_KEY => {
            let value = text_of(&fields[0].1).unwrap_or_else(|| fail("a $number is a text"));
            Json::int(number_flag("the value", value))
        }
        Json::Object(fields) => {
            let mut fields: Vec<(String, Json)> = fields
                .iter()
                .map(|(key, value)| (key.clone(), resolve(value)))
                .collect();
            fields.sort_by(|left, right| left.0.cmp(&right.0));
            Json::Object(fields)
        }
        Json::Array(items) => Json::Array(items.iter().map(resolve).collect()),
        other => other.clone(),
    }
}

fn launch(flags: Flags) {
    let manifest = manifest(&flags);
    refuse_rest(&flags, &texts_list(manifest.get("flags"), "flags"));
    let spawns = match manifest.get("spawns") {
        Json::Array(items) => items
            .iter()
            .map(|item| Spawn {
                tool: field(item, "tool"),
                program: field(item, "program"),
                handler: field(item, "handler"),
            })
            .collect(),
        _ => fail("the manifest has no spawns"),
    };
    let steps: Vec<Step> = match manifest.get("steps") {
        Json::Array(items) if !items.is_empty() && items.len() <= STEPS_MAX => items
            .iter()
            .map(|item| Step {
                handler: field(item, "handler"),
                arguments: resolve(item.get("args")),
                refused: field(item, "refused"),
            })
            .collect(),
        _ => fail(&format!("the manifest has 1 to {STEPS_MAX} steps")),
    };
    let idle = match manifest.get("idle") {
        Json::Undefined | Json::Null => None,
        idle => {
            let turns = resolve(idle.get("turns"))
                .bigint()
                .ok()
                .and_then(|turns| u64::try_from(turns).ok())
                .unwrap_or_else(|| fail("the manifest's idle turns are a number"));
            if turns > IDLE_TURNS_MAX {
                fail_usage(&format!(
                    "{turns} idle turns is more than IDLE_TURNS_MAX ({IDLE_TURNS_MAX})"
                ));
            }
            Some((field(idle, "handler"), turns))
        }
    };
    let plan = Plan {
        texts: texts(&manifest),
        model: field(&manifest, "model"),
        raw: field(&manifest, "print") != "text",
        steps,
        idle,
    };
    let program = field(&manifest, "program");
    let module_dir = flags.module_dir.clone();
    let loaded =
        host::load_program(module_dir.as_deref(), &program).unwrap_or_else(|error| fail(&error));
    run_plan(&flags, &loaded, module_dir, spawns, &plan);
}

fn first_step(flags: &Flags, session: &Session, plan: &Plan) -> (usize, u64) {
    if let Some(turn) = flags.turn {
        return (0, turn);
    }
    let held = session
        .records()
        .map(|records| last_turn(&records))
        .unwrap_or_else(|error| fail(&error));
    let Some(held) = held else {
        return (0, FIRST_TURN);
    };
    let last = plan.steps.len() - 1;
    if same(&held, &plan.steps[last]) {
        return (last, held.turn);
    }
    let unfinished = plan.steps[..last]
        .iter()
        .position(|step| !held.finished && same(&held, step));
    match unfinished {
        Some(index) => (index, held.turn),
        None => (0, held.turn + 1),
    }
}

fn run_plan(
    flags: &Flags,
    loaded: &Loaded,
    module_dir: Option<PathBuf>,
    spawns: Vec<Spawn>,
    plan: &Plan,
) {
    assert!(!plan.steps.is_empty(), "a plan has a step");
    let (journal, mut session) = open(flags, module_dir, spawns, plan.texts.clone());
    let (first, mut turn) = first_step(flags, &session, plan);
    for step in plan
        .steps
        .iter()
        .filter(|step| step.handler == host::CONFIGURE_HANDLER)
    {
        session
            .remember_configure(&step.arguments)
            .unwrap_or_else(|error| fail(&error));
    }
    session.ledger.mark("start", now_ms());
    let last = plan.steps.len() - 1;
    for step in &plan.steps[first..last] {
        let request = host::request(&step.handler, turn, step.arguments.clone());
        match host::run_turn(loaded, &mut session, &request) {
            Ok(Ran::Finished(finish)) if finish.completed => {
                let (name, reason) = host::outcome_of(&finish.result);
                if name == "failed" {
                    session.close();
                    fail_usage(&reason);
                }
                turn += 1;
            }
            Err(error) if error.contains(ARGUMENTS_REFUSED) => {
                session.close();
                fail_usage(&format!("{} {}", step.refused, step.arguments.encode()));
            }
            Err(error) if error.contains(NO_HANDLER) => {
                eprintln!(
                    "{LABEL}: the program declares no {} handler; going on",
                    step.handler
                );
            }
            other => {
                session.close();
                let (_, _) = report(&other, plan.raw);
                fail(&format!("the {} turn did not complete", step.handler));
            }
        }
    }
    let step = &plan.steps[last];
    let request = host::request(&step.handler, turn, step.arguments.clone());
    let ran = host::run_turn(loaded, &mut session, &request);
    if let Some((handler, turns)) = &plan.idle
        && matches!(&ran, Ok(Ran::Finished(_)))
    {
        idle_turns(loaded, &mut session, handler, *turns);
    }
    session.close();
    session.ledger.mark("end", now_ms());
    let (finish, code) = report(&ran, plan.raw);
    if let Err(error) = ledger::write_usage(&journal, &plan.model, &finish) {
        fail(&error);
    }
    std::process::exit(code);
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

fn last_turn(records: &[Record]) -> Option<Last> {
    let mut last: Option<Last> = None;
    for record in records.iter().cloned() {
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
    last
}

fn same(held: &Last, step: &Step) -> bool {
    held.handler == step.handler
        && harnesslang::run::json::decode(&step.arguments.encode())
            .ok()
            .as_ref()
            == Some(&held.arguments)
}

fn idle_turns(loaded: &Loaded, session: &mut Session, handler: &str, turns: u64) {
    let mut ran = 0;
    for _ in 0..turns * IDLE_REQUESTS_PER_TURN_MAX {
        if ran == turns {
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
        let turn = match session
            .records()
            .and_then(|records| host::next_turn(&records))
        {
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
        let request = host::request(handler, turn, Json::Object(Vec::new()));
        match host::run_once(loaded, session, &request) {
            Ok(Ran::Finished(finish)) => {
                ran += 1;
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
