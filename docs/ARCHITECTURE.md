# harnesslang architecture

## Goal

An agent harness is a loop around a model: admit a prompt, call the model, run
the tools it asks for, feed the results back, finish. Making that loop durable
is most of the work in nanocode, and the right way to do it differs by where
the loop runs. A Durable Object's writes are atomic per event and gate its
outgoing requests. A Vercel Workflow step runs at least once and its result is
recorded once it returns. A Postgres session can have two owners after a lease
expires. A JSONL file can lose its unsynced tail or tear its last line.

harnesslang is a language for harnesses whose compiler knows those guarantees.
A program says what the harness does. A target says where it runs and what that
place promises about durability. The compiler checks the program against the
target and emits the code that is best there: native Rust over a JSONL file,
SQLite or Postgres, a Durable Object class over its SQLite, or a Vercel Workflow.

## Principles

These come from tempoxyz/tempolang and nanocodexyz/nanocode.

1. **Invalid harnesses do not compile.** A dispatched external effect that
   crashed before its result was recorded has an unknown outcome. Its type says
   so: an `external` tool returns `maybe<T>` (`ok`, `failed` or `unknown`), and
   a `match` that leaves out `unknown` is a compile error. "Never silently
   replay dispatched work" is a type rule, not a review comment.
2. **The spec is the code.** One source gives an interpreter (the executable
   spec and the oracle) and generated code for each target. Generated code is
   checked against the interpreter by differential simulation, seed by seed.
3. **Deterministic by construction.** Time, randomness, model calls and tools
   go only through effects the runtime journals, so a handler
   re-executed over its journal takes the same path. Recovery is replay.
4. **Bounded by construction.** Every loop has a bound and every list a
   capacity, named for what it bounds. The checker computes each handler's
   worst case (effects, journal records, outbound requests), where a `return`
   ends its path, and checks outbound requests against the target's limit
   where the target has one.
5. **The target is a model.** A target is a durability model (what survives a
   crash, what is atomic, who else may run) and a place (memory, outbound
   requests, wall time). The same model drives the checker, the lowering and
   the simulator's faults.
6. **Zero dependencies.** The compiler, interpreter, simulator and every
   generated program use only the standard library of their language. Code that
   checks harnesslang against outside software (nanocode's own crates) lives in
   `oracle/`, its own workspace, never in the build.

## The language

A program is one harness: constants, types, tools, a model, session state,
invariants and handlers. This is `examples/agent.hl`:

```hl examples/agent.hl
harness agent

const turn_steps_max: u64 = 64
const calls_per_step_max: u64 = 16
const context_entries_max: u64 = 4096
const turns_max: u64 = 1000000

type outcome = completed(text) | failed(text) | exhausted

tool read(path: text) -> text effect none
tool edit(path: text, body: text) -> text effect idempotent
tool bash(command: text) -> text effect external

model infer calls calls_per_step_max

state {
  context: list<entry, context_entries_max>
  turns: u64
}

invariant turns_bounded: turns <= turns_max
invariant context_bounded: len(context) <= context_entries_max

on prompt(input: text) -> outcome
  requires turns < turns_max
    and len(context) + 1 + turn_steps_max * (1 + calls_per_step_max) <= context_entries_max
{
  push context user(input)
  turns = turns + 1
  loop step below turn_steps_max {
    let reply = infer(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return completed(words) }
      calls(requested) => {
        for call in requested {
          match dispatch(call) {
            ok(output) => { push context result(call, output) }
            failed(error) => { push context error(call, error) }
            unknown => { push context unknown(call) }
          }
        }
      }
    }
  }
  return exhausted
}
```

### Values and types

- `u64`, `bool`, `text`. Text has `++` and four pure builtins that never index
  characters (`contains`, `before`, `after`, `trim`). Arithmetic on `u64` traps on overflow and underflow;
  the trap is a typed failure of the handler, never a wrapped value.
- Sum types: `type name = variant(type, ...) | ...`. Variants carry only what
  that case has.
- Text enums: `type name = "value" | ...`, a sum of fieldless values lowered as
  `ir::Sum` with `SumKind::Text`, written in the journal as the text. Matched
  by value, exhaustively; a text literal is a value where the enum is
  expected.
- `blob<N>`: at most `N` opaque bytes a tool returns, journaled by their
  SHA-256 digest with the bytes in the first result that holds them; it rides
  in lists and context entries (`attached`) and `keep L blobs last N` evicts
  the oldest as nanocode's image window does.
- `list<T, N>`: a list with capacity `N`, a named constant. `push` onto a full
  list fails the handler with `bound(name)`, naming the list.
- State fields start at zero (`0`, `false`, empty text, a text enum's first
  value, an empty list), so they are `u64`, `bool`, `text`, text enums or
  lists. A tool returns `u64`, `bool`, `text` or a text enum; every backend
  records a result that does not fit as the call's failure before it is
  journaled. In a pattern, `_` ignores a field.
- Builtins every program has:
  - `call = call(id: text, tool: text, input: text)`, with fields `id`, `tool`, `input`
  - `reply = answer(text) | calls(list<call, M>)`, where `M` is the model's
    `calls` bound, unless the model declares its own reply type
    (`model name calls M reply type`), whose one variant carrying
    `list<call, M>` the worst case multiplies
  - `entry = user(text) | assistant(reply) | result(call, text) | error(call, text) | unknown(call)`
  - `known<T> = ok(T) | failed(text)` and `maybe<T> = ok(T) | failed(text) | unknown`
  - with `model name calls M window W`, a model call returns `attempt = ok(reply, stop, u64, u64, u64, u64) | throttled(text, u64) | transient(text, u64) | rejected(text)`: the reply, the text enum `stop` (`end`, `tool_use`, `max_tokens`, `refusal`, `context_exceeded`, `pause`) and the input, output, cache read and cache write tokens, each at most `W`, or a provider error with its `Retry-After` in milliseconds; the context may be `list with e`, one more entry the list never holds
  - `len(t)` of a text counts UTF-16 code units, and `truncate_middle(t, n)`, `n` a constant, keeps the head and tail of a text over `n` estimated tokens, at most `4n + 39` code units, which the prover knows

### Effects

An effect is anything whose result the program cannot recompute: a tool, the
model, `now()`, `random()`, `wait(c)` (and `wait(e) at most c`) and `suspend(key)`. Each effect site is
journaled. A `spawn` tool (`spawn name runs handler from "file"`) is an external
tool whose `ok` result the runtime records as the child's key, its own turn
and sequence number `T.S`; the host keeps the child's session under the
parent's and that key, and `join(key)`, a suspend, ends the parent's run until
the child's completion resumes it with the child's result.

| Class | Declared by | Result type | On recovery, when its result was not recorded |
| --- | --- | --- | --- |
| `none` | `effect none`, `now()`, `random()`, `wait(c)`, `suspend(key)` | `known<T>` (`now`, `random` and `wait` return `u64`, `suspend` returns `text`) | run again; a wait sleeps only until its recorded deadline, and an open suspend waits again without a second `call` |
| `idempotent` | `effect idempotent`, the model | `known<T>` (the model returns `reply`, or `known<R>` for a declared reply type `R`, whose `failed` is a provider failure or a reply that does not fit) | run again with the same idempotency key |
| `external` | `effect external`, `dispatch` of a tool the program does not declare | `maybe<T>` | never run again: the result is `unknown` |

These are nanocode's `EffectClass` (`None`, `Idempotent`, `External`), and an
undeclared tool defaults to `external` as it does there. `dispatch(call)` looks
the call's tool up among the declared tools at run time and always has type
`maybe<text>`; a declared none or idempotent tool reached through it never
yields `unknown`.

A tool's parameters are its input type; a parameter may declare a default
(`timeout: u64 = timeout_ms`), which `dispatch` fills into input that leaves it
out before the check, so the journal holds the filled object. A model may
declare call modes (`model m calls N modes text_only`), and `m(context) mode
text_only` passes the mode to the host and journals it with the reply. `dispatch` checks the call's input
text against them before the tool runs: a JSON object holding exactly the
parameters, each of its type, is journaled as that object and sent; anything
else is journaled as the text and recorded as the call's failure without
running. `tools from listing(name, bound) (params) effect class` declares a
family of tools named `name__TOOL` at run time (nanocode's `mcp__SERVER__TOOL`)
with one class and one input type, so `dispatch`'s class stays a function of
the program and the tool's name. `name()` reads the listing as a `none` effect
whose recorded result is the listing (tool name to fingerprint) and admits
its tools for the rest of the turn. `dispatch` refuses a listed tool the
latest read did not list, and sends the admitted fingerprint with the call so
the host refuses a tool that changed since admission, as nanocode's MCP host
does (`native_call_admitted`); both refusals are recorded failures.

### Statements

`let`, assignment to a state field, `push list value`, `clear list`,
`keep list last n` (drop all but the last `n` entries), `keep list last n after e` (the same, with `e` put first: a summary replacing what it summarizes), `if` / `else`, `match`
(exhaustive), `loop name below N` (`N` a constant), `for name in list`,
`for call in list parallel` (one `match dispatch(call)` whose arms only push, bind and assign:
every call record is appended before any dispatch, none and idempotent calls
run at once, an external call alone after one flush of the batch, results in
list order),
`return value`, and a call of a `fn`, whose body the checker inlines at each
call before lowering, in a scope that sees only its parameters and the
harness's declarations. There is no recursion (a fn may not call itself,
directly or through others) and no unbounded loop: a handler is straight-line
code over bounded loops, so its worst case is a number the checker computes.

### Hooks

`before tool` and `after tool` are inlined at every `match dispatch(...)`, and the
lifecycle hooks (`on session start`, `on prompt submit(p: T)`, `on pre compact`,
`on post compact`, `on stop`, `on subagent start`, `on subagent stop` and the
handler `on session end`) at the points they name, as a fn is inlined. A
decision lowers to hidden state fields (`_hook_decided`, `_hook_denied`,
`_hook_reason`, `_hook_input`), so a hook's code appears once per dispatch: a
`deny` in `before tool` is a `none` effect site that records the call as tool
`deny` and its `failed` result without contacting the host, `allow(input)`
dispatches the rewritten input, and a `deny` in `after tool` gives the match a
`failed` outcome. Statements after an `if` or `match` that may decide run under
`if not _hook_decided`.

### Events

`on cancel` and `on steer(message: text) bound N` declare the events a running
turn takes from outside it. `cancelled` and `take steers` (a `list<text, N>`)
read them in a handler's body, at no record and no sequence number. The
runtime appends one `event` record (nanocode-turnlog's `Event`) per event that
reaches a running turn, with `at`, the sequence number of the effect it
arrived during; a read between effects `s` and `s + 1` sees the events with
`at <= s`. Events enter only while an effect runs (on Vercel, at a step
boundary), and a re-run takes none at the effects it replays, so replay shows
every read what it saw. Events that arrive during a parallel for's calls are
recorded at its last call's sequence number, and only when that call runs in
this run: a re-run that finishes a batch whose last call is already in the
journal (its external calls recovered as unknown) leaves them for the next
effect. The sender is answered once the record is durable.
LANGUAGE.md has the details and `src/target/inbound.rs` each target's delivery
path with its platform facts.

### Code mode

`code exec { tools read, edit, bash  calls cell_calls_max  parallel ...  timers ...  timer_ms ...  journal_bytes ...  yield_ms ... }`
offers a tool through which the model writes a JavaScript cell that calls the
listed tools as nested calls. `exec(script)` and `dispatch` of it have type
`maybe<text>`: `unknown` only when the cell could not be resumed. A nested
external call is journaled and flushed with the cell's header before it runs
and is never dispatched twice; an interrupted cell is replayed from its
journal, and an open nested external call is delivered to the script as
nanocode's unknown outcome. [CODE_MODE.md](CODE_MODE.md) is the whole design:
construct, IR, cell journal records and durability points C1 and C2, recovery
and placement per target, simulator invariants, proofs, mutations and oracle.

## Execution model

A session is a journal. A handler runs as ordinary code; each effect site gets
the next sequence number within its turn. The runtime looks the sequence number
up in the journal: if the result is there it is returned without running the
effect, and if not the effect runs and its result is appended. State is not
journaled: it is a function of the journal, recomputed by replay. A checkpoint
of the state at a finished turn bounds how far back replay goes.

Journal records follow nanocode's turn log (`nanocode-turnlog`), so a harnesslang
journal and a nanocode journal fold the same way:

| Record | Written | Holds |
| --- | --- | --- |
| `admitted` | a handler starts | turn, handler, arguments |
| `call` | before an effect runs | sequence, tool, effect class, input |
| `result` | after it returns | sequence, `ok` or `failed` and the value |
| `model` | after the model returns | sequence, the reply, or `{"failed": error}` for a failed call to a model with a declared reply type, and the call's `mode` when it has one |
| `recovery` | a recovery finds open external calls | turn, attempt, the unknown sequences |
| `status` | the turn finishes | turn, outcome |
| `checkpoint` | after a finish, by policy | revision, state |
| `cell` (code mode) | a code cell's header, entries, nested call states and finish, as nanocode's `CellJournalUpdate` | cell, session, update |

### Durability points

Records are buffered and flushed at durability points, which the compiler
places:

1. Before an `external` effect runs, every buffered record is durable,
   including its `call` record. A crash after the flush and before the result
   leaves an open `call`, which recovery turns into `unknown`.
2. Before a turn's outcome is acknowledged, its `status` record is durable.
3. Before a `wait` sleeps and before a `suspend` ends the run, every buffered
   record is durable, including the wait's or suspend's `call` record, so a
   host evicted during a wait, or a turn that waits for hours on a person,
   loses nothing before it. A resume's `result` is durable before its
   sender is answered and before the turn goes on.
4. Nothing else forces a flush. Results of `none` and `idempotent` effects and
   model replies ride the next flush; losing them in a crash only repeats work
   that is safe to repeat.

How a flush happens is the target's business, and is where targets differ most.

### Journal encoding

Each record is one line: `{"crc":N,"record":R}`, where `R` is the record as
nanocode-turnlog's serde writes it (externally tagged, snake_case, keys
sorted) and `N` is the CRC-32 (ISO-HDLC) of `R`'s bytes. Call ids are
`turn/seq`, turns are decimal text, `call`, `result` and `model` also carry
`seq`, and a checkpoint's `payload` is the state as JSON text. A line whose
checksum fails or that lacks its newline ends the readable journal: it and
everything after it are dropped. Row targets store the same `R` per row.

## IR

`check` lowers a program to `ir::Program`, which the interpreter runs and the
emitters read. Names are resolved to indices: constants, sums, tools, state
fields, and per handler its locals and effect sites. Types are explicit: `u64`,
`bool`, `text`, a sum by index (`call`, `reply` and `entry` first), `list`
with its named capacity, and `known`/`maybe` with fixed variants `ok`,
`failed`, `unknown`.

A handler is a tree of statements: `Let`, `Assign`, `Push` and `Shrink`
(`clear` and `keep`, which carry what they keep) on state fields, `If`, `Match` with one arm per variant in variant order, `Loop` and
`For` with their named bound, `Parallel` (a `For` over calls whose one
dispatch site runs as a batch, with its push-only arms), `Effect` and `Finish`. Expressions are pure:
lowering hoists every effect out of the expression it appears in into an
`Effect` statement that binds a temporary local, so a statement's effects run
first, left to right, then its expression. `and` and `or` evaluate both sides.

Each effect site has a kind (a tool, `dispatch`, the model, `now`, `random`,
`wait` with its constant, `suspend`),
a class (fixed, or resolved at run time for `dispatch`), a result type and a
durability: `RidesNextFlush`, `FlushBeforeRun` (external, and every wait and suspend), or
`FlushBeforeRunWhenExternal` (dispatch). An effect runs as: take the next
sequence number; return the journaled result if there is one; otherwise append
its `call` record (the model writes none), flush if its durability says so,
run it, append its `result` or `model` record. `Finish` appends the `status`
and `checkpoint` records, flushes, then acknowledges: durability point 2. A
trap ends the turn the same way with outcome `failed`. A target may flush
more often than the IR asks (the output gate before every outbound request, a
step boundary after every external step) but never less.

A suspend site ends the run instead of running: after its flush the handler
returns `Suspended { turn, seq, key }` to the host and appends no `status`; the
turn's waiting status is that open `call`. The host runs the turn again with a
resume, `{key, answer}`; the run replays to the suspend, which records the
answer as its `result` when the key matches and waits again when it does not.
A wait reads the clock, appends `{"ms":c,"until":T}`, flushes and sleeps until
`T`; its `result` is the clock at its wake. [LANGUAGE.md](LANGUAGE.md) has both.

Code mode adds a code site: `SiteKind::Cell` with class `Cell` and durability `Cell`: its
own `call` record is `none`, and its nested calls follow points 1 to 3 plus
C1 (header durable before a nested external) and C2 (header durable before a
timer wait); see [CODE_MODE.md](CODE_MODE.md).

Each handler also carries its worst case: effects, journal records,
outbound requests and milliseconds of waits, each with the chain of loops that
multiplies into it. A
`Finish` ends its path: a loop whose iteration may return counts the
iterations before it in full and the last one up to its return, and code after
a statement every path of which returns adds nothing.

## Targets

A target is a durability model and a place.

| Target | Runs in | Journal | Flush | Atomic unit | Who else may run | Limits checked |
| --- | --- | --- | --- | --- | --- | --- |
| `native-jsonl` | a process | append-only JSONL file, one per session | write and fsync | one record (checksummed; a torn tail is dropped) | nobody | the program's own |
| `native-sqlite` | a process | SQLite rows through a host `Sql` | one transaction | one transaction | nobody | the program's own |
| `server-postgres` | a server pool | Postgres rows through a host `Sql` | one transaction fenced by the owner's epoch | one transaction | a second owner after a lease expires; the fence refuses its writes | the program's own |
| `cloudflare` | a Durable Object | the object's SQLite | `storage.sync()`, which the output gate already waits for before the outbound request or response that follows every flush | one event | nobody else (one object per session), though another event enters it at any await; an alarm restarts it after a reset | outbound requests per invocation; each wait, against the 15 minutes of an alarm (memory is reported, not checked) |
| `vercel` | a Function driving a Workflow run | the run's event log | the step boundary | one step | a retried step: steps run at least once | each wait, against a Function's 800 s (other effects' duration is reported, not checked) |

Vercel documents no limit on the outbound requests of a Function
(vercel.com/docs/functions/limitations); its limits are duration, memory and
request and response body size. Duration enters the check through waits, the
one effect whose duration the program states; the others' durations and text
sizes are not modelled. Workflow's per-run limits on
steps and events (vercel.com/docs/workflows/pricing) span every turn of a
session, so they are not a per-handler bound either.

What the compiler does differently per target:

- **native-jsonl**: group commit. Records between two durability points are one
  `write` and one `fsync`. Each line carries a CRC-32 so a torn last line is
  detected and dropped instead of decoded.
- **native-sqlite**: one transaction per durability point.
- **server-postgres**: one transaction per durability point that first checks
  the session's owner epoch (`update ... where epoch = $owner`). A fenced write
  stops the handler; the new owner's recovery reports the open external calls.
- **cloudflare**: journal inserts and the outgoing `fetch` are issued in
  program order inside one event; the output gate makes the fetch wait for
  the inserts to commit, and a flush awaits `storage.sync()` so the rows the
  simulator counts as committed are. Turn requests and alarms queue behind
  each other, since another event enters the object at any await. The alarm
  is armed before the first external effect so a reset object resumes. Each
  of these platform facts is a `Fact` in `src/target/mod.rs` with its
  documentation and the probe in `oracle/cloudflare/storage.mjs` that
  measures it on local workerd; `hl check --target cloudflare` prints them. When a handler's worst case exceeds the
  per-invocation request limit, `hl check --target cloudflare` refuses it;
  splitting it into continuations driven by the alarm is future work (see
  Next).
- **vercel**: a step reruns the turn over the run's records and ends at the
  turn's next flush, returning the records it buffered; the workflow function
  records them and starts the next step. So the `none` and `idempotent`
  effects between two flushes share one step, and an `external` effect runs
  in the step after the one that committed its `call`. That step reads its
  attempt number and returns `unknown` on any attempt after the first, since
  a retried step may follow a dispatch whose result was lost.

Where a code cell runs: the host's engine in the process (native, Postgres),
a dynamic Worker isolate with no network whose steps are RPC calls into the
session's Durable Object (Cloudflare), the re-executed workflow function with
every nested call a step (Vercel). A target whose platform cannot give that
says `cells: unsupported` and `hl check` refuses `code` there.

## Verification

### Checks at compile time

- Names and types resolve; `match` is exhaustive; `maybe<T>` results are
  matched before use.
- Every loop is `below` a constant and every list has a constant capacity.
- Every handler proves (below): durability, finishing once, no overflow,
  pushes within capacity, invariants and the worst case.
- Worst case per handler: effects, journal records and outbound requests.
  Outbound requests are compared with the target's limit where it has one
  (Cloudflare); the message names the handler, the bound and the loops that
  multiply into it.
- No ambient time, randomness or I/O: the language has none outside effects.

### Proofs

`hl prove [--target T] FILE` proves properties of every path through each
handler, per target, and `hl check` runs the same proofs and fails when one
fails. A failure names the handler, the target, the property, the path (the
branches, loop iterations, crash and recovery choices that reach it) and,
for arithmetic, a counterexample.

A program states what the arithmetic proofs may assume and must keep:

    invariant turns_bounded: turns <= turns_max

    on prompt(input: text) -> outcome
      requires turns < turns_max
        and len(context) + 1 + turn_steps_max * (1 + calls_per_step_max) <= context_entries_max
    { ... }

An `invariant` must hold in the zero state and wherever a turn can end. A
handler's `requires` is checked when the turn is admitted; when it is false
the turn finishes `failed` before its body runs, so state is unchanged.

| Property | Proved |
| --- | --- |
| `finish_once` | every path of the body ends in exactly one `return`; on every target, every path of the lowered plan, traps included, appends one `status`, flushes it, then acknowledges, never twice |
| `durable_before_external` | an external effect, top-level or nested in a cell, runs only when nothing is buffered (a cell's header included) and its own `call` record is flushed, on first runs and on every rerun after a crash |
| `external_once` | an external call whose `call` record a crash left open is never run again: at-load recovery reports it `unknown`, Vercel's in-step recovery runs it only on attempt 1; Postgres flushes are fenced |
| `unknown_reported`, `idempotent_known` | a reopened external call yields `unknown`, a reopened none or idempotent call runs again |
| `seq_unique` | no path appends a second `call` record for a sequence number already in the journal |
| `output_gate` | on Cloudflare, nothing leaves the object while a record is uncommitted |
| `liveness`, `replay_equal` | the lowering checksums JSONL lines and writes checkpoints in sorted order (checks of the lowering, not of paths) |
| `no_overflow` | every `+`, `-` and `*` on `u64`, in `requires` too, stays in range |
| `push_bound` | every `push` finds its list below capacity |
| `invariant_initial`, `invariant_preserved` | invariants hold in the zero state, at every `return`, and where a model reply fails to decode: the one trap left once `no_overflow` and `push_bound` hold |
| `worst_case` | journal records per run, counted on the lowered plan, equal the IR's count on every target; outbound requests per invocation, counted on the plan with every nested call of a cell as one, are within the target's limit |
| `header_before_wait` | a cell waits on a timer only after its header is durable (C2) |
| `durable_before_wait` | a top-level `wait` sleeps only when nothing is buffered and its own `call` record, with its deadline, is flushed |
| `suspend_durable` | a `suspend` ends the run only when nothing is buffered and its `call` is durable, and never after the turn's `status`; a suspend replayed from its recorded answer appends nothing (`seq_unique`) |
| `cell_finish_once` | every path through a code site, divergence and the journal limit included, appends one `finished` update and one site `result`, and a cell whose result is recorded never starts its engine again |
| `cell_replay_equal` | nothing is dispatched after a replayed cell diverges from its journal |
| `unknown_nested_reported`, `nested_known` | a reopened nested external call reaches the script as the unknown outcome after a `recovery` record lists it, and the cell runs on; a reopened none or idempotent nested call runs again |
| `cell_bounds` | `journal_bytes` is at most nanocode's `JOURNAL_LIMIT_BYTES` and holds the entries of `calls` nested calls and `timers` timers at `ENTRY_BYTES` each; the runner enforces the limits (a check of the lowering) |

With `no_overflow` and `push_bound` proved, a turn of a proving program
finishes `failed` only when its `requires` is false or a reply of a model
without a declared reply type does not decode: never by a trap in its own
code. A push that may
find its list full is an error, not a warning: `requires` bounds the room a
turn needs, as `examples/minimal.hl` does.

How:

- **Durability** is a reachability proof over a model of the lowering. For
  each target the prover lowers a handler to a plan: the records each
  statement appends, the flushes the target performs (the IR's durability
  points, Cloudflare's output gate before every outbound request, Vercel's
  step commit), each effect's run, and the acknowledgement. The handler's
  start is a choice between a first run and a rerun after a crash; every
  effect is a choice between running, replaying its recorded result on a
  rerun, and, where the target can leave its `call` durable without its
  result, being reopened by recovery; `dispatch` is a choice of every class
  a tool can have; every possible trap is a choice that finishes the turn.
  The prover walks every path with the set of abstract states it can reach
  (buffered records, open and durable calls, the finish phase, the step
  attempt), iterating each loop to a fixpoint within its bound, and checks
  each operation against each state.
- **Arithmetic** is abstract interpretation over linear integer constraints.
  Every `u64` field, list length and local is a variable in `[0, max]`;
  assignments add equalities, `clear` sets a length to 0 and `keep` to
  `min(len, n)` (the join of the two cases), linear comparisons in conditions add facts, and
  `requires` and the invariants are assumed at entry. An obligation holds
  when its negation and the facts have no solution, decided by Fourier-Motzkin
  elimination with integer tightening. A loop's body is first run once with
  the quantities it changes left free, which bounds each one's change per
  iteration; iteration `k` then starts from the entry value plus `k` times
  that change, and the loop exits after its exact count (`loop` runs its
  bound, `for` the list's length at entry, which the interpreter snapshots).
  An invariant that holds where a loop starts, and still holds at the end of
  a body run from any state satisfying it, is assumed at every iteration's
  start and at the exit, so a counter a guard keeps below its bound proves.
  At a branch join the prover keeps the facts both sides imply and the hull
  of each changed quantity. A product of two variables is bounded by the
  product of their upper bounds. Every approximation drops facts and never
  adds one, so a failure can be spurious but a proof is not.
  `PROOF_STEPS_PER_HANDLER_MAX` bounds the elimination steps per handler and
  `PROOF_CONSTRAINTS_MAX` the constraints per step; reaching either fails
  the obligation and says so.
- **Cells** lower to a choice between a fresh cell, a cell restarted because
  its header never became durable, a cell replayed from its durable header and
  a finished cell taken from its recorded result. Inside, a loop of at most
  `calls` steps chooses a nested call of each listed tool (run, reopened or
  replayed from the journal), a divergence (on replay only) or the journal
  limit, with up to `timers` waits before and after each step; a wait flushes
  when the header is still buffered (C2; on Cloudflare the output gate always
  commits). The abstract state adds each cell's phase (recorded, running,
  diverged, finished, closed), the nested calls replayed from the journal and
  whether a `recovery` record was written.
- **Worst case** is the longest path of appended records and of outbound
  runs in the plan, a path ending where the plan ends it (after a finish's
  acknowledgement), loops multiplied by their bounds. Neither may exceed the
  IR's own count, which `hl check` reports, and the records must equal it,
  since the first run of the IR's longest path is a path of the plan: two
  computations of one bound, paired.
- **Counterexamples** are a solution of the prover's facts at the failing
  point, not a replay. After a join or a loop summary they can name values
  no run reaches.

What is assumed: the target models in `src/target/` (what a flush makes
durable, the atomic unit, who else may run, how recovery reports open calls)
are true of the platform; the plan is what the interpreter and the generated
code do, which the simulator, the oracle and the differential runs check,
not the prover; the runner reruns only unfinished turns, and a fenced owner's
writes are refused; tools and the model return values of their declared types,
or the turn exits as corrupt without finishing.

What is not proved: the generated Rust and JavaScript themselves; properties
across turns other than the invariants' induction; recovery from a torn
checkpoint; Cloudflare's alarm; memory, wall time beyond the waits a program
states, and text sizes; conditions
outside linear `u64` comparisons joined by `and`, `or` and `not` (an
invariant over `bool` or `text` fails rather than being assumed, and a
condition the prover cannot read adds no fact to its branch).

Every property comes with a planted bug: `hl prove --mutation M` takes the
simulator's lowering mutations and five of its own (`drop_flush`,
`drop_finish`, `drop_requires`, `repeat_assign`, `drop_cell_finish`), and
`scripts/prove-mutations.sh` requires each to fail proving `agent.hl`,
`reviewer.hl` and `code_agent.hl` on every target it applies to, naming the
property it breaks; a cell mutation must fail `code_agent.hl` and leave the
programs without a `code` tool proved, and `suspend_before_flush` and
`resume_replays_suspend` must fail `hooks.hl` and leave the programs without a
suspend proved.
The same script requires every example to prove and the prover's invalid
examples under `examples/invalid/` to fail with their message: overflow in a
loop, across a branch join and of a product, underflow, a push that can fill
its list without a `requires`, on an `unknown` arm, in a `for` over the list
itself or after an early return, invariants broken at a return, at a trap,
in the zero state or stated over a `bool`, and cells: a `journal_bytes` past
nanocode's limit or too small for its `calls` and `timers`, nested calls
that take a handler past Cloudflare's outbound limit, and a cell's output
pushed without room.

### Deterministic simulation

`hl sim` runs a program on a simulated target from a seed: a scripted model
(answers and tool calls drawn from the seed, or a declared reply type's
variants and provider failures), a tool host that counts real
dispatches per call, and the target's fault model:

- crashes at any effect or durability point, before or after the write;
- for JSONL, lost unsynced writes, torn last lines and a lying cache, as
  nanocode-sim's fault profiles (`clean`, `errors`, `async`, `lying`);
- for Postgres, a lease expiring and a second owner starting;
- for Cloudflare, a crash is an object reset, and the next run is the
  restart its alarm would make;
- for Vercel, a crash is a step that ran and is retried with the next
  attempt number;
- a seeded clock that a wait advances to its deadline, crashes in the middle of
  a wait, and resumes that arrive up to eight hours after a turn reported
  waiting, one in five with another key.

Invariants, named as nanocode-sim names them:

| Invariant | Holds when |
| --- | --- |
| `external_once` | every external call is dispatched at most once |
| `unknown_reported` | an external call that may have been dispatched without a recorded result is reported `unknown`, and only then |
| `idempotent_known` | every none and idempotent call ends with a known result |
| `finish_once` | every admitted turn finishes exactly once |
| `seq_unique` | journal sequence numbers are unique and increasing |
| `liveness` | every turn finishes within its bound of restarts |
| `replay_equal` | the same seed gives the same journal bytes twice (`--twice`) |
| `backend_equal` | generated code and the interpreter give the same journal and dispatch trace for a seed (the differential runs, `scripts/rust-diff.sh` and `scripts/js-sim.mjs`) |
| `panic` | no assertion of ours fails; a panic is caught at the seed and reported with it |
| `event_before_read` | a read sees only events recorded before its place in the turn (`event_read_unstamped` ignores the place) |
| `event_replay_equal` | a read the run replays sees exactly the events its journal holds there, and no event is recorded at an earlier place than one before it (`event_drain_on_replay` takes events at replayed effects, `batch_drains_replayed` in a parallel for whose last call is replayed) |
| `event_once` | no event is recorded twice (`event_redelivered` records a delivery sent again) |
| `event_kept` | every event is answered, one answered as delivered is in the journal and one answered otherwise is not (`event_ack_early` answers before the flush) |
| `checkpoint_compacted` | every checkpoint holds the state that replaying the journal reaches at its turn, so a list `clear` or `keep` shrank is short in it (`retire_skipped` writes the dropped entries back), and a turn that dropped entries flushes its checkpoint with its status, so the next load never replays the dropped prefix (`seal_skipped` leaves it out) |
| `sequential_equal` | a parallel for's results are recorded in list order in every journal, and on a clean seed that sends no events the journal, read per sequence number, and the acknowledgements equal those of the same seed with every parallel for run one call at a time |
| `enum_valid` | every result the journal records for a tool returning a text enum is what the tool returned when that was one of its values, and the call's failure when it was not (the simulator returns unlisted values on purpose; `enum_unchecked` falls back to the last value instead) |
| `mode_sent` | every journaled model reply answered a request sent in the mode its `model` record names (`mode_dropped` sends the request without its mode) |
| `blob_journaled` | every journaled blob's data hashes to its digest, a result that holds only a digest follows one that holds its data, and every checkpoint's blobs are held (`blob_unjournaled` journals digests alone) |
| `search_listed` | every name a journaled search found was listed by its turn's latest read of the listing before it (`search_unchecked` records searches unchecked) |
| `tool_admitted` | every listed tool that ran had the fingerprint its turn's latest listing read admitted, a deferred one only after a search of its turn found it (`discovery_unchecked`), and every tool with a declared input ran only with input that fits it (the simulator changes listings between turns, at crashes and at model calls, and draws unlisted names and input that does not fit; `listing_unchecked` and `input_unchecked` skip the checks, and `listed_class_dropped` journals an external listed tool as `none`, which `external_once` catches) |
| `suspend_durable`, `resume_matched`, `wait_until`, `wall_limit` | waits and suspends, defined in [LANGUAGE.md](LANGUAGE.md#suspend-and-resume) |
| `nested_external_once`, `unknown_nested_reported`, `nested_known`, `cell_finishes_once`, `cell_replay_equal`, `header_before_wait`, `cell_bounded`, `nested_enum_valid` | code cells, defined in [CODE_MODE.md](CODE_MODE.md) |

Each invariant comes with a planted bug (a mutation of the lowering or of a
target adapter) that the simulator must catch, as tempoxyz/zempo and
nanocode-sim require: `hl sim --mutation <name>` fails within its seed range,
and `scripts/mutations.sh` runs them all. Its census fails when a mutation `hl
sim` accepts has no plan, or an invariant is exercised by no mutation and no
planted program in `scripts/strict.sh`. Under `--strict`, a run that went past
its handler's worst case is reported as `worst_case` before the journal is
checked, since the overrun is the cause of whatever the journal shows later.

The differential runs have planted bugs of their own: `RUST_DIFF_PLANT` and
`JS_DIFF_PLANT` edit the emitted code with sed (a flush skipped, a decode
message changed, a second fsync, a batch's sequence numbers off by one, the
recovery skipped, the enum check removed), and `scripts/mutations.sh --diff`
requires each to fail on every target it changes with the invariant its plan
names. A plant that leaves a target's code unchanged needs no plan there; one
that changes it without failing needs an exemption with its reason.
`scripts/check.sh` runs every example through both differential runs, and its
census fails when an example was not swept by `hl sim` or by a differential
run on any target. It runs its independent steps in a pool of `CHECK_JOBS`
jobs (`nproc` by default), each in its own process group, stops the rest at
the first failure and prints that job's log, and kills every job on exit.

### Oracle

`oracle/` folds harnesslang journals with nanocode's own `nanocode-turnlog`
crate and checks that every turn's phase, outcome and open calls agree with the
interpreter's. `hl sim --dump-journal DIR` writes each seed's journal and,
beside it, the interpreter's summary after every record: the turn it touched,
its phase, outcome, recovery attempt, model replies, every call's effect class,
its open calls and the external ones a recovery must report `unknown`. The
`hl-oracle` binary reads the journal with its own line rules (CRC-32 from the
`crc` crate, a torn or failing line ends the journal), decodes each record
with `nanocode_turnlog::Record`, applies it to a `nanocode_turnlog::Fold` and
compares the fold with the summary at every prefix, since every load the
interpreter makes is a prefix of the final journal. It also checks what only
nanocode's fold can say: each call's class is what nanocode's `Effects` gives
for the program's declarations (`now` and `random` are `none`, an undeclared
tool `external`), a recovery's attempt is the fold's `next_attempt`, an
at-load recovery reports exactly the fold's open external calls of its turn and
an in-step one exactly one of them, and no call is open when its turn finishes.
`scripts/oracle.sh` sweeps every example on every target and requires the
oracle to disagree with the journals of the mutations whose records nanocode's
fold and ours read differently; `HL_ORACLE=1 scripts/check.sh` runs it.

## Layout

- `src/`: the compiler, interpreter and simulator, one std-only crate
  - `syntax/`: lexer and parser
  - `check/`: names, types, exhaustiveness and bounds
  - `ir/`: the lowered program: effect sites, durability points, loop bounds
    and each handler's worst case
  - `run/`: the interpreter and the journal
  - `target/`: the target models
  - `prove/`: `hl prove`: the plan per target, the durability, finishing and
    arithmetic proofs, and the linear constraint solver
  - `emit/`: Rust, JavaScript and SQL generation
  - `sim/`: the deterministic simulator, faults, invariants and mutations
- `examples/`: harnesses
- `oracle/`: the nanocode oracle, its own workspace
- `scripts/`: the check, docs check, mutation, prover mutation, oracle,
  differential and crash drill scripts

## Next

- Write nanocode's Claude and Codex harness loops in harnesslang and compile
  them into nanocode drivers.
- Subagent trees deeper than one level (a spawned harness spawns nothing
  today), side questions answered from a snapshot of the transcript, and the
  simulator at 100K children.
- Waits as durable sleeps outside the run: on Vercel a workflow `sleep()`
  between steps and on Cloudflare the object's alarm, so a wait spends no
  Function or alarm time.
- Cloudflare continuations: split a handler whose worst case exceeds the
  per-invocation request limit at a loop boundary into continuations driven
  by the alarm, instead of refusing it.
- Bound compaction, so long sessions stay within their capacities.
