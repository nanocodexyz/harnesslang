use crate::ir::{IDLE_AT, IDLE_DUE, Program};
use crate::run::journal::Record;
use crate::run::json::Json;
use crate::run::session;
use crate::run::turn::{self, NotDue, TurnRequest};
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;
use crate::sim::world::SimWorld;

pub const IDLE_PREEMPT_PPM: u64 = 300_000;
pub const IDLE_EARLY_PPM: u64 = 150_000;
pub const IDLE_LATE_PPM: u64 = 100_000;
pub const IDLE_LATE_MS_MAX: u64 = 2 * 5 * 60 * 1000;
pub const CACHE_WARM_PPM: u64 = 800_000;
pub const REQUESTS_PER_DUE_MAX: u64 = 2;

pub enum Period {
    Preempted,
    Disarmed,
}

pub enum Requested {
    Admitted,
    Refused(NotDue),
}

fn fault(detail: String) -> Violation {
    Violation {
        invariant: Invariant::IdleDue,
        detail,
    }
}

impl SimWorld<'_> {
    pub fn idle_turn(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|request| self.program.idles(&request.handler))
    }

    pub fn warm_usage(&self, rng: &mut Rng, usage: &mut [Json]) {
        if !self.idle_turn() || !rng.chance(CACHE_WARM_PPM) {
            return;
        }
        let [read, written] = usage else {
            panic!("a cache's usage is its read and written tokens");
        };
        if read.as_u64() == Some(0) {
            *read = Json::Number(1);
        }
        *written = Json::Number(0);
    }

    pub fn refused(&mut self, request: &TurnRequest, refusal: &NotDue) {
        self.stats.turns -= 1;
        self.stats.idle_refused += 1;
        self.idle_log.push((request.turn, refusal.clone()));
    }

    fn armed_due(&self) -> Result<Option<u64>, Violation> {
        let records = self.medium.durable().map_err(|error| Violation {
            invariant: Invariant::Liveness,
            detail: format!("the journal cannot be read between turns: {error}"),
        })?;
        turn::idle_due(self.program, &self.target, &records).map_err(|exit| Violation {
            invariant: Invariant::Liveness,
            detail: format!("the idle turn's due cannot be read: {exit:?}"),
        })
    }
}

pub fn period(
    world: &mut SimWorld<'_>,
    next: u64,
    last: bool,
    mut request: impl FnMut(&mut SimWorld<'_>, TurnRequest) -> Result<Requested, Violation>,
) -> Result<(u64, Period), Violation> {
    let program = world.program;
    let idle = program
        .idle
        .as_ref()
        .expect("a period runs for an idle handler");
    let requests_max = REQUESTS_PER_DUE_MAX * (idle.bound.value + 1);
    let mut rng = Rng::derive_keyed(world.seed, "idle", &[next]);
    let mut turn = next;
    let mut early_for = None;
    for _ in 0..requests_max {
        let Some(due) = world.armed_due()? else {
            return Ok((turn, Period::Disarmed));
        };
        if !last && rng.chance(IDLE_PREEMPT_PPM) {
            if due > world.clock {
                world.clock += rng.below(due - world.clock);
            }
            return Ok((turn, Period::Preempted));
        }
        if early_for != Some(due) && rng.chance(IDLE_EARLY_PPM) {
            early_for = Some(due);
        } else {
            let Some(woke) = world
                .clock
                .max(due)
                .checked_add(if rng.chance(IDLE_LATE_PPM) {
                    rng.range(1, IDLE_LATE_MS_MAX)
                } else {
                    0
                })
            else {
                return Ok((turn, Period::Disarmed));
            };
            world.clock = woke;
        }
        let asked = TurnRequest {
            turn,
            handler: idle.handler.clone(),
            arguments: Vec::new(),
        };
        if let Requested::Admitted = request(world, asked)? {
            world.stats.idle_turns += 1;
            turn += 1;
        }
    }
    match world.armed_due()? {
        None => Ok((turn, Period::Disarmed)),
        Some(due) => Err(fault(format!(
            "an idle turn is still armed for {due} after {requests_max} requests: REQUESTS_PER_DUE_MAX ({REQUESTS_PER_DUE_MAX}) for each of {} ({}) idle turns and the one after",
            idle.bound.name, idle.bound.value
        ))),
    }
}

pub fn check(program: &Program, world: &SimWorld<'_>, records: &[Record]) -> Result<(), Violation> {
    let Some(idle) = &program.idle else {
        return Ok(());
    };
    for (turn, refusal) in &world.idle_log {
        if let NotDue::Early { due, clock } = refusal
            && clock >= due
        {
            return Err(fault(format!(
                "idle turn {turn} was refused as early at clock {clock}, at or after its due {due}"
            )));
        }
    }
    let Ok(session) = session::fold(records) else {
        return Ok(());
    };
    let Ok(mut state) = turn::entry_state(program, &session) else {
        return Ok(());
    };
    let mut since_other = 0;
    for (number, log) in &session.turns {
        if program.idles(&log.handler) {
            since_other += 1;
            let at = log.arguments.get(IDLE_AT).and_then(Json::as_u64);
            let recorded = log.arguments.get(IDLE_DUE).and_then(Json::as_u64);
            let (Some(at), Some(recorded)) = (at, recorded) else {
                return Err(fault(format!(
                    "idle turn {number} is admitted with {}, which names no clock reading and due",
                    log.arguments.encode()
                )));
            };
            if at < recorded {
                return Err(fault(format!(
                    "idle turn {number} was admitted at clock {at}, before its due {recorded}"
                )));
            }
            match turn::armed(program, &state) {
                Some(due) if due == recorded => {}
                armed => {
                    return Err(fault(format!(
                        "idle turn {number} ran for due {recorded}, where its state armed {armed:?}"
                    )));
                }
            }
            if since_other > idle.bound.value {
                return Err(fault(format!(
                    "idle turn {number} is idle turn {since_other} after the last other turn, past {} ({})",
                    idle.bound.name, idle.bound.value
                )));
            }
        } else {
            since_other = 0;
        }
        if log.finished.is_none() {
            break;
        }
        let Ok(replayed) = turn::replay(program, &world.target, *number, log, state) else {
            return Ok(());
        };
        state = replayed;
    }
    Ok(())
}
