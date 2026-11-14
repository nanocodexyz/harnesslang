use crate::ir::Program;
use crate::run::blob::Blobs;
use crate::run::cell::{CellEvent, Input, Step, Wait};
use crate::run::engine::{
    End, Engine, Exit, Finish, ListingRequest, Mode, ModelRequest, Point, Read, Resume, Setup,
    Stop, Suspension, ToolRequest, World,
};
use crate::run::journal::{Delivery, Outcome, Output, Record};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::session::{self, Session, TurnLog};
use crate::run::turn::state_before;
use crate::run::value::Value;
use crate::target;

pub const SIDE_TURN: u64 = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideRequest {
    pub handler: String,
    pub arguments: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideAnswer {
    pub outcome: Outcome,
    pub result: Json,
}

pub trait SideHost {
    fn side_tool(&mut self, request: &ToolRequest<'_>) -> Output;
    fn side_model(&mut self, request: &ModelRequest<'_>) -> Json;
}

impl<W: World + ?Sized> SideHost for W {
    fn side_tool(&mut self, request: &ToolRequest<'_>) -> Output {
        self.tool(request)
    }

    fn side_model(&mut self, request: &ModelRequest<'_>) -> Json {
        self.model(request)
    }
}

struct Scratch<'w, H: SideHost + ?Sized> {
    host: &'w mut H,
    records: Vec<Record>,
    answered: Option<Finish>,
}

const REFUSED: &str = "the checker refuses this effect in a side handler";

impl<H: SideHost + ?Sized> World for Scratch<'_, H> {
    fn hook(&mut self, _point: Point) -> Result<(), Stop> {
        Ok(())
    }

    fn flush(&mut self, _epoch: u64, records: &[Record]) -> Result<(), Stop> {
        self.records.extend_from_slice(records);
        Ok(())
    }

    fn attempt(&mut self, _turn: u64, _seq: u64) -> Result<u64, Stop> {
        panic!("{REFUSED}: a side question runs no external call")
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        assert_eq!(
            request.turn, SIDE_TURN,
            "a side question's tool calls carry the side turn"
        );
        assert!(
            request.nested.is_none(),
            "{REFUSED}: a side question runs no cell"
        );
        self.host.side_tool(request)
    }

    fn listing(&mut self, _request: &ListingRequest<'_>) -> Output {
        panic!("{REFUSED}: a side question reads no listing")
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        assert_eq!(
            request.turn, SIDE_TURN,
            "a side question's model calls carry the side turn"
        );
        self.host.side_model(request)
    }

    fn now(&mut self) -> u64 {
        panic!("{REFUSED}: now")
    }

    fn random(&mut self) -> u64 {
        panic!("{REFUSED}: random")
    }

    fn acknowledge(&mut self, finish: &Finish) {
        assert!(self.answered.is_none(), "a side question is answered once");
        self.answered = Some(finish.clone());
    }

    fn cell_start(&mut self, _cell: u64, _script: &str) -> Step {
        panic!("{REFUSED}: a code cell")
    }

    fn cell_resume(&mut self, _cell: u64, _input: Input) -> Step {
        panic!("{REFUSED}: a code cell")
    }

    fn nested_attempt(
        &mut self,
        _turn: u64,
        _seq: u64,
        _nested: u64,
        _first: bool,
    ) -> Result<u64, Stop> {
        panic!("{REFUSED}: a code cell")
    }

    fn wait(&mut self, _wait: &Wait) -> Result<(), Stop> {
        panic!("{REFUSED}: a code cell's timer")
    }

    fn waited(&mut self, _wait: &Wait) -> bool {
        panic!("{REFUSED}: a code cell's timer")
    }

    fn observe(&mut self, _event: &CellEvent) {
        panic!("{REFUSED}: a code cell")
    }

    fn clock(&mut self) -> u64 {
        panic!("{REFUSED}: wait")
    }

    fn sleep(&mut self, _turn: u64, _seq: u64, _until_ms: u64) -> Result<u64, Stop> {
        panic!("{REFUSED}: wait")
    }

    fn resume(&mut self, _suspension: &Suspension) -> Option<Resume> {
        panic!("{REFUSED}: suspend")
    }

    fn resumed(&mut self, _suspension: &Suspension, _resume: &Resume, _matched: bool) {
        panic!("{REFUSED}: suspend")
    }

    fn inbox(&mut self, turn: u64) -> Vec<Delivery> {
        assert_eq!(turn, SIDE_TURN, "a side question reads no turn's inbox");
        Vec::new()
    }

    fn answer(
        &mut self,
        _turn: u64,
        _position: u64,
        _answers: &[(String, crate::run::engine::Answer)],
    ) {
        panic!("a side question takes no events, so it answers none")
    }

    fn read(&mut self, _read: &Read) {
        panic!("{REFUSED}: an event read")
    }
}

pub fn run<H: SideHost + ?Sized>(
    setup: Setup<'_>,
    host: &mut H,
    state: Vec<Value>,
    blobs: Blobs,
    request: &SideRequest,
) -> Result<(SideAnswer, Vec<Record>), Exit> {
    let side = setup
        .program
        .side(&request.handler)
        .expect("a side request names a side handler of the program");
    let log = TurnLog::new(side.name.clone(), Json::Null);
    let mut scratch = Scratch {
        host,
        records: Vec::new(),
        answered: None,
    };
    let mode = Mode::Live {
        world: &mut scratch,
        epoch: 0,
    };
    let ran = Engine::new(setup, side, mode, SIDE_TURN, state, &log)
        .with_blobs(blobs)
        .run(&request.arguments)?;
    let End::Finished(finish) = ran.end else {
        panic!("{REFUSED}: a side handler never suspends");
    };
    assert_eq!(
        scratch.answered.as_ref(),
        Some(&finish),
        "a side question's finish is its answer"
    );
    let answer = SideAnswer {
        outcome: finish.outcome,
        result: finish.result,
    };
    Ok((answer, scratch.records))
}

pub fn last_finished(session: &Session) -> u64 {
    session
        .turns
        .iter()
        .rev()
        .find(|(_, log)| log.finished.is_some())
        .map_or(session::ENTRY_REVISION, |(turn, _)| *turn)
}

pub fn snapshot(
    program: &Program,
    target: &target::Model,
    session: &Session,
) -> Result<Vec<Value>, Exit> {
    state_before(program, target, session, last_finished(session) + 1)
}

pub fn answer(
    setup: Setup<'_>,
    world: &mut dyn World,
    epoch: u64,
    records: &[Record],
    request: &SideRequest,
) -> Result<SideAnswer, Exit> {
    let session = session::fold(records).map_err(Exit::Corrupt)?;
    let state = snapshot(setup.program, setup.target, &session)?;
    let blobs = Blobs::of_session(setup.program, &session).map_err(Exit::Corrupt)?;
    let (answer, scratch) = run(setup, world, state, blobs, request)?;
    if setup.mutation == Mutation::SideJournaled {
        world.flush(epoch, &scratch).map_err(Exit::Stop)?;
    }
    Ok(answer)
}
