# Decisions

## 2026-11-14: harnesslang starts from tempolang's method and nanocode's durability

harnesslang takes tempolang's method (an interpreter as the executable spec, generated code checked against it, deterministic simulation from a seed, invariants as compiler checks) and applies it to nanocode's problem: making an agent harness durable on hosts that promise different things. The journal follows nanocode-turnlog's records, the effect classes are nanocode's `EffectClass`, and the simulator's invariants, fault profiles and mutation discipline are nanocode-sim's, so that nanocode is the oracle.

The compiler is one std-only Rust crate. Generated programs are std-only too. Targets are models in `src/target/` that the checker, the lowering and the simulator share.

## 2026-11-14: the core: parser, checker, IR, replay interpreter, target models and simulator

`src/syntax` lexes and parses the language of `docs/ARCHITECTURE.md` with an explicit nesting bound (`NESTING_DEPTH_MAX` = 64, policy: deeper than any harness we write, shallow enough for the default thread stack). `src/check` resolves names and types, proves matches exhaustive (a `maybe` match without `unknown` names the reason), requires every loop and list capacity to be a named constant and lowers straight to `src/ir`. `src/ir` holds the lowered program, its validation (pair assertions: every site placed exactly once, durability follows class) and the worst-case walk. `src/run` is the replay interpreter, the journal (JSON lines with CRC-32, own JSON codec and CRC) and the session fold. `src/target` holds the five target models and the storage media the simulator drives. `src/sim` is `hl sim`. `hl emit` exists and fails with "not yet" until `src/emit` is built.

Decisions the spec left open:

- Journal lines are `{"crc":N,"record":R}`; `R` follows nanocode-turnlog's `Record` field for field so serde decodes it: `admitted` (digest = handler name, first = 1, the first sequence number of a turn, hand = null, metadata = {handler}, prompt = the arguments by name), `call`, `result`, `model`, `recovery`, `status` (always `finished`, outcome `completed` or `failed`), `checkpoint` (payload is the state as JSON text, state is the harness name, revision is the turn it follows). `call`, `result` and `model` add `seq`, which serde ignores. Call ids are `turn/seq`.
- The model writes only a `model` record; `now` and `random` write `call` and `result` with tool names `now` and `random`. Worst-case records per effect are 2 (model 1), plus admitted, status and checkpoint per turn; recovery records are one per restart and are reported apart.
- Effects are hoisted out of expressions: a statement's effects run first, left to right. `and`/`or` evaluate both sides. No shadowing. A variant constructor takes its type from the expected type, else from the one sum that has it, else it is an error naming the candidates. State fields need a zero value (u64, bool, text, list). Tools return u64, bool or text so the simulator can produce results. `reply` and `entry` need a model.
- Every finish writes a checkpoint in the same flush. State before a turn is the latest checkpoint plus replay of any finished turns after it (a torn checkpoint line is recovered by replay). The simulator also checks that replaying the whole journal from nothing gives the same state.
- Recovery is `AtLoad` (the runner turns open external calls into a `recovery` record before the handler runs) everywhere but Vercel, where it is `InStep`: the external step reads a durable attempt counter and returns `unknown` on any attempt after the first, writing a `recovery` record for that one call. Recovery attempts number per turn.
- Cloudflare commits before every outbound request (the output gate) and before the response; Vercel also commits each external step's result when the step returns; Postgres takes a new owner epoch on every run and refuses a flush from an older epoch; JSONL fsyncs on open before trusting what the page cache shows, then truncates a torn tail.
- Acknowledging a finish checks that its status is durable at that moment; that is how `finish_once` sees an early acknowledgement whose outcome happens to repeat.
- `world.medium.durable().ok()?` in the simulator treats an unreadable durable journal as "no durable status" when an acknowledgement is checked, which reports the acknowledgement as a `finish_once` violation; the unreadable journal itself is reported by `liveness`.

Simulator policy constants (src/sim): `TURNS_PER_SESSION_MAX` 4, `RESTARTS_PER_TURN_MAX` 12 (after the budget a run is fault free, so `liveness` fails only on a bug), `RUNS_PER_TURN_MAX` = restarts + 2 (the clean run and a fenced old owner), crash probability per hook 2% to 25% per seed, torn writes and crashes between write and fsync up to 30%, power loss on a crash 30% to 100% in the async and lying profiles, half of unsynced 512-byte sectors persisted in the lying profile, lease expiry 2% to 10% per hook in the Postgres lease profile, model answers 35% of the time, 25% of requested tools undeclared, 0.5% of replies over the calls bound (the handler then fails with `bound`), 15% of tool runs fail. Profiles: 3 in 10 seeds clean, the rest errors (JSONL: also async 2 in 10 and lying 2 in 10; Postgres: lease 4 in 10). JSONL "lying" here means a power loss that persists unsynced sectors out of order, the unpersisted ones reading as zeros.

Mutations (`hl sim --mutation`, `scripts/mutations.sh` runs each with `--twice` on seeds 0..200 of examples/agent.hl) and the first catching seed:

| Mutation | Targets | Caught by | Seed |
| --- | --- | --- | --- |
| flush_after_dispatch | jsonl, sqlite, postgres | external_once | 0 |
| recovery_redispatches_external | jsonl, sqlite, postgres, cloudflare | external_once | 0 |
| vercel_retry_redispatches | vercel | external_once | 0 |
| postgres_no_fence | postgres | external_once | 4 |
| jsonl_no_checksum | jsonl | liveness (the torn tail is decoded and fails) | 0 |
| finish_before_flush | jsonl, sqlite, postgres, vercel | finish_once | 0 |
| idempotent_reported_unknown | jsonl, cloudflare | idempotent_known | 44 on jsonl, 0 on cloudflare |
| gate_releases_early | cloudflare | external_once | 0 |
| recovery_reports_failed | jsonl, sqlite, postgres, cloudflare | unknown_reported | 0 |
| rerun_rewrites_call | jsonl, cloudflare | seq_unique | 44 on jsonl, 0 on cloudflare |
| checkpoint_hash_order | all | replay_equal (with --twice) | 0 or 1 |

A mutation lists only the targets where it has a path to manifest. flush_after_dispatch and finish_before_flush are lowering bugs that Cloudflare's output gate makes harmless, and the simulator agrees. idempotent_reported_unknown and rerun_rewrites_call need a none or idempotent `call` record durable without its result; on SQLite, Postgres and Vercel the two share one transaction or step, so it never happens, while a torn JSONL write and Cloudflare's commit before each outbound request produce it. checkpoint_hash_order writes the checkpoint through a std `HashMap`; its catching seed depends on the process's hash keys, the one deliberate nondeterminism in the crate.

Platform facts in `src/target/mod.rs`, all read through web search summaries of the vendor docs on 2026-11-14 rather than from the pages themselves, so treat them as unverified until someone reads the pages:

- Cloudflare Workers Paid: 10,000 subrequests per invocation by default, configurable to 10,000,000; Free: 50 (developers.cloudflare.com/workers/platform/limits, changelog 2026-02-11 "Workers are no longer limited to 1000 subrequests"). The check uses 10,000.
- Cloudflare Durable Objects: 128 MB memory per isolate, shared by objects in it (developers.cloudflare.com/durable-objects/platform/limits). Stored as 128 MiB; whether "MB" means MiB is unverified. Not checked, since text values carry no bound.
- Vercel Functions with fluid compute: 300 s default, 800 s maximum on Pro and Enterprise, 300 s on Hobby, up to 1,800 s in beta (vercel.com/docs/functions/limitations). Stored as 800 s and reported, not checked: effect durations are not modelled.
- Vercel Workflow steps: `maxRetries` defaults to 3, so 4 attempts (workflow-sdk.dev/docs/foundations/errors-and-retries). Unverified and assumed: the attempt number is durable before a step body runs, so a retried external step can tell it may have run.
- Cloudflare output gate semantics and alarm restart after a reset are taken from the spec, not re-verified. The alarm is not modelled: a restart always follows a reset.
- 512-byte sectors: the common Linux logical block size, used only by the simulator's lying profile.

Measurements (5-core Linux Hand, rustc 1.100.0):

- `cargo build --release --locked` from clean: 3.5 s; `target/release/hl` 9.5 MB with `debug = 1`, 1.0 MB stripped; Cargo.lock lists harnesslang alone; 7,382 lines of Rust in src.
- examples/agent.hl, 2,000 seeds: native-jsonl 340 seeds/s, native-sqlite 409, server-postgres 450, cloudflare 365, vercel 393 (one core each).
- Sweeps that passed: agent.hl seeds 0..20000 on every target (jsonl: 49,772 turns, 201,237 crashes, 18,852 power losses, 27,409 torn writes, 71,723 unknown results; postgres: 13,065 takeovers, 8,669 fenced flushes); reviewer.hl seeds 0..5000 on every target; every example seeds 0..2000 on every target and agent.hl 0..300 `--twice` in `scripts/check.sh`.
- `scripts/check.sh` end to end: 15 s (sweeps run one target per core).

Commands: `scripts/check.sh`, `scripts/mutations.sh`, `target/release/hl sim --target T --seeds A..B [--twice] [--mutation M] FILE`, `target/release/hl check --target T FILE`, `target/release/hl run FILE --handler NAME --arg VALUE`.
