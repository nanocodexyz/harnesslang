use crate::target::Target;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mutation {
    None,
    FlushAfterDispatch,
    RecoveryRedispatchesExternal,
    VercelRetryRedispatches,
    PostgresNoFence,
    JsonlNoChecksum,
    FinishBeforeFlush,
    IdempotentReportedUnknown,
    GateReleasesEarly,
    RecoveryReportsFailed,
    RerunRewritesCall,
    CheckpointHashOrder,
    RecoverySkipped,
    EnumUnchecked,
    ModelFailureUnrecorded,
    ListingUnchecked,
    ListedClassDropped,
    InputUnchecked,
}

impl Mutation {
    pub const ALL: [Self; 18] = [
        Self::None,
        Self::FlushAfterDispatch,
        Self::RecoveryRedispatchesExternal,
        Self::VercelRetryRedispatches,
        Self::PostgresNoFence,
        Self::JsonlNoChecksum,
        Self::FinishBeforeFlush,
        Self::IdempotentReportedUnknown,
        Self::GateReleasesEarly,
        Self::RecoveryReportsFailed,
        Self::RerunRewritesCall,
        Self::CheckpointHashOrder,
        Self::RecoverySkipped,
        Self::EnumUnchecked,
        Self::ModelFailureUnrecorded,
        Self::ListingUnchecked,
        Self::ListedClassDropped,
        Self::InputUnchecked,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::FlushAfterDispatch => "flush_after_dispatch",
            Self::RecoveryRedispatchesExternal => "recovery_redispatches_external",
            Self::VercelRetryRedispatches => "vercel_retry_redispatches",
            Self::PostgresNoFence => "postgres_no_fence",
            Self::JsonlNoChecksum => "jsonl_no_checksum",
            Self::FinishBeforeFlush => "finish_before_flush",
            Self::IdempotentReportedUnknown => "idempotent_reported_unknown",
            Self::GateReleasesEarly => "gate_releases_early",
            Self::RecoveryReportsFailed => "recovery_reports_failed",
            Self::RerunRewritesCall => "rerun_rewrites_call",
            Self::CheckpointHashOrder => "checkpoint_hash_order",
            Self::RecoverySkipped => "recovery_skipped",
            Self::EnumUnchecked => "enum_unchecked",
            Self::ModelFailureUnrecorded => "model_failure_unrecorded",
            Self::ListingUnchecked => "listing_unchecked",
            Self::ListedClassDropped => "listed_class_dropped",
            Self::InputUnchecked => "input_unchecked",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub fn names() -> String {
        Self::ALL.map(Self::name).join("|")
    }

    pub fn targets(self) -> Vec<Target> {
        let all = Target::ALL.to_vec();
        match self {
            Self::None => all,
            Self::FlushAfterDispatch => vec![
                Target::NativeJsonl,
                Target::NativeSqlite,
                Target::ServerPostgres,
            ],
            Self::RecoveryRedispatchesExternal => vec![
                Target::NativeJsonl,
                Target::NativeSqlite,
                Target::ServerPostgres,
                Target::Cloudflare,
            ],
            Self::VercelRetryRedispatches => vec![Target::Vercel],
            Self::PostgresNoFence => vec![Target::ServerPostgres],
            Self::JsonlNoChecksum => vec![Target::NativeJsonl],
            Self::FinishBeforeFlush => vec![
                Target::NativeJsonl,
                Target::NativeSqlite,
                Target::ServerPostgres,
                Target::Vercel,
            ],
            Self::IdempotentReportedUnknown => vec![Target::NativeJsonl, Target::Cloudflare],
            Self::GateReleasesEarly => vec![Target::Cloudflare],
            Self::RecoveryReportsFailed => vec![
                Target::NativeJsonl,
                Target::NativeSqlite,
                Target::ServerPostgres,
                Target::Cloudflare,
            ],
            Self::RerunRewritesCall => vec![Target::NativeJsonl, Target::Cloudflare],
            Self::CheckpointHashOrder
            | Self::EnumUnchecked
            | Self::ModelFailureUnrecorded
            | Self::ListingUnchecked
            | Self::ListedClassDropped
            | Self::InputUnchecked => all,
            Self::RecoverySkipped => vec![
                Target::NativeJsonl,
                Target::NativeSqlite,
                Target::ServerPostgres,
                Target::Cloudflare,
            ],
        }
    }
}
