pub mod inbound;
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
pub const CLOUDFLARE_CONNECTIONS_WAITING_MAX: u64 = 6;
pub const VERCEL_FUNCTION_DURATION_SECONDS_MAX: u64 = 800;
pub const VERCEL_STEP_ATTEMPTS_MAX: u64 = 4;
pub const MILLISECONDS_PER_SECOND: u64 = 1000;
pub const CLOUDFLARE_ALARM_WALL_SECONDS_MAX: u64 = 15 * 60;
pub const BLOCK_DEVICE_SECTOR_BYTES: usize = 512;
pub const CLOUDFLARE_SQL_ROW_BYTES_MAX: u64 = 2_000_000;
pub const SQLITE_LENGTH_BYTES_MAX: u64 = 1_000_000_000;
pub const POSTGRES_FIELD_BYTES_MAX: u64 = 1 << 30;

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
                "a dynamic Worker isolate loaded through the object's Worker Loader binding with globalOutbound: null, whose steps are RPC calls into the object"
            }
            Self::WorkflowFunction => {
                "the workflow's steps: each replays the cell from the run's journal in a node:vm context and ends at its flush, so a nested external is a step of its own and a timer a durable sleep"
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
pub enum About {
    Journal,
    Cells,
    Inbound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fact {
    pub about: About,
    pub claim: &'static str,
    pub source: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitScope {
    Step,
}

impl WaitScope {
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Step => {
                "each wait on its own, since the run that holds it may be cut and resumed at a wait"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitLimit {
    pub limit: Limit,
    pub scope: WaitScope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub memory: Option<Limit>,
    pub wait_ms: Option<WaitLimit>,
    pub outbound_requests_per_invocation: Option<Limit>,
    pub duration_per_invocation: Option<Limit>,
    pub concurrent_requests: Option<Limit>,
    pub record_bytes: Option<Limit>,
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
    wait_ms: None,
    outbound_requests_per_invocation: None,
    duration_per_invocation: None,
    concurrent_requests: None,
    record_bytes: None,
};

const CLOUDFLARE_LIMITS: Limits = Limits {
    memory: Some(Limit {
        value: CLOUDFLARE_ISOLATE_MEMORY_BYTES_MAX,
        unit: "bytes per isolate",
        source: "developers.cloudflare.com/durable-objects/platform/limits",
    }),
    wait_ms: Some(WaitLimit {
        limit: Limit {
            value: CLOUDFLARE_ALARM_WALL_SECONDS_MAX * MILLISECONDS_PER_SECOND,
            unit: "milliseconds per wait",
            source: "developers.cloudflare.com/durable-objects/platform/limits (an alarm handler's wall time is 15 minutes; a request's is unlimited while its client stays connected); a turn a reset leaves unfinished resumes in the alarm, and a run cut at that wall time resumes from the alarm the turn keeps armed, past every wait whose deadline it recorded, so each wait must fit one alarm invocation",
        },
        scope: WaitScope::Step,
    }),
    outbound_requests_per_invocation: Some(Limit {
        value: CLOUDFLARE_SUBREQUESTS_PER_INVOCATION_MAX,
        unit: "outbound requests per invocation",
        source: "developers.cloudflare.com/workers/platform/limits (Workers Paid default)",
    }),
    duration_per_invocation: None,
    concurrent_requests: Some(Limit {
        value: CLOUDFLARE_CONNECTIONS_WAITING_MAX,
        unit: "requests waiting for response headers per invocation",
        source: "developers.cloudflare.com/workers/platform/limits (simultaneous open connections; relaxed to the wait for headers on 2026-04-09); a seventh fetch is queued by the runtime",
    }),
    record_bytes: Some(Limit {
        value: CLOUDFLARE_SQL_ROW_BYTES_MAX,
        unit: "bytes per journal row",
        source: "developers.cloudflare.com/durable-objects/platform/limits (SQLite-backed objects: maximum string, BLOB or table row size, 2 MB); a record is one row's text",
    }),
};

const SQLITE_LIMITS: Limits = Limits {
    record_bytes: Some(Limit {
        value: SQLITE_LENGTH_BYTES_MAX,
        unit: "bytes per journal row",
        source: "sqlite.org/limits.html (SQLITE_MAX_LENGTH, the default length of a string or BLOB); a record is one row's text",
    }),
    ..NO_LIMITS
};

const POSTGRES_LIMITS: Limits = Limits {
    record_bytes: Some(Limit {
        value: POSTGRES_FIELD_BYTES_MAX,
        unit: "bytes per journal row",
        source: "postgresql.org/docs/current/limits.html (field size, 1 GB); a record is one row's text",
    }),
    ..NO_LIMITS
};

const CLOUDFLARE_FACTS: &[Fact] = &[
    Fact {
        about: About::Journal,
        claim: "an outbound fetch or a response leaves only after the event's earlier writes commit",
        source: "blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three (output gates); oracle/cloudflare/storage.mjs gate_holds_outbound_fetch, consistent with it but not isolating it",
    },
    Fact {
        about: About::Journal,
        claim: "writes with no await between them commit together, and none commits while the event runs synchronously",
        source: "developers.cloudflare.com/durable-objects/api/storage-api (write coalescing); oracle/cloudflare/storage.mjs event_commits_after_it_yields",
    },
    Fact {
        about: About::Journal,
        claim: "a flush is committed once storage.sync() resolves; an awaited microtask commits nothing",
        source: "developers.cloudflare.com/durable-objects/api/storage-api (sync); oracle/cloudflare/storage.mjs sync_commits, microtask_await_does_not_commit",
    },
    Fact {
        about: About::Journal,
        claim: "a committed alarm runs after the object resets",
        source: "developers.cloudflare.com/durable-objects/api/alarms (at-least-once); oracle/cloudflare/storage.mjs alarm_runs_after_reset",
    },
    Fact {
        about: About::Journal,
        claim: "another event enters the object at any await of non-storage I/O",
        source: "blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three (input gates); oracle/cloudflare/storage.mjs event_enters_at_await",
    },
    Fact {
        about: About::Cells,
        claim: "the Worker Loader binding is experimental: workerd refuses it without --experimental, and production offers it as an open beta on Workers Paid (unverified)",
        source: "developers.cloudflare.com/workers/runtime-apis/bindings/worker-loader (search summary, not read); oracle/cloudflare/loader.mjs worker_loader_needs_experimental",
    },
    Fact {
        about: About::Cells,
        claim: "a loaded Worker reaches the object through an object stub or a ctx.exports entrypoint in WorkerCode.env; a namespace binding or an RpcTarget is refused",
        source: "oracle/cloudflare/loader.mjs object_stub_and_entrypoint_accepted, namespace_binding_refused, rpc_target_refused",
    },
    Fact {
        about: About::Cells,
        claim: "with globalOutbound: null the isolate's fetch() and connect() throw, and left out the isolate inherits the network",
        source: "oracle/cloudflare/loader.mjs null_outbound_blocks_fetch, null_outbound_blocks_connect, omitted_outbound_inherits_network",
    },
    Fact {
        about: About::Cells,
        claim: "each nested call enters the object as its own RPC event while the loading event awaits the cell, and its outbound request reached the host after its call row committed, consistent with the output gate but not isolating it",
        source: "oracle/cloudflare/loader.mjs rpc_into_object, gate_holds_nested_outbound, gate_holds_every_nested_outbound",
    },
    Fact {
        about: About::Cells,
        claim: "Durable Object RPC counts as subrequests to internal services under the per-invocation limit, so a cell's nested calls count as calls; workerd enforces no subrequest limit (unverified in production)",
        source: "developers.cloudflare.com/workers/platform/limits (search summary, not read); oracle/cloudflare/loader.mjs rpc_volume_stub, rpc_volume_exports",
    },
    Fact {
        about: About::Cells,
        claim: "the isolate has its own global scope; whether its memory counts against the object's isolate limit is not verified",
        source: "oracle/cloudflare/loader.mjs separate_global_scope",
    },
];

const VERCEL_FACTS: &[Fact] = &[
    Fact {
        about: About::Cells,
        claim: "a step function has the full Node.js runtime, so a step compiles a cell with node:vm; a workflow function runs sandboxed, without Node.js modules, fetch or timers",
        source: "workflow-sdk.dev/docs/foundations/workflows-and-steps (search summary, not read; not measured)",
    },
    Fact {
        about: About::Cells,
        claim: "sleep() is recorded in the run's event log and replays after a crash, so a cell's timer is a durable workflow sleep",
        source: "workflow-sdk.dev/docs/api-reference/workflow (sleep; search summary, not read; not measured)",
    },
    Fact {
        about: About::Cells,
        claim: "getStepMetadata().attempt counts the executions of the current step from 1, and a step runs at most maxRetries + 1 = 4 times by default; that the count is durable before the step body runs is assumed",
        source: "workflow-sdk.dev/docs/api-reference/workflow/get-step-metadata, workflow-sdk.dev/docs/foundations/errors-and-retries (search summary, not read; not measured)",
    },
    Fact {
        about: About::Cells,
        claim: "step inputs and results are serialized to the run's event log, which a resumed workflow replays",
        source: "workflow-sdk.dev/docs/foundations/workflows-and-steps (search summary, not read; not measured)",
    },
];

const VERCEL_LIMITS: Limits = Limits {
    memory: None,
    wait_ms: Some(WaitLimit {
        limit: Limit {
            value: VERCEL_FUNCTION_DURATION_SECONDS_MAX * MILLISECONDS_PER_SECOND,
            unit: "milliseconds per wait",
            source: "vercel.com/docs/functions/limitations (a Function's duration, Pro and Enterprise, fluid compute; a Workflow step runs in one invocation, while a workflow sleep() has no limit, vercel.com/docs/workflows/pricing); a wait runs inside the step after its flush",
        },
        scope: WaitScope::Step,
    }),
    outbound_requests_per_invocation: None,
    duration_per_invocation: Some(Limit {
        value: VERCEL_FUNCTION_DURATION_SECONDS_MAX,
        unit: "seconds per Function invocation",
        source: "vercel.com/docs/functions/limitations (Pro and Enterprise, fluid compute)",
    }),
    concurrent_requests: None,
    record_bytes: None,
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
            limits: SQLITE_LIMITS,
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
            limits: POSTGRES_LIMITS,
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
            facts: VERCEL_FACTS,
        },
    }
}

pub fn longest_wait(program: &Program, handler: &crate::ir::Handler) -> Option<(String, u64)> {
    handler
        .sites
        .iter()
        .filter_map(|site| match site.kind {
            crate::ir::SiteKind::Wait(constant) => Some(&program.constants[constant]),
            _ => None,
        })
        .max_by_key(|constant| constant.value)
        .map(|constant| (constant.name.clone(), constant.value))
}

pub fn check_waits(program: &Program, model: &Model) -> Vec<String> {
    let Some(WaitLimit { limit, scope }) = model.limits.wait_ms else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    for handler in &program.handlers {
        let over = match scope {
            WaitScope::Step => longest_wait(program, handler)
                .filter(|(_, value)| *value > limit.value)
                .map(|(name, value)| {
                    (
                        format!("wait({name}) of {value} milliseconds"),
                        String::new(),
                    )
                }),
        };
        if let Some((what, loops)) = over {
            errors.push(format!(
                "handler '{}': {what} exceeds {}'s limit of {} {}{loops}",
                handler.name,
                model.target.name(),
                limit.value,
                limit.unit
            ));
        }
    }
    errors
}

pub fn blob_record_bytes(bytes: u64) -> Option<u64> {
    use crate::run::blob;
    use crate::run::journal::{self, Output, Record};
    use crate::run::json::Json;
    let digest = "0".repeat(blob::DIGEST_HEX_LEN);
    let empty = Record::Result {
        turn: u64::MAX,
        seq: u64::MAX,
        output: Output::Ok(Json::object([
            (blob::BLOB_KEY, Json::text(&digest)),
            (blob::DATA_KEY, Json::text("")),
        ])),
    };
    let around = journal::to_json(&empty).encode().len() as u64;
    blob::base64_len(bytes)?.checked_add(around)
}

fn check_blobs(program: &Program, model: &Model) -> Vec<String> {
    let Some(limit) = model.limits.record_bytes else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    for tool in &program.tools {
        let crate::ir::Type::Blob(bytes) = &tool.returns else {
            continue;
        };
        let record = blob_record_bytes(bytes.value);
        if record.is_none_or(|record| record > limit.value) {
            errors.push(format!(
                "tool '{}': a result record of blob<{}> ({} bytes) holds up to {} bytes, over {}'s limit of {} {}",
                tool.name,
                bytes.name,
                bytes.value,
                record.map_or_else(|| "more than u64".to_owned(), |record| record.to_string()),
                model.target.name(),
                limit.value,
                limit.unit
            ));
        }
    }
    errors
}

pub fn check_limits(program: &Program, model: &Model) -> Vec<String> {
    let mut errors = check_waits(program, model);
    errors.extend(check_blobs(program, model));
    let Some(limit) = model.limits.outbound_requests_per_invocation else {
        return errors;
    };
    let handlers = program
        .handlers
        .iter()
        .map(|handler| ("handler", handler))
        .chain(program.sides.iter().map(|side| ("side handler", side)));
    for (kind, handler) in handlers {
        let requests = &handler.worst.requests;
        if requests.total > limit.value {
            errors.push(format!(
                "{kind} '{}': worst case {} outbound requests exceeds {}'s limit of {} {}; loops: {}",
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
        ("concurrent requests", model.limits.concurrent_requests),
    ];
    for (what, limit) in limits {
        if let Some(limit) = limit {
            lines.push(format!(
                "  limit {what}: {} {} ({})",
                limit.value, limit.unit, limit.source
            ));
        }
    }
    if let Some(WaitLimit { limit, scope }) = model.limits.wait_ms {
        lines.push(format!(
            "  limit waits: {} {}, checked on {} ({})",
            limit.value,
            limit.unit,
            scope.describe(),
            limit.source
        ));
    }
    for fact in model
        .facts
        .iter()
        .filter(|fact| fact.about == About::Journal)
    {
        lines.push(format!("  fact: {} ({})", fact.claim, fact.source));
    }
    lines.join("\n")
}

pub fn describe_cells(model: &Model) -> String {
    let mut lines = vec![format!("  cells run in: {}", model.cells.describe())];
    for fact in model.facts.iter().filter(|fact| fact.about == About::Cells) {
        lines.push(format!("  cell fact: {} ({})", fact.claim, fact.source));
    }
    lines.join("\n")
}
