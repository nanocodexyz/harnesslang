use crate::ir::{self, ForkAt, Program};
use crate::run::engine::{Setup, ToolRequest};
use crate::run::fork;
use crate::run::journal::{Output, Record};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::session;
use crate::run::turn::TurnRequest;
use crate::sim::invariants::{self, Invariant, Violation};
use crate::sim::rng::Rng;
use crate::sim::world::{CHILD_SESSIONS_PER_SEED_MAX, Child, SimWorld, Stats};
use crate::sim::{Placement, generate, request_turn};

pub const FORK_CHILD_TURNS_MAX: u64 = 2;
pub const FORK_TURN_PAST_PPM: u64 = 250_000;
pub const FORKED_ENTRY_PPM: u64 = 250_000;
pub const FORKED_ENTRY_ORIGIN: &str = "host";
pub const FORKED_ENTRY_DISCARDED_MAX: u64 = 2;

pub fn forked_entry(
    program: &Program,
    target: &crate::target::Model,
    seed: u64,
) -> Option<[Record; 2]> {
    if !program.forks() && program.forked.is_none() {
        return None;
    }
    let mut rng = Rng::derive(seed, "forked entry");
    if !rng.chance(FORKED_ENTRY_PPM) {
        return None;
    }
    let selected = fork::Selected {
        turn: session::ENTRY_REVISION,
        discarded: rng.below(FORKED_ENTRY_DISCARDED_MAX),
    };
    let setup = Setup {
        program,
        target,
        mutation: Mutation::None,
    };
    let payload = fork::entry_payload(
        setup,
        &session::Session::default(),
        &selected,
        FORKED_ENTRY_ORIGIN,
        None,
    )
    .expect("the entry state replays no turn");
    Some(
        fork::records_of(
            program,
            &fork::input(&selected, FORKED_ENTRY_ORIGIN, payload),
        )
        .expect("a fork's input holds its records"),
    )
}

fn unequal(detail: String) -> Violation {
    Violation {
        invariant: Invariant::ForkStateEqual,
        detail,
    }
}

impl SimWorld<'_> {
    pub(crate) fn fork_turn_input(
        &self,
        rng: &mut Rng,
        tool: &str,
        turn: u64,
        input: String,
    ) -> String {
        if self.program.fork_of(tool) != Some(ForkAt::Turn) {
            return input;
        }
        let Ok(Json::Object(mut fields)) = crate::run::json::decode(&input) else {
            return input;
        };
        if !fields.contains_key(ir::FORK_TURN) {
            return input;
        }
        let drawn = if rng.chance(FORK_TURN_PAST_PPM) {
            turn.saturating_add(rng.below(turn.max(1)))
        } else {
            rng.below(turn)
        };
        fields.insert(ir::FORK_TURN.to_owned(), Json::Number(drawn));
        Json::Object(fields).encode()
    }

    fn fork_expected(
        &self,
        at: ForkAt,
        request: &ToolRequest<'_>,
        key: &str,
    ) -> Result<[Record; 2], Violation> {
        let records = self.medium.durable().map_err(|error| {
            unequal(format!(
                "the parent's journal cannot be read at the fork: {error}"
            ))
        })?;
        let at_call = records
            .iter()
            .position(|record| {
                matches!(record, Record::Call { turn, seq, .. } if *turn == request.turn && *seq == request.seq)
            })
            .ok_or_else(|| {
                unequal(format!(
                    "fork {} at turn {} seq {} reached the host before its call record was durable",
                    request.tool, request.turn, request.seq
                ))
            })?;
        let session = session::fold(&records[..at_call]).map_err(|error| {
            unequal(format!(
                "the parent's journal does not fold at the fork: {error}"
            ))
        })?;
        let requested = fork::requested(at, request.input);
        let selected = fork::select(&session, requested).map_err(|turn| {
            unequal(format!(
                "fork {} at turn {} seq {} reached the host, but turn {turn} is not a finished turn of the parent's durable journal",
                request.tool, request.turn, request.seq
            ))
        })?;
        let setup = Setup {
            program: self.program,
            target: &self.target,
            mutation: Mutation::None,
        };
        let payload =
            fork::entry_payload(setup, &session, &selected, key, None).map_err(|exit| {
                unequal(format!(
                    "the parent's durable journal does not replay: {exit:?}"
                ))
            })?;
        fork::records_of(self.program, &fork::input(&selected, key, payload))
            .map_err(|error| unequal(error))
    }

    fn fork_checked(
        &self,
        at: ForkAt,
        request: &ToolRequest<'_>,
        key: &str,
    ) -> Result<[Record; 2], Violation> {
        let expected = self.fork_expected(at, request, key)?;
        let sent = fork::records_of(self.program, request.input).map_err(|error| {
            unequal(format!(
                "fork {} at turn {} seq {}: {error}",
                request.tool, request.turn, request.seq
            ))
        })?;
        for (sent, expected) in sent.iter().zip(&expected) {
            if sent != expected {
                return Err(unequal(format!(
                    "fork {} at turn {} seq {} starts the child with {}",
                    request.tool,
                    request.turn,
                    request.seq,
                    differs(sent, expected)
                )));
            }
        }
        Ok(sent)
    }

    pub(crate) fn fork_child(&mut self, request: &ToolRequest<'_>, output: Output) -> Output {
        let Some(at) = self.program.fork_of(request.tool) else {
            return output;
        };
        if !matches!(output, Output::Ok(_)) {
            return output;
        }
        let key = ir::scoped_spawn_key(&self.scope, request.turn, request.seq);
        if self.children.contains_key(&key) {
            return Output::Ok(Json::String(key));
        }
        let records = match self.fork_checked(at, request, &key) {
            Ok(records) => records,
            Err(violation) => {
                self.cell_faults.push(violation);
                return Output::Ok(Json::String(key));
            }
        };
        if self.family_left == 0 {
            return Output::Failed(format!(
                "{} refused: the host holds child_sessions_per_seed_max ({CHILD_SESSIONS_PER_SEED_MAX}) child sessions",
                request.tool
            ));
        }
        self.family_left -= 1;
        let seed = Rng::derive_keyed(self.seed, "fork", &[request.turn, request.seq]).draw();
        let (result, stats) = run_fork_child(
            self.program,
            &Placement {
                target: self.target.target,
                mutation: self.mutation,
                seed,
                scope: &key,
                family_left: self.family_left,
            },
            &records,
        );
        let descendants = usize::try_from(stats.children + stats.forks)
            .expect("a family's sessions fit usize, bounded by child_sessions_per_seed_max");
        assert!(
            descendants <= self.family_left,
            "a fork starts no more sessions than the family has left"
        );
        self.family_left -= descendants;
        let child_crashes = self.stats.child_crashes + stats.crashes;
        crate::sim::add(&mut self.stats, &stats);
        self.stats.child_crashes = child_crashes;
        self.stats.forks += 1;
        if let Err(violation) = &result {
            self.cell_faults.push(Violation {
                invariant: violation.invariant,
                detail: format!("forked session {key}: {}", violation.detail),
            });
        }
        self.children.insert(
            key.clone(),
            Child {
                result,
                turn: request.turn,
                tool: request.tool.to_owned(),
                sessions: 1,
            },
        );
        Output::Ok(Json::String(key))
    }
}

pub fn run_fork_child(
    program: &Program,
    placement: &Placement<'_>,
    entry: &[Record; 2],
) -> (Result<String, Violation>, Stats) {
    let mut world = SimWorld::new(
        program,
        placement.target,
        placement.mutation,
        placement.seed,
        true,
    );
    world.scope = placement.scope.to_owned();
    world.family_left = placement.family_left;
    for record in entry {
        world.seed_entry(record);
    }
    let faulty = world.profile.crash_ppm > 0 || world.profile.lease_ppm > 0;
    let mut workload = Rng::derive(placement.seed, "workload");
    let turns = workload.range(1, FORK_CHILD_TURNS_MAX);
    let prompted = program.prompted();
    let mut result = Ok(String::new());
    for turn in 1..=turns {
        let handler = prompted[workload.pick(prompted.len())];
        let arguments = handler
            .params
            .iter()
            .map(|local| generate(program, &handler.locals[*local].ty, &mut workload, None))
            .collect();
        let request = TurnRequest {
            turn,
            handler: handler.name.clone(),
            arguments,
        };
        if let Err(violation) = request_turn(program, &mut world, faulty, request) {
            result = Err(violation);
            break;
        }
    }
    let result = result.and_then(|_| {
        let records = world
            .medium
            .durable()
            .map_err(|error| unequal(format!("the forked session's journal cannot be read: {error}")))?;
        if records.get(..entry.len()) != Some(entry.as_slice()) {
            return Err(unequal(
                "the forked session's journal does not start with its forked record and entry checkpoint"
                    .to_owned(),
            ));
        }
        invariants::check(program, &world, turns, &records)?;
        Ok(records
            .iter()
            .rev()
            .find_map(|record| match record {
                Record::Status { result, .. } => Some(result.encode()),
                _ => None,
            })
            .unwrap_or_default())
    });
    (result, world.stats.clone())
}

fn shown(json: &Json) -> String {
    match json {
        Json::Array(items) => format!("{} entries", items.len()),
        other => other.encode(),
    }
}

fn differs(sent: &Record, expected: &Record) -> String {
    let (Record::Checkpoint { payload: left, .. }, Record::Checkpoint { payload: right, .. }) =
        (sent, expected)
    else {
        return format!(
            "{}, where the parent's durable journal gives {}",
            crate::run::journal::to_json(sent).encode(),
            crate::run::journal::to_json(expected).encode()
        );
    };
    let (Ok(Json::Object(left)), Ok(Json::Object(right))) = (
        crate::run::json::decode(left),
        crate::run::json::decode(right),
    ) else {
        return "an entry checkpoint that is not a state object".to_owned();
    };
    let field = left
        .keys()
        .chain(right.keys())
        .find(|key| left.get(*key) != right.get(*key));
    match field {
        Some(field) => format!(
            "'{field}' = {} in its entry checkpoint, where the hook over the parent's durable journal gives {}",
            left.get(field).map_or_else(|| "nothing".to_owned(), shown),
            right.get(field).map_or_else(|| "nothing".to_owned(), shown)
        ),
        None => "an entry checkpoint encoded otherwise".to_owned(),
    }
}
