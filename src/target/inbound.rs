use crate::target::{Fact, Target};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drain {
    AfterRun,
    AfterFlush,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inbound {
    pub path: &'static str,
    pub drain: Drain,
    pub ack: &'static str,
    pub facts: &'static [Fact],
}

const STD_CHANNEL: Fact = Fact {
    claim: "a receiver's try_recv never blocks, and each sender's messages arrive in the order it sent them",
    source: "doc.rust-lang.org/std/sync/mpsc (Receiver::try_recv, Sender)",
};

const NATIVE_FACTS: &[Fact] = &[STD_CHANNEL];

const POSTGRES_FACTS: &[Fact] = &[
    STD_CHANNEL,
    Fact {
        claim: "only the lease owner's flush commits, so an event recorded by a fenced owner is never answered",
        source: "the fenced transaction of this target's model (src/target/mod.rs); scripts/rust-sql-drill.sh takeover drill",
    },
];

const CLOUDFLARE_FACTS: &[Fact] = &[
    Fact {
        claim: "every request for one object id reaches the one live instance of that object",
        source: "developers.cloudflare.com/durable-objects (what are Durable Objects: one instance per id)",
    },
    Fact {
        claim: "an event's fetch enters the object while the turn awaits a tool or the model",
        source: "blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three (input gates); oracle/cloudflare/storage.mjs event_enters_at_await",
    },
];

const VERCEL_FACTS: &[Fact] = &[
    Fact {
        claim: "resumeHook(token, payload) resolves only once the payload is durably recorded as hook_received in the run's event log",
        source: "workflow-sdk.dev (hooks: createHook, resumeHook), read through a web search summary on 2026-11-14",
    },
    Fact {
        claim: "a replayed workflow function receives each hook payload once, at the same place among its step results",
        source: "workflow-sdk.dev (event sourcing and replay; hook_received deduplicated by resumeId), read through a web search summary on 2026-11-14; assumed, not measured",
    },
];

pub const fn of(target: Target) -> Inbound {
    match target {
        Target::NativeJsonl | Target::NativeSqlite => Inbound {
            path: "the host's HTTP endpoint sends each event into a std::sync::mpsc channel the turn drains",
            drain: Drain::AfterRun,
            ack: "after the flush that makes the event record durable",
            facts: NATIVE_FACTS,
        },
        Target::ServerPostgres => Inbound {
            path: "the lease owner's HTTP endpoint sends each event into a std::sync::mpsc channel the turn drains; another server answers not_running",
            drain: Drain::AfterRun,
            ack: "after the fenced flush that makes the event record durable",
            facts: POSTGRES_FACTS,
        },
        Target::Cloudflare => Inbound {
            path: "POST /event to the session's Durable Object, answered once the turn records it",
            drain: Drain::AfterRun,
            ack: "after storage.sync() commits the event record",
            facts: CLOUDFLARE_FACTS,
        },
        Target::Vercel => Inbound {
            path: "resumeHook on the turn's events hook; the workflow function records payloads between steps",
            drain: Drain::AfterFlush,
            ack: "by an answer step, after the workflow function appends the event record",
            facts: VERCEL_FACTS,
        },
    }
}

pub fn describe(target: Target) -> String {
    let inbound = of(target);
    let drain = match inbound.drain {
        Drain::AfterRun => "after every effect it runs",
        Drain::AfterFlush => "at every step boundary",
    };
    let mut lines = vec![
        format!("  events: {}", inbound.path),
        format!(
            "  events: a turn records them {drain}, answered {}",
            inbound.ack
        ),
    ];
    for fact in inbound.facts {
        lines.push(format!("  fact: {} ({})", fact.claim, fact.source));
    }
    lines.join("\n")
}
