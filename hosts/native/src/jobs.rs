use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::texts::Texts;
use crate::tools::{self, Started};

pub const JOB_POLL: Duration = Duration::from_millis(50);
pub const DONE_PREFIX: &str = "done:";
pub const STALL_PREFIX: &str = "stall:";
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
    pub texts: Texts,
    running: BTreeMap<String, Running>,
    finished: BTreeMap<String, String>,
    stalls: BTreeMap<String, String>,
}

pub fn started_text(texts: &Texts, key: &str, message: &str, held: bool) -> String {
    let name = if held {
        "job_started"
    } else {
        "job_started_unheld"
    };
    texts.fill(name, &[("message", message), ("key", key)])
}

pub fn prompting(texts: &Texts, tail: &str) -> bool {
    let line = tail
        .trim_end()
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let question = line.trim_end_matches(' ').ends_with('?')
        && texts.list("prompt_phrases").iter().any(|phrase| {
            line.match_indices(phrase.as_str()).any(|(at, _)| {
                let before = line[..at].chars().next_back();
                let after = line[at + phrase.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
            })
        });
    question
        || texts
            .list("prompt_patterns")
            .iter()
            .any(|pattern| line.contains(pattern.as_str()))
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

impl Jobs {
    pub fn new(texts: Texts) -> Self {
        Jobs {
            texts,
            running: BTreeMap::new(),
            finished: BTreeMap::new(),
            stalls: BTreeMap::new(),
        }
    }

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
        assert!(previous.is_none(), "duplicate job");
    }

    fn watch_stalls(&mut self) {
        let (Some(silence_ms), Some(tail_bytes)) = (
            self.texts.number("stall_silence_ms"),
            self.texts.number("stall_tail_bytes"),
        ) else {
            return;
        };
        let silence = Duration::from_millis(silence_ms);
        for (key, job) in self.running.iter_mut().filter(|(_, job)| !job.stalled) {
            let written = job.started.written_bytes();
            if written > job.written {
                job.written = written;
                job.quiet = Instant::now();
                continue;
            }
            if job.quiet.elapsed() < silence {
                continue;
            }
            let Some(tail) = job.started.stdout_tail(tail_bytes) else {
                continue;
            };
            let tail = without_escapes(&tail);
            if !prompting(&self.texts, &tail) {
                job.quiet = Instant::now();
                continue;
            }
            job.stalled = true;
            let notice = self.texts.fill(
                "job_stalled",
                &[
                    ("key", key),
                    ("command", &job.command),
                    ("tail", tail.trim_end()),
                ],
            );
            self.stalls.insert(key.clone(), notice);
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
            let job = self.running.remove(&key).expect("finished job missing");
            let mut started = job.started;
            let status = started.child.wait().expect("job wait");
            let notice = match tools::settled(started, status, false) {
                Ok(shell) => format!(
                    "{}\n{key}\n{}\n{}\n{}{}",
                    tools::JOB_MARK,
                    tools::exit_code(&shell),
                    job.command.len(),
                    job.command,
                    tools::shown(&shell)
                ),
                Err(error) => self.texts.fill(
                    "job_lost",
                    &[("key", &key), ("command", &job.command), ("error", &error)],
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
        for (key, mut job) in std::mem::take(&mut self.running) {
            if let Err(error) = tools::kill_group(&mut job.started.child) {
                eprintln!("hl-host: stopping job {key}: {error}");
            }
            let _ = job.started.child.wait();
        }
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        self.stop_all();
    }
}
