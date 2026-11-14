mod anthropic;
mod json;
mod stream;
mod tools;

include!(concat!(env!("OUT_DIR"), "/harness_mod.rs"));

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anthropic::{Client, Usage};
use harness::program::{self, Arguments};
use harness::{
    Exit, FileDisk, Finish, Host, Json, ListingRequest, ModelRequest, Output, Ran, RunError,
    ToolRequest, TurnRequest,
};
use json::Value;

const EXIT_HARNESS_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 2;
const FIRST_TURN: u64 = 1;
const SYSTEM_PROMPT: &str = include_str!("../prompts/claude-code-system.json");
const TOOL_DEFINITIONS: &str = include_str!("../prompts/tools.json");
const USAGE: &str = "usage: hl-agent run --instruction <file> --cwd <dir> --model <id> --journal <dir> [--max-turns N] [--keep-warm N]";
const IDLE_REQUESTS_PER_REFRESH_MAX: u64 = 2;

struct Options {
    instruction: PathBuf,
    cwd: PathBuf,
    model: String,
    journal: PathBuf,
    rounds: u64,
    keep_warm: u64,
}

fn fail_usage(message: &str) -> ! {
    eprintln!("hl-agent: {message}\n{USAGE}");
    std::process::exit(EXIT_USAGE)
}

fn options() -> Options {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("run") {
        fail_usage("the only command is run");
    }
    let (mut instruction, mut cwd, mut model, mut journal, mut rounds) =
        (None, None, None, None, None);
    let mut keep_warm = 0;
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .unwrap_or_else(|| fail_usage(&format!("{flag} needs a value")))
            .clone();
        match flag.as_str() {
            "--instruction" => instruction = Some(PathBuf::from(value)),
            "--cwd" => cwd = Some(PathBuf::from(value)),
            "--model" => model = Some(value),
            "--journal" => journal = Some(PathBuf::from(value)),
            "--max-turns" => {
                let parsed: u64 = value.parse().unwrap_or_else(|_| {
                    fail_usage(&format!("--max-turns {value} is not a number"))
                });
                if parsed == 0 || parsed > program::ROUNDS_MAX {
                    fail_usage(&format!(
                        "--max-turns must be between 1 and the harness's rounds_max ({})",
                        program::ROUNDS_MAX
                    ));
                }
                rounds = Some(parsed);
            }
            "--keep-warm" => {
                keep_warm = value.parse().unwrap_or_else(|_| {
                    fail_usage(&format!("--keep-warm {value} is not a number"))
                });
                if keep_warm > program::CACHE_REFRESHES {
                    fail_usage(&format!(
                        "--keep-warm must be at most the harness's cache_refreshes ({})",
                        program::CACHE_REFRESHES
                    ));
                }
            }
            other => fail_usage(&format!("unknown flag {other}")),
        }
    }
    Options {
        instruction: instruction.unwrap_or_else(|| fail_usage("--instruction is required")),
        cwd: cwd.unwrap_or_else(|| fail_usage("--cwd is required")),
        model: model.unwrap_or_else(|| fail_usage("--model is required")),
        journal: journal.unwrap_or_else(|| fail_usage("--journal is required")),
        rounds: rounds.unwrap_or(program::ROUNDS_MAX),
        keep_warm,
    }
}

fn keep_warm(agent: &mut Agent, disk: &mut FileDisk, refreshes: u64) {
    let mut refreshed = 0;
    for _ in 0..refreshes * IDLE_REQUESTS_PER_REFRESH_MAX {
        if refreshed == refreshes {
            return;
        }
        let journal = match harness::JsonlJournal::open(disk) {
            Ok(journal) => journal,
            Err(error) => {
                eprintln!("hl-agent: journal: {error}");
                return;
            }
        };
        let (due, turn) = match (journal.idle_due(), journal.next_turn()) {
            (Ok(Some(due)), Ok(turn)) => (due, turn),
            (Ok(None), _) => {
                eprintln!("hl-agent: no prompt cache refresh is armed");
                return;
            }
            (Err(error), _) | (_, Err(error)) => {
                eprintln!("hl-agent: journal: {error:?}");
                return;
            }
        };
        let now = now_ms();
        if due > now {
            eprintln!(
                "hl-agent: idle {} ms until the prompt cache refresh due at {due}",
                due - now
            );
            std::thread::sleep(std::time::Duration::from_millis(due - now));
        }
        let request = TurnRequest {
            turn,
            arguments: Arguments::Refresh,
        };
        match journal.run(agent, &request) {
            Ok((_, Ran::Finished(finish))) => {
                refreshed += 1;
                eprintln!("hl-agent: idle turn {turn}: {}", finish.result.encode());
            }
            Ok((_, Ran::NotDue(not_due))) => {
                eprintln!("hl-agent: idle turn {turn} is not due: {not_due:?}")
            }
            Ok((_, other)) => {
                eprintln!("hl-agent: idle turn {turn} ended {other:?}");
                return;
            }
            Err(error) => {
                eprintln!("hl-agent: idle turn {turn}: {error:?}");
                return;
            }
        }
    }
}

fn now_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970");
    u64::try_from(elapsed.as_millis()).expect("milliseconds since 1970 fit in u64")
}

struct Ledger {
    file: std::fs::File,
}

impl Ledger {
    fn append(&mut self, line: Value) {
        let mut text = line.encode();
        text.push('\n');
        self.file
            .write_all(text.as_bytes())
            .expect("append to the usage ledger");
        self.file.sync_data().expect("sync the usage ledger");
    }
}

struct Agent {
    client: Client,
    workspace: tools::Workspace,
    ledger: Ledger,
}

fn to_value(json: &Json) -> Value {
    Value::decode(&json.encode()).expect("the harness encodes valid JSON")
}

fn to_json(value: &Value) -> Json {
    Json::decode(&value.encode()).expect("a reply the host built decodes as harness JSON")
}

impl Agent {
    fn stream_file(&self, request: &ModelRequest<'_>) -> PathBuf {
        self.workspace
            .state
            .join(format!("stream-{}-{}.sse", request.turn, request.seq))
    }

    fn record_model(&mut self, used: Usage, outcome: &str) {
        self.ledger.append(Value::object([
            ("kind", Value::text("model")),
            ("at_ms", Value::number(now_ms())),
            ("outcome", Value::text(outcome)),
            ("input_tokens", Value::number(used.input)),
            ("output_tokens", Value::number(used.output)),
            ("cache_read_tokens", Value::number(used.cache_read)),
            ("cache_write_tokens", Value::number(used.cache_write)),
        ]));
    }
}

impl Host for Agent {
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        let input = to_value(request.input);
        self.ledger.append(Value::object([
            ("kind", Value::text("tool")),
            ("at_ms", Value::number(now_ms())),
            ("tool", Value::text(request.tool)),
            (
                "call",
                Value::text(&harness::call_id(request.turn, request.seq)),
            ),
        ]));
        let shown = input
            .get("description")
            .or(input.get("file_path"))
            .or(input.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("");
        eprintln!(
            "hl-agent: {} {}: {shown}",
            harness::call_id(request.turn, request.seq),
            request.tool
        );
        match self.workspace.run(request.tool, &input) {
            tools::Ran::Ok(text) => Output::Ok(Json::String(text)),
            tools::Ran::Image(bytes) => Output::Ok(harness::host_blob_bytes(&bytes)),
            tools::Ran::Failed(text) => Output::Failed(text),
        }
    }

    fn listing(&mut self, _request: &ListingRequest<'_>) -> Output {
        Output::Ok(Json::Object(std::collections::BTreeMap::new()))
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        let call = harness::call_id(request.turn, request.seq);
        let context = to_value(&(request.context)());
        let attached = |digest: &str| {
            request.blobs.get(digest).map(|bytes| {
                (
                    tools::media_type(bytes).unwrap_or_default().to_owned(),
                    harness::base64_encode(bytes),
                )
            })
        };
        let body = match self.client.body(&context, request.mode, &attached) {
            Ok(body) => body,
            Err(error) => {
                return to_json(&anthropic::transient(
                    &format!("context does not render: {error}"),
                    0,
                ));
            }
        };
        let partial = self.stream_file(request);
        if let Ok(cut) = std::fs::metadata(&partial) {
            eprintln!(
                "hl-agent: {call} a crash cut the previous stream of this call after {} bytes; requesting it again",
                cut.len()
            );
            self.ledger.append(Value::object([
                ("kind", Value::text("stream_discarded")),
                ("at_ms", Value::number(now_ms())),
                ("call", Value::text(&call)),
                ("bytes", Value::number(cut.len())),
            ]));
        }
        if let Err(error) = std::fs::write(&partial, "") {
            return to_json(&anthropic::transient(
                &format!("{}: {error}", partial.display()),
                0,
            ));
        }
        let mode = request
            .mode
            .map(|mode| format!(" mode {mode}"))
            .unwrap_or_default();
        eprintln!("hl-agent: {call} model {}{mode}", self.client.model);
        let (reply, used) = match self.client.send(&body, &partial) {
            Ok((status, text, retry_after_ms)) if request.mode == Some(anthropic::MODE_REFRESH) => {
                anthropic::interpret(status, &text, retry_after_ms)
            }
            Ok((status, text, retry_after_ms)) => {
                anthropic::interpret_stream(status, &text, retry_after_ms)
            }
            Err(error) => (anthropic::transient(&error, 0), Usage::default()),
        };
        if let Err(error) = std::fs::remove_file(&partial) {
            eprintln!("hl-agent: {}: {error}", partial.display());
        }
        let outcome = match &reply {
            Value::Object(entries) if entries.len() == 1 => entries[0].0.clone(),
            _ => "reply".to_owned(),
        };
        if outcome != "ok" {
            eprintln!("hl-agent: model call {outcome}: {}", reply.encode());
        }
        self.record_model(used, &outcome);
        to_json(&reply)
    }

    fn now(&mut self) -> u64 {
        now_ms()
    }

    fn random(&mut self) -> u64 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after 1970")
            .subsec_nanos();
        u64::from(nanos) ^ now_ms().rotate_left(u32::BITS)
    }

    fn acknowledge(&mut self, _finish: &Finish) {}
}

fn command_line(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn environment_section(cwd: &Path) -> String {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let mut lines = vec![
        "# Environment".to_owned(),
        "You have been invoked in the following environment: ".to_owned(),
        format!("Primary working directory: {}", cwd.display()),
        format!(
            "Is a git repository: {}",
            cwd.ancestors()
                .any(|directory| directory.join(".git").exists())
        ),
        format!("Platform: {platform}"),
    ];
    if let Some(shell) = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .and_then(|shell| {
            shell
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
    {
        lines.push(format!("Shell: {shell}"));
    }
    if let Some(version) = command_line("uname", &["-sr"]) {
        lines.push(format!("OS Version: {version}"));
    }
    lines.push(format!(
        "Today's date: {}",
        command_line("date", &["+%Y-%m-%d"]).unwrap_or_default()
    ));
    lines.join("\n")
}

fn system_prompt(cwd: &Path, state: &Path) -> Result<Value, String> {
    let saved = state.join("system.json");
    if let Ok(text) = std::fs::read_to_string(&saved) {
        return Value::decode(&text).map_err(|error| format!("{}: {error}", saved.display()));
    }
    let mut blocks = match Value::decode(SYSTEM_PROMPT) {
        Ok(Value::Array(blocks)) => blocks,
        other => panic!("the bundled system prompt is a list of blocks, found {other:?}"),
    };
    blocks.push(Value::object([
        ("type", Value::text("text")),
        ("text", Value::String(environment_section(cwd))),
    ]));
    if let Ok(text) = std::fs::read_to_string(cwd.join("AGENTS.md")) {
        let instructions = format!(
            "# AGENTS.md instructions for {}\n\n<INSTRUCTIONS>\n{}\n</INSTRUCTIONS>",
            cwd.display(),
            text.trim_end()
        );
        blocks.push(Value::object([
            ("type", Value::text("text")),
            ("text", Value::String(instructions)),
        ]));
    }
    let system = Value::Array(blocks);
    let temporary = state.join("system.json.new");
    std::fs::write(&temporary, system.encode())
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, &saved).map_err(|error| format!("{}: {error}", saved.display()))?;
    Ok(system)
}

#[derive(Default)]
struct Totals {
    usage: Usage,
    requests: u64,
    tool_calls: u64,
    wall_ms: u64,
}

fn totals(ledger: &Path) -> Result<Totals, String> {
    let text = std::fs::read_to_string(ledger)
        .map_err(|error| format!("{}: {error}", ledger.display()))?;
    let mut totals = Totals::default();
    let mut run: Option<(u64, u64)> = None;
    for line in text.lines().filter(|line| !line.is_empty()) {
        let Ok(value) = Value::decode(line) else {
            continue;
        };
        let at = value.get("at_ms").and_then(Value::as_u64).unwrap_or(0);
        let field = |name: &str| value.get(name).and_then(Value::as_u64).unwrap_or(0);
        match value.get("kind").and_then(Value::as_str) {
            Some("start") => {
                if let Some((start, last)) = run {
                    totals.wall_ms += last - start;
                }
                run = Some((at, at));
                continue;
            }
            Some("model") => {
                totals.requests += 1;
                totals.usage.input += field("input_tokens");
                totals.usage.output += field("output_tokens");
                totals.usage.cache_read += field("cache_read_tokens");
                totals.usage.cache_write += field("cache_write_tokens");
            }
            Some("tool") => totals.tool_calls += 1,
            _ => {}
        }
        if let Some((start, last)) = run.as_mut() {
            *last = at.max(*last).max(*start);
        }
    }
    if let Some((start, last)) = run {
        totals.wall_ms += last - start;
    }
    Ok(totals)
}

fn write_usage(journal: &Path, model: &str, finish: &str) -> Result<(), String> {
    let totals = totals(&journal.join("usage.jsonl"))?;
    let usage = Value::object([
        ("model", Value::text(model)),
        ("input_tokens", Value::number(totals.usage.input)),
        ("output_tokens", Value::number(totals.usage.output)),
        ("cache_read_tokens", Value::number(totals.usage.cache_read)),
        (
            "cache_write_tokens",
            Value::number(totals.usage.cache_write),
        ),
        ("requests", Value::number(totals.requests)),
        ("tool_calls", Value::number(totals.tool_calls)),
        ("wall_ms", Value::number(totals.wall_ms)),
        ("finish", Value::text(finish)),
    ]);
    let temporary = journal.join("usage.json.new");
    std::fs::write(&temporary, usage.encode() + "\n")
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, journal.join("usage.json"))
        .map_err(|error| format!("usage.json: {error}"))
}

fn outcome(finish: &Finish) -> (String, String) {
    let result = to_value(&finish.result);
    match &result {
        Value::String(name) => (name.clone(), String::new()),
        Value::Object(entries) if entries.len() == 1 => {
            let (name, payload) = &entries[0];
            (
                name.clone(),
                payload
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| payload.encode()),
            )
        }
        other => ("unknown".to_owned(), other.encode()),
    }
}

fn setup(options: &Options) -> Result<(Agent, String), String> {
    let instruction = std::fs::read_to_string(&options.instruction)
        .map_err(|error| format!("{}: {error}", options.instruction.display()))?;
    let cwd = std::fs::canonicalize(&options.cwd)
        .map_err(|error| format!("{}: {error}", options.cwd.display()))?;
    let key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY is not set".to_owned())?;
    let base_url = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| anthropic::BASE_URL_DEFAULT.to_owned());
    let max_tokens = anthropic::model_max_tokens(&options.model).ok_or_else(|| {
        format!(
            "unknown Claude model {}: nanocode's table has no max_tokens for it",
            options.model
        )
    })?;
    std::fs::create_dir_all(&options.journal)
        .map_err(|error| format!("{}: {error}", options.journal.display()))?;
    let journal = std::fs::canonicalize(&options.journal)
        .map_err(|error| format!("{}: {error}", options.journal.display()))?;
    let state = journal.join("state");
    std::fs::create_dir_all(&state).map_err(|error| format!("{}: {error}", state.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal.join("usage.jsonl"))
        .map_err(|error| format!("usage.jsonl: {error}"))?;
    let tools = Value::decode(TOOL_DEFINITIONS).expect("the bundled tool definitions are JSON");
    let client = Client {
        base_url,
        key,
        model: options.model.clone(),
        max_tokens,
        system: system_prompt(&cwd, &state)?,
        tools,
        scratch: state.clone(),
    };
    let workspace = tools::Workspace {
        root: cwd,
        state,
        image_bytes_max: program::blob_bound("Read").map(|(_, bytes)| bytes),
    };
    Ok((
        Agent {
            client,
            workspace,
            ledger: Ledger { file },
        },
        instruction,
    ))
}

fn main() {
    harness::self_check();
    harness::jsonl_self_check();
    let options = options();
    let (mut agent, instruction) = setup(&options).unwrap_or_else(|error| {
        eprintln!("hl-agent: {error}");
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    agent.ledger.append(Value::object([
        ("kind", Value::text("start")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let journal_path = options.journal.join("journal.jsonl");
    let mut disk = FileDisk::open(&journal_path).unwrap_or_else(|error| {
        eprintln!("hl-agent: {}: {error}", journal_path.display());
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    let request = TurnRequest {
        turn: FIRST_TURN,
        arguments: Arguments::Prompt(instruction, options.rounds),
    };
    let ran = harness::run_jsonl(&mut agent, &mut disk, &request);
    if options.keep_warm > 0 && matches!(&ran, Ok(Ran::Finished(_) | Ran::AlreadyFinished(_))) {
        keep_warm(&mut agent, &mut disk, options.keep_warm);
    }
    agent.ledger.append(Value::object([
        ("kind", Value::text("end")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let (finish, code) = match &ran {
        Ok(Ran::Finished(finish) | Ran::AlreadyFinished(finish))
            if finish.outcome == harness::Outcome::Completed =>
        {
            let (name, text) = outcome(finish);
            println!("{text}");
            (name, 0)
        }
        Ok(Ran::Finished(finish) | Ran::AlreadyFinished(finish)) => {
            eprintln!("hl-agent: the harness trapped: {}", finish.result.encode());
            ("trap".to_owned(), EXIT_HARNESS_ERROR)
        }
        Ok(Ran::Suspended(suspension)) => {
            eprintln!(
                "hl-agent: turn {} waits on suspend {} ({}), which this host never resumes",
                suspension.turn, suspension.seq, suspension.key
            );
            ("suspended".to_owned(), EXIT_HARNESS_ERROR)
        }
        Ok(Ran::NotDue(not_due)) => {
            eprintln!("hl-agent: the prompt turn was refused as an idle turn: {not_due:?}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
        Err(RunError::Open(error)) => {
            eprintln!("hl-agent: journal: {error}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
        Err(RunError::Exit(Exit::Corrupt(error))) => {
            eprintln!("hl-agent: journal: {error}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
        Err(RunError::Exit(Exit::Stop(stop))) => {
            eprintln!("hl-agent: stopped: {stop:?}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
    };
    if let Err(error) = write_usage(&options.journal, &options.model, &finish) {
        eprintln!("hl-agent: {error}");
        std::process::exit(EXIT_HARNESS_ERROR);
    }
    std::process::exit(code);
}
