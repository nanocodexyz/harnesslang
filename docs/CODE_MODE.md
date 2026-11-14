# Code mode

Code mode lets the model answer with a JavaScript cell instead of one tool
call. The cell runs on the machine the target gives it, calls the harness's
tools as nested calls, sleeps on timers and returns text. Inference, the
turn loop and durability stay in the harness. This is nanocode's Code Mode
(`crates/runtime/code-mode`, engine `crates/runtime/code-mode-engine`, djs)
with its cell journals (`code_mode/replay.rs`) and one-log rows
(`crates/runtime/agent/src/one_log.rs`, `OneLogJournals::save`), so the
same rules hold and nanocode's own crates can check harnesslang's journals.

Three rules carry over unchanged:

1. A dispatched nested `external` call is never dispatched again. Its call
   record and the cell's header are durable before it runs; a crash before
   its result is recorded delivers nanocode's unknown outcome to the script.
2. A cell interrupted by a crash is replayed, not rewritten: the engine
   re-runs the script from its header and is fed the journal's results,
   timers, clocks and random numbers in their recorded order.
3. A replay that asks for something the journal does not hold at that step
   (a divergence) stops the cell with an unknown outcome. Nothing it asks for
   after the divergence is dispatched.

## The language

A harness offers code mode with a `code` declaration: the tool's name, the
tools a cell may call, and the engine's limits, each a named constant.

    const cell_calls_max: u64 = 256
    const cell_parallel_max: u64 = 16
    const cell_timers_max: u64 = 64
    const cell_timer_ms_max: u64 = 600000
    const cell_journal_bytes_max: u64 = 33554432
    const cell_yield_ms: u64 = 10000

    tool read(path: text) -> text effect none
    tool edit(path: text, body: text) -> text effect idempotent
    tool bash(command: text) -> text effect external

    code exec {
      tools read, edit, bash
      calls cell_calls_max
      parallel cell_parallel_max
      timers cell_timers_max
      timer_ms cell_timer_ms_max
      journal_bytes cell_journal_bytes_max
      yield_ms cell_yield_ms
    }

| Field | Bounds | Source of the value |
| --- | --- | --- |
| `tools` | the nested tool set; each must be a declared tool, never the code tool itself | the program |
| `calls` | nested calls per cell, every tool together; the next call is refused with nanocode's `CALL_LIMIT` result and does not run | caller policy (nanocode's `CellGuard.calls_per_tool` is per tool; harnesslang counts all tools, one bound the prover multiplies) |
| `parallel` | nested calls running at once; more are journaled `Queued` and started in order as others finish | caller policy (nanocode's `Limits.nested`) |
| `timers` | `setTimeout` calls per cell; the next one throws in the script | caller policy |
| `timer_ms` | the longest single timer; a longer delay throws in the script | caller policy; on Vercel it is also checked against the target's sleep limit |
| `journal_bytes` | the cell journal's bytes; past it the cell is unreplayable and finishes `unknown` at its next durability point | nanocode's `JOURNAL_LIMIT_BYTES` = 32 MiB (`code-mode-engine/src/journal.rs`) |
| `yield_ms` | written into the header as `yield_time_ms` for nanocode's fold; not a wait (see Not in this spec) | nanocode's `DEFAULT_YIELD` = 10 s (`code_mode/spec.rs`) |

The declaration adds a tool named `exec` with input `text` (the script).
The model reaches it like any tool, through `dispatch(call)`, whose type
stays `maybe<text>`, or a handler calls it directly: `let out = exec(script)`
has type `maybe<text>`:

- `ok(output)`: the cell finished; `output` is what it returned and printed.
- `failed(error)`: the script threw, or a limit stopped it.
- `unknown`: the cell could not be resumed (a divergence, or its journal
  passed `journal_bytes`); nested calls it made may have run. The text the
  runtime gives the model in that case is nanocode's `divergence_text`.

A nested call whose own outcome is unknown does not make the cell unknown:
the script receives it as a failed call carrying nanocode's
`unknown_outcome()` value and decides what to do, as in nanocode.

Inside the cell the script sees `tools.<name>(input)` for each listed tool
(a promise of its text, or a rejection with the tool's error), `setTimeout`,
`Date.now()`, `performance.now()`, `Math.random()` and
`crypto.getRandomValues`, each journaled, and no network, file or process
access of its own: every effect is a nested call. A program may declare more
than one `code` tool, each with its own tool set.

Checker rules: the nested set is non-empty, names declared tools only and
not a `code` tool; every limit is a named constant; `parallel <= calls`;
`code` needs a `model`; a target whose model says `cells: unsupported`
refuses the declaration with the reason the target model gives.

## IR

- `ir::Program.codes: Vec<Code>`, `Code { name, tools: Vec<ToolIndex>,
  calls, parallel, timers, timer_ms, journal_bytes, yield_ms: Bound }`, each
  bound a constant index as loops have.
- `SiteKind::Cell(CodeIndex)`: an effect site whose class is the new
  `SiteClass::Cell` and whose durability is `Durability::Cell`. Its result
  type is `maybe<text>`. `dispatch` gains the code tools as run-time
  candidates, so `FlushBeforeRunWhenExternal` becomes "by the resolved
  class", where a cell is resolved to `Cell`.
- The site's own `call` record is written with effect `none` (a cell is
  replayable; its external work is journaled by its nested calls), so
  nanocode's `Effects` must declare each code tool `none`.
- `ir::worst` counts a cell site as `calls` nested effects, each with the
  worst class of the nested set, plus `timers` waits; outbound requests are
  `calls` for every nested tool that leaves the process (see Targets).
- `src/run/cell.rs` is the cell runner shared by the interpreter and the
  emitters' runtimes: it owns the cell journal, the replay cursor and the
  durability decisions. The engine is behind `trait CellEngine` (below).
- `src/target/mod.rs` gains `cells: CellPlacement` per target (where a cell
  runs, the table under Targets) and the platform facts it relies on.

### The engine interface

The engine is the thing that runs JavaScript. harnesslang does not contain
one: a std-only compiler cannot embed djs, and generated programs use only
their language's std and the platform's API. The interface is nanocode's
journal protocol, so djs fits it as nanocode uses it:

    start(cell_id, script) -> Step
    resume(cell_id, input: Input) -> Step
    Step = Call { id, name, input } | Timeout { id, delay_ms } | Clock
         | Monotonic | Random | Bytes { length } | Done { output } | Threw { error }
    Input = Result { id, encoded, success } | Timer { id } | Clock { value }
          | Monotonic { value } | Random { value } | Bytes { data }

Each step is one `JournalEntry` (`code-mode-engine/src/journal.rs`); the
runner journals it, answers it from the journal on replay or runs it live.
Nested call ids are the engine's, numbered from 1 in issue order within a
cell. The simulator's engine is `src/sim/cell.rs`: cells the scripted model
writes as a list of operations drawn from the seed (`call`, `parallel`,
`sleep`, `clock`, `random`, `print`, `throw`, `return`), run
deterministically, each with the JavaScript text that means the same thing so
`oracle/` can run it in djs.

## Cell journal

A cell's journal lives in the session's journal as nanocode's `cell` record:

    {"cell":{"cell":N,"fresh":B,"session":S,"update":U}}

where `U` is nanocode's `CellJournalUpdate` (`code_mode/replay.rs`) field
for field: `session_id`, `cell_id`, `seq` (contiguous per cell, from 0),
`header` on the first update only (`version` = `CELL_JOURNAL_VERSION` 1,
`origin_call_id`, `script`, `tools` (the nested tools' names and input
types), `stored` empty, `prelude` false, `yield_time_ms`,
`max_output_tokens` null), `entries_from` and `entries` (the
`JournalEntry` values appended since the last update), `calls` (the
`JournaledCall`s whose state changed: `queued`, `running`,
`completed`), `content_returned` 0, and `finished` on the last. `fresh`
is true exactly on the update carrying the header. `cell_id` counts cells
in the session from 1. `session` is the session's name.

| Record | Written | Holds | Durability |
| --- | --- | --- | --- |
| header | the cell starts | `cell` with `fresh` and `header`, `seq` 0 | rides the next flush, which C1 and C2 force |
| nested call | a nested call starts | a `Call` entry and its `running` `JournaledCall`; for an `external` tool also harnesslang's `call` record, id `<origin>/code-<id>`, effect `external` | external: flushed before dispatch (C1); none and idempotent: ride |
| nested result | a nested call returns | a `Result` entry and the call `completed`; for an `external` tool also a `result` record for its id | rides the next flush |
| queued | a call over `parallel` | the call `queued`, no entry yet; its `Call` entry is written when it starts | rides |
| timeout | the script arms a timer | a `Timeout { id, delay_ms, at_ms }` entry, `at_ms` from the world clock | flushed before the cell waits when the header is not yet durable (C2), else rides |
| timer | a timer fires | a `Timer { id }` entry | rides |
| clock, random | the script reads one | `Clock`, `Monotonic`, `Random`, `Bytes` entries | ride |
| unknown | recovery finds an open nested external call | a `Result` entry with `success` false and `encoded` = nanocode's `unknown_outcome()` (`code` `CODE_MODE_CALL_INTERRUPTED`, `outcome` `unknown`); the turn's `recovery` record lists the id | rides; the recovery record is written as today, before the handler runs again |
| finish | the engine returns `Done` or `Threw`, or the runner stops the cell | an update with `finished`, then the code site's `result` record | rides; the turn's status flush (point 2) covers it |

The origin id is the code site's call id, `turn/seq`, so nested ids are
`turn/seq/code-<id>`, nanocode's `format!("{}/code-{id}", call_id)`.
harnesslang writes the nested `call` record's `turn` (nanocode writes
`None`); serde decodes both, and the oracle's per-turn `unknown` needs it.
harnesslang writes no `forget` and no `stored` records: a finished cell
is needed until its turn's checkpoint, and nanocode's tool catalog
compaction (`TOOLS_MARK`) is a size optimisation harnesslang does not need
since its `tools` is a list of names.

### Durability points for cells

Points 1 to 3 of the architecture hold for nested calls as for top-level
ones. Two more, both nanocode's:

- **C1.** Before a nested `external` call is dispatched, every buffered
  record is durable: the cell's header, every entry before the call, its
  `Call` entry and harnesslang's `call` record. This is
  `OneLogJournals::save` committing whenever an update carries a
  `running` call whose tool `needs_row()`.
- **C2.** Before a cell waits on a timer, its header is durable. The flush
  happens when the update carries a `Timeout` and the header is new or no
  flush happened since it was buffered (nanocode's `OneLog::unsaved`, keyed
  by session and cell with the committed `seq`). A wait is where a host is
  most likely to die (an evicted object, a suspended Function); losing the
  header there makes recovery ask the model to write the cell again.

Nothing else flushes. A cell of only none and idempotent calls costs no
flush of its own: it rides the turn's status flush, as nanocode's
synchronous cells do.

### Recovery

A turn is rerun after a crash as today. At the code site:

1. The site's `result` record is in the journal: return it. The engine does
   not run (`cell_finishes_once`).
2. No header for the site's origin id is durable: the cell never reached C1
   or C2, so nothing external ran. Start the cell fresh with the next
   `cell_id`.
3. A header is durable and not finished: fold the cell's updates (nanocode's
   `CellJournal::fold`: sorted by `seq`, from the fresh update, stopping at
   a gap) and replay:
   - start the engine on the header's script and walk the entries: each step
     the engine takes must equal the next output entry under nanocode's
     `JournalEntry::same_step` (`Call`: same id, name and input;
     `Timeout`: same id and delay; `Clock`, `Monotonic`, `Random`: same
     kind; `Bytes`: same length), and each input entry (`Result`, `Timer`)
     is delivered at its recorded position. A step that does not match is a
     divergence: write `finished`, give the site `unknown`, dispatch
     nothing more.
   - a nested call that is `running` without a `Result`: if its tool is
     `external`, it was listed unknown by the turn's recovery record
     (at-load targets) and its unknown `Result` is delivered now; it is
     never dispatched. If it is `none` or `idempotent` it runs again with
     its id as the idempotency key.
   - a call that is `queued` runs normally when a `parallel` slot frees.
   - a `Timeout` without its `Timer` is re-armed for
     `at_ms + delay_ms - now`, or fires at once when that is past.
   - past the last entry the cell runs live.

Per target:

| Target | Who reports an open nested external call unknown | What is replayed |
| --- | --- | --- |
| native-jsonl, native-sqlite | the runner at load: its `recovery` record lists `Fold::unknown` of the turn, nested ids included | the cell, from the folded journal, in the rerun process |
| server-postgres | the new owner at load, after taking a new epoch; the fenced owner's next flush fails and its cell is dropped, so a nested external it had not flushed never runs | the cell, by the new owner |
| cloudflare | the object at load after a reset (the alarm, armed before the first external effect, also covers C1 and every C2 wait) | the cell, in a new isolate; nothing survives the old one |
| vercel | the nested external's own step: on attempt 2 or later it returns unknown and writes a `recovery` record for that one call, as top-level external steps do | the workflow body re-executes and the engine is replayed against step results |

A cell never asks the model for anything: subagents and inference stay in
the handler, and `spawn` or the model are not nested tools.

## Targets

| Target | Where a cell runs | What that changes |
| --- | --- | --- |
| native-jsonl, native-sqlite | the host's `CellEngine` in the harness process (djs through `nanocode-code-mode-engine` in a real host and in the drills) | a crash kills the cell with the process; group commit covers C1 and C2 like point 1 |
| server-postgres | the host's engine in the process holding the session's lease | every cell flush is fenced by the owner's epoch like any other; takeover mid-cell is a fault the simulator drives |
| cloudflare | a dynamic Worker isolate loaded through the object's Worker Loader binding, with `globalOutbound: null`, whose `tools`, timers, clocks and random numbers are RPC calls into the session's Durable Object through a stub of the object (or a `ctx.exports` entrypoint) in the loaded Worker's `env`; an `RpcTarget` or the namespace binding cannot be passed there | each nested call reaches the object as its own RPC event, interleaved with the event that awaits the cell, so it must not wait behind the object's turn serialization; that event journals the call and its output gate holds the nested external's outbound request until the C1 records commit, as for top-level effects; with `globalOutbound: null` the isolate's own `fetch()` and `connect()` throw, so nothing it does escapes the gate (left out, the isolate inherits the object's network); a timer wait arms the object's alarm at the timer's end; the isolate has its own global scope, and whether its memory is outside the object's 128 MB is not verified |
| vercel | the workflow function, which re-executes on every resume: the engine runs there and every nested call is a step (`none` and `idempotent` neighbours grouped, `external` alone with its attempt check), every timer a durable workflow sleep | the Workflow's event log is the replay source; C1 is the step boundary before the external step; `at_ms` and clocks come from the journal, never the workflow's own clock |

Cloudflare facts checked on local workerd by `oracle/cloudflare/loader.mjs`
(run by `scripts/cloudflare-drill.sh`, measurements in DECISIONS.md): the
Worker Loader binding exists and needs workerd's `--experimental` flag, an
object stub in the loaded Worker's `env` carries nested calls into the
object, `globalOutbound: null` makes `fetch()` and `connect()` throw, and
every nested call's outbound request left the object only after its call row
committed. Still unverified, to be recorded in `src/target/mod.rs` when
cells land: production availability (an open beta on Workers Paid, per a
search summary of the vendor docs) and how subrequests are counted, which
local workerd does not enforce at all. The vendor's limits page counts
Durable Object RPC as subrequests to internal services, under the same
per-invocation limit as outbound requests, so the model counts a cell's
nested calls as `calls` internal subrequests of the loaded Worker's
invocation and each nested call's outbound request against the RPC event
that makes it, not none as first assumed. Unverified for Vercel: whether
the workflow sandbox lets the engine compile a script (`node:vm` or an
equivalent) and the longest workflow sleep. Where a fact turns out false the target says
`cells: unsupported` and `hl check` refuses `code` there; at-most-once is
never weakened to make a target fit.

## Simulator

The scripted model answers with a cell call in 20% of its tool-call replies
(policy), with a script of up to `calls` operations drawn from the seed.

Faults, each at the target's existing rates:

- a crash at every cell hook: before and after the header is written, at
  every entry append, between a nested external's flush and its dispatch,
  after its dispatch and before its result, while a timer waits, between the
  last nested result and `finished`, between `finished` and the site's
  `result`;
- JSONL: a torn or lost cell record, as for any record;
- Postgres: a lease expiring mid-cell, the old owner's engine continuing;
- Cloudflare: a reset that loses the isolate;
- Vercel: a nested step retried after it ran; the Function killed between
  steps and the workflow body re-executed;
- engine divergence (1% of replays, policy): the engine takes a different
  step from the journal's, as a nondeterministic script would;
- the clock jumping forward between runs, so re-armed timers fire late or at
  once.

Invariants, beside the existing ones (each counts per nested call id):

| Invariant | Holds when |
| --- | --- |
| `nested_external_once` | every nested external call id is dispatched at most once across all runs, and only after C1's records were durable |
| `unknown_nested_reported` | a nested external call that may have been dispatched without a recorded result is delivered to the script as nanocode's unknown outcome and listed in a `recovery` record, and a call that was not dispatched or whose result is recorded never is |
| `nested_known` | every none and idempotent nested call ends with a known result |
| `cell_finishes_once` | every cell has exactly one `finished` update, the code site exactly one `result`, and the engine never starts on a finished cell |
| `cell_replay_equal` | a replay without the divergence fault matches its journal step for step; under the fault the cell finishes `unknown` and nothing after the divergence is dispatched |
| `header_before_wait` | whenever a cell waits on a timer, its header is durable |
| `cell_bounded` | nested calls, running calls, timers and journal bytes stay within the declaration's bounds; reaching one is the typed refusal the script sees, never a dropped call |

`seq_unique` extends to nested ids, and `external_once`,
`unknown_reported` and `idempotent_known` keep counting top-level calls
only, so a failure names the level it happened at.

## Proofs

The prover's plan gains the cell site: a loop of `calls` nested effects,
each a choice over the classes of the nested set, interleaved with up to
`timers` waits, and the recovery choices for each nested call (replayed,
rerun, reopened unknown) and for the cell (finished, never started, replayed,
diverged). Obligations:

| Property | Proved |
| --- | --- |
| `durable_before_external` | extended: a nested external runs only when nothing is buffered, the header and its `call` record included |
| `external_once` | extended to nested calls: a reopened nested external is never run again, on first runs, reruns and replays, Vercel's attempt check included |
| `header_before_wait` | every wait in the plan follows a flush that included the header |
| `cell_finish_once` | every path through the cell site, divergence and limits included, appends one `finished` and one site `result`, and a replayed finished cell runs nothing |
| `worst_case` | a cell site's outbound requests (`calls` times the nested tools that leave the process) and records (header, two entries and two records per nested external, one entry and one record per other call, two entries per timer, one finish) are inside the target's limits; Cloudflare counts nested outbound requests per invocation |
| `cell_bounds` | `parallel <= calls`, `timer_ms` within the target's sleep limit, `journal_bytes` at most `JOURNAL_LIMIT_BYTES` |

The arithmetic proofs see a cell's result as an unconstrained `maybe<text>`,
so a handler that pushes a cell's output needs room for one entry per cell,
as for any dispatch.

## Mutations

Each must be caught by `hl sim` and, where it is a lowering bug, by
`hl prove`; `scripts/mutations.sh` and `scripts/prove-mutations.sh` gain
them with the property each must fail with.

| Mutation | Targets | Must fail |
| --- | --- | --- |
| `cell_header_rides_past_external` (C1 flushes the call but not the header) | jsonl, sqlite, postgres | `nested_external_once` (the rerun cannot find the cell, asks the model, a new cell redispatches) |
| `nested_flush_after_dispatch` | jsonl, sqlite, postgres | `nested_external_once` |
| `cell_recovery_redispatches_external` | jsonl, sqlite, postgres, cloudflare | `nested_external_once` |
| `cell_recovery_reports_failed` (a plain failure instead of the unknown outcome) | jsonl, sqlite, postgres, cloudflare | `unknown_nested_reported` |
| `cell_replay_ignores_journal` (rerun the script live) | all | `nested_external_once` |
| `cell_rerun_after_finish` | all | `cell_finishes_once` |
| `timeout_rides` (C2 removed) | jsonl, sqlite, postgres, vercel | `header_before_wait` |
| `divergence_dispatches` (dispatch the divergent call) | all | `cell_replay_equal` |
| `nested_ids_restart` (ids from 1 on every run) | all | `seq_unique` |
| `cell_limit_unchecked` | all | `cell_bounded` |
| `vercel_nested_retry_redispatches` | vercel | `nested_external_once` |
| `cell_gate_releases_early` | cloudflare | `nested_external_once` |
| `postgres_cell_unfenced` | postgres | `nested_external_once` |

## Oracle

`oracle/` adds `nanocode-code-mode` (for `CellJournal`,
`CellJournalUpdate`, `JournaledCall`) and `nanocode-code-mode-engine`
(for `JournalEntry` and djs) at the same nanocode revision. For every
journal it checks:

- every `cell` record decodes as `nanocode_turnlog::Record::Cell` and its
  `update` as `CellJournalUpdate`, and applies to the `Fold` exactly when
  harnesslang's session fold accepts it;
- at every prefix, for every cell, `CellJournal::fold` over the fold's
  `cells()` updates consumes all of them (a `seq` gap or an
  `entries_from` past the end would make it stop early, which is a
  disagreement), and its `entries`, `calls` with states, `finished` and
  `unknown_calls()` equal the interpreter's summary for that cell;
- before a `recovery` record, its `unknown` equals `Fold::unknown` of the
  turn, nested ids included; after it, every listed nested id has an unknown
  `Result` entry in its cell before the cell finishes;
- before a nested external call is dispatched (the dump marks it), the
  cell's header and the call's `Call` entry are in the journal's durable
  prefix;
- at the end of a passing seed every cell is `finished` and its site has a
  `result`;
- the engine differential: each simulator cell's JavaScript text runs in
  djs through `nanocode-code-mode-engine` with the same results, timers,
  clocks and random numbers fed in, and the `JournalEntry` stream djs writes
  equals the simulator engine's, step for step under `same_step`; then djs
  replays that journal and must not diverge.

## Not in this spec

- Yield and `wait`: nanocode returns "Script running" after
  `yield_time_ms` so the model can work while a cell runs, and a later
  `wait` collects it. A harnesslang turn blocks on its cell; the header
  carries `yield_time_ms` only so nanocode's fold reads it. Adding yield
  means a second tool and a cell that outlives its site, which is its own
  change.
- Hands: nanocode can run a cell on another machine and route nested calls
  by `hand`. harnesslang cells run where the target puts them.
- `stored` values, the `prelude`, `max_output_tokens` and nanocode's
  per-tool observation policies (`code_mode_policy.rs` in the Claude and
  Codex backends route workspace tools and mark `Write`/`Edit` and
  `apply_patch` exclusive); harnesslang's nested tools have no workspace
  routing, so every nested call may run in parallel up to `parallel`.
