use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use harnesslang::wasm::js::Json;

use crate::host::{self, Ran, Session, Setup};
use crate::json::Value;
use crate::tools;

pub const RUNNING_MAX: usize = 16;
pub const LISTED_MAX: u64 = 4096;
const DIRECTORY: &str = "children";
const SPAWN_FILE: &str = "spawn.json";
const CONFIGURE_TURN: u64 = 1;
const NO_HANDLER: &str = "no handler";

enum Child {
    Running(JoinHandle<Result<String, String>>),
    Ended(Result<String, String>),
}

pub struct Children {
    directory: PathBuf,
    setup: Setup,
    cancel: PathBuf,
    started: BTreeMap<String, Child>,
}

fn failed(message: &str) -> String {
    Value::object([("failed", Value::text(message))]).encode()
}

fn is_key(key: &str) -> bool {
    let Some((turn, seq)) = key.split_once(harnesslang::ir::SPAWN_KEY_SEPARATOR) else {
        return false;
    };
    [turn, seq]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

pub fn write_durably(path: &Path, text: &str) -> Result<(), String> {
    let temporary = path.with_extension("new");
    let mut file = std::fs::File::create(&temporary)
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|error| format!("{}: {error}", path.display()))?;
    let parent = path.parent().expect("a durable file is inside a directory");
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("{}: {error}", parent.display()))
}

impl Children {
    pub fn new(session: &Path, setup: Setup, cancel: &Path) -> Self {
        Self {
            directory: session.join(DIRECTORY),
            setup,
            cancel: cancel.to_path_buf(),
            started: BTreeMap::new(),
        }
    }

    fn running(&self) -> usize {
        self.started
            .values()
            .filter(|child| matches!(child, Child::Running(handle) if !handle.is_finished()))
            .count()
    }

    pub fn spawn(
        &mut self,
        turn: u64,
        seq: u64,
        program: &str,
        handler: &str,
        input: &Value,
        configure: Option<Json>,
    ) -> tools::Ran {
        let key = format!("{turn}{}{seq}", harnesslang::ir::SPAWN_KEY_SEPARATOR);
        assert!(
            is_key(&key),
            "a spawn's key is its call's turn and sequence number"
        );
        if self.running() >= RUNNING_MAX {
            return tools::Ran::Failed(format!(
                "{RUNNING_MAX} subagents are already running; wait for one before starting another"
            ));
        }
        let directory = self.directory.join(&key);
        if directory.join(SPAWN_FILE).exists() {
            return tools::Ran::Failed(format!(
                "{} already exists: the journal and its children directory disagree",
                directory.display()
            ));
        }
        let configure = configure.map_or(Value::Null, |json| host::to_value(&json));
        let spawn = Value::object([
            ("program", Value::text(program)),
            ("handler", Value::text(handler)),
            ("arguments", input.clone()),
            ("configure", configure),
        ]);
        if let Err(error) = std::fs::create_dir_all(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))
            .and_then(|()| write_durably(&directory.join(SPAWN_FILE), &spawn.encode()))
        {
            return tools::Ran::Failed(error);
        }
        let (setup, cancel, thread_key) = (self.setup.clone(), self.cancel.clone(), key.clone());
        let started = std::thread::Builder::new()
            .name(key.clone())
            .spawn(move || run(&setup, &directory, &thread_key, &cancel));
        match started {
            Ok(handle) => {
                self.started.insert(key.clone(), Child::Running(handle));
                tools::Ran::Ok(key)
            }
            Err(error) => tools::Ran::Failed(format!("start subagent {key}: {error}")),
        }
    }

    fn settle(&mut self, key: &str) {
        if !matches!(self.started.get(key), Some(Child::Running(_))) {
            return;
        }
        let Some(Child::Running(handle)) = self.started.remove(key) else {
            panic!("a running child is in the map");
        };
        let ended = handle
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        self.started.insert(key.to_owned(), Child::Ended(ended));
    }

    pub fn join(&mut self, key: &str) -> Option<String> {
        if !is_key(key) {
            return Some(failed(&format!(
                "{key:?} is not an agent ID that a spawn returned"
            )));
        }
        let directory = self.directory.join(key);
        if !self.started.contains_key(key) {
            if !directory.join(SPAWN_FILE).exists() {
                return Some(failed(&format!("no subagent has the ID {key}")));
            }
            let ended = run(&self.setup, &directory, key, &self.cancel);
            self.started.insert(key.to_owned(), Child::Ended(ended));
        }
        self.settle(key);
        match self.started.get(key) {
            Some(Child::Ended(Ok(answer))) => Some(answer.clone()),
            Some(Child::Ended(Err(error))) => {
                eprintln!("hl-host: subagent {key} did not finish: {error}");
                self.started.remove(key);
                None
            }
            _ => panic!("a settled child has ended"),
        }
    }

    pub fn list(&mut self, input: &Value) -> tools::Ran {
        let include_completed = matches!(input.get("include_completed"), Some(Value::Bool(true)));
        let mut keys = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.directory) {
            for entry in entries {
                let Ok(entry) = entry else {
                    return tools::Ran::Failed(format!("read {}", self.directory.display()));
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_key(&name) {
                    keys.push(name);
                }
            }
        }
        if keys.len() as u64 > LISTED_MAX {
            return tools::Ran::Failed(format!(
                "{} children, more than LISTED_MAX ({LISTED_MAX})",
                keys.len()
            ));
        }
        keys.sort_by_key(|key| {
            key.split_once(harnesslang::ir::SPAWN_KEY_SEPARATOR)
                .map(|(turn, seq)| (turn.parse::<u64>().ok(), seq.parse::<u64>().ok()))
        });
        let mut agents = Vec::new();
        for key in keys {
            if let Some(agent) = self.listed(&key, include_completed) {
                agents.push(agent);
            }
        }
        tools::Ran::Ok(Value::object([("agents", Value::Array(agents))]).encode())
    }

    fn listed(&mut self, key: &str, include_completed: bool) -> Option<Value> {
        if matches!(self.started.get(key), Some(Child::Running(handle)) if handle.is_finished()) {
            self.settle(key);
        }
        let role = std::fs::read_to_string(self.directory.join(key).join(SPAWN_FILE))
            .ok()
            .and_then(|text| Value::decode(&text).ok())
            .and_then(|spawn| {
                spawn
                    .get("arguments")
                    .and_then(|arguments| arguments.get("role"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let status = match self.started.get(key) {
            Some(Child::Running(_)) => Value::object([("state", Value::text("running"))]),
            Some(Child::Ended(Ok(output))) if include_completed => Value::object([
                ("state", Value::text("completed")),
                ("output", Value::text(output)),
            ]),
            Some(Child::Ended(Ok(_))) => return None,
            Some(Child::Ended(Err(error))) => Value::object([
                ("state", Value::text("failed")),
                ("error", Value::text(error)),
            ]),
            None => Value::object([("state", Value::text("interrupted"))]),
        };
        Some(Value::object([
            ("agent_id", Value::text(key)),
            ("role", Value::String(role)),
            ("status", status),
        ]))
    }

    pub fn finish(&mut self) {
        let running: Vec<String> = self
            .started
            .iter()
            .filter(|(_, child)| matches!(child, Child::Running(_)))
            .map(|(key, _)| key.clone())
            .collect();
        for key in running {
            eprintln!("hl-host: waiting for subagent {key}, whose result the turn did not read");
            self.settle(&key);
        }
    }
}

fn run(setup: &Setup, directory: &Path, key: &str, cancel: &Path) -> Result<String, String> {
    let spawn_path = directory.join(SPAWN_FILE);
    let spawn = std::fs::read_to_string(&spawn_path)
        .map_err(|error| format!("{}: {error}", spawn_path.display()))
        .and_then(|text| {
            Value::decode(&text).map_err(|error| format!("{}: {error}", spawn_path.display()))
        })?;
    let field = |name: &str| {
        spawn
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("{} has no {name}", spawn_path.display()))
    };
    let (program, handler) = (field("program")?, field("handler")?);
    let arguments = spawn
        .get("arguments")
        .map(host::to_json)
        .ok_or_else(|| format!("{} has no arguments", spawn_path.display()))?;
    let loaded = host::load_program(setup.module_dir.as_deref(), &program)?;
    let label = format!("hl-host[{key}]");
    let mut session = Session::open(setup.clone(), directory, &label, cancel)?;
    session.ledger.mark("start", crate::now_ms());
    let mut turn = CONFIGURE_TURN;
    if let Some(configure) = spawn
        .get("configure")
        .filter(|value| !matches!(value, Value::Null))
    {
        let configure = host::to_json(configure);
        session.remember_configure(&configure)?;
        let request = host::request(host::CONFIGURE_HANDLER, CONFIGURE_TURN, configure);
        match host::run_turn(&loaded, &mut session, &request) {
            Ok(Ran::Finished(finish)) if finish.completed => turn += 1,
            Ok(Ran::Finished(finish)) => {
                session.close();
                return Err(format!("configure ended {}", finish.result.encode()));
            }
            Err(error) if error.contains(NO_HANDLER) => {}
            Ok(Ran::Suspended { seq, .. }) => {
                session.close();
                return Err(format!("configure suspended at {seq}"));
            }
            Ok(Ran::NotDue) => {}
            Err(error) => {
                session.close();
                return Err(error);
            }
        }
    }
    let request = host::request(&handler, turn, arguments);
    let ran = host::run_turn(&loaded, &mut session, &request);
    session.close();
    session.ledger.mark("end", crate::now_ms());
    match ran {
        Ok(Ran::Finished(finish)) => {
            eprintln!("{label}: finished {}", finish.result.encode());
            Ok(finish.result.encode())
        }
        Ok(Ran::Suspended { seq, .. }) => Err(format!("the child turn waits on suspend {seq}")),
        Ok(Ran::NotDue) => Err("the child turn was refused as not due".to_owned()),
        Err(error) => Err(error),
    }
}
