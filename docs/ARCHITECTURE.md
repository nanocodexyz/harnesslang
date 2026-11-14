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
3. **Deterministic by construction.** Time, randomness, model calls, tools and
   children go only through effects the runtime journals, so a handler
   re-executed over its journal takes the same path. Recovery is replay.
4. **Bounded by construction.** Every loop has a bound and every list a
   capacity, named for what it bounds. The checker computes each handler's
   worst case (effects, journal records, outbound requests) and compares it
   with the target's limits.
5. **The target is a model.** A target is a durability model (what survives a
   crash, what is atomic, who else may run) and a place (memory, outbound
   requests, wall time). The same model drives the checker, the lowering and
   the simulator's faults.
6. **Zero dependencies.** The compiler, interpreter, simulator and every
   generated program use only the standard library of their language. Code that
   checks harnesslang against outside software (nanocode's own crates) lives in
   `oracle/`, its own workspace, never in the build.

## The language

A program is one harness: constants, types, tools, a model, session state and
handlers.

    harness agent

    const turn_steps_max: u64 = 64
    const calls_per_step_max: u64 = 16
    const context_entries_max: u64 = 4096

    type outcome = completed(text) | failed(text) | exhausted

    tool read(path: text) -> text effect none
    tool edit(path: text, body: text) -> text effect idempotent
    tool bash(command: text) -> text effect external

    model infer calls calls_per_step_max

    state {
      context: list<entry, context_entries_max>
      turns: u64
    }

    on prompt(input: text) -> outcome {
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

### Values and types

- `u64`, `bool`, `text`. Arithmetic on `u64` traps on overflow and underflow;
  the trap is a typed failure of the handler, never a wrapped value.
- Sum types: `type name = variant(type, ...) | ...`. Variants carry only what
  that case has.
- `list<T, N>`: a list with capacity `N`, a named constant. `push` onto a full
  list fails the handler with `bound(name)`, naming the list.
- State fields start at zero (`0`, `false`, empty text, an empty list), so
  they are `u64`, `bool`, `text` or lists. A tool returns `u64`, `bool` or
  `text`. In a pattern, `_` ignores a field.
- Builtins every program has:
  - `call = call(id: text, tool: text, input: text)`, with fields `id`, `tool`, `input`
  - `reply = answer(text) | calls(list<call, M>)`, where `M` is the model's
    `calls` bound
  - `entry = user(text) | assistant(reply) | result(call, text) | error(call, text) | unknown(call)`
  - `known<T> = ok(T) | failed(text)` and `maybe<T> = ok(T) | failed(text) | unknown`

### Effects

An effect is anything whose result the program cannot recompute: a tool, the
model, `now()`, `random()`, `spawn` and `wait`. Each effect site is journaled.

| Class | Declared by | Result type | On recovery, when its result was not recorded |
| --- | --- | --- | --- |
| `none` | `effect none`, `now()`, `random()` | `known<T>` (`now` and `random` return `u64`) | run again |
| `idempotent` | `effect idempotent`, the model, `spawn` | `known<T>` (the model returns `reply`) | run again with the same idempotency key |
| `external` | `effect external`, `dispatch` of a tool the program does not declare | `maybe<T>` | never run again: the result is `unknown` |

These are nanocode's `EffectClass` (`None`, `Idempotent`, `External`), and an
undeclared tool defaults to `external` as it does there. `dispatch(call)` looks
the call's tool up among the declared tools at run time and always has type
`maybe<text>`; a declared none or idempotent tool reached through it never
yields `unknown`.

### Statements

`let`, assignment to a state field, `push list value`, `if` / `else`, `match`
(exhaustive), `loop name below N` (`N` a constant), `for name in list`,
`return value`. There is no recursion, no unbounded loop and no user function:
a handler is straight-line code over bounded loops, so its worst case is a
number the checker computes.

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
| `model` | after the model returns | sequence, the reply |
| `recovery` | a recovery finds open external calls | turn, attempt, the unknown sequences |
| `status` | the turn finishes | turn, outcome |
| `checkpoint` | after a finish, by policy | revision, state |

### Durability points

Records are buffered and flushed at durability points, which the compiler
places:

1. Before an `external` effect runs, every buffered record is durable,
   including its `call` record. A crash after the flush and before the result
   leaves an open `call`, which recovery turns into `unknown`.
2. Before a turn's outcome is acknowledged, its `status` record is durable.
3. Nothing else forces a flush. Results of `none` and `idempotent` effects and
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

A handler is a tree of statements: `Let`, `Assign` and `Push` on state
fields, `If`, `Match` with one arm per variant in variant order, `Loop` and
`For` with their named bound, `Effect` and `Finish`. Expressions are pure:
lowering hoists every effect out of the expression it appears in into an
`Effect` statement that binds a temporary local, so a statement's effects run
first, left to right, then its expression. `and` and `or` evaluate both sides.

Each effect site has a kind (a tool, `dispatch`, the model, `now`, `random`),
a class (fixed, or resolved at run time for `dispatch`), a result type and a
durability: `RidesNextFlush`, `FlushBeforeRun` (external), or
`FlushBeforeRunWhenExternal` (dispatch). An effect runs as: take the next
sequence number; return the journaled result if there is one; otherwise append
its `call` record (the model writes none), flush if its durability says so,
run it, append its `result` or `model` record. `Finish` appends the `status`
and `checkpoint` records, flushes, then acknowledges: durability point 2. A
trap ends the turn the same way with outcome `failed`. A target may flush
more often than the IR asks (the output gate before every outbound request, a
step boundary after every external step) but never less.

Each handler also carries its worst case: effects, journal records and
outbound requests, each with the chain of loops that multiplies into it.

## Targets

A target is a durability model and a place.

| Target | Runs in | Journal | Flush | Atomic unit | Who else may run | Limits checked |
| --- | --- | --- | --- | --- | --- | --- |
| `native-jsonl` | a process | append-only JSONL file, one per session | write and fsync | one record (checksummed; a torn tail is dropped) | nobody | the program's own |
| `native-sqlite` | a process | SQLite rows through a host `Sql` | one transaction | one transaction | nobody | the program's own |
| `server-postgres` | a server pool | Postgres rows through a host `Sql` | one transaction fenced by the owner's epoch | one transaction | a second owner after a lease expires; the fence refuses its writes | the program's own |
| `cloudflare` | a Durable Object | the object's SQLite | none: writes in one event are atomic and the output gate holds outgoing requests until they commit | one event | nobody (one object per session); an alarm restarts it after a reset | memory, outbound requests per invocation |
| `vercel` | a Function driving a Workflow run | the run's event log | the step boundary | one step | a retried step: steps run at least once | outbound requests and wall time per Function |

What the compiler does differently per target:

- **native-jsonl**: group commit. Records between two durability points are one
  `write` and one `fsync`. Each line carries a CRC-32 so a torn last line is
  detected and dropped instead of decoded.
- **native-sqlite**: one transaction per durability point.
- **server-postgres**: one transaction per durability point that first checks
  the session's owner epoch (`update ... where epoch = $owner`). A fenced write
  stops the handler; the new owner's recovery reports the open external calls.
- **cloudflare**: no flushes. Journal inserts and the outgoing `fetch` are
  issued in program order inside one event; the output gate makes the fetch
  wait for the insert to commit. The alarm is armed before the first external
  effect so a reset object resumes. When a handler's worst case exceeds the
  per-invocation request limit, the compiler splits it at a loop boundary into
  continuations driven by the alarm.
- **vercel**: effects become steps. An `external` effect is a step that reads
  its attempt number and returns `unknown` on any attempt after the first,
  since a retried step may follow a dispatch whose result was lost. Adjacent
  `none` and `idempotent` effects with no external effect between them are
  grouped into one step, so a turn costs fewer queue round trips.

## Verification

### Checks at compile time

- Names and types resolve; `match` is exhaustive; `maybe<T>` results are
  matched before use.
- Every loop is `below` a constant and every list has a constant capacity.
- Worst case per handler: effects, journal records and outbound requests,
  compared with the target's limits. The message names the handler, the bound
  and the loops that multiply into it.
- No ambient time, randomness or I/O: the language has none outside effects.

### Deterministic simulation

`hl sim` runs a program on a simulated target from a seed: a scripted model
(answers and tool calls drawn from the seed), a tool host that counts real
dispatches per call, and the target's fault model:

- crashes at any effect or durability point, before or after the write;
- for JSONL, lost unsynced writes, torn last lines and a lying cache, as
  nanocode-sim's fault profiles (`clean`, `errors`, `async`, `lying`);
- for Postgres, a lease expiring and a second owner starting;
- for Cloudflare, an object reset and an alarm retry;
- for Vercel, a step retried after it ran, and a Function killed between steps.

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
| `backend_equal` | generated code and the interpreter give the same journal and dispatch trace for a seed |

Each invariant comes with a planted bug (a mutation of the lowering or of a
target adapter) that the simulator must catch, as tempoxyz/zempo and
nanocode-sim require: `hl sim --mutation <name>` fails within its seed range,
and `scripts/mutations.sh` runs them all.

### Oracle

`oracle/` folds harnesslang journals with nanocode's own `nanocode-turnlog`
crate and checks that every turn's phase, outcome and open calls agree with the
interpreter's.

## Layout

- `src/`: the compiler, interpreter and simulator, one std-only crate
  - `syntax/`: lexer and parser
  - `check/`: names, types, exhaustiveness, bounds and worst case
  - `ir/`: the lowered program: effect sites, durability points, loop bounds
  - `run/`: the interpreter and the journal
  - `target/`: the target models
  - `emit/`: Rust, JavaScript and SQL generation
  - `sim/`: the deterministic simulator, faults, invariants and mutations
- `examples/`: harnesses
- `oracle/`: the nanocode oracle, its own workspace
- `scripts/`: the check, mutation and differential scripts

## Next

- Write nanocode's Claude and Codex harness loops in harnesslang and compile
  them into nanocode drivers.
- Subagent trees as sessions spawned by `spawn`, simulated at 100K children.
- Bound compaction, so long sessions stay within their capacities.
