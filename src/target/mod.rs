pub mod medium;

use crate::ir::Program;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    NativeJsonl,
    NativeSqlite,
    ServerPostgres,
    Cloudflare,
    Vercel,
}

impl Target {
    pub const ALL: [Self; 5] = [
        Self::NativeJsonl,
        Self::NativeSqlite,
        Self::ServerPostgres,
        Self::Cloudflare,
        Self::Vercel,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::NativeJsonl => "native-jsonl",
            Self::NativeSqlite => "native-sqlite",
            Self::ServerPostgres => "server-postgres",
            Self::Cloudflare => "cloudflare",
            Self::Vercel => "vercel",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.name() == name)
    }

    pub fn names() -> String {
        Self::ALL.map(Self::name).join("|")
    }
}

pub const CLOUDFLARE_ISOLATE_MEMORY_BYTES_MAX: u64 = 128 * 1024 * 1024;
pub const CLOUDFLARE_SUBREQUESTS_PER_INVOCATION_MAX: u64 = 10_000;
pub const VERCEL_FUNCTION_DURATION_SECONDS_MAX: u64 = 800;
pub const VERCEL_STEP_ATTEMPTS_MAX: u64 = 4;
pub const BLOCK_DEVICE_SECTOR_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flush {
    WriteAndFsync,
    Transaction,
    FencedTransaction,
    OutputGate,
    StepBoundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicUnit {
    ChecksummedRecord,
    Transaction,
    Event,
    Step,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Others {
    Nobody,
    SecondOwnerAfterLease,
    AlarmAfterReset,
    RetriedStep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    AtLoad,
    InStep { attempts_max: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub value: u64,
    pub unit: &'static str,
    pub source: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub memory: Option<Limit>,
    pub outbound_requests_per_invocation: Option<Limit>,
    pub duration_per_invocation: Option<Limit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    pub target: Target,
    pub runs_in: &'static str,
    pub journal: &'static str,
    pub flush: Flush,
    pub atomic: AtomicUnit,
    pub others: Others,
    pub recovery: Recovery,
    pub gate_before_outbound: bool,
    pub limits: Limits,
}

const NO_LIMITS: Limits = Limits {
    memory: None,
    outbound_requests_per_invocation: None,
    duration_per_invocation: None,
};

const CLOUDFLARE_LIMITS: Limits = Limits {
    memory: Some(Limit {
        value: CLOUDFLARE_ISOLATE_MEMORY_BYTES_MAX,
        unit: "bytes per isolate",
        source: "developers.cloudflare.com/durable-objects/platform/limits",
    }),
    outbound_requests_per_invocation: Some(Limit {
        value: CLOUDFLARE_SUBREQUESTS_PER_INVOCATION_MAX,
        unit: "outbound requests per invocation",
        source: "developers.cloudflare.com/workers/platform/limits (Workers Paid default)",
    }),
    duration_per_invocation: None,
};

const VERCEL_LIMITS: Limits = Limits {
    memory: None,
    outbound_requests_per_invocation: None,
    duration_per_invocation: Some(Limit {
        value: VERCEL_FUNCTION_DURATION_SECONDS_MAX,
        unit: "seconds per Function invocation",
        source: "vercel.com/docs/functions/limitations (Pro and Enterprise, fluid compute)",
    }),
};

pub const fn model(target: Target) -> Model {
    match target {
        Target::NativeJsonl => Model {
            target,
            runs_in: "a process",
            journal: "append-only JSONL file, one per session",
            flush: Flush::WriteAndFsync,
            atomic: AtomicUnit::ChecksummedRecord,
            others: Others::Nobody,
            recovery: Recovery::AtLoad,
            gate_before_outbound: false,
            limits: NO_LIMITS,
        },
        Target::NativeSqlite => Model {
            target,
            runs_in: "a process",
            journal: "SQLite rows through a host Sql",
            flush: Flush::Transaction,
            atomic: AtomicUnit::Transaction,
            others: Others::Nobody,
            recovery: Recovery::AtLoad,
            gate_before_outbound: false,
            limits: NO_LIMITS,
        },
        Target::ServerPostgres => Model {
            target,
            runs_in: "a server pool",
            journal: "Postgres rows through a host Sql",
            flush: Flush::FencedTransaction,
            atomic: AtomicUnit::Transaction,
            others: Others::SecondOwnerAfterLease,
            recovery: Recovery::AtLoad,
            gate_before_outbound: false,
            limits: NO_LIMITS,
        },
        Target::Cloudflare => Model {
            target,
            runs_in: "a Durable Object",
            journal: "the object's SQLite",
            flush: Flush::OutputGate,
            atomic: AtomicUnit::Event,
            others: Others::AlarmAfterReset,
            recovery: Recovery::AtLoad,
            gate_before_outbound: true,
            limits: CLOUDFLARE_LIMITS,
        },
        Target::Vercel => Model {
            target,
            runs_in: "a Function driving a Workflow run",
            journal: "the run's event log",
            flush: Flush::StepBoundary,
            atomic: AtomicUnit::Step,
            others: Others::RetriedStep,
            recovery: Recovery::InStep {
                attempts_max: VERCEL_STEP_ATTEMPTS_MAX,
            },
            gate_before_outbound: false,
            limits: VERCEL_LIMITS,
        },
    }
}

pub fn check_limits(program: &Program, model: &Model) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(limit) = model.limits.outbound_requests_per_invocation else {
        return errors;
    };
    for handler in &program.handlers {
        let requests = &handler.worst.requests;
        if requests.total > limit.value {
            errors.push(format!(
                "handler '{}': worst case {} outbound requests exceeds {}'s limit of {} {}; loops: {}",
                handler.name,
                requests.total,
                model.target.name(),
                limit.value,
                limit.unit,
                requests.describe_loops()
            ));
        }
    }
    errors
}

pub fn describe(model: &Model) -> String {
    let mut lines = vec![format!(
        "target {}: runs in {}; journal: {}; flush: {:?}; atomic unit: {:?}; who else may run: {:?}",
        model.target.name(),
        model.runs_in,
        model.journal,
        model.flush,
        model.atomic,
        model.others
    )];
    let limits = [
        ("memory", model.limits.memory),
        (
            "outbound requests",
            model.limits.outbound_requests_per_invocation,
        ),
        ("duration", model.limits.duration_per_invocation),
    ];
    for (what, limit) in limits {
        if let Some(limit) = limit {
            lines.push(format!(
                "  limit {what}: {} {} ({})",
                limit.value, limit.unit, limit.source
            ));
        }
    }
    lines.join("\n")
}
