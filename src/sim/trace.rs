use std::collections::BTreeMap;
use std::fmt::Write;

use crate::effect::EffectClass;
use crate::ir::SiteKind;
use crate::run::engine::{Exit, Point, Stop};
use crate::run::json::Json;
use crate::run::turn::Ran;

#[derive(Clone, Debug, Default)]
pub struct Trace {
    pub lines: Vec<String>,
    opened: Vec<u8>,
}

impl Trace {
    pub fn event(&mut self, name: &str, fields: Vec<(&str, Json)>) {
        let mut map = BTreeMap::new();
        map.insert("event".to_owned(), Json::text(name));
        for (key, value) in fields {
            let fresh = map.insert(key.to_owned(), value).is_none();
            assert!(fresh, "a trace event's fields are distinct");
        }
        self.lines.push(Json::Object(map).encode());
    }

    pub fn opened(&mut self, name: &str, epoch: u64, bytes: &[u8]) {
        let keep = self
            .opened
            .iter()
            .zip(bytes)
            .position(|(left, right)| left != right)
            .unwrap_or(self.opened.len().min(bytes.len()));
        self.event(
            name,
            vec![
                ("epoch", Json::Number(epoch)),
                ("keep", Json::Number(keep as u64)),
                ("tail", Json::String(hex(&bytes[keep..]))),
            ],
        );
        self.opened = bytes.to_vec();
    }

    pub fn exit(&mut self, ran: &Result<Ran, Exit>) {
        let result = match ran {
            Ok(Ran::Finished(_)) => "finished",
            Ok(Ran::AlreadyFinished(_)) => "already_finished",
            Ok(Ran::Suspended(_)) => "suspended",
            Err(Exit::Stop(Stop::Crash)) => "crash",
            Err(Exit::Stop(Stop::Fenced)) => "fenced",
            Err(Exit::Corrupt(_)) => "corrupt",
        };
        self.event("exit", vec![("result", Json::text(result))]);
    }
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string succeeds");
    }
    out
}

pub fn answer(result: &Result<(), Stop>) -> Json {
    Json::text(match result {
        Ok(()) => "ok",
        Err(Stop::Crash) => "crash",
        Err(Stop::Fenced) => "fenced",
    })
}

pub fn point_fields(point: Point) -> Vec<(&'static str, Json)> {
    let run = |name: &'static str, kind: SiteKind, effect: EffectClass| {
        vec![
            ("point", Json::text(name)),
            ("kind", Json::text(kind.name())),
            ("effect", Json::text(effect.name())),
        ]
    };
    match point {
        Point::BeforeFlush => vec![("point", Json::text("before_flush"))],
        Point::AfterFlush => vec![("point", Json::text("after_flush"))],
        Point::BeforeRun(kind, effect) => run("before_run", kind, effect),
        Point::AfterRun(kind, effect) => run("after_run", kind, effect),
        Point::BeforeAcknowledge => vec![("point", Json::text("before_acknowledge"))],
        Point::Cell(cell) => match cell {
            crate::run::cell::CellPoint::BeforeNested(effect)
            | crate::run::cell::CellPoint::AfterNested(effect) => vec![
                ("point", Json::text(cell.name())),
                ("effect", Json::text(effect.name())),
            ],
            other => vec![("point", Json::text(other.name()))],
        },
    }
}
