use std::collections::BTreeMap;

use crate::ir::Program;
use crate::run::engine::{Answer, Read};
use crate::run::journal::{self, Delivery, Inbound, Record};
use crate::run::json::Json;
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;
use crate::sim::world::SimWorld;

pub const EVENT_TURN_PPM: u64 = 500_000;
pub const CANCEL_PPM: u64 = 400_000;
pub const ARRIVAL_HOOKS_MAX: u64 = 48;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scheduled {
    pub turn: u64,
    pub delivery: Delivery,
    pub arrives: u64,
    pub answer: Option<Answer>,
}

type Loaded = Vec<(u64, u64, Delivery)>;

#[derive(Clone, Debug, Default)]
pub struct Inbox {
    pub scheduled: Vec<Scheduled>,
    pub recorded: u64,
    hooks: u64,
    recorded_at: BTreeMap<(u64, String), u64>,
    loaded: Vec<Loaded>,
    faults: Vec<Violation>,
}

fn fault(invariant: Invariant, detail: String) -> Violation {
    Violation { invariant, detail }
}

impl Inbox {
    pub fn schedule(&mut self, program: &Program, seed: u64, turn: u64) {
        self.hooks = 0;
        let events = &program.events;
        if !events.any() {
            return;
        }
        let mut rng = Rng::derive_keyed(seed, "events", &[turn]);
        if !rng.chance(EVENT_TURN_PPM) {
            return;
        }
        let mut deliveries = Vec::new();
        if events.cancel && rng.chance(CANCEL_PPM) {
            deliveries.push(Inbound::Cancel);
        }
        if let Some(bound) = &events.steers {
            let count = rng.below(bound.value + 2);
            for _ in 0..count {
                deliveries.push(Inbound::Steer(SimWorld::generated_text(&mut rng)));
            }
        }
        for (index, inbound) in deliveries.into_iter().enumerate() {
            self.scheduled.push(Scheduled {
                turn,
                delivery: Delivery {
                    id: format!("e{turn}-{index}"),
                    inbound,
                },
                arrives: rng.below(ARRIVAL_HOOKS_MAX),
                answer: None,
            });
        }
    }

    pub fn hook(&mut self) {
        self.hooks += 1;
    }

    pub fn arrived(&self, turn: u64) -> Vec<Delivery> {
        let mut arrived: Vec<&Scheduled> = self
            .scheduled
            .iter()
            .filter(|event| event.turn == turn && event.answer.is_none())
            .filter(|event| event.arrives <= self.hooks)
            .collect();
        arrived.sort_by_key(|event| event.arrives);
        arrived
            .into_iter()
            .map(|event| event.delivery.clone())
            .collect()
    }

    pub fn answer(&mut self, turn: u64, position: u64, answers: &[(String, Answer)]) {
        for (id, answer) in answers {
            let Some(event) = self
                .scheduled
                .iter_mut()
                .find(|event| event.turn == turn && event.delivery.id == *id)
            else {
                self.faults.push(fault(
                    Invariant::EventKept,
                    format!("turn {turn} answered event '{id}', which nobody sent"),
                ));
                continue;
            };
            if let Some(earlier) = event.answer {
                if earlier.delivered() != answer.delivered() {
                    self.faults.push(fault(
                        Invariant::EventKept,
                        format!(
                            "event '{id}' of turn {turn} was answered {} after {}",
                            answer.name(),
                            earlier.name()
                        ),
                    ));
                }
                continue;
            }
            event.answer = Some(*answer);
            if *answer == Answer::Recorded {
                self.recorded += 1;
                self.recorded_at.insert((turn, id.clone()), position);
            }
        }
    }

    pub fn load(&mut self, records: &[Record], nested: bool) {
        let mut loaded = Vec::new();
        for record in records {
            if let Record::Event {
                turn, at, delivery, ..
            } = record
            {
                self.recorded_at.insert((*turn, delivery.id.clone()), *at);
                loaded.push((*turn, *at, delivery.clone()));
            }
        }
        if !nested {
            self.loaded.clear();
        }
        self.loaded.push(loaded);
    }

    pub fn unload(&mut self) {
        assert!(
            self.loaded.len() > 1,
            "a nested run unloads only its own journal"
        );
        self.loaded.pop();
    }

    pub fn read(&mut self, read: &Read) {
        for id in &read.visible {
            match self.recorded_at.get(&(read.turn, id.clone())) {
                Some(at) if *at <= read.position => {}
                Some(at) => self.faults.push(fault(
                    Invariant::EventBeforeRead,
                    format!(
                        "a read of turn {} at seq {} saw event '{id}', recorded at seq {at}",
                        read.turn, read.position
                    ),
                )),
                None => self.faults.push(fault(
                    Invariant::EventBeforeRead,
                    format!(
                        "a read of turn {} at seq {} saw event '{id}', which was never recorded",
                        read.turn, read.position
                    ),
                )),
            }
        }
        if !read.frozen {
            return;
        }
        let loaded = self
            .loaded
            .last()
            .expect("a run loads its journal before it reads");
        let expected: Vec<String> = loaded
            .iter()
            .filter(|(turn, at, delivery)| {
                *turn == read.turn && *at <= read.position && delivery.inbound.name() == read.kind
            })
            .map(|(_, _, delivery)| delivery.id.clone())
            .collect();
        if expected != read.visible {
            self.faults.push(fault(
                Invariant::EventReplayEqual,
                format!(
                    "a replayed read of turn {} at seq {} saw {} events {:?}, its journal holds {:?}",
                    read.turn, read.position, read.kind, read.visible, expected
                ),
            ));
        }
    }

    pub fn end_turn(&mut self, turn: u64) {
        let hooks = self.hooks;
        for event in &mut self.scheduled {
            if event.turn == turn && event.answer.is_none() && event.arrives > hooks {
                event.answer = Some(Answer::Finished);
            }
        }
    }

    pub fn safety(&self, records: &[Record]) -> Result<(), Violation> {
        if let Some(fault) = self.faults.first() {
            return Err(fault.clone());
        }
        let mut seen: BTreeMap<(u64, &str), u64> = BTreeMap::new();
        let mut last_at: BTreeMap<u64, u64> = BTreeMap::new();
        for record in records {
            if let Record::Event {
                turn, at, delivery, ..
            } = record
            {
                let last = last_at.entry(*turn).or_insert(0);
                if *at < *last {
                    return Err(fault(
                        Invariant::EventReplayEqual,
                        format!(
                            "event '{}' of turn {turn} is recorded at seq {at} after an event at seq {last}, so a replay shows it to reads that ran without it",
                            delivery.id
                        ),
                    ));
                }
                *last = *at;
                let count = seen.entry((*turn, delivery.id.as_str())).or_insert(0);
                *count += 1;
                if *count > 1 {
                    return Err(fault(
                        Invariant::EventOnce,
                        format!(
                            "event '{}' of turn {turn} is recorded {count} times",
                            delivery.id
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn kept(&self, records: &[Record]) -> Result<(), Violation> {
        let journal: BTreeMap<(u64, &str), &Delivery> = records
            .iter()
            .filter_map(|record| match record {
                Record::Event { turn, delivery, .. } => {
                    Some(((*turn, delivery.id.as_str()), delivery))
                }
                _ => None,
            })
            .collect();
        for event in &self.scheduled {
            let key = (event.turn, event.delivery.id.as_str());
            let recorded = journal.get(&key);
            let lost = |detail: String| Err(fault(Invariant::EventKept, detail));
            match event.answer {
                None => {
                    return lost(format!(
                        "event '{}' of turn {} arrived and was never answered",
                        event.delivery.id, event.turn
                    ));
                }
                Some(answer) if answer.delivered() && recorded != Some(&&event.delivery) => {
                    return lost(format!(
                        "event '{}' of turn {} was answered {} but the journal holds {}",
                        event.delivery.id,
                        event.turn,
                        answer.name(),
                        recorded.map_or_else(
                            || "no record of it".to_owned(),
                            |delivery| journal::delivery_json(delivery).encode()
                        )
                    ));
                }
                Some(answer) if !answer.delivered() && recorded.is_some() => {
                    return lost(format!(
                        "event '{}' of turn {} was answered {} yet the journal records it",
                        event.delivery.id,
                        event.turn,
                        answer.name()
                    ));
                }
                Some(_) => {}
            }
        }
        for (turn, id) in journal.keys() {
            let sent = self
                .scheduled
                .iter()
                .any(|event| event.turn == *turn && event.delivery.id == *id);
            if !sent {
                return Err(fault(
                    Invariant::EventKept,
                    format!("turn {turn} records event '{id}', which nobody sent"),
                ));
            }
        }
        Ok(())
    }
}

pub fn deliveries_json(deliveries: &[Delivery]) -> Json {
    Json::Array(deliveries.iter().map(journal::delivery_json).collect())
}

pub fn answers_json(answers: &[(String, Answer)]) -> Json {
    Json::Array(
        answers
            .iter()
            .map(|(id, answer)| {
                Json::object([
                    ("answer", Json::text(answer.name())),
                    ("id", Json::text(id)),
                ])
            })
            .collect(),
    )
}
