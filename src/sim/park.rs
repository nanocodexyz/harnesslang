use crate::run::engine::{Resume, Stop, Suspension, WAIT_UNTIL, wait_until};
use crate::run::journal::{self, Output, Record};
use crate::run::json::Json;
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;
use crate::sim::world::SimWorld;
use crate::target::{WaitLimit, WaitScope};

pub const SUSPEND_DELAY_MS_MAX: u64 = 8 * 60 * 60 * 1000;
pub const WRONG_KEY_PPM: u64 = 200_000;
pub const ALLOW_PPM: u64 = 500_000;
pub const ALLOW_ANSWER: &str = "allow";
pub const WRONG_KEY_SUFFIX: &str = "-elsewhere";

#[derive(Clone, Debug, Default)]
pub struct Parking {
    pub pending: Option<(Suspension, Resume)>,
    pub sent: Vec<(u64, u64, String, String)>,
    pub faults: Vec<Violation>,
    pub suspensions: u64,
    pub run_waited_ms: u64,
}

fn fault(invariant: Invariant, detail: String) -> Violation {
    Violation { invariant, detail }
}

impl Parking {
    pub fn key(&self) -> String {
        format!(
            "{:?} {:?} {} {}",
            self.pending, self.sent, self.suspensions, self.run_waited_ms
        )
    }
}

fn durable_call(records: &[Record], turn: u64, seq: u64) -> Option<(String, Json)> {
    records.iter().find_map(|record| match record {
        Record::Call {
            turn: called,
            seq: at,
            tool,
            input,
            ..
        } if *called == turn && *at == seq => Some((tool.clone(), input.clone())),
        _ => None,
    })
}

impl SimWorld<'_> {
    pub fn suspended(&mut self, suspension: &Suspension) -> Result<(), Violation> {
        let id = journal::call_id(suspension.turn, suspension.seq);
        let records = self.medium.durable().unwrap_or_default();
        let durable = durable_call(&records, suspension.turn, suspension.seq);
        let keyed = durable.as_ref().is_some_and(|(tool, input)| {
            tool == crate::ir::SUSPEND_TOOL
                && input
                    .get(crate::run::engine::SUSPEND_KEY)
                    .and_then(Json::as_str)
                    == Some(suspension.key.as_str())
        });
        if !keyed {
            return Err(fault(
                Invariant::SuspendDurable,
                format!(
                    "turn {} reported waiting on suspend {id} ({}), whose call record is not durable",
                    suspension.turn, suspension.key
                ),
            ));
        }
        self.parking.suspensions += 1;
        self.stats.suspensions += 1;
        let suspensions_max = self.handler_worst().effects.total;
        if self.parking.suspensions > suspensions_max {
            return Err(fault(
                Invariant::Liveness,
                format!(
                    "turn {} suspended {} times, more than its {suspensions_max} worst-case effects",
                    suspension.turn, self.parking.suspensions
                ),
            ));
        }
        let mut rng = Rng::derive_keyed(
            self.seed,
            "resume",
            &[suspension.turn, suspension.seq, self.parking.suspensions],
        );
        let key = if rng.chance(WRONG_KEY_PPM) {
            format!("{}{WRONG_KEY_SUFFIX}", suspension.key)
        } else {
            suspension.key.clone()
        };
        let answer = if rng.chance(ALLOW_PPM) {
            ALLOW_ANSWER.to_owned()
        } else {
            SimWorld::generated_text(&mut rng)
        };
        let answer = match self.children.get(&suspension.key) {
            Some(crate::sim::world::Child { result: Ok(result) }) => {
                self.stats.joined += 1;
                result.clone()
            }
            _ => answer,
        };
        self.clock += rng.range(1, SUSPEND_DELAY_MS_MAX);
        self.parking.pending = Some((suspension.clone(), Resume { key, answer }));
        Ok(())
    }

    pub fn deliver_resume(&mut self, suspension: &Suspension) -> Option<Resume> {
        let resume = match &self.parking.pending {
            Some((waiting, resume)) if waiting == suspension && !self.nested => {
                Some(resume.clone())
            }
            _ => None,
        };
        let Some(resume) = resume else {
            return None;
        };
        self.parking.sent.push((
            suspension.turn,
            suspension.seq,
            resume.key.clone(),
            resume.answer.clone(),
        ));
        self.record("resume", || {
            vec![
                ("turn", Json::Number(suspension.turn)),
                ("seq", Json::Number(suspension.seq)),
                ("key", Json::text(&resume.key)),
                ("answer", Json::text(&resume.answer)),
            ]
        });
        Some(resume)
    }

    pub fn answer_resume(&mut self, suspension: &Suspension, resume: &Resume, matched: bool) {
        self.record("resumed", || {
            vec![
                ("turn", Json::Number(suspension.turn)),
                ("seq", Json::Number(suspension.seq)),
                ("matched", Json::Bool(matched)),
            ]
        });
        if matched && resume.key != suspension.key {
            self.parking.faults.push(fault(
                Invariant::ResumeMatched,
                format!(
                    "suspend {} waits on key {}, and a resume with key {} resumed it",
                    journal::call_id(suspension.turn, suspension.seq),
                    suspension.key,
                    resume.key
                ),
            ));
        }
        self.parking.pending = None;
    }

    pub fn wait_clock(&mut self) -> u64 {
        self.clock += self.rng_step();
        let clock = self.clock;
        self.record("clock", || vec![("answer", Json::Number(clock))]);
        clock
    }

    pub fn sleep_until(&mut self, turn: u64, seq: u64, until: u64) -> Result<u64, Stop> {
        self.count_wait();
        let before = self.clock;
        if let Some(stop) = self.crash_mid_wait(until) {
            let clock = self.clock;
            self.record("sleep", || {
                vec![
                    ("turn", Json::Number(turn)),
                    ("seq", Json::Number(seq)),
                    ("until", Json::Number(until)),
                    ("clock", Json::Number(clock)),
                    ("answer", Json::text("crash")),
                ]
            });
            return Err(stop);
        }
        self.clock = self.clock.max(until);
        let slept = self.clock - before;
        self.check_sleep(turn, seq, until, slept);
        let clock = self.clock;
        self.record("sleep", || {
            vec![
                ("turn", Json::Number(turn)),
                ("seq", Json::Number(seq)),
                ("until", Json::Number(until)),
                ("clock", Json::Number(clock)),
                ("answer", Json::text("ok")),
            ]
        });
        Ok(clock)
    }

    pub fn check_sleep(&mut self, turn: u64, seq: u64, until: u64, slept: u64) {
        let id = journal::call_id(turn, seq);
        let records = self.medium.durable().unwrap_or_default();
        match durable_call(&records, turn, seq).and_then(|(_, input)| wait_until(&input)) {
            None => self.parking.faults.push(fault(
                Invariant::WaitUntil,
                format!("wait {id} sleeps before its call record and deadline are durable"),
            )),
            Some(recorded) if recorded != until => self.parking.faults.push(fault(
                Invariant::WaitUntil,
                format!("wait {id} sleeps until {until}, and its durable call record says {WAIT_UNTIL} {recorded}"),
            )),
            Some(_) => {}
        }
        self.parking.run_waited_ms += slept;
        let waited = self.parking.run_waited_ms;
        let worst = self.handler_wait_ms();
        if waited > worst {
            self.parking.faults.push(fault(
                Invariant::WallLimit,
                format!("one run of turn {turn} waited {waited} ms, over its handler's worst case of {worst} ms"),
            ));
        }
        if let Some(WaitLimit { limit, scope }) = self.target.limits.wait_ms {
            let (measured, what) = match scope {
                WaitScope::Run => (waited, format!("one run of turn {turn} waited")),
                WaitScope::Step => (slept, format!("wait {id} waited")),
            };
            if measured > limit.value {
                self.parking.faults.push(fault(
                    Invariant::WallLimit,
                    format!(
                        "{what} {measured} ms by the end of wait {id}, over {}'s limit of {} {}",
                        self.target.target.name(),
                        limit.value,
                        limit.unit
                    ),
                ));
            }
        }
    }
}

pub fn check(world: &SimWorld<'_>, records: &[Record]) -> Result<(), Violation> {
    if let Some(found) = world.parking.faults.first() {
        return Err(found.clone());
    }
    let mut calls = std::collections::BTreeMap::new();
    for record in records {
        match record {
            Record::Call {
                turn,
                seq,
                tool,
                input,
                ..
            } => {
                calls.insert((*turn, *seq), (tool.clone(), input.clone()));
            }
            Record::Result {
                turn,
                seq,
                output: Output::Ok(value),
            } => {
                let Some((tool, input)) = calls.get(&(*turn, *seq)) else {
                    continue;
                };
                result(world, *turn, *seq, tool, input, value)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn result(
    world: &SimWorld<'_>,
    turn: u64,
    seq: u64,
    tool: &str,
    input: &Json,
    value: &Json,
) -> Result<(), Violation> {
    let id = journal::call_id(turn, seq);
    if tool == crate::ir::WAIT_TOOL
        && let Some(until) = wait_until(input)
        && value.as_u64().is_some_and(|woke| woke < until)
    {
        return Err(fault(
            Invariant::WaitUntil,
            format!(
                "wait {id} records waking at {}, before its deadline {until}",
                value.encode()
            ),
        ));
    }
    if tool == crate::ir::SUSPEND_TOOL
        && world.program.events.resume
        && let Some(key) = input
            .get(crate::run::engine::SUSPEND_KEY)
            .and_then(Json::as_str)
    {
        let answered = world
            .parking
            .sent
            .iter()
            .any(|(sent_turn, sent_seq, sent_key, answer)| {
                *sent_turn == turn
                    && *sent_seq == seq
                    && sent_key == key
                    && value.as_str() == Some(answer.as_str())
            });
        if !answered {
            return Err(fault(
                Invariant::ResumeMatched,
                format!(
                    "suspend {id} ({key}) records {}, which no resume with its key answered",
                    value.encode()
                ),
            ));
        }
    }
    Ok(())
}
