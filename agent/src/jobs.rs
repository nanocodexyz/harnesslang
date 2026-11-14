use std::collections::BTreeMap;
use std::time::Duration;

use crate::tools::{self, Started};

pub const JOB_POLL: Duration = Duration::from_millis(50);
pub const DONE_PREFIX: &str = "done:";
const NOTIFIED: &str = "You will be notified when it completes.";
const ENDS_WITH_TURN: &str = "If it exits while you are still working you will be notified, but it is terminated when you give your final response and no notification can follow that \u{2014} so do not end your turn to wait for it; if you need its result, wait for it before giving your final response.";

struct Running {
    started: Started,
    command: String,
    held: bool,
}

#[derive(Default)]
pub struct Jobs {
    running: BTreeMap<String, Running>,
    finished: BTreeMap<String, String>,
}

pub fn started_text(key: &str, message: &str, held: bool) -> String {
    if held {
        format!("{message} (ID: {key}). {NOTIFIED}")
    } else {
        format!("{message} with ID: {key}. {ENDS_WITH_TURN}")
    }
}

impl Jobs {
    pub fn start(&mut self, key: &str, started: Started, command: &str, held: bool) {
        let previous = self.running.insert(
            key.to_owned(),
            Running {
                started,
                command: command.to_owned(),
                held,
            },
        );
        assert!(previous.is_none(), "a call starts at most one job");
    }

    pub fn poll(&mut self) {
        let done: Vec<String> = self
            .running
            .iter_mut()
            .filter_map(|(key, job)| match job.started.child.try_wait() {
                Ok(Some(_)) | Err(_) => Some(key.clone()),
                Ok(None) => None,
            })
            .collect();
        for key in done {
            let job = self.running.remove(&key).expect("a finished job was running");
            let mut started = job.started;
            let status = started.child.wait().expect("wait for a finished job");
            let shell = tools::settled(started, status, false);
            let code = tools::exit_code(&shell);
            let (state, summary) = if code == 0 {
                ("completed", "completed".to_owned())
            } else {
                ("failed", format!("failed with exit code {code}"))
            };
            let notice = format!(
                "<task-notification>\n<task-id>{key}</task-id>\n<status>{state}</status>\n<summary>Background command \"{}\" {summary}</summary>\n</task-notification>\n{}",
                job.command,
                tools::shown(&shell)
            );
            self.finished.insert(key, notice);
        }
    }

    pub fn notices(&mut self) -> Vec<(String, String)> {
        self.poll();
        self.finished
            .iter()
            .map(|(key, notice)| (key.clone(), notice.clone()))
            .collect()
    }

    pub fn answered(&mut self, id: &str, delivered: bool) {
        if let (true, Some(key)) = (delivered, id.strip_prefix(DONE_PREFIX)) {
            self.finished.remove(key);
        }
    }

    pub fn holding(&self) -> bool {
        self.running.values().any(|job| job.held)
    }

    pub fn pending(&self) -> bool {
        !self.finished.is_empty()
    }

    pub fn knows(&self, key: &str) -> bool {
        self.running.contains_key(key) || self.finished.contains_key(key)
    }

    pub fn stop_all(&mut self) {
        for (_, mut job) in std::mem::take(&mut self.running) {
            tools::kill_group(&job.started.child);
            let _ = job.started.child.wait();
        }
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        self.stop_all();
    }
}
