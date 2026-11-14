#[path = "../anthropic.rs"]
#[allow(dead_code)]
mod anthropic;
#[path = "../json.rs"]
#[allow(dead_code)]
mod json;
#[path = "../stream.rs"]
#[allow(dead_code)]
mod stream;
#[path = "../tools.rs"]
#[allow(dead_code)]
mod tools;

include!(concat!(env!("OUT_DIR"), "/climb_mod.rs"));

use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anthropic::{Client, Usage};
use climb_harness::program::{self, Arguments};
use climb_harness::{Exit, FileDisk, Json, Output, Ran, Recovered, RunError, TurnRequest};
use json::Value;

const EXIT_HARNESS_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 2;
const FIRST_TURN: u64 = 1;
const LABEL: &str = "hl-climb";
const TOOL_DEFINITIONS: &str = include_str!("../../prompts/climb-tools.json");
const USAGE: &str = "usage: hl-climb run --goal <file> --cwd <repo> --bench <command> --test <command> --model <id> --journal <dir>";
const BASELINE: &str = "baseline";
const GIT_IDENTITY: [&str; 6] = [
    "-c",
    "user.name=hl-climb",
    "-c",
    "user.email=hl-climb@localhost",
    "-c",
    "commit.gpgsign=false",
];
const BENCH_TREE: &str = "bench-tree";
const SHELL: &str = "bash";
const OFFERED: [&str; 3] = ["Bash", "Read", "Edit"];
const RECEIPTED: [&str; 3] = ["Bash", "Edit", "commit"];
const RECEIPT_RECOVERY: &str = "Bash";
const READ_OFFSET_FIRST: u64 = 1;
const READ_LIMIT_WHOLE: u64 = 0;
const MODEL_ATTEMPTS_MAX: u32 = 6;
const RETRY_BASE_MS: u64 = 2_000;
const RETRY_WAIT_MAX_MS: u64 = 60_000;
const OUTPUT_TAIL_CHARS: usize = tools::ERROR_CHARS;
const STOPS_REFUSED: [&str; 2] = ["context_exceeded", "refusal"];
const SHOWN_FIELDS: [&str; 4] = ["command", "file_path", "message", "revision"];
const PASSED: u64 = 0;
const FAILING: u64 = 1;

struct Options {
    goal: PathBuf,
    cwd: PathBuf,
    bench: String,
    test: String,
    model: String,
    journal: PathBuf,
}

fn fail_usage(message: &str) -> ! {
    eprintln!("{LABEL}: {message}\n{USAGE}");
    std::process::exit(EXIT_USAGE)
}

fn options() -> Options {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("run") {
        fail_usage("the only command is run");
    }
    let (mut goal, mut cwd, mut bench, mut test, mut model, mut journal) =
        (None, None, None, None, None, None);
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .unwrap_or_else(|| fail_usage(&format!("{flag} needs a value")))
            .clone();
        match flag.as_str() {
            "--goal" => goal = Some(PathBuf::from(value)),
            "--cwd" => cwd = Some(PathBuf::from(value)),
            "--bench" => bench = Some(value),
            "--test" => test = Some(value),
            "--model" => model = Some(value),
            "--journal" => journal = Some(PathBuf::from(value)),
            other => fail_usage(&format!("unknown flag {other}")),
        }
    }
    Options {
        goal: goal.unwrap_or_else(|| fail_usage("--goal is required")),
        cwd: cwd.unwrap_or_else(|| fail_usage("--cwd is required")),
        bench: bench.unwrap_or_else(|| fail_usage("--bench is required")),
        test: test.unwrap_or_else(|| fail_usage("--test is required")),
        model: model.unwrap_or_else(|| fail_usage("--model is required")),
        journal: journal.unwrap_or_else(|| fail_usage("--journal is required")),
    }
}

fn now_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970");
    u64::try_from(elapsed.as_millis()).expect("milliseconds since 1970 fit in u64")
}

fn random() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970")
        .subsec_nanos();
    u64::from(nanos) ^ now_ms().rotate_left(u32::BITS)
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

struct Shelled {
    code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

impl Shelled {
    fn tail(&self) -> String {
        let both = format!("{}{}", self.stdout, self.stderr);
        let skipped = both.chars().count().saturating_sub(OUTPUT_TAIL_CHARS);
        both.chars().skip(skipped).collect()
    }
}

struct Climber {
    client: Client,
    workspace: tools::Workspace,
    ledger: Ledger,
    cwd: PathBuf,
    state: PathBuf,
    bench_tree: PathBuf,
    bench: String,
    test: String,
}

fn text_field<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing or invalid {key}"))
}

fn with_defaults(tool: &str, mut input: Value) -> Value {
    let defaults: Vec<(&str, Value)> = match tool {
        "Bash" => vec![
            ("description", Value::text("")),
            ("run_in_background", Value::Bool(false)),
            ("dangerouslyDisableSandbox", Value::Bool(false)),
        ],
        "Read" => vec![
            ("offset", Value::number(READ_OFFSET_FIRST)),
            ("limit", Value::number(READ_LIMIT_WHOLE)),
            ("pages", Value::text("")),
            ("allow_large", Value::Bool(false)),
        ],
        "Edit" => vec![("replace_all", Value::Bool(false))],
        _ => Vec::new(),
    };
    for (key, value) in defaults {
        if input.get(key).is_none() {
            input.insert(key, value);
        }
    }
    input
}

fn git(directory: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(GIT_IDENTITY)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} in {} exited {}: {}",
            args.join(" "),
            directory.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn head(directory: &Path) -> Result<String, String> {
    git(directory, &["rev-parse", "HEAD"])
}

fn failed(text: &str) -> Value {
    Value::object([("failed", Value::text(text))])
}

fn tagged(value: &Value) -> Option<(&str, &[Value])> {
    match value {
        Value::Object(entries) if entries.len() == 1 => {
            Some((entries[0].0.as_str(), entries[0].1.as_array()?))
        }
        _ => None,
    }
}

fn output_of(ran: tools::Ran) -> Output {
    match ran {
        tools::Ran::Ok(text) => Output::Ok(Json::String(text)),
        tools::Ran::Failed(text) => Output::Failed(text),
        tools::Ran::Image(_) => Output::Failed("this host returns text only".to_owned()),
        tools::Ran::Job(text) => Output::Failed(format!("this host runs no jobs: {text}")),
    }
}

fn text_output(result: Result<String, String>) -> Output {
    match result {
        Ok(text) => Output::Ok(Json::String(text)),
        Err(error) => Output::Failed(error),
    }
}

fn number_output(result: Result<u64, String>) -> Output {
    match result {
        Ok(number) => Output::Ok(Json::Number(number)),
        Err(error) => Output::Failed(error),
    }
}

impl Climber {
    fn commit(&self, input: &Value) -> Result<String, String> {
        let message = text_field(input, "message")?;
        if message.trim().is_empty() {
            return Err("the commit message is empty".to_owned());
        }
        git(&self.cwd, &["add", "-A"])?;
        let staged = git(&self.cwd, &["diff", "--cached", "--name-only"])?;
        if staged.is_empty() {
            if message == BASELINE {
                return head(&self.cwd);
            }
            return Err(
                "nothing to commit: the working tree matches HEAD, so the candidate changed nothing"
                    .to_owned(),
            );
        }
        git(&self.cwd, &["commit", "-q", "--no-verify", "-m", message])?;
        head(&self.cwd)
    }

    fn restore(&self, input: &Value) -> Result<String, String> {
        let revision = text_field(input, "revision")?;
        git(
            &self.cwd,
            &["checkout", "-q", "--force", "--detach", revision],
        )?;
        git(&self.cwd, &["clean", "-q", "-fdx"])?;
        head(&self.cwd)
    }

    fn checkout_bench(&self, revision: &str) -> Result<(), String> {
        if !self.bench_tree.join(".git").exists() {
            if self.bench_tree.exists() {
                std::fs::remove_dir_all(&self.bench_tree)
                    .map_err(|error| format!("{}: {error}", self.bench_tree.display()))?;
            }
            git(&self.cwd, &["worktree", "prune"])?;
            let tree = self.bench_tree.display().to_string();
            git(&self.cwd, &["worktree", "add", "-q", "--detach", &tree])?;
        }
        git(
            &self.bench_tree,
            &["checkout", "-q", "--force", "--detach", revision],
        )?;
        git(&self.bench_tree, &["clean", "-q", "-fdx"])?;
        Ok(())
    }

    fn shell(&self, command: &str, name: &str) -> Result<Shelled, String> {
        let stdout_path = self.state.join(format!("{name}.stdout"));
        let stderr_path = self.state.join(format!("{name}.stderr"));
        let stdout = std::fs::File::create(&stdout_path)
            .map_err(|error| format!("{}: {error}", stdout_path.display()))?;
        let stderr = std::fs::File::create(&stderr_path)
            .map_err(|error| format!("{}: {error}", stderr_path.display()))?;
        let mut child = Command::new(SHELL)
            .arg("-c")
            .arg(command)
            .current_dir(&self.bench_tree)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0)
            .spawn()
            .map_err(|error| format!("start {SHELL} -c {command}: {error}"))?;
        let timeout = Duration::from_millis(program::BASH_TIMEOUT_MS);
        let begun = Instant::now();
        let (status, timed_out) = loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                break (status, false);
            }
            if begun.elapsed() >= timeout {
                tools::kill_group(&child);
                break (child.wait().map_err(|error| error.to_string())?, true);
            }
            std::thread::sleep(tools::TIMEOUT_POLL);
        };
        let read = |path: &Path| {
            std::fs::read(path)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|error| format!("{}: {error}", path.display()))
        };
        Ok(Shelled {
            code: status.code(),
            timed_out,
            stdout: read(&stdout_path)?,
            stderr: read(&stderr_path)?,
        })
    }

    fn bench(&self, input: &Value) -> Result<u64, String> {
        let revision = text_field(input, "revision")?;
        self.checkout_bench(revision)?;
        let ran = self.shell(&self.bench, "bench")?;
        if ran.timed_out {
            return Err(format!(
                "the benchmark ran past BASH_TIMEOUT_MS ({} ms): {}",
                program::BASH_TIMEOUT_MS,
                ran.tail()
            ));
        }
        if ran.code != Some(0) {
            return Err(format!(
                "the benchmark exited {:?}: {}",
                ran.code,
                ran.tail()
            ));
        }
        let last = ran
            .stdout
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        last.trim().parse::<u64>().map_err(|_| {
            format!(
                "the benchmark's last line {last:?} is not microseconds: {}",
                ran.tail()
            )
        })
    }

    fn failing(&self, input: &Value) -> Result<u64, String> {
        let revision = text_field(input, "revision")?;
        self.checkout_bench(revision)?;
        let ran = self.shell(&self.test, "test")?;
        Ok(if ran.code == Some(0) && !ran.timed_out {
            PASSED
        } else {
            FAILING
        })
    }

    fn record(&mut self, kind: &str, fields: Vec<(&str, Value)>) {
        let mut line = Value::object([
            ("kind", Value::text(kind)),
            ("at_ms", Value::number(now_ms())),
        ]);
        for (key, value) in fields {
            line.insert(key, value);
        }
        self.ledger.append(line);
    }

    fn dispatch(&mut self, turn: u64, seq: u64, tool: &str, input: Value) -> Output {
        let call = climb_harness::call_id(turn, seq);
        let receipted = RECEIPTED.contains(&tool);
        if receipted && let Err(error) = self.workspace.dispatching(turn, seq) {
            return Output::Failed(format!("receipt: {error}"));
        }
        self.record(
            "tool",
            vec![("tool", Value::text(tool)), ("call", Value::text(&call))],
        );
        let shown = SHOWN_FIELDS
            .iter()
            .find_map(|key| input.get(key).and_then(Value::as_str))
            .unwrap_or("")
            .to_owned();
        eprintln!("{LABEL}: {call} {tool}: {shown}");
        let output = match tool {
            "Bash" | "Read" | "Edit" => {
                let ran = self.workspace.run(tool, &with_defaults(tool, input));
                if receipted {
                    self.workspace.dispatched(turn, seq, &ran);
                }
                output_of(ran)
            }
            "commit" => {
                let ran = match self.commit(&input) {
                    Ok(sha) => tools::Ran::Ok(sha),
                    Err(error) => tools::Ran::Failed(error),
                };
                self.workspace.dispatched(turn, seq, &ran);
                output_of(ran)
            }
            "restore" => text_output(self.restore(&input)),
            "bench" => number_output(self.bench(&input)),
            "failing" => number_output(self.failing(&input)),
            other => Output::Failed(tools::unavailable(other, &OFFERED)),
        };
        if !OFFERED.contains(&tool) {
            let (key, value) = match &output {
                Output::Ok(Json::String(text)) => ("ok", Value::text(text)),
                Output::Ok(json) => ("ok", Value::text(&json.encode())),
                Output::Failed(error) => ("failed", Value::text(error)),
            };
            eprintln!(
                "{LABEL}: {call} {tool} {key}: {}",
                value.as_str().unwrap_or("")
            );
            self.record(
                "outcome",
                vec![
                    ("tool", Value::text(tool)),
                    ("call", Value::text(&call)),
                    ("input", Value::String(shown)),
                    (key, value),
                ],
            );
        }
        output
    }

    fn attempt(&mut self, turn: u64, seq: u64, body: &Value) -> (Value, Usage) {
        let call = climb_harness::call_id(turn, seq);
        let partial = self.state.join(format!("stream-{turn}-{seq}.sse"));
        if let Ok(cut) = std::fs::metadata(&partial) {
            eprintln!(
                "{LABEL}: {call} a crash cut the previous stream of this call after {} bytes; requesting it again",
                cut.len()
            );
            self.record(
                "stream_discarded",
                vec![
                    ("call", Value::text(&call)),
                    ("bytes", Value::number(cut.len())),
                ],
            );
        }
        if let Err(error) = std::fs::write(&partial, "") {
            return (
                anthropic::transient(&format!("{}: {error}", partial.display()), 0),
                Usage::default(),
            );
        }
        eprintln!("{LABEL}: {call} model {}", self.client.model);
        let attempted = match self.client.send(body, &partial) {
            Ok((status, text, retry_after_ms)) => {
                anthropic::interpret_stream(status, &text, retry_after_ms)
            }
            Err(error) => (anthropic::transient(&error, 0), Usage::default()),
        };
        if let Err(error) = std::fs::remove_file(&partial) {
            eprintln!("{LABEL}: {}: {error}", partial.display());
        }
        attempted
    }

    fn ask(&mut self, turn: u64, seq: u64, context: &Value) -> Value {
        let call = climb_harness::call_id(turn, seq);
        let body = match self.client.body(context, None, None, &|_| None) {
            Ok(body) => body,
            Err(error) => return failed(&format!("context does not render: {error}")),
        };
        let mut last = String::new();
        for attempt in 0..MODEL_ATTEMPTS_MAX {
            let (reply, used) = self.attempt(turn, seq, &body);
            let Some((outcome, fields)) = tagged(&reply) else {
                return failed(&format!("the host built no attempt: {}", reply.encode()));
            };
            self.record(
                "model",
                vec![
                    ("call", Value::text(&call)),
                    ("outcome", Value::text(outcome)),
                    ("input_tokens", Value::number(used.input)),
                    ("output_tokens", Value::number(used.output)),
                    ("cache_read_tokens", Value::number(used.cache_read)),
                    ("cache_write_tokens", Value::number(used.cache_write)),
                ],
            );
            let text = fields.first().and_then(Value::as_str).unwrap_or("");
            match outcome {
                "ok" => {
                    let stop = fields.get(1).and_then(Value::as_str).unwrap_or("");
                    if STOPS_REFUSED.contains(&stop) {
                        return failed(&format!("the model stopped with {stop}"));
                    }
                    return fields
                        .first()
                        .cloned()
                        .unwrap_or_else(|| failed("an ok attempt holds no reply"));
                }
                "rejected" => return failed(text),
                _ => {
                    last = text.to_owned();
                    let retry_after = fields.get(1).and_then(Value::as_u64).unwrap_or(0);
                    let backoff = RETRY_BASE_MS
                        .checked_shl(attempt)
                        .unwrap_or(RETRY_WAIT_MAX_MS)
                        .min(RETRY_WAIT_MAX_MS);
                    let wait = retry_after.max(backoff);
                    eprintln!("{LABEL}: {call} model {outcome}: {last}; waiting {wait} ms");
                    std::thread::sleep(Duration::from_millis(wait));
                }
            }
        }
        failed(&format!(
            "the model failed all MODEL_ATTEMPTS_MAX ({MODEL_ATTEMPTS_MAX}) attempts: {last}"
        ))
    }
}

impl climb_harness::Host for Climber {
    fn tool(&mut self, request: &climb_harness::ToolRequest<'_>) -> Output {
        let input = Value::decode(&request.input.encode()).expect("the harness encodes valid JSON");
        self.dispatch(request.turn, request.seq, request.tool, input)
    }

    fn recover(&mut self, request: &climb_harness::ToolRequest<'_>) -> Recovered {
        let recovered = if RECEIPTED.contains(&request.tool) {
            self.workspace
                .recover(request.turn, request.seq, RECEIPT_RECOVERY, &Value::Null)
        } else {
            tools::Recovered::Unknown
        };
        let (answer, recovered) = match recovered {
            tools::Recovered::Reply(ran) => ("reply", Recovered::Reply(output_of(ran))),
            tools::Recovered::NotDispatched => ("not_dispatched", Recovered::NotDispatched),
            tools::Recovered::Unknown => ("unknown", Recovered::Unknown),
        };
        let call = climb_harness::call_id(request.turn, request.seq);
        eprintln!("{LABEL}: {call} recover {}: {answer}", request.tool);
        self.record(
            "recover",
            vec![
                ("tool", Value::text(request.tool)),
                ("call", Value::text(&call)),
                ("answer", Value::text(answer)),
            ],
        );
        recovered
    }

    fn listing(&mut self, _request: &climb_harness::ListingRequest<'_>) -> Output {
        Output::Ok(Json::Object(std::collections::BTreeMap::new()))
    }

    fn model(&mut self, request: &climb_harness::ModelRequest<'_>) -> Json {
        let context =
            Value::decode(&(request.context)().encode()).expect("the harness encodes valid JSON");
        let reply = self.ask(request.turn, request.seq, &context);
        Json::decode(&reply.encode()).expect("a reply the host built decodes as harness JSON")
    }

    fn now(&mut self) -> u64 {
        now_ms()
    }

    fn random(&mut self) -> u64 {
        random()
    }

    fn acknowledge(&mut self, _finish: &climb_harness::Finish) {}
}

fn system_prompt(cwd: &Path, bench: &str, test: &str) -> Value {
    let text = format!(
        "You are improving the performance of the code in the git working tree at {}. \
The harness benchmarks each change you make: it runs '{bench}' there, whose last output line is the time in microseconds (lower is better), and it runs '{test}', which must keep passing. \
You may run the benchmark and the tests yourself with the Bash tool. \
Make exactly one focused change, keep the tests passing, and do not commit, branch or check out with git: the harness commits your change and restores revisions itself. \
When the change is made, end with a text answer, without calling a tool, that states the change and the hypothesis for why it is faster in one or two plain sentences: that answer becomes the commit message verbatim, so write nothing else in it, no greeting, heading, list or Markdown.",
        cwd.display()
    );
    Value::Array(vec![Value::object([
        ("type", Value::text("text")),
        ("text", Value::String(text)),
    ])])
}

fn setup(options: &Options) -> Result<(Climber, String), String> {
    let goal = std::fs::read_to_string(&options.goal)
        .map_err(|error| format!("{}: {error}", options.goal.display()))?;
    let cwd = std::fs::canonicalize(&options.cwd)
        .map_err(|error| format!("{}: {error}", options.cwd.display()))?;
    let key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY is not set".to_owned())?;
    let base_url = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| anthropic::BASE_URL_DEFAULT.to_owned());
    let max_tokens = anthropic::model_max_tokens(&options.model).ok_or_else(|| {
        format!(
            "unknown Claude model {}: the table has no max_tokens for it",
            options.model
        )
    })?;
    std::fs::create_dir_all(&options.journal)
        .map_err(|error| format!("{}: {error}", options.journal.display()))?;
    let journal = std::fs::canonicalize(&options.journal)
        .map_err(|error| format!("{}: {error}", options.journal.display()))?;
    if journal.starts_with(&cwd) {
        return Err(format!(
            "the journal {} is inside the working tree {}, whose restores clean it away",
            journal.display(),
            cwd.display()
        ));
    }
    let state = journal.join("state");
    std::fs::create_dir_all(&state).map_err(|error| format!("{}: {error}", state.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal.join("usage.jsonl"))
        .map_err(|error| format!("usage.jsonl: {error}"))?;
    let tools = Value::decode(TOOL_DEFINITIONS)
        .map_err(|error| format!("the bundled tool definitions: {error}"))?;
    let client = Client {
        base_url,
        key,
        model: options.model.clone(),
        max_tokens,
        system: system_prompt(&cwd, &options.bench, &options.test),
        tools,
        scratch: state.clone(),
    };
    let workspace = tools::Workspace {
        root: cwd.clone(),
        state: state.clone(),
        image_bytes_max: None,
        left: std::sync::Mutex::new(Vec::new()),
    };
    Ok((
        Climber {
            client,
            workspace,
            ledger: Ledger { file },
            cwd,
            bench_tree: journal.join(BENCH_TREE),
            state,
            bench: options.bench.clone(),
            test: options.test.clone(),
        },
        goal,
    ))
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
        let value =
            Value::decode(line).map_err(|error| format!("{}: {error}", ledger.display()))?;
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

fn result_text(result: &Json) -> String {
    match result {
        Json::String(text) => text.clone(),
        other => other.encode(),
    }
}

fn main() {
    climb_harness::self_check();
    climb_harness::jsonl_self_check();
    let options = options();
    let (mut climber, goal) = setup(&options).unwrap_or_else(|error| {
        eprintln!("{LABEL}: {error}");
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    climber.record("start", Vec::new());
    let journal_path = options.journal.join("journal.jsonl");
    let mut disk = FileDisk::open(&journal_path).unwrap_or_else(|error| {
        eprintln!("{LABEL}: {}: {error}", journal_path.display());
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    let request = TurnRequest {
        turn: FIRST_TURN,
        arguments: Arguments::Prompt(goal),
    };
    let ran = climb_harness::run_jsonl(&mut climber, &mut disk, &request);
    climber.workspace.stop_left();
    climber.record("end", Vec::new());
    let (finish, code) = match &ran {
        Ok(Ran::Finished(finish) | Ran::AlreadyFinished(finish))
            if finish.outcome == climb_harness::Outcome::Completed =>
        {
            println!("{}", result_text(&finish.result));
            ("completed", 0)
        }
        Ok(Ran::Finished(finish) | Ran::AlreadyFinished(finish)) => {
            eprintln!("{LABEL}: the harness trapped: {}", finish.result.encode());
            ("trap", EXIT_HARNESS_ERROR)
        }
        Ok(other) => {
            eprintln!("{LABEL}: the turn ended {other:?}, which this host cannot continue");
            ("error", EXIT_HARNESS_ERROR)
        }
        Err(RunError::Open(error)) => {
            eprintln!("{LABEL}: journal: {error}");
            ("error", EXIT_HARNESS_ERROR)
        }
        Err(RunError::Exit(Exit::Corrupt(error))) => {
            eprintln!("{LABEL}: journal: {error}");
            ("error", EXIT_HARNESS_ERROR)
        }
        Err(RunError::Exit(Exit::Stop(stop))) => {
            eprintln!("{LABEL}: stopped: {stop:?}");
            ("error", EXIT_HARNESS_ERROR)
        }
    };
    if let Err(error) = write_usage(&options.journal, &options.model, finish) {
        eprintln!("{LABEL}: {error}");
        std::process::exit(EXIT_HARNESS_ERROR);
    }
    std::process::exit(code);
}
