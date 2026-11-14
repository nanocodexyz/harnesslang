use std::collections::BTreeMap;

use crate::ir::Program;
use crate::run::engine::Answer;
use crate::run::journal::{Delivery, Inbound, JobNotice, LOST_PREFIX, Record};
use crate::run::json::Json;
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;

pub const JOB_START_PPM: u64 = 400_000;
pub const JOB_STALL_PPM: u64 = 250_000;
pub const JOB_DIES_PPM: u64 = 500_000;
pub const JOB_RUN_MS_MAX: u64 = 10 * crate::sim::world::CLOCK_STEP_MAX;
pub const RECOVER_UNKNOWN_PPM: u64 = 300_000;
pub const DONE_PREFIX: &str = "done:";
pub const STALL_PREFIX: &str = "stall:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimJob {
    pub turn: u64,
    pub done_ms: u64,
    pub stall_ms: Option<u64>,
    pub alive: bool,
    pub done: Option<Answer>,
    pub stalled: Option<Answer>,
    pub reported: Option<u64>,
    pub drained: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Jobs {
    pub jobs: BTreeMap<String, SimJob>,
    pub started: u64,
    pub notices: u64,
    pub died: u64,
    faults: Vec<Violation>,
}

fn fault(invariant: Invariant, detail: String) -> Violation {
    Violation { invariant, detail }
}

fn settled(answer: Option<Answer>) -> bool {
    answer.is_some_and(Answer::delivered)
}

pub fn started_text(key: &str) -> String {
    format!("Command running in background with ID: {key}")
}

pub fn notice_text(key: &str, status: &str) -> String {
    format!(
        "<task-notification>\n<task-id>{key}</task-id>\n<status>{status}</status>\n</task-notification>"
    )
}

impl Jobs {
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    pub fn key(&self) -> String {
        format!("{:?} {} {}", self.jobs, self.started, self.died)
    }

    pub fn start(&mut self, rng: &mut Rng, key: &str, turn: u64, clock: u64) -> Json {
        let done_ms = clock + rng.range(1, JOB_RUN_MS_MAX);
        let stall_ms = rng
            .chance(JOB_STALL_PPM)
            .then(|| clock + rng.below(done_ms - clock));
        let reply = Json::object([(crate::ir::JOB_KEY, Json::String(started_text(key)))]);
        if self.jobs.contains_key(key) {
            return reply;
        }
        self.jobs.insert(
            key.to_owned(),
            SimJob {
                turn,
                done_ms,
                stall_ms,
                alive: true,
                done: None,
                stalled: None,
                reported: None,
                drained: false,
            },
        );
        self.started += 1;
        reply
    }

    pub fn crash(&mut self, seed: u64, crashes: u64) {
        let mut rng = Rng::derive_keyed(seed, "job-death", &[crashes]);
        for job in self.jobs.values_mut() {
            if job.alive && !settled(job.done) && rng.chance(JOB_DIES_PPM) {
                job.alive = false;
                self.died += 1;
            }
        }
    }

    fn due(job: &SimJob) -> Vec<(u64, bool)> {
        let mut due = Vec::new();
        if !job.alive {
            return due;
        }
        if let Some(stall) = job.stall_ms
            && !settled(job.stalled)
            && !settled(job.done)
        {
            due.push((stall, false));
        }
        if !settled(job.done) {
            due.push((job.done_ms, true));
        }
        due
    }

    pub fn arrived(&self, clock: u64) -> Vec<Delivery> {
        let mut arrived = Vec::new();
        for (key, job) in &self.jobs {
            for (at, done) in Self::due(job) {
                if at > clock {
                    continue;
                }
                let (prefix, status) = if done {
                    (DONE_PREFIX, "completed")
                } else {
                    (STALL_PREFIX, "waiting for interactive input")
                };
                arrived.push(Delivery {
                    id: format!("{prefix}{key}"),
                    inbound: Inbound::Job(JobNotice {
                        key: key.clone(),
                        notice: notice_text(key, status),
                        done,
                    }),
                });
            }
        }
        arrived
    }

    pub fn next_due(&self) -> Option<u64> {
        self.jobs
            .values()
            .flat_map(Self::due)
            .map(|(at, _)| at)
            .min()
    }

    pub fn owns(id: &str) -> bool {
        id.starts_with(DONE_PREFIX) || id.starts_with(STALL_PREFIX)
    }

    pub fn answer(&mut self, id: &str, answer: Answer) {
        let (key, done) = match (id.strip_prefix(DONE_PREFIX), id.strip_prefix(STALL_PREFIX)) {
            (Some(key), _) => (key, true),
            (None, Some(key)) => (key, false),
            (None, None) => panic!("only job notices reach the jobs"),
        };
        let Some(job) = self.jobs.get_mut(key) else {
            self.faults.push(fault(
                Invariant::JobOnce,
                format!("job notice '{id}' was answered, but no job {key} ran"),
            ));
            return;
        };
        if !matches!(answer, Answer::Recorded | Answer::Duplicate | Answer::Full) {
            self.faults.push(fault(
                Invariant::JobOnce,
                format!("job notice '{id}' was answered {}", answer.name()),
            ));
        }
        if answer == Answer::Recorded {
            self.notices += 1;
        }
        let slot = if done {
            &mut job.done
        } else {
            &mut job.stalled
        };
        if !settled(*slot) {
            *slot = Some(answer);
        }
    }

    pub fn lost(&mut self, open: &[String], run: u64) -> Vec<String> {
        let mut lost = Vec::new();
        for key in open {
            let dead = self.jobs.get(key).is_none_or(|job| !job.alive);
            if !dead {
                continue;
            }
            if let Some(job) = self.jobs.get_mut(key) {
                job.reported = Some(run);
                job.drained = false;
            }
            lost.push(key.clone());
        }
        lost
    }

    pub fn drained(&mut self, run: u64) {
        for job in self.jobs.values_mut() {
            if job.reported == Some(run) {
                job.drained = true;
            }
        }
    }

    pub fn check(&self, program: &Program, records: &[Record]) -> Result<(), Violation> {
        if let Some(fault) = self.faults.first() {
            return Err(fault.clone());
        }
        let Some(bound) = &program.events.jobs else {
            return Ok(());
        };
        let mut closed: BTreeMap<&str, u64> = BTreeMap::new();
        let mut per_turn: BTreeMap<u64, u64> = BTreeMap::new();
        for record in records {
            let Record::Event { turn, delivery, .. } = record else {
                continue;
            };
            let Inbound::Job(job) = &delivery.inbound else {
                continue;
            };
            *per_turn.entry(*turn).or_insert(0) += 1;
            if !job.done {
                continue;
            }
            let count = closed.entry(job.key.as_str()).or_insert(0);
            *count += 1;
            if *count > 1 {
                return Err(fault(
                    Invariant::JobOnce,
                    format!("job {} is closed {count} times", job.key),
                ));
            }
            let alive = self.jobs.get(&job.key).is_some_and(|ran| ran.alive);
            if delivery.id.starts_with(LOST_PREFIX) && alive {
                return Err(fault(
                    Invariant::JobOnce,
                    format!(
                        "job {} is reported lost in turn {turn}, yet it was still running",
                        job.key
                    ),
                ));
            }
        }
        let full = per_turn.values().any(|count| *count >= bound.value);
        for (key, job) in &self.jobs {
            if job.drained && !closed.contains_key(key.as_str()) && !full {
                return Err(fault(
                    Invariant::JobReported,
                    format!(
                        "job {key} died in a crash and its host reported it lost, yet a later run took events without recording its lost notice"
                    ),
                ));
            }
        }
        Ok(())
    }
}
