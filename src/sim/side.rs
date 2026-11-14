use crate::ir::{self, Program, Type};
use crate::run::blob::Blobs;
use crate::run::engine::{Exit, ModelRequest, Setup, ToolRequest};
use crate::run::journal::{self, Output, Record};
use crate::run::json::Json;
use crate::run::mutation::Mutation;
use crate::run::session;
use crate::run::side::{self, SIDE_TURN, SideAnswer, SideHost, SideRequest};
use crate::run::turn;
use crate::run::value;
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;
use crate::sim::world::{SimWorld, TOOL_FAILURE_PPM};
use crate::target::medium::Medium;

pub const SIDE_AT_EFFECT_PPM: u64 = 100_000;
pub const SIDE_BETWEEN_TURNS_PPM: u64 = 500_000;
pub const SIDE_PARKED_PPM: u64 = 500_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SideCall {
    Model {
        seq: u64,
        mode: Option<String>,
        context: Json,
        response: Json,
    },
    Tool {
        seq: u64,
        tool: String,
        input: Json,
        output: Output,
    },
}

impl SideCall {
    fn describe(&self) -> String {
        match self {
            Self::Model { seq, context, .. } => format!(
                "a model call at seq {seq} over {} context entries (crc {})",
                match context {
                    Json::Array(entries) => entries.len(),
                    _ => 0,
                },
                crate::run::crc::crc32(context.encode().as_bytes())
            ),
            Self::Tool { seq, tool, .. } => format!("tool {tool} at seq {seq}"),
        }
    }
}

fn described(call: Option<&SideCall>) -> String {
    call.map_or_else(|| "nothing".to_owned(), SideCall::describe)
}

#[derive(Clone, Debug)]
struct Open {
    at: String,
    medium: Medium,
    buffered: Vec<Record>,
    durable: Vec<Record>,
    turn: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Sides {
    pub asked: u64,
    polls: u64,
    calls: Vec<SideCall>,
    open: Option<Open>,
    pub faults: Vec<Violation>,
}

fn isolated(detail: String) -> Violation {
    Violation {
        invariant: Invariant::SideIsolated,
        detail,
    }
}

fn context_json(program: &Program, request: &ModelRequest<'_>) -> Json {
    Json::Array(
        request
            .context
            .as_list()
            .iter()
            .map(|entry| value::to_json(program, &Type::Sum(ir::ENTRY), entry))
            .collect(),
    )
}

struct Replay<'c> {
    program: &'c Program,
    calls: &'c [SideCall],
    next: usize,
    mismatch: Option<String>,
}

impl Replay<'_> {
    fn take(&mut self, asked: &SideCall) -> Option<&SideCall> {
        let recorded = self.calls.get(self.next);
        self.next += 1;
        let same = match (recorded, asked) {
            (
                Some(SideCall::Model {
                    seq, mode, context, ..
                }),
                SideCall::Model {
                    seq: asked_seq,
                    mode: asked_mode,
                    context: asked_context,
                    ..
                },
            ) => seq == asked_seq && mode == asked_mode && context == asked_context,
            (
                Some(SideCall::Tool {
                    seq, tool, input, ..
                }),
                SideCall::Tool {
                    seq: asked_seq,
                    tool: asked_tool,
                    input: asked_input,
                    ..
                },
            ) => seq == asked_seq && tool == asked_tool && input == asked_input,
            _ => false,
        };
        if !same && self.mismatch.is_none() {
            self.mismatch = Some(format!(
                "request {} of the oracle's run is {}, where the runtime's was {}",
                self.next,
                asked.describe(),
                described(recorded)
            ));
        }
        recorded.filter(|_| same)
    }
}

impl SideHost for Replay<'_> {
    fn side_tool(&mut self, request: &ToolRequest<'_>) -> Output {
        let asked = SideCall::Tool {
            seq: request.seq,
            tool: request.tool.to_owned(),
            input: request.input.clone(),
            output: Output::Failed(String::new()),
        };
        match self.take(&asked) {
            Some(SideCall::Tool { output, .. }) => output.clone(),
            _ => Output::Failed("the oracle's side run diverged".to_owned()),
        }
    }

    fn side_model(&mut self, request: &ModelRequest<'_>) -> Json {
        let asked = SideCall::Model {
            seq: request.seq,
            mode: request.mode.map(str::to_owned),
            context: context_json(self.program, request),
            response: Json::Null,
        };
        match self.take(&asked) {
            Some(SideCall::Model { response, .. }) => response.clone(),
            _ => Json::Null,
        }
    }
}

impl SimWorld<'_> {
    pub(crate) fn side_at_effect(
        &mut self,
        turn: u64,
        seq: u64,
        buffered: &[Record],
    ) -> Option<SideRequest> {
        assert!(
            !self.program.sides.is_empty(),
            "only a program with side handlers is asked side questions"
        );
        self.sides.polls += 1;
        let mut rng = Rng::derive_keyed(self.seed, "side", &[turn, seq, self.sides.polls]);
        if !rng.chance(SIDE_AT_EFFECT_PPM) {
            return None;
        }
        let at = format!("turn {turn} before seq {seq}");
        self.side_open(at, Some((turn, seq)), buffered, &mut rng)
    }

    fn side_open(
        &mut self,
        at: String,
        point: Option<(u64, u64)>,
        buffered: &[Record],
        rng: &mut Rng,
    ) -> Option<SideRequest> {
        let durable = self.medium.durable().ok()?;
        let program = self.program;
        let side = &program.sides[rng.pick(program.sides.len())];
        let arguments = side
            .params
            .iter()
            .map(|local| {
                crate::sim::generate(program, &side.locals[*local].ty, rng, self.edges.as_ref())
            })
            .collect();
        let request = SideRequest {
            handler: side.name.clone(),
            arguments,
        };
        self.sides.asked += 1;
        self.stats.side_questions += 1;
        self.sides.calls.clear();
        assert!(
            self.sides.open.is_none(),
            "one side question is open at a time"
        );
        self.sides.open = Some(Open {
            at,
            medium: self.medium.clone(),
            buffered: buffered.to_vec(),
            durable,
            turn: point.map(|(turn, _)| turn),
        });
        self.record("side", || {
            let number = |value: Option<u64>| value.map_or(Json::Null, Json::Number);
            vec![
                ("turn", number(point.map(|(turn, _)| turn))),
                ("seq", number(point.map(|(_, seq)| seq))),
                ("handler", Json::text(&request.handler)),
                ("arguments", side_arguments(program, &request)),
            ]
        });
        Some(request)
    }

    pub(crate) fn side_answered(
        &mut self,
        request: &SideRequest,
        answer: &Result<SideAnswer, Exit>,
        buffered: &[Record],
    ) {
        let open = self
            .sides
            .open
            .take()
            .expect("a side answer closes the side question it answers");
        self.record("side_answer", || match answer {
            Ok(answer) => vec![
                ("outcome", Json::text(answer.outcome.name())),
                ("result", answer.result.clone()),
            ],
            Err(exit) => vec![("error", Json::String(format!("{exit:?}")))],
        });
        let question = format!(
            "side question {} ({} {})",
            self.sides.asked, request.handler, open.at
        );
        if open.medium != self.medium {
            let fault = isolated(format!(
                "{question} changed the session's journal: {} bytes before it, {} after",
                open.medium.bytes().len(),
                self.medium.bytes().len()
            ));
            self.sides.faults.push(fault);
            return;
        }
        if open.buffered != buffered {
            let fault = isolated(format!(
                "{question} changed the turn's buffered records: {} before it, {} after",
                open.buffered.len(),
                buffered.len()
            ));
            self.sides.faults.push(fault);
            return;
        }
        let answer = match answer {
            Ok(answer) => answer,
            Err(exit) => {
                self.sides
                    .faults
                    .push(isolated(format!("{question} was not answered: {exit:?}")));
                return;
            }
        };
        if let Some(detail) = self.side_oracle(request, &open.durable, open.turn, answer) {
            self.sides
                .faults
                .push(isolated(format!("{question}: {detail}")));
        }
    }

    fn side_oracle(
        &self,
        request: &SideRequest,
        durable: &[Record],
        running: Option<u64>,
        answer: &SideAnswer,
    ) -> Option<String> {
        let program = self.program;
        let mut session = session::fold(durable).ok()?;
        if let Some(running) = running {
            session.turns.retain(|turn, _| *turn < running);
        }
        let state = turn::replay_all(program, &self.target, &session).ok()?;
        let blobs = Blobs::of_session(program, &session).ok()?;
        let mut replay = Replay {
            program,
            calls: &self.sides.calls,
            next: 0,
            mismatch: None,
        };
        let setup = Setup {
            program,
            target: &self.target,
            mutation: Mutation::None,
        };
        let oracle = side::run(setup, &mut replay, state, blobs, request);
        if let Some(mismatch) = replay.mismatch {
            return Some(format!(
                "the snapshot at the last durable status gives another run: {mismatch}"
            ));
        }
        if replay.next != self.sides.calls.len() {
            return Some(format!(
                "the runtime's run made {} host requests, the oracle's over the snapshot at the last durable status {}",
                self.sides.calls.len(),
                replay.next
            ));
        }
        match oracle {
            Ok((oracle, _)) if oracle == *answer => None,
            Ok((oracle, _)) => Some(format!(
                "answered {} {}, where the snapshot at the last durable status answers {} {}",
                answer.outcome.name(),
                answer.result.encode(),
                oracle.outcome.name(),
                oracle.result.encode()
            )),
            Err(exit) => Some(format!(
                "the oracle's run over the snapshot stopped: {exit:?}"
            )),
        }
    }

    pub fn side_between(&mut self, ppm: u64, at: String) -> Result<(), Violation> {
        if self.program.sides.is_empty() {
            return Ok(());
        }
        self.sides.polls += 1;
        let mut rng = Rng::derive_keyed(self.seed, "side between", &[self.sides.polls]);
        if !rng.chance(ppm) || self.medium.durable().is_err() {
            return self.side_fault();
        }
        let epoch = self.run_epoch;
        let bytes = self.trace.as_ref().map(|_| durable_bytes(&self.medium));
        self.opened("side_open", epoch, bytes);
        if let Some(request) = self.side_open(at, None, &[], &mut rng) {
            let program = self.program;
            let target = self.target;
            let setup = Setup {
                program,
                target: &target,
                mutation: self.mutation,
            };
            let durable = self
                .sides
                .open
                .as_ref()
                .expect("opened above")
                .durable
                .clone();
            let answer = side::answer(setup, self, epoch, &durable, &request);
            self.side_answered(&request, &answer, &[]);
        }
        self.side_fault()
    }

    pub fn side_fault(&self) -> Result<(), Violation> {
        match self.sides.faults.first() {
            Some(fault) => Err(fault.clone()),
            None => Ok(()),
        }
    }

    pub(crate) fn side_model_reply(&mut self, request: &ModelRequest<'_>) -> Json {
        assert_eq!(
            request.turn, SIDE_TURN,
            "a side request carries the side turn"
        );
        let mut rng = Rng::derive_keyed(self.seed, "side model", &[self.sides.asked, request.seq]);
        let response = self.reply_from(&mut rng, request);
        let context = context_json(self.program, request);
        self.record("side_model", || {
            let mut fields = vec![
                ("seq", Json::Number(request.seq)),
                (
                    "context_crc",
                    Json::Number(u64::from(crate::run::crc::crc32(
                        context.encode().as_bytes(),
                    ))),
                ),
                ("response", response.clone()),
            ];
            if let Some(mode) = request.mode {
                fields.push((journal::MODEL_MODE, Json::text(mode)));
            }
            fields
        });
        self.sides.calls.push(SideCall::Model {
            seq: request.seq,
            mode: request.mode.map(str::to_owned),
            context,
            response: response.clone(),
        });
        response
    }

    pub(crate) fn side_tool_output(&mut self, request: &ToolRequest<'_>) -> Output {
        assert_eq!(
            request.turn, SIDE_TURN,
            "a side request carries the side turn"
        );
        let mut rng = Rng::derive_keyed(self.seed, "side tool", &[self.sides.asked, request.seq]);
        let output = if rng.chance(TOOL_FAILURE_PPM) {
            Output::Failed(format!(
                "{} failed: {}",
                request.tool,
                Self::generated_text(&mut rng)
            ))
        } else {
            Output::Ok(Self::tool_value(
                self.program,
                &mut rng,
                request.returns,
                self.edges.as_ref(),
            ))
        };
        self.record("side_tool", || {
            vec![
                ("seq", Json::Number(request.seq)),
                ("tool", Json::text(request.tool)),
                ("input", request.input.clone()),
                ("output", journal::output_json(&output)),
            ]
        });
        self.sides.calls.push(SideCall::Tool {
            seq: request.seq,
            tool: request.tool.to_owned(),
            input: request.input.clone(),
            output: output.clone(),
        });
        output
    }
}

fn durable_bytes(medium: &Medium) -> Vec<u8> {
    match medium {
        Medium::Jsonl(file) => file.written[..file.synced].to_vec(),
        Medium::Rows(_) => medium.bytes(),
    }
}

pub fn side_arguments(program: &Program, request: &SideRequest) -> Json {
    let side = program
        .side(&request.handler)
        .expect("a side request names a side handler");
    turn::arguments_json(program, side, &request.arguments)
}
