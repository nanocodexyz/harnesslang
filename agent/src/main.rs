mod anthropic;
mod children;
mod jobs;
mod json;
mod stream;
mod tools;

include!(concat!(env!("OUT_DIR"), "/harness_mod.rs"));
include!(concat!(env!("OUT_DIR"), "/child_mod.rs"));

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anthropic::{Client, Usage};
use harness::program::{self, Arguments};
use harness::{Exit, FileDisk, Finish, Json, Ran, RunError, TurnRequest};
use json::Value;

const EXIT_HARNESS_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 2;
const FIRST_TURN: u64 = 1;
const SYSTEM_PROMPT: &str = include_str!("../prompts/claude-code-system.json");
const TOOL_DEFINITIONS: &str = include_str!("../prompts/tools.json");
const SUBAGENT_TOOL_DEFINITIONS: &str = include_str!("../prompts/subagent-tools.json");
const JOINS_PER_TURN_MAX: u64 = program::ROUNDS_MAX * program::CALLS_PER_ROUND_MAX;
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
    children: Option<children::Children>,
    label: String,
    jobs: jobs::Jobs,
}

fn to_value(json: &Json) -> Value {
    Value::decode(&json.encode()).expect("the harness encodes valid JSON")
}

impl Agent {
    fn stream_file(&self, turn: u64, seq: u64) -> PathBuf {
        self.workspace
            .state
            .join(format!("stream-{turn}-{seq}.sse"))
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

    fn run_tool(
        &mut self,
        turn: u64,
        seq: u64,
        tool: &str,
        input: &Value,
        room: bool,
    ) -> tools::Ran {
        let receipted = matches!(tool, "Bash" | "Write") && self.children.is_some();
        if receipted && let Err(error) = self.workspace.dispatching(turn, seq) {
            return tools::Ran::Failed(format!(
                "<tool_use_error>receipt: {error}</tool_use_error>"
            ));
        }
        self.ledger.append(Value::object([
            ("kind", Value::text("tool")),
            ("at_ms", Value::number(now_ms())),
            ("tool", Value::text(tool)),
            ("call", Value::text(&harness::call_id(turn, seq))),
        ]));
        let shown = input
            .get("description")
            .or(input.get("file_path"))
            .or(input.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("");
        eprintln!(
            "{}: {} {tool}: {shown}",
            self.label,
            harness::call_id(turn, seq)
        );
        let ran = match &mut self.children {
            Some(children) if tool == children::SPAWN_TOOL => children.spawn(turn, seq, input),
            Some(children) if tool == children::LIST_TOOL => children.list(input),
            None if tool == children::SUBMIT_TOOL => children::submitted(input),
            _ if receipted => self.bash(turn, seq, input, room),
            _ => self.workspace.run(tool, input),
        };
        if receipted {
            self.workspace.dispatched(turn, seq, &ran);
        }
        ran
    }

    fn bash(&mut self, turn: u64, seq: u64, input: &Value, room: bool) -> tools::Ran {
        match self.workspace.bash_job(input, room) {
            tools::Bashed::Ran(ran) => ran,
            tools::Bashed::Job(started, message) => {
                let key = format!("{turn}.{seq}");
                let held = !input
                    .get("run_in_background")
                    .is_some_and(|value| matches!(value, Value::Bool(true)));
                let command = input.get("command").and_then(Value::as_str).unwrap_or("");
                self.jobs.start(&key, started, command, held);
                eprintln!(
                    "{}: {turn}/{seq} Bash became background job {key}",
                    self.label
                );
                tools::Ran::Job(jobs::started_text(&key, &message, held))
            }
        }
    }

    fn run_model(
        &mut self,
        turn: u64,
        seq: u64,
        mode: Option<&str>,
        effort: Option<&str>,
        context: &Value,
        attached: &anthropic::Attached<'_>,
    ) -> Value {
        let call = harness::call_id(turn, seq);
        let body = match self.client.body(context, mode, effort, attached) {
            Ok(body) => body,
            Err(error) => {
                return anthropic::transient(&format!("context does not render: {error}"), 0);
            }
        };
        let partial = self.stream_file(turn, seq);
        if let Ok(cut) = std::fs::metadata(&partial) {
            eprintln!(
                "{}: {call} a crash cut the previous stream of this call after {} bytes; requesting it again",
                self.label,
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
            return anthropic::transient(&format!("{}: {error}", partial.display()), 0);
        }
        let shown = mode.map(|mode| format!(" mode {mode}")).unwrap_or_default();
        eprintln!("{}: {call} model {}{shown}", self.label, self.client.model);
        let (reply, used) = match self.client.send(&body, &partial) {
            Ok((status, text, retry_after_ms)) if mode == Some(anthropic::MODE_REFRESH) => {
                anthropic::interpret(status, &text, retry_after_ms)
            }
            Ok((status, text, retry_after_ms)) => {
                anthropic::interpret_stream(status, &text, retry_after_ms)
            }
            Err(error) => (anthropic::transient(&error, 0), Usage::default()),
        };
        if let Err(error) = std::fs::remove_file(&partial) {
            eprintln!("{}: {}: {error}", self.label, partial.display());
        }
        let outcome = match &reply {
            Value::Object(entries) if entries.len() == 1 => entries[0].0.clone(),
            _ => "reply".to_owned(),
        };
        if outcome != "ok" {
            eprintln!("{}: model call {outcome}: {}", self.label, reply.encode());
        }
        self.record_model(used, &outcome);
        reply
    }
}

fn random() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970")
        .subsec_nanos();
    u64::from(nanos) ^ now_ms().rotate_left(u32::BITS)
}

macro_rules! output_of {
    ($module:ident, $ran:expr) => {
        match $ran {
            tools::Ran::Ok(text) => $module::Output::Ok($module::Json::String(text)),
            tools::Ran::Image(bytes) => $module::Output::Ok($module::host_blob_bytes(&bytes)),
            tools::Ran::Failed(text) => $module::Output::Failed(text),
            tools::Ran::Job(text) => $module::Output::Ok($module::Json::Object(
                std::collections::BTreeMap::from([("job".to_owned(), $module::Json::String(text))]),
            )),
        }
    };
}

macro_rules! host_for {
    ($module:ident) => {
        impl $module::Host for Agent {
            fn tool(&mut self, request: &$module::ToolRequest<'_>) -> $module::Output {
                let input =
                    Value::decode(&request.input.encode()).expect("the harness encodes valid JSON");
                let ran =
                    self.run_tool(request.turn, request.seq, request.tool, &input, request.job);
                output_of!($module, ran)
            }

            fn recover(&mut self, request: &$module::ToolRequest<'_>) -> $module::Recovered {
                let input =
                    Value::decode(&request.input.encode()).expect("the harness encodes valid JSON");
                let recovered =
                    self.workspace
                        .recover(request.turn, request.seq, request.tool, &input);
                eprintln!(
                    "{}: {} recover {}: {}",
                    self.label,
                    harness::call_id(request.turn, request.seq),
                    request.tool,
                    match &recovered {
                        tools::Recovered::Reply(_) => "reply",
                        tools::Recovered::NotDispatched => "not_dispatched",
                        tools::Recovered::Unknown => "unknown",
                    }
                );
                self.ledger.append(Value::object([
                    ("kind", Value::text("recover")),
                    ("at_ms", Value::number(now_ms())),
                    ("tool", Value::text(request.tool)),
                    (
                        "call",
                        Value::text(&harness::call_id(request.turn, request.seq)),
                    ),
                    (
                        "answer",
                        Value::text(match &recovered {
                            tools::Recovered::Reply(_) => "reply",
                            tools::Recovered::NotDispatched => "not_dispatched",
                            tools::Recovered::Unknown => "unknown",
                        }),
                    ),
                ]));
                match recovered {
                    tools::Recovered::Reply(ran) => {
                        $module::Recovered::Reply(output_of!($module, ran))
                    }
                    tools::Recovered::NotDispatched => $module::Recovered::NotDispatched,
                    tools::Recovered::Unknown => $module::Recovered::Unknown,
                }
            }

            fn jobs(&mut self, _turn: u64, open: &[String]) -> Vec<String> {
                self.jobs.poll();
                open.iter()
                    .filter(|key| !self.jobs.knows(key))
                    .cloned()
                    .collect()
            }

            fn hold(&mut self, _turn: u64, _seq: u64, until_ms: u64) -> Result<u64, $module::Stop> {
                loop {
                    self.jobs.poll();
                    let now = now_ms();
                    if self.jobs.pending() || !self.jobs.holding() || now >= until_ms {
                        return Ok(now);
                    }
                    std::thread::sleep(jobs::JOB_POLL);
                }
            }

            fn inbox(&mut self, _turn: u64) -> Vec<$module::Delivery> {
                self.jobs
                    .notices()
                    .into_iter()
                    .map(|(key, notice)| $module::Delivery {
                        id: format!("{}{key}", jobs::DONE_PREFIX),
                        inbound: $module::Inbound::Job($module::JobNotice {
                            key,
                            notice,
                            done: true,
                        }),
                    })
                    .collect()
            }

            fn answer(
                &mut self,
                _turn: u64,
                _position: u64,
                answers: &[(String, $module::EventAnswer)],
            ) {
                for (id, answer) in answers {
                    let delivered = matches!(
                        answer,
                        $module::EventAnswer::Recorded | $module::EventAnswer::Duplicate
                    );
                    self.jobs.answered(id, delivered);
                }
            }

            fn listing(&mut self, _request: &$module::ListingRequest<'_>) -> $module::Output {
                $module::Output::Ok($module::Json::Object(std::collections::BTreeMap::new()))
            }

            fn model(&mut self, request: &$module::ModelRequest<'_>) -> $module::Json {
                let context = Value::decode(&(request.context)().encode())
                    .expect("the harness encodes valid JSON");
                let attached = |digest: &str| {
                    request.blobs.get(digest).map(|bytes| {
                        (
                            tools::media_type(bytes).unwrap_or_default().to_owned(),
                            $module::base64_encode(bytes),
                        )
                    })
                };
                let effort = request
                    .settings
                    .and_then(|settings| settings.get(anthropic::EFFORT_SETTING))
                    .and_then(|effort| effort.as_str());
                let reply = self.run_model(
                    request.turn,
                    request.seq,
                    request.mode,
                    effort,
                    &context,
                    &attached,
                );
                $module::Json::decode(&reply.encode())
                    .expect("a reply the host built decodes as harness JSON")
            }

            fn now(&mut self) -> u64 {
                now_ms()
            }

            fn random(&mut self) -> u64 {
                random()
            }

            fn acknowledge(&mut self, _finish: &$module::Finish) {}

            fn resume(&mut self, suspension: &$module::Suspension) -> Option<$module::Resume> {
                let answer = self.children.as_mut()?.join(&suspension.key)?;
                Some($module::Resume {
                    key: suspension.key.clone(),
                    answer,
                })
            }
        }
    };
}

host_for!(harness);
host_for!(child_harness);

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
    children: u64,
}

impl Totals {
    fn add_child(&mut self, child: &Totals) {
        self.usage.input += child.usage.input;
        self.usage.output += child.usage.output;
        self.usage.cache_read += child.usage.cache_read;
        self.usage.cache_write += child.usage.cache_write;
        self.requests += child.requests;
        self.tool_calls += child.tool_calls;
        self.children += 1;
    }
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
    let mut totals = totals(&journal.join("usage.jsonl"))?;
    for ledger in children::ledgers(journal)? {
        totals.add_child(&self::totals(&ledger)?);
    }
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
        ("children", Value::number(totals.children)),
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
    let (tools, child_tools) = tool_definitions();
    let system = system_prompt(&cwd, &state)?;
    let children = children::Children::new(
        &journal,
        children::Setup {
            base_url: base_url.clone(),
            key: key.clone(),
            model: options.model.clone(),
            max_tokens,
            system: system.clone(),
            tools: child_tools,
            cwd: cwd.clone(),
        },
    );
    let client = Client {
        base_url,
        key,
        model: options.model.clone(),
        max_tokens,
        system,
        tools,
        scratch: state.clone(),
    };
    let workspace = tools::Workspace {
        root: cwd,
        state,
        image_bytes_max: program::blob_bound("Read").map(|(_, bytes)| bytes),
        left: std::sync::Mutex::new(Vec::new()),
    };
    Ok((
        Agent {
            client,
            workspace,
            ledger: Ledger { file },
            children: Some(children),
            label: "hl-agent".to_owned(),
            jobs: jobs::Jobs::default(),
        },
        instruction,
    ))
}

fn tool_definitions() -> (Value, Value) {
    let Ok(Value::Array(mut parent)) = Value::decode(TOOL_DEFINITIONS) else {
        panic!("the bundled tool definitions are a JSON list");
    };
    let mut child = parent.clone();
    let subagent = Value::decode(SUBAGENT_TOOL_DEFINITIONS)
        .expect("the bundled subagent tool definitions are JSON");
    let list = |name: &str| {
        subagent
            .get(name)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("the subagent tool definitions hold a {name} list"))
            .to_vec()
    };
    parent.extend(list("parent"));
    child.extend(list("child"));
    (Value::Array(parent), Value::Array(child))
}

fn run_turn(
    agent: &mut Agent,
    disk: &mut FileDisk,
    request: &TurnRequest,
) -> Result<Ran, RunError> {
    let mut ran = harness::run_jsonl(agent, disk, request);
    let mut waited = None;
    for _ in 0..JOINS_PER_TURN_MAX {
        let Ok(Ran::Suspended(suspension)) = &ran else {
            return ran;
        };
        if waited == Some(suspension.seq) {
            return ran;
        }
        waited = Some(suspension.seq);
        ran = harness::run_jsonl(agent, disk, request);
    }
    ran
}

fn main() {
    harness::self_check();
    harness::jsonl_self_check();
    child_harness::self_check();
    child_harness::jsonl_self_check();
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
    let ran = run_turn(&mut agent, &mut disk, &request);
    agent.jobs.stop_all();
    agent.workspace.stop_left();
    if options.keep_warm > 0 && matches!(&ran, Ok(Ran::Finished(_) | Ran::AlreadyFinished(_))) {
        keep_warm(&mut agent, &mut disk, options.keep_warm);
    }
    if let Some(children) = agent.children.as_mut() {
        children.finish();
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
                "hl-agent: turn {} waits on suspend {} ({}), which this host could not resume",
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
