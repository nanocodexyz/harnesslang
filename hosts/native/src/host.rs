use std::path::{Path, PathBuf};

use harnesslang::run::journal::Record;

use crate::store;
use harnesslang::wasm::js::{Json, parse};
use harnesslang::wasm::native::{self, World};
use harnesslang::wasm::sim::Loaded;

use crate::children::{self, Children};
use crate::control;
use crate::json::Value;
use crate::ledger::{self, Ledger};
use crate::texts::Texts;
use crate::{codex, jobs, now_ms, random, tools, wire};

pub const JOURNAL_FILE: &str = "journal.jsonl";
pub const STATE_DIRECTORY: &str = "state";
pub const CONFIGURE_HANDLER: &str = "configure";
pub const CONFIGURE_FILE: &str = "configure.json";
pub const MODULE_EXTENSION: &str = "wasm";
pub const JOINS_PER_TURN_MAX: u64 = 4096;
const FIRST_TURN: u64 = 1;
const SUSPEND_TOOL: &str = "suspend";
const SUSPEND_KEY: &str = "key";
const RECEIPTED_LIST: &str = "receipted_tools";
const CANCELLABLE_LIST: &str = "cancellable_tools";
const CHILDREN_LIST: &str = "list_tools";
const SUBMIT_LIST: &str = "submit_tools";
const ANSWER_FULL: &str = "full";

#[derive(Clone)]
pub struct Spawn {
    pub tool: String,
    pub program: String,
    pub handler: String,
}

#[derive(Clone)]
pub struct Setup {
    pub texts: Texts,
    pub module_dir: Option<PathBuf>,
    pub cwd: PathBuf,
    pub spawns: Vec<Spawn>,
}

pub struct Finish {
    pub turn: u64,
    pub completed: bool,
    pub result: Value,
}

pub enum Ran {
    Finished(Finish),
    Suspended { turn: u64, seq: u64 },
    NotDue,
}

pub struct Session {
    pub setup: Setup,
    pub directory: PathBuf,
    pub label: String,
    pub workspace: tools::Workspace,
    pub exec: codex::Exec,
    pub jobs: jobs::Jobs,
    pub ledger: Ledger,
    pub children: Children,
    pub control: PathBuf,
    pub cancel: PathBuf,
    pub store: store::Spec,
    finish: Option<Json>,
}

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

pub fn module_path(module_dir: &Path, program: &str) -> PathBuf {
    module_dir.join(format!("{program}.{MODULE_EXTENSION}"))
}

pub fn load_program(module_dir: Option<&Path>, program: &str) -> Result<Loaded, String> {
    if let Some(module_dir) = module_dir {
        return load(&module_path(module_dir, program));
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| module_path(dir, program)))
        .filter(|path| path.exists());
    if let Some(path) = beside {
        return load(&path);
    }
    let (_, bytes) = EMBEDDED
        .iter()
        .find(|(name, _)| *name == program)
        .ok_or_else(|| format!("no module {program}.{MODULE_EXTENSION}: give --module-dir"))?;
    harnesslang::wasm::sim::load(bytes).map_err(|error| format!("{program}: {error}"))
}

pub fn load(path: &Path) -> Result<Loaded, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    harnesslang::wasm::sim::load(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

pub fn to_value(json: &Json) -> Value {
    Value::decode(&json.encode()).expect("module JSON")
}

pub fn to_json(value: &Value) -> Json {
    parse(&value.encode()).expect("host JSON")
}

pub fn request(handler: &str, turn: u64, arguments: Json) -> Json {
    object(vec![
        ("arguments", arguments),
        ("handler", Json::text(handler)),
        ("turn", Json::int(turn)),
    ])
}

pub fn object(fields: Vec<(&str, Json)>) -> Json {
    let mut fields: Vec<(String, Json)> = fields
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    fields.sort_by(|left, right| left.0.cmp(&right.0));
    Json::Object(fields)
}

fn number(json: &Json, what: &str) -> Result<u64, String> {
    json.bigint()
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| format!("{what}: not numeric: {}", json.encode()))
}

fn text(json: &Json) -> Option<&str> {
    match json {
        Json::Text(text) => Some(text),
        _ => None,
    }
}

pub fn output_json(ran: tools::Ran) -> Json {
    match ran {
        tools::Ran::Ok(text) => object(vec![("ok", Json::Text(text))]),
        tools::Ran::Image(bytes) => object(vec![(
            "ok",
            object(vec![(
                "data",
                Json::Text(harnesslang::run::blob::base64_encode(&bytes)),
            )]),
        )]),
        tools::Ran::Failed(text) => object(vec![("failed", Json::Text(text))]),
        tools::Ran::Job(text) => object(vec![("ok", object(vec![("job", Json::Text(text))]))]),
    }
}

pub fn submitted(texts: &Texts, input: &Value) -> tools::Ran {
    match input.get("output").and_then(Value::as_str) {
        Some(output) => tools::Ran::Ok(output.to_owned()),
        None => tools::Ran::Failed(texts.fill("submit_no_output", &[])),
    }
}

impl Session {
    pub fn open(
        setup: Setup,
        directory: &Path,
        label: &str,
        cancel: &Path,
    ) -> Result<Self, String> {
        let state = directory.join(STATE_DIRECTORY);
        std::fs::create_dir_all(&state).map_err(|error| format!("{}: {error}", state.display()))?;
        let ledger = Ledger::open(&directory.join(ledger::LEDGER_FILE))?;
        Ok(Session {
            workspace: tools::Workspace {
                texts: setup.texts.clone(),
                root: setup.cwd.clone(),
                state: state.clone(),
                image_bytes_max: Some(tools::IMAGE_BYTES_MAX),
            },
            exec: codex::Exec::new(setup.texts.clone(), setup.cwd.clone(), state),
            jobs: jobs::Jobs::new(setup.texts.clone()),
            ledger,
            children: Children::new(directory, setup.clone(), cancel),
            control: control::directory(directory),
            cancel: cancel.to_path_buf(),
            directory: directory.to_path_buf(),
            label: label.to_owned(),
            setup,
            store: store::Spec::Jsonl,
            finish: None,
        })
    }

    pub fn journal_path(&self) -> PathBuf {
        self.directory.join(JOURNAL_FILE)
    }

    pub fn records(&self) -> Result<Vec<Record>, String> {
        store::records(&self.store, &self.journal_path())
    }

    pub fn configured(&self) -> Option<Json> {
        let text = std::fs::read_to_string(self.directory.join(CONFIGURE_FILE)).ok()?;
        parse(&text).ok()
    }

    pub fn remember_configure(&self, arguments: &Json) -> Result<(), String> {
        let path = self.directory.join(CONFIGURE_FILE);
        children::write_durably(&path, &arguments.encode())
    }

    pub fn close(&mut self) {
        self.children.finish();
        self.jobs.stop_all();
        for (session, pid) in self.exec.leave_running() {
            eprintln!(
                "{}: exec session {session} (process group {pid}) is left running",
                self.label
            );
        }
    }

    fn stop(&self, cancellable: bool) -> impl Fn() -> bool + use<> {
        let cancel = self.cancel.clone();
        move || cancellable && control::cancel_pending(&cancel)
    }

    fn spawn_of(&self, request: &Json, tool: &str) -> Option<(String, String)> {
        let declared = request.get("spawn");
        if let (Some(program), Some(handler)) =
            (text(declared.get("program")), text(declared.get("handler")))
        {
            return Some((program.to_owned(), handler.to_owned()));
        }
        self.setup
            .spawns
            .iter()
            .find(|spawn| spawn.tool == tool)
            .map(|spawn| (spawn.program.clone(), spawn.handler.clone()))
    }

    fn run_tool(&mut self, request: &Json) -> Result<tools::Ran, String> {
        let turn = number(request.get("turn"), "a tool request's turn")?;
        let seq = number(request.get("seq"), "a tool request's seq")?;
        let tool = text(request.get("tool"))
            .ok_or("tool request unnamed")?
            .to_owned();
        let input = to_value(request.get("input"));
        let room = matches!(request.get("job"), Json::Bool(true));
        let kind = |list: &str| self.setup.texts.list(list).contains(&tool);
        let receipted = kind(RECEIPTED_LIST);
        let cancellable = kind(CANCELLABLE_LIST);
        let listing = kind(CHILDREN_LIST);
        let submitting = kind(SUBMIT_LIST);
        if receipted && let Err(error) = self.workspace.dispatching(turn, seq) {
            return Ok(tools::Ran::Failed(format!("receipt: {error}")));
        }
        let call = harnesslang::run::journal::call_id(turn, seq);
        self.ledger.append(Value::object([
            ("kind", Value::text("tool")),
            ("at_ms", Value::number(now_ms())),
            ("tool", Value::text(&tool)),
            ("call", Value::text(&call)),
        ]));
        let shown = [
            "description",
            "file_path",
            "command",
            "cmd",
            "chars",
            "path",
        ]
        .iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .unwrap_or("");
        eprintln!("{}: {call} {tool}: {shown}", self.label);
        let stop = self.stop(cancellable);
        let configured = self.configured();
        let ran = match (tool.as_str(), self.spawn_of(request, &tool)) {
            (_, Some((program, handler))) => self
                .children
                .spawn(turn, seq, &program, &handler, &input, configured),
            (_, None) if listing => self.children.list(&input),
            (_, None) if submitting => submitted(&self.setup.texts, &input),
            ("Bash", None) if room => self.bash_job(turn, seq, &input),
            ("Bash" | "Read" | "Edit" | "Write" | "read_file" | "write_bytes", None) => {
                self.workspace.run(&tool, &input)
            }
            ("exec_command", None) => self.exec.exec_command(&input, &stop),
            ("write_stdin", None) => self.exec.write_stdin(&input, &stop),
            ("apply_patch", None) => {
                codex::apply_patch(&self.workspace.texts, &self.workspace.root, &input)
            }
            ("view_image", None) => {
                codex::view_image(&self.workspace.texts, &self.workspace.root, &input)
            }
            (other, None) => tools::Ran::Failed(format!("no syscall {other}")),
        };
        if receipted {
            self.workspace.dispatched(turn, seq, &ran);
        }
        Ok(ran)
    }

    fn bash_job(&mut self, turn: u64, seq: u64, input: &Value) -> tools::Ran {
        match self.workspace.bash_job(input, true) {
            tools::Bashed::Ran(ran) => ran,
            tools::Bashed::Job(started, message) => {
                let key = format!("{turn}.{seq}");
                let held = !input
                    .get("run_in_background")
                    .is_some_and(|value| matches!(value, Value::Bool(true)));
                let command = input.get("command").and_then(Value::as_str).unwrap_or("");
                self.jobs.start(&key, started, command, held);
                eprintln!("{}: {turn}/{seq} backgrounded: {key}", self.label);
                tools::Ran::Job(jobs::started_text(
                    &self.workspace.texts,
                    &key,
                    &message,
                    held,
                ))
            }
        }
    }

    fn hold(&mut self, until_ms: u64) -> Json {
        loop {
            self.jobs.poll();
            let now = now_ms();
            let steered = !control::deliveries(&self.control).is_empty();
            if self.jobs.pending() || !self.jobs.holding() || steered || now >= until_ms {
                return object(vec![
                    ("answer", Json::text("ok")),
                    ("clock", Json::int(now)),
                ]);
            }
            std::thread::sleep(jobs::JOB_POLL);
        }
    }

    fn inbox(&mut self) -> Json {
        let mut found: Vec<Json> = self
            .jobs
            .notices()
            .into_iter()
            .map(|notice| {
                object(vec![(
                    "job",
                    object(vec![
                        ("done", Json::Bool(notice.done)),
                        ("id", Json::Text(notice.id)),
                        ("key", Json::Text(notice.key)),
                        ("notice", Json::Text(notice.notice)),
                    ]),
                )])
            })
            .collect();
        found.extend(control::deliveries(&self.control));
        object(vec![("answer", Json::Array(found))])
    }

    fn answer(&mut self, request: &Json) -> Result<Json, String> {
        let Json::Array(answers) = request.get("answers") else {
            return Err("answers missing".to_owned());
        };
        for answer in answers {
            let id = text(answer.get("id")).ok_or("answer unnamed")?;
            let name = text(answer.get("answer")).unwrap_or_default();
            if control::is_delivery(id) {
                if name == ANSWER_FULL {
                    continue;
                }
                eprintln!("{}: event {id}: {name}", self.label);
                control::consume(&self.control, id);
                continue;
            }
            self.jobs
                .answered(id, matches!(name, "recorded" | "duplicate"));
        }
        Ok(Json::Object(Vec::new()))
    }

    fn recover(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a recovery's turn")?;
        let seq = number(request.get("seq"), "a recovery's seq")?;
        let tool = text(request.get("tool"))
            .ok_or("recovery unnamed")?
            .to_owned();
        let input = to_value(request.get("input"));
        let recovered =
            match codex::patch_applied(&self.workspace.texts, &self.workspace.root, &input) {
                Some(ran) if tool == "apply_patch" => tools::Recovered::Reply(ran),
                _ => self.workspace.recover(turn, seq, &tool, &input),
            };
        let name = match &recovered {
            tools::Recovered::Reply(_) => "reply",
            tools::Recovered::NotDispatched => "not_dispatched",
            tools::Recovered::Unknown => "unknown",
        };
        let call = harnesslang::run::journal::call_id(turn, seq);
        eprintln!("{}: {call} recover {tool}: {name}", self.label);
        self.ledger.append(Value::object([
            ("kind", Value::text("recover")),
            ("at_ms", Value::number(now_ms())),
            ("tool", Value::text(&tool)),
            ("call", Value::text(&call)),
            ("answer", Value::text(name)),
        ]));
        Ok(object(vec![(
            "answer",
            match recovered {
                tools::Recovered::Reply(ran) => object(vec![("reply", output_json(ran))]),
                tools::Recovered::NotDispatched | tools::Recovered::Unknown => Json::text(name),
            },
        )]))
    }

    fn suspend_key(&self, turn: u64, seq: u64) -> Result<String, String> {
        for record in self.records()? {
            if let Record::Call {
                turn: at,
                seq: site,
                tool,
                input,
                ..
            } = record
                && at == turn
                && site == seq
                && tool == SUSPEND_TOOL
            {
                return Ok(input
                    .get(SUSPEND_KEY)
                    .and_then(harnesslang::run::json::Json::as_str)
                    .unwrap_or_default()
                    .to_owned());
            }
        }
        Err(format!("resume {turn}/{seq}: no suspend"))
    }

    fn resume(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a resume's turn")?;
        let seq = number(request.get("seq"), "a resume's seq")?;
        let key = self.suspend_key(turn, seq)?;
        Ok(match self.children.join(&key) {
            Some(answer) => object(vec![
                ("answer", Json::Text(answer)),
                ("key", Json::Text(key)),
            ]),
            None => Json::Null,
        })
    }
}

impl World for Session {
    fn tool(&mut self, request: &Json) -> Result<Json, String> {
        self.run_tool(request).map(output_json)
    }

    fn model(&mut self, request: &Json) -> Result<Json, String> {
        let http = request.get("http");
        if !matches!(http, Json::Object(_)) {
            return Err(format!("non-wire request: {}", request.get("seq").encode()));
        }
        let call = format!(
            "{}/{}",
            request.get("turn").encode(),
            request.get("seq").encode()
        );
        eprintln!(
            "{}: {call} model {}",
            self.label,
            text(http.get("url")).unwrap_or_default()
        );
        let stop = self.stop(true);
        let answer = wire::call(http, &self.workspace.state, &stop);
        let outcome = match &answer {
            Json::Object(fields) if fields.iter().any(|(key, _)| key == "status") => {
                format!("status {}", answer.get("status").encode())
            }
            Json::Object(fields) => fields
                .first()
                .map(|(key, _)| key.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        if !outcome.starts_with("status 2") {
            eprintln!("{}: {call} model answered {outcome}", self.label);
        }
        self.ledger.append(Value::object([
            ("kind", Value::text("model")),
            ("at_ms", Value::number(now_ms())),
            ("call", Value::text(&call)),
            ("outcome", Value::text(&outcome)),
        ]));
        Ok(answer)
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
            "jobs" => {
                self.jobs.poll();
                let Json::Array(open) = request.get("open") else {
                    return Err("jobs missing".to_owned());
                };
                let lost = open
                    .iter()
                    .filter_map(text)
                    .filter(|key| !self.jobs.knows(key))
                    .map(Json::text)
                    .collect();
                Ok(object(vec![("answer", Json::Array(lost))]))
            }
            "hold" => {
                let until = number(request.get("until"), "a hold's until")?;
                Ok(self.hold(until))
            }
            "sleep" => {
                let until = number(request.get("until"), "a sleep's until")?;
                let now = now_ms();
                if until > now {
                    std::thread::sleep(std::time::Duration::from_millis(until - now));
                }
                Ok(object(vec![
                    ("answer", Json::text("ok")),
                    ("clock", Json::int(now_ms().max(until))),
                ]))
            }
            "clock" => Ok(object(vec![("answer", Json::int(now_ms()))])),
            "inbox" => Ok(self.inbox()),
            "answer" => self.answer(request),
            "listing" => Ok(object(vec![(
                "output",
                object(vec![("ok", Json::Object(Vec::new()))]),
            )])),
            "recover" => self.recover(request),
            "resume" => self.resume(request),
            "resumed" => Ok(Json::Object(Vec::new())),
            other => Err(format!(
                "hl-host answers no {other} request: {}",
                request.encode()
            )),
        }
    }

    fn acknowledge(&mut self, finish: &Json) -> Result<(), String> {
        self.finish = Some(finish.clone());
        Ok(())
    }
}

fn finish_of(json: &Json) -> Result<Finish, String> {
    Ok(Finish {
        turn: number(json.get("turn"), "a finish's turn")?,
        completed: json.get("outcome").is_text("completed"),
        result: to_value(json.get("result")),
    })
}

pub fn run_once(loaded: &Loaded, session: &mut Session, request: &Json) -> Result<Ran, String> {
    let mut journal = store::open(&session.store, &session.journal_path())?;
    session.finish = None;
    let exit = native::run(loaded, &mut journal, request, session)?;
    match text(exit.get("exit")) {
        Some("finished") => {
            let finish = session.finish.take().ok_or("finish unacknowledged")?;
            finish_of(&finish).map(Ran::Finished)
        }
        Some("already_finished") => finish_of(exit.get("finish")).map(Ran::Finished),
        Some("suspended") => Ok(Ran::Suspended {
            turn: number(exit.get("turn"), "a suspension's turn")?,
            seq: number(exit.get("seq"), "a suspension's seq")?,
        }),
        Some("not_due") => Ok(Ran::NotDue),
        Some("corrupt") => Err(format!("corrupt journal: {}", exit.get("error").encode())),
        _ => Err(format!("the module exited {}", exit.encode())),
    }
}

pub fn run_turn(loaded: &Loaded, session: &mut Session, request: &Json) -> Result<Ran, String> {
    let mut ran = run_once(loaded, session, request);
    let mut waited = None;
    for _ in 0..JOINS_PER_TURN_MAX {
        let Ok(Ran::Suspended { seq, .. }) = &ran else {
            return ran;
        };
        if waited == Some(*seq) {
            return ran;
        }
        waited = Some(*seq);
        ran = run_once(loaded, session, request);
    }
    ran
}

pub fn idle_due(loaded: &Loaded, session: &mut Session) -> Result<Option<u64>, String> {
    let mut journal = store::open(&session.store, &session.journal_path())?;
    native::idle(loaded, &mut journal, session)
}

pub fn next_turn(records: &[Record]) -> Result<u64, String> {
    let last = records
        .iter()
        .filter_map(|record| match record {
            Record::Admitted { turn, .. } => Some(*turn),
            _ => None,
        })
        .max();
    Ok(last.map_or(FIRST_TURN, |turn| turn + 1))
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
