#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Tool,
    Listing,
    Dispatch,
    Model,
    Now,
    Random,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Listing => "listing",
            Self::Dispatch => "dispatch",
            Self::Model => "model",
            Self::Now => "now",
            Self::Random => "random",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    RidesNextFlush,
    FlushBeforeRun,
    FlushBeforeRunWhenExternal,
}

impl Durability {
    pub const fn flushes(self, resolved: EffectClass) -> bool {
        match self {
            Self::RidesNextFlush => false,
            Self::FlushBeforeRun => true,
            Self::FlushBeforeRunWhenExternal => matches!(resolved, EffectClass::External),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site {
    pub kind: Kind,
    pub durability: Durability,
    pub maybe: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    BeforeFlush,
    AfterFlush,
    BeforeRun(Kind, EffectClass),
    AfterRun(Kind, EffectClass),
    BeforeAcknowledge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Crash,
    Fenced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    Stop(Stop),
    Corrupt(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Halt {
    Exit(Exit),
    Trap(String),
}

impl From<Stop> for Halt {
    fn from(stop: Stop) -> Self {
        Self::Exit(Exit::Stop(stop))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finish {
    pub turn: u64,
    pub outcome: Outcome,
    pub result: Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ran {
    Finished(Finish),
    AlreadyFinished(Finish),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Returns {
    U64,
    Bool,
    Text,
    Values {
        name: &'static str,
        values: &'static [&'static str],
    },
}

pub fn checked_output(tool: &str, returns: Returns, output: Output) -> Output {
    let Output::Ok(json) = &output else {
        return output;
    };
    let (fits, expected) = match returns {
        Returns::U64 => (json.as_u64().is_some(), "a u64".to_owned()),
        Returns::Bool => (matches!(json, Json::Bool(_)), "a bool".to_owned()),
        Returns::Text => (json.as_str().is_some(), "text".to_owned()),
        Returns::Values { name, values } => (
            json.as_str().is_some_and(|text| values.contains(&text)),
            format!("a value of {name}"),
        ),
    };
    if fits {
        return output;
    }
    Output::Failed(format!(
        "{tool} returned {}, which is not {expected}",
        json.encode()
    ))
}

pub const LISTED_TOOL_SEPARATOR: &str = "__";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Untyped,
    Typed(Json),
    Invalid(String),
}

pub fn listed(tool: &str) -> Option<(usize, &str)> {
    let (prefix, name) = tool.split_once(LISTED_TOOL_SEPARATOR)?;
    if name.is_empty() {
        return None;
    }
    let id = program::LISTINGS.iter().position(|listing| listing.0 == prefix)?;
    Some((id, name))
}

pub fn typed_input(tool: &str, raw: &str, check: fn(&Json) -> Result<(), String>) -> Input {
    let input = match Json::decode(raw) {
        Ok(input @ Json::Object(_)) => input,
        _ => return Input::Invalid(format!("input of {tool}: not a JSON object")),
    };
    match check(&input) {
        Ok(()) => Input::Typed(input),
        Err(error) => Input::Invalid(format!("input of {tool}: {error}")),
    }
}

pub fn input_fields(input: &Json) -> Result<&BTreeMap<String, Json>, String> {
    match input {
        Json::Object(fields) => Ok(fields),
        _ => Err("not a JSON object".to_owned()),
    }
}

pub fn input_param<T>(fields: &BTreeMap<String, Json>, name: &str, parse: fn(&Json) -> Result<T, String>) -> Result<(), String> {
    let field = fields.get(name).ok_or_else(|| format!("missing '{name}'"))?;
    parse(field).map(drop).map_err(|error| format!("'{name}': {error}"))
}

pub fn input_extra(fields: &BTreeMap<String, Json>, params: &[&str]) -> Result<(), String> {
    match fields.keys().find(|key| !params.contains(&key.as_str())) {
        Some(extra) => Err(format!("'{extra}' is not a parameter")),
        None => Ok(()),
    }
}

pub fn not_admitted(tool: &str, listing: &str, name: &str) -> String {
    format!("{tool} was not admitted: listing {listing} did not list {name} in this turn")
}

pub fn admitted_tools(json: &Json) -> Result<BTreeMap<String, String>, String> {
    let Json::Object(map) = json else {
        return Err(format!("a listing is an object, found {}", json.encode()));
    };
    let mut tools = BTreeMap::new();
    for (name, fingerprint) in map {
        let fingerprint = fingerprint
            .as_str()
            .ok_or_else(|| format!("listed tool '{name}' has fingerprint {}, not text", fingerprint.encode()))?;
        tools.insert(name.clone(), fingerprint.to_owned());
    }
    Ok(tools)
}

pub fn listing_count(json: &Json) -> Result<u64, String> {
    admitted_tools(json).map(|tools| tools.len() as u64)
}

pub fn checked_listing(listing: usize, output: Output) -> Output {
    let (name, bound_name, bound) = program::LISTINGS[listing];
    let Output::Ok(json) = &output else {
        return output;
    };
    let fingerprints = match json {
        Json::Object(map) if map.iter().all(|(tool, fingerprint)| !tool.is_empty() && fingerprint.as_str().is_some()) => map.len() as u64,
        other => {
            return Output::Failed(format!(
                "listing {name} returned {}, which is not an object of fingerprints",
                other.encode()
            ));
        }
    };
    if fingerprints > bound {
        return Output::Failed(format!(
            "listing {name} returned {fingerprints} tools, over its bound {bound_name} ({bound})"
        ));
    }
    output
}

pub struct ToolRequest<'a> {
    pub turn: u64,
    pub seq: u64,
    pub tool: &'a str,
    pub effect: EffectClass,
    pub input: &'a Json,
    pub returns: Returns,
    pub admitted: Option<&'a str>,
}

pub struct ListingRequest<'a> {
    pub turn: u64,
    pub seq: u64,
    pub listing: &'a str,
}

pub struct ModelRequest<'a> {
    pub turn: u64,
    pub seq: u64,
    pub calls_max: u64,
    pub context: &'a dyn Fn() -> Json,
}

pub trait World {
    fn hook(&mut self, point: Point) -> Result<(), Stop>;
    fn flush(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop>;
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output;
    fn listing(&mut self, request: &ListingRequest<'_>) -> Output;
    fn model(&mut self, request: &ModelRequest<'_>) -> Json;
    fn now(&mut self) -> u64;
    fn random(&mut self) -> u64;
    fn acknowledge(&mut self, finish: &Finish);
}

pub trait Host {
    fn hook(&mut self, _point: Point) -> Result<(), Stop> {
        Ok(())
    }
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output;
    fn listing(&mut self, request: &ListingRequest<'_>) -> Output;
    fn model(&mut self, request: &ModelRequest<'_>) -> Json;
    fn now(&mut self) -> u64;
    fn random(&mut self) -> u64;
    fn acknowledge(&mut self, finish: &Finish);
}

pub enum Run<'a> {
    Tool(Returns),
    Listing(usize),
    Model {
        calls_max: u64,
        context: &'a dyn Fn() -> Json,
    },
    Now,
    Random,
}

pub enum Answer {
    Output(Output),
    Reply(Json),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Known<T> {
    Ok(T),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Maybe<T> {
    Ok(T),
    Failed(String),
    Unknown,
}

pub enum Mode<'a> {
    Live {
        world: &'a mut dyn World,
        epoch: u64,
    },
    Replay,
}

pub struct Engine<'a> {
    mode: Mode<'a>,
    turn: u64,
    sites: BTreeMap<u64, SiteLog>,
    seq: u64,
    attempts: u64,
    buffer: Vec<Record>,
    acknowledged: bool,
    admitted: BTreeMap<usize, BTreeMap<String, String>>,
}

impl<'a> Engine<'a> {
    pub fn new(mode: Mode<'a>, turn: u64, log: &TurnLog) -> Self {
        Self {
            mode,
            turn,
            sites: log.sites.clone(),
            seq: 0,
            attempts: log.attempts,
            buffer: Vec::new(),
            acknowledged: false,
            admitted: BTreeMap::new(),
        }
    }

    pub fn append(&mut self, record: Record) {
        if matches!(self.mode, Mode::Live { .. }) {
            self.buffer.push(record);
        }
    }

    pub fn next_attempt(&mut self) -> u64 {
        self.attempts += 1;
        self.attempts
    }

    pub fn run(mut self, mut state: program::State, arguments: &program::Arguments) -> Result<(Finish, program::State), Exit> {
        let finish = match program::run_body(&mut self, &mut state, arguments) {
            Ok(result) => self.finish(&state, Outcome::Completed, result),
            Err(Halt::Trap(trap)) => self.finish(
                &state,
                Outcome::Failed,
                Json::object(vec![("trap", Json::String(trap))]),
            ),
            Err(Halt::Exit(exit)) => Err(exit),
        }?;
        Ok((finish, state))
    }

    fn finish(&mut self, state: &program::State, outcome: Outcome, result: Json) -> Result<Finish, Exit> {
        let finish = Finish {
            turn: self.turn,
            outcome,
            result: result.clone(),
        };
        self.append(Record::Status {
            turn: self.turn,
            last: self.seq,
            outcome,
            result,
        });
        self.append(Record::Checkpoint {
            revision: self.turn,
            state: program::HARNESS_NAME.to_owned(),
            payload: program::state_json(state).encode(),
        });
        self.flush().map_err(halt_exit)?;
        assert!(
            self.buffer.is_empty(),
            "a turn's status is durable before it is acknowledged"
        );
        self.hook(Point::BeforeAcknowledge).map_err(halt_exit)?;
        if !self.acknowledged {
            self.acknowledged = true;
            if let Mode::Live { world, .. } = &mut self.mode {
                world.acknowledge(&finish);
            }
        }
        Ok(finish)
    }

    fn hook(&mut self, point: Point) -> Result<(), Halt> {
        match &mut self.mode {
            Mode::Live { world, .. } => world.hook(point).map_err(Halt::from),
            Mode::Replay => Ok(()),
        }
    }

    fn flush(&mut self) -> Result<(), Halt> {
        let Mode::Live { world, epoch } = &mut self.mode else {
            return Ok(());
        };
        if self.buffer.is_empty() {
            return Ok(());
        }
        world.hook(Point::BeforeFlush)?;
        world.flush(*epoch, &self.buffer)?;
        self.buffer.clear();
        world.hook(Point::AfterFlush)?;
        Ok(())
    }

    pub fn corrupt(&self, error: String) -> Halt {
        Halt::Exit(Exit::Corrupt(format!("turn {} result: {error}", self.turn)))
    }

    pub fn effect(&mut self, site: Site, call: Call, run: Run<'_>) -> Result<Answer, Halt> {
        self.seq += 1;
        let seq = self.seq;
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
                if site.kind == Kind::Model {
                    return Err(self.corrupt(format!("{output:?} does not fit a reply")));
                }
                if let Run::Listing(listing) = run {
                    self.admit_listing(listing, &output)?;
                }
                Ok(Answer::Output(output))
            }
            Some(SiteLog::Reply(response)) => {
                if site.kind != Kind::Model {
                    return Err(self.diverged(seq, "a model reply", &call.tool));
                }
                Ok(Answer::Reply(response))
            }
            Some(SiteLog::Unknown(recorded)) => {
                self.same(seq, &recorded, &call)?;
                if !site.maybe {
                    return self.reopen(site, seq, call, run);
                }
                Ok(Answer::Unknown)
            }
            Some(SiteLog::Open(recorded)) => {
                self.same(seq, &recorded, &call)?;
                self.reopen(site, seq, call, run)
            }
            None => {
                self.require_live(seq)?;
                if site.kind != Kind::Model {
                    self.append(Record::Call {
                        turn: self.turn,
                        seq,
                        tool: call.tool.clone(),
                        effect: call.effect,
                        input: call.input.clone(),
                    });
                    self.sites.insert(seq, SiteLog::Open(call.clone()));
                }
                self.execute(site, seq, call, run)
            }
        }
    }

    fn require_live(&self, seq: u64) -> Result<(), Halt> {
        match self.mode {
            Mode::Live { .. } => Ok(()),
            Mode::Replay => Err(Halt::Exit(Exit::Corrupt(format!(
                "replay of turn {} needs seq {seq}, which the journal does not record",
                self.turn
            )))),
        }
    }

    fn reopen(&mut self, site: Site, seq: u64, call: Call, run: Run<'_>) -> Result<Answer, Halt> {
        self.require_live(seq)?;
        assert!(
            !call.effect.may_be_unknown(),
            "recovery turns every open external call unknown before the turn runs again"
        );
        self.execute(site, seq, call, run)
    }

    fn execute(&mut self, site: Site, seq: u64, call: Call, run: Run<'_>) -> Result<Answer, Halt> {
        if site.durability.flushes(call.effect) {
            self.flush()?;
            assert!(
                self.buffer.is_empty(),
                "the call record is durable before its effect runs"
            );
        }
        self.hook(Point::BeforeRun(site.kind, call.effect))?;
        let turn = self.turn;
        let gate = if site.kind == Kind::Dispatch { self.gate(&call) } else { Ok(None) };
        let listing = match run {
            Run::Listing(listing) => Some(listing),
            _ => None,
        };
        let Mode::Live { world, .. } = &mut self.mode else {
            unreachable!("replay never runs an effect");
        };
        let output = match run {
            Run::Model { calls_max, context } => {
                let response = world.model(&ModelRequest {
                    turn,
                    seq,
                    calls_max,
                    context,
                });
                self.hook(Point::AfterRun(site.kind, call.effect))?;
                self.append(Record::Model {
                    turn,
                    seq,
                    response: response.clone(),
                });
                self.sites.insert(seq, SiteLog::Reply(response.clone()));
                return Ok(Answer::Reply(response));
            }
            Run::Now => Output::Ok(Json::Number(world.now())),
            Run::Random => Output::Ok(Json::Number(world.random())),
            Run::Listing(listing) => {
                let output = world.listing(&ListingRequest {
                    turn,
                    seq,
                    listing: program::LISTINGS[listing].0,
                });
                checked_listing(listing, output)
            }
            Run::Tool(returns) => match gate {
                Err(refused) => Output::Failed(refused),
                Ok(admitted) => {
                    let output = world.tool(&ToolRequest {
                        turn,
                        seq,
                        tool: &call.tool,
                        effect: call.effect,
                        input: &call.input,
                        returns,
                        admitted: admitted.as_deref(),
                    });
                    checked_output(&call.tool, returns, output)
                }
            },
        };
        self.hook(Point::AfterRun(site.kind, call.effect))?;
        self.append(Record::Result {
            turn,
            seq,
            output: output.clone(),
        });
        self.sites.insert(seq, SiteLog::Done(call, output.clone()));
        if let Some(listing) = listing {
            self.admit_listing(listing, &output)?;
        }
        Ok(Answer::Output(output))
    }

    fn gate(&self, call: &Call) -> Result<Option<String>, String> {
        if let Json::String(raw) = &call.input
            && let Input::Invalid(error) = program::tool_input(&call.tool, raw)
        {
            return Err(error);
        }
        let Some((listing, name)) = listed(&call.tool) else {
            return Ok(None);
        };
        match self.admitted.get(&listing).and_then(|tools| tools.get(name)) {
            Some(fingerprint) => Ok(Some(fingerprint.clone())),
            None => Err(not_admitted(&call.tool, program::LISTINGS[listing].0, name)),
        }
    }

    fn admit_listing(&mut self, listing: usize, output: &Output) -> Result<(), Halt> {
        match output {
            Output::Ok(json) => {
                let tools = admitted_tools(json).map_err(|error| self.corrupt(error))?;
                self.admitted.insert(listing, tools);
            }
            Output::Failed(_) => {
                self.admitted.remove(&listing);
            }
        }
        Ok(())
    }

    fn same(&self, seq: u64, recorded: &Call, call: &Call) -> Result<(), Halt> {
        if recorded.tool != call.tool {
            return Err(self.diverged(seq, &recorded.tool, &call.tool));
        }
        if recorded.effect != call.effect || recorded.input != call.input {
            return Err(Halt::Exit(Exit::Corrupt(format!(
                "replay of turn {} diverged at seq {seq}: tool '{}' was recorded with another class or input",
                self.turn, call.tool
            ))));
        }
        Ok(())
    }

    fn diverged(&self, seq: u64, recorded: &str, running: &str) -> Halt {
        Halt::Exit(Exit::Corrupt(format!(
            "replay of turn {} diverged at seq {seq}: the journal records {recorded}, the program runs '{running}' ({})",
            self.turn,
            call_id(self.turn, seq)
        )))
    }
}

fn halt_exit(halt: Halt) -> Exit {
    match halt {
        Halt::Exit(exit) => exit,
        Halt::Trap(trap) => panic!("finishing a turn cannot trap, found {trap}"),
    }
}

pub fn known<T>(engine: &Engine<'_>, answer: Answer, parse: fn(&Json) -> Result<T, String>) -> Result<Known<T>, Halt> {
    match answer {
        Answer::Output(Output::Ok(json)) => Ok(Known::Ok(parse(&json).map_err(|error| engine.corrupt(error))?)),
        Answer::Output(Output::Failed(error)) => Ok(Known::Failed(error)),
        Answer::Reply(_) | Answer::Unknown => panic!("a known site gets an output"),
    }
}

pub fn maybe<T>(engine: &Engine<'_>, answer: Answer, parse: fn(&Json) -> Result<T, String>) -> Result<Maybe<T>, Halt> {
    match answer {
        Answer::Output(Output::Ok(json)) => Ok(Maybe::Ok(parse(&json).map_err(|error| engine.corrupt(error))?)),
        Answer::Output(Output::Failed(error)) => Ok(Maybe::Failed(error)),
        Answer::Unknown => Ok(Maybe::Unknown),
        Answer::Reply(_) => panic!("a tool site gets no model reply"),
    }
}

pub fn scalar(engine: &Engine<'_>, answer: Answer) -> Result<u64, Halt> {
    match answer {
        Answer::Output(Output::Ok(json)) => json
            .as_u64()
            .ok_or_else(|| engine.corrupt(format!("{} is not u64", json.encode()))),
        Answer::Output(Output::Failed(error)) => Err(engine.corrupt(format!("Failed({error:?}) does not fit U64"))),
        Answer::Reply(_) | Answer::Unknown => panic!("a clock or random site gets an output"),
    }
}

pub const MODEL_FAILED: &str = "failed";

pub fn model_failure(response: &Json) -> Option<&str> {
    match response {
        Json::Object(map) if map.len() == 1 => map.get(MODEL_FAILED).and_then(Json::as_str),
        _ => None,
    }
}

pub fn known_reply<T>(answer: Answer, parse: fn(&Json) -> Result<T, String>) -> Known<T> {
    match answer {
        Answer::Reply(response) => match model_failure(&response) {
            Some(error) => Known::Failed(error.to_owned()),
            None => match parse(&response) {
                Ok(reply) => Known::Ok(reply),
                Err(error) => Known::Failed(format!("model reply: {error}")),
            },
        },
        Answer::Output(_) | Answer::Unknown => panic!("a model site gets a reply"),
    }
}

pub fn reply<T>(answer: Answer, parse: fn(&Json) -> Result<T, String>) -> Result<T, Halt> {
    match answer {
        Answer::Reply(response) => parse(&response).map_err(|error| Halt::Trap(format!("model reply: {error}"))),
        Answer::Output(_) | Answer::Unknown => panic!("a model site gets a reply"),
    }
}

pub fn add(left: u64, right: u64) -> Result<u64, Halt> {
    left.checked_add(right)
        .ok_or_else(|| Halt::Trap("u64 overflow in '+'".to_owned()))
}

pub fn subtract(left: u64, right: u64) -> Result<u64, Halt> {
    left.checked_sub(right)
        .ok_or_else(|| Halt::Trap("u64 underflow in '-'".to_owned()))
}

pub fn multiply(left: u64, right: u64) -> Result<u64, Halt> {
    left.checked_mul(right)
        .ok_or_else(|| Halt::Trap("u64 overflow in '*'".to_owned()))
}

pub fn concat(left: String, right: String) -> String {
    let mut out = left;
    out.push_str(&right);
    out
}

pub fn push<T>(list: &mut Vec<T>, value: T, capacity: u64, field: &str) -> Result<(), Halt> {
    if list.len() as u64 >= capacity {
        return Err(Halt::Trap(format!("bound({field})")));
    }
    list.push(value);
    Ok(())
}

pub fn parse_u64(json: &Json) -> Result<u64, String> {
    json.as_u64()
        .ok_or_else(|| format!("expected u64, found {}", json.encode()))
}

pub fn parse_bool(json: &Json) -> Result<bool, String> {
    match json {
        Json::Bool(flag) => Ok(*flag),
        other => Err(format!("expected bool, found {}", other.encode())),
    }
}

pub fn parse_text(json: &Json) -> Result<String, String> {
    json.as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("expected text, found {}", json.encode()))
}

pub fn parse_list<T>(json: &Json, capacity: u64, name: &str, element: fn(&Json) -> Result<T, String>) -> Result<Vec<T>, String> {
    let Json::Array(items) = json else {
        return Err(format!("expected a list, found {}", json.encode()));
    };
    if items.len() as u64 > capacity {
        return Err(format!("bound({name})"));
    }
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        values.push(element(item)?);
    }
    Ok(values)
}

pub fn variant_of<'j>(json: &'j Json, type_name: &str, variants: &[&str]) -> Result<(usize, Option<&'j Json>), String> {
    let (name, payload) = match json {
        Json::String(name) => (name.as_str(), None),
        other => match other.tagged() {
            Some((name, payload)) => (name, Some(payload)),
            None => {
                return Err(format!(
                    "expected a variant of {type_name}, found {}",
                    json.encode()
                ));
            }
        },
    };
    let Some(index) = variants.iter().position(|variant| *variant == name) else {
        return Err(format!("'{name}' is not a variant of {type_name}"));
    };
    Ok((index, payload))
}

pub fn variant_fields<'j>(payload: Option<&'j Json>, name: &str, count: usize) -> Result<Vec<&'j Json>, String> {
    match (count, payload) {
        (0, None) => Ok(Vec::new()),
        (1, Some(payload)) => Ok(vec![payload]),
        (count, Some(Json::Array(items))) if count > 1 && items.len() == count => Ok(items.iter().collect()),
        _ => Err(format!("variant '{name}' has {count} field(s)")),
    }
}

pub fn tagged(name: &str, mut fields: Vec<Json>) -> Json {
    match fields.len() {
        0 => Json::text(name),
        1 => Json::object(vec![(name, fields.pop().expect("one field"))]),
        _ => Json::object(vec![(name, Json::Array(fields))]),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRequest {
    pub turn: u64,
    pub arguments: program::Arguments,
}

fn corrupt(error: String) -> Exit {
    Exit::Corrupt(error)
}

pub fn replay(turn: u64, log: &TurnLog, state: program::State) -> Result<program::State, Exit> {
    let arguments = program::decode_arguments(&log.handler, &log.arguments).map_err(|error| match error {
        program::ArgumentsError::NoHandler => corrupt(format!(
            "turn {turn} ran handler '{}', which the program lacks",
            log.handler
        )),
        program::ArgumentsError::Invalid(error) => corrupt(error),
    })?;
    let finished = log
        .finished
        .as_ref()
        .ok_or_else(|| corrupt(format!("turn {turn} is replayed before it finished")))?;
    let (finish, state) = Engine::new(Mode::Replay, turn, log).run(state, &arguments)?;
    if finish.outcome != finished.outcome || finish.result != finished.result {
        return Err(corrupt(format!(
            "replay of turn {turn} finished {} where the journal says {}",
            finish.result.encode(),
            finished.result.encode()
        )));
    }
    Ok(state)
}

pub fn state_before(session: &Session, turn: u64) -> Result<program::State, Exit> {
    let (start, mut state) = match session.checkpoints.range(..turn).next_back() {
        Some((revision, payload)) => {
            let payload = Json::decode(payload)
                .map_err(|error| corrupt(format!("checkpoint {revision}: {error}")))?;
            (revision + 1, program::state_from_json(&payload).map_err(corrupt)?)
        }
        None => (1, program::State::default()),
    };
    for earlier in start..turn {
        let log = session
            .turns
            .get(&earlier)
            .ok_or_else(|| corrupt(format!("turn {earlier} is missing before turn {turn}")))?;
        state = replay(earlier, log, state)?;
    }
    Ok(state)
}

fn recover(log: &mut TurnLog) -> Vec<u64> {
    let unknown: Vec<u64> = log
        .sites
        .iter()
        .filter_map(|(seq, site)| match site {
            SiteLog::Open(call) if call.effect.may_be_unknown() => Some(*seq),
            _ => None,
        })
        .collect();
    for seq in &unknown {
        let site = log.sites.get_mut(seq).expect("an open seq has a site");
        let SiteLog::Open(call) = site else {
            panic!("recover marks only open sites");
        };
        *site = SiteLog::Unknown(call.clone());
    }
    unknown
}

pub fn run_turn(world: &mut dyn World, epoch: u64, records: &[Record], request: &TurnRequest) -> Result<Ran, Exit> {
    run_turn_from(world, epoch, Start::Beginning, records, request)
}

pub fn run_turn_from(world: &mut dyn World, epoch: u64, start: Start, records: &[Record], request: &TurnRequest) -> Result<Ran, Exit> {
    if let Start::Checkpoint(revision) = start {
        assert!(
            request.turn > revision,
            "a turn runs over a journal read from after it finished"
        );
    }
    let session = fold_from(start, records).map_err(corrupt)?;
    let handler = program::handler_name(&request.arguments);
    let arguments = program::arguments_json(&request.arguments);
    if let Some(log) = session.turns.get(&request.turn)
        && let Some(finished) = &log.finished
    {
        return Ok(Ran::AlreadyFinished(Finish {
            turn: request.turn,
            outcome: finished.outcome,
            result: finished.result.clone(),
        }));
    }
    let state = state_before(&session, request.turn)?;
    let mut pending = None;
    let mut unknown = Vec::new();
    let log = match session.turns.get(&request.turn) {
        Some(log) => {
            if log.handler != handler || log.arguments != arguments {
                return Err(corrupt(format!(
                    "turn {} was admitted with other arguments",
                    request.turn
                )));
            }
            let mut log = log.clone();
            unknown = recover(&mut log);
            log
        }
        None => {
            let next = session
                .turns
                .last_key_value()
                .map_or(session.start.first_turn(), |(last, _)| last + 1);
            if request.turn != next {
                return Err(corrupt(format!(
                    "turn {} is requested when turn {next} is next",
                    request.turn
                )));
            }
            pending = Some(Record::Admitted {
                turn: request.turn,
                handler: handler.to_owned(),
                arguments,
            });
            TurnLog {
                handler: handler.to_owned(),
                arguments: Json::Null,
                sites: BTreeMap::new(),
                attempts: 0,
                finished: None,
            }
        }
    };
    let mut engine = Engine::new(Mode::Live { world, epoch }, request.turn, &log);
    if let Some(record) = pending {
        engine.append(record);
    }
    if !unknown.is_empty() {
        let attempt = engine.next_attempt();
        engine.append(Record::Recovery {
            turn: request.turn,
            attempt,
            unknown,
        });
    }
    engine
        .run(state, &request.arguments)
        .map(|(finish, _)| Ran::Finished(finish))
}

pub fn self_check() {
    assert_eq!(crc32(CRC32_CHECK_INPUT), CRC32_CHECK, "CRC-32 matches its catalogued check value");
}
