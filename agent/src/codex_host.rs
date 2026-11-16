use std::path::{Path, PathBuf};

use harnesslang::wasm::js::Json;
use harnesslang::wasm::native::{self, Journal, World};
use harnesslang::wasm::sim::Loaded;

use crate::codex;
use crate::codex_harness;
use crate::host;
use crate::json::Value;
use crate::openai;
use crate::reviewer::{self, Reviewer, Verdict};
use crate::tools;
use crate::transport::Sent;
use crate::{EXIT_HARNESS_ERROR, Ledger, Options, now_ms, outcome_of, random, write_usage};

const INSTRUCTIONS: &str = include_str!("../prompts/codex-instructions.md");
const TOOL_DEFINITIONS: &str = include_str!("../prompts/codex-tools.json");
const CUSTOM_TOOLS: [&str; 1] = ["apply_patch"];
const RECEIPTED_TOOLS: [&str; 2] = ["exec_command", "apply_patch"];
const CONTROL: &str = "control";
const CANCEL_PREFIX: &str = "cancel-";
const STEER_PREFIX: &str = "steer-";
const CACHE_KEY_FILE: &str = "cache_key";
const PREAMBLE_FILE: &str = "preamble.json";
const LABEL: &str = "hl-agent";
const FINISH_CHECKS_MAX: u64 = 2;
const REVIEWED: &str = "A reviewer read the task and the end of your transcript and found that the task is not done yet. Fix each point below, check each one by running commands, and end your turn only with a short report of the checks you ran and their results.";
const FINISH_CHECK: &str = "Before this task is done, check it by running commands, not by reading your own code. Go through each behaviour the task states, one by one: each file exists at the exact path it names with the content or format it asks for, each program runs with the exact command and interpreter it names, each service responds through the protocol, port and account the task names, and each output matches the stated format. Run every check now. If any check fails or a stated behaviour is not yet verified, fix it and continue working. Nobody will answer a question or supply anything: if something seems missing, find it in the environment or take the most reasonable assumption, and make every requested file and program exist and run. End your turn again only after the work passes, with a short report listing each check you ran and its result.";

const CANCELLABLE_TOOLS: [&str; 2] = ["exec_command", "write_stdin"];
const EFFORT_LEVELS: [&str; 4] = ["medium", "low", "high", "xhigh"];
const SET_EFFORT: &str = "set_effort";
const PROMPT: &str = "prompt";

pub struct CodexAgent {
    client: openai::Client,
    workspace: tools::Workspace,
    exec: codex::Exec,
    ledger: Ledger,
    control: PathBuf,
    reviewer: Option<Reviewer>,
    last_context: Value,
}

fn control_dir(journal: &Path) -> PathBuf {
    journal.join(CONTROL)
}

pub fn send_event(journal: &Path, steer: Option<&str>) -> Result<String, String> {
    let directory = control_dir(journal);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let (name, text) = match steer {
        Some(text) => (
            format!("{STEER_PREFIX}{:020}-{:016x}", now_ms(), random()),
            text,
        ),
        None => (
            format!("{CANCEL_PREFIX}{:020}-{:016x}", now_ms(), random()),
            "",
        ),
    };
    let temporary = directory.join(format!(".{name}.new"));
    std::fs::write(&temporary, text)
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, directory.join(&name))
        .map_err(|error| format!("{name}: {error}"))?;
    Ok(name)
}

fn cancel_pending(control: &Path) -> bool {
    std::fs::read_dir(control).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(CANCEL_PREFIX)
        })
    })
}

impl CodexAgent {
    fn review(&mut self, index: u64) -> Option<String> {
        let reviewer = self.reviewer.as_ref()?;
        let (verdict, used) = reviewer.review(&self.last_context, index);
        let outcome = if verdict.is_ok() { "ok" } else { "failed" };
        self.ledger.append(Value::object([
            ("kind", Value::text("model")),
            ("at_ms", Value::number(now_ms())),
            ("model", Value::text(reviewer::MODEL)),
            ("outcome", Value::text(outcome)),
            ("input_tokens", Value::number(used.input)),
            ("output_tokens", Value::number(used.output)),
            ("cache_read_tokens", Value::number(used.cache_read)),
            ("cache_write_tokens", Value::number(used.cache_write)),
            (
                "cost_nano_usd",
                Value::number(reviewer::cost_nano_usd(used)),
            ),
        ]));
        match verdict {
            Ok(Verdict::Done(text)) => {
                eprintln!("{LABEL}: review {index}: {text}");
                None
            }
            Ok(Verdict::NotDone(text)) => {
                eprintln!("{LABEL}: review {index}: {text}");
                Some(text)
            }
            Err(error) => {
                eprintln!("{LABEL}: review {index}: {error}");
                None
            }
        }
    }

    fn record_model(&mut self, used: openai::Usage, outcome: &str) {
        let mut line = Value::object([
            ("kind", Value::text("model")),
            ("at_ms", Value::number(now_ms())),
            ("outcome", Value::text(outcome)),
            (
                "input_tokens",
                Value::number(used.input.saturating_sub(used.cached)),
            ),
            ("output_tokens", Value::number(used.output)),
            ("cache_read_tokens", Value::number(used.cached)),
            ("cache_write_tokens", Value::number(0)),
            ("reasoning_tokens", Value::number(used.reasoning)),
        ]);
        if let Some(cost) = openai::cost_nano_usd(&self.client.model, used) {
            line.insert("cost_nano_usd", Value::number(cost));
        }
        self.ledger.append(line);
    }

    fn run_model(
        &mut self,
        turn: u64,
        seq: u64,
        effort: Option<&str>,
        context: &Value,
        cancellable: bool,
        attached: &openai::Attached<'_>,
    ) -> Value {
        let call = harnesslang::run::journal::call_id(turn, seq);
        let body = match self.client.body(context, effort, attached) {
            Ok(body) => body,
            Err(error) => {
                return openai::transient(&format!("context does not render: {error}"), 0);
            }
        };
        let partial = self
            .workspace
            .state
            .join(format!("stream-{turn}-{seq}.sse"));
        if let Ok(cut) = std::fs::metadata(&partial) {
            eprintln!(
                "{LABEL}: {call} a crash cut the previous stream of this call after {} bytes; requesting it again",
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
            return openai::transient(&format!("{}: {error}", partial.display()), 0);
        }
        eprintln!("{LABEL}: {call} model {}", self.client.model);
        let control = self.control.clone();
        let stop = move || cancellable && cancel_pending(&control);
        let (reply, used) = match self.client.send(&body, &partial, &stop) {
            Ok(Sent::Done(status, text, retry_after_ms)) => {
                openai::interpret(status, &text, retry_after_ms)
            }
            Ok(Sent::Stopped) => {
                eprintln!("{LABEL}: {call} model stream stopped by a cancel");
                (openai::interrupted(), openai::Usage::default())
            }
            Err(error) => (openai::transient(&error, 0), openai::Usage::default()),
        };
        if let Err(error) = std::fs::remove_file(&partial) {
            eprintln!("{LABEL}: {}: {error}", partial.display());
        }
        let outcome = match &reply {
            Value::Object(entries) if entries.len() == 1 => entries[0].0.clone(),
            _ => "reply".to_owned(),
        };
        if outcome != "ok" {
            eprintln!("{LABEL}: model call {outcome}: {}", reply.encode());
        }
        self.record_model(used, &outcome);
        reply
    }

    fn run_tool(
        &mut self,
        turn: u64,
        seq: u64,
        tool: &str,
        input: &Value,
        cancellable: bool,
    ) -> tools::Ran {
        let receipted = RECEIPTED_TOOLS.contains(&tool);
        if receipted && let Err(error) = self.workspace.dispatching(turn, seq) {
            return tools::Ran::Failed(format!("receipt: {error}"));
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
            .get("cmd")
            .or(input.get("chars"))
            .and_then(Value::as_str)
            .unwrap_or("");
        eprintln!(
            "{LABEL}: {} {tool}: {shown}",
            harnesslang::run::journal::call_id(turn, seq)
        );
        let control = self.control.clone();
        let stop = move || cancellable && cancel_pending(&control);
        let ran = match tool {
            "exec_command" => self.exec.exec_command(input, &stop),
            "write_stdin" => self.exec.write_stdin(input, &stop),
            "apply_patch" => codex::apply_patch(&self.workspace.root, input),
            "update_plan" => codex::update_plan(input),
            "view_image" => codex::view_image(&self.workspace.root, input),
            other => tools::Ran::Failed(format!("unsupported call: {other}")),
        };
        if receipted {
            self.workspace.dispatched(turn, seq, &ran);
        }
        ran
    }

    fn deliveries(&self) -> Vec<Json> {
        let Ok(entries) = std::fs::read_dir(&self.control) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(CANCEL_PREFIX) || name.starts_with(STEER_PREFIX))
            .collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|name| {
                let delivery = if name.starts_with(CANCEL_PREFIX) {
                    Json::Object(vec![(
                        "cancel".to_owned(),
                        Json::Object(vec![("id".to_owned(), Json::Text(name))]),
                    )])
                } else {
                    let message = std::fs::read_to_string(self.control.join(&name)).ok()?;
                    Json::Object(vec![(
                        "steer".to_owned(),
                        Json::Object(vec![
                            ("id".to_owned(), Json::Text(name)),
                            ("message".to_owned(), Json::Text(message)),
                        ]),
                    )])
                };
                Some(delivery)
            })
            .collect()
    }
}

fn environment_context(cwd: &Path) -> String {
    let shell = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .and_then(|shell| {
            shell
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "bash".to_owned());
    let date = crate::command_line("date", &["+%Y-%m-%d"]).unwrap_or_default();
    let timezone = crate::command_line("date", &["+%Z"]).unwrap_or_default();
    format!(
        "<environment_context>\n  <cwd>{}</cwd>\n  <shell>{shell}</shell>\n  <current_date>{date}</current_date>\n  <timezone>{timezone}</timezone>\n</environment_context>",
        cwd.display()
    )
}

fn preamble(cwd: &Path, state: &Path) -> Result<Vec<Value>, String> {
    let saved = state.join(PREAMBLE_FILE);
    if let Ok(text) = std::fs::read_to_string(&saved) {
        return match Value::decode(&text) {
            Ok(Value::Array(items)) => Ok(items),
            _ => Err(format!("{}: not a list of input items", saved.display())),
        };
    }
    let mut items = Vec::new();
    if let Ok(text) = std::fs::read_to_string(cwd.join("AGENTS.md")) {
        items.push(openai::user_message(&format!(
            "# AGENTS.md instructions for {}\n\n<INSTRUCTIONS>\n{}\n</INSTRUCTIONS>",
            cwd.display(),
            text.trim_end()
        )));
    }
    items.push(openai::user_message(&environment_context(cwd)));
    let temporary = state.join(format!("{PREAMBLE_FILE}.new"));
    std::fs::write(&temporary, Value::Array(items.clone()).encode())
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, &saved).map_err(|error| format!("{}: {error}", saved.display()))?;
    Ok(items)
}

fn cache_key(state: &Path) -> Result<String, String> {
    let path = state.join(CACHE_KEY_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        return Ok(text.trim().to_owned());
    }
    let (high, low) = (random(), random().rotate_left(u32::BITS));
    let key = format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        high >> 32,
        (high >> 16) & 0xffff,
        high & 0xfff,
        (low >> 48) & 0x3fff | 0x8000,
        low & 0xffff_ffff_ffff
    );
    let temporary = state.join(format!("{CACHE_KEY_FILE}.new"));
    std::fs::write(&temporary, &key)
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, &path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(key)
}

fn setup(options: &Options) -> Result<(CodexAgent, String), String> {
    let instruction = std::fs::read_to_string(&options.instruction)
        .map_err(|error| format!("{}: {error}", options.instruction.display()))?;
    let cwd = std::fs::canonicalize(&options.cwd)
        .map_err(|error| format!("{}: {error}", options.cwd.display()))?;
    let key =
        std::env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY is not set".to_owned())?;
    let base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| openai::BASE_URL_DEFAULT.to_owned());
    let base_url = base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_owned();
    std::fs::create_dir_all(&options.journal)
        .map_err(|error| format!("{}: {error}", options.journal.display()))?;
    let journal = std::fs::canonicalize(&options.journal)
        .map_err(|error| format!("{}: {error}", options.journal.display()))?;
    let state = journal.join("state");
    std::fs::create_dir_all(&state).map_err(|error| format!("{}: {error}", state.display()))?;
    let control = control_dir(&journal);
    std::fs::create_dir_all(&control).map_err(|error| format!("{}: {error}", control.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal.join("usage.jsonl"))
        .map_err(|error| format!("usage.jsonl: {error}"))?;
    let tools =
        Value::decode(TOOL_DEFINITIONS).expect("the bundled Codex tool definitions are JSON");
    let client = openai::Client {
        base_url,
        key,
        model: options.model.clone(),
        instructions: INSTRUCTIONS.to_owned(),
        preamble: preamble(&cwd, &state)?,
        tools,
        custom_tools: CUSTOM_TOOLS.iter().map(|tool| (*tool).to_owned()).collect(),
        cache_key: cache_key(&state)?,
        scratch: state.clone(),
    };
    let workspace = tools::Workspace {
        root: cwd.clone(),
        state: state.clone(),
        image_bytes_max: None,
    };
    let reviewer = options.reviewer_url.as_deref().map(|url| Reviewer {
        relay: PathBuf::from(url),
        instruction: instruction.clone(),
    });
    Ok((
        CodexAgent {
            client,
            workspace,
            exec: codex::Exec::new(cwd, state),
            ledger: Ledger { file },
            control,
            reviewer,
            last_context: Value::Array(Vec::new()),
        },
        instruction,
    ))
}

struct Hosted<'a> {
    agent: &'a mut CodexAgent,
    finish: Option<Json>,
}

fn number(json: &Json, what: &str) -> Result<u64, String> {
    json.bigint()
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| format!("{what} is not a number: {}", json.encode()))
}

fn text(json: &Json) -> Option<&str> {
    match json {
        Json::Text(text) => Some(text),
        _ => None,
    }
}

impl World for Hosted<'_> {
    fn tool(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a tool request's turn")?;
        let seq = number(request.get("seq"), "a tool request's seq")?;
        let tool = text(request.get("tool"))
            .ok_or("a tool request names its tool")?
            .to_owned();
        let input = host::to_value(request.get("input"));
        let cancellable = CANCELLABLE_TOOLS.contains(&tool.as_str());
        Ok(host::output_json(self.agent.run_tool(
            turn,
            seq,
            &tool,
            &input,
            cancellable,
        )))
    }

    fn model(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a model request's turn")?;
        let seq = number(request.get("seq"), "a model request's seq")?;
        let context = host::to_value(request.get("context"));
        if self.agent.reviewer.is_some() {
            self.agent.last_context = context.clone();
        }
        let effort = text(request.get("settings").get(openai::EFFORT_SETTING)).map(str::to_owned);
        let mut blobs = Vec::new();
        if let Json::Object(held) = request.get("blobs") {
            for (digest, data) in held {
                let data = text(data).ok_or("a model request's blob is base64 text")?;
                let bytes = harnesslang::run::blob::base64_decode(data)?;
                blobs.push((
                    digest.clone(),
                    tools::media_type(&bytes).unwrap_or_default().to_owned(),
                    data.to_owned(),
                ));
            }
        }
        let attached = |digest: &str| {
            blobs
                .iter()
                .find(|(held, _, _)| held == digest)
                .map(|(_, media, data)| (media.clone(), data.clone()))
        };
        let reply = self
            .agent
            .run_model(turn, seq, effort.as_deref(), &context, true, &attached);
        Ok(host::to_json(&reply))
    }

    fn now(&mut self) -> Result<u64, String> {
        Ok(now_ms())
    }

    fn random(&mut self) -> Result<u64, String> {
        Ok(random())
    }

    fn world(&mut self, request: &Json) -> Result<Json, String> {
        let event = text(request.get("event")).unwrap_or_default().to_owned();
        match event.as_str() {
            "inbox" => Ok(Json::Object(vec![(
                "answer".to_owned(),
                Json::Array(self.agent.deliveries()),
            )])),
            "answer" => {
                let Json::Array(answers) = request.get("answers") else {
                    return Err("an answer request lists its answers".to_owned());
                };
                for answer in answers {
                    let id = text(answer.get("id")).ok_or("an answer names its event")?;
                    let name = text(answer.get("answer")).unwrap_or_default();
                    if name == "full" {
                        continue;
                    }
                    eprintln!("{LABEL}: event {id}: {name}");
                    let _ = std::fs::remove_file(self.agent.control.join(id));
                }
                Ok(Json::Object(Vec::new()))
            }
            "recover" => self.recover(request),
            "clock" => Ok(Json::Object(vec![(
                "answer".to_owned(),
                Json::int(now_ms()),
            )])),
            "sleep" => {
                let until = number(request.get("until"), "a sleep's until")?;
                let now = now_ms();
                if until > now {
                    std::thread::sleep(std::time::Duration::from_millis(until - now));
                }
                Ok(Json::Object(vec![
                    ("answer".to_owned(), Json::text("ok")),
                    ("clock".to_owned(), Json::int(now_ms().max(until))),
                ]))
            }
            "jobs" => Ok(Json::Object(vec![(
                "answer".to_owned(),
                Json::Array(Vec::new()),
            )])),
            "listing" => Ok(Json::Object(vec![(
                "output".to_owned(),
                Json::Object(vec![("ok".to_owned(), Json::Object(Vec::new()))]),
            )])),
            "resumed" => Ok(Json::Object(Vec::new())),
            other => Err(format!(
                "{LABEL} answers no {other} request: {}",
                request.encode()
            )),
        }
    }

    fn acknowledge(&mut self, finish: &Json) -> Result<(), String> {
        self.finish = Some(finish.clone());
        Ok(())
    }
}

impl Hosted<'_> {
    fn recover(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a recovery's turn")?;
        let seq = number(request.get("seq"), "a recovery's seq")?;
        let tool = text(request.get("tool"))
            .ok_or("a recovery names its tool")?
            .to_owned();
        let input = host::to_value(request.get("input"));
        let recovered = match codex::patch_applied(&self.agent.workspace.root, &input) {
            Some(ran) if tool == "apply_patch" => tools::Recovered::Reply(ran),
            _ => self.agent.workspace.recover(turn, seq, &tool, &input),
        };
        let name = match &recovered {
            tools::Recovered::Reply(_) => "reply",
            tools::Recovered::NotDispatched => "not_dispatched",
            tools::Recovered::Unknown => "unknown",
        };
        let call = harnesslang::run::journal::call_id(turn, seq);
        eprintln!("{LABEL}: {call} recover {tool}: {name}");
        self.agent.ledger.append(Value::object([
            ("kind", Value::text("recover")),
            ("at_ms", Value::number(now_ms())),
            ("tool", Value::text(&tool)),
            ("call", Value::text(&call)),
            ("answer", Value::text(name)),
        ]));
        Ok(Json::Object(vec![(
            "answer".to_owned(),
            match recovered {
                tools::Recovered::Reply(ran) => {
                    Json::Object(vec![("reply".to_owned(), host::output_json(ran))])
                }
                tools::Recovered::NotDispatched | tools::Recovered::Unknown => Json::text(name),
            },
        )]))
    }
}

enum Ended {
    Finished(host::Finish),
    Failed(String),
}

fn run_turn(
    loaded: &Loaded,
    agent: &mut CodexAgent,
    journal: &mut Journal,
    handler: &str,
    turn: u64,
    arguments: Value,
) -> Ended {
    let request = host::request(handler, turn, arguments);
    let mut hosted = Hosted {
        agent,
        finish: None,
    };
    let exit = match native::run(loaded, journal, &request, &mut hosted) {
        Ok(exit) => exit,
        Err(error) => return Ended::Failed(error),
    };
    let finish = match text(exit.get("exit")) {
        Some("finished") => match hosted.finish.take() {
            Some(finish) => finish,
            None => return Ended::Failed("a finished turn is acknowledged".to_owned()),
        },
        Some("already_finished") => exit.get("finish").clone(),
        _ => return Ended::Failed(format!("the module exited {}", exit.encode())),
    };
    match (
        number(finish.get("turn"), "a finish's turn"),
        finish.get("outcome").is_text("completed"),
    ) {
        (Ok(turn), completed) => Ended::Finished(host::Finish {
            turn,
            completed,
            result: host::to_value(finish.get("result")),
        }),
        (Err(error), _) => Ended::Failed(error),
    }
}

fn ended_completed(ended: &Ended) -> bool {
    match ended {
        Ended::Finished(finish) if finish.completed => outcome_of(&finish.result).0 == "completed",
        _ => false,
    }
}

fn prompt(input: &str, rounds: u64) -> Value {
    Value::object([
        ("input", Value::text(input)),
        (
            "rounds",
            Value::number(rounds.min(codex_harness::ROUNDS_MAX)),
        ),
    ])
}

pub fn main(options: &Options) -> ! {
    let loaded = host::load(codex_harness::MODULE);
    let (mut agent, instruction) = setup(options).unwrap_or_else(|error| {
        eprintln!("{LABEL}: {error}");
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    agent.ledger.append(Value::object([
        ("kind", Value::text("start")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let journal_path = options.journal.join("journal.jsonl");
    let mut disk = Journal::open(&journal_path).unwrap_or_else(|error| {
        eprintln!("{LABEL}: {}: {error}", journal_path.display());
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    let mut turn = options.turn;
    let effort_text = options
        .effort_text
        .as_deref()
        .or_else(|| openai::default_effort(&options.model));
    if let Some(effort) = effort_text {
        if !EFFORT_LEVELS.contains(&effort) {
            eprintln!("{LABEL}: --effort {effort} is not medium, low, high or xhigh");
            std::process::exit(crate::EXIT_USAGE);
        }
        let level = Value::object([("level", Value::text(effort))]);
        match run_turn(&loaded, &mut agent, &mut disk, SET_EFFORT, turn, level) {
            Ended::Finished(finish) if finish.completed => {}
            Ended::Finished(finish) => {
                eprintln!(
                    "{LABEL}: --effort: the set_effort turn trapped: {}",
                    finish.result.encode()
                );
                std::process::exit(EXIT_HARNESS_ERROR);
            }
            Ended::Failed(error) => {
                eprintln!("{LABEL}: --effort: the set_effort turn ended {error}");
                std::process::exit(EXIT_HARNESS_ERROR);
            }
        }
        turn += 1;
    }
    let finish_checks = options.finish_checks.unwrap_or(FINISH_CHECKS_MAX);
    if finish_checks > FINISH_CHECKS_MAX {
        eprintln!("{LABEL}: --finish-checks {finish_checks} is above {FINISH_CHECKS_MAX}");
        std::process::exit(crate::EXIT_USAGE);
    }
    let mut input = instruction;
    let mut checks = 0;
    let ran = loop {
        let arguments = prompt(&input, options.rounds);
        let ran = run_turn(&loaded, &mut agent, &mut disk, PROMPT, turn, arguments);
        if checks >= finish_checks || !ended_completed(&ran) {
            break ran;
        }
        checks += 1;
        turn += 1;
        input = FINISH_CHECK.to_owned();
    };
    assert!(checks <= finish_checks);
    let mut ran = ran;
    let mut reviews = 0;
    while reviews < reviewer::REVIEWS_MAX && ended_completed(&ran) {
        let Some(review) = agent.review(reviews) else {
            break;
        };
        reviews += 1;
        turn += 1;
        let arguments = prompt(&format!("{REVIEWED}\n\n{review}"), options.rounds);
        ran = run_turn(&loaded, &mut agent, &mut disk, PROMPT, turn, arguments);
    }
    assert!(reviews <= reviewer::REVIEWS_MAX);
    for (session, pid) in agent.exec.leave_running() {
        eprintln!("{LABEL}: exec session {session} (pid {pid}) left running after the task");
    }
    agent.ledger.append(Value::object([
        ("kind", Value::text("end")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let (finish, code) = match &ran {
        Ended::Finished(finish) if finish.completed => {
            let (name, text) = outcome_of(&finish.result);
            println!("{text}");
            (name, 0)
        }
        Ended::Finished(finish) => {
            eprintln!("{LABEL}: the harness trapped: {}", finish.result.encode());
            ("trap".to_owned(), EXIT_HARNESS_ERROR)
        }
        Ended::Failed(error) => {
            eprintln!("{LABEL}: {error}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
    };
    if let Err(error) = write_usage(&options.journal, &options.model, &finish) {
        eprintln!("{LABEL}: {error}");
        std::process::exit(EXIT_HARNESS_ERROR);
    }
    std::process::exit(code)
}
