use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::tools::{self, Started};

pub const JOB_POLL: Duration = Duration::from_millis(50);
pub const DONE_PREFIX: &str = "done:";
pub const STALL_PREFIX: &str = "stall:";
const STALL_SILENCE: Duration = Duration::from_secs(45);
const STALL_TAIL_BYTES: u64 = 1024;
const NOTIFIED: &str = "You will be notified when it completes.";
const ENDS_WITH_TURN: &str = "If it exits while you are still working you will be notified, but it is terminated when you give your final response and no notification can follow that \u{2014} so do not end your turn to wait for it; if you need its result, wait for it before giving your final response.";
const STALL_ADVICE: &str = "The command is likely blocked on an interactive prompt. Stop this task and re-run with piped input (e.g., `echo y | command`) or a non-interactive flag if one exists.";
const PROMPT_PHRASES: [&str; 5] = ["do you", "would you", "shall i", "are you sure", "ready to"];
const PROMPT_PATTERNS: [&str; 7] = [
    "(y/n)",
    "[y/n]",
    "(yes/no)",
    "press any key",
    "press enter",
    "continue?",
    "overwrite?",
];

struct Running {
    started: Started,
    command: String,
    held: bool,
    written: u64,
    quiet: Instant,
    stalled: bool,
}

pub struct Notice {
    pub id: String,
    pub key: String,
    pub notice: String,
    pub done: bool,
}

#[derive(Default)]
pub struct Jobs {
    running: BTreeMap<String, Running>,
    finished: BTreeMap<String, String>,
    stalls: BTreeMap<String, String>,
}

pub fn started_text(key: &str, message: &str, held: bool) -> String {
    if held {
        format!("{message} (ID: {key}). {NOTIFIED}")
    } else {
        format!("{message} with ID: {key}. {ENDS_WITH_TURN}")
    }
}

pub fn prompting(tail: &str) -> bool {
    let line = tail
        .trim_end()
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let question = line.trim_end_matches(' ').ends_with('?')
        && PROMPT_PHRASES.iter().any(|phrase| {
            line.match_indices(phrase).any(|(at, _)| {
                let before = line[..at].chars().next_back();
                let after = line[at + phrase.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
            })
        });
    question || PROMPT_PATTERNS.iter().any(|pattern| line.contains(pattern))
}

fn without_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        }
    }
    out
}

fn stall_text(key: &str, command: &str, tail: &str) -> String {
    format!(
        "<task-notification>\n<task-id>{key}</task-id>\n<summary>Background command \"{command}\" appears to be waiting for interactive input</summary>\n</task-notification>\nLast output:\n{}\n\n{STALL_ADVICE}",
        tail.trim_end()
    )
}

impl Jobs {
    pub fn start(&mut self, key: &str, started: Started, command: &str, held: bool) {
        let written = started.written_bytes();
        let previous = self.running.insert(
            key.to_owned(),
            Running {
                started,
                command: command.to_owned(),
                held,
                written,
                quiet: Instant::now(),
                stalled: false,
            },
        );
        assert!(previous.is_none(), "a call starts at most one job");
    }

    fn watch_stalls(&mut self) {
        for (key, job) in self.running.iter_mut().filter(|(_, job)| !job.stalled) {
            let written = job.started.written_bytes();
            if written > job.written {
                job.written = written;
                job.quiet = Instant::now();
                continue;
            }
            if job.quiet.elapsed() < STALL_SILENCE {
                continue;
            }
            let Some(tail) = job.started.stdout_tail(STALL_TAIL_BYTES) else {
                continue;
            };
            let tail = without_escapes(&tail);
            if !prompting(&tail) {
                job.quiet = Instant::now();
                continue;
            }
            job.stalled = true;
            self.stalls
                .insert(key.clone(), stall_text(key, &job.command, &tail));
        }
    }

    pub fn poll(&mut self) {
        self.watch_stalls();
        let done: Vec<String> = self
            .running
            .iter_mut()
            .filter_map(|(key, job)| match job.started.child.try_wait() {
                Ok(Some(_)) | Err(_) => Some(key.clone()),
                Ok(None) => None,
            })
            .collect();
        for key in done {
            let job = self
                .running
                .remove(&key)
                .expect("a finished job was running");
            let mut started = job.started;
            let status = started.child.wait().expect("wait for a finished job");
            let notice = match tools::settled(started, status, false) {
                Ok(shell) => {
                    let code = tools::exit_code(&shell);
                    let (state, summary) = if code == 0 {
                        ("completed", "completed".to_owned())
                    } else {
                        ("failed", format!("failed with exit code {code}"))
                    };
                    format!(
                        "<task-notification>\n<task-id>{key}</task-id>\n<status>{state}</status>\n<summary>Background command \"{}\" {summary}</summary>\n</task-notification>\n{}",
                        job.command,
                        tools::shown(&shell)
                    )
                }
                Err(error) => format!(
                    "<task-notification>\n<task-id>{key}</task-id>\n<status>failed</status>\n<summary>Background command \"{}\" lost its output: {error}</summary>\n</task-notification>",
                    job.command
                ),
            };
            self.stalls.remove(&key);
            self.finished.insert(key, notice);
        }
    }

    pub fn notices(&mut self) -> Vec<Notice> {
        self.poll();
        let stalls = self.stalls.iter().map(|(key, notice)| Notice {
            id: format!("{STALL_PREFIX}{key}"),
            key: key.clone(),
            notice: notice.clone(),
            done: false,
        });
        let finished = self.finished.iter().map(|(key, notice)| Notice {
            id: format!("{DONE_PREFIX}{key}"),
            key: key.clone(),
            notice: notice.clone(),
            done: true,
        });
        stalls.chain(finished).collect()
    }

    pub fn answered(&mut self, id: &str, delivered: bool) {
        if !delivered {
            return;
        }
        if let Some(key) = id.strip_prefix(DONE_PREFIX) {
            self.finished.remove(key);
        }
        if let Some(key) = id.strip_prefix(STALL_PREFIX) {
            self.stalls.remove(key);
        }
    }

    pub fn holding(&self) -> bool {
        self.running.values().any(|job| job.held)
    }

    pub fn pending(&self) -> bool {
        !self.finished.is_empty() || !self.stalls.is_empty()
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
