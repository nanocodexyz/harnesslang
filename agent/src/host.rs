use std::path::{Path, PathBuf};

use harnesslang::run::journal::{Record, decode_lines};
use harnesslang::wasm::js::{Json, parse};
use harnesslang::wasm::native::{self, Journal, World};
use harnesslang::wasm::sim::Loaded;

use crate::json::Value;
use crate::{Agent, anthropic, now_ms, random, tools};

const JOURNAL_CHECKSUMS: bool = true;
const FIRST_TURN: u64 = 1;
const SUSPEND_TOOL: &str = "suspend";
const SUSPEND_KEY: &str = "key";

pub struct Finish {
    pub turn: u64,
    pub completed: bool,
    pub result: Value,
}

pub enum Ran {
    Finished(Finish),
    Suspended { turn: u64, seq: u64 },
    NotDue,
}

pub fn load(module: &[u8]) -> Loaded {
    harnesslang::wasm::sim::load(module)
        .expect("the build script emitted a module the interpreter loads")
}

pub fn to_value(json: &Json) -> Value {
    Value::decode(&json.encode()).expect("the module writes valid JSON")
}

pub fn to_json(value: &Value) -> Json {
    parse(&value.encode()).expect("the host writes valid JSON")
}

pub fn request(handler: &str, turn: u64, arguments: Value) -> Json {
    to_json(&Value::object([
        ("arguments", arguments),
        ("handler", Value::text(handler)),
        ("turn", Value::number(turn)),
    ]))
}

fn object(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn number(json: &Json, what: &str) -> Result<u64, String> {
    json.bigint()
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| format!("{what} is not a number: {}", json.encode()))
}

fn text(json: &Json) -> Option<&str> {
    match json {
        Json::Text(text) => Some(text),
        _ => None,
    }
}

pub fn output_json(ran: tools::Ran) -> Json {
    match ran {
        tools::Ran::Ok(text) => object(vec![("ok", Json::Text(text))]),
        tools::Ran::Image(bytes) => object(vec![(
            "ok",
            object(vec![(
                "data",
                Json::Text(harnesslang::run::blob::base64_encode(&bytes)),
            )]),
        )]),
        tools::Ran::Failed(text) => object(vec![("failed", Json::Text(text))]),
        tools::Ran::Job(text) => object(vec![("ok", object(vec![("job", Json::Text(text))]))]),
    }
}

pub fn records(path: &Path) -> Result<Vec<Record>, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    decode_lines(&bytes, JOURNAL_CHECKSUMS)
        .map(|decoded| decoded.records)
        .map_err(|error| format!("{}: {error}", path.display()))
}

struct Hosted<'a> {
    agent: &'a mut Agent,
    journal: PathBuf,
    finish: Option<Json>,
}

impl Hosted<'_> {
    fn suspend_key(&self, turn: u64, seq: u64) -> Result<String, String> {
        for record in records(&self.journal)? {
            if let Record::Call {
                turn: at,
                seq: site,
                tool,
                input,
                ..
            } = record
                && at == turn
                && site == seq
                && tool == SUSPEND_TOOL
            {
                return Ok(match &input {
                    harnesslang::run::json::Json::Object(map) => map
                        .get(SUSPEND_KEY)
                        .and_then(harnesslang::run::json::Json::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    _ => String::new(),
                });
            }
        }
        Err(format!(
            "the module resumes {turn}/{seq}, which has no suspend call in the journal"
        ))
    }

    fn recover(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a recovery's turn")?;
        let seq = number(request.get("seq"), "a recovery's seq")?;
        let tool = text(request.get("tool"))
            .ok_or("a recovery names its tool")?
            .to_owned();
        let input = to_value(request.get("input"));
        let recovered = self.agent.workspace.recover(turn, seq, &tool, &input);
        let name = match &recovered {
            tools::Recovered::Reply(_) => "reply",
            tools::Recovered::NotDispatched => "not_dispatched",
            tools::Recovered::Unknown => "unknown",
        };
        let call = harnesslang::run::journal::call_id(turn, seq);
        eprintln!("{}: {call} recover {tool}: {name}", self.agent.label);
        self.agent.ledger.append(Value::object([
            ("kind", Value::text("recover")),
            ("at_ms", Value::number(now_ms())),
            ("tool", Value::text(&tool)),
            ("call", Value::text(&call)),
            ("answer", Value::text(name)),
        ]));
        Ok(object(vec![(
            "answer",
            match recovered {
                tools::Recovered::Reply(ran) => object(vec![("reply", output_json(ran))]),
                tools::Recovered::NotDispatched | tools::Recovered::Unknown => Json::text(name),
            },
        )]))
    }

    fn hold(&mut self, until_ms: u64) -> Json {
        loop {
            self.agent.jobs.poll();
            let now = now_ms();
            if self.agent.jobs.pending() || !self.agent.jobs.holding() || now >= until_ms {
                return object(vec![
                    ("answer", Json::text("ok")),
                    ("clock", Json::int(now)),
                ]);
            }
            std::thread::sleep(crate::jobs::JOB_POLL);
        }
    }

    fn resume(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a resume's turn")?;
        let seq = number(request.get("seq"), "a resume's seq")?;
        let key = self.suspend_key(turn, seq)?;
        let Some(children) = self.agent.children.as_mut() else {
            return Ok(Json::Null);
        };
        Ok(match children.join(&key) {
            Some(answer) => object(vec![
                ("answer", Json::Text(answer)),
                ("key", Json::Text(key)),
            ]),
            None => Json::Null,
        })
    }

    fn inbox(&mut self) -> Json {
        let notices = self
            .agent
            .jobs
            .notices()
            .into_iter()
            .map(|notice| {
                object(vec![(
                    "job",
                    object(vec![
                        ("done", Json::Bool(notice.done)),
                        ("id", Json::Text(notice.id)),
                        ("key", Json::Text(notice.key)),
                        ("notice", Json::Text(notice.notice)),
                    ]),
                )])
            })
            .collect();
        object(vec![("answer", Json::Array(notices))])
    }
}

impl World for Hosted<'_> {
    fn tool(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a tool request's turn")?;
        let seq = number(request.get("seq"), "a tool request's seq")?;
        let tool = text(request.get("tool"))
            .ok_or("a tool request names its tool")?
            .to_owned();
        let input = to_value(request.get("input"));
        let room = matches!(request.get("job"), Json::Bool(true));
        Ok(output_json(
            self.agent.run_tool(turn, seq, &tool, &input, room),
        ))
    }

    fn model(&mut self, request: &Json) -> Result<Json, String> {
        let turn = number(request.get("turn"), "a model request's turn")?;
        let seq = number(request.get("seq"), "a model request's seq")?;
        let context = to_value(request.get("context"));
        let mode = text(request.get("mode")).map(str::to_owned);
        let effort =
            text(request.get("settings").get(anthropic::EFFORT_SETTING)).map(str::to_owned);
        let mut blobs = Vec::new();
        if let Json::Object(held) = request.get("blobs") {
            for (digest, data) in held {
                let data = text(data).ok_or("a model request's blob is base64 text")?;
                let bytes = harnesslang::run::blob::base64_decode(data)?;
                blobs.push((
                    digest.clone(),
                    tools::media_type(&bytes).unwrap_or_default().to_owned(),
                    data.to_owned(),
                ));
            }
        }
        let attached = |digest: &str| {
            blobs
                .iter()
                .find(|(held, _, _)| held == digest)
                .map(|(_, media, data)| (media.clone(), data.clone()))
        };
        let reply = self.agent.run_model(
            turn,
            seq,
            mode.as_deref(),
            effort.as_deref(),
            &context,
            &attached,
        );
        Ok(to_json(&reply))
    }

    fn now(&mut self) -> Result<u64, String> {
        Ok(now_ms())
    }

    fn random(&mut self) -> Result<u64, String> {
        Ok(random())
    }

    fn world(&mut self, request: &Json) -> Result<Json, String> {
        let event = text(request.get("event")).unwrap_or_default().to_owned();
        match event.as_str() {
            "jobs" => {
                self.agent.jobs.poll();
                let Json::Array(open) = request.get("open") else {
                    return Err("a jobs request lists its open jobs".to_owned());
                };
                let lost = open
                    .iter()
                    .filter_map(text)
                    .filter(|key| !self.agent.jobs.knows(key))
                    .map(Json::text)
                    .collect();
                Ok(object(vec![("answer", Json::Array(lost))]))
            }
            "hold" => {
                let until = number(request.get("until"), "a hold's until")?;
                Ok(self.hold(until))
            }
            "sleep" => {
                let until = number(request.get("until"), "a sleep's until")?;
                let now = now_ms();
                if until > now {
                    std::thread::sleep(std::time::Duration::from_millis(until - now));
                }
                Ok(object(vec![
                    ("answer", Json::text("ok")),
                    ("clock", Json::int(now_ms().max(until))),
                ]))
            }
            "clock" => Ok(object(vec![("answer", Json::int(now_ms()))])),
            "inbox" => Ok(self.inbox()),
            "answer" => {
                let Json::Array(answers) = request.get("answers") else {
                    return Err("an answer request lists its answers".to_owned());
                };
                for answer in answers {
                    let id = text(answer.get("id")).ok_or("an answer names its event")?;
                    let delivered =
                        matches!(text(answer.get("answer")), Some("recorded" | "duplicate"));
                    self.agent.jobs.answered(id, delivered);
                }
                Ok(Json::Object(Vec::new()))
            }
            "listing" => Ok(object(vec![(
                "output",
                object(vec![("ok", Json::Object(Vec::new()))]),
            )])),
            "recover" => self.recover(request),
            "resume" => self.resume(request),
            "resumed" => Ok(Json::Object(Vec::new())),
            other => Err(format!(
                "hl-agent answers no {other} request: {}",
                request.encode()
            )),
        }
    }

    fn acknowledge(&mut self, finish: &Json) -> Result<(), String> {
        self.finish = Some(finish.clone());
        Ok(())
    }
}

fn finish_of(json: &Json) -> Result<Finish, String> {
    Ok(Finish {
        turn: number(json.get("turn"), "a finish's turn")?,
        completed: json.get("outcome").is_text("completed"),
        result: to_value(json.get("result")),
    })
}

pub fn run(
    loaded: &Loaded,
    agent: &mut Agent,
    journal: &mut Journal,
    request: &Json,
) -> Result<Ran, String> {
    let mut hosted = Hosted {
        agent,
        journal: journal.path().to_path_buf(),
        finish: None,
    };
    let exit = native::run(loaded, journal, request, &mut hosted)?;
    match text(exit.get("exit")) {
        Some("finished") => {
            let finish = hosted
                .finish
                .take()
                .ok_or("a finished turn is acknowledged")?;
            finish_of(&finish).map(Ran::Finished)
        }
        Some("already_finished") => finish_of(exit.get("finish")).map(Ran::Finished),
        Some("suspended") => Ok(Ran::Suspended {
            turn: number(exit.get("turn"), "a suspension's turn")?,
            seq: number(exit.get("seq"), "a suspension's seq")?,
        }),
        Some("not_due") => Ok(Ran::NotDue),
        Some("corrupt") => Err(format!(
            "the journal is corrupt: {}",
            exit.get("error").encode()
        )),
        _ => Err(format!("the module exited {}", exit.encode())),
    }
}

pub fn idle_due(
    loaded: &Loaded,
    agent: &mut Agent,
    journal: &mut Journal,
) -> Result<Option<u64>, String> {
    let mut hosted = Hosted {
        agent,
        journal: journal.path().to_path_buf(),
        finish: None,
    };
    native::idle(loaded, journal, &mut hosted)
}

pub fn next_turn(journal: &Journal) -> Result<u64, String> {
    let last = records(journal.path())?
        .iter()
        .filter_map(|record| match record {
            Record::Admitted { turn, .. } => Some(*turn),
            _ => None,
        })
        .max();
    Ok(last.map_or(FIRST_TURN, |turn| turn + 1))
}
