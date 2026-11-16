mod anthropic;
mod children;
mod codex;
mod codex_host;
mod jobs;
mod json;
mod mpp;
mod openai;
mod reviewer;
mod stream;
mod tools;
mod transport;

mod host;

include!(concat!(env!("OUT_DIR"), "/constants.rs"));
include!(concat!(env!("OUT_DIR"), "/mpp_mod.rs"));
include!(concat!(env!("OUT_DIR"), "/codex_mod.rs"));

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anthropic::{Client, Usage};
use harnesslang::run::journal::Record;
use harnesslang::wasm::native::Journal;
use harnesslang::wasm::sim::Loaded;
use host::{Finish, Ran};
use json::Value;

pub const EXIT_HARNESS_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
const FIRST_TURN: u64 = 1;
const SYSTEM_PROMPT: &str = include_str!("../prompts/claude-code-system.json");
const TOOL_DEFINITIONS: &str = include_str!("../prompts/tools.json");
const SUBAGENT_TOOL_DEFINITIONS: &str = include_str!("../prompts/subagent-tools.json");
const JOINS_PER_TURN_MAX: u64 = harness::ROUNDS_MAX * harness::CALLS_PER_ROUND_MAX;
const USAGE: &str = "usage: hl-agent run --instruction <file> --cwd <dir> --model <id> --journal <dir> [--harness claude|codex] [--max-turns N] [--effort high|medium|low|xhigh] [--reviewer-relay <dir>] [--finish-checks N] [--keep-warm N] [--turn N] [--fork-from <journal> [--at T]]\n       hl-agent pay --instruction <file> --cwd <dir> --model <id> --journal <dir> [--turn N]\n       hl-agent steer --journal <dir> --text <message>\n       hl-agent cancel --journal <dir>";
const PLAIN_MODEL_ATTEMPTS: u64 = 3;
const PLAIN_MODEL_RETRY: std::time::Duration = std::time::Duration::from_secs(2);
const MPP_SYSTEM_PROMPT: &str = include_str!("../prompts/mpp-system.json");
const MPP_TOOL_DEFINITIONS: &str = include_str!("../prompts/mpp-tools.json");
const HARNESS_CLAUDE: &str = "claude";
const HARNESS_CODEX: &str = "codex";
const CLAUDE_MODEL_PREFIX: &str = "claude";
const CLAUDE_EFFORT_LEVELS: [&str; 3] = ["high", "medium", "low"];
const SET_EFFORT_HANDLER: &str = "set_effort";
const IDLE_REQUESTS_PER_REFRESH_MAX: u64 = 2;
const PROMPT_HANDLER: &str = "prompt";
const IDLE_HANDLER: &str = "refresh";
const JOURNAL_CHECKSUMS: bool = true;

pub struct Options {
    pub instruction: PathBuf,
    pub cwd: PathBuf,
    pub model: String,
    pub journal: PathBuf,
    pub rounds: u64,
    pub keep_warm: u64,
    pub turn: u64,
    pub fork_from: Option<PathBuf>,
    pub at: Option<u64>,
    pub effort: Option<String>,
    pub effort_text: Option<String>,
    pub reviewer_url: Option<String>,
    pub finish_checks: Option<u64>,
    pub harness: String,
}

fn fail_usage(message: &str) -> ! {
    eprintln!("hl-agent: {message}\n{USAGE}");
    std::process::exit(EXIT_USAGE)
}

fn options() -> Options {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run" | "pay") => {}
        Some(command @ ("steer" | "cancel")) => send_event(command, &args[1..]),
        _ => fail_usage("the commands are run, pay, steer and cancel"),
    }
    let (mut instruction, mut cwd, mut model, mut journal, mut rounds) =
        (None, None, None, None, None);
    let mut keep_warm = 0;
    let mut turn = FIRST_TURN;
    let (mut fork_from, mut at) = (None, None);
    let mut effort = None;
    let mut effort_text = None;
    let mut harness = None;
    let mut reviewer_url = None;
    let mut finish_checks = None;
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
                if parsed == 0 || parsed > harness::ROUNDS_MAX {
                    fail_usage(&format!(
                        "--max-turns must be between 1 and the harness's rounds_max ({})",
                        harness::ROUNDS_MAX
                    ));
                }
                rounds = Some(parsed);
            }
            "--keep-warm" => {
                keep_warm = value.parse().unwrap_or_else(|_| {
                    fail_usage(&format!("--keep-warm {value} is not a number"))
                });
                if keep_warm > harness::CACHE_REFRESHES {
                    fail_usage(&format!(
                        "--keep-warm must be at most the harness's cache_refreshes ({})",
                        harness::CACHE_REFRESHES
                    ));
                }
            }
            "--turn" => {
                turn = value
                    .parse()
                    .ok()
                    .filter(|turn| *turn >= FIRST_TURN)
                    .unwrap_or_else(|| fail_usage(&format!("--turn {value} is not a turn number")));
            }
            "--effort" => {
                effort = CLAUDE_EFFORT_LEVELS
                    .contains(&value.as_str())
                    .then(|| value.clone());
                effort_text = Some(value);
            }
            "--harness" => harness = Some(value),
            "--reviewer-relay" => reviewer_url = Some(value),
            "--finish-checks" => {
                finish_checks = Some(value.parse().unwrap_or_else(|_| {
                    fail_usage(&format!("--finish-checks {value} is not a number"))
                }));
            }
            "--fork-from" => fork_from = Some(PathBuf::from(value)),
            "--at" => {
                at =
                    Some(value.parse().unwrap_or_else(|_| {
                        fail_usage(&format!("--at {value} is not a turn number"))
                    }));
            }
            other => fail_usage(&format!("unknown flag {other}")),
        }
    }
    if at.is_some() && fork_from.is_none() {
        fail_usage("--at needs --fork-from");
    }
    let model = model.unwrap_or_else(|| fail_usage("--model is required"));
    let harness = harness.unwrap_or_else(|| {
        if model.starts_with(CLAUDE_MODEL_PREFIX) {
            HARNESS_CLAUDE.to_owned()
        } else {
            HARNESS_CODEX.to_owned()
        }
    });
    if harness != HARNESS_CLAUDE && harness != HARNESS_CODEX {
        fail_usage(&format!("--harness {harness} is not claude or codex"));
    }
    if harness == HARNESS_CLAUDE && effort_text.is_some() && effort.is_none() {
        fail_usage("--effort for the Claude harness is high, medium or low");
    }
    if harness == HARNESS_CODEX && (keep_warm > 0 || fork_from.is_some()) {
        fail_usage("--keep-warm and --fork-from are for the Claude harness");
    }
    if harness == HARNESS_CLAUDE && reviewer_url.is_some() {
        fail_usage("--reviewer-relay is for the Codex harness");
    }
    if harness == HARNESS_CLAUDE && finish_checks.is_some() {
        fail_usage("--finish-checks is for the Codex harness");
    }
    Options {
        instruction: instruction.unwrap_or_else(|| fail_usage("--instruction is required")),
        cwd: cwd.unwrap_or_else(|| fail_usage("--cwd is required")),
        model,
        journal: journal.unwrap_or_else(|| fail_usage("--journal is required")),
        rounds: rounds.unwrap_or(harness::ROUNDS_MAX),
        keep_warm,
        turn,
        fork_from,
        at,
        effort,
        effort_text,
        reviewer_url,
        finish_checks,
        harness,
    }
}

fn send_event(command: &str, args: &[String]) -> ! {
    let (mut journal, mut text) = (None, None);
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .unwrap_or_else(|| fail_usage(&format!("{flag} needs a value")))
            .clone();
        match flag.as_str() {
            "--journal" => journal = Some(PathBuf::from(value)),
            "--text" if command == "steer" => text = Some(value),
            other => fail_usage(&format!("unknown flag {other}")),
        }
    }
    let journal = journal.unwrap_or_else(|| fail_usage("--journal is required"));
    if command == "steer" && text.is_none() {
        fail_usage("steer needs --text");
    }
    match codex_host::send_event(&journal, text.as_deref()) {
        Ok(name) => {
            println!("{name}");
            std::process::exit(0)
        }
        Err(error) => {
            eprintln!("hl-agent: {error}");
            std::process::exit(EXIT_HARNESS_ERROR)
        }
    }
}

fn keep_warm(loaded: &Loaded, agent: &mut Agent, journal: &mut Journal, refreshes: u64) {
    let mut refreshed = 0;
    for _ in 0..refreshes * IDLE_REQUESTS_PER_REFRESH_MAX {
        if refreshed == refreshes {
            return;
        }
        let due = match host::idle_due(loaded, agent, journal) {
            Ok(Some(due)) => due,
            Ok(None) => {
                eprintln!("hl-agent: no prompt cache refresh is armed");
                return;
            }
            Err(error) => {
                eprintln!("hl-agent: journal: {error}");
                return;
            }
        };
        let turn = match host::next_turn(journal) {
            Ok(turn) => turn,
            Err(error) => {
                eprintln!("hl-agent: journal: {error}");
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
        let request = host::request(IDLE_HANDLER, turn, Value::object([]));
        match host::run(loaded, agent, journal, &request) {
            Ok(Ran::Finished(finish)) => {
                refreshed += 1;
                eprintln!("hl-agent: idle turn {turn}: {}", finish.result.encode());
            }
            Ok(Ran::NotDue) => eprintln!("hl-agent: idle turn {turn} is not due"),
            Ok(Ran::Suspended { seq, .. }) => {
                eprintln!("hl-agent: idle turn {turn} suspended at {seq}");
                return;
            }
            Err(error) => {
                eprintln!("hl-agent: idle turn {turn}: {error}");
                return;
            }
        }
    }
}

pub fn now_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970");
    u64::try_from(elapsed.as_millis()).expect("milliseconds since 1970 fit in u64")
}

pub struct Ledger {
    pub file: std::fs::File,
}

impl Ledger {
    pub fn append(&mut self, line: Value) {
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
    payer: Option<mpp::Payer>,
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
        let call = harnesslang::run::journal::call_id(turn, seq);
        let translated;
        let (tool, input) = match self.payer.as_mut() {
            Some(payer) if payer.wallet == tool => return paid(payer.pay(&call, input)),
            Some(payer) if tool == mpp::FETCH_TOOL => {
                eprintln!("{}: {call} fetch", self.label);
                return paid(payer.fetch(&call, input));
            }
            Some(_) => match claude_code_call(tool, input) {
                Some((name, renamed)) => {
                    translated = renamed;
                    (name, &translated)
                }
                None => (tool, input),
            },
            None => (tool, input),
        };
        let receipted =
            matches!(tool, "Bash" | "Write") && (self.children.is_some() || self.payer.is_some());
        if receipted && let Err(error) = self.workspace.dispatching(turn, seq) {
            return tools::Ran::Failed(format!(
                "<tool_use_error>receipt: {error}</tool_use_error>"
            ));
        }
        self.ledger.append(Value::object([
            ("kind", Value::text("tool")),
            ("at_ms", Value::number(now_ms())),
            ("tool", Value::text(tool)),
            (
                "call",
                Value::text(&harnesslang::run::journal::call_id(turn, seq)),
            ),
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
            harnesslang::run::journal::call_id(turn, seq)
        );
        let ran = match &mut self.children {
            Some(children) if tool == children::SPAWN_TOOL => children.spawn(turn, seq, input),
            Some(children) if tool == children::LIST_TOOL => children.list(input),
            None if tool == children::SUBMIT_TOOL => children::submitted(input),
            _ if receipted && tool == "Bash" => self.bash(turn, seq, input, room),
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
        let call = harnesslang::run::journal::call_id(turn, seq);
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

fn claude_code_call(tool: &str, input: &Value) -> Option<(&'static str, Value)> {
    match tool {
        "read" => Some((
            "Read",
            Value::object([
                ("file_path", input.get("path")?.clone()),
                ("offset", Value::number(harness::READ_OFFSET_FIRST)),
                ("limit", Value::number(harness::READ_LIMIT_WHOLE)),
                ("pages", Value::text("")),
                ("allow_large", Value::Bool(false)),
            ]),
        )),
        "bash" => Some((
            "Bash",
            Value::object([
                ("command", input.get("command")?.clone()),
                ("timeout", Value::number(harness::BASH_TIMEOUT_MS)),
                ("description", Value::text("")),
                ("run_in_background", Value::Bool(false)),
                ("dangerouslyDisableSandbox", Value::Bool(false)),
            ]),
        )),
        _ => None,
    }
}

fn paid(ran: mpp::Ran) -> tools::Ran {
    match ran {
        mpp::Ran::Ok(text) => tools::Ran::Ok(text),
        mpp::Ran::Failed(text) => tools::Ran::Failed(text),
        mpp::Ran::Units(units) => tools::Ran::Units(units),
    }
}

pub fn random() -> u64 {
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
            tools::Ran::Units(units) => $module::Output::Ok($module::Json::Number(units)),
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
                let recovered = match self.payer.as_ref() {
                    Some(payer) if payer.wallet == request.tool => {
                        match payer.recover_pay(&$module::call_id(request.turn, request.seq)) {
                            Some(units) => tools::Recovered::Reply(tools::Ran::Units(units)),
                            None => tools::Recovered::NotDispatched,
                        }
                    }
                    Some(_) => match claude_code_call(request.tool, &input) {
                        Some((tool, input)) => {
                            self.workspace
                                .recover(request.turn, request.seq, tool, &input)
                        }
                        None => {
                            self.workspace
                                .recover(request.turn, request.seq, request.tool, &input)
                        }
                    },
                    None => self
                        .workspace
                        .recover(request.turn, request.seq, request.tool, &input),
                };
                eprintln!(
                    "{}: {} recover {}: {}",
                    self.label,
                    $module::call_id(request.turn, request.seq),
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
                        Value::text(&$module::call_id(request.turn, request.seq)),
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
                    .map(|notice| $module::Delivery {
                        id: notice.id,
                        inbound: $module::Inbound::Job($module::JobNotice {
                            key: notice.key,
                            notice: notice.notice,
                            done: notice.done,
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
                let mut reply = self.run_model(
                    request.turn,
                    request.seq,
                    request.mode,
                    effort,
                    &context,
                    &attached,
                );
                if self.payer.is_some() {
                    for _ in 1..PLAIN_MODEL_ATTEMPTS {
                        if mpp::plain_reply(&reply).is_some() {
                            break;
                        }
                        std::thread::sleep(PLAIN_MODEL_RETRY);
                        reply = self.run_model(
                            request.turn,
                            request.seq,
                            request.mode,
                            effort,
                            &context,
                            &attached,
                        );
                    }
                    reply = mpp::plain_reply(&reply).unwrap_or(reply);
                }
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

host_for!(mpp_harness);

pub fn command_line(program: &str, args: &[&str]) -> Option<String> {
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
    reasoning: u64,
    cost_nano_usd: u64,
    priced: u64,
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
        self.reasoning += child.reasoning;
        self.cost_nano_usd += child.cost_nano_usd;
        self.priced += child.priced;
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
                totals.reasoning += field("reasoning_tokens");
                if value.get("cost_nano_usd").is_some() {
                    totals.priced += 1;
                    totals.cost_nano_usd += field("cost_nano_usd");
                }
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

pub fn write_usage(journal: &Path, model: &str, finish: &str) -> Result<(), String> {
    let mut totals = totals(&journal.join("usage.jsonl"))?;
    for ledger in children::ledgers(journal)? {
        totals.add_child(&self::totals(&ledger)?);
    }
    let mut usage = Value::object([
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
    if totals.reasoning > 0 {
        usage.insert("reasoning_tokens", Value::number(totals.reasoning));
    }
    if totals.priced > 0 {
        usage.insert(
            "cost_usd",
            Value::Number(format!(
                "{:.6}",
                totals.cost_nano_usd as f64 / openai::NANO_USD_PER_USD
            )),
        );
        usage.insert("priced_requests", Value::number(totals.priced));
    }
    let temporary = journal.join("usage.json.new");
    std::fs::write(&temporary, usage.encode() + "\n")
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, journal.join("usage.json"))
        .map_err(|error| format!("usage.json: {error}"))
}

fn outcome(finish: &Finish) -> (String, String) {
    outcome_of(&finish.result)
}

pub fn outcome_of(result: &Value) -> (String, String) {
    match result {
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
        image_bytes_max: harness::READ_BYTES_MAX,
    };
    Ok((
        Agent {
            client,
            workspace,
            ledger: Ledger { file },
            children: Some(children),
            label: "hl-agent".to_owned(),
            jobs: jobs::Jobs::default(),
            payer: None,
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

fn fork_into(parent: &Path, at: Option<u64>, journal: &mut Journal) -> Result<(), String> {
    let parent = if parent.is_dir() {
        parent.join("journal.jsonl")
    } else {
        parent.to_path_buf()
    };
    if !journal.read()?.is_empty() {
        return match host::records(journal.path())?.first() {
            Some(Record::Host { key, .. }) if key == harnesslang::ir::FORKED_HOST_KEY => Ok(()),
            _ => Err("--fork-from names a parent, but the journal already holds a session that was not forked".to_owned()),
        };
    }
    if !parent.is_file() {
        return Err(format!("{}: no such journal", parent.display()));
    }
    let records = host::records(&parent)?;
    let program = harnesslang::load::load(harness::PROGRAM)?;
    let origin = parent.display().to_string();
    let model = harnesslang::target::model(harnesslang::target::Target::NativeJsonl);
    let entry = harnesslang::run::fork::fork_entry(&program, &model, &records, at, &origin)
        .map_err(|refused| format!("{}: {}", parent.display(), refused.describe()))?;
    journal.append(&harnesslang::run::journal::encode_lines(
        &entry,
        JOURNAL_CHECKSUMS,
    ))?;
    eprintln!(
        "hl-agent: forked from {origin} at {}",
        match &entry[0] {
            Record::Host { value, .. } => value.encode(),
            _ => String::new(),
        }
    );
    Ok(())
}

fn run_turn(
    loaded: &Loaded,
    agent: &mut Agent,
    journal: &mut Journal,
    request: &harnesslang::wasm::js::Json,
) -> Result<Ran, String> {
    let mut ran = host::run(loaded, agent, journal, request);
    let mut waited = None;
    for _ in 0..JOINS_PER_TURN_MAX {
        let Ok(Ran::Suspended { seq, .. }) = &ran else {
            return ran;
        };
        if waited == Some(*seq) {
            return ran;
        }
        waited = Some(*seq);
        ran = host::run(loaded, agent, journal, request);
    }
    ran
}

fn paying_setup(options: &Options) -> Result<(Agent, String), String> {
    let (mut agent, instruction) = setup(options)?;
    let wallet = mpp_harness::program::WALLET.ok_or("examples/mpp_agent.hl declares no wallet")?;
    agent.client.tools = Value::decode(MPP_TOOL_DEFINITIONS)
        .map_err(|error| format!("the bundled MPP tool definitions: {error}"))?;
    let Value::Array(mut system) = Value::decode(MPP_SYSTEM_PROMPT)
        .map_err(|error| format!("the bundled MPP system prompt: {error}"))?
    else {
        return Err("the bundled MPP system prompt is not a list of blocks".to_owned());
    };
    system.push(Value::object([
        ("type", Value::text("text")),
        (
            "text",
            Value::String(format!(
                "Make at most {} tool calls in one reply; the harness refuses a reply with more.",
                mpp_harness::program::CALLS_PER_STEP_MAX
            )),
        ),
    ]));
    agent.client.system = Value::Array(system);
    agent.children = None;
    agent.payer = Some(mpp::Payer::open(wallet, &agent.workspace.state, random())?);
    Ok((agent, instruction))
}

fn pay_main(options: &Options) -> ! {
    mpp_harness::self_check();
    mpp_harness::jsonl_self_check();
    let (mut agent, instruction) = paying_setup(options).unwrap_or_else(|error| {
        eprintln!("hl-agent: {error}");
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    agent.ledger.append(Value::object([
        ("kind", Value::text("start")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let journal_path = options.journal.join("journal.jsonl");
    let mut disk = mpp_harness::FileDisk::open(&journal_path).unwrap_or_else(|error| {
        eprintln!("hl-agent: {}: {error}", journal_path.display());
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    let request = mpp_harness::TurnRequest {
        turn: options.turn,
        arguments: mpp_harness::program::Arguments::Prompt(instruction),
    };
    let ran = mpp_harness::run_jsonl(&mut agent, &mut disk, &request);
    agent.ledger.append(Value::object([
        ("kind", Value::text("end")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let (finish, code) = match &ran {
        Ok(mpp_harness::Ran::Finished(finish) | mpp_harness::Ran::AlreadyFinished(finish))
            if finish.outcome == mpp_harness::Outcome::Completed =>
        {
            println!("{}", finish.result.encode());
            ("completed".to_owned(), 0)
        }
        Ok(mpp_harness::Ran::Finished(finish) | mpp_harness::Ran::AlreadyFinished(finish)) => {
            eprintln!("hl-agent: the harness trapped: {}", finish.result.encode());
            ("trap".to_owned(), EXIT_HARNESS_ERROR)
        }
        Ok(other) => {
            eprintln!("hl-agent: the paying turn did not finish: {other:?}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
        Err(error) => {
            eprintln!("hl-agent: {error:?}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
    };
    if let Err(error) = write_usage(&options.journal, &options.model, &finish) {
        eprintln!("hl-agent: {error}");
        std::process::exit(EXIT_HARNESS_ERROR);
    }
    std::process::exit(code)
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("pay") {
        pay_main(&options());
    }
    let loaded = host::load(harness::MODULE);
    let options = options();
    if options.harness == HARNESS_CODEX {
        codex_host::main(&options);
    }
    let (mut agent, instruction) = setup(&options).unwrap_or_else(|error| {
        eprintln!("hl-agent: {error}");
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    agent.ledger.append(Value::object([
        ("kind", Value::text("start")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let journal_path = options.journal.join("journal.jsonl");
    let mut disk = Journal::open(&journal_path).unwrap_or_else(|error| {
        eprintln!("hl-agent: {}: {error}", journal_path.display());
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    if let Some(parent) = &options.fork_from
        && let Err(error) = fork_into(parent, options.at, &mut disk)
    {
        eprintln!("hl-agent: --fork-from: {error}");
        std::process::exit(EXIT_HARNESS_ERROR);
    }
    let mut turn = options.turn;
    if let Some(level) = &options.effort {
        let setting = host::request(
            SET_EFFORT_HANDLER,
            turn,
            Value::object([("level", Value::text(level))]),
        );
        let ended = match run_turn(&loaded, &mut agent, &mut disk, &setting) {
            Ok(Ran::Finished(finish)) if finish.completed => None,
            Ok(Ran::Finished(finish)) => Some(format!("in a trap: {}", finish.result.encode())),
            Ok(Ran::Suspended { seq, .. }) => Some(format!("suspended at {seq}")),
            Ok(Ran::NotDue) => Some("refused as an idle turn".to_owned()),
            Err(error) => Some(error),
        };
        if let Some(ended) = ended {
            eprintln!("hl-agent: --effort: the set_effort turn ended {ended}");
            std::process::exit(EXIT_HARNESS_ERROR);
        }
        turn += 1;
    }
    let request = host::request(
        PROMPT_HANDLER,
        turn,
        Value::object([
            ("input", Value::text(&instruction)),
            ("rounds", Value::number(options.rounds)),
        ]),
    );
    let ran = run_turn(&loaded, &mut agent, &mut disk, &request);
    agent.jobs.stop_all();
    if options.keep_warm > 0 && matches!(&ran, Ok(Ran::Finished(_))) {
        keep_warm(&loaded, &mut agent, &mut disk, options.keep_warm);
    }
    if let Some(children) = agent.children.as_mut() {
        children.finish();
    }
    agent.ledger.append(Value::object([
        ("kind", Value::text("end")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let (finish, code) = match &ran {
        Ok(Ran::Finished(finish)) if finish.completed => {
            let (name, text) = outcome(finish);
            println!("{text}");
            (name, 0)
        }
        Ok(Ran::Finished(finish)) => {
            eprintln!(
                "hl-agent: the harness trapped in turn {}: {}",
                finish.turn,
                finish.result.encode()
            );
            ("trap".to_owned(), EXIT_HARNESS_ERROR)
        }
        Ok(Ran::Suspended { turn, seq }) => {
            eprintln!(
                "hl-agent: turn {turn} waits on suspend {seq}, which this host could not resume"
            );
            ("suspended".to_owned(), EXIT_HARNESS_ERROR)
        }
        Ok(Ran::NotDue) => {
            eprintln!("hl-agent: the prompt turn was refused as an idle turn");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
        Err(error) => {
            eprintln!("hl-agent: {error}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
    };
    if let Err(error) = write_usage(&options.journal, &options.model, &finish) {
        eprintln!("hl-agent: {error}");
        std::process::exit(EXIT_HARNESS_ERROR);
    }
    std::process::exit(code);
}
