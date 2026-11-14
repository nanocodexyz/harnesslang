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
    EventRedelivered,
    EventAckEarly,
    EventReadUnstamped,
    EventDrainOnReplay,
    BatchDrainsReplayed,
    ModelFailureUnrecorded,
    RetireSkipped,
    SealSkipped,
    ListingUnchecked,
    ListedClassDropped,
    InputUnchecked,
    BatchFlushAfterDispatch,
    ResultsInCompletionOrder,
    BatchPushesReversed,
    LoopPastBound,
}

impl Mutation {
    pub const ALL: [Self; 29] = [
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
        Self::EventRedelivered,
        Self::EventAckEarly,
        Self::EventReadUnstamped,
        Self::EventDrainOnReplay,
        Self::BatchDrainsReplayed,
        Self::ModelFailureUnrecorded,
        Self::RetireSkipped,
        Self::SealSkipped,
        Self::ListingUnchecked,
        Self::ListedClassDropped,
        Self::InputUnchecked,
        Self::BatchFlushAfterDispatch,
        Self::ResultsInCompletionOrder,
        Self::BatchPushesReversed,
        Self::LoopPastBound,
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
            Self::EventRedelivered => "event_redelivered",
            Self::EventAckEarly => "event_ack_early",
            Self::EventReadUnstamped => "event_read_unstamped",
            Self::EventDrainOnReplay => "event_drain_on_replay",
            Self::BatchDrainsReplayed => "batch_drains_replayed",
            Self::ModelFailureUnrecorded => "model_failure_unrecorded",
            Self::RetireSkipped => "retire_skipped",
            Self::SealSkipped => "seal_skipped",
            Self::ListingUnchecked => "listing_unchecked",
            Self::ListedClassDropped => "listed_class_dropped",
            Self::InputUnchecked => "input_unchecked",
            Self::BatchFlushAfterDispatch => "batch_flush_after_dispatch",
            Self::ResultsInCompletionOrder => "results_in_completion_order",
            Self::BatchPushesReversed => "batch_pushes_reversed",
            Self::LoopPastBound => "loop_past_bound",
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
            | Self::EventReadUnstamped
            | Self::EventDrainOnReplay
            | Self::ModelFailureUnrecorded
            | Self::RetireSkipped
            | Self::SealSkipped
            | Self::ResultsInCompletionOrder
            | Self::BatchPushesReversed
            | Self::ListingUnchecked
            | Self::ListedClassDropped
            | Self::InputUnchecked
            | Self::LoopPastBound => all,
            Self::EventRedelivered | Self::EventAckEarly | Self::BatchDrainsReplayed => vec![
                Target::NativeJsonl,
                Target::NativeSqlite,
                Target::ServerPostgres,
                Target::Cloudflare,
            ],
            Self::BatchFlushAfterDispatch => vec![
                Target::NativeJsonl,
                Target::NativeSqlite,
                Target::ServerPostgres,
                Target::Vercel,
            ],
            Self::RecoverySkipped => vec![
                Target::NativeJsonl,
                Target::NativeSqlite,
                Target::ServerPostgres,
                Target::Cloudflare,
            ],
        }
    }
}
