use std::path::{Path, PathBuf};

use crate::codex;
use crate::codex_harness::program::{self, Arguments};
use crate::codex_harness::{self, Exit, FileDisk, Json, Ran, RunError, TurnRequest};
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
const FINISH_CHECK: &str = "Before this task is done, check it by running commands, not by reading your own code. Go through each behaviour the task states, one by one: each file exists at the exact path it names with the content or format it asks for, each program runs with the exact command and interpreter it names, each service responds through the protocol, port and account the task names, and each output matches the stated format. Run every check now. If any check fails or a stated behaviour is not yet verified, fix it and continue working. End your turn again only after the work passes, with a short report listing each check you ran and its result.";

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
    ) -> Value {
        let call = codex_harness::call_id(turn, seq);
        let body = match self.client.body(context, effort) {
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
            ("call", Value::text(&codex_harness::call_id(turn, seq))),
        ]));
        let shown = input
            .get("cmd")
            .or(input.get("chars"))
            .and_then(Value::as_str)
            .unwrap_or("");
        eprintln!(
            "{LABEL}: {} {tool}: {shown}",
            codex_harness::call_id(turn, seq)
        );
        let control = self.control.clone();
        let stop = move || cancellable && cancel_pending(&control);
        let ran = match tool {
            "exec_command" => self.exec.exec_command(input, &stop),
            "write_stdin" => self.exec.write_stdin(input, &stop),
            "apply_patch" => codex::apply_patch(&self.workspace.root, input),
            "update_plan" => codex::update_plan(input),
            other => tools::Ran::Failed(format!("unsupported call: {other}")),
        };
        if receipted {
            self.workspace.dispatched(turn, seq, &ran);
        }
        ran
    }

    fn deliveries(&self) -> Vec<codex_harness::Delivery> {
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
                let inbound = if name.starts_with(CANCEL_PREFIX) {
                    codex_harness::Inbound::Cancel
                } else {
                    codex_harness::Inbound::Steer(
                        std::fs::read_to_string(self.control.join(&name)).ok()?,
                    )
                };
                Some(codex_harness::Delivery { id: name, inbound })
            })
            .collect()
    }
}

fn output_of(ran: tools::Ran) -> codex_harness::Output {
    match ran {
        tools::Ran::Ok(text) | tools::Ran::Job(text) => {
            codex_harness::Output::Ok(Json::String(text))
        }
        tools::Ran::Image(_) => {
            codex_harness::Output::Failed("this host returns no images".to_owned())
        }
        tools::Ran::Failed(text) => codex_harness::Output::Failed(text),
        tools::Ran::Units(units) => codex_harness::Output::Ok(Json::Number(units)),
    }
}

impl codex_harness::Host for CodexAgent {
    fn tool(&mut self, request: &codex_harness::ToolRequest<'_>) -> codex_harness::Output {
        let input = Value::decode(&request.input.encode()).expect("the harness encodes valid JSON");
        let ran = self.run_tool(
            request.turn,
            request.seq,
            request.tool,
            &input,
            request.interrupts.cancel,
        );
        output_of(ran)
    }

    fn recover(&mut self, request: &codex_harness::ToolRequest<'_>) -> codex_harness::Recovered {
        let input = Value::decode(&request.input.encode()).expect("the harness encodes valid JSON");
        let recovered = match codex::patch_applied(&self.workspace.root, &input) {
            Some(ran) if request.tool == "apply_patch" => tools::Recovered::Reply(ran),
            _ => self
                .workspace
                .recover(request.turn, request.seq, request.tool, &input),
        };
        let answer = match &recovered {
            tools::Recovered::Reply(_) => "reply",
            tools::Recovered::NotDispatched => "not_dispatched",
            tools::Recovered::Unknown => "unknown",
        };
        let call = codex_harness::call_id(request.turn, request.seq);
        eprintln!("{LABEL}: {call} recover {}: {answer}", request.tool);
        self.ledger.append(Value::object([
            ("kind", Value::text("recover")),
            ("at_ms", Value::number(now_ms())),
            ("tool", Value::text(request.tool)),
            ("call", Value::text(&call)),
            ("answer", Value::text(answer)),
        ]));
        match recovered {
            tools::Recovered::Reply(ran) => codex_harness::Recovered::Reply(output_of(ran)),
            tools::Recovered::NotDispatched => codex_harness::Recovered::NotDispatched,
            tools::Recovered::Unknown => codex_harness::Recovered::Unknown,
        }
    }

    fn inbox(&mut self, _turn: u64) -> Vec<codex_harness::Delivery> {
        self.deliveries()
    }

    fn answer(
        &mut self,
        _turn: u64,
        _position: u64,
        answers: &[(String, codex_harness::EventAnswer)],
    ) {
        for (id, answer) in answers {
            if *answer == codex_harness::EventAnswer::Full {
                continue;
            }
            eprintln!("{LABEL}: event {id}: {answer:?}");
            let _ = std::fs::remove_file(self.control.join(id));
        }
    }

    fn listing(&mut self, _request: &codex_harness::ListingRequest<'_>) -> codex_harness::Output {
        codex_harness::Output::Ok(Json::Object(std::collections::BTreeMap::new()))
    }

    fn model(&mut self, request: &codex_harness::ModelRequest<'_>) -> Json {
        let context =
            Value::decode(&(request.context)().encode()).expect("the harness encodes valid JSON");
        if self.reviewer.is_some() {
            self.last_context = context.clone();
        }
        let effort = request
            .settings
            .and_then(|settings| settings.get(openai::EFFORT_SETTING))
            .and_then(|effort| effort.as_str());
        let reply = self.run_model(
            request.turn,
            request.seq,
            effort,
            &context,
            request.interrupts.cancel,
        );
        Json::decode(&reply.encode()).expect("a reply the host built decodes as harness JSON")
    }

    fn now(&mut self) -> u64 {
        now_ms()
    }

    fn random(&mut self) -> u64 {
        random()
    }

    fn acknowledge(&mut self, _finish: &codex_harness::Finish) {}
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
    let key = format!(
        "hl-{:016x}{:016x}",
        random(),
        random().rotate_left(u32::BITS)
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

fn ended_completed(ran: &Result<Ran, RunError>) -> bool {
    let Ok(Ran::Finished(finish) | Ran::AlreadyFinished(finish)) = ran else {
        return false;
    };
    if finish.outcome != codex_harness::Outcome::Completed {
        return false;
    }
    let result = Value::decode(&finish.result.encode()).expect("the harness encodes valid JSON");
    outcome_of(&result).0 == "completed"
}

fn effort_of(text: &str) -> Option<program::SumEffortLevel> {
    match text {
        "medium" => Some(program::SumEffortLevel::Medium),
        "low" => Some(program::SumEffortLevel::Low),
        "high" => Some(program::SumEffortLevel::High),
        "xhigh" => Some(program::SumEffortLevel::Xhigh),
        _ => None,
    }
}

pub fn main(options: &Options) -> ! {
    codex_harness::self_check();
    codex_harness::jsonl_self_check();
    let (mut agent, instruction) = setup(options).unwrap_or_else(|error| {
        eprintln!("{LABEL}: {error}");
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    agent.ledger.append(Value::object([
        ("kind", Value::text("start")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let journal_path = options.journal.join("journal.jsonl");
    let mut disk = FileDisk::open(&journal_path).unwrap_or_else(|error| {
        eprintln!("{LABEL}: {}: {error}", journal_path.display());
        std::process::exit(EXIT_HARNESS_ERROR)
    });
    let mut turn = options.turn;
    if let Some(effort) = &options.effort_text {
        let Some(level) = effort_of(effort) else {
            eprintln!("{LABEL}: --effort {effort} is not medium, low, high or xhigh");
            std::process::exit(crate::EXIT_USAGE);
        };
        let setting = TurnRequest {
            turn,
            arguments: Arguments::SetEffort(level),
        };
        match codex_harness::run_jsonl(&mut agent, &mut disk, &setting) {
            Ok(Ran::Finished(finish) | Ran::AlreadyFinished(finish))
                if finish.outcome == codex_harness::Outcome::Completed => {}
            other => {
                eprintln!("{LABEL}: --effort: the set_effort turn ended {other:?}");
                std::process::exit(EXIT_HARNESS_ERROR);
            }
        }
        turn += 1;
    }
    let mut input = instruction;
    let mut checks = 0;
    let ran = loop {
        let request = TurnRequest {
            turn,
            arguments: Arguments::Prompt(input, options.rounds.min(program::ROUNDS_MAX)),
        };
        let ran = codex_harness::run_jsonl(&mut agent, &mut disk, &request);
        if checks >= FINISH_CHECKS_MAX || !ended_completed(&ran) {
            break ran;
        }
        checks += 1;
        turn += 1;
        input = FINISH_CHECK.to_owned();
    };
    assert!(checks <= FINISH_CHECKS_MAX);
    let mut ran = ran;
    let mut reviews = 0;
    while reviews < reviewer::REVIEWS_MAX && ended_completed(&ran) {
        let Some(review) = agent.review(reviews) else {
            break;
        };
        reviews += 1;
        turn += 1;
        let request = TurnRequest {
            turn,
            arguments: Arguments::Prompt(
                format!("{REVIEWED}\n\n{review}"),
                options.rounds.min(program::ROUNDS_MAX),
            ),
        };
        ran = codex_harness::run_jsonl(&mut agent, &mut disk, &request);
    }
    assert!(reviews <= reviewer::REVIEWS_MAX);
    agent.exec.stop_all();
    agent.ledger.append(Value::object([
        ("kind", Value::text("end")),
        ("at_ms", Value::number(now_ms())),
    ]));
    let (finish, code) = match &ran {
        Ok(Ran::Finished(finish) | Ran::AlreadyFinished(finish))
            if finish.outcome == codex_harness::Outcome::Completed =>
        {
            let (name, text) = outcome_of(
                &Value::decode(&finish.result.encode()).expect("the harness encodes valid JSON"),
            );
            println!("{text}");
            (name, 0)
        }
        Ok(Ran::Finished(finish) | Ran::AlreadyFinished(finish)) => {
            eprintln!("{LABEL}: the harness trapped: {}", finish.result.encode());
            ("trap".to_owned(), EXIT_HARNESS_ERROR)
        }
        Ok(other) => {
            eprintln!("{LABEL}: the prompt turn ended {other:?}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
        Err(RunError::Open(error)) => {
            eprintln!("{LABEL}: journal: {error}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
        Err(RunError::Exit(Exit::Corrupt(error))) => {
            eprintln!("{LABEL}: journal: {error}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
        Err(RunError::Exit(Exit::Stop(stop))) => {
            eprintln!("{LABEL}: stopped: {stop:?}");
            ("error".to_owned(), EXIT_HARNESS_ERROR)
        }
    };
    if let Err(error) = write_usage(&options.journal, &options.model, &finish) {
        eprintln!("{LABEL}: {error}");
        std::process::exit(EXIT_HARNESS_ERROR);
    }
    std::process::exit(code)
}
