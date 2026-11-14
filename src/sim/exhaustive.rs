use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::ops::Range;

use crate::effect::EffectClass;
use crate::ir::{Program, Type};
use crate::run::engine::{
    Answer, Exit, Finish, ModelRequest, Point, Read, Stop, ToolRequest, World,
};
use crate::run::journal::{self, Delivery, Output, Record};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::turn::{Ran, TurnRequest, run_turn};
use crate::sim::invariants::{self, Invariant, Violation};
use crate::sim::rng::{FNV_OFFSET, Rng, fnv};
use crate::sim::world::{CLOCK_STEP_MAX, RESTARTS_PER_TURN_MAX, SimWorld, TOOL_FAILURE_PPM};
use crate::sim::{RUNS_PER_TURN_MAX, TURNS_PER_SESSION_MAX, generate};
use crate::target::medium::Medium;
use crate::target::{BLOCK_DEVICE_SECTOR_BYTES, Target};

pub const FAULTS_PER_SESSION_DEFAULT: u64 = 1;
pub const UNSYNCED_SECTORS_MAX: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cuts {
    Lines,
    Bytes,
}

impl Cuts {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "lines" => Some(Self::Lines),
            "bytes" => Some(Self::Bytes),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Lines => "lines",
            Self::Bytes => "bytes",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub target: Target,
    pub seeds: Range<u64>,
    pub mutation: Mutation,
    pub faults_per_session: u64,
    pub cuts: Cuts,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub seeds: u64,
    pub turns: u64,
    pub states: u64,
    pub duplicates: u64,
    pub runs: u64,
    pub sessions: u64,
    pub crashes: u64,
    pub torn: u64,
    pub takeovers: u64,
    pub fenced: u64,
    pub flipped_outcomes: u64,
    pub key_bytes: u64,
    pub unsynced_sectors_max: u64,
    pub choices_per_run_max: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stopped {
    Violation {
        seed: u64,
        violation: Violation,
        path: Vec<String>,
    },
    Exhausted {
        seed: u64,
        detail: String,
    },
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub stats: Stats,
    pub stopped: Option<Stopped>,
}

struct Session<'a> {
    program: &'a Program,
    target: Target,
    mutation: Mutation,
    seed: u64,
    requests: Vec<TurnRequest>,
    cuts: Cuts,
}

#[derive(Clone)]
struct State<'a> {
    sim: SimWorld<'a>,
    model_calls: BTreeMap<(u64, u64), u64>,
    clock: u64,
    randoms: u64,
    turn: u64,
    runs: u64,
    faults_left: u64,
    epoch: u64,
    records: Vec<Record>,
    path: Vec<String>,
}

#[derive(Clone, Debug)]
struct Made {
    taken: usize,
    count: usize,
    medium: u64,
    label: Option<String>,
}

#[derive(Default)]
struct Choices {
    forced: Vec<usize>,
    made: Vec<Made>,
    previous: Vec<Made>,
    mismatch: Option<String>,
}

impl Choices {
    fn choose(&mut self, count: usize, medium: u64) -> usize {
        assert!(count > 1, "a choice has at least two alternatives");
        let index = self.made.len();
        if index < self.forced.len() {
            let previous = &self.previous[index];
            if self.mismatch.is_none() && (previous.count != count || previous.medium != medium) {
                self.mismatch = Some(format!(
                    "replaying choice {index} of a run met {count} alternatives over a journal with hash {medium:016x}; the first run met {} over {:016x}",
                    previous.count, previous.medium
                ));
            }
        }
        let taken = self.forced.get(index).copied().unwrap_or(0).min(count - 1);
        self.made.push(Made {
            taken,
            count,
            medium,
            label: None,
        });
        taken
    }

    fn label(&mut self, text: String) {
        let made = self.made.last_mut().expect("a label follows a choice");
        assert!(made.taken > 0, "only a fault or a failure is labelled");
        made.label = Some(text);
    }

    fn advance(&mut self) -> bool {
        let mut made = std::mem::take(&mut self.made);
        self.previous.clone_from(&made);
        while made.last().is_some_and(|last| last.taken + 1 == last.count) {
            made.pop();
        }
        let Some(last) = made.last_mut() else {
            return false;
        };
        last.taken += 1;
        self.forced = made.iter().map(|made| made.taken).collect();
        true
    }

    fn labels(&self) -> Vec<String> {
        self.made
            .iter()
            .filter_map(|made| made.label.clone())
            .collect()
    }
}

#[derive(Clone, Debug)]
enum Torn {
    Cut(usize),
    Sectors(Vec<bool>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    Crash,
    Torn,
}

struct Run<'s, 'a> {
    session: &'s Session<'a>,
    state: State<'a>,
    choices: &'s mut Choices,
    nested: bool,
    fault: Option<Fault>,
    takeovers: u64,
    flipped_outcomes: u64,
    unsynced_sectors_max: u64,
    exhausted: Option<String>,
}

impl Run<'_, '_> {
    fn medium_hash(&self) -> u64 {
        fnv(FNV_OFFSET, &self.state.sim.medium.bytes())
    }

    fn spend(&mut self) {
        assert!(
            self.state.faults_left > 0,
            "a fault spends the session's budget"
        );
        self.state.faults_left -= 1;
    }

    fn crash(&mut self, fault: Fault, label: String) -> Stop {
        self.spend();
        self.choices.label(label);
        assert!(self.fault.is_none(), "a run ends at its first crash");
        self.fault = Some(fault);
        Stop::Crash
    }

    fn request(&self) -> TurnRequest {
        self.session.requests[usize::try_from(self.state.turn - 1).expect("turns fit in usize")]
            .clone()
    }

    fn second_owner(&mut self) {
        self.takeovers += 1;
        let request = self.request();
        let session = self.session;
        let Medium::Rows(rows) = &mut self.state.sim.medium else {
            panic!("only the postgres target has leases");
        };
        let epoch = rows.take_over();
        let records = match self.state.sim.medium.open() {
            Ok(records) => records,
            Err(error) => {
                self.state
                    .sim
                    .owner_faults
                    .push(format!("second owner could not read the journal: {error}"));
                return;
            }
        };
        let model = self.state.sim.target;
        assert!(!self.nested, "a second owner runs no third owner");
        self.nested = true;
        let ran = run_turn(
            session.program,
            &model,
            session.mutation,
            self,
            epoch,
            &records,
            &request,
        );
        self.nested = false;
        match ran {
            Ok(Ran::Finished(_)) => {}
            Ok(Ran::AlreadyFinished(finish)) => World::acknowledge(&mut self.state.sim, &finish),
            Err(exit) => self
                .state
                .sim
                .owner_faults
                .push(format!("second owner stopped: {exit:?}")),
        }
    }

    fn torn_outcomes(&mut self, bytes: &[u8]) -> Vec<Torn> {
        let mut cuts = BTreeSet::new();
        match self.session.cuts {
            Cuts::Bytes => cuts.extend(0..=bytes.len()),
            Cuts::Lines => line_cuts(bytes, &mut cuts),
        }
        let mut outcomes: Vec<Torn> = cuts.into_iter().map(Torn::Cut).collect();
        let Medium::Jsonl(file) = &self.state.sim.medium else {
            panic!("only JSONL writes tear");
        };
        assert_eq!(file.unsynced(), 0, "every flush starts from a synced file");
        let first = file.synced / BLOCK_DEVICE_SECTOR_BYTES;
        let end = (file.written.len() + bytes.len()).div_ceil(BLOCK_DEVICE_SECTOR_BYTES);
        let sectors = end - first;
        self.unsynced_sectors_max = self.unsynced_sectors_max.max(sectors as u64);
        if self.session.cuts == Cuts::Bytes && sectors > UNSYNCED_SECTORS_MAX {
            self.exhausted.get_or_insert(format!(
                "a flush leaves {sectors} unsynced sectors, more than unsynced_sectors_max ({UNSYNCED_SECTORS_MAX})"
            ));
            return outcomes;
        }
        outcomes.extend(
            sector_masks(self.session.cuts, sectors)
                .into_iter()
                .map(Torn::Sectors),
        );
        outcomes
    }

    fn flush_jsonl(&mut self, records: &[Record]) -> Result<(), Stop> {
        let checksum = match &self.state.sim.medium {
            Medium::Jsonl(file) => file.checksum,
            Medium::Rows(_) => panic!("flush_jsonl writes a JSONL file"),
        };
        let bytes = journal::encode_lines(records, checksum);
        let outcomes = if self.nested || self.state.faults_left == 0 {
            Vec::new()
        } else {
            self.torn_outcomes(&bytes)
        };
        let taken = if outcomes.is_empty() {
            0
        } else {
            let medium = self.medium_hash();
            self.choices.choose(outcomes.len() + 1, medium)
        };
        let Medium::Jsonl(file) = &mut self.state.sim.medium else {
            unreachable!("checked above");
        };
        let Some(torn) = taken.checked_sub(1).map(|index| outcomes[index].clone()) else {
            file.write(&bytes);
            file.sync();
            return Ok(());
        };
        let label = match &torn {
            Torn::Cut(cut) => {
                file.write(&bytes[..*cut]);
                format!(
                    "crash in a flush of {} bytes after {cut} reached the page cache",
                    bytes.len()
                )
            }
            Torn::Sectors(persisted) => {
                file.write(&bytes);
                file.lose_unsynced_sectors(persisted);
                format!(
                    "power loss after a flush of {} bytes persisting sectors {persisted:?}",
                    bytes.len()
                )
            }
        };
        Err(self.crash(Fault::Torn, label))
    }

    fn flush_rows(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop> {
        let fence = self.session.mutation != Mutation::PostgresNoFence
            && self.session.target == Target::ServerPostgres;
        let Medium::Rows(rows) = &mut self.state.sim.medium else {
            panic!("flush_rows commits rows");
        };
        if fence && rows.fenced(epoch) {
            return Err(Stop::Fenced);
        }
        rows.commit(records);
        Ok(())
    }

    fn outcome_flipped(&mut self, request: &ToolRequest<'_>, id: &str) -> bool {
        if request.effect != EffectClass::External || self.nested || self.state.faults_left == 0 {
            return false;
        }
        let medium = self.medium_hash();
        let flipped = self.choices.choose(2, medium) == 1;
        if flipped {
            self.spend();
            self.flipped_outcomes += 1;
            self.choices.label(format!(
                "external {id} ({}) has the other outcome",
                request.tool
            ));
        }
        flipped
    }
}

fn sector_masks(cuts: Cuts, sectors: usize) -> BTreeSet<Vec<bool>> {
    let mut masks = BTreeSet::new();
    match cuts {
        Cuts::Bytes => {
            assert!(
                sectors <= UNSYNCED_SECTORS_MAX,
                "the caller bounds full subsets"
            );
            for mask in 0..1_u64 << sectors {
                masks.insert((0..sectors).map(|bit| mask >> bit & 1 == 1).collect());
            }
        }
        Cuts::Lines => {
            for lost in 0..sectors {
                for rest in [false, true] {
                    masks.insert(
                        (0..sectors)
                            .map(|index| index < lost || (index > lost && rest))
                            .collect(),
                    );
                }
            }
        }
    }
    masks.remove(&vec![false; sectors]);
    masks.remove(&vec![true; sectors]);
    masks
}

fn line_cuts(bytes: &[u8], cuts: &mut BTreeSet<usize>) {
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != journal::LINE_END {
            continue;
        }
        cuts.insert(start);
        if index > start + 1 {
            cuts.insert(start + (index - start) / 2);
        }
        cuts.insert(index);
        start = index + 1;
    }
    assert_eq!(start, bytes.len(), "encoded lines end with a newline");
    cuts.insert(bytes.len());
}

fn tool_value(rng: &mut Rng, returns: &Type) -> Json {
    match returns {
        Type::U64 => Json::Number(rng.draw() >> 32),
        Type::Bool => Json::Bool(rng.draw() & 1 == 1),
        Type::Text => Json::String(SimWorld::generated_text(rng)),
        other => panic!("the checker admits only scalar tool results, found {other:?}"),
    }
}

impl World for Run<'_, '_> {
    fn hook(&mut self, point: Point) -> Result<(), Stop> {
        if self.nested || self.state.faults_left == 0 {
            return Ok(());
        }
        let lease = self.session.target == Target::ServerPostgres;
        let medium = self.medium_hash();
        match self.choices.choose(if lease { 3 } else { 2 }, medium) {
            0 => Ok(()),
            1 => {
                if let Medium::Jsonl(file) = &self.state.sim.medium {
                    assert_eq!(file.unsynced(), 0, "a hook sees a synced file");
                }
                Err(self.crash(Fault::Crash, format!("crash at {point:?}")))
            }
            _ => {
                self.spend();
                self.choices.label(format!(
                    "lease expires at {point:?}; a second owner runs the turn"
                ));
                self.second_owner();
                Ok(())
            }
        }
    }

    fn flush(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop> {
        match self.state.sim.medium {
            Medium::Jsonl(_) => self.flush_jsonl(records),
            Medium::Rows(_) => self.flush_rows(epoch, records),
        }
    }

    fn attempt(&mut self, turn: u64, seq: u64) -> Result<u64, Stop> {
        World::attempt(&mut self.state.sim, turn, seq)
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        let id = journal::call_id(request.turn, request.seq);
        let flipped = self.outcome_flipped(request, &id);
        let entry = self.state.sim.dispatched.entry(id).or_default();
        entry.tool = request.tool.to_owned();
        if request.effect == EffectClass::External {
            entry.external += 1;
        } else {
            entry.other += 1;
        }
        let count = entry.external + entry.other;
        let seed = self.session.seed;
        let mut rng = Rng::derive_keyed(seed, "tool", &[request.turn, request.seq, count]);
        let failed = rng.chance(TOOL_FAILURE_PPM) != flipped;
        let output = if failed {
            Output::Failed(format!(
                "{} failed: {}",
                request.tool,
                SimWorld::generated_text(&mut rng)
            ))
        } else {
            Output::Ok(tool_value(&mut rng, request.returns))
        };
        if request.effect == EffectClass::External {
            entry.outputs.push(output.clone());
        }
        output
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        *self
            .state
            .model_calls
            .entry((request.turn, request.seq))
            .or_insert(0) += 1;
        World::model(&mut self.state.sim, request)
    }

    fn now(&mut self) -> u64 {
        let mut rng = Rng::derive_keyed(self.session.seed, "clock", &[self.state.clock]);
        self.state.clock += rng.range(1, CLOCK_STEP_MAX);
        self.state.clock
    }

    fn random(&mut self) -> u64 {
        self.state.randoms += 1;
        Rng::derive_keyed(self.session.seed, "random", &[self.state.randoms]).draw()
    }

    fn acknowledge(&mut self, finish: &Finish) {
        World::acknowledge(&mut self.state.sim, finish);
    }

    fn inbox(&mut self, turn: u64) -> Vec<Delivery> {
        World::inbox(&mut self.state.sim, turn)
    }

    fn answer(&mut self, turn: u64, position: u64, answers: &[(String, Answer)]) {
        World::answer(&mut self.state.sim, turn, position, answers);
    }

    fn read(&mut self, read: &Read) {
        World::read(&mut self.state.sim, read);
    }
}

fn key(state: &State<'_>) -> Vec<u8> {
    let mut text = String::new();
    let mut put = |part: std::fmt::Arguments<'_>| {
        text.write_fmt(part).expect("writing to a string succeeds");
        text.push('\u{0}');
    };
    put(format_args!(
        "{} {} {} {} {} {}",
        state.turn, state.runs, state.faults_left, state.epoch, state.clock, state.randoms
    ));
    put(format_args!("{:?}", state.model_calls));
    put(format_args!("{:?}", state.sim.dispatched));
    put(format_args!("{:?}", state.sim.acks));
    put(format_args!("{:?}", state.sim.ack_faults));
    put(format_args!("{:?}", state.sim.owner_faults));
    if let Medium::Rows(rows) = &state.sim.medium {
        put(format_args!("{} {:?}", rows.epoch, rows.attempts));
    }
    let mut bytes = text.into_bytes();
    bytes.extend_from_slice(&state.sim.medium.bytes());
    bytes
}

fn workload(program: &Program, seed: u64) -> Vec<TurnRequest> {
    let mut workload = Rng::derive(seed, "workload");
    let turns = workload.range(1, TURNS_PER_SESSION_MAX);
    (1..=turns)
        .map(|turn| {
            let handler = &program.handlers[workload.pick(program.handlers.len())];
            let arguments = handler
                .params
                .iter()
                .map(|local| generate(program, &handler.locals[*local].ty, &mut workload, None))
                .collect();
            TurnRequest {
                turn,
                handler: handler.name.clone(),
                arguments,
            }
        })
        .collect()
}

fn liveness(detail: String) -> Violation {
    Violation {
        invariant: Invariant::Liveness,
        detail,
    }
}

fn begin_run(state: &mut State<'_>, target: Target) -> Result<(), Violation> {
    state.runs += 1;
    let records = state.sim.medium.open().map_err(|error| {
        liveness(format!(
            "turn {}: the journal cannot be read: {error}",
            state.turn
        ))
    })?;
    state.epoch = match &mut state.sim.medium {
        Medium::Rows(rows) if target == Target::ServerPostgres => rows.take_over(),
        Medium::Rows(rows) => rows.epoch,
        Medium::Jsonl(_) => 0,
    };
    state.records = records;
    Ok(())
}

fn safety(state: &State<'_>) -> Result<(), Violation> {
    let records = state
        .sim
        .medium
        .durable()
        .map_err(|error| liveness(format!("the durable journal cannot be read: {error}")))?;
    invariants::safety(&state.sim, &records).map(|_| ())
}

fn specific(state: &State<'_>, violation: Violation) -> Violation {
    safety(state).err().unwrap_or(violation)
}

struct Search<'s, 'a> {
    session: &'s Session<'a>,
    stack: Vec<State<'a>>,
    seen: BTreeSet<Vec<u8>>,
    stats: Stats,
}

impl<'a> Search<'_, 'a> {
    fn fail(&self, state: &State<'_>, violation: Violation) -> Stopped {
        Stopped::Violation {
            seed: self.session.seed,
            violation,
            path: state.path.clone(),
        }
    }

    fn admit(&mut self, mut state: State<'a>) -> Result<(), Stopped> {
        if let Err(violation) = begin_run(&mut state, self.session.target) {
            let violation = specific(&state, violation);
            return Err(self.fail(&state, violation));
        }
        let key = key(&state);
        if self.seen.contains(&key) {
            self.stats.duplicates += 1;
            return Ok(());
        }
        if let Err(violation) = safety(&state) {
            return Err(self.fail(&state, violation));
        }
        self.stats.states += 1;
        self.stats.key_bytes += key.len() as u64;
        self.seen.insert(key);
        self.stack.push(state);
        Ok(())
    }

    fn finish_session(&mut self, state: &State<'a>) -> Result<(), Stopped> {
        let turns = self.session.requests.len() as u64;
        let records = state.sim.medium.durable().map_err(|error| {
            self.fail(
                state,
                liveness(format!("the final journal cannot be read: {error}")),
            )
        })?;
        let checked = invariants::check(self.session.program, &state.sim, turns, &records);
        self.stats.sessions += 1;
        checked.map_err(|violation| self.fail(state, violation))
    }

    fn next_turn(&mut self, mut state: State<'a>) -> Result<(), Stopped> {
        if state.turn == self.session.requests.len() as u64 {
            return self.finish_session(&state);
        }
        state.turn += 1;
        state.runs = 0;
        self.admit(state)
    }

    fn after_run(&mut self, mut state: State<'a>, ran: Result<Ran, Exit>) -> Result<(), Stopped> {
        match ran {
            Ok(Ran::Finished(_)) => self.next_turn(state),
            Ok(Ran::AlreadyFinished(finish)) => {
                World::acknowledge(&mut state.sim, &finish);
                self.next_turn(state)
            }
            Err(Exit::Stop(_)) if state.runs == RUNS_PER_TURN_MAX => {
                let violation = specific(
                    &state,
                    liveness(format!(
                        "turn {} did not finish within runs_per_turn_max ({RUNS_PER_TURN_MAX})",
                        state.turn
                    )),
                );
                Err(self.fail(&state, violation))
            }
            Err(Exit::Stop(_)) => self.admit(state),
            Err(Exit::Corrupt(error)) => {
                let violation = specific(&state, liveness(format!("turn {}: {error}", state.turn)));
                Err(self.fail(&state, violation))
            }
        }
    }

    fn execute(
        &mut self,
        start: &State<'a>,
        choices: &mut Choices,
    ) -> Result<(State<'a>, Result<Ran, Exit>), Stopped> {
        let session = self.session;
        let request = session.requests[usize::try_from(start.turn - 1).expect("turns fit")].clone();
        let mut run = Run {
            session,
            state: start.clone(),
            choices,
            nested: false,
            fault: None,
            takeovers: 0,
            flipped_outcomes: 0,
            unsynced_sectors_max: 0,
            exhausted: None,
        };
        let model = run.state.sim.target;
        let epoch = run.state.epoch;
        let records = std::mem::take(&mut run.state.records);
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_turn(
                session.program,
                &model,
                session.mutation,
                &mut run,
                epoch,
                &records,
                &request,
            )
        }));
        self.count(&run, &ran);
        let labels = run.choices.labels();
        let mut state = run.state;
        if !labels.is_empty() {
            state.path.push(format!(
                "turn {} run {}: {}",
                state.turn,
                state.runs,
                labels.join("; ")
            ));
        }
        if let Some(detail) = run.exhausted {
            return Err(Stopped::Exhausted {
                seed: session.seed,
                detail,
            });
        }
        match ran {
            Ok(ran) => Ok((state, ran)),
            Err(payload) => Err(self.fail(&state, panic_violation(payload.as_ref()))),
        }
    }

    fn count(&mut self, run: &Run<'_, '_>, ran: &std::thread::Result<Result<Ran, Exit>>) {
        self.stats.runs += 1;
        self.stats.takeovers += run.takeovers;
        self.stats.flipped_outcomes += run.flipped_outcomes;
        self.stats.unsynced_sectors_max = self
            .stats
            .unsynced_sectors_max
            .max(run.unsynced_sectors_max);
        self.stats.choices_per_run_max = self
            .stats
            .choices_per_run_max
            .max(run.choices.made.len() as u64);
        match run.fault {
            Some(Fault::Crash) => self.stats.crashes += 1,
            Some(Fault::Torn) => self.stats.torn += 1,
            None => {}
        }
        if matches!(ran, Ok(Err(Exit::Stop(Stop::Fenced)))) {
            self.stats.fenced += 1;
        }
    }

    fn expand(&mut self, start: &State<'a>) -> Result<(), Stopped> {
        let mut choices = Choices::default();
        loop {
            let (state, ran) = self.execute(start, &mut choices)?;
            if let Some(detail) = choices.mismatch.take() {
                return Err(self.fail(
                    &state,
                    Violation {
                        invariant: Invariant::ReplayEqual,
                        detail,
                    },
                ));
            }
            self.after_run(state, ran)?;
            if !choices.advance() {
                return Ok(());
            }
        }
    }
}

fn panic_violation(payload: &(dyn std::any::Any + Send)) -> Violation {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_owned());
    Violation {
        invariant: Invariant::Panic,
        detail,
    }
}

fn explore_seed(program: &Program, options: &Options, seed: u64) -> Summary {
    let session = Session {
        program,
        target: options.target,
        mutation: options.mutation,
        seed,
        requests: workload(program, seed),
        cuts: options.cuts,
    };
    let mut search = Search {
        session: &session,
        stack: Vec::new(),
        seen: BTreeSet::new(),
        stats: Stats {
            seeds: 1,
            turns: session.requests.len() as u64,
            ..Stats::default()
        },
    };
    let first = State {
        sim: SimWorld::new(program, options.target, options.mutation, seed, false),
        model_calls: BTreeMap::new(),
        clock: 0,
        randoms: 0,
        turn: 1,
        runs: 0,
        faults_left: options.faults_per_session,
        epoch: 0,
        records: Vec::new(),
        path: Vec::new(),
    };
    let mut stopped = search.admit(first).err();
    while stopped.is_none() {
        let Some(state) = search.stack.pop() else {
            break;
        };
        stopped = search.expand(&state).err();
    }
    Summary {
        stats: search.stats,
        stopped,
    }
}

fn add(total: &mut Stats, more: &Stats) {
    total.seeds += more.seeds;
    total.turns += more.turns;
    total.states += more.states;
    total.duplicates += more.duplicates;
    total.runs += more.runs;
    total.sessions += more.sessions;
    total.crashes += more.crashes;
    total.torn += more.torn;
    total.takeovers += more.takeovers;
    total.fenced += more.fenced;
    total.flipped_outcomes += more.flipped_outcomes;
    total.key_bytes += more.key_bytes;
    total.unsynced_sectors_max = total.unsynced_sectors_max.max(more.unsynced_sectors_max);
    total.choices_per_run_max = total.choices_per_run_max.max(more.choices_per_run_max);
}

pub fn validate(options: &Options) -> Result<(), String> {
    if options.faults_per_session > RESTARTS_PER_TURN_MAX {
        return Err(format!(
            "--depth {} exceeds restarts_per_turn_max ({RESTARTS_PER_TURN_MAX})",
            options.faults_per_session
        ));
    }
    Ok(())
}

pub fn explore(program: &Program, options: &Options) -> Summary {
    assert!(validate(options).is_ok(), "options are validated first");
    let mut total = Summary {
        stats: Stats::default(),
        stopped: None,
    };
    for seed in options.seeds.clone() {
        let summary = explore_seed(program, options, seed);
        add(&mut total.stats, &summary.stats);
        if summary.stopped.is_some() {
            total.stopped = summary.stopped;
            return total;
        }
    }
    total
}

pub fn report(summary: &Summary, options: &Options, file: &str) -> String {
    let stats = &summary.stats;
    let mut replay = format!(
        "hl sim --exhaustive --depth {} --cuts {} --target {}",
        options.faults_per_session,
        options.cuts.name(),
        options.target.name()
    );
    if options.mutation != Mutation::None {
        replay.push_str(&format!(" --mutation {}", options.mutation.name()));
    }
    let counts = format!(
        "seeds={} turns={} states={} duplicates={} runs={} sessions={} crashes={} torn={} takeovers={} fenced={} flipped_outcomes={} key_bytes={} unsynced_sectors_max={} choices_per_run_max={}",
        stats.seeds,
        stats.turns,
        stats.states,
        stats.duplicates,
        stats.runs,
        stats.sessions,
        stats.crashes,
        stats.torn,
        stats.takeovers,
        stats.fenced,
        stats.flipped_outcomes,
        stats.key_bytes,
        stats.unsynced_sectors_max,
        stats.choices_per_run_max
    );
    match &summary.stopped {
        None => format!(
            "ok exhaustive target={} mutation={} depth={} cuts={} seeds={}..{}: {counts}",
            options.target.name(),
            options.mutation.name(),
            options.faults_per_session,
            options.cuts.name(),
            options.seeds.start,
            options.seeds.end
        ),
        Some(Stopped::Violation {
            seed,
            violation,
            path,
        }) => format!(
            "FAIL invariant={} seed={seed} target={} mutation={} depth={}: {}\npath: {}\nexplored: {counts}\nreplay: {replay} --seeds {seed}..{} {file}",
            violation.invariant.name(),
            options.target.name(),
            options.mutation.name(),
            options.faults_per_session,
            violation.detail,
            if path.is_empty() {
                "no faults".to_owned()
            } else {
                path.join(" | ")
            },
            seed + 1
        ),
        Some(Stopped::Exhausted { seed, detail }) => format!(
            "EXHAUSTED seed={seed} target={}: {detail}\nexplored: {counts}\nreplay: {replay} --seeds {seed}..{} {file}",
            options.target.name(),
            seed + 1
        ),
    }
}
