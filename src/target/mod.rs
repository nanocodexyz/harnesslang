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
pub enum CellPlacement {
    HostProcess,
    LeaseHolder,
    WorkerIsolate,
    WorkflowFunction,
    Unsupported { reason: &'static str },
}

impl CellPlacement {
    pub const fn describe(self) -> &'static str {
        match self {
            Self::HostProcess => "the host's engine in the harness process",
            Self::LeaseHolder => "the host's engine in the process holding the session's lease",
            Self::WorkerIsolate => {
                "a dynamic Worker isolate without network whose steps are RPC calls into the object (Worker Loader, unverified)"
            }
            Self::WorkflowFunction => {
                "the re-executed workflow function, every nested call a step (script compilation in the workflow sandbox unverified)"
            }
            Self::Unsupported { reason } => reason,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub value: u64,
    pub unit: &'static str,
    pub source: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fact {
    pub claim: &'static str,
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
    pub cells: CellPlacement,
    pub facts: &'static [Fact],
}

impl Model {
    pub const fn wait_is_durable(&self) -> bool {
        matches!(self.recovery, Recovery::InStep { .. })
    }

    pub const fn wait_flushes(&self) -> bool {
        self.gate_before_outbound || matches!(self.recovery, Recovery::InStep { .. })
    }
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

const CLOUDFLARE_FACTS: &[Fact] = &[
    Fact {
        claim: "an outbound fetch or a response leaves only after the event's earlier writes commit",
        source: "blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three (output gates); oracle/cloudflare/storage.mjs gate_holds_outbound_fetch, consistent with it but not isolating it",
    },
    Fact {
        claim: "writes with no await between them commit together, and none commits while the event runs synchronously",
        source: "developers.cloudflare.com/durable-objects/api/storage-api (write coalescing); oracle/cloudflare/storage.mjs event_commits_after_it_yields",
    },
    Fact {
        claim: "a flush is committed once storage.sync() resolves; an awaited microtask commits nothing",
        source: "developers.cloudflare.com/durable-objects/api/storage-api (sync); oracle/cloudflare/storage.mjs sync_commits, microtask_await_does_not_commit",
    },
    Fact {
        claim: "a committed alarm runs after the object resets",
        source: "developers.cloudflare.com/durable-objects/api/alarms (at-least-once); oracle/cloudflare/storage.mjs alarm_runs_after_reset",
    },
    Fact {
        claim: "another event enters the object at any await of non-storage I/O",
        source: "blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three (input gates); oracle/cloudflare/storage.mjs event_enters_at_await",
    },
];

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
            cells: CellPlacement::HostProcess,
            facts: &[],
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
            cells: CellPlacement::HostProcess,
            facts: &[],
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
            cells: CellPlacement::LeaseHolder,
            facts: &[],
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
            cells: CellPlacement::WorkerIsolate,
            facts: CLOUDFLARE_FACTS,
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
            cells: CellPlacement::WorkflowFunction,
            facts: &[],
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

pub fn check_cells(program: &Program, model: &Model) -> Vec<String> {
    let CellPlacement::Unsupported { reason } = model.cells else {
        return Vec::new();
    };
    program
        .codes
        .iter()
        .map(|code| {
            format!(
                "code '{}': cells: unsupported on {}: {reason}",
                code.name,
                model.target.name()
            )
        })
        .collect()
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
    for fact in model.facts {
        lines.push(format!("  fact: {} ({})", fact.claim, fact.source));
    }
    lines.join("\n")
}
