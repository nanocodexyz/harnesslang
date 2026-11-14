use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use crate::anthropic::Client;
use crate::json::Value;
use crate::{Agent, Ledger, child_harness, harness, now_ms, tools};

pub const SPAWN_TOOL: &str = "spawn_agent";
pub const LIST_TOOL: &str = "list_agents";
pub const SUBMIT_TOOL: &str = "submit_result";
pub const RUNNING_MAX: usize = 16;
const LISTED_MAX: u64 = harness::program::ROUNDS_MAX * harness::program::CALLS_PER_ROUND_MAX;
const DIRECTORY: &str = "children";
const SPAWN_FILE: &str = "spawn.json";
const JOURNAL_FILE: &str = "journal.jsonl";
const LEDGER_FILE: &str = "usage.jsonl";

#[derive(Clone)]
pub struct Setup {
    pub base_url: String,
    pub key: String,
    pub model: String,
    pub max_tokens: u64,
    pub system: Value,
    pub tools: Value,
    pub cwd: PathBuf,
}

enum Child {
    Running(JoinHandle<Result<String, String>>),
    Ended(Result<String, String>),
}

pub struct Children {
    directory: PathBuf,
    setup: Setup,
    started: BTreeMap<String, Child>,
}

fn failed(message: &str) -> String {
    Value::object([("failed", Value::text(message))]).encode()
}

fn is_key(key: &str) -> bool {
    let Some((turn, seq)) = key.split_once(harness::SPAWN_KEY_SEPARATOR) else {
        return false;
    };
    [turn, seq]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn write_durably(path: &Path, text: &str) -> Result<(), String> {
    let temporary = path.with_extension("new");
    let mut file = std::fs::File::create(&temporary)
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|error| format!("{}: {error}", path.display()))?;
    let parent = path
        .parent()
        .expect("a child's file is inside its directory");
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("{}: {error}", parent.display()))
}

pub fn submitted(input: &Value) -> tools::Ran {
    match input.get("output").and_then(Value::as_str) {
        Some(output) => tools::Ran::Ok(output.to_owned()),
        None => tools::Ran::Failed("submit_result needs output, a string".to_owned()),
    }
}

pub fn ledgers(journal: &Path) -> Result<Vec<PathBuf>, String> {
    let directory = journal.join(DIRECTORY);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", directory.display()))?;
        let ledger = entry.path().join(LEDGER_FILE);
        if ledger.exists() {
            found.push(ledger);
        }
    }
    found.sort();
    Ok(found)
}

impl Children {
    pub fn new(journal: &Path, setup: Setup) -> Self {
        Self {
            directory: journal.join(DIRECTORY),
            setup,
            started: BTreeMap::new(),
        }
    }

    fn running(&self) -> usize {
        self.started
            .values()
            .filter(|child| matches!(child, Child::Running(handle) if !handle.is_finished()))
            .count()
    }

    pub fn spawn(&mut self, turn: u64, seq: u64, input: &Value) -> tools::Ran {
        let key = format!("{turn}{}{seq}", harness::SPAWN_KEY_SEPARATOR);
        assert!(
            is_key(&key),
            "a spawn's key is its call's turn and sequence number"
        );
        if self.running() >= RUNNING_MAX {
            return tools::Ran::Failed(format!(
                "{RUNNING_MAX} subagents are already running; wait for one with wait_agent before starting another"
            ));
        }
        let fields = ["role", "task"].map(|name| input.get(name).and_then(Value::as_str));
        let [Some(role), Some(task)] = fields else {
            return tools::Ran::Failed("spawn_agent needs role and task, both strings".to_owned());
        };
        let directory = self.directory.join(&key);
        if directory.join(SPAWN_FILE).exists() {
            return tools::Ran::Failed(format!(
                "{} already exists: the journal and its children directory disagree",
                directory.display()
            ));
        }
        let spawn = Value::object([("role", Value::text(role)), ("task", Value::text(task))]);
        if let Err(error) = std::fs::create_dir_all(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))
            .and_then(|()| write_durably(&directory.join(SPAWN_FILE), &spawn.encode()))
        {
            return tools::Ran::Failed(error);
        }
        let setup = self.setup.clone();
        let thread_key = key.clone();
        let started = std::thread::Builder::new()
            .name(key.clone())
            .spawn(move || run(&setup, &directory, &thread_key));
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
                "{key:?} is not an agent ID that spawn_agent returned"
            )));
        }
        let directory = self.directory.join(key);
        if !self.started.contains_key(key) {
            if !directory.join(SPAWN_FILE).exists() {
                return Some(failed(&format!("no subagent has the ID {key}")));
            }
            let ended = run(&self.setup, &directory, key);
            self.started.insert(key.to_owned(), Child::Ended(ended));
        }
        self.settle(key);
        match self.started.get(key) {
            Some(Child::Ended(Ok(answer))) => Some(answer.clone()),
            Some(Child::Ended(Err(error))) => {
                eprintln!("hl-agent: subagent {key} did not finish: {error}");
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
                "{} children, more than a turn can start (LISTED_MAX, {LISTED_MAX})",
                keys.len()
            ));
        }
        keys.sort_by_key(|key| {
            key.split_once(harness::SPAWN_KEY_SEPARATOR)
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
            .and_then(|spawn| spawn.get("role").and_then(Value::as_str).map(str::to_owned))
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
            eprintln!("hl-agent: waiting for subagent {key}, whose result the turn did not read");
            self.settle(&key);
        }
    }
}

fn run(setup: &Setup, directory: &Path, key: &str) -> Result<String, String> {
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
    let (role, task) = (field("role")?, field("task")?);
    let state = directory.join("state");
    std::fs::create_dir_all(&state).map_err(|error| format!("{}: {error}", state.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(LEDGER_FILE))
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let mut agent = Agent {
        client: Client {
            base_url: setup.base_url.clone(),
            key: setup.key.clone(),
            model: setup.model.clone(),
            max_tokens: setup.max_tokens,
            system: setup.system.clone(),
            tools: setup.tools.clone(),
            scratch: state.clone(),
        },
        workspace: tools::Workspace {
            root: setup.cwd.clone(),
            state,
            image_bytes_max: child_harness::program::blob_bound("Read").map(|(_, bytes)| bytes),
            left: std::sync::Mutex::new(Vec::new()),
        },
        ledger: Ledger { file },
        children: None,
        label: format!("hl-agent[{key}]"),
        jobs: crate::jobs::Jobs::default(),
    };
    let mark = |agent: &mut Agent, kind: &str| {
        agent.ledger.append(Value::object([
            ("kind", Value::text(kind)),
            ("at_ms", Value::number(now_ms())),
        ]));
    };
    mark(&mut agent, "start");
    let journal = directory.join(JOURNAL_FILE);
    let mut disk = child_harness::FileDisk::open(&journal)
        .map_err(|error| format!("{}: {error}", journal.display()))?;
    let request = child_harness::TurnRequest {
        turn: child_harness::FIRST_TURN,
        arguments: child_harness::program::Arguments::Delegated(role, task),
    };
    let ran = child_harness::run_jsonl(&mut agent, &mut disk, &request);
    mark(&mut agent, "end");
    match ran {
        Ok(child_harness::Ran::Finished(finish) | child_harness::Ran::AlreadyFinished(finish)) => {
            eprintln!("{}: finished {}", agent.label, finish.result.encode());
            Ok(finish.result.encode())
        }
        Ok(child_harness::Ran::Suspended(_)) => {
            panic!("the child harness declares no suspend, so its turn never waits")
        }
        Ok(child_harness::Ran::NotDue(_)) => {
            panic!(
                "the child harness declares no idle handler, so its prompt is never refused as not due"
            )
        }
        Err(error) => Err(format!("{error:?}")),
    }
}
