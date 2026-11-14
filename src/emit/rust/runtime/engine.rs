#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Tool,
    Listing,
    Dispatch,
    Model,
    Now,
    Random,
    Wait,
    Suspend,
    Deny,
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
            Self::Wait => "wait",
            Self::Suspend => "suspend",
            Self::Deny => "deny",
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
    pub mode: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    BeforeFlush,
    AfterFlush,
    BeforeRun(Kind, EffectClass),
    AfterRun(Kind, EffectClass),
    BeforeAcknowledge,
    Cell(CellPoint),
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
    Suspend(Suspension),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suspension {
    pub turn: u64,
    pub seq: u64,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    pub key: String,
    pub answer: String,
}

pub const WAIT_TOOL: &str = "wait";
pub const WAIT_MS: &str = "ms";
pub const WAIT_UNTIL: &str = "until";
pub const SUSPEND_TOOL: &str = "suspend";
pub const SUSPEND_KEY: &str = "key";
pub const DENY_TOOL: &str = "deny";
pub const SPAWN_KEY_SEPARATOR: &str = ".";

pub fn deny_input(id: &str, tool: &str, input: &str, reason: &str) -> Json {
    Json::object(vec![
        ("id", Json::String(id.to_owned())),
        ("input", Json::String(input.to_owned())),
        ("reason", Json::String(reason.to_owned())),
        ("tool", Json::String(tool.to_owned())),
    ])
}

pub fn spawn_output(turn: u64, seq: u64, tool: &str, output: Output) -> Output {
    if !matches!(output, Output::Ok(_)) || !program::spawns(tool) {
        return output;
    }
    Output::Ok(Json::String(format!("{turn}{SPAWN_KEY_SEPARATOR}{seq}")))
}

pub fn wait_until(input: &Json) -> Option<u64> {
    input.get(WAIT_UNTIL).and_then(Json::as_u64)
}

fn without_until(call: &Call) -> Call {
    let input = match &call.input {
        Json::Object(fields) => Json::Object(
            fields
                .iter()
                .filter(|(key, _)| key.as_str() != WAIT_UNTIL)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        other => other.clone(),
    };
    Call {
        input,
        ..call.clone()
    }
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
    Suspended(Suspension),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Finished(Finish),
    Suspended(Suspension),
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
    Blob {
        name: &'static str,
        bytes: u64,
    },
}

pub fn checked_output(tool: &str, returns: Returns, output: Output) -> Output {
    if let Some((name, bytes)) = program::blob_bound(tool)
        && (matches!(returns, Returns::Blob { .. }) || matches!(&output, Output::Ok(Json::Object(_))))
    {
        return checked_blob(tool, name, bytes, output);
    }
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
        Returns::Blob { .. } => unreachable!("a tool that returns a blob has a blob bound"),
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
pub enum ToolInput {
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

pub fn typed_input(tool: &str, raw: &str, check: fn(&mut Json) -> Result<(), String>) -> ToolInput {
    let mut input = match Json::decode(raw) {
        Ok(input @ Json::Object(_)) => input,
        _ => return ToolInput::Invalid(format!("input of {tool}: not a JSON object")),
    };
    match check(&mut input) {
        Ok(()) => ToolInput::Typed(input),
        Err(error) => ToolInput::Invalid(format!("input of {tool}: {error}")),
    }
}

pub fn input_default(input: &mut Json, name: &str, value: Json) {
    if let Json::Object(fields) = input {
        fields.entry(name.to_owned()).or_insert(value);
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

pub fn not_discovered(tool: &str, listing: &str) -> String {
    format!("{tool} was not discovered: no search of listing {listing} found it in this turn")
}

pub fn not_found(tool: &str, found: &str, listing: &str) -> String {
    format!("{tool} found {found}, which listing {listing} did not list in this turn")
}

pub const SEARCH_SEPARATOR: char = '\n';

pub fn found_names(text: &str) -> Vec<&str> {
    text.split(SEARCH_SEPARATOR).filter(|line| !line.is_empty()).collect()
}

pub fn checked_search(tool: &str, listing: usize, admitted: Option<&BTreeMap<String, String>>, output: Output) -> Output {
    let Output::Ok(Json::String(text)) = &output else {
        return output;
    };
    let (name, bound_name, bound) = program::LISTINGS[listing];
    let names = found_names(text);
    if names.len() as u64 > bound {
        return Output::Failed(format!(
            "{tool} found {} tools, over listing {name}'s bound {bound_name} ({bound})",
            names.len()
        ));
    }
    for found in names {
        let listed_now = listed(found)
            .filter(|(id, _)| *id == listing)
            .is_some_and(|(_, bare)| admitted.is_some_and(|tools| tools.contains_key(bare)));
        if !listed_now {
            return Output::Failed(not_found(tool, found, name));
        }
    }
    output
}

pub fn listing_change(listing: &str, before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> String {
    let named = |name: &str| format!("{listing}{LISTED_TOOL_SEPARATOR}{name}");
    let added: Vec<String> = after.keys().filter(|name| !before.contains_key(*name)).map(|name| named(name)).collect();
    let changed: Vec<String> = after
        .iter()
        .filter(|(name, fingerprint)| before.get(*name).is_some_and(|old| old != *fingerprint))
        .map(|(name, _)| named(name))
        .collect();
    let removed: Vec<String> = before.keys().filter(|name| !after.contains_key(*name)).map(|name| named(name)).collect();
    let parts: Vec<String> = [("added", added), ("changed", changed), ("removed", removed)]
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(what, names)| format!("{what} {}", names.join(", ")))
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!("tools of listing {listing} changed: {}", parts.join("; "))
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
    pub nested: Option<NestedRef>,
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
    pub mode: Option<&'a str>,
    pub context: &'a dyn Fn() -> Json,
    pub blobs: &'a Blobs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventAnswer {
    Recorded,
    Duplicate,
    Full,
    Undeclared,
    Finished,
    NotRunning,
}

impl EventAnswer {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Duplicate => "duplicate",
            Self::Full => "full",
            Self::Undeclared => "undeclared",
            Self::Finished => "finished",
            Self::NotRunning => "not_running",
        }
    }
}

pub trait World {
    fn hook(&mut self, point: Point) -> Result<(), Stop>;
    fn flush(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop>;
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output;
    fn listing(&mut self, request: &ListingRequest<'_>) -> Output;
    fn tools(&mut self, requests: &[ToolRequest<'_>], limit: Option<u64>) -> Result<Vec<(usize, Output)>, Stop>;
    fn model(&mut self, request: &ModelRequest<'_>) -> Json;
    fn now(&mut self) -> u64;
    fn random(&mut self) -> u64;
    fn acknowledge(&mut self, finish: &Finish);
    fn cell_start(&mut self, cell: u64, script: &str) -> Step;
    fn cell_resume(&mut self, cell: u64, input: Input) -> Step;
    fn wait(&mut self, wait: &Wait) -> Result<(), Stop>;
    fn observe(&mut self, event: &CellEvent);
    fn inbox(&mut self, turn: u64) -> Vec<Delivery>;
    fn answer(&mut self, turn: u64, position: u64, answers: &[(String, EventAnswer)]);
    fn clock(&mut self) -> u64;
    fn sleep(&mut self, turn: u64, seq: u64, until_ms: u64) -> Result<u64, Stop>;
    fn resume(&mut self, suspension: &Suspension) -> Option<Resume>;
    fn resumed(&mut self, suspension: &Suspension, resume: &Resume, matched: bool);
    fn side(&mut self, _turn: u64, _seq: u64) -> Option<program::SideArguments> {
        None
    }
    fn sided(&mut self, _side: &program::SideArguments, _answer: &Result<SideAnswer, Exit>) {}
}

pub trait Host {
    fn hook(&mut self, _point: Point) -> Result<(), Stop> {
        Ok(())
    }
    fn tool(&mut self, request: &ToolRequest<'_>) -> Output;
    fn listing(&mut self, request: &ListingRequest<'_>) -> Output;
    fn concurrent(&self) -> Option<&dyn ConcurrentTools> {
        None
    }
    fn model(&mut self, request: &ModelRequest<'_>) -> Json;
    fn now(&mut self) -> u64;
    fn random(&mut self) -> u64;
    fn acknowledge(&mut self, finish: &Finish);
    fn cells(&mut self) -> Option<&mut dyn CellEngine> {
        None
    }
    fn inbox(&mut self, _turn: u64) -> Vec<Delivery> {
        Vec::new()
    }
    fn answer(&mut self, _turn: u64, _position: u64, _answers: &[(String, EventAnswer)]) {}
    fn clock(&mut self) -> u64 {
        self.now()
    }
    fn sleep(&mut self, _turn: u64, _seq: u64, until_ms: u64) -> Result<u64, Stop> {
        let clock = self.clock();
        if until_ms > clock {
            std::thread::sleep(std::time::Duration::from_millis(until_ms - clock));
        }
        Ok(self.clock().max(until_ms))
    }
    fn resume(&mut self, _suspension: &Suspension) -> Option<Resume> {
        None
    }
    fn resumed(&mut self, _suspension: &Suspension, _resume: &Resume, _matched: bool) {}
    fn side(&mut self, _turn: u64, _seq: u64) -> Option<program::SideArguments> {
        None
    }
    fn sided(&mut self, _side: &program::SideArguments, _answer: &Result<SideAnswer, Exit>) {}
}

pub struct Sent {
    pub turn: u64,
    pub delivery: Delivery,
    pub reply: std::sync::mpsc::Sender<EventAnswer>,
}

pub struct Inbox {
    receiver: std::sync::mpsc::Receiver<Sent>,
    waiting: Vec<Sent>,
}

impl Inbox {
    pub fn channel(capacity: usize) -> (std::sync::mpsc::SyncSender<Sent>, Self) {
        let (sender, receiver) = std::sync::mpsc::sync_channel(capacity);
        let inbox = Self {
            receiver,
            waiting: Vec::with_capacity(capacity),
        };
        (sender, inbox)
    }

    pub fn arrived(&mut self, turn: u64) -> Vec<Delivery> {
        while let Ok(sent) = self.receiver.try_recv() {
            if sent.turn == turn {
                self.waiting.push(sent);
            } else {
                let _ = sent.reply.send(EventAnswer::NotRunning);
            }
        }
        self.waiting.iter().map(|sent| sent.delivery.clone()).collect()
    }

    pub fn answer(&mut self, turn: u64, answers: &[(String, EventAnswer)]) {
        for (id, answer) in answers {
            let found = self
                .waiting
                .iter()
                .position(|sent| sent.turn == turn && sent.delivery.id == *id);
            if let Some(index) = found {
                let sent = self.waiting.remove(index);
                let _ = sent.reply.send(*answer);
            }
        }
    }
}

pub const fn events_taken() -> bool {
    program::TAKES_CANCEL || program::STEER_EVENT.is_some()
}

pub fn event_seq(turn: u64, index: u64) -> Option<u64> {
    assert!(index >= 1, "an event's index in its turn counts from 1");
    turn.checked_sub(1)?.checked_mul(program::EVENT_RECORDS_MAX)?.checked_add(index)
}

pub fn steer_count(events: &[Recorded]) -> u64 {
    events
        .iter()
        .filter(|event| matches!(event.delivery.inbound, Inbound::Steer(_)))
        .count() as u64
}

pub fn close_inbox(world: &mut dyn World, turn: u64, position: u64, events: &[Recorded]) {
    let arrived = world.inbox(turn);
    if arrived.is_empty() {
        return;
    }
    let answers: Vec<(String, EventAnswer)> = arrived
        .into_iter()
        .map(|delivery| {
            let recorded = events.iter().any(|event| event.delivery.id == delivery.id);
            let answer = if recorded { EventAnswer::Duplicate } else { EventAnswer::Finished };
            (delivery.id, answer)
        })
        .collect();
    world.answer(turn, position, &answers);
}

fn check_events(turn: u64, log: &TurnLog) -> Result<(), Exit> {
    for event in &log.events {
        let taken = match &event.delivery.inbound {
            Inbound::Cancel => program::TAKES_CANCEL,
            Inbound::Steer(_) => program::STEER_EVENT.is_some(),
        };
        if !taken {
            return Err(corrupt(format!(
                "turn {turn} records a {} event, which the program does not take",
                event.delivery.inbound.name()
            )));
        }
    }
    let steers = steer_count(&log.events);
    if let Some((name, bound)) = program::STEER_EVENT
        && steers > bound
    {
        return Err(corrupt(format!(
            "turn {turn} records {steers} steers, more than {name} ({bound})"
        )));
    }
    Ok(())
}

pub fn host_cell_start<H: Host>(host: &mut H, cell: u64, script: &str) -> Step {
    match host.cells() {
        Some(engine) => engine.start(cell, script),
        None => Step::Threw {
            error: NO_CELL_ENGINE.to_owned(),
        },
    }
}

pub fn host_cell_resume<H: Host>(host: &mut H, cell: u64, input: Input) -> Step {
    let engine = host
        .cells()
        .expect("a host that started a cell resumes it with the same engine");
    engine.resume(cell, input)
}

pub fn host_wait<H: Host>(host: &mut H, wait: &Wait) -> Result<(), Stop> {
    let engine = host
        .cells()
        .expect("a host that armed a timer waits on it with the same engine");
    engine.wait(wait)
}

pub const NO_CELL_ENGINE: &str = "the host runs no cell engine, so no code tool can run";

pub trait ConcurrentTools: Sync {
    fn tool(&self, request: &ToolRequest<'_>) -> Output;
}

pub fn batch_width(count: usize, limit: Option<u64>) -> usize {
    match limit {
        Some(limit) => {
            assert!(limit > 0, "a concurrency limit admits at least one request");
            usize::try_from(limit).map_or(count, |limit| limit.min(count))
        }
        None => count,
    }
}

pub fn dispatch_concurrently(tools: &dyn ConcurrentTools, requests: &[ToolRequest<'_>], limit: Option<u64>) -> Vec<(usize, Output)> {
    let width = batch_width(requests.len(), limit);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..width {
            let sender = sender.clone();
            let next = &next;
            scope.spawn(move || {
                for _ in 0..requests.len() {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if index >= requests.len() {
                        break;
                    }
                    let output = tools.tool(&requests[index]);
                    sender
                        .send((index, output))
                        .expect("the receiver outlives the dispatching threads");
                }
            });
        }
    });
    drop(sender);
    let completed: Vec<(usize, Output)> = receiver.into_iter().collect();
    assert_eq!(completed.len(), requests.len(), "every request of a batch completes once");
    completed
}

pub fn host_tools<H: Host>(host: &mut H, requests: &[ToolRequest<'_>], limit: Option<u64>) -> Result<Vec<(usize, Output)>, Stop> {
    for request in requests {
        host.hook(Point::BeforeRun(Kind::Dispatch, request.effect))?;
    }
    let concurrent = host
        .concurrent()
        .map(|tools| dispatch_concurrently(tools, requests, limit));
    let completed = match concurrent {
        Some(completed) => completed,
        None => requests
            .iter()
            .enumerate()
            .map(|(index, request)| (index, host.tool(request)))
            .collect(),
    };
    for (index, _) in &completed {
        host.hook(Point::AfterRun(Kind::Dispatch, requests[*index].effect))?;
    }
    Ok(completed)
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
    Wait,
    Suspend,
    Deny(String),
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
    cells: BTreeMap<u64, CellLog>,
    nested: BTreeMap<(u64, u64), SiteLog>,
    next_cell: u64,
    events: Vec<Recorded>,
    taken: usize,
    admitted: BTreeMap<usize, BTreeMap<String, String>>,
    listed: BTreeMap<usize, BTreeMap<String, String>>,
    notices: BTreeMap<usize, String>,
    discovered: BTreeMap<usize, std::collections::BTreeSet<String>>,
    blobs: Blobs,
    snapshot: Option<program::State>,
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
            cells: log.cells.clone(),
            nested: log.nested.clone(),
            next_cell: 0,
            events: log.events.clone(),
            taken: 0,
            admitted: BTreeMap::new(),
            listed: BTreeMap::new(),
            notices: BTreeMap::new(),
            discovered: BTreeMap::new(),
            blobs: Blobs::default(),
            snapshot: None,
        }
    }

    pub fn with_snapshot(mut self, snapshot: program::State) -> Self {
        assert!(program::HAS_SIDES, "only a program with side handlers keeps the snapshot a side question reads");
        self.snapshot = Some(snapshot);
        self
    }

    pub fn run_side(mut self, mut state: program::State, side: &program::SideArguments) -> Result<Finish, Exit> {
        match program::run_side_body(&mut self, &mut state, side) {
            Ok(result) => self.finish(&state, Outcome::Completed, result),
            Err(Halt::Trap(trap)) => self.finish(
                &state,
                Outcome::Failed,
                Json::object(vec![("trap", Json::String(trap))]),
            ),
            Err(Halt::Suspend(_)) => panic!("{SIDE_REFUSED}: a side handler never suspends"),
            Err(Halt::Exit(exit)) => Err(exit),
        }
    }

    fn side_question(&mut self, seq: u64) -> Result<(), Halt> {
        if !program::HAS_SIDES {
            return Ok(());
        }
        let Mode::Live { world, .. } = &mut self.mode else {
            unreachable!("a side question arrives only while a turn runs live")
        };
        let Some(side) = world.side(self.turn, seq) else {
            return Ok(());
        };
        let state = self
            .snapshot
            .clone()
            .expect("a live run of a program with side handlers keeps its snapshot");
        let answer = run_side(&mut **world, state, self.blobs.clone(), &side);
        world.sided(&side, &answer);
        Ok(())
    }

    pub fn with_blobs(mut self, blobs: Blobs) -> Self {
        self.blobs = blobs;
        self
    }

    pub fn blobs(&self) -> &Blobs {
        &self.blobs
    }

    pub fn listing_notice(&self, listing: usize) -> String {
        self.notices.get(&listing).cloned().unwrap_or_default()
    }

    pub fn with_next_cell(mut self, next_cell: u64) -> Self {
        self.next_cell = next_cell;
        self
    }

    pub fn cancelled(&self) -> bool {
        self.events
            .iter()
            .any(|event| event.delivery.inbound == Inbound::Cancel && event.at <= self.seq)
    }

    pub fn take_steers(&mut self) -> Vec<String> {
        let visible: Vec<String> = self
            .events
            .iter()
            .filter(|event| event.at <= self.seq)
            .filter_map(|event| match &event.delivery.inbound {
                Inbound::Steer(message) => Some(message.clone()),
                Inbound::Cancel => None,
            })
            .collect();
        assert!(self.taken <= visible.len(), "the steers a turn sees only grow");
        let fresh = visible[self.taken..].to_vec();
        self.taken = visible.len();
        fresh
    }

    fn drain(&mut self) -> Result<(), Halt> {
        if !events_taken() {
            return Ok(());
        }
        let Mode::Live { world, .. } = &mut self.mode else {
            return Ok(());
        };
        let arrived = world.inbox(self.turn);
        if arrived.is_empty() {
            return Ok(());
        }
        let position = self.seq;
        let mut answers = Vec::with_capacity(arrived.len());
        for delivery in arrived {
            let answer = self.admit_event(position, &delivery);
            answers.push((delivery.id, answer));
        }
        if answers.iter().any(|(_, answer)| *answer == EventAnswer::Recorded) {
            self.flush()?;
        }
        if let Mode::Live { world, .. } = &mut self.mode {
            world.answer(self.turn, position, &answers);
        }
        Ok(())
    }

    fn admit_event(&mut self, position: u64, delivery: &Delivery) -> EventAnswer {
        if self.events.iter().any(|event| event.delivery.id == delivery.id) {
            return EventAnswer::Duplicate;
        }
        match &delivery.inbound {
            Inbound::Cancel if !program::TAKES_CANCEL => return EventAnswer::Undeclared,
            Inbound::Cancel => {
                if self.events.iter().any(|event| event.delivery.inbound == Inbound::Cancel) {
                    return EventAnswer::Duplicate;
                }
            }
            Inbound::Steer(_) => {
                let Some((_, bound)) = program::STEER_EVENT else {
                    return EventAnswer::Undeclared;
                };
                if steer_count(&self.events) >= bound {
                    return EventAnswer::Full;
                }
            }
        }
        let Some(seq) = event_seq(self.turn, self.events.len() as u64 + 1) else {
            return EventAnswer::Full;
        };
        self.append(Record::Event {
            turn: self.turn,
            seq,
            at: position,
            delivery: delivery.clone(),
        });
        self.events.push(Recorded {
            at: position,
            delivery: delivery.clone(),
        });
        EventAnswer::Recorded
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

    pub fn run(
        mut self,
        mut state: program::State,
        arguments: &program::Arguments,
    ) -> Result<(End, program::State), Exit> {
        let end = match program::run_body(&mut self, &mut state, arguments) {
            Ok(result) => self
                .finish(&state, Outcome::Completed, result)
                .map(End::Finished),
            Err(Halt::Trap(trap)) => self
                .finish(
                    &state,
                    Outcome::Failed,
                    Json::object(vec![("trap", Json::String(trap))]),
                )
                .map(End::Finished),
            Err(Halt::Suspend(suspension)) => Ok(End::Suspended(suspension)),
            Err(Halt::Exit(exit)) => Err(exit),
        }?;
        Ok((end, state))
    }

    pub fn resume(&mut self, suspension: &Suspension, resume: &Resume) -> Result<bool, Exit> {
        let Some(SiteLog::Open(call)) = self.sites.get(&suspension.seq).cloned() else {
            panic!("a resume reaches a turn whose suspend is open");
        };
        let matched = resume.key == suspension.key;
        if matched {
            let output = Output::Ok(Json::String(resume.answer.clone()));
            self.append(Record::Result {
                turn: self.turn,
                seq: suspension.seq,
                output: output.clone(),
            });
            self.sites
                .insert(suspension.seq, SiteLog::Done(call, output));
            self.flush().map_err(halt_exit)?;
            assert!(
                self.buffer.is_empty(),
                "a resume's result is durable before it is answered"
            );
        }
        if let Mode::Live { world, .. } = &mut self.mode {
            world.resumed(suspension, resume, matched);
        }
        Ok(matched)
    }

    fn suspend_site(&mut self, seq: u64, call: Call) -> Result<Answer, Halt> {
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
                Ok(Answer::Output(output))
            }
            Some(SiteLog::Open(recorded) | SiteLog::Unknown(recorded)) => {
                self.same(seq, &recorded, &call)?;
                self.require_live(seq)?;
                let suspension = Suspension {
                    turn: self.turn,
                    seq,
                    key: recorded
                        .input
                        .get(SUSPEND_KEY)
                        .and_then(Json::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                };
                if let Some(resume) = self.world().resume(&suspension)
                    && self.resume(&suspension, &resume).map_err(Halt::Exit)?
                {
                    return Ok(Answer::Output(Output::Ok(Json::String(resume.answer))));
                }
                self.suspend(seq, &recorded)
            }
            Some(SiteLog::Reply(..)) => Err(self.diverged(seq, "a model reply", &call.tool)),
            None => {
                self.require_live(seq)?;
                self.append(Record::Call {
                    turn: self.turn,
                    seq,
                    tool: call.tool.clone(),
                    effect: call.effect,
                    input: call.input.clone(),
                });
                self.sites.insert(seq, SiteLog::Open(call.clone()));
                self.suspend(seq, &call)
            }
        }
    }

    fn suspend(&mut self, seq: u64, call: &Call) -> Result<Answer, Halt> {
        self.flush()?;
        assert!(
            self.buffer.is_empty(),
            "a suspend's call record is durable before the turn waits"
        );
        self.hook(Point::BeforeRun(Kind::Suspend, EffectClass::None))?;
        let key = call
            .input
            .get(SUSPEND_KEY)
            .and_then(Json::as_str)
            .ok_or_else(|| {
                Halt::Exit(Exit::Corrupt(format!(
                    "turn {} seq {seq}: a suspend's input lacks its key",
                    self.turn
                )))
            })?
            .to_owned();
        Err(Halt::Suspend(Suspension {
            turn: self.turn,
            seq,
            key,
        }))
    }

    fn wait_site(&mut self, site: Site, seq: u64, call: Call) -> Result<Answer, Halt> {
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &without_until(&recorded), &call)?;
                Ok(Answer::Output(output))
            }
            Some(SiteLog::Open(recorded) | SiteLog::Unknown(recorded)) => {
                self.same(seq, &without_until(&recorded), &call)?;
                self.reopen(site, seq, recorded, Run::Wait)
            }
            Some(SiteLog::Reply(..)) => Err(self.diverged(seq, "a model reply", &call.tool)),
            None => {
                self.require_live(seq)?;
                let ms = call
                    .input
                    .get(WAIT_MS)
                    .and_then(Json::as_u64)
                    .expect("a wait's input holds its duration");
                let clock = self.world().clock();
                let until = clock.checked_add(ms).ok_or_else(|| {
                    Halt::Exit(Exit::Corrupt(format!(
                        "turn {} seq {seq}: the clock {clock} plus the wait's {ms} ms passes u64",
                        self.turn
                    )))
                })?;
                let call = Call {
                    input: Json::object(vec![
                        (WAIT_MS, Json::Number(ms)),
                        (WAIT_UNTIL, Json::Number(until)),
                    ]),
                    ..call
                };
                self.append(Record::Call {
                    turn: self.turn,
                    seq,
                    tool: call.tool.clone(),
                    effect: call.effect,
                    input: call.input.clone(),
                });
                self.sites.insert(seq, SiteLog::Open(call.clone()));
                self.execute(site, seq, call, Run::Wait)
            }
        }
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
        if events_taken()
            && let Mode::Live { world, .. } = &mut self.mode
        {
            close_inbox(&mut **world, self.turn, self.seq, &self.events);
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

    pub fn cell(&mut self, code: usize, call: Call) -> Result<Answer, Halt> {
        self.seq += 1;
        self.cell_site(code, self.seq, call)
    }

    fn cell_site(&mut self, code: usize, seq: u64, call: Call) -> Result<Answer, Halt> {
        let call = Call {
            effect: EffectClass::None,
            ..call
        };
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
                return self.cell_answer(&output);
            }
            Some(SiteLog::Unknown(recorded)) => {
                self.same(seq, &recorded, &call)?;
                return Ok(Answer::Unknown);
            }
            Some(SiteLog::Reply(..)) => return Err(self.diverged(seq, "a model reply", &call.tool)),
            Some(SiteLog::Open(recorded)) => {
                self.same(seq, &recorded, &call)?;
                self.require_live(seq)?;
            }
            None => {
                self.require_live(seq)?;
                self.append(Record::Call {
                    turn: self.turn,
                    seq,
                    tool: call.tool.clone(),
                    effect: call.effect,
                    input: call.input.clone(),
                });
                self.sites.insert(seq, SiteLog::Open(call.clone()));
            }
        }
        let outcome = self.run_code(code, seq, &call)?;
        let output = outcome.output();
        self.append(Record::Result {
            turn: self.turn,
            seq,
            output: output.clone(),
        });
        self.sites.insert(seq, SiteLog::Done(call, output.clone()));
        self.cell_answer(&output)
    }

    fn run_code(&mut self, code: usize, seq: u64, call: &Call) -> Result<CellOutcome, Halt> {
        let script = call.input.as_str().expect("a cell's input is its script").to_owned();
        let existing = self.cells.get(&seq).cloned();
        if let Some(log) = &existing
            && log.finished
        {
            return Ok(CellOutcome::Unknown(divergence_text(
                log.cell,
                "it finished but its result was lost with the journal's tail",
            )));
        }
        let fresh = self.next_cell;
        assert!(fresh > 0, "a live turn knows its session's next cell id");
        let nested = self.nested.clone();
        let site = CellSite {
            code,
            session: program::HARNESS_NAME,
            turn: self.turn,
            seq,
            script: &script,
            cell: match &existing {
                Some(log) => CellStart::Existing(log),
                None => CellStart::Fresh(fresh),
            },
            nested: &nested,
        };
        if existing.is_none() {
            self.next_cell += 1;
        }
        run_cell(site, self).map_err(Halt::from)
    }

    fn cell_answer(&self, output: &Output) -> Result<Answer, Halt> {
        let outcome = CellOutcome::from_output(output).map_err(|error| self.corrupt(error))?;
        Ok(match outcome {
            CellOutcome::Done(text) => Answer::Output(Output::Ok(Json::String(text))),
            CellOutcome::Threw(error) => Answer::Output(Output::Failed(error)),
            CellOutcome::Unknown(_) => Answer::Unknown,
        })
    }

    fn world(&mut self) -> &mut dyn World {
        match &mut self.mode {
            Mode::Live { world, .. } => &mut **world,
            Mode::Replay => unreachable!("cells run only live"),
        }
    }

    pub fn effect(&mut self, site: Site, call: Call, run: Run<'_>) -> Result<Answer, Halt> {
        self.seq += 1;
        let seq = self.seq;
        if site.kind == Kind::Dispatch
            && let Some(code) = program::code_of(&call.tool)
        {
            return self.cell_site(code, seq, call);
        }
        if site.kind == Kind::Suspend {
            return self.suspend_site(seq, call);
        }
        if site.kind == Kind::Wait {
            return self.wait_site(site, seq, call);
        }
        match self.sites.get(&seq).cloned() {
            Some(SiteLog::Done(recorded, output)) => {
                self.same(seq, &recorded, &call)?;
                if site.kind == Kind::Model {
                    return Err(self.corrupt(format!("{output:?} does not fit a reply")));
                }
                if let Run::Listing(listing) = run {
                    self.admit_listing(listing, &output)?;
                }
                self.discover(&call.tool, &output);
                Ok(Answer::Output(output))
            }
            Some(SiteLog::Reply(response, recorded)) => {
                if site.kind != Kind::Model {
                    return Err(self.diverged(seq, "a model reply", &call.tool));
                }
                if recorded.as_deref() != site.mode {
                    return Err(self.diverged(
                        seq,
                        &format!("a model reply in {}", mode_text(recorded.as_deref())),
                        &format!("{} in {}", call.tool, mode_text(site.mode)),
                    ));
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
                self.side_question(seq)?;
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
        let tool = matches!(run, Run::Tool(_));
        let Mode::Live { world, .. } = &mut self.mode else {
            unreachable!("replay never runs an effect");
        };
        let output = match run {
            Run::Model { calls_max, context } => {
                let response = world.model(&ModelRequest {
                    turn,
                    seq,
                    calls_max,
                    mode: site.mode,
                    context,
                    blobs: &self.blobs,
                });
                self.hook(Point::AfterRun(site.kind, call.effect))?;
                self.drain()?;
                let mode = site.mode.map(str::to_owned);
                self.append(Record::Model {
                    turn,
                    seq,
                    response: response.clone(),
                    mode: mode.clone(),
                });
                self.sites.insert(seq, SiteLog::Reply(response.clone(), mode));
                return Ok(Answer::Reply(response));
            }
            Run::Now => Output::Ok(Json::Number(world.now())),
            Run::Suspend => unreachable!("a suspend ends the run before it would run"),
            Run::Wait => {
                let until = wait_until(&call.input).ok_or_else(|| {
                    Halt::Exit(Exit::Corrupt(format!(
                        "turn {turn} seq {seq}: a wait's input lacks its deadline"
                    )))
                })?;
                Output::Ok(Json::Number(world.sleep(turn, seq, until)?))
            }
            Run::Random => Output::Ok(Json::Number(world.random())),
            Run::Deny(reason) => Output::Failed(reason),
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
                        nested: None,
                    });
                    spawn_output(turn, seq, &call.tool, {
                        checked_output(&call.tool, returns, output)
                    })
                }
            },
        };
        let output = if tool { self.settled(&call.tool, output) } else { output };
        self.hook(Point::AfterRun(site.kind, call.effect))?;
        self.drain()?;
        self.append(Record::Result {
            turn,
            seq,
            output: output.clone(),
        });
        if let Some(listing) = listing {
            self.admit_listing(listing, &output)?;
        }
        self.discover(&call.tool, &output);
        self.sites.insert(seq, SiteLog::Done(call, output.clone()));
        Ok(Answer::Output(output))
    }

    fn settled(&mut self, tool: &str, output: Output) -> Output {
        let output = match program::search_of(tool) {
            Some(listing) => checked_search(tool, listing, self.admitted.get(&listing), output),
            None => output,
        };
        match output {
            Output::Ok(json) if program::blob_bound(tool).is_some() && host_data(&json).is_some() => {
                let data = host_data(&json).expect("matched above");
                Output::Ok(self.blobs.journaled(data))
            }
            output => output,
        }
    }

    fn discover(&mut self, tool: &str, output: &Output) {
        let (Some(listing), Output::Ok(Json::String(text))) = (program::search_of(tool), output) else {
            return;
        };
        let found = self.discovered.entry(listing).or_default();
        for name in found_names(text) {
            if let Some((_, bare)) = listed(name) {
                found.insert(bare.to_owned());
            }
        }
    }

    fn gate(&self, call: &Call) -> Result<Option<String>, String> {
        if let Json::String(raw) = &call.input
            && let ToolInput::Invalid(error) = program::tool_input(&call.tool, raw)
        {
            return Err(error);
        }
        let Some((listing, name)) = listed(&call.tool) else {
            return Ok(None);
        };
        let Some(fingerprint) = self.admitted.get(&listing).and_then(|tools| tools.get(name)) else {
            return Err(not_admitted(&call.tool, program::LISTINGS[listing].0, name));
        };
        let found = self.discovered.get(&listing).is_some_and(|found| found.contains(name));
        if program::DEFERRED[listing] && !found {
            return Err(not_discovered(&call.tool, program::LISTINGS[listing].0));
        }
        Ok(Some(fingerprint.clone()))
    }

    fn admit_listing(&mut self, listing: usize, output: &Output) -> Result<(), Halt> {
        match output {
            Output::Ok(json) => {
                let tools = admitted_tools(json).map_err(|error| self.corrupt(error))?;
                let notice = match self.listed.get(&listing) {
                    Some(before) => listing_change(program::LISTINGS[listing].0, before, &tools),
                    None => String::new(),
                };
                self.notices.insert(listing, notice);
                self.listed.insert(listing, tools.clone());
                self.admitted.insert(listing, tools);
            }
            Output::Failed(_) => {
                self.notices.insert(listing, String::new());
                self.admitted.remove(&listing);
            }
        }
        Ok(())
    }

    pub fn parallel(&mut self, calls: Vec<Call>, returns: fn(&str) -> Returns, limit: Option<u64>) -> Result<Vec<Answer>, Halt> {
        let mut answers: Vec<Option<Answer>> = (0..calls.len()).map(|_| None).collect();
        let mut pending: Vec<(usize, u64)> = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            self.seq += 1;
            let seq = self.seq;
            match self.sites.get(&seq).cloned() {
                Some(SiteLog::Done(recorded, output)) => {
                    self.same(seq, &recorded, call)?;
                    self.discover(&call.tool, &output);
                    answers[index] = Some(Answer::Output(output));
                }
                Some(SiteLog::Reply(..)) => return Err(self.diverged(seq, "a model reply", &call.tool)),
                Some(SiteLog::Unknown(recorded)) => {
                    self.same(seq, &recorded, call)?;
                    answers[index] = Some(Answer::Unknown);
                }
                Some(SiteLog::Open(recorded)) => {
                    self.same(seq, &recorded, call)?;
                    self.require_live(seq)?;
                    assert!(
                        !call.effect.may_be_unknown(),
                        "recovery turns every open external call unknown before the turn runs again"
                    );
                    pending.push((index, seq));
                }
                None => {
                    self.require_live(seq)?;
                    self.append(Record::Call {
                        turn: self.turn,
                        seq,
                        tool: call.tool.clone(),
                        effect: call.effect,
                        input: call.input.clone(),
                    });
                    self.sites.insert(seq, SiteLog::Open(call.clone()));
                    pending.push((index, seq));
                }
            }
        }
        let mut start = 0;
        while start < pending.len() {
            let alone = calls[pending[start].0].effect.may_be_unknown();
            let mut end = start + 1;
            while !alone && end < pending.len() && !calls[pending[end].0].effect.may_be_unknown() {
                end += 1;
            }
            self.batch_group(&calls, &pending[start..end], returns, limit, &mut answers)?;
            start = end;
        }
        Ok(answers
            .into_iter()
            .map(|answer| answer.expect("every call of a parallel for has an answer before its pushes"))
            .collect())
    }

    fn batch_group(&mut self, calls: &[Call], group: &[(usize, u64)], returns: fn(&str) -> Returns, limit: Option<u64>, answers: &mut [Option<Answer>]) -> Result<(), Halt> {
        let external = calls[group[0].0].effect.may_be_unknown();
        assert!(!external || group.len() == 1, "an external call of a parallel for runs alone");
        if external {
            self.flush()?;
            assert!(
                self.buffer.is_empty(),
                "every call record of a parallel for is durable before an external call of it runs"
            );
        }
        let turn = self.turn;
        let gates: Vec<Result<Option<String>, String>> = group.iter().map(|(index, _)| self.gate(&calls[*index])).collect();
        let sent: Vec<usize> = (0..group.len()).filter(|position| gates[*position].is_ok()).collect();
        let requests: Vec<ToolRequest<'_>> = sent
            .iter()
            .map(|position| {
                let (index, seq) = group[*position];
                ToolRequest {
                    turn,
                    seq,
                    tool: &calls[index].tool,
                    effect: calls[index].effect,
                    input: &calls[index].input,
                    returns: returns(&calls[index].tool),
                    admitted: gates[*position].as_ref().ok().and_then(Option::as_deref),
                    nested: None,
                }
            })
            .collect();
        let Mode::Live { world, .. } = &mut self.mode else {
            unreachable!("replay never runs an effect");
        };
        let mut completed = if requests.is_empty() { Vec::new() } else { world.tools(&requests, limit)? };
        assert_eq!(completed.len(), sent.len(), "a batch returns one output per request");
        if matches!(self.sites.get(&self.seq), Some(SiteLog::Open(_))) {
            self.drain()?;
        }
        for (position, _) in &mut completed {
            *position = sent[*position];
        }
        for (position, gate) in gates.into_iter().enumerate() {
            if let Err(refused) = gate {
                completed.push((position, Output::Failed(refused)));
            }
        }
        completed.sort_by_key(|(position, _)| *position);
        for (position, output) in completed {
            let (index, seq) = group[position];
            let output = checked_output(&calls[index].tool, returns(&calls[index].tool), output);
            let output = spawn_output(turn, seq, &calls[index].tool, self.settled(&calls[index].tool, output));
            self.append(Record::Result {
                turn,
                seq,
                output: output.clone(),
            });
            self.sites.insert(seq, SiteLog::Done(calls[index].clone(), output.clone()));
            self.discover(&calls[index].tool, &output);
            answers[index] = Some(Answer::Output(output));
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

impl CellHost for Engine<'_> {
    fn append(&mut self, record: Record) {
        Engine::append(self, record);
    }

    fn flush(&mut self) -> Result<(), Stop> {
        Engine::flush(self).map_err(halt_stop)
    }

    fn hook(&mut self, point: CellPoint) -> Result<(), Stop> {
        self.world().hook(Point::Cell(point))
    }

    fn start(&mut self, cell: u64, script: &str) -> Step {
        self.world().cell_start(cell, script)
    }

    fn resume(&mut self, cell: u64, input: Input) -> Step {
        self.world().cell_resume(cell, input)
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        self.world().tool(request)
    }

    fn clock(&mut self) -> u64 {
        self.world().now()
    }

    fn random(&mut self) -> u64 {
        self.world().random()
    }

    fn wait(&mut self, wait: &Wait) -> Result<(), Stop> {
        self.world().wait(wait)
    }

    fn observe(&mut self, event: &CellEvent) {
        self.world().observe(event);
    }
}

fn halt_stop(halt: Halt) -> Stop {
    match halt {
        Halt::Exit(Exit::Stop(stop)) => stop,
        Halt::Exit(Exit::Corrupt(error)) => panic!("a flush cannot find the journal corrupt, found {error}"),
        Halt::Trap(trap) => panic!("a flush cannot trap, found {trap}"),
        Halt::Suspend(_) => panic!("a flush cannot suspend"),
    }
}

fn halt_exit(halt: Halt) -> Exit {
    match halt {
        Halt::Exit(exit) => exit,
        Halt::Trap(trap) => panic!("finishing a turn cannot trap, found {trap}"),
        Halt::Suspend(_) => panic!("finishing a turn cannot suspend"),
    }
}

pub fn answer_text(engine: &Engine<'_>, answer: Answer) -> Result<String, Halt> {
    match answer {
        Answer::Output(Output::Ok(Json::String(text))) => Ok(text),
        Answer::Output(output) => Err(engine.corrupt(format!("{output:?} does not fit Text"))),
        Answer::Reply(_) | Answer::Unknown => panic!("a suspend site gets an output"),
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

pub fn text_contains(subject: String, separator: String) -> bool {
    subject.contains(separator.as_str())
}

pub fn text_before(subject: String, separator: String) -> String {
    match subject.split_once(separator.as_str()) {
        Some((before, _)) => before.to_owned(),
        None => subject,
    }
}

pub fn text_after(subject: String, separator: String) -> String {
    match subject.split_once(separator.as_str()) {
        Some((_, after)) => after.to_owned(),
        None => String::new(),
    }
}

pub fn text_trim(subject: String) -> String {
    const WHITESPACE: [char; 5] = [' ', '\t', '\n', '\x0c', '\r'];
    subject.trim_matches(WHITESPACE.as_slice()).to_owned()
}

pub fn mode_text(mode: Option<&str>) -> String {
    match mode {
        Some(mode) => format!("mode {mode}"),
        None => "no mode".to_owned(),
    }
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

pub fn keep_last_after<T>(list: &mut Vec<T>, count: u64, first: T, capacity: u64, field: &str) -> Result<(), Halt> {
    let length = list.len() as u64;
    let kept = count.min(length);
    if kept >= capacity {
        return Err(Halt::Trap(format!("bound({field})")));
    }
    let dropped = usize::try_from(length - kept).expect("a list's length fits usize");
    list.drain(..dropped);
    list.insert(0, first);
    assert_eq!(
        list.len() as u64,
        kept + 1,
        "keep after leaves the first entry and at most the last count entries"
    );
    Ok(())
}

pub fn with_entry<T>(list: Vec<T>, entry: T) -> Vec<T> {
    let mut items = list;
    items.push(entry);
    items
}

pub fn wait_capped(ms: u64, cap: u64, name: &str) -> Result<u64, Halt> {
    if ms > cap {
        return Err(Halt::Trap(format!("bound({name})")));
    }
    Ok(ms)
}

pub const UTF16_UNITS_PER_TOKEN: u64 = 4;
pub const TRUNCATION_OPEN: &str = "\u{2026}";
pub const TRUNCATION_CLOSE: &str = " tokens truncated\u{2026}";

pub fn utf16_len(text: &str) -> u64 {
    text.encode_utf16().count() as u64
}

pub fn truncate_middle(text: &str, tokens_max: u64) -> String {
    let total = utf16_len(text).div_ceil(UTF16_UNITS_PER_TOKEN);
    if total <= tokens_max {
        return text.to_owned();
    }
    let side = tokens_max.saturating_mul(UTF16_UNITS_PER_TOKEN / 2);
    let mut units = 0_u64;
    let head = text
        .char_indices()
        .find(|(_, character)| {
            units += character.len_utf16() as u64;
            units > side
        })
        .map_or(text.len(), |(end, _)| end);
    units = 0;
    let tail = text
        .char_indices()
        .rev()
        .find(|(_, character)| {
            units += character.len_utf16() as u64;
            units > side
        })
        .map_or(0, |(start, character)| start + character.len_utf8())
        .max(head);
    format!("{}{TRUNCATION_OPEN}{}{TRUNCATION_CLOSE}{}", &text[..head], total - tokens_max, &text[tail..])
}

pub const ATTEMPT_OK: &str = "ok";
pub const USAGE_FIELDS: [&str; 4] = ["input", "output", "cache_read", "cache_write"];
pub const ATTEMPT_USAGE: usize = 2;

pub fn usage_over(response: &Json, window: u64) -> Option<String> {
    let Json::Object(map) = response else { return None };
    let Some(Json::Array(fields)) = map.get(ATTEMPT_OK) else { return None };
    USAGE_FIELDS.iter().enumerate().find_map(|(index, name)| {
        let used = fields.get(ATTEMPT_USAGE + index)?.as_u64()?;
        (used > window).then(|| format!("model reply: {name} usage of {used} tokens exceeds the window of {window} tokens"))
    })
}

pub fn attempt_reply<T>(answer: Answer, parse: fn(&Json) -> Result<T, String>, window: u64, rejected: fn(String) -> T) -> T {
    match answer {
        Answer::Reply(response) => match parse(&response) {
            Err(error) => rejected(format!("model reply: {error}")),
            Ok(attempt) => match usage_over(&response, window) {
                Some(error) => rejected(error),
                None => attempt,
            },
        },
        Answer::Output(_) | Answer::Unknown => panic!("a model site gets a reply"),
    }
}

pub fn keep_last<T>(list: &mut Vec<T>, count: u64) {
    let length = list.len() as u64;
    if count >= length {
        return;
    }
    let dropped = usize::try_from(length - count).expect("a list's length fits usize");
    list.drain(..dropped);
    assert_eq!(
        list.len() as u64,
        count,
        "keep leaves exactly the last count entries"
    );
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
    check_events(turn, log)?;
    let finished = log
        .finished
        .as_ref()
        .ok_or_else(|| corrupt(format!("turn {turn} is replayed before it finished")))?;
    let (end, state) = Engine::new(Mode::Replay, turn, log).run(state, &arguments)?;
    let End::Finished(finish) = end else {
        return Err(corrupt(format!(
            "replay of turn {turn} suspends, where the journal says it finished"
        )));
    };
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

fn reopen_nested(log: &mut TurnLog) -> Vec<(u64, u64)> {
    let unknown = log.open_nested(|class| class.may_be_unknown());
    for id in &unknown {
        let site = log.nested.get_mut(id).expect("an open nested call has a site");
        let SiteLog::Open(call) = site else {
            panic!("open_nested returns open calls");
        };
        *site = SiteLog::Unknown(call.clone());
    }
    unknown
}

pub fn run_turn(world: &mut dyn World, epoch: u64, records: &[Record], request: &TurnRequest) -> Result<Ran, Exit> {
    run_turn_from(world, epoch, Start::Beginning, records, request)
}

pub fn run_turn_from(world: &mut dyn World, epoch: u64, start: Start, records: &[Record], request: &TurnRequest) -> Result<Ran, Exit> {
    if let Start::Checkpoint { revision, .. } = start {
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
        if events_taken() {
            close_inbox(world, request.turn, finished.last, &log.events);
        }
        return Ok(Ran::AlreadyFinished(Finish {
            turn: request.turn,
            outcome: finished.outcome,
            result: finished.result.clone(),
        }));
    }
    let state = state_before(&session, request.turn)?;
    let blobs = Blobs::of_session(&session).map_err(corrupt)?;
    blobs.check_state(&state).map_err(corrupt)?;
    let mut pending = None;
    let mut unknown = Vec::new();
    let mut nested = Vec::new();
    let log = match session.turns.get(&request.turn) {
        Some(log) => {
            if log.handler != handler || log.arguments != arguments {
                return Err(corrupt(format!(
                    "turn {} was admitted with other arguments",
                    request.turn
                )));
            }
            check_events(request.turn, log)?;
            let mut log = log.clone();
            unknown = recover(&mut log);
            nested = reopen_nested(&mut log);
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
            TurnLog::new(handler.to_owned(), Json::Null)
        }
    };
    let snapshot = program::HAS_SIDES.then(|| state.clone());
    let mut engine = Engine::new(Mode::Live { world, epoch }, request.turn, &log).with_next_cell(session.next_cell()).with_blobs(blobs);
    if let Some(snapshot) = snapshot {
        engine = engine.with_snapshot(snapshot);
    }
    if let Some(record) = pending {
        engine.append(record);
    }
    if !unknown.is_empty() || !nested.is_empty() {
        let attempt = engine.next_attempt();
        engine.append(Record::Recovery {
            turn: request.turn,
            attempt,
            unknown,
            nested,
        });
    }
    engine
        .run(state, &request.arguments)
        .map(|(end, _)| match end {
            End::Finished(finish) => Ran::Finished(finish),
            End::Suspended(suspension) => Ran::Suspended(suspension),
        })
}

pub const SIDE_TURN: u64 = 0;
pub const SIDE_REFUSED: &str = "the checker refuses this effect in a side handler";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideAnswer {
    pub outcome: Outcome,
    pub result: Json,
}

struct SideWorld<'w> {
    host: &'w mut dyn World,
    answered: Option<Finish>,
}

impl World for SideWorld<'_> {
    fn hook(&mut self, _point: Point) -> Result<(), Stop> {
        Ok(())
    }

    fn flush(&mut self, _epoch: u64, _records: &[Record]) -> Result<(), Stop> {
        Ok(())
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        assert_eq!(request.turn, SIDE_TURN, "a side question's tool calls carry the side turn");
        assert!(request.nested.is_none(), "{SIDE_REFUSED}: a side question runs no cell");
        self.host.tool(request)
    }

    fn listing(&mut self, _request: &ListingRequest<'_>) -> Output {
        panic!("{SIDE_REFUSED}: a side question reads no listing")
    }

    fn tools(&mut self, _requests: &[ToolRequest<'_>], _limit: Option<u64>) -> Result<Vec<(usize, Output)>, Stop> {
        panic!("{SIDE_REFUSED}: a side question dispatches no batch")
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        assert_eq!(request.turn, SIDE_TURN, "a side question's model calls carry the side turn");
        self.host.model(request)
    }

    fn now(&mut self) -> u64 {
        panic!("{SIDE_REFUSED}: now")
    }

    fn random(&mut self) -> u64 {
        panic!("{SIDE_REFUSED}: random")
    }

    fn acknowledge(&mut self, finish: &Finish) {
        assert!(self.answered.is_none(), "a side question is answered once");
        self.answered = Some(finish.clone());
    }

    fn cell_start(&mut self, _cell: u64, _script: &str) -> Step {
        panic!("{SIDE_REFUSED}: a code cell")
    }

    fn cell_resume(&mut self, _cell: u64, _input: Input) -> Step {
        panic!("{SIDE_REFUSED}: a code cell")
    }

    fn wait(&mut self, _wait: &Wait) -> Result<(), Stop> {
        panic!("{SIDE_REFUSED}: a code cell's timer")
    }

    fn observe(&mut self, _event: &CellEvent) {
        panic!("{SIDE_REFUSED}: a code cell")
    }

    fn inbox(&mut self, turn: u64) -> Vec<Delivery> {
        assert_eq!(turn, SIDE_TURN, "a side question reads no turn's inbox");
        Vec::new()
    }

    fn answer(&mut self, _turn: u64, _position: u64, _answers: &[(String, EventAnswer)]) {
        panic!("a side question takes no events, so it answers none")
    }

    fn clock(&mut self) -> u64 {
        panic!("{SIDE_REFUSED}: wait")
    }

    fn sleep(&mut self, _turn: u64, _seq: u64, _until_ms: u64) -> Result<u64, Stop> {
        panic!("{SIDE_REFUSED}: wait")
    }

    fn resume(&mut self, _suspension: &Suspension) -> Option<Resume> {
        panic!("{SIDE_REFUSED}: suspend")
    }

    fn resumed(&mut self, _suspension: &Suspension, _resume: &Resume, _matched: bool) {
        panic!("{SIDE_REFUSED}: suspend")
    }
}

pub fn run_side(world: &mut dyn World, state: program::State, blobs: Blobs, side: &program::SideArguments) -> Result<SideAnswer, Exit> {
    let log = TurnLog::new(program::side_name(side).to_owned(), Json::Null);
    let mut scratch = SideWorld { host: world, answered: None };
    let engine = Engine::new(Mode::Live { world: &mut scratch, epoch: 0 }, SIDE_TURN, &log).with_blobs(blobs);
    let finish = engine.run_side(state, side)?;
    assert_eq!(scratch.answered.as_ref(), Some(&finish), "a side question's finish is its answer");
    Ok(SideAnswer {
        outcome: finish.outcome,
        result: finish.result,
    })
}

pub fn last_finished(session: &Session) -> u64 {
    session
        .turns
        .iter()
        .rev()
        .find(|(_, log)| log.finished.is_some())
        .map_or(session.start.first_turn() - 1, |(turn, _)| *turn)
}

pub fn answer_side(world: &mut dyn World, start: Start, records: &[Record], side: &program::SideArguments) -> Result<SideAnswer, Exit> {
    let session = fold_from(start, records).map_err(corrupt)?;
    let state = state_before(&session, last_finished(&session) + 1)?;
    let blobs = Blobs::of_session(&session).map_err(corrupt)?;
    blobs.check_state(&state).map_err(corrupt)?;
    run_side(world, state, blobs, side)
}

pub fn self_check() {
    assert_eq!(crc32(CRC32_CHECK_INPUT), CRC32_CHECK, "CRC-32 matches its catalogued check value");
    blob_self_check();
}
