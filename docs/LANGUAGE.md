# The harnesslang language

This is the reference for people who write harnesses. It covers every
construct, the grammar as the parser reads it, the type rules, what each effect
class guarantees on each target, the bounds the compiler enforces, the errors
the checker and the prover print, and what each target generates. It opens
with a tutorial that builds `examples/agent.hl` one error at a time.

Every program here is a file under `docs/snippets/` or `examples/`, and every
command is run by `scripts/docs-check.sh`, which `scripts/check.sh` calls: a
program block must equal its file, a snippet passes `hl check` on every target
unless it sits under an `errors/` directory (then it must fail, with the
error a command below shows), and every command must print exactly what is
quoted after it. A line holding only `...` stands for lines left out. The
keyword, symbol and builtin lists and the grammar's terminals are compared
with `src/syntax`, and each named bound quoted as `NAME` = value with its
constant in `src`.

The design, the journal format and the proofs' method are in
[ARCHITECTURE.md](ARCHITECTURE.md); code mode is in [CODE_MODE.md](CODE_MODE.md).

## Tutorial: building examples/agent.hl

Build the compiler once. Commands below call it as `hl`.

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

### One model call

A harness has a name, constants, a model, session state and handlers. This one
answers a prompt with one model reply:

```hl docs/snippets/tutorial/errors/1-one-reply.hl
harness agent

const calls_per_step_max: u64 = 16
const context_entries_max: u64 = 4096

type outcome = completed(text) | failed(text) | exhausted

model infer calls calls_per_step_max

state {
  context: list<entry, context_entries_max>
}

on prompt(input: text) -> outcome {
  push context user(input)
  let reply = infer(context)
  push context assistant(reply)
  match reply {
    answer(words) => { return completed(words) }
    calls(requested) => { return failed("no tools yet") }
  }
}
```

`infer` is the model; `calls calls_per_step_max` bounds how many tool calls one
reply may hold. `context` is a list of the builtin `entry` type with a named
capacity. `hl check` parses, type-checks and then proves the program. Dropping
standard output leaves the errors:

```console
$ hl check docs/snippets/tutorial/errors/1-one-reply.hl > /dev/null
docs/snippets/tutorial/errors/1-one-reply.hl: error: handler 'prompt': push_bound: push onto context may find it full and trap with bound(context); path: entry; counterexample: len(context) = 4096
docs/snippets/tutorial/errors/1-one-reply.hl: error: 1 proof obligation(s) failed for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
```

Nothing stops `context` from being full when a turn starts, and a `push` onto a
full list traps. The prover does not accept a program that can trap in its own
code.

### requires

A `requires` clause states what must hold when a turn is admitted. When it is
false the turn finishes `failed` before its body runs, with the state
unchanged. Two pushes need two free entries:

```hl docs/snippets/tutorial/2-requires.hl
harness agent

const calls_per_step_max: u64 = 16
const context_entries_max: u64 = 4096

type outcome = completed(text) | failed(text) | exhausted

model infer calls calls_per_step_max

state {
  context: list<entry, context_entries_max>
}

on prompt(input: text) -> outcome
  requires len(context) + 2 <= context_entries_max
{
  push context user(input)
  let reply = infer(context)
  push context assistant(reply)
  match reply {
    answer(words) => { return completed(words) }
    calls(requested) => { return failed("no tools yet") }
  }
}
```

```console
$ hl check docs/snippets/tutorial/2-requires.hl
docs/snippets/tutorial/2-requires.hl: harness agent: 0 tool(s), 1 state field(s), 1 handler(s)
  handler prompt: worst case 1 effects, 4 journal records, 1 outbound requests
    effects: no loops
    journal records: no loops
    outbound requests: no loops
    plus one recovery record per restart
  proved handler prompt: finish_once: every path ends in exactly one of 2 return(s)
  proved handler prompt: no_overflow: 1 arithmetic operation(s) proved in range; invariant_preserved: 0 invariant check(s) at returns and trap points; push_bound: 2 push(es) proved below capacity (15 prover steps)
  proved handler prompt on native-jsonl: durability: 0 external effect site(s) run only after a flush of their call and every record before it; 6 acknowledgement point(s) follow their status flush
  proved handler prompt on native-jsonl: worst_case: 4 journal records per run on the plan's longest path, as hl check counts (no loops)
  proved handler prompt on native-sqlite: durability: 0 external effect site(s) run only after a flush of their call and every record before it; 6 acknowledgement point(s) follow their status flush
  proved handler prompt on native-sqlite: worst_case: 4 journal records per run on the plan's longest path, as hl check counts (no loops)
  proved handler prompt on server-postgres: durability: 0 external effect site(s) run only after a flush of their call and every record before it; 6 acknowledgement point(s) follow their status flush
  proved handler prompt on server-postgres: worst_case: 4 journal records per run on the plan's longest path, as hl check counts (no loops)
  proved handler prompt on cloudflare: durability: 0 external effect site(s) run only after a flush of their call and every record before it; 6 acknowledgement point(s) follow their status flush
  proved handler prompt on cloudflare: worst_case: 4 journal records per run on the plan's longest path, as hl check counts (no loops)
  proved handler prompt on cloudflare: worst_case: 1 outbound requests <= 10000 outbound requests per invocation (no loops)
  proved handler prompt on vercel: durability: 0 external effect site(s) run only after a flush of their call and every record before it; 6 acknowledgement point(s) follow their status flush
  proved handler prompt on vercel: worst_case: 4 journal records per run on the plan's longest path, as hl check counts (no loops)
docs/snippets/tutorial/2-requires.hl: proved for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
```

The worst case counts one effect (the model), four journal records (admitted,
model, status, checkpoint) and one outbound request. The six acknowledgement
points are every place the turn can finish: the two `return`s and the traps
the plan keeps (a false `requires`, each `push`, a model reply that does not
decode), each acknowledged only after its status flush. With no invariants
declared there is no zero-state line.

### Tools and the unknown outcome

Now the model may ask for tools. Each tool declares its effect class: `read` is
`none` (safe to run again), `edit` is `idempotent` (safe to run again with the
same key), `bash` is `external` (must never run twice). The loop runs at most
`turn_steps_max` model replies, and `dispatch(call)` runs whatever tool a
reply names:

```hl docs/snippets/tutorial/errors/3-tools.hl
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
}

on prompt(input: text) -> outcome
  requires len(context) + 2 <= context_entries_max
{
  push context user(input)
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
          }
        }
      }
    }
  }
  return exhausted
}
```

```console
$ hl check docs/snippets/tutorial/errors/3-tools.hl
docs/snippets/tutorial/errors/3-tools.hl:30:11: error: match on maybe<text> is missing 'unknown': an external effect whose result was not recorded has an unknown outcome
```

`dispatch` has type `maybe<text>`: if the process dies after an external tool
was sent and before its result was recorded, nobody knows whether it ran.
Recovery never sends it again; it reports `unknown`, and the program must say
what to do then.

### The room a turn needs

With an `unknown` arm the program type-checks, but `requires` still reserves two
entries while the loop pushes many more:

```hl docs/snippets/tutorial/errors/4-unknown.hl
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
}

on prompt(input: text) -> outcome
  requires len(context) + 2 <= context_entries_max
{
  push context user(input)
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

```console
$ hl check docs/snippets/tutorial/errors/4-unknown.hl 2> /dev/null | head -n 6
docs/snippets/tutorial/errors/4-unknown.hl: harness agent: 3 tool(s), 1 state field(s), 1 handler(s)
  handler prompt: worst case 1088 effects, 2115 journal records, 1088 outbound requests
    effects: step below turn_steps_max (64) x call below calls_per_step_max (16)
    journal records: step below turn_steps_max (64) x call below calls_per_step_max (16)
    outbound requests: step below turn_steps_max (64) x call below calls_per_step_max (16)
    plus one recovery record per restart
$ hl check docs/snippets/tutorial/errors/4-unknown.hl > /dev/null
docs/snippets/tutorial/errors/4-unknown.hl: error: handler 'prompt': push_bound: push onto context may find it full and trap with bound(context); path: loop step below turn_steps_max (64); counterexample: len(context) = 4096, iteration of loop step below turn_steps_max (64) = 1
docs/snippets/tutorial/errors/4-unknown.hl: error: handler 'prompt': push_bound: push onto context may find it full and trap with bound(context); path: loop step below turn_steps_max (64) > match reply: calls > for call in requested (at most calls_per_step_max = 16) > match dispatch(call): ok; counterexample: len(context) = 4096, iteration of loop step below turn_steps_max (64) = 0, len(requested) = 1
docs/snippets/tutorial/errors/4-unknown.hl: error: handler 'prompt': push_bound: push onto context may find it full and trap with bound(context); path: loop step below turn_steps_max (64) > match reply: calls > for call in requested (at most calls_per_step_max = 16) > match dispatch(call): failed; counterexample: len(context) = 4096, iteration of loop step below turn_steps_max (64) = 0, len(requested) = 1
docs/snippets/tutorial/errors/4-unknown.hl: error: handler 'prompt': push_bound: push onto context may find it full and trap with bound(context); path: loop step below turn_steps_max (64) > match reply: calls > for call in requested (at most calls_per_step_max = 16) > match dispatch(call): unknown; counterexample: len(context) = 4096, iteration of loop step below turn_steps_max (64) = 0, len(requested) = 1
docs/snippets/tutorial/errors/4-unknown.hl: error: 4 proof obligation(s) failed for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
```

The worst case is now 64 model replies of 16 calls each: 1,088 effects and as
many outbound requests, and 2,115 journal records (each tool call writes a
`call` and a `result`, each reply one `model` record). Each failing push is
named by its path through the loops and matches, with values that reach it.
One prompt pushes at most `1 + turn_steps_max * (1 + calls_per_step_max)`
entries, so that is the room to require.

### Arithmetic that cannot overflow

Count turns in the state:

```hl docs/snippets/tutorial/errors/5-turns.hl
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

on prompt(input: text) -> outcome
  requires len(context) + 1 + turn_steps_max * (1 + calls_per_step_max) <= context_entries_max
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

```console
$ hl check docs/snippets/tutorial/errors/5-turns.hl > /dev/null
docs/snippets/tutorial/errors/5-turns.hl: error: handler 'prompt': no_overflow: u64 overflow in 'turns + 1' is possible; path: entry; counterexample: turns = 18446744073709551615
docs/snippets/tutorial/errors/5-turns.hl: error: 1 proof obligation(s) failed for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
```

`u64` arithmetic traps on overflow, so `turns + 1` needs a bound. Requiring
`turns < turns_max` gives one, and an `invariant` states it for every turn:
the prover checks it in the zero state and wherever a turn can end, and
assumes it when a turn starts. That is `examples/agent.hl`:

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

### Check it against a target

`--target` adds the target's model and checks the worst case against its
limits. On Cloudflare one invocation may make 10,000 outbound requests:

```console
$ hl check --target cloudflare examples/agent.hl
examples/agent.hl: harness agent: 3 tool(s), 2 state field(s), 1 handler(s)
  handler prompt: worst case 1088 effects, 2115 journal records, 1088 outbound requests
    effects: step below turn_steps_max (64) x call below calls_per_step_max (16)
    journal records: step below turn_steps_max (64) x call below calls_per_step_max (16)
    outbound requests: step below turn_steps_max (64) x call below calls_per_step_max (16)
    plus one recovery record per restart
  proved handler prompt: finish_once: every path ends in exactly one of 2 return(s)
  proved zero state: invariant_initial: 2 invariant(s) hold
  proved handler prompt: no_overflow: 5 arithmetic operation(s) proved in range; invariant_preserved: 6 invariant check(s) at returns and trap points; push_bound: 5 push(es) proved below capacity (1002 prover steps)
  proved handler prompt on cloudflare: durability: 1 external effect site(s) run only after a flush of their call and every record before it; 10 acknowledgement point(s) follow their status flush
  proved handler prompt on cloudflare: worst_case: 2115 journal records per run on the plan's longest path, as hl check counts (step below turn_steps_max (64) x call below calls_per_step_max (16))
  proved handler prompt on cloudflare: worst_case: 1088 outbound requests <= 10000 outbound requests per invocation (step below turn_steps_max (64) x call below calls_per_step_max (16))
examples/agent.hl: proved for cloudflare
target cloudflare: runs in a Durable Object; journal: the object's SQLite; flush: OutputGate; atomic unit: Event; who else may run: AlarmAfterReset
  limit memory: 134217728 bytes per isolate (developers.cloudflare.com/durable-objects/platform/limits)
  limit outbound requests: 10000 outbound requests per invocation (developers.cloudflare.com/workers/platform/limits (Workers Paid default))
  limit concurrent requests: 6 requests waiting for response headers per invocation (developers.cloudflare.com/workers/platform/limits (simultaneous open connections; relaxed to the wait for headers on 2026-04-09); a seventh fetch is queued by the runtime)
  limit waits: 900000 milliseconds per wait, checked on each wait on its own, since the run that holds it may be cut and resumed at a wait (developers.cloudflare.com/durable-objects/platform/limits (an alarm handler's wall time is 15 minutes; a request's is unlimited while its client stays connected); a turn a reset leaves unfinished resumes in the alarm, and a run cut at that wall time resumes from the alarm the turn keeps armed, past every wait whose deadline it recorded, so each wait must fit one alarm invocation)
  fact: an outbound fetch or a response leaves only after the event's earlier writes commit (blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three (output gates); oracle/cloudflare/storage.mjs gate_holds_outbound_fetch, consistent with it but not isolating it)
  fact: writes with no await between them commit together, and none commits while the event runs synchronously (developers.cloudflare.com/durable-objects/api/storage-api (write coalescing); oracle/cloudflare/storage.mjs event_commits_after_it_yields)
  fact: a flush is committed once storage.sync() resolves; an awaited microtask commits nothing (developers.cloudflare.com/durable-objects/api/storage-api (sync); oracle/cloudflare/storage.mjs sync_commits, microtask_await_does_not_commit)
  fact: a committed alarm runs after the object resets (developers.cloudflare.com/durable-objects/api/alarms (at-least-once); oracle/cloudflare/storage.mjs alarm_runs_after_reset)
  fact: another event enters the object at any await of non-storage I/O (blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three (input gates); oracle/cloudflare/storage.mjs event_enters_at_await)
  not checked: memory (134217728 bytes per isolate), since text values carry no bound
examples/agent.hl: ok for cloudflare
```

With 1,024 steps instead of 64 the same loop exceeds that limit, and
Cloudflare alone refuses it:

```console
$ hl check --target cloudflare examples/invalid/over_cloudflare.hl > /dev/null
examples/invalid/over_cloudflare.hl: error: handler 'prompt' on cloudflare: worst_case: worst case 17408 outbound requests exceeds cloudflare's limit of 10000 outbound requests per invocation; path: step below turn_steps_max (1024) x call below calls_per_step_max (16)
examples/invalid/over_cloudflare.hl: error: 1 proof obligation(s) failed for cloudflare
examples/invalid/over_cloudflare.hl: error: handler 'prompt': worst case 17408 outbound requests exceeds cloudflare's limit of 10000 outbound requests per invocation; loops: step below turn_steps_max (1024) x call below calls_per_step_max (16)
$ hl check --target vercel examples/invalid/over_cloudflare.hl | tail -n 1
examples/invalid/over_cloudflare.hl: ok for vercel
```

### Simulate it

`hl sim` runs the harness on a target's fault model, one session per seed: a
scripted model, a tool host that counts real dispatches, and crashes, torn and
lost writes, lease takeovers or retried steps as the target allows. Every seed
must keep the invariants (an external call dispatched at most once, every turn
finished once, unknown reported exactly when a dispatch may have happened):

```console
$ hl sim --target native-jsonl --seeds 0..200 examples/agent.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=2764 crashes=2260 power_losses=159 torn_writes=291 takeovers=0 fenced=0 dispatches=7032 unknown=800
```

Each invariant has a planted bug the simulator must catch. Here recovery sends
open external calls again instead of reporting them unknown:

```console
$ hl sim --target native-jsonl --seeds 0..200 --mutation recovery_redispatches_external examples/agent.hl
FAIL invariant=external_once seed=0 target=native-jsonl profile=errors mutation=recovery_redispatches_external: external call 1/16 (web_fetch) was dispatched 3 times
replay: hl sim --target native-jsonl --seeds 0..1 --mutation recovery_redispatches_external examples/agent.hl
```

### Run and emit it

`hl run` runs one turn in the interpreter over the simulated world and prints
the journal, one checksummed record per line:

```console
$ hl run examples/agent.hl --handler prompt --arg hello | head -n 1
{"crc":1413599148,"record":{"admitted":{"digest":"prompt","first":1,"hand":null,"metadata":{"handler":"prompt"},"prompt":{"input":"hello"},"turn":"1"}}}
```

`hl emit` prints the program for a target as a WebAssembly module (`--format
wat` prints its text, `--format mjs` an ES module holding its bytes), which
the platform's fixed host runs. The compiler writes the bytes itself, and the
host is the same for every program; a host drives a turn through the module's
exports:

```console
$ hl emit --format wat --target native-jsonl examples/agent.hl | grep -oE '\(export "[a-z_]+"' | cut -d'"' -f2 | paste -sd' '
memory hl_alloc hl_side hl_seq hl_run hl_facts hl_check hl_idle
```

[Targets](#targets) lists what every target generates.

## Lexical structure

A source file is UTF-8 text. Whitespace (space, tab, carriage return, newline)
separates tokens and is otherwise ignored. There are no comments.

- **Names**: an ASCII letter or `_`, then ASCII letters, digits and `_`. Names
  starting with `_` are reserved for the compiler; a lone `_` in a pattern
  ignores a field.
- **Numbers**: decimal digits, with `_` allowed between them (`1_000_000`).
  A number must fit in `u64` and may not be followed by a letter. `0x`
  starts a hexadecimal number (`0xff`). There are no negative or fractional
  literals.
- **Bytes**: one ascii character between single quotes (`'a'`) is the `u64`
  value of that byte. The escapes are `\'`, `\\` and `\n`.
- **Text**: between double quotes, on one line. The escapes are `\"`, `\\`
  and `\n`; any other backslash is an error.
- **Symbols**, longest match first:

Symbols: `{` `}` `(` `)` `<` `<=` `>` `>=` `,` `:` `=` `==` `!=` `->` `=>` `+` `++` `-` `*` `/` `%` `[` `]` `.` `|`

Keywords, which cannot be used as names:

Keywords: `harness` `code` `const` `type` `tool` `model` `state` `on` `let` `push` `clear` `keep` `if` `else` `match` `loop` `for` `return` `true` `false` `and` `or` `not` `list` `effect` `invariant` `requires`

Words that are keywords only where the grammar expects them, and names
elsewhere:

Contextual words: `accept` `after` `at` `before` `below` `best` `blob` `blobs` `bound` `by` `bytes` `calls` `candidate` `candidates` `charge` `climb` `compact` `cost` `cycles` `drop` `end` `failures` `finds` `fn` `fork` `forked` `from` `gate` `higher` `idle` `in` `interruptible` `jobs` `known` `last` `least` `ledger` `listing` `lower` `maybe` `mean` `measure` `metric` `mode` `modes` `most` `over` `paid` `parallel` `payments` `post` `pre` `prompt` `propose` `recoverable` `reply` `rewrite` `runs` `score` `set` `session` `sessions` `settings` `side` `sign` `spawn` `spend` `start` `stop` `subagent` `submit` `take` `tools` `trial` `trials` `var` `wall` `wallet` `when` `while` `window` `wire` `with` `within` `yield`

Builtin functions, whose names declarations may not take:

Builtins: `after` `allow` `before` `bytes_of` `bytes_of_hex` `contains` `deny` `dispatch` `hex_of` `hold` `join` `json_escape` `len` `now` `random` `respond` `slice` `suspend` `text_of` `trim` `truncate_middle` `wait`

```console
$ hl check docs/snippets/errors/comment.hl
docs/snippets/errors/comment.hl:3:1: error: unexpected character '#'
$ hl check docs/snippets/errors/escape.hl
docs/snippets/errors/escape.hl:3:32: error: unknown escape in text literal: only \" \\ and \n are escapes
$ hl check docs/snippets/errors/keyword-name.hl
docs/snippets/errors/keyword-name.hl:4:7: error: expected a binding name, found keyword 'loop'
```

```hl docs/snippets/errors/comment.hl
harness notes

# a harness has no comments

on ping() -> u64 { return 1 }
```

```hl docs/snippets/errors/escape.hl
harness notes

on ping() -> text { return "tab\there" }
```

```hl docs/snippets/errors/keyword-name.hl
harness notes

on ping() -> u64 {
  let loop = 1
  return loop
}
```

## Grammar

This is the grammar `src/syntax/parser.rs` implements, in EBNF: `{ x }` is zero
or more, `[ x ]` optional, quoted text a terminal, `name`, `number` and `text`
the tokens above.

```ebnf
program    = "harness" name { item } ;
item       = const | type | tool | listing | code | wallet | spawn | fork | model | state | invariant | function | hook | lifecycle | handler | side | event ;
const      = "const" name ":" type_expr "=" number ;
type       = "type" name "=" ( variant { "|" variant } | text { "|" text } ) ;
variant    = name [ "(" type_expr { "," type_expr } ")" ] ;
tool       = "tool" name tool_params "->" type_expr "effect" effect_class [ "recoverable" ] [ interrupts ]
             [ "jobs" "at" "most" bound ] [ "finds" name ] [ paid ] ;
paid       = "paid" "by" name ;
wallet     = "wallet" name "spend" "at" "most" bound "charge" "at" "most" bound "payments" "at" "most" bound ;
interrupts = "interruptible" "by" name { "," name } ;
tool_params = "(" [ tool_param { "," tool_param } ] ")" ;
tool_param = name ":" type_expr [ "=" expr ] ;
effect_class = "none" | "idempotent" | "external" ;
listing    = "tools" "from" "listing" "(" name "," bound ")" tool_params "effect" effect_class [ paid ] ;
code       = "code" name "{" { name ( bound | name { "," name } ) } "}" ;
model      = "model" name "calls" bound [ "reply" name ] [ "window" bound ] [ "modes" name { "," name } ] [ "settings" tool_params ] [ interrupts ] [ "wire" name ] ;
state      = "state" "{" { name ":" type_expr } "}" ;
invariant  = "invariant" name ":" expr ;
function   = "fn" name params [ "->" type_expr ] block ;
spawn      = "spawn" name "runs" name "from" text [ "sessions" "at" "most" name ] ;
fork       = "fork" name "at" name ;
hook       = ( "before" | "after" ) "tool" params block ;
lifecycle  = "on" ( "session" "start" | "prompt" "submit" | "pre" "compact" | "post" "compact"
                  | "stop" | "subagent" "start" | "subagent" "stop" ) [ params ] block
           | "on" "session" "end" params "->" type_expr [ "requires" expr ] block
           | "on" "idle" name params "at" name "bound" bound "->" type_expr [ "requires" expr ] block
           | "on" "forked" params block ;
handler    = "on" name params "->" type_expr [ "requires" expr ] block ;
side       = "on" "side" name params "->" type_expr [ "requires" expr ] block ;
event      = "on" name [ params "bound" bound ] ;
params     = "(" [ name ":" type_expr { "," name ":" type_expr } ] ")" ;
bound      = name ;
type_expr  = "list" "<" type_expr "," bound ">"
           | "known" "<" type_expr ">"
           | "maybe" "<" type_expr ">"
           | "blob" "<" bound ">"
           | "bytes" "<" bound ">"
           | name ;
block      = "{" { statement } "}" ;
statement  = "let" name [ ":" type_expr ] "=" expr
           | "var" name [ ":" type_expr ] "=" expr
           | "set" name "[" expr "]" "=" expr
           | "push" name expr
           | "clear" name
           | "keep" name "last" expr [ "after" expr ]
           | "keep" name "blobs" "last" bound
           | "rewrite" name "{" { name [ "(" name { "," name } ")" ] "=>" ( expr | "drop" ) [ "," ] } "}"
           | if
           | "match" expr "{" { arm [ "," ] } "}"
           | "loop" name "below" bound [ "while" expr ] block
           | "for" name "in" expr [ "parallel" ] block
           | "return" expr
           | climb
           | "yield" ( "candidate" "(" expr "," expr ")" | "score" "(" expr { "," expr } ")" | "cost" expr )
           | name "=" expr
           | name "(" [ expr { "," expr } ] ")" ;
if         = "if" expr block [ "else" ( if | block ) ] ;
climb      = "climb" name "over" name { climb_clause } "{" "propose" "(" name ")" block "measure" "(" name ")" block "}" ;
climb_clause = "from" expr | "ledger" name | "end" name
           | ( "cycles" | "candidates" | "trials" | "wall" | "failures" ) bound
           | "cost" bound "propose" bound "trial" bound
           | "metric" name ( "lower" | "higher" ) "at" "most" bound [ "within" bound ]
           | "gate" expr
           | "accept" ( ( "mean" | "best" ) "by" bound | "sign" "at" "least" bound
                      | "when" "(" name { "," name } ")" expr ) ;
arm        = ( name [ "(" name { "," name } ")" ] | text ) "=>" block ;
expr       = or [ "with" or ] ;
or         = and { "or" and } ;
and        = not { "and" not } ;
not        = "not" not | comparison ;
comparison = sum [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) sum ] ;
sum        = product { ( "+" | "-" | "++" ) product } ;
product    = postfix { ( "*" | "/" | "%" ) postfix } ;
postfix    = primary { "." name | "[" expr "]" } ;
primary    = number | text | "true" | "false" | "(" expr ")" | "[" "]"
           | "take" name
           | name [ "(" [ expr { "," expr } ] ")" [ "at" "most" bound ] [ "mode" name ] [ "with" "{" name ":" expr { "," name ":" expr } "}" ] ] ;
```

Notes the grammar leaves out:

- Items may come in any order and refer to each other in any order, but a
  harness has at most one `model` and one `state` block and at least one
  handler.
- Operators bind, loosest first: `or`, `and`, `not`, comparisons, `+ - ++`,
  `* / %`, `.` and `[ ]`. Binary operators are left associative; a comparison does not chain
  (`a < b < c` is a parse error).
- `known`, `maybe`, `blob` and `bytes` start a type only when `<` follows. A field name after
  `.` may be any name.
- A `bound` is a name: a number there is an error that names the number.
- Nesting of blocks, types and expressions is bounded by
  `NESTING_DEPTH_MAX` = 64 (policy: deeper than any harness, shallow enough for
  the default thread stack); deeper input is an error, not a crash.

## Declarations

### harness

`harness name` opens the file. The name labels the journal's checkpoints and
the compiled module.

### const

`const name: u64 = number`. Constants are `u64` and are the only things a bound
may name: loop bounds, list capacities and the model's `calls`. Naming every
limit is the point: the checker's messages and the worst case name them back.

```console
$ hl check docs/snippets/errors/literal-capacity.hl
docs/snippets/errors/literal-capacity.hl:4:19: error: a list's capacity must be a named constant, found number 64
$ hl check docs/snippets/errors/not-a-constant.hl
docs/snippets/errors/not-a-constant.hl:6:19: error: loop 'step' must be a named constant: 'limit' is not a constant
```

```hl docs/snippets/errors/literal-capacity.hl
harness notes

state {
  log: list<text, 64>
}

on ping() -> u64 { return len(log) }
```

```hl docs/snippets/errors/not-a-constant.hl
harness notes

const steps_max: u64 = 8

on ping(limit: u64) -> u64 {
  loop step below limit {
  }
  return 1
}
```

### type

`type name = variant | variant(type, ...) | ...` declares a sum type. Variants
carry only what that case has. A variant name may appear in several types; a
type may not contain itself, directly or through other types or lists, since
its values would have no bound. `type name = "value" | "value" | ...` declares
a [text enum](#text-enums) instead.

```console
$ hl check docs/snippets/errors/recursive-type.hl
docs/snippets/errors/recursive-type.hl:3:6: error: type 'tree' contains itself, so its values have no bound
```

```hl docs/snippets/errors/recursive-type.hl
harness notes

type tree = leaf(text) | node(tree, tree)

on ping() -> u64 { return 1 }
```

### tool

`tool name(param: type, ...) -> type effect class` declares a tool the model
may call and the program may call directly. It returns `u64`, `bool`, `text`
or a text enum, so that every target and the simulator can carry its result. A
result that does not fit the declared type is recorded as the call's failure
([Text enums](#text-enums)). The class is `none`, `idempotent` or `external`
([Effects](#effects)).

The parameters are the tool's input type, checked at the boundary. A call the
program makes passes typed arguments. A call the model makes reaches the tool
through `dispatch`, whose input is the call's `input` text: it must be a
JSON object holding exactly the parameters, each of its type. Then the
`call` record holds that object and the host receives it. Otherwise the
`call` record holds the text, the tool does not run, and the call is
recorded as failed with what was wrong (`input of read: missing 'path'`),
never run with a guess. A tool the program does not declare has no input
type and receives the text.

A parameter may declare a default, `name: type = value`, where the value is a
literal of the parameter's type or, for `u64`, a named constant. `dispatch`
fills each default the input leaves out before it checks the input, so the
`call` record, the host and every replay see the filled object, never what
the model left out; a parameter without a default is still required, and a
call the program makes passes every argument. Parameters of `tools from
listing` take defaults the same way. The simulator draws inputs that leave
each defaulted parameter out, and `default_unfilled` runs such a call without
filling it, which `tool_admitted` catches:

```hl docs/snippets/defaults.hl
harness shell

const calls_max: u64 = 4
const rounds_max: u64 = 8
const context_max: u64 = 64
const timeout_default_ms: u64 = 120000

type outcome = done(text) | exhausted

tool bash(command: text, timeout_ms: u64 = timeout_default_ms, background: bool = false) -> text effect external

model think calls calls_max

state {
  context: list<entry, context_max>
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  loop round below rounds_max {
    let reply = think(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return done(words) }
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

```console
$ hl run docs/snippets/defaults.hl --handler ask --arg hi --seed 0 | grep -o '"input":{[^}]*}' | head -n 1
"input":{"background":false,"command":"alpha-14","timeout_ms":120000}
$ hl check docs/snippets/errors/default-type.hl
docs/snippets/errors/default-type.hl:3:45: error: the default of parameter 'background' must be a literal of bool or, for u64, a named constant
$ hl sim --target native-jsonl --seeds 0..200 --twice --mutation default_unfilled docs/snippets/defaults.hl | head -n 1
FAIL invariant=tool_admitted seed=0 target=native-jsonl profile=errors mutation=default_unfilled: call 1/8 (bash) ran with input {"background":false,"command":"beta-6"}, which does not fit its declared input: missing 'timeout_ms'
```

```hl docs/snippets/errors/default-type.hl
harness notes

tool bash(command: text, background: bool = "no") -> text effect external

on ping() -> u64 { return 1 }
```

```console
$ hl check docs/snippets/errors/tool-returns.hl
docs/snippets/errors/tool-returns.hl:7:26: error: a tool returns u64, bool, text, a text enum or a blob
```

```hl docs/snippets/errors/tool-returns.hl
harness notes

const calls_max: u64 = 4

model think calls calls_max

tool plan(goal: text) -> reply effect none

on ping() -> u64 { return 1 }
```

### tools from listing

`tools from listing(name, bound) (param: type, ...) effect class` declares a
family of tools whose names are known only at run time, such as an MCP
server's: the listing `name` names them `name__TOOL`, as nanocode names MCP
tools `mcp__SERVER__TOOL`. The declaration fixes what the program relies on,
so the effect rule stays a compile-time fact:

- Every tool of the family has the declared class. `dispatch` of
  `docs__search` journals its call as that class, flushes it first when it is
  `external`, and reports `unknown` after a crash exactly as for a declared
  tool.
- Every tool of the family takes the declared input, checked as a declared
  tool's is ([tool](#tool)), and returns `text`.
- `name()` reads the listing: an effect of class `none` that returns
  `known<u64>`, the number of tools it admitted. Its `result` record holds
  the listing, an object of tool name to fingerprint text (what the host
  hashes a tool's definition to), so a rerun admits the same tools. A listing
  that holds more than `bound` tools, or that is not such an object, is
  recorded as the read's failure.
- `dispatch` admits a listed tool by name against the latest read of its
  listing in the turn. A tool that read did not list is refused, and the host
  receives the admitted fingerprint with the call and refuses a tool whose
  current definition differs (`TOOL changed since admission; rediscover
  before executing`, nanocode's words). Both refusals are recorded as the
  call's failure, so the program meets them in its `failed` arm, and a
  rerun replays them.

Reading the listing at the start of every round admits what the model is
offered in that round, as nanocode admits dynamic tools per model request:

```hl docs/snippets/listing.hl
harness lookup

const calls_max: u64 = 4
const rounds_max: u64 = 8
const context_max: u64 = 64
const docs_max: u64 = 6

type outcome = done(text) | stopped(text) | exhausted

tool read(path: text, line: u64) -> text effect none
tools from listing(docs, docs_max) (query: text) effect idempotent

model think calls calls_max

state {
  context: list<entry, context_max>
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  loop round below rounds_max {
    match docs() {
      ok(listed) => {
        if listed == 0 { return stopped("no docs tools") }
      }
      failed(error) => { return stopped(error) }
    }
    let reply = think(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return done(words) }
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

```console
$ hl check docs/snippets/listing.hl | grep -E 'worst case'
  handler ask: worst case 48 effects, 91 journal records, 48 outbound requests
$ hl run docs/snippets/listing.hl --handler ask --arg hi --seed 10 | sed -n 3p
{"crc":3538033750,"record":{"result":{"call_id":"1/1","output":{"ok":{"delta":"delta-v1","epsilon":"epsilon-v1","theta":"theta-v1"}},"seq":1}}}
$ hl run docs/snippets/listing.hl --handler ask --arg hi --seed 10 | grep -o '"failed":"[^"]*"'
"failed":"input of read: missing 'path'"
"failed":"docs__epsilon was not admitted: listing docs did not list epsilon in this turn"
"failed":"input of docs__theta: 'beta-1' is not a parameter"
$ hl run docs/snippets/listing.hl --handler ask --arg hi --seed 4 | grep -o '"failed":"docs__epsilon[^"]*"'
"failed":"docs__epsilon changed since admission; rediscover before executing"
```

Each round's listing read costs one effect, two records and one outbound
request. `hl sim` keeps a listing per declaration that starts with each of
eight names listed in `LISTED_PRESENT_PPM` = 500000 cases per million and
changes between turns, at every crash and at every model call: each name is
added, removed or given a new fingerprint in `LISTING_CHANGE_PPM` = 100000
cases per million. `LISTED_TOOL_PPM` = 250000 of the model's calls name a
listed tool, `UNLISTED_TOOL_PPM` = 250000 of those a name its listing may
not hold, and `INPUT_INVALID_PPM` = 100000 of the calls to a declared or
listed tool carry input that does not fit (not JSON, a parameter missing or
one too many). The `tool_admitted` invariant requires that every listed
tool that ran had the fingerprint its turn's listing admitted, and that every
tool with a declared input ran only with input that fits it. Three planted
bugs break it: `listing_unchecked` sends listed tools without their
admission, `input_unchecked` sends input that does not fit, and
`listed_class_dropped` journals an external listed tool as `none`, so a
rerun sends it again:

```console
$ hl sim --target native-jsonl --seeds 0..200 --mutation listing_unchecked examples/codex_agent.hl | head -n 1
FAIL invariant=tool_admitted seed=27 target=native-jsonl profile=clean mutation=listing_unchecked: listed call 2/10 (mcp__delta) ran as Some("delta-v1") while listing mcp admitted None in turn 2: a tool that changed since admission must be refused
$ hl sim --target native-jsonl --seeds 0..200 --mutation input_unchecked examples/codex_agent.hl | head -n 1
FAIL invariant=tool_admitted seed=25 target=native-jsonl profile=errors mutation=input_unchecked: call 2/7 (exec_command) ran with input "{\"beta-24\":true,\"cmd\":\"gamma-43\",\"shell\":\"delta-62\",\"tty\":false}", which does not fit its declared input: not a JSON object
$ hl sim --target native-jsonl --seeds 0..200 --mutation listed_class_dropped examples/codex_agent.hl | head -n 1
FAIL invariant=replay_equal seed=13 target=native-jsonl profile=async mutation=listed_class_dropped: replaying the journal failed: Corrupt("replay of turn 4 diverged at seq 14: tool 'mcp__alpha' was recorded with another class or input")
```

The checker's listing errors:

```console
$ hl check examples/invalid/listing_tool_name.hl
examples/invalid/listing_tool_name.hl:5:6: error: tool 'docs__search' is named like a tool of listing 'docs'
$ hl check examples/invalid/listing_unknown.hl
examples/invalid/listing_unknown.hl:11:5: error: 'unknown' is not a variant of known<u64>
$ hl check docs/snippets/errors/listing-name.hl
docs/snippets/errors/listing-name.hl:5:20: error: listing 'team__docs' names its tools 'team__docs__NAME', so its name holds no '__' and does not end with '_'
$ hl check docs/snippets/errors/listing-arguments.hl
docs/snippets/errors/listing-arguments.hl:8:14: error: cannot append a notice to 'query': it is a local binding, and only state fields change
$ hl check docs/snippets/errors/listing-bound.hl
docs/snippets/errors/listing-bound.hl:3:26: error: the tools one listing may hold must be a named constant, found number 16
```

```hl docs/snippets/errors/listing-name.hl
harness notes

const docs_max: u64 = 16

tools from listing(team__docs, docs_max) (query: text) effect none

on ping() -> u64 { return 1 }
```

```hl docs/snippets/errors/listing-arguments.hl
harness notes

const docs_max: u64 = 16

tools from listing(docs, docs_max) (query: text) effect none

on admit(query: text) -> u64 {
  match docs(query) {
    ok(listed) => { return listed }
    failed(error) => { return 0 }
  }
}
```

```hl docs/snippets/errors/listing-bound.hl
harness notes

tools from listing(docs, 16) (query: text) effect none

on ping() -> u64 { return 1 }
```

### Listing notices and tool search

A listing can change while a turn runs, as nanocode's dynamic tools do: its
Claude backend reads the tool list again before every request
(`refresh_dynamic_tools`) and tells the model in a system message which tools
were added or removed since the prompt it cached. Two additions write that.

`name(context)` reads the listing like `name()` and then, when this read's
fingerprints differ from the turn's previous successful read of the same
listing, appends the builtin entry `notice(text)` to `context`, a state field
of type `list<entry, _>`. The text names each tool the read added, changed
(its fingerprint differs) or removed, in sorted order; the first read of a
turn and a failed read append nothing. The notice is a function of the
journaled listing results, so a replay appends the same entries; it is one
more push, so the prover counts it against the context's capacity like any
other. The `notice` variant exists in every program with a model and a
listing.

`tool T(...) -> text effect C finds L` declares `T` a search over listing
`L`, as nanocode's `ToolSearch` discovers deferred tools. Its result is the
names it found, one per line. Each must be `L__NAME` for a tool the turn's
latest read of `L` listed, and there may be at most `L`'s bound of them;
otherwise the result is recorded as the call's failure (`search found
mcp__zeta, which listing mcp did not list in this turn`), checked at run
time against the journaled read. A listing that a tool finds is deferred:
`dispatch` admits its tools only once a search result of the same turn named
them (`mcp__alpha was not discovered: no search of listing mcp found it in
this turn`), on top of the listing's own admission and fingerprint check. A
search changes nothing, so its class is `none` or `idempotent`:

```hl docs/snippets/catalog.hl
harness catalog

const calls_max: u64 = 4
const rounds_max: u64 = 8
const context_max: u64 = 64
const mcp_max: u64 = 8

type outcome = done(text) | stopped(text) | exhausted

tool search(query: text) -> text effect none finds mcp
tools from listing(mcp, mcp_max) (query: text) effect external

model think calls calls_max

state {
  context: list<entry, context_max>
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (2 + calls_max) <= context_max
{
  push context user(question)
  loop round below rounds_max {
    match mcp(context) {
      ok(listed) => {}
      failed(error) => { return stopped(error) }
    }
    let reply = think(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return done(words) }
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

```console
$ hl check docs/snippets/catalog.hl | grep -E 'worst case'
  handler ask: worst case 48 effects, 91 journal records, 48 outbound requests
$ hl run docs/snippets/catalog.hl --handler ask --arg hi --seed 12 | grep -o 'tools of listing mcp changed: [^\\]*' | head -n 2
tools of listing mcp changed: removed mcp__epsilon
tools of listing mcp changed: added mcp__eta
$ hl run docs/snippets/catalog.hl --handler ask --arg hi --seed 12 | grep -o '"failed":"[a-z_]* found [^"]*"' | head -n 1
"failed":"search found mcp__zeta, which listing mcp did not list in this turn"
$ hl run docs/snippets/catalog.hl --handler ask --arg hi --seed 12 | grep -o '"failed":"mcp__[a-z]* was not discovered[^"]*"' | head -n 1
"failed":"mcp__alpha was not discovered: no search of listing mcp found it in this turn"
$ hl check docs/snippets/errors/search-external.hl
docs/snippets/errors/search-external.hl:6:6: error: tool 'search' finds tools of listing 'mcp': a search changes nothing, so its effect is none or idempotent
```

```hl docs/snippets/errors/search-external.hl
harness notes

const calls_max: u64 = 4
const mcp_max: u64 = 8

tool search(query: text) -> text effect external finds mcp
tools from listing(mcp, mcp_max) (query: text) effect none

model think calls calls_max

on ping() -> u64 { return 1 }
```

The simulator answers a search with up to `FOUND_TOOLS_MAX` = 3 names drawn
from the listing as it stands, `UNLISTED_TOOL_PPM` of them names it may not
hold. `search_listed` requires that every name a journaled search found was
listed by its turn's latest read before it, and `tool_admitted` that a
deferred tool ran only after a search of its turn found it. `search_unchecked`
records a search without checking its names, `discovery_unchecked` admits
deferred tools without a search, and `notice_dropped` leaves the notice out
of the live run, which replay appends:

```console
$ hl sim --target native-jsonl --seeds 0..200 --twice --mutation search_unchecked docs/snippets/catalog.hl | head -n 1
FAIL invariant=search_listed seed=2 target=native-jsonl profile=lying mutation=search_unchecked: search 2/5 (search) recorded "mcp__zeta" as found, which the latest read of listing mcp in turn 2 did not list
$ hl sim --target native-jsonl --seeds 0..200 --twice --mutation discovery_unchecked docs/snippets/catalog.hl | head -n 1
FAIL invariant=tool_admitted seed=2 target=native-jsonl profile=lying mutation=discovery_unchecked: listed call 2/3 (mcp__beta) ran although no search in turn 2 found it: listing mcp is deferred
$ hl sim --target native-jsonl --seeds 0..200 --twice --mutation notice_dropped docs/snippets/catalog.hl | head -n 1
FAIL invariant=checkpoint_compacted seed=4 target=native-jsonl profile=async mutation=notice_dropped: checkpoint 1 holds 7 entries of context where replaying the journal to turn 1 gives 8 entries of context: a checkpoint must hold the lists as clear and keep left them
```

### model

`model name calls bound` declares the model. Calling it, `name(context)`, takes a
`list<entry, N>` and returns a `reply`; `bound` caps the tool calls in one
reply, so `calls(requested)` binds a `list<call, bound>`. A reply with more
calls, or one that does not decode, traps the turn. `model name calls bound
reply type` declares the reply's type instead, and calling the model then
returns `known<type>` ([Model replies](#model-replies)). The `reply` and
`entry` types exist only when a model is declared:

```console
$ hl check docs/snippets/errors/needs-model.hl
docs/snippets/errors/needs-model.hl:3:15: error: type 'entry' needs a model declaration: its replies are bounded by the model's calls
```

```hl docs/snippets/errors/needs-model.hl
harness notes

on ping(said: entry) -> u64 { return 1 }
```

`modes name, ...` after the model declares a finite set of call modes, and
`name(context) mode m` calls the model in mode `m`, which must be one of
them. A mode is a name the host interprets, such as Claude's `tool_choice`
of `none` for `text_only`: the program decides when to use it, the host how.
The host receives the mode with the request, the `model` record carries it
as `"mode"` (a call without a mode writes none), and a replay whose site asks
for a different mode than the journal records is a divergence, as a
different tool would be. A mode is one more model call site, so the worst
case counts it like any other. The simulator records the mode of every
request; `mode_sent` requires that each journaled reply answered a request
sent in the mode its record names, and `mode_dropped` sends the request
without its mode:

```hl docs/snippets/modes.hl
harness summarizer

const calls_max: u64 = 4
const context_max: u64 = 64

type outcome = summary(text) | stopped(text)
type turn_reply = answer(text) | calls(list<call, calls_max>)

model think calls calls_max reply turn_reply modes text_only

state {
  context: list<entry, context_max>
}

on summarize(conversation: text) -> outcome
  requires len(context) + 1 <= context_max
{
  push context user(conversation)
  match think(context) {
    failed(error) => { return stopped(error) }
    ok(reply) => {
      match reply {
        answer(words) => { return summary(words) }
        calls(_) => {
          match think(context) mode text_only {
            failed(error) => { return stopped(error) }
            ok(forced) => {
              match forced {
                answer(words) => { return summary(words) }
                calls(_) => { return stopped("the summary called a tool") }
              }
            }
          }
        }
      }
    }
  }
}
```

```console
$ hl run docs/snippets/modes.hl --handler summarize --arg hi --seed 0 | grep -o '"model":{"call_id":"1/2","mode":"text_only"'
"model":{"call_id":"1/2","mode":"text_only"
$ hl check docs/snippets/errors/undeclared-mode.hl
docs/snippets/errors/undeclared-mode.hl:13:35: error: model 'think' declares no mode 'summary'
$ hl sim --target vercel --seeds 0..200 --twice --mutation mode_dropped docs/snippets/modes.hl | head -n 1
FAIL invariant=mode_sent seed=0 target=vercel profile=errors mutation=mode_dropped: model call 1/2 is journaled with mode text_only but its reply answered a request sent with no mode
```

```hl docs/snippets/errors/undeclared-mode.hl
harness notes

const calls_max: u64 = 4
const context_max: u64 = 8

model think calls calls_max modes text_only

state {
  context: list<entry, context_max>
}

on ping() -> u64 {
  let reply = think(context) mode summary
  return 1
}
```

`settings(name: type = default, ...)` after the modes declares the model's request
settings, typed values the host sends with every request, such as Claude's
reasoning effort or a fast mode. A setting is a `bool` or a [text
enum](#text-enums), and its default is a literal of that type.
`name(context) with {setting: e, ...}` gives a call's settings, where `e` is
any expression of the setting's type, a state field among them, so a
harness keeps its settings as session state that ordinary handlers change
between turns and every call follows. A setting the call leaves out takes its
default. Settings combine with a mode: `think(context) mode text_only with
{effort: chosen}`. Every request of a model with settings carries all of
them, the `model` record journals them as `"settings"`, an object of every
setting's value (a model without settings writes none, so other journals are
unchanged), and a replay whose call computes other settings than the record
names diverges, as a different mode does. Settings add no call site, so the
worst case is unchanged. The simulator records the settings of every request;
`settings_sent` requires that each journaled reply answered a request sent with
the settings its record names, and `settings_dropped` sends the request without
them:

```hl docs/snippets/settings.hl
harness tuned

const calls_max: u64 = 4
const context_max: u64 = 64

type effort = "low" | "medium" | "high"
type outcome = answered(text) | stopped(text)
type turn_reply = answer(text) | calls(list<call, calls_max>)

model think calls calls_max reply turn_reply settings(effort: effort = "high", fast: bool = false)

state {
  context: list<entry, context_max>
  chosen: effort
  quick: bool
}

on configure(level: effort, fast: bool) -> u64 {
  chosen = level
  quick = fast
  return 0
}

on ask(question: text) -> outcome
  requires len(context) + 2 <= context_max
{
  push context user(question)
  match think(context) with {effort: chosen, fast: quick} {
    failed(error) => { return stopped(error) }
    ok(reply) => {
      match reply {
        answer(words) => {
          push context user(words)
          return answered(words)
        }
        calls(_) => {
          match think(context) {
            failed(error) => { return stopped(error) }
            ok(forced) => {
              match forced {
                answer(words) => { return answered(words) }
                calls(_) => { return stopped("the model kept calling tools") }
              }
            }
          }
        }
      }
    }
  }
}
```

`configure` is a turn of its own that only assigns state, and the simulator
draws it between the turns that ask, with random arguments, so the settings
change from turn to turn. The first call of `ask` sends the stored settings and
the second leaves them out, so it sends the defaults; a state field of a text
enum starts at its first value, `"low"`:

```console
$ hl run docs/snippets/settings.hl --handler ask --arg hi --seed 0 | grep -o '"settings":{[^}]*}'
"settings":{"effort":"low","fast":false}
"settings":{"effort":"high","fast":false}
$ hl sim --target vercel --seeds 0..200 --twice --mutation settings_dropped docs/snippets/settings.hl | head -n 1
FAIL invariant=settings_sent seed=0 target=vercel profile=errors mutation=settings_dropped: model call 1/1 is journaled with settings {"effort":"low","fast":false} but its reply answered a request sent with no settings
$ hl check docs/snippets/errors/setting-type.hl
docs/snippets/errors/setting-type.hl:5:46: error: setting 'budget' of model 'think' is u64: a model setting is a bool or a text enum, the values a host can send
$ hl check docs/snippets/errors/undeclared-setting.hl
docs/snippets/errors/undeclared-setting.hl:15:36: error: model 'think' declares no setting 'speed'
```

```hl docs/snippets/errors/setting-type.hl
harness notes

const calls_max: u64 = 4

model think calls calls_max settings(budget: u64 = calls_max)

on ping() -> u64 { return 1 }
```

```hl docs/snippets/errors/undeclared-setting.hl
harness notes

const calls_max: u64 = 4
const context_max: u64 = 8

type effort = "low" | "high"

model think calls calls_max settings(effort: effort = "high")

state {
  context: list<entry, context_max>
}

on ping() -> u64 {
  let reply = think(context) with {speed: true}
  return 1
}
```

`examples/claude_code.hl` keeps Claude's reasoning effort so, as nanocode's
session settings do: `set_effort` stores it between turns, each `prompt` turn
pins the effort in effect when it starts, and its requests and prompt cache
refreshes send the pinned value, which hl-agent puts in the request's
`output_config`.

### Wire models

`wire anthropic` or `wire openai` at the end of a model declaration makes
the program, not the host, build the model's HTTP request. The call sends
`head`, the context rendered as the API's messages (Messages for
`anthropic`, Responses input for `openai`) and `tail` to `url` with
`headers`, all four `text` settings the program sets, and decodes the
answer into the model's reply itself. The host only moves bytes: it
substitutes `${NAME}` in headers from its environment, and `${@PATH}` with
the trimmed text of the file at `PATH`, or `${@PATH#a.b}` with the text
field `a.b` of that file's JSON (either takes a `:-default`), sends the
request and hands back the status, headers and body. The value never enters
the journal, so a program can name a login file (`codex_agent.hl` names
Codex's `auth.json`, `claude_code.hl` Claude Code's `.credentials.json`)
and the host reads its current token at each request. A wire answer that is
not a success becomes `rejected(text)` (or `throttled` or `transient`); a
402 whose `WWW-Authenticate` is an MPP `Payment` challenge keeps that challenge
in the text between `@@hl-challenge@@` and `@@hl-end@@`, so a program can pay it
and retry (the agents below do). A wire model's reply type must
have the shape its wire decodes, and its settings must name those four
fields. The interpreter, the simulator and the compiled targets share one
codec (`src/wire`, mirrored in the WebAssembly runtime), so the request a
journal records is the request every backend would send:

```hl docs/snippets/wire.hl
harness wired

const calls_max: u64 = 4
const rounds_max: u64 = 8
const context_max: u64 = 64
const window_tokens: u64 = 200000
const image_bytes_max: u64 = 1048576

type outcome = done(text) | failed(text) | exhausted
type claude_reply = answer(text, text) | calls(list<call, calls_max>, text, text)

tool read(path: text) -> text or blob<image_bytes_max> effect none
tool bash(command: text) -> text effect external

model claude calls calls_max reply claude_reply window window_tokens settings(url: text = "https://api.anthropic.com/v1/messages", headers: text = "[[\"x-api-key\",\"${ANTHROPIC_API_KEY}\"],[\"anthropic-version\",\"2023-06-01\"]]", head: text = "{\"model\":\"claude-opus-5-5\",\"max_tokens\":32000,\"stream\":true,\"messages\":", tail: text = "}") wire anthropic

state {
  context: list<entry, context_max>
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  loop round below rounds_max {
    match claude(context) {
      throttled(error, _) => { return failed(error) }
      transient(error, _) => { return failed(error) }
      rejected(error) => { return failed(error) }
      ok(reply, _, _, _, _, _) => {
        push context assistant(reply)
        match reply {
          answer(words, _) => { return done(words) }
          calls(requested, _, _) => {
            for call in requested {
              match dispatch(call) {
                ok(output) => { push context result(call, output) }
                blob(image) => { push context attached(call, image) }
                failed(error) => { push context error(call, error) }
                unknown => { push context unknown(call) }
              }
            }
          }
        }
      }
    }
  }
  return exhausted
}
```

The same program on the OpenAI Responses wire, which the checks run as well:

```hl docs/snippets/wire_openai.hl
harness wired

const calls_max: u64 = 4
const rounds_max: u64 = 8
const context_max: u64 = 64
const window_tokens: u64 = 200000
const image_bytes_max: u64 = 1048576

type outcome = done(text) | failed(text) | exhausted
type claude_reply = message(text, text, bool) | calls(list<call, calls_max>, text, text)

tool read(path: text) -> text or blob<image_bytes_max> effect none
tool bash(command: text) -> text effect external

model claude calls calls_max reply claude_reply window window_tokens settings(url: text = "https://api.anthropic.com/v1/messages", headers: text = "[[\"x-api-key\",\"${ANTHROPIC_API_KEY}\"],[\"anthropic-version\",\"2023-06-01\"]]", head: text = "{\"model\":\"claude-opus-5-5\",\"max_tokens\":32000,\"stream\":true,\"messages\":", tail: text = "}") wire openai

state {
  context: list<entry, context_max>
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  loop round below rounds_max {
    match claude(context) {
      throttled(error, _) => { return failed(error) }
      transient(error, _) => { return failed(error) }
      rejected(error) => { return failed(error) }
      ok(reply, _, _, _, _, _) => {
        push context assistant(reply)
        match reply {
          message(words, _, _) => { return done(words) }
          calls(requested, _, _) => {
            for call in requested {
              match dispatch(call) {
                ok(output) => { push context result(call, output) }
                blob(image) => { push context attached(call, image) }
                failed(error) => { push context error(call, error) }
                unknown => { push context unknown(call) }
              }
            }
          }
        }
      }
    }
  }
  return exhausted
}
```

`examples/claude_code.hl` and `examples/claude_code_child.hl` are
`wire anthropic` programs and `examples/codex_agent.hl` a `wire openai`
one; each sets its request's settings in a `configure` handler the host runs
before every prompt.

### state

`state { name: type ... }` is the session's state, the only thing that lives
across turns. Fields start at zero (`0`, `false`, empty text, a text enum's
first value, an empty list), so a field is `u64`, `bool`, `text`, a text enum
or a list. State is not journaled: it is a
function of the journal, rebuilt by replay from the latest checkpoint.

```console
$ hl check docs/snippets/errors/state-sum.hl
docs/snippets/errors/state-sum.hl:6:12: error: state field 'current' has no initial value: state fields are u64, bool, text, text enums or lists
```

```hl docs/snippets/errors/state-sum.hl
harness notes

type phase = idle | busy(text)

state {
  current: phase
}

on ping() -> u64 { return 1 }
```

### invariant

`invariant name: condition` is a pure `bool` expression over state and
constants that must hold in the zero state and wherever a turn can end
(every `return` and every trap the prover cannot rule out). The prover
assumes the invariants when a turn starts. It reasons about `u64` comparisons
joined by `and`, `or` and `not`; an invariant over `bool` or `text` fails
rather than being assumed.

### on (handlers)

`on name(param: type, ...) -> type [requires condition] { ... }` handles one
kind of turn. Its parameters are the turn's arguments, recorded in the
`admitted` record. `requires` is a pure condition checked at admission: when
false, the turn finishes `failed` with `requires(name)` before the body runs.
Every path through the body must end in `return`, and nothing may follow a
`return` in its block.
`on side name(...)` declares a side handler, which answers a question
without a turn ([Side questions](#side-questions)).

```console
$ hl check docs/snippets/errors/no-return.hl
docs/snippets/errors/no-return.hl:3:28: error: handler 'ping' can reach its end without returning
$ hl check docs/snippets/errors/unreachable.hl
docs/snippets/errors/unreachable.hl:5:3: error: statement is unreachable after a return
$ hl check docs/snippets/errors/impure-requires.hl
docs/snippets/errors/impure-requires.hl:4:18: error: the requires of handler 'ping' must be pure: it calls an effect
```

```hl docs/snippets/errors/no-return.hl
harness notes

on ping(value: u64) -> u64 {
  if value > 1 {
    return value
  }
}
```

```hl docs/snippets/errors/unreachable.hl
harness notes

on ping() -> u64 {
  return 1
  return 2
}
```

```hl docs/snippets/errors/impure-requires.hl
harness notes

on ping() -> u64
  requires now() > 0
{
  return 1
}
```

### on (events)

`on resume` declares that a turn ended by `suspend` takes a resume event carrying
its key and a text answer; [Suspend and resume](#suspend-and-resume) has the
details. The events below reach a running turn.

`on cancel` and `on steer(message: text) bound N` declare the events a running
turn takes from outside it (a third, `on job(notice: text) bound N`, takes
the notices of background jobs: [Effects in flight](#effects-in-flight)): a person cancelling the turn, or steering it with
one more message, as nanocode's turn inbox delivers them. A handler reads them
with two expressions, and a read writes no record and takes no sequence
number:

- `cancelled` is `true` once a cancel is recorded for this turn. With `on cancel`
  declared, nothing else may be named `cancelled`: not a variant, a field, a
  constant or a local.
- `take steers` is a `list<text, N>` of the steers recorded since the turn's last
  `take`, oldest first. Like an effect, it runs before the statement it
  appears in.

Both belong in a handler's body (or a fn it calls): not in `requires`, which
runs before the turn takes anything, and not in an invariant. `bound` names
the most steers one turn records; the steer's one parameter is
`message: text`, and the cancel carries nothing.

How the runtime records them:

- When an event reaches a running turn, the runtime appends one `event` record,
  nanocode-turnlog's `Event`: `{"event":{"at":S,"event":{"steer":{"id":I,"message":M}},"seq":N,"turn":T}}`
  (a cancel is `{"cancel":{"id":I}}`). `at` is the sequence number of the
  effect the event arrived during, and `seq` numbers it within the session:
  (turn - 1) times the events one turn records at most, plus its place in the
  turn, so it increases as nanocode's fold requires and is known from the
  turn alone. The sender's `id` makes a delivery sent again a duplicate.
- A read between effects `s` and `s + 1` returns the events recorded with
  `at <= s`: those recorded before the next sequence number. Events enter only
  while an effect runs, never between a read and the next effect, so every
  read at one place sees the same events, and replaying the journal shows
  each read exactly what it saw. A turn re-run after a crash takes no new
  events at the effects it replays.
- The sender is answered only once the record is durable: `recorded`,
  `duplicate` (already recorded, or a second cancel), `full` (the turn holds
  its `bound` of steers), `undeclared` (the harness takes no such event) or
  `finished` (the turn ended before taking it). A turn records at most one
  cancel and `N` steers, so `hl check` reports them beside the worst case.
- Where they come in: native and server targets drain a `std::sync::mpsc`
  channel the host's HTTP endpoint fills (`Inbox::channel` in the generated
  code), after every effect the turn runs, and flush before answering; on
  Postgres only the lease owner's endpoint takes them. Cloudflare takes
  `POST /event` on the session's Durable Object while the turn awaits a tool
  or the model, and answers after `storage.sync()`. Vercel takes `resumeHook`
  payloads on the turn's events hook; the workflow function records them at
  each step boundary and answers them with a step. `hl check --target T`
  prints each target's path with the platform facts it rests on.

```hl docs/snippets/events.hl
harness worker

const steps_max: u64 = 8
const steers_max: u64 = 4
const notes_max: u64 = 64

type outcome = done(text) | stopped

tool work(task: text) -> text effect external

on cancel
on steer(message: text) bound steers_max

state {
  notes: list<text, notes_max>
}

invariant notes_bounded: len(notes) <= notes_max

on run(task: text) -> outcome
  requires len(notes) + steps_max * (1 + steers_max) <= notes_max
{
  loop step below steps_max {
    for steer in take steers {
      push notes steer
    }
    match work(task) {
      ok(output) => { push notes output }
      failed(error) => { push notes error }
      unknown => { return stopped }
    }
    if cancelled { return stopped }
  }
  return done(task)
}
```

```console
$ hl check docs/snippets/events.hl
docs/snippets/events.hl: harness worker: 1 tool(s), 1 state field(s), 1 handler(s)
  handler run: worst case 8 effects, 19 journal records, 8 outbound requests
    effects: step below steps_max (8)
    journal records: step below steps_max (8)
    outbound requests: step below steps_max (8)
    plus one recovery record per restart
    plus at most 5 event record(s) per turn (1 cancel, steers_max (4) steer(s))
...
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/events.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=1338 crashes=834 power_losses=135 torn_writes=185 takeovers=0 fenced=0 dispatches=1410 unknown=362 events=177
$ hl check docs/snippets/errors/event-steer-shape.hl
docs/snippets/errors/event-steer-shape.hl:5:4: error: event 'steer' carries one text: write 'on steer(message: text) bound N'
$ hl check docs/snippets/errors/event-cancelled-variant.hl
docs/snippets/errors/event-cancelled-variant.hl:3:23: error: variant 'cancelled' hides the cancel event's read: rename it
$ hl check docs/snippets/errors/event-requires.hl
docs/snippets/errors/event-requires.hl:6:16: error: 'cancelled' reads the turn's events, so it belongs in a handler's body
```

```hl docs/snippets/errors/event-steer-shape.hl
harness worker

const steers_max: u64 = 4

on steer(note: text) bound steers_max

on run() -> u64 { return 1 }
```

```hl docs/snippets/errors/event-cancelled-variant.hl
harness worker

type outcome = done | cancelled

on cancel

on run() -> outcome { return done }
```

```hl docs/snippets/errors/event-requires.hl
harness worker

on cancel

on run() -> u64
  requires not cancelled
{
  return 1
}
```

The simulator sends events to half the turns of a harness that declares any
(`EVENT_TURN_PPM` = 500000 per million): a cancel in `CANCEL_PPM` = 400000 per
million of them and up to `N` + 1 steers, so `full` is reached, each arriving
at one of the first `ARRIVAL_HOOKS_MAX` = 48 hooks of the turn, so during a
tool call, at a flush or after a crash, and sent again until answered. Its
invariants: `event_before_read` (a read never sees an event recorded after it),
`event_replay_equal` (a replayed read sees exactly what its journal holds, and
no event is recorded at an earlier place than one before it), `event_once` (no
event is recorded twice) and `event_kept` (every event is answered, and one
answered as delivered is in the journal). Their mutations are
`event_read_unstamped`, `event_drain_on_replay`, `event_redelivered` and
`event_ack_early`, and `batch_drains_replayed` for a parallel for (below).

### fn

`fn name(param: type, ...) { ... }` names a block of statements that handlers
share, and the statement `name(argument, ...)` runs it. The checker inlines the
body at each call before it lowers the handler, so every handler that calls a
fn gets its own effect sites, sequence numbers, worst case and proofs, as if
the body were written there. A call is the same as writing, in a block of its
own, `let param: type = argument` for each parameter in order, then the body:

- A fn sees its parameters, its own bindings and the harness's declarations
  (constants, types and their variants, tools, the model, state fields), never
  its caller's locals, so its names may repeat its caller's.
- A fn has no value. A `return` in its body finishes the turn of the handler
  that called it, with that handler's result type; a body that does not
  return goes on to the statement after the call.
- A fn may call another fn but never itself, directly or through others.
- Every fn is called somewhere: only an inlined body is checked.

`examples/subagents.hl` shares its model loop between its two handlers this way.
This program shares a comment and a log line between two handlers:

```hl docs/snippets/fn.hl
harness reporter

const lines_max: u64 = 64

type report = posted(text) | refused(text) | lost

tool comment(change: text, body: text) -> text effect external

state {
  lines: list<text, lines_max>
}

fn note(label: text, detail: text) {
  push lines label ++ ": " ++ detail
}

fn post(target: text, body: text) {
  match comment(target, body) {
    ok(link) => {
      note("posted", link)
      return posted(link)
    }
    failed(error) => {
      note("refused", error)
      return refused(error)
    }
    unknown => { return lost }
  }
}

on approve(change: text) -> report
  requires len(lines) < lines_max
{
  post(change, "approved")
}

on withdraw(change: text, reason: text) -> report
  requires len(lines) < lines_max
{
  post(change, "withdrawn: " ++ reason)
}
```

Written out by hand, it is this program, and every target emits the same code
for both:

```hl docs/snippets/fn_inline.hl
harness reporter

const lines_max: u64 = 64

type report = posted(text) | refused(text) | lost

tool comment(change: text, body: text) -> text effect external

state {
  lines: list<text, lines_max>
}

on approve(change: text) -> report
  requires len(lines) < lines_max
{
  let target: text = change
  let body: text = "approved"
  match comment(target, body) {
    ok(link) => {
      let label: text = "posted"
      let detail: text = link
      push lines label ++ ": " ++ detail
      return posted(link)
    }
    failed(error) => {
      let label: text = "refused"
      let detail: text = error
      push lines label ++ ": " ++ detail
      return refused(error)
    }
    unknown => { return lost }
  }
}

on withdraw(change: text, reason: text) -> report
  requires len(lines) < lines_max
{
  let target: text = change
  let body: text = "withdrawn: " ++ reason
  match comment(target, body) {
    ok(link) => {
      let label: text = "posted"
      let detail: text = link
      push lines label ++ ": " ++ detail
      return posted(link)
    }
    failed(error) => {
      let label: text = "refused"
      let detail: text = error
      push lines label ++ ": " ++ detail
      return refused(error)
    }
    unknown => { return lost }
  }
}
```

```console
$ for target in native-jsonl native-sqlite server-postgres cloudflare vercel; do hl emit --target $target docs/snippets/fn_inline.hl > output/fn_inline.wasm; hl emit --target $target docs/snippets/fn.hl | cmp - output/fn_inline.wasm && echo "$target: identical"; done
native-jsonl: identical
native-sqlite: identical
server-postgres: identical
cloudflare: identical
vercel: identical
```

A model loop that asks again after running tools by calling itself is
recursion, which the checker refuses, naming the cycle; a `loop` with a named
bound is how a harness repeats:

```hl examples/invalid/fn_recursion.hl
harness fn_recursion

const calls_max: u64 = 4
const context_max: u64 = 64

type outcome = completed(text) | failed(text)

tool read(path: text) -> text effect none

model infer calls calls_max

state {
  context: list<entry, context_max>
}

fn ask() {
  let reply = infer(context)
  push context assistant(reply)
  match reply {
    answer(words) => { return completed(words) }
    calls(requested) => { run_tools(requested) }
  }
}

fn run_tools(requested: list<call, calls_max>) {
  for call in requested {
    match dispatch(call) {
      ok(output) => { push context result(call, output) }
      failed(error) => { push context error(call, error) }
      unknown => { push context unknown(call) }
    }
  }
  ask()
}

on prompt(input: text) -> outcome {
  push context user(input)
  ask()
}
```

```console
$ hl check examples/invalid/fn_recursion.hl
examples/invalid/fn_recursion.hl:33:3: error: fn 'ask' is recursive: ask -> run_tools -> ask; a fn is inlined where it is called, so it cannot call itself, directly or through another fn
$ hl check docs/snippets/errors/fn-self.hl
docs/snippets/errors/fn-self.hl:9:3: error: fn 'again' is recursive: again -> again; a fn is inlined where it is called, so it cannot call itself, directly or through another fn
$ hl check examples/invalid/fn_arity.hl
examples/invalid/fn_arity.hl:16:3: error: fn 'note' takes 2 argument(s), found 1
$ hl check examples/invalid/fn_hidden_name.hl
examples/invalid/fn_hidden_name.hl:10:14: error: fn 'remember' cannot see 'input': a fn sees its parameters, its own bindings and the harness's declarations
$ hl check docs/snippets/errors/fn-expression.hl
docs/snippets/errors/fn-expression.hl:8:15: error: fn 'greet' has no value: call it as a statement of its own, not inside an expression
$ hl check docs/snippets/errors/fn-statement.hl
docs/snippets/errors/fn-statement.hl:6:3: error: 'read' is not a fn: only a fn is called as a statement; bind an effect's result with let or match it
$ hl check docs/snippets/errors/fn-unused.hl
docs/snippets/errors/fn-unused.hl:7:4: error: fn 'reset' is never called
```

```hl docs/snippets/errors/fn-self.hl
harness notes

state {
  count: u64
}

fn again() {
  count = count + 1
  again()
}

on ping() -> u64 {
  again()
  return count
}
```

```hl docs/snippets/errors/fn-expression.hl
harness notes

fn greet(name: text) {
  return "hello " ++ name
}

on ping(name: text) -> text {
  let words = greet(name)
  return words
}
```

```hl docs/snippets/errors/fn-statement.hl
harness notes

tool read(path: text) -> text effect none

on ping(path: text) -> u64 {
  read(path)
  return 1
}
```

```hl docs/snippets/errors/fn-unused.hl
harness notes

state {
  count: u64
}

fn reset() {
  count = 0
}

on ping() -> u64 { return count }
```

### Pure fns

`fn name(param: type, ...) -> type { ... }` is a pure fn: a function with a
result that computes without effects. A call is an expression, `name(argument,
...)`, and runs the body out of line: the checker checks the body once and
every target compiles it to one function. A pure fn exists so a harness can
compute (parse, encode, sort, format) without spending journal records: it
calls no tool, model or effect, reads no clock, no state field and no event,
so a call writes nothing to the journal.

- A pure fn sees its parameters, its own bindings, the constants, the types
  and their variants, the builtins that compute, and the pure fns declared
  above it. A call to a pure fn declared below, or to itself, is an error, so
  a pure fn never recurses.
- Its body ends in `return`, and only its last statement returns: every path
  through the body reaches a `return`.
- `var name: type = value` binds a local that `name = value` may change; a
  `let` and a parameter never change. `var`, `set`, `while`, `/`, `%`,
  `xs[i]`, `[]` and `bytes<N>` live only inside pure fns.
- `loop i below N while condition { ... }` runs at most `N` times and stops
  early once `condition` is false, checked before each pass.
- `a / b` and `a % b` divide `u64`s; a zero divisor traps with
  `u64 division by zero in '/'`.
- `xs[i]` reads entry `i` of a list or a byte buffer, and `set xs[i] = v`
  changes it in a `var`; an index at or past `len(xs)` traps with `index`.
  `push xs v` appends to a `var` list or byte buffer, and traps with
  `bound(N)` past its capacity. `[]` is an empty list or byte buffer, typed by
  where it goes.
- `bytes<N>` is a byte buffer of at most `N` bytes. Its entries are `u64`s
  below 256, so a byte literal such as `'a'` is one. `len(b)` is its length,
  `push b x` appends a byte or another buffer (a byte at 256 or more traps
  with `byte`), `slice(b, from, to)` copies the bytes from `from` up to `to`
  (traps with `index` outside `b`), `bytes_of(t)` is the UTF-8 bytes of text
  `t` (traps with `bound(N)` past the capacity of where it goes), and
  `text_of(b)` is the text the bytes spell (traps with `text_of` unless they
  are UTF-8). `hex_of(b)` is the lowercase hex of the bytes, two digits each,
  and `bytes_of_hex(t)` is the bytes hex text spells, upper or lower case
  (traps with `hex` on an odd length or a non-hex digit, and with `bound(N)`
  past the capacity of where it goes). A byte buffer lives only inside pure fns: it goes in no state
  field, tool, sum, list or handler local, and a handler calls no pure fn
  that returns one.
- Values stay values: a list or byte buffer handed to a `var`, a `let` or a
  call is a copy, so `set` changes only the `var` it names.
- Compiled pure fns drop a runtime check they can show never fires: `+` and
  `*` whose operands' largest values fit a u64, `/` and `%` by a nonzero
  constant, and a `push` of a value below 256 skip their checks. Largest
  values come from constants, loop indexes below their bound, a byte read,
  `len` of a buffer, and a `let` no `set` changes, which an index that
  succeeded also bounds by its buffer's capacity. Every other check stays
  and traps as the interpreter does.

Every pure fn is bounded: its loops have named bounds, its buffers have
capacities and it never recurses, so every call ends. What the prover leaves
to run time inside a pure fn is arithmetic, indexing and capacity: an
overflow, a zero divisor, an index past the end or a push past the capacity
traps with its name, and the interpreter and every target trap alike. A
handler's own arithmetic stays proved, and the prover reads a call's result
as any value of its type.

This program formats numbers and text with pure fns, and its handler spends
the same three journal records as one that returns a constant:

```hl docs/snippets/pure.hl
harness digest

const buffer_max: u64 = 64
const digits_max: u64 = 20
const items_max: u64 = 8

fn hex(input: text) -> text {
  let source: bytes<buffer_max> = bytes_of(input)
  let table: bytes<buffer_max> = bytes_of("0123456789abcdef")
  var out: bytes<buffer_max> = []
  loop i below buffer_max while i < len(source) {
    push out table[source[i] / 16]
    push out table[source[i] % 0x10]
  }
  return text_of(out)
}

fn decimal(n: u64) -> text {
  var reversed: bytes<digits_max> = []
  var rest = n
  push reversed '0' + rest % 10
  rest = rest / 10
  loop step below digits_max while rest > 0 {
    push reversed '0' + rest % 10
    rest = rest / 10
  }
  var out: bytes<digits_max> = []
  loop i below digits_max while i < len(reversed) {
    push out reversed[len(reversed) - 1 - i]
  }
  return text_of(out)
}

fn sorted(input: list<u64, items_max>) -> list<u64, items_max> {
  var xs = input
  loop i below items_max while i < len(xs) {
    loop j below items_max while j + 1 + i < len(xs) {
      if xs[j] > xs[j + 1] {
        let held = xs[j]
        set xs[j] = xs[j + 1]
        set xs[j + 1] = held
      }
    }
  }
  return xs
}

fn spread(n: u64) -> text {
  var xs: list<u64, items_max> = []
  push xs n % 7
  push xs n % 5
  push xs n % 3
  let ordered = sorted(xs)
  return decimal(ordered[0]) ++ " " ++ decimal(ordered[1]) ++ " " ++ decimal(ordered[2])
}

fn share(n: u64) -> text {
  return decimal(1000 / (n % 4))
}

on digest(input: text, n: u64) -> text {
  return hex(input) ++ " " ++ share(n) ++ " " ++ spread(n)
}
```

This one turns text into bytes, spells them in hex and reads the hex back;
`bytes_of_hex` traps with `hex` on input such as `hi`, which is no hex:

```hl docs/snippets/pure_hex.hl
harness hexes

const buffer_max: u64 = 32

fn round(input: text) -> text {
  let raw: bytes<buffer_max> = bytes_of(input)
  let spelled = hex_of(raw)
  let back: bytes<buffer_max> = bytes_of_hex(spelled)
  var folded: bytes<buffer_max> = []
  loop i below buffer_max while i < len(back) {
    push folded back[i] % 26 + 'a'
  }
  return text_of(folded) ++ " " ++ spelled
}

fn strict(input: text) -> text {
  let parsed: bytes<buffer_max> = bytes_of_hex(input)
  return hex_of(parsed)
}

on hexes(input: text) -> text {
  return round(input) ++ " " ++ strict(input)
}
```

The checker refuses what would break those rules:

```console
$ hl check docs/snippets/errors/pure-recursion.hl
docs/snippets/errors/pure-recursion.hl:4:10: error: fn 'down' calls itself: a pure fn calls only the pure fns above it, so it never recurses
$ hl check docs/snippets/errors/pure-tool.hl
docs/snippets/errors/pure-tool.hl:6:10: error: fn 'peek' is pure: it cannot call 'read', since it calls no tool, model or effect and reads no clock
$ hl check docs/snippets/errors/pure-state.hl
docs/snippets/errors/pure-state.hl:8:14: error: fn 'next' is pure: it cannot read 'total', since it reads no state or events
$ hl check docs/snippets/errors/pure-var-outside.hl
docs/snippets/errors/pure-var-outside.hl:4:7: error: a var lives only in a pure fn (fn f(...) -> T), which computes without effects
$ hl check docs/snippets/errors/pure-set-let.hl
docs/snippets/errors/pure-set-let.hl:7:7: error: cannot set an entry of 'ys': it is a let or a parameter, and only a var changes
$ hl check docs/snippets/errors/pure-no-return.hl
docs/snippets/errors/pure-no-return.hl:3:24: error: fn 'half' can reach its end without returning
$ hl check docs/snippets/errors/pure-bytes-handler.hl
docs/snippets/errors/pure-bytes-handler.hl:6:35: error: bytes_of(...) lives only in a pure fn (fn f(...) -> T), which computes without effects
```

```hl docs/snippets/errors/pure-recursion.hl
harness counting

fn down(n: u64) -> u64 {
  return down(n)
}

on go(n: u64) -> u64 {
  return down(n)
}
```

```hl docs/snippets/errors/pure-tool.hl
harness reading

tool read(path: text) -> text effect none

fn peek(path: text) -> text {
  return read(path)
}

on go(path: text) -> text {
  return peek(path)
}
```

```hl docs/snippets/errors/pure-state.hl
harness counting

state {
  total: u64
}

fn next(n: u64) -> u64 {
  return n + total
}

on go(n: u64) -> u64 {
  return next(n)
}
```

```hl docs/snippets/errors/pure-var-outside.hl
harness counting

on go(n: u64) -> u64 {
  var count = n
  return count
}
```

```hl docs/snippets/errors/pure-set-let.hl
harness counting

const items_max: u64 = 8

fn first(xs: list<u64, items_max>) -> u64 {
  let ys = xs
  set ys[0] = 1
  return ys[0]
}

on go(n: u64) -> u64 {
  return n
}
```

```hl docs/snippets/errors/pure-no-return.hl
harness counting

fn half(n: u64) -> u64 {
  if n > 1 {
    return n / 2
  }
}

on go(n: u64) -> u64 {
  return n
}
```

```hl docs/snippets/errors/pure-bytes-handler.hl
harness buffers

const buffer_max: u64 = 64

on go(input: text) -> u64 {
  let buffer: bytes<buffer_max> = bytes_of(input)
  return len(buffer)
}
```

### Hooks

`before tool` and `after tool` run at every dispatch, as nanocode's
`hooked_handler` wraps every tool handler, and the lifecycle hooks run at the
points of a session that have no statement of their own. Like a fn, each is
inlined where it runs: it sees its parameters, its own bindings and the
harness's declarations, every handler that reaches the point gets its own
effect sites, worst case and proofs, and a hook that runs nowhere is refused.

| Hook | Runs | Binds |
| --- | --- | --- |
| `before tool(c: call)` | before every `match dispatch(...)` | the call |
| `after tool(c: call, outcome: maybe<text>)` | after the dispatch, before the match takes an arm | the call and its outcome |
| `on session start` | at the start of the session's first turn, whichever handler it runs | nothing |
| `on prompt submit(p: T)` | at the start of each handler that takes a parameter `p: T` | that parameter |
| `on pre compact`, `on post compact` | before and after every `keep` or `clear` of a list of entries | nothing |
| `on stop` | before every `return` | nothing |
| `on subagent start(agent: text)` | in the ok arm of a match on a spawn tool, or on a dispatch of one | the child's key |
| `on subagent stop(agent: text, result: text)` | after every `join` | the key and the child's result |
| `on session end(p: T) -> R { ... }` | as the handler `session_end`, after which every handler is refused | its parameters |

In `before tool`, `deny(reason)` refuses the call: the runtime appends a
`call` record with tool `deny`, class `none` and input
`{"id","input","reason","tool"}`, then its `failed(reason)` result, without
contacting the host, and the match takes its `failed` arm. `allow(input)`
dispatches the call with that text as its input instead, which the input gate
then checks as it checks the model's. `respond(text)` answers the call
in the program, so a tool the model calls by name can be one the program
implements rather than the host: the call's input passes the same input gate
`dispatch` would apply, and the runtime appends a `call` record with tool
`respond` and the same input as a `deny`, then an `ok(text)` result when
the input was admitted and the gate's `failed` otherwise, without contacting
the host. In `after tool`, `deny(reason)` withholds
an outcome the tool already returned: the match takes `failed(reason)` though
the tool's result stays in the journal. A block that ends without either lets
the call through as it is. Statements after an `if` or `match` that decided
run only when it did not, so guard clauses read as they do in nanocode:

```hl docs/snippets/hooks.hl
harness guarded

const calls_max: u64 = 4
const rounds_max: u64 = 4
const context_max: u64 = 64
const kept: u64 = 8

type outcome = completed(text) | exhausted

tool Read(file_path: text) -> text effect none
tool Bash(command: text) -> text effect external

model messages calls calls_max

state {
  context: list<entry, context_max>
  greeted: bool
}

invariant context_bounded: len(context) <= context_max

before tool(c: call) {
  if c.tool == "web_fetch" {
    deny("web_fetch is not offered here; the tool was not run")
  }
  if contains(c.input, "rm -rf") {
    deny("rm -rf is refused; the tool was not run")
  }
  if c.tool == "Bash" {
    let opening = before(c.input, "command")
    let quote = after(opening, "{")
    let strict = opening ++ "command" ++ quote ++ ":" ++ quote ++ "set -e; "
    allow(strict ++ after(c.input, ":" ++ quote))
  }
}

after tool(c: call, outcome: maybe<text>) {
  match outcome {
    ok(output) => {
      if contains(output, "secret") {
        deny("the output of " ++ c.tool ++ " held a secret and was withheld")
      }
    }
    failed(_) => {}
    unknown => {}
  }
}

on session start {
  greeted = true
}

on prompt submit(input: text) {
  push context user("Prompt submitted: " ++ input)
}

on pre compact {
  push context user("Compacting.")
}

on prompt(input: text) -> outcome
  requires len(context) + 2 + rounds_max * (1 + calls_max) <= context_max
{
  loop round below rounds_max {
    if len(context) + 1 + (1 + calls_max) > context_max {
      keep context last kept
    }
    let reply = messages(context)
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

```console
$ hl check docs/snippets/hooks.hl
docs/snippets/hooks.hl: harness guarded: 2 tool(s), 8 state field(s), 1 handler(s)
  handler prompt: worst case 20 effects, 39 journal records, 20 outbound requests
    effects: round below rounds_max (4) x call below calls_max (4)
    journal records: round below rounds_max (4) x call below calls_max (4)
    outbound requests: round below rounds_max (4) x call below calls_max (4)
    plus one recovery record per restart
...
```

The checker lowers the decision to five hidden state fields,
`_hook_decided`, `_hook_denied`, `_hook_responded`, `_hook_reason` and
`_hook_input`, so a hook's
code is inlined once per dispatch and the match's arms at most three times:
after a deny, after a withheld outcome and after the call ran. `on session
start` adds `_session_started` and `on session end` adds `_session_ended`, which
every handler's `requires` then tests. Names starting with `_` are reserved, so
no program can name them. A deny or a response writes two records and no
outbound request, so it never costs more than the dispatch it replaces.

A program implements `update_plan` and refuses a plan with two steps in progress:

```hl docs/snippets/respond.hl
harness planner

const calls_max: u64 = 4
const rounds_max: u64 = 8
const context_max: u64 = 64

type outcome = done(text) | exhausted

tool update_plan(plan: text, explanation: text = "") -> text effect none
tool bash(command: text) -> text effect external

model think calls calls_max

state {
  context: list<entry, context_max>
}

before tool(c: call) {
  if c.tool == "update_plan" {
    if contains(after(c.input, "\"in_progress\""), "\"in_progress\"") {
      deny("At most one step can be in_progress at a time.")
    }
    respond("Plan updated")
  }
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  loop round below rounds_max {
    let reply = think(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return done(words) }
      calls(requested) => {
        for call in requested parallel {
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

What the checker refuses:

```console
$ hl check docs/snippets/errors/deny-outside.hl
docs/snippets/errors/deny-outside.hl:6:3: error: 'deny' belongs in a 'before tool' or 'after tool' block, not in handler 'ping'
$ hl check docs/snippets/errors/deny-loop.hl
docs/snippets/errors/deny-loop.hl:8:3: error: a 'deny', 'allow' or 'respond' decides the call once, so it cannot sit inside a loop
$ hl check docs/snippets/errors/hook-dispatch.hl
docs/snippets/errors/hook-dispatch.hl:14:9: error: a hook cannot dispatch: a dispatch inside it would run without the hooks every dispatch runs
$ hl check docs/snippets/errors/plain-dispatch.hl
docs/snippets/errors/plain-dispatch.hl:12:17: error: match on dispatch(...) directly: 'before tool' runs at every dispatch and decides the arm its match takes
$ hl check docs/snippets/errors/hook-unused.hl
docs/snippets/errors/hook-unused.hl:5:1: error: 'before tool' runs nowhere: no handler dispatches a call
```

```hl docs/snippets/errors/deny-outside.hl
harness notes

tool read(path: text) -> text effect none

on ping(path: text) -> u64 {
  deny("no")
  return 1
}
```

```hl docs/snippets/errors/deny-loop.hl
harness notes

const tries_max: u64 = 4

tool read(path: text) -> text effect none

before tool(c: call) {
  loop attempt below tries_max {
    if c.tool == "write" {
      deny("read only")
    }
  }
}

on ping(c: call) -> u64 {
  match dispatch(c) {
    ok(_) => { return 1 }
    failed(_) => { return 0 }
    unknown => { return 0 }
  }
}
```

```hl docs/snippets/errors/hook-dispatch.hl
harness notes

const calls_max: u64 = 4

tool read(path: text) -> text effect none

model infer calls calls_max

state {
  context: list<entry, calls_max>
}

on stop {
  match dispatch(call("audit", "read", "{}")) {
    ok(_) => {}
    failed(_) => {}
    unknown => {}
  }
}

on ping(path: text) -> u64 {
  return 1
}
```

```hl docs/snippets/errors/plain-dispatch.hl
harness notes

tool read(path: text) -> text effect none

before tool(c: call) {
  if c.tool != "read" {
    deny("read only")
  }
}

on ping(c: call) -> u64 {
  let outcome = dispatch(c)
  return 1
}
```

```hl docs/snippets/errors/hook-unused.hl
harness notes

tool read(path: text) -> text effect none

before tool(c: call) {
  if c.tool != "read" {
    deny("read only")
  }
}

on ping(path: text) -> u64 {
  return 1
}
```

A parallel for dispatches its calls at once, so a harness with `on subagent
start` refuses one, and one whose `before tool` block runs an effect, `allow`s a
rewritten input or sits beside an `after tool` block (see [Lanes](#lanes)).
Otherwise the block decides every call of the batch before any is sent, and
a call it denies or answers with `respond` is settled in list order between
the batch's groups, so `examples/codex_agent.hl` answers `update_plan` in its
parallel round. The proofs and the cost bounds read the batch as the
sequential for it equals. The simulator watches the
host: a tool request that arrives between a deny site's run points is
`denied_unrun`, and the mutation `deny_dispatches`, a runtime that runs the
denied call anyway, is caught at once:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/hooks.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=1922 crashes=1418 power_losses=87 torn_writes=151 takeovers=0 fenced=0 dispatches=1563 unknown=339
$ hl sim --target native-jsonl --seeds 0..200 --mutation deny_dispatches docs/snippets/hooks.hl
FAIL invariant=denied_unrun seed=0 target=native-jsonl profile=errors mutation=deny_dispatches: call 2/6 was denied by its before tool block, yet the host ran web_fetch for it
replay: hl sim --target native-jsonl --seeds 0..1 --mutation deny_dispatches docs/snippets/hooks.hl
```

`examples/hooks.hl` ports nanocode's hooks: a `before tool` that applies the
permission mode, asks a person in `"ask"` mode through `suspend`, and then asks
a PreToolUse hook to allow, update or deny; an `after tool` that withholds an
outcome when the PostToolUse hook fails, with nanocode's notice; and the
SessionStart, UserPromptSubmit, PreCompact, PostCompact, Stop and SessionEnd
hooks through a `lifecycle_hook` tool whose answer becomes additional context.

## Types

| Type | Values | Operations |
| --- | --- | --- |
| `u64` | 0 to 18446744073709551615 | `+ - *` (trap on overflow and underflow), `< <= > >=`, `== !=` |
| `bool` | `true`, `false` | `and`, `or`, `not`, `== !=` |
| `text` | UTF-8 text | `++` (concatenate), `== !=`, [`contains`, `before`, `after`, `trim`](#text) |
| a declared sum | its variants | construct, `match` |
| a text enum | its listed texts | a text literal where it is expected, `match` |
| `list<T, N>` | up to `N` values of `T` | `push` (state fields only), `for`, `len(list)` |
| `known<T>` | `ok(T)`, `failed(text)` | `match` |
| `maybe<T>` | `ok(T)`, `failed(text)`, `unknown` | `match`, which must have an `unknown` arm |
| `blob<N>` | at most `N` opaque bytes, journaled by digest | a list or a variant's field, `attached` ([Blobs](#blobs)) |

Builtin sum types:

- `call = call(text, text, text)`, read with `.id`, `.tool` and `.input`.
- `reply = answer(text) | calls(list<call, M>)`, where `M` is the model's `calls`,
  unless the model declares its reply type, which then takes `reply`'s place.
- `entry = user(text) | assistant(reply) | result(call, text) | error(call, text) | unknown(call)`,
  then `notice(text)` in a program with a listing ([Listing notices and tool
  search](#listing-notices-and-tool-search)) and `attached(call, blob<N>)` in one
  whose tools return blobs ([Blobs](#blobs)).

Rules:

- There are no implicit conversions; both sides of an operator have one type.
  `==` and `!=` compare `u64`, `bool` or `text` only.
- `and` and `or` evaluate both sides.
- A variant constructor takes its type from where it is used (an assignment's
  field, a `push`'s list, a `return`, an argument). With nothing expected it
  takes the one sum that has the variant; `ok`, `failed` and `unknown` always
  need an expected type.
- Names are bound once per handler: no shadowing, and a local may not reuse
  a state field's, constant's, tool's or the model's name. A fn's body is a
  scope of its own, so its names may repeat its caller's.
- Locals (`let`, parameters, loop variables, pattern bindings) never change.
  Only state fields are assigned and pushed.

This snippet uses most of them:

```hl docs/snippets/values.hl
harness values

const calls_max: u64 = 4
const notes_max: u64 = 32
const width_max: u64 = 1000

type size = small | large(u64)

model think calls calls_max

state {
  notes: list<text, notes_max>
  history: list<entry, notes_max>
  seen: u64
  quiet: bool
  last: text
}

on measure(width: u64, label: text) -> size
  requires width <= width_max and seen < width_max and len(notes) < notes_max
{
  seen = seen + 1
  last = "measured " ++ label
  quiet = width == 0 or label == ""
  if width > 100 and not quiet {
    push notes label
    return large(width * 2)
  } else if width > 10 {
    return large(width - 10)
  }
  return small
}

on recount() -> u64 {
  for note in notes {
    if note == last { return len(notes) }
  }
  return 0
}

on tools_of(reply: reply) -> text {
  match reply {
    answer(words) => { return words }
    calls(requested) => {
      for call in requested {
        if call.tool == "bash" { return call.id ++ ": " ++ call.input }
      }
      return "no bash call"
    }
  }
}
```

The checker's type errors:

```console
$ hl check docs/snippets/errors/type-mismatch.hl
docs/snippets/errors/type-mismatch.hl:8:11: error: expected u64, found text
$ hl check docs/snippets/errors/compare-sum.hl
docs/snippets/errors/compare-sum.hl:5:58: error: '==' compares u64, bool or text, not light
$ hl check docs/snippets/errors/ambiguous-variant.hl
docs/snippets/errors/ambiguous-variant.hl:7:15: error: cannot tell which type 'done' builds here: it is a variant of outcome, signal
$ hl check docs/snippets/errors/call-field.hl
docs/snippets/errors/call-field.hl:7:43: error: call has no field 'name'
$ hl check docs/snippets/errors/assign-local.hl
docs/snippets/errors/assign-local.hl:5:3: error: cannot assign to 'count': it is a local binding, and only state fields change
$ hl check docs/snippets/errors/shadow.hl
docs/snippets/errors/shadow.hl:5:9: error: 'value' is already bound in this handler
$ hl check docs/snippets/errors/effect-name.hl
docs/snippets/errors/effect-name.hl:6:16: error: 'read' is an effect: call it with arguments
$ hl check docs/snippets/errors/unknown-name.hl
docs/snippets/errors/unknown-name.hl:4:10: error: unknown name 'inpt'
```

```hl docs/snippets/errors/type-mismatch.hl
harness notes

state {
  count: u64
}

on ping(name: text) -> u64 {
  count = name
  return count
}
```

```hl docs/snippets/errors/compare-sum.hl
harness notes

type light = red | green

on ping(left: light, right: light) -> bool { return left == right }
```

```hl docs/snippets/errors/ambiguous-variant.hl
harness notes

type outcome = done(text) | stopped
type signal = done(text) | waiting

on ping(words: text) -> u64 {
  let value = done(words)
  return 1
}
```

```hl docs/snippets/errors/call-field.hl
harness notes

const calls_max: u64 = 4

model think calls calls_max

on ping(call: call) -> text { return call.name }
```

```hl docs/snippets/errors/assign-local.hl
harness notes

on ping() -> u64 {
  let count = 1
  count = 2
  return count
}
```

```hl docs/snippets/errors/shadow.hl
harness notes

on ping(value: u64) -> u64 {
  if value > 1 {
    let value = 2
    return value
  }
  return value
}
```

```hl docs/snippets/errors/effect-name.hl
harness notes

tool read(path: text) -> text effect none

on ping() -> u64 {
  let reader = read
  return 1
}
```

```hl docs/snippets/errors/unknown-name.hl
harness notes

on echo(input: text) -> text {
  return inpt
}
```

### Text

Five builtins read text without an effect, so they replay like any
expression, and every backend implements them identically:

- `contains(t, s)` is whether `s` occurs in `t`.
- `before(t, s)` is `t` up to the first `s`, or all of `t` when `s` does not
  occur.
- `after(t, s)` is what follows the first `s`, or the empty text when `s` does
  not occur.
- `trim(t)` is `t` without leading and trailing ASCII whitespace (space, tab,
  newline, form feed, carriage return).
- `json_escape(t)` is `t` written as the inside of a JSON string: `"` and `\`
  take a backslash, newline, carriage return and tab become `\n`, `\r` and
  `\t`, and the other characters below U+0020 become `\u00XX`. A program that
  builds JSON by joining text, as `examples/entry.hl` builds the launch
  manifest from command-line flags, escapes every value it did not write.

None of them counts or indexes characters, so nothing depends on how a
platform measures text (bytes in Rust, UTF-16 units in JavaScript), and none
can fail. `examples/claude_code.hl` strips a compaction summary's `<analysis>`
block and rewrites its `<summary>` tag with them, as nanocode's
`compact_summary` does.

```hl docs/snippets/text.hl
harness pages

const turns_max: u64 = 1000000

type outcome = summary(text) | parts(text, text) | missing(text)

tool fetch(key: text) -> text effect none

state {
  turns: u64
}

invariant turns_bounded: turns <= turns_max

on summarize(key: text) -> outcome
  requires turns < turns_max
{
  turns = turns + 1
  match fetch(key) {
    failed(error) => { return missing(trim("  " ++ error ++ "\n")) }
    ok(page) => {
      if contains(page, "eta") {
        return parts(before(page, "eta"), after(page, "eta"))
      }
      let tagged = "<analysis>draft</analysis>\n<summary> " ++ page ++ " </summary>"
      return summary(trim(before(after(tagged, "<summary>"), "</summary>")) ++ after(page, "a-"))
    }
  }
}
```

```console
$ hl run docs/snippets/text.hl --handler summarize --arg notes --seed 3 | grep -o '"result":{"parts[^}]*}'
"result":{"parts":["b","-4"]}
$ hl run docs/snippets/text.hl --handler summarize --arg notes --seed 10 | grep -o '"result":{"summary[^}]*}'
"result":{"summary":"alpha-2424"}
$ hl check docs/snippets/errors/text-argument.hl
docs/snippets/errors/text-argument.hl:3:65: error: expected text, found u64
```

```hl docs/snippets/errors/text-argument.hl
harness notes

on ping(page: text, count: u64) -> bool { return contains(page, count) }
```

```hl docs/snippets/json_escape.hl
harness quoting

on wrap(path: text) -> text {
  let note = "say \"hi\" to C:\\tmp\n"
  return "{\"path\":\"" ++ json_escape(path) ++ "\",\"note\":\"" ++ json_escape(note) ++ "\"}"
}
```

```console
$ hl run docs/snippets/json_escape.hl --handler wrap --arg 'a "b"' | grep -o '"result":"[^}]*}'
"result":"{\"path\":\"a \\\"b\\\"\",\"note\":\"say \\\"hi\\\" to C:\\\\tmp\\n\"}
```

### Text enums

`type name = "value" | "value" | ...` declares a text enum: a type whose
values are the listed texts and nothing else, such as a subagent's completion
status or a hook's decision. It is a parameter, state field, tool result or
handler result type like any other, and its values are any texts, spaces
included.

- A text literal where a text enum is expected (an assignment's field, a
  `push`'s list, a `return`, an argument) must be one of its values, and is
  that value. Anywhere else a literal is `text`. There is no conversion
  between a text enum and `text`, and `==` does not compare text enums:
  `match` them.
- `match` takes one arm per value, written as the text. It is exhaustive like
  a sum's: a missing value, an unlisted one or a variant pattern is an error.
- A state field of a text enum starts at its first value.
- The journal holds the text itself: an argument in the `admitted` record, a
  field in a checkpoint and a tool's result are JSON strings.
- Every boundary checks the value and never falls back to another one. An
  argument, or a value read back from the journal, that is not one of the
  values is refused: `hl run` and the compiled module's argument decoding
  reject it, and a journal holding one is corrupt. A tool result that is not
  one of them is recorded as the call's failure, `TOOL returned JSON, which is
  not a value of TYPE`, so the program meets it in its `failed` arm. The same
  rule holds for every tool result: one that does not fit the declared type is
  recorded as failed.
- `hl sim` draws arguments from the values, every value as likely as the
  next, and tool results from them too, except
  `TEXT_ENUM_INVALID_PPM` = 100000 results per million that lie outside them
  and must come back as failures (the `enum_valid` invariant).

```hl docs/snippets/text_enums.hl
harness reviews

const approvals_max: u64 = 1000

type verdict = "approve" | "request changes" | "comment"
type outcome = merged(text) | blocked(text)

tool review(change: text) -> verdict effect idempotent

state {
  last: verdict
  approvals: u64
}

on submit(change: text) -> outcome
  requires approvals < approvals_max
{
  match review(change) {
    ok(said) => {
      last = said
      match said {
        "approve" => {
          approvals = approvals + 1
          return merged(change)
        }
        "request changes" => { return blocked("changes requested") }
        "comment" => { return blocked("no verdict yet") }
      }
    }
    failed(error) => { return blocked(error) }
  }
}

on record(said: verdict) -> verdict {
  last = said
  return "comment"
}
```

```console
$ hl run docs/snippets/text_enums.hl --handler record --arg "request changes"
{"crc":1525710037,"record":{"admitted":{"digest":"record","first":1,"hand":null,"metadata":{"handler":"record"},"prompt":{"said":"request changes"},"turn":"1"}}}
{"crc":2322276109,"record":{"status":{"status":{"finished":{"last":0,"outcome":"completed","result":"comment"}},"turn":"1"}}}
{"crc":2374166743,"record":{"checkpoint":{"payload":"{\"approvals\":0,\"last\":\"request changes\"}","revision":1,"state":"reviews"}}}
$ hl run docs/snippets/text_enums.hl --handler record --arg approved
docs/snippets/text_enums.hl: error: argument 'said': 'approved' is not a variant of verdict
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/text_enums.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=846 crashes=342 power_losses=40 torn_writes=47 takeovers=0 fenced=0 dispatches=359 unknown=0
```

The planted bug `enum_unchecked` skips the check and takes an unlisted result
as the enum's last value, the fallback an `else` branch writes by hand. The
simulator catches it in `examples/hooks.hl`, whose stop hook returns a
decision:

```console
$ hl sim --target native-jsonl --seeds 0..200 --mutation enum_unchecked examples/hooks.hl
FAIL invariant=enum_valid seed=0 target=native-jsonl profile=errors mutation=enum_unchecked: call 2/6 (pre_tool_use) records Ok(String("deny")), but the tool returned [Ok(String("alpha-16"))]: a result outside its text enum must be recorded as failed
replay: hl sim --target native-jsonl --seeds 0..1 --mutation enum_unchecked examples/hooks.hl
```

The checker's text enum errors:

```console
$ hl check examples/invalid/enum_missing_value.hl
examples/invalid/enum_missing_value.hl:6:3: error: match on status is missing "interrupted"
$ hl check examples/invalid/enum_unknown_value.hl
examples/invalid/enum_unknown_value.hl:10:10: error: "cancelled" is not a value of status
$ hl check docs/snippets/errors/text-enum-pattern.hl
docs/snippets/errors/text-enum-pattern.hl:7:5: error: status is a text enum: match its values as text, such as "completed"
$ hl check docs/snippets/errors/text-enum-compare.hl
docs/snippets/errors/text-enum-compare.hl:5:49: error: '==' compares u64, bool or text, not status
$ hl check docs/snippets/errors/text-enum-duplicate.hl
docs/snippets/errors/text-enum-duplicate.hl:3:40: error: value "completed" appears twice in 'status'
```

```hl docs/snippets/errors/text-enum-pattern.hl
harness notes

type status = "completed" | "failed"

on show(status: status) -> text {
  match status {
    completed => { return "done" }
    failed => { return "failed" }
  }
}
```

```hl docs/snippets/errors/text-enum-compare.hl
harness notes

type status = "completed" | "failed"

on show(status: status) -> bool { return status == "completed" }
```

```hl docs/snippets/errors/text-enum-duplicate.hl
harness notes

type status = "completed" | "failed" | "completed"

on ping() -> u64 { return 1 }
```

### Model replies

`model name calls bound reply type` declares the type of the model's replies,
so a harness can branch on everything its provider reports, such as Claude's
stop reasons. The type is a sum this harness declares, and:

- Exactly one variant carries `list<call, bound>`, the model's calls; the
  worst case multiplies that list's capacity like any `for` over it.
- Every other field is `u64`, `bool`, `text` or a text enum, so every
  target and the simulator can carry it.
- No variant is named `failed`, the name a failed model call is recorded
  under.
- It takes the builtin `reply`'s place: `entry`'s `assistant` variant
  carries it, and the name `reply` is free.

Calling the model returns `known<type>`: the model is idempotent, so its
outcome is never unknown, but the provider can fail. `failed(error)` is a
host reporting a failure, as the reply `{"failed": "error"}`, or a reply that
does not fit the type (`model reply: ...`, a list over the calls bound
included), so the program handles both, typically by asking again under a
named bound. Nothing traps. Matching the reply is exhaustive like any sum.

```hl docs/snippets/model_reply.hl
harness replies

const calls_max: u64 = 4
const rounds_max: u64 = 16
const attempts_max: u64 = 5
const continuations_max: u64 = 3
const context_max: u64 = 256

type outcome = done(text) | stopped(text) | exhausted
type turn_reply = answer(text, u64)
  | calls(list<call, calls_max>, u64)
  | truncated(text)
  | refused(text)

tool read(path: text) -> text effect none

model think calls calls_max reply turn_reply

state {
  context: list<entry, context_max>
  failures: u64
  continuations: u64
}

invariant failures_bounded: failures < attempts_max
invariant continuations_bounded: continuations <= continuations_max

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  failures = 0
  continuations = 0
  loop round below rounds_max {
    match think(context) {
      failed(error) => {
        if failures + 1 >= attempts_max { return stopped(error) }
        failures = failures + 1
      }
      ok(reply) => {
        failures = 0
        push context assistant(reply)
        match reply {
          answer(words, _) => { return done(words) }
          calls(requested, _) => {
            for call in requested {
              match dispatch(call) {
                ok(output) => { push context result(call, output) }
                failed(error) => { push context error(call, error) }
                unknown => { push context unknown(call) }
              }
            }
          }
          truncated(partial) => {
            if continuations >= continuations_max { return stopped(partial) }
            continuations = continuations + 1
          }
          refused(reason) => { return stopped(reason) }
        }
      }
    }
  }
  return exhausted
}
```

The prover bounds both counters. Each holds its invariant where the loop
starts and every iteration that falls through keeps it, so it holds at every
iteration's start ([Invariants and requires](#invariants-and-requires)). Each
retry and each continuation spends one of `rounds_max` rounds, so the worst
case is the same as without them:

```console
$ hl check docs/snippets/model_reply.hl | grep -E 'worst case|no_overflow'
  handler ask: worst case 80 effects, 147 journal records, 80 outbound requests
  proved handler ask: no_overflow: 7 arithmetic operation(s) proved in range; invariant_preserved: 10 invariant check(s) at returns and trap points; push_bound: 5 push(es) proved below capacity (2085 prover steps)
```

The journal is unchanged. A `model` record holds the reply as the host
returned it, and a failed call writes a `model` record too, holding
`{"failed": error}`, so a rerun replays the failure and takes the same path:

```console
$ hl run docs/snippets/model_reply.hl --handler ask --arg hi --seed 2 | sed -n 2p
{"crc":1388021480,"record":{"model":{"call_id":"1/1","response":{"failed":"think failed: epsilon-53"},"seq":1,"turn":"1"}}}
```

`hl sim` draws a failure in `MODEL_FAILURE_PPM` = 150000 calls per million to
a model with a declared reply, and otherwise a variant of the type: one other
than the calls' in `ANSWER_PPM` = 350000 replies per million, every one as
likely as the next, and the calls' variant in the rest, with fields drawn as
tool results are, values outside a text enum included.
`examples/claude_agent.hl` ports Claude's stop reasons this way: `max_tokens`
continues at most `continuations_max` times, `pause_turn` sends the context
again, `model_context_window_exceeded` compacts once
([clear and keep](#clear-and-keep)), `refusal` stops the
turn, and a failed call is asked again up to `throttled_attempts_max` times
in a row. `examples/codex_agent.hl` ports the Codex turn's `end_turn`
(openai/codex `core/src/session/turn.rs`): its reply is
`message(text, text, bool) | calls(list<call, calls_per_round_max>, text, text)`,
the words, the output items to send back (encrypted reasoning included) and
`end_turn`, and a message whose `end_turn` is false, or after which a steer
waits, sends the context again instead of finishing; its calls run with
`parallel`, listed MCP tools included. Each
continuation spends one of `rounds_max` rounds, so the prover bounds it like
any other round and the worst case counts it.

The planted bug `model_failure_unrecorded` writes no record for a failed
call, as a harness that treats provider errors as transient might. A rerun
asks the model again, and replaying the journal from nothing takes another
path, past the sequence numbers the journal records:

```console
$ hl sim --target native-jsonl --seeds 0..200 --mutation model_failure_unrecorded examples/claude_agent.hl
FAIL invariant=replay_equal seed=0 target=native-jsonl profile=errors mutation=model_failure_unrecorded: replaying the journal failed: Corrupt("replay of turn 4 needs seq 45, which the journal does not record")
replay: hl sim --target native-jsonl --seeds 0..1 --mutation model_failure_unrecorded examples/claude_agent.hl
```

The checker's reply errors:

```console
$ hl check examples/invalid/reply_two_call_lists.hl
examples/invalid/reply_two_call_lists.hl:7:73: error: reply type 'split_reply' carries list<call, calls_max> in 'reads' and 'writes': exactly one variant carries the model's calls
$ hl check examples/invalid/reply_no_call_list.hl
examples/invalid/reply_no_call_list.hl:6:6: error: reply type 'text_reply' has no variant carrying list<call, calls_max>: exactly one variant carries the model's calls
$ hl check examples/invalid/reply_unmatched_failure.hl
examples/invalid/reply_unmatched_failure.hl:20:5: error: 'answer' is not a variant of known<turn_reply>
$ hl check docs/snippets/errors/reply-failed-variant.hl
docs/snippets/errors/reply-failed-variant.hl:5:65: error: reply type 'turn_reply' has a variant named 'failed', the name a failed model call is recorded under
$ hl check docs/snippets/errors/reply-field.hl
docs/snippets/errors/reply-field.hl:6:71: error: variant 'notes' of reply type 'turn_reply' carries list<text, notes_max>: a reply's fields are u64, bool, text, text enums and the one list<call, calls_max>
$ hl check docs/snippets/errors/reply-builtin.hl
docs/snippets/errors/reply-builtin.hl:9:15: error: unknown type 'reply': the model replies with 'turn_reply'
```

```hl docs/snippets/errors/reply-failed-variant.hl
harness notes

const calls_max: u64 = 4

type turn_reply = answer(text) | calls(list<call, calls_max>) | failed(text)

model think calls calls_max reply turn_reply

on ping() -> u64 { return 1 }
```

```hl docs/snippets/errors/reply-field.hl
harness notes

const calls_max: u64 = 4
const notes_max: u64 = 8

type turn_reply = answer(text) | calls(list<call, calls_max>) | notes(list<text, notes_max>)

model think calls calls_max reply turn_reply

on ping() -> u64 { return 1 }
```

```hl docs/snippets/errors/reply-builtin.hl
harness notes

const calls_max: u64 = 4

type turn_reply = answer(text) | calls(list<call, calls_max>)

model think calls calls_max reply turn_reply

on show(said: reply) -> u64 { return 1 }
```

### Usage, stop reasons and model errors

`model name calls bound window tokens` declares the model's context window,
and with it every call to the model returns the builtin sum `attempt`:

```text
attempt = ok(reply, stop, u64, u64, u64, u64)
        | throttled(text, u64)
        | transient(text, u64)
        | rejected(text)
stop    = "end" | "tool_use" | "max_tokens" | "refusal" | "context_exceeded" | "pause"
```

`ok` carries the reply (the builtin `reply` or the declared reply type), why
the provider stopped, and the four usage counts the provider reported: input,
output, cache read and cache write tokens. A reply that stopped early keeps
what it said: `max_tokens` and `refusal` come with the partial answer or calls, so
the harness decides what to keep and whether to continue. `throttled` and
`transient` carry the provider's error and its `Retry-After` in milliseconds (0 when
it sent none); `rejected` is an error that asking again does not fix. The whole
attempt is the `model` record, so a rerun replays the same stop, usage and delay.
The names `attempt` and `stop` are types a harness can name, and `stop` is matched
exhaustively like any text enum.

Each usage count is at most the window. A reply that does not decode, a stop
outside the enum, or a usage count past the window becomes `rejected("model reply: ...")`
on every backend, so the prover may assume each count in `ok` is at most
`tokens`: summing the four, as nanocode's compaction threshold does, is proved not to
overflow.

`docs/snippets/recovery.hl` ports the recoveries of nanocode's Claude loop: it compacts
when the usage of the last reply plus an estimate of the tool results since
reaches the window less a reserve, continues after `max_tokens` at most three times,
asks once more after a refusal, compacts on `context_exceeded`, and backs off on
`throttled` and `transient` with doubling delays that honour `Retry-After`:

```hl docs/snippets/recovery.hl
harness recovery

const rounds_max: u64 = 16
const calls_max: u64 = 4
const context_max: u64 = 128
const kept_max: u64 = 8
const window_tokens: u64 = 200000
const compact_tokens: u64 = 167000
const result_tokens_max: u64 = 25000
const result_units_max: u64 = 100039
const continuations_max: u64 = 3
const throttled_max: u64 = 10
const retry_base_ms: u64 = 500
const retry_cap_ms: u64 = 32000

type outcome = done(text) | failed(text) | exhausted

tool read(path: text) -> text effect none
tool bash(command: text) -> text effect external

model think calls calls_max window window_tokens

state {
  context: list<entry, context_max>
  context_tokens: u64
  pending_units: u64
  continuations: u64
  throttled: u64
  backoff_ms: u64
  refused: bool
}

invariant tokens_bounded: context_tokens <= 4 * window_tokens
invariant continuations_bounded: continuations <= continuations_max
invariant throttled_bounded: throttled < throttled_max
invariant backoff_bounded: backoff_ms <= retry_cap_ms
invariant pending_bounded: pending_units <= calls_max * result_units_max

fn back_off(error: text, after_ms: u64) {
  if throttled + 1 >= throttled_max { return failed(error) }
  if after_ms > retry_cap_ms { return failed("Retry-After past the cap: " ++ error) }
  throttled = throttled + 1
  if after_ms > backoff_ms {
    let woke = wait(after_ms) at most retry_cap_ms
  } else {
    let woke = wait(backoff_ms) at most retry_cap_ms
  }
  if backoff_ms * 2 > retry_cap_ms { backoff_ms = retry_cap_ms } else { backoff_ms = backoff_ms * 2 }
}

fn compact() {
  match think(context with user("Summarize the conversation so far.")) {
    ok(summary, stop, _, _, _, _) => {
      match summary {
        answer(words) => {
          keep context last kept_max after user("Summary of the earlier conversation:\n" ++ words)
          context_tokens = 0
          pending_units = 0
        }
        calls(_) => { return failed("the summary asked for tools") }
      }
    }
    throttled(error, _) => { return failed("compaction throttled: " ++ error) }
    transient(error, _) => { return failed("compaction failed: " ++ error) }
    rejected(error) => { return failed("compaction rejected: " ++ error) }
  }
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (2 + calls_max) <= context_max
{
  push context user(question)
  continuations = 0
  throttled = 0
  backoff_ms = retry_base_ms
  refused = false
  loop round below rounds_max {
    if context_tokens * 4 + pending_units >= compact_tokens * 4 { compact() }
    match think(context) {
      throttled(error, after_ms) => { back_off(error, after_ms) }
      transient(error, after_ms) => { back_off(error, after_ms) }
      rejected(error) => { return failed(error) }
      ok(reply, stop, input_tokens, output_tokens, cache_read, cache_write) => {
        throttled = 0
        backoff_ms = retry_base_ms
        context_tokens = input_tokens + output_tokens + cache_read + cache_write
        pending_units = 0
        push context assistant(reply)
        match reply {
          answer(words) => {
            match stop {
              "end" => { return done(words) }
              "tool_use" => { return done(words) }
              "max_tokens" => {
                if continuations >= continuations_max { return failed("output token limit after every continuation") }
                continuations = continuations + 1
                push context user("Output token limit hit. Resume directly.")
              }
              "refusal" => {
                if refused { return failed("refused twice: " ++ words) }
                refused = true
                push context user("Continue.")
              }
              "context_exceeded" => { compact() }
              "pause" => {}
            }
          }
          calls(requested) => {
            for call in requested {
              match dispatch(call) {
                ok(output) => {
                  let cut = truncate_middle(output, result_tokens_max)
                  pending_units = pending_units + len(cut)
                  push context result(call, cut)
                }
                failed(error) => { push context error(call, truncate_middle(error, result_tokens_max)) }
                unknown => { push context unknown(call) }
              }
            }
          }
        }
      }
    }
  }
  return exhausted
}
```

```console
$ hl check docs/snippets/recovery.hl | grep -E 'handler ask: worst|milliseconds of waits:|proved for'
  handler ask: worst case 96 effects, 163 journal records, 96 outbound requests
    milliseconds of waits: 512000, round below rounds_max (16)
docs/snippets/recovery.hl: proved for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
$ hl run docs/snippets/recovery.hl --handler ask --arg hi --seed 41 | sed -n 2,3p
{"crc":3390402489,"record":{"model":{"call_id":"1/1","response":{"throttled":["model failed: gamma-34",0]},"seq":1,"turn":"1"}}}
{"crc":1242477268,"record":{"call":{"call_id":"1/2","effect":"none","input":{"ms":500,"until":1115},"seq":2,"tool":"wait","turn":"1"}}}
```

`hl sim` draws, for a model with a window, a throttled attempt in
`MODEL_THROTTLED_PPM` = 100000 calls per million, a transient one in
`MODEL_TRANSIENT_PPM` = 50000 and a rejected one in `MODEL_REJECTED_PPM` = 20000, with
a `Retry-After` absent half the time and otherwise up to `RETRY_AFTER_MS_MAX` = 120000;
a reply stops early (`max_tokens`, `refusal`, `context_exceeded` or `pause`) in
`STOP_FAULT_PPM` = 60000, and its usage counts reach the window or pass it by one in
`USAGE_EDGE_PPM` = 10000 each:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/recovery.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=1351 crashes=847 power_losses=80 torn_writes=104 takeovers=0 fenced=0 dispatches=416 unknown=122 sleeps=63 suspensions=0
```

Two planted bugs show the journal is what makes these replays exact.
`usage_unchecked` accepts a usage count past the window, and
`retry_after_unjournaled` journals an attempt without its `Retry-After`, so a rerun
would wait a different time than the run it replays; the simulator finds a
journaled reply that no request returned:

```console
$ hl sim --target native-jsonl --seeds 0..200 --twice --mutation usage_unchecked docs/snippets/recovery.hl | head -n 1
FAIL invariant=replay_equal seed=3 target=native-jsonl profile=clean mutation=usage_unchecked: replaying the journal failed: Corrupt("replay of turn 1 finished {\"failed\":\"model reply: output usage of 200001 tokens exceeds the window of 200000 tokens\"} where the journal says {\"failed\":\"compaction rejected: model reply: 'eta-26' is not a variant of stop\"}")
$ hl sim --target native-jsonl --seeds 0..200 --twice --mutation retry_after_unjournaled docs/snippets/recovery.hl | head -n 1
FAIL invariant=mode_sent seed=4 target=native-jsonl profile=async mutation=retry_after_unjournaled: model call 1/7 is journaled with a reply no request returned
```

`examples/claude_code.hl` holds the same recoveries with nanocode's constants and
prompts: a 200,000-token window less a 33,000-token reserve, tool results cut
at 12,000 tokens for `exec` and `wait` and 25,000 for the rest, three continuations,
one refusal retry, and backoff from 500 ms doubling to 32 s, honouring
`Retry-After` up to 60 s, with 10 throttled attempts within 180 s and 11 for other
transient errors.

### Text length and cuts

`len(t)` of a text is its length in UTF-16 code units, the unit JavaScript strings
count and nanocode's token estimate divides by four. `truncate_middle(t, n)`,
with `n` a named constant, keeps a text whose estimate (its length divided by four,
rounded up) is at most `n` tokens, and otherwise keeps its first and last `2n` code
units, whole characters only, joined by `…K tokens truncated…` where `K` is how many
tokens it dropped: nanocode's `truncate_middle`, byte for byte on every backend. A cut
text is at most `4n + 39` code units long (the marker is one ellipsis, at most 20
digits and ` tokens truncated…`), and the prover knows it: a local bound to a cut,
and `len` of one, carries that bound, so `recovery.hl` proves its estimate of
pending tool results fits `calls_max * result_units_max` with `result_units_max` =
4 × 25000 + 39, and one less is refused:

```console
$ hl check docs/snippets/errors/cut-bound.hl 2>&1 | grep invariant_preserved
docs/snippets/errors/cut-bound.hl: error: handler 'ask': invariant_preserved: invariant 'cut_bounded' (cut_units <= cut_units_max) may not hold when the handler returns; path: entry; counterexample: cut_units = 79, len(cut) = 79
```

```hl docs/snippets/errors/cut-bound.hl
harness cuts

const tokens_max: u64 = 10
const cut_units_max: u64 = 78

state {
  cut_units: u64
}

invariant cut_bounded: cut_units <= cut_units_max

on ask(said: text) -> u64 {
  let cut = truncate_middle(said, tokens_max)
  cut_units = len(cut)
  return cut_units
}
```

The simulator draws tool results long enough to be cut, with characters of
one, two and four bytes and surrogate pairs, in `LONG_TEXT_PPM` = 20000 results per
million of a program that cuts. The planted bug `truncate_uncut` leaves a text uncut,
and the checkpoint it writes no longer matches the journal's replay:

```console
$ hl sim --target native-jsonl --seeds 0..200 --twice --mutation truncate_uncut docs/snippets/recovery.hl | head -n 1
FAIL invariant=checkpoint_compacted seed=39 target=native-jsonl profile=errors mutation=truncate_uncut: checkpoint 1 holds 4 entries of context where replaying the journal to turn 1 gives 4 entries of context: a checkpoint must hold the lists as clear and keep left them
```

### Blobs

`blob<N>` is an opaque value of at most `N` bytes, `N` a named constant: an
image a tool read, a screenshot, a structured result the model sees as an
attachment. A program cannot look inside a blob, compare it or build one; it
can receive one from a tool, keep it in a list or a variant, and give it to
the model.

- `tool T(...) -> blob<N> effect C` returns a blob. `tool T(...) -> text or
  blob<N> effect C` returns either, as nanocode's `Read` returns text for a
  text file and an image block for an image; such a tool is called only
  through `dispatch`. Every blob a tool returns has the same bound.
- In such a program `dispatch(call)` returns the builtin sum `dispatched =
  ok(text) | failed(text) | unknown | blob(blob<N>)` in place of
  `maybe<text>`, so a `match` on it must take the blob, and the builtin
  entry `attached(call, blob<N>)` puts it in the context. A tool, a handler
  parameter or a code cell's tool cannot take a blob.
- A host returns a blob as `{"data": BASE64}`. Data that is not base64, or
  that decodes to more than `N` bytes, is recorded as the call's failure. The
  result record holds `{"blob": SHA256, "data": BASE64}` the first time the
  session journals those bytes and `{"blob": SHA256}` after, and a blob is
  `{"blob": SHA256}` everywhere else: in the state, in checkpoints and in the
  context a model request carries, with the session's bytes beside it. A
  load reads every journaled blob, refuses data whose digest differs, and
  refuses a state that holds a blob no record holds.
- `keep L blobs last N`, `L` a `list<entry, _>` and `N` a named constant,
  keeps at most the newest `N` attached blobs of `L` and turns each older one
  into `result(call, "[image omitted from this request, which keeps only the
  newest N images: sha256:DIGEST]")`, nanocode's image window: it evicts in
  batches of half of `N`, rounded up, so the cached prefix of the requests
  that follow changes once a batch rather than at every new image. A list of
  blobs, `list<blob<N>, M>`, is bounded by `keep L last n` like any list.
- The worst case counts the bytes of blobs a handler may journal, and a
  target whose journal stores a record per row checks that a blob's record
  fits one: Cloudflare's 2,000,000-byte row, SQLite's default string length
  and Postgres's 1 GB field.

```hl docs/snippets/images.hl
harness viewer

const calls_max: u64 = 4
const rounds_max: u64 = 8
const context_max: u64 = 64
const image_bytes_max: u64 = 1048576
const images_kept: u64 = 4
const shots_max: u64 = 2

type outcome = done(text) | exhausted

tool screenshot(url: text) -> blob<image_bytes_max> effect idempotent
tool read(path: text) -> text or blob<image_bytes_max> effect none

model think calls calls_max

state {
  context: list<entry, context_max>
  shots: list<blob<image_bytes_max>, shots_max>
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  loop round below rounds_max {
    keep context blobs last images_kept
    let reply = think(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return done(words) }
      calls(requested) => {
        for call in requested {
          match dispatch(call) {
            ok(output) => { push context result(call, output) }
            blob(image) => {
              push context attached(call, image)
              keep shots last shots_max - 1
              push shots image
            }
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

```console
$ hl check docs/snippets/images.hl | grep -E 'worst case|blob bytes'
  handler ask: worst case 40 effects, 75 journal records, 40 outbound requests
    blob bytes journaled: 33554432, round below rounds_max (8) x call below calls_max (4)
$ hl run docs/snippets/images.hl --handler ask --arg hi --seed 10 | grep -o '"ok":{"blob":"[0-9a-f]*"[^}]*}' | head -n 2
"ok":{"blob":"6351825030155836664876686dcff61b6a91254a0458537072a1b26ad151b5ce","data":"YWw="}
"ok":{"blob":"3f79bb7b435b05321651daefd374cdc681dc06faa65e374e38337b88ca046dea","data":"ZQ=="}
$ hl run docs/snippets/images.hl --handler ask --arg hi --seed 10 | grep -o '"ok":{"blob":"[0-9a-f]*"}}' | head -n 1
"ok":{"blob":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}}
$ hl run docs/snippets/images.hl --handler ask --arg hi --seed 10 | grep -o 'image omitted[^]]*' | head -n 1
image omitted from this request, which keeps only the newest 4 images: sha256:6351825030155836664876686dcff61b6a91254a0458537072a1b26ad151b5ce
$ hl check --target cloudflare examples/invalid/blob_row.hl 2>&1 > /dev/null | tail -n 1
examples/invalid/blob_row.hl: error: tool 'photo': a result record of blob<photo_bytes_max> (2097152 bytes) holds up to 2796399 bytes, over cloudflare's limit of 2000000 bytes per journal row
$ hl check docs/snippets/errors/blob-direct.hl
docs/snippets/errors/blob-direct.hl:11:9: error: tool 'read' returns text or a blob: call it through dispatch, whose blob arm takes the blob
```

```hl docs/snippets/errors/blob-direct.hl
harness notes

const calls_max: u64 = 4
const image_bytes_max: u64 = 1048576

tool read(path: text) -> text or blob<image_bytes_max> effect none

model think calls calls_max

on peek(path: text) -> u64 {
  match read(path) {
    ok(_) => { return 1 }
    failed(_) => { return 0 }
  }
}
```

The simulator answers a blob tool with short repeating byte strings, so the
same bytes come back often and later records hold only the digest, with whole
blobs at the bound in `BLOB_WHOLE_PPM` = 10000, blobs one byte over it in
`BLOB_OVER_PPM` = 20000 and data that is not base64 in `BLOB_INVALID_PPM` =
50000 cases per million; a `text or blob` tool answers a blob in
`BLOB_ATTACHED_PPM` = 250000. `blob_journaled` requires that every journaled
blob's data hashes to its digest, that a record without data follows one with
it, and that every checkpoint's blobs are held. `blob_unjournaled` journals
only digests, and `blobs_unevicted` keeps every blob in the live run, which
replay evicts:

```console
$ hl sim --target cloudflare --seeds 0..200 --twice --mutation blob_unjournaled docs/snippets/images.hl | head -n 1
FAIL invariant=blob_journaled seed=0 target=cloudflare profile=errors mutation=blob_unjournaled: result 1/3 (screenshot): blob 4cdfd556037e71f444cec02bf05421e860141c49d5701afdd109d29ab9c8ed29 is journaled without its data, and no earlier record holds it
$ hl sim --target vercel --seeds 0..200 --twice --mutation blobs_unevicted docs/snippets/images.hl | head -n 1
FAIL invariant=checkpoint_compacted seed=0 target=vercel profile=errors mutation=blobs_unevicted: checkpoint 4 holds 29 entries of context where replaying the journal to turn 4 gives 29 entries of context: a checkpoint must hold the lists as clear and keep left them
```

## Statements

| Statement | Meaning |
| --- | --- |
| `let x = e` | bind a new local |
| `let x: T = e` | bind a new local of type `T`; `e` is checked against `T`, so a variant constructor takes its type from it |
| `field = e` | assign a state field |
| `push field e` | append to a state list; a full list traps with `bound(field)` |
| `clear field` | empty a state list |
| `keep field last n` | drop all but the last `n` entries of a state list, `n` a `u64`; a list of `n` entries or fewer is unchanged ([clear and keep](#clear-and-keep)) |
| `keep field last n after e` | keep the last `n` entries and put `e` before them; traps with `bound(field)` when that leaves more than the capacity ([clear and keep](#clear-and-keep)) |
| `if c { } else if c { } else { }` | `c` is `bool` |
| `match e { variant(a, _) => { } ... }` | one arm per variant, no more, no fewer; arms may be separated by commas; a text enum's arms are its values, `"value" => { }` |
| `loop i below N { }` | runs the body with `i` = 0 to N - 1, `N` a constant; there is no `break` |
| `for x in list { }` | runs the body once per element of the list as it was when the loop started |
| `for call in list parallel { }` | dispatches every call of the list at once, then runs the arms in list order; see [Parallel tool calls](#parallel-tool-calls) |
| `return e` | finish the turn with `e`, of the handler's result type |
| `f(a, ...)` | run [fn](#fn) `f`'s body here |

There is no `while` and no recursion, and a fn is inlined where it is
called: a handler is straight-line code over bounded loops, so its worst
case is a number.

```console
$ hl check docs/snippets/errors/missing-variant.hl
docs/snippets/errors/missing-variant.hl:7:3: error: match on outcome is missing 'exhausted'
$ hl check examples/invalid/literal_bound.hl
examples/invalid/literal_bound.hl:13:19: error: the bound of loop 'step' must be a named constant, found number 64
$ hl check examples/invalid/unbounded_list.hl
examples/invalid/unbounded_list.hl:8:22: error: a list needs a capacity: write list<T, constant>
```

```hl docs/snippets/errors/missing-variant.hl
harness notes

type outcome = completed(text) | failed(text) | exhausted

on finish(done: bool, words: text) -> outcome {
  let result = completed(words)
  match result {
    completed(words_out) => { return result }
    failed(error) => { return result }
  }
}
```

### clear and keep

A list only grows through `push`; `clear` and `keep` shrink it. `clear
field` empties a state list, and `keep field last n` drops all but its last
`n` entries. With them a harness compacts its history instead of refusing
turns once the history is full: it asks the model for a summary, keeps the
most recent entries verbatim and pushes the summary after them, as nanocode's
compaction keeps its most recent messages (`COMPACT_KEPT`):

```hl docs/snippets/compaction.hl
harness compaction

const rounds_max: u64 = 4
const calls_max: u64 = 2
const history_max: u64 = 24
const kept_max: u64 = 8

type outcome = completed(text) | failed(text) | exhausted

tool read(path: text) -> text effect none
tool bash(command: text) -> text effect external

model infer calls calls_max

state {
  history: list<entry, history_max>
}

on prompt(input: text) -> outcome {
  if len(history) + 1 + rounds_max * (1 + calls_max) > history_max {
    keep history last history_max - 1
    push history user("Summarize the conversation so far.")
    let summary = infer(history)
    keep history last kept_max
    match summary {
      answer(_) => { push history assistant(summary) }
      calls(_) => { return failed("the summary asked for tools") }
    }
  }
  push history user(input)
  loop round below rounds_max {
    let reply = infer(history)
    push history assistant(reply)
    match reply {
      answer(words) => { return completed(words) }
      calls(requested) => {
        for call in requested {
          match dispatch(call) {
            ok(output) => { push history result(call, output) }
            failed(error) => { push history error(call, error) }
            unknown => { push history unknown(call) }
          }
        }
      }
    }
  }
  return exhausted
}

on forget() -> u64 {
  clear history
  return 0
}
```

`prompt` compacts when the history lacks room for a whole turn: one prompt
and `rounds_max` replies of up to `calls_max` calls. The first `keep` makes
room for the request should the history be full; the model's answer to it
is the summary, and a reply asking for tools instead fails the turn. The
summary comes from the same model as every other reply: compaction is an
ordinary branch, with an ordinary effect site and `model` record.

The prover sets a list's length to 0 after `clear`, and after `keep` to
`min(len, n)`, the join of the list already being that short (unchanged)
and the list being longer (now `n`). So the pushes that follow a
compaction are proved against `kept_max`, not against what the turn started
with, and no `requires` reserves room for the turn. `hl check` reports the
length each one leaves and the most it can drop:

```console
$ hl check docs/snippets/compaction.hl | grep -E 'worst case|shrink'
  handler prompt: worst case 13 effects, 24 journal records, 13 outbound requests
  handler forget: worst case 0 effects, 3 journal records, 0 outbound requests
  proved handler prompt: shrink: keep history last history_max - 1 leaves at most 23 of 24 entries and drops at most 1; keep history last kept_max leaves at most 8 of 24 entries and drops at most 16
  proved handler forget: shrink: clear history leaves at most 0 of 24 entries and drops at most 24
```

`clear` and `keep` are not effects: they write no record and add nothing to
the worst case, which counts the summary as one more model call. What they
leave is in the checkpoint written with the turn's status, so the load that
follows starts from the compacted list and never replays the entries they
dropped. `hl sim` checks both halves of that as `checkpoint_compacted`:
every checkpoint holds the state that replaying the journal reaches at its
turn, and a turn that dropped entries flushes its checkpoint together with
its status. Its planted bugs are nanocode-sim's: `retire_skipped` writes the
dropped entries back into the checkpoint, and `seal_skipped` flushes a
compacting turn's status without its checkpoint:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/compaction.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=1278 crashes=774 power_losses=74 torn_writes=99 takeovers=0 fenced=0 dispatches=508 unknown=140
$ hl sim --target native-jsonl --seeds 0..200 --mutation retire_skipped docs/snippets/compaction.hl
FAIL invariant=checkpoint_compacted seed=1 target=native-jsonl profile=errors mutation=retire_skipped: checkpoint 2 holds 2 entries of history where replaying the journal to turn 2 gives 0 entries of history: a checkpoint must hold the lists as clear and keep left them
replay: hl sim --target native-jsonl --seeds 1..2 --mutation retire_skipped docs/snippets/compaction.hl
$ hl sim --target native-jsonl --seeds 0..200 --mutation seal_skipped docs/snippets/compaction.hl
FAIL invariant=checkpoint_compacted seed=1 target=native-jsonl profile=errors mutation=seal_skipped: turn 2 retired 2 list entries but flushed its status without its checkpoint, so the next load replays the retired prefix from an earlier checkpoint
replay: hl sim --target native-jsonl --seeds 1..2 --mutation seal_skipped docs/snippets/compaction.hl
```

Only a state list can be cleared or kept short, `n` is a `u64`, and a
`keep` that leaves a full list leaves no room for the push after it:

```console
$ hl check docs/snippets/errors/clear-field.hl
docs/snippets/errors/clear-field.hl:8:9: error: cannot clear 'turns': it is not a list
$ hl check docs/snippets/errors/keep-count.hl
docs/snippets/errors/keep-count.hl:10:19: error: expected u64, found text
$ hl check docs/snippets/errors/keep-room.hl > /dev/null
docs/snippets/errors/keep-room.hl: error: handler 'note': push_bound: push onto lines may find it full and trap with bound(lines); path: entry; counterexample: len(lines) = 8
docs/snippets/errors/keep-room.hl: error: 1 proof obligation(s) failed for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
```

```hl docs/snippets/errors/clear-field.hl
harness notes

state {
  turns: u64
}

on reset() -> u64 {
  clear turns
  return turns
}
```

```hl docs/snippets/errors/keep-count.hl
harness notes

const lines_max: u64 = 8

state {
  lines: list<text, lines_max>
}

on trim(count: text) -> u64 {
  keep lines last count
  return len(lines)
}
```

```hl docs/snippets/errors/keep-room.hl
harness notes

const lines_max: u64 = 8

state {
  lines: list<text, lines_max>
}

on note(line: text) -> u64 {
  keep lines last lines_max
  push lines line
  return len(lines)
}
```

`examples/claude_agent.hl`, `subagents.hl` and
`subagent_child.hl` compact this way before every model request, keeping
`compact_kept` entries. `codex_agent.hl` compacts as Codex's local compaction
does (`core/src/compact.rs`): when the last usage plus the results since
reach nine tenths of the window, or when the provider reports the window
exceeded, it asks for a handoff summary, drops every entry but the user's
messages with a [rewrite](#rewrite) and pushes the summary after them. `claude_agent.hl` writes compaction once as a
[fn](#fn) and also calls it when the model reports the context window
exhausted, and `subagents.hl` also keeps the `agents_kept` most recent agents
of its roster when a child completes.


`keep field last n after e` puts `e` before the last `n` entries, so a
summary replaces what it summarizes instead of following what was kept, and
`context with e`, as the argument of a model call, sends the context with
one more entry the list itself never holds. Together they are summarizing
compaction: the model reads the history and the request to summarize it, and
its answer becomes the history's first entry, ahead of the entries kept
verbatim, as nanocode's compaction keeps the messages after its summary.
`docs/snippets/recovery.hl` compacts so ([Usage, stop reasons and model
errors](#usage-stop-reasons-and-model-errors)):

```text
match think(context with user("Summarize the conversation so far.")) {
  ok(summary, stop, _, _, _, _) => {
    match summary {
      answer(words) => {
        keep context last kept_max after user("Summary of the earlier conversation:\n" ++ words)
```

The summary is an ordinary model call, with every outcome of one: a
throttled or refused summary is the harness's to retry or fail. The prover
treats `keep after` as a `keep` followed by a push, so its length is at most
`min(len, n) + 1` and the push is a `push_bound` obligation; the runtime
traps with `bound(field)` where it would not fit. `with` adds no record and
changes no length, and anywhere but a model call's argument it is an error:

```console
$ hl check examples/invalid/with_outside_model.hl
examples/invalid/with_outside_model.hl:13:23: error: 'with' adds an entry to the context a model call reads: it is only a model call's argument
```

### rewrite

`rewrite field { pattern => entry, pattern => drop, ... }` maps the entries a state
list already holds, in place: each entry whose variant an arm names becomes
that arm's entry, or leaves the list when the arm says `drop`, and an entry no
arm names stays as it is. The list is a list of a sum, such as a context of
`entry`; a pattern names one variant and binds its fields, as a `match` arm does;
each variant is named at most once; and an arm's entry is an expression of the
list's element type that runs no effect, so a rewrite is a pure, total map
over the list. It repairs a history a provider refused, as nanocode's Claude
loop drops the blocks the provider rejected and retries once:

```hl docs/snippets/rewrite.hl
harness repair

const rounds_max: u64 = 3
const calls_max: u64 = 4
const context_max: u64 = 64
const result_tokens_max: u64 = 16

type outcome = answered(text) | stopped(text)
type turn_reply = answer(text) | calls(list<call, calls_max>)

tool read(path: text) -> text effect none
tool bash(command: text) -> text effect external

model think calls calls_max reply turn_reply

state {
  context: list<entry, context_max>
  repaired: bool
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  repaired = false
  loop round below rounds_max {
    match think(context) {
      failed(error) => {
        if repaired { return stopped(error) }
        repaired = true
        rewrite context {
          result(call, output) => result(call, truncate_middle(output, result_tokens_max))
          unknown(_) => drop
        }
      }
      ok(reply) => {
        push context assistant(reply)
        match reply {
          answer(words) => { return answered(words) }
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
    }
  }
  return stopped("the turn ran out of rounds")
}
```

A rewrite is not an effect: it writes no record and adds nothing to the worst
case, and what it leaves is in the checkpoint written with the turn's status,
as for `clear` and `keep`. The prover leaves the list's length unchanged when no
arm drops, and otherwise sets it to at most what it was, so pushes that follow
are proved against what the rewrite can leave; it checks every arm's entry as
it checks a pushed value, and the cuts of `truncate_middle` in an arm count among
the program's cuts. `hl sim` checks it with `checkpoint_compacted`: every
checkpoint holds the state that replaying the journal reaches, and
`rewrite_live_only` rewrites only when the turn runs past its journal, not when a
rerun replays it:

```console
$ hl check docs/snippets/rewrite.hl | grep shrink
  proved handler ask: shrink: rewrite context leaves at most 59 of 64 entries and drops at most 59
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/rewrite.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=1736 crashes=1232 power_losses=98 torn_writes=150 takeovers=0 fenced=0 dispatches=1210 unknown=320
$ hl sim --target native-jsonl --seeds 0..200 --twice --mutation rewrite_live_only docs/snippets/rewrite.hl | head -n 1
FAIL invariant=checkpoint_compacted seed=4 target=native-jsonl profile=async mutation=rewrite_live_only: checkpoint 2 holds 10 entries of context where replaying the journal to turn 2 gives 9 entries of context: a checkpoint must hold the lists as clear and keep left them
$ hl check docs/snippets/errors/rewrite-effect.hl
docs/snippets/errors/rewrite-effect.hl:16:20: error: a rewrite arm runs no effect: it only maps an entry to another
$ hl check docs/snippets/errors/rewrite-list.hl
docs/snippets/errors/rewrite-list.hl:10:3: error: 'rewrite lines' maps the entries of a list of a sum, found a list of text
```

```hl docs/snippets/errors/rewrite-effect.hl
harness notes

const calls_max: u64 = 4
const context_max: u64 = 8

tool summarize(text: text) -> text effect none

model think calls calls_max

state {
  context: list<entry, context_max>
}

on shorten() -> u64 {
  rewrite context {
    user(words) => user(summarize(words))
  }
  return len(context)
}
```

```hl docs/snippets/errors/rewrite-list.hl
harness notes

const lines_max: u64 = 8

state {
  lines: list<text, lines_max>
}

on shorten() -> u64 {
  rewrite lines {
    line(words) => drop
  }
  return len(lines)
}
```

`examples/claude_code.hl` repairs its history so when the provider rejects a
request over the tool changes appended to it (an error naming
`tool_addition`, `tool_removal` or `tool_reference`): once per turn it drops the
listing notices from the context and asks again, as nanocode strips the
appended tool-change blocks and sends the current tool list instead.

## Effects

An effect is anything whose result the program cannot recompute: a tool, the
model, `now()` and `random()`. Each effect site is journaled. When a handler
runs again after a crash, each site takes the next sequence number in its turn
and, if the journal holds its result, returns that result without running.
Effects inside an expression run before the expression, left to right.

| Class | Declared by | Result | When a crash lost its result |
| --- | --- | --- | --- |
| `none` | `effect none`, `now()`, `random()` | `known<T>` for a tool; `now()` and `random()` give a plain `u64` | runs again |
| `idempotent` | `effect idempotent`, the model | `known<T>` for a tool; the model gives a plain `reply`, or `known<R>` when it declares its reply type `R` | runs again with the same turn and sequence number, its idempotency key |
| `external` | `effect external`, a tool the program does not declare | `maybe<T>` | never runs again: the result is `unknown` |

These are nanocode's `EffectClass`. `dispatch(call)` looks `call.tool` up among
the declared tools and the listed families ([tools from
listing](#tools-from-listing)) at run time, defaults an undeclared tool to
`external`, and always has type `maybe<text>`; a none or idempotent tool reached through it
never yields `unknown`. Calling a tool by name gives its own class's result
type, so only an external tool needs an `unknown` arm:

```hl docs/snippets/effects.hl
harness effects

const tries_max: u64 = 3
const lines_max: u64 = 64

type report = posted(text, u64) | refused(text) | lost(u64)

tool read(path: text) -> text effect none
tool label(change: text, name: text) -> bool effect idempotent
tool comment(change: text, body: text) -> text effect external

state {
  lines: list<text, lines_max>
  posts: u64
}

invariant posts_bounded: posts <= tries_max

on post(change: text, path: text) -> report
  requires len(lines) + 2 <= lines_max and posts == 0
{
  let started = now()
  match read(path) {
    ok(body) => { push lines body }
    failed(error) => { return refused(error) }
  }
  match label(change, "posted") {
    ok(applied) => {
      if not applied { push lines "label was already there" }
    }
    failed(error) => { push lines "label failed: " ++ error }
  }
  match comment(change, "see " ++ path) {
    ok(link) => {
      posts = posts + 1
      return posted(link, started)
    }
    failed(error) => { return refused(error) }
    unknown => { return lost(started) }
  }
}
```

```console
$ hl check docs/snippets/errors/known-unknown.hl
docs/snippets/errors/known-unknown.hl:9:5: error: 'unknown' is not a variant of known<text>
$ hl check examples/invalid/unchecked_external.hl
examples/invalid/unchecked_external.hl:10:3: error: match on maybe<text> is missing 'unknown': an external effect whose result was not recorded has an unknown outcome
```

```hl docs/snippets/errors/known-unknown.hl
harness notes

tool read(path: text) -> text effect none

on show(path: text) -> text {
  match read(path) {
    ok(body) => { return body }
    failed(error) => { return error }
    unknown => { return "lost" }
  }
}
```

### Durability points

The compiler places the flushes; a program cannot move them.

1. Before an `external` effect runs, every buffered record is durable,
   including its own `call` record.
2. Before a turn's outcome is acknowledged, its `status` record (and the
   checkpoint written with it) is durable.
3. Nothing else forces a flush. Results of `none` and `idempotent` effects
   and model replies ride the next flush; a crash that loses them only
   repeats work that is safe to repeat.

### What each target guarantees

The same three classes, per target. A target may flush more often than the
points above, never less.

| | `native-jsonl` | `native-sqlite` | `server-postgres` | `cloudflare` | `vercel` |
| --- | --- | --- | --- | --- | --- |
| A flush is | one `write` and one `fsync` of every buffered line | one transaction | one transaction that first checks the session's owner epoch | no flush: the object's writes in one event commit together | the end of a Workflow step, which returns its records for the run to record |
| `none`, `idempotent`, model | buffered; rerun if the crash lost them | same | same | same | same |
| Before an `external` runs | its line and every line before it are written and synced | the transaction with its `call` row commits | the fenced transaction commits, or the run stops if another owner took over | the `fetch` is issued after the insert, and the output gate holds it until the insert commits | its `call` is committed by an earlier step; the step that runs it reads its attempt number |
| An `external` left open | at load, a `recovery` record reports it `unknown` before the handler reruns | same | same, by the new owner; the old owner's writes are refused | same, when the object restarts | in its step: an attempt after the first returns `unknown` and writes a `recovery` record |
| Before acknowledging | `status` and checkpoint written and synced | committed | committed, fenced | committed before the response leaves | committed at the step's end |
| What a crash can do to the journal | lose the unsynced tail or tear the last line: each line's CRC-32 drops a torn tail, and the file is synced on open before it is trusted | nothing partial | nothing partial; a second owner may start after a lease expires | nothing partial; a reset object resumes from its alarm | a step retried after it ran |

### Parallel tool calls

`for call in requested parallel { ... }` runs a reply's tool calls at once, as
nanocode runs the calls of a reply that its tools allow in parallel. Its body
is one `match dispatch(call)` on the item it walks, whose arms are each
call's lane: they may run effects of their own, but neither return, dispatch
again nor read events. A turn runs it in three steps:

1. In list order, each call takes the next sequence number and appends its
   `call` record (a call the journal already holds is replayed). Nothing is
   dispatched yet.
2. The calls are dispatched in groups, in list order. Consecutive none and
   idempotent calls form one group, dispatched at once; an external call is
   a group of its own, as nanocode runs writers exclusively. Before the first
   external call runs, one flush makes every `call` record of the batch
   durable (durability point 1, once for the whole batch). Each group's
   results are appended in list order, whatever order they completed in.
3. The arms run in list order with each call's result, and each arm's effects
   take the sequence numbers after the batch's, one call after another.

So the journal holds, per sequence number, exactly what the plain `for`
writes: the same `call` and `result` records, the calls of a batch
ahead of its results. A crash in the middle of a batch leaves each call as a
crash leaves a plain one: a none or idempotent call runs again, and an
external call whose record is durable is reported `unknown`, even one the
batch had not dispatched yet. A push that finds its list full traps after
every call of the batch ran, where the plain `for` would trap after the
calls before it.

Each target dispatches a group with its platform's own concurrency, at most
as many calls at once as its model allows: native Rust starts one thread per
call with `std::thread::scope` when the host offers `ConcurrentTools` (and
calls them one by one otherwise), bounded by the list's capacity; Cloudflare
runs at most `CLOUDFLARE_CONNECTIONS_WAITING_MAX` fetches at once inside the
object's event, so none waits in the runtime's own queue; Vercel runs a group
inside one step, which documents no concurrency limit. The simulator
completes a group's calls in a random order and crashes between them.

Events that arrive while a group runs are recorded at the batch's last
sequence number, once the group's calls return and before their results,
so a read after the batch sees them. The body reads no events, so no read
falls inside a batch: check `cancelled` before and after it. A re-run can find
a batch whose last call the journal already holds while earlier none calls
still run again, when recovery reported its external calls `unknown`. Then
the batch records no events, since reads after it replay, and the next
effect takes them (`batch_drains_replayed` records them anyway, and
`event_replay_equal` catches it).

```hl docs/snippets/parallel.hl
harness readers

const calls_max: u64 = 8
const context_max: u64 = 64

tool Read(path: text) -> text effect none
tool Grep(pattern: text) -> text effect none
tool Edit(path: text, change: text) -> text effect external

model think calls calls_max

state {
  context: list<entry, context_max>
}

on ask(question: text) -> text
  requires len(context) + 2 + calls_max <= context_max
{
  push context user(question)
  let reply = think(context)
  push context assistant(reply)
  match reply {
    answer(words) => { return words }
    calls(requested) => {
      for call in requested parallel {
        match dispatch(call) {
          ok(output) => { push context result(call, output) }
          failed(error) => { push context error(call, error) }
          unknown => { push context unknown(call) }
        }
      }
      return "ran the calls"
    }
  }
}
```

Its worst case is the plain loop's, since the records are the same:

```console
$ hl check docs/snippets/parallel.hl | grep -E 'handler ask: worst|proved for'
  handler ask: worst case 9 effects, 20 journal records, 9 outbound requests
docs/snippets/parallel.hl: proved for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
$ hl sim --target cloudflare --seeds 0..200 docs/snippets/parallel.hl
ok target=cloudflare mutation=none seeds=0..200 (200 seeds): turns=504 runs=1444 crashes=940 power_losses=0 torn_writes=0 takeovers=0 fenced=0 dispatches=1633 unknown=383
```

Anything else in the body is an error: a statement before the match, a match
on something other than the item's dispatch, an arm that returns, and a
parallel for inside another.

```console
$ hl check docs/snippets/errors/parallel-statement.hl
docs/snippets/errors/parallel-statement.hl:11:9: error: the body of a parallel for is only a match on dispatch(call): found a let
$ hl check docs/snippets/errors/parallel-dispatch.hl
docs/snippets/errors/parallel-dispatch.hl:13:11: error: a parallel for matches on dispatch(call), the item it walks
$ hl check docs/snippets/errors/parallel-arm.hl
docs/snippets/errors/parallel-arm.hl:12:23: error: the arms of a parallel for run for every call of the batch, so they cannot return: found a return
$ hl check docs/snippets/errors/parallel-nested.hl
docs/snippets/errors/parallel-nested.hl:11:5: error: a parallel for cannot be nested in another parallel for
```

```hl docs/snippets/errors/parallel-statement.hl
harness readers

const calls_max: u64 = 8

state {
  notes: list<text, calls_max>
}

on run(requested: list<call, calls_max>) -> u64 {
  for call in requested parallel {
    let name = call.tool
    match dispatch(call) {
      ok(output) => { push notes output }
      failed(error) => { push notes error }
      unknown => { push notes "lost" }
    }
  }
  return 0
}
```

```hl docs/snippets/errors/parallel-dispatch.hl
harness readers

const calls_max: u64 = 8

tool Read(path: text) -> text effect none

state {
  notes: list<text, calls_max>
}

on run(requested: list<call, calls_max>) -> u64 {
  for call in requested parallel {
    match Read(call.input) {
      ok(body) => { push notes body }
      failed(error) => { push notes error }
    }
  }
  return 0
}
```

```hl docs/snippets/errors/parallel-arm.hl
harness readers

const calls_max: u64 = 8

state {
  notes: list<text, calls_max>
}

on run(requested: list<call, calls_max>) -> u64 {
  for call in requested parallel {
    match dispatch(call) {
      ok(output) => { return 1 }
      failed(error) => { push notes error }
      unknown => { push notes "lost" }
    }
  }
  return 0
}
```

```hl docs/snippets/errors/parallel-nested.hl
harness readers

const calls_max: u64 = 8

state {
  notes: list<text, calls_max>
}

on run(requested: list<call, calls_max>) -> u64 {
  for call in requested parallel {
    for again in requested parallel {
      match dispatch(again) {
        ok(output) => { push notes output }
        failed(error) => { push notes error }
        unknown => { push notes "lost" }
      }
    }
  }
  return 0
}
```

#### Lanes

An arm of a parallel for is its call's lane, as nanocode's Claude loop runs
each `durable_tool` of a batch through `hooked_handler`, the handler with its
PostToolUse hook. The lane runs after the batch, in list order, with its
call's result, and its effects are ordinary effects: each takes the next
sequence number after the batch's, appends its own records and follows its
own durability point, so a crash in a lane leaves the batch's results
journaled and the lane's call as a crash leaves any call. An `after tool`
hook runs in each lane, before the match takes an arm. A `before tool` hook
decides each call before the batch instead, so it cannot share a parallel for
with an `after tool` hook, run an effect or `allow` a rewritten input; the
calls it denies or answers take their sequence numbers in list order, and the
batch's calls on either side of them go out as separate groups, so the
journal still equals the plain loop's:

```hl docs/snippets/lanes.hl
harness lanes

const calls_max: u64 = 4
const context_max: u64 = 64
const summary_tokens_max: u64 = 32

tool Read(path: text) -> text effect none
tool Grep(pattern: text) -> text effect none
tool Bash(command: text) -> text effect external
tool scan(output: text) -> bool effect none
tool summarize(output: text) -> text effect idempotent

model think calls calls_max

state {
  context: list<entry, context_max>
}

after tool(c: call, outcome: maybe<text>) {
  match outcome {
    ok(output) => {
      match scan(output) {
        ok(flagged) => {
          if flagged { deny("the output of " ++ c.tool ++ " was flagged and withheld") }
        }
        failed(_) => {}
      }
    }
    failed(_) => {}
    unknown => {}
  }
}

on ask(question: text) -> text
  requires len(context) + 2 + calls_max <= context_max
{
  push context user(question)
  let reply = think(context)
  push context assistant(reply)
  match reply {
    answer(words) => { return words }
    calls(requested) => {
      for call in requested parallel {
        match dispatch(call) {
          ok(output) => {
            match summarize(output) {
              ok(short) => { push context result(call, truncate_middle(short, summary_tokens_max)) }
              failed(error) => { push context error(call, error) }
            }
          }
          failed(error) => { push context error(call, error) }
          unknown => { push context unknown(call) }
        }
      }
      return "ran the calls"
    }
  }
}
```

The worst case is the plain loop's, lanes included, since every effect keeps
its own record. The simulator runs the lanes' effects with crashes between
them on every target; a parallel for whose lanes run effects numbers them
after the batch where the plain loop interleaves them with its calls, so its
journal is not compared with the plain loop's (`sequential_equal` holds for
batches whose arms only push). `lane_seq_shared` numbers the lanes' effects
from the batch's first sequence number instead, and the rerun's replay
diverges:

```console
$ hl check docs/snippets/lanes.hl | grep -E 'handler ask: worst|proved for'
  handler ask: worst case 13 effects, 28 journal records, 13 outbound requests
docs/snippets/lanes.hl: proved for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
$ hl sim --target cloudflare --seeds 0..200 docs/snippets/lanes.hl
ok target=cloudflare mutation=none seeds=0..200 (200 seeds): turns=504 runs=1612 crashes=1108 power_losses=0 torn_writes=0 takeovers=0 fenced=0 dispatches=1962 unknown=133
$ hl sim --target native-jsonl --seeds 0..200 --twice --mutation lane_seq_shared docs/snippets/lanes.hl | head -n 1
FAIL invariant=liveness seed=0 target=native-jsonl profile=errors mutation=lane_seq_shared: turn 2: replay of turn 2 diverged at seq 2: the journal records spawn_agent, the program runs 'scan' (2/2)
```

### Waits

`wait(c)` pauses the turn for `c` milliseconds, where `c` is a named
constant, and yields the clock at its wake as a `u64`, in the milliseconds
`now()` reads. It is a `none` effect with a durability point of its own:
the runtime reads the clock, appends the wait's `call` record with its
deadline, `{"ms":c,"until":T}`, and flushes every buffered record before it
sleeps, so a host evicted during a long wait loses nothing that came before
it (code mode's C2, for a wait at the top level). Its `result` records the
wake. A rerun after a crash mid-wait finds the call open and sleeps only until
the recorded `until`; a replay returns the recorded wake at once.

The duration enters the worst case as a fourth measure, the milliseconds of
waits, multiplied by the loops around each wait, and `hl check --target`
compares it with the target's wall-time limit (`src/target/mod.rs`, with
sources):

- cloudflare: a turn a reset leaves unfinished resumes in the object's alarm,
  whose wall time is 15 minutes. The turn keeps the alarm armed from its
  first wait or external call to its status, so a run the wall time cuts
  resumes from the alarm past every wait whose deadline it recorded: each
  wait on its own must fit 900,000 ms.
- vercel: a wait's flush ends a Workflow step and the wait runs at the start of
  the next one, so a step holds at most one wait, which must fit a Function's
  800 seconds.
- native-jsonl, native-sqlite and server-postgres set no limit. A Postgres
  lease shorter than a wait is caller policy and not modelled.

`examples/subagents.hl` waits `wait_agent_ms` before it reads a child's
status, as nanocode's `wait_agent` blocks with a timeout (30 s by default
there); the port waits 800 ms. A wait of 600 s in a loop of four fits both
platforms, and an hour-long wait is refused by both:

```console
$ hl check --target cloudflare examples/invalid/over_wait.hl | tail -n 1
examples/invalid/over_wait.hl: ok for cloudflare
$ hl check --target cloudflare examples/invalid/long_wait.hl 2>&1 > /dev/null | tail -n 1
examples/invalid/long_wait.hl: error: handler 'nap': wait(hour_ms) of 3600000 milliseconds exceeds cloudflare's limit of 900000 milliseconds per wait
$ hl check --target vercel examples/invalid/long_wait.hl 2>&1 > /dev/null | tail -n 1
examples/invalid/long_wait.hl: error: handler 'nap': wait(hour_ms) of 3600000 milliseconds exceeds vercel's limit of 800000 milliseconds per wait
```

`wait(c)` waits a named constant. `wait(e) at most c` waits a `u64`
expression bounded by the constant `c`, the delay a harness computes from a
backoff or a provider's `Retry-After`: the worst case counts `c`, the prover
must show `e <= c` (`wait_bound`), and a wait that would pass `c` traps with
`bound(c)` on every backend before its call record is written:

```console
$ hl check examples/invalid/wait_literal.hl
examples/invalid/wait_literal.hl:6:19: error: the duration of 'wait' must be a named constant in milliseconds, or bounded by one: wait(duration) at most constant
$ hl check examples/invalid/wait_uncapped.hl 2>&1 | grep wait_bound
examples/invalid/wait_uncapped.hl: error: handler 'poll': wait_bound: wait(after_ms) may sleep past its cap retry_cap_ms and trap with bound(retry_cap_ms); path: match status(job): ok; counterexample: after_ms = 32001
```

The simulator runs a program past the checker too, and `wall_limit` catches a
run whose waits pass the target's limit or the handler's worst case:

```console
$ hl sim --target cloudflare --seeds 0..20 examples/invalid/long_wait.hl
FAIL invariant=wall_limit seed=0 target=cloudflare profile=errors mutation=none: wait 1/1 waited 3600000 ms by the end of wait 1/1, over cloudflare's limit of 900000 milliseconds per wait
replay: hl sim --target cloudflare --seeds 0..1 examples/invalid/long_wait.hl
```

### Suspend and resume

`suspend(key)` ends the turn waiting for a person or a process outside it,
for as long as that takes, and yields the text answer that resumes it. It
needs `on resume` on the harness. `docs/snippets/park.hl` waits between model
calls and asks for approval before each tool call, as a permission prompt
does:

```hl docs/snippets/park.hl
harness approvals

const steps_max: u64 = 2
const calls_max: u64 = 2
const entries_max: u64 = 64
const poll_ms: u64 = 2000

type outcome = completed(text) | exhausted

tool Bash(command: text) -> text effect external

model infer calls calls_max

on resume

state {
  context: list<entry, entries_max>
}

invariant context_bounded: len(context) <= entries_max

on prompt(input: text) -> outcome
  requires len(context) + 1 + steps_max * (1 + calls_max) <= entries_max
{
  push context user(input)
  loop step below steps_max {
    let woke = wait(poll_ms)
    let reply = infer(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return completed(words) }
      calls(requested) => {
        for call in requested {
          let verdict = suspend(call.id)
          if verdict == "allow" {
            match dispatch(call) {
              ok(output) => { push context result(call, output) }
              failed(error) => { push context error(call, error) }
              unknown => { push context unknown(call) }
            }
          } else {
            push context error(call, "Denied: " ++ verdict)
          }
        }
      }
    }
  }
  return exhausted
}
```

How it runs:

- The first time a run reaches a suspend, the runtime appends its `call`
  record, `{"key":K}` with class `none`, flushes every buffered record and
  ends the run without a `status`: the host learns that turn `T` waits on
  sequence number `S` with key `K`. The waiting status is the durable open
  call, which nanocode's fold reads as an open call, as it reads a parked turn.
- A resume carries a key and a text answer: `Host::resume` in the generated
  Rust, `POST /resume` with `{"turn","key","answer"}` on the Durable Object,
  and a workflow hook with token `session:turn:seq:resume` on Vercel. The
  host runs the turn again with it. The run replays its journal up to the open
  suspend, which takes the resume there: with the same key it appends the
  suspend's `result`, `{"ok":answer}`, flushes it, answers the sender and
  goes on with the answer; with another key it answers unmatched and the turn
  stays waiting.
- A run that reaches the open suspend with no resume, after a crash, waits
  again without appending anything: the call is in the journal. A replay of a
  finished turn returns the recorded answer.
- The worst case counts a suspend as one effect and two records, its call and
  the result its resume writes.

The checker refuses a suspend without the event:

```console
$ hl check examples/invalid/suspend_undeclared.hl
examples/invalid/suspend_undeclared.hl:6:16: error: 'suspend' needs 'on resume' on the harness
```

The prover lowers a suspend to a choice between ending the run there (`Park`,
which must find nothing buffered and the call durable: `suspend_durable`) and
taking its resume, and a wait to a sleep that must follow the flush of its call
and everything before it (`durable_before_wait`):

```console
$ hl check --target vercel docs/snippets/park.hl | grep -E 'parks|wait'
    milliseconds of waits: 4000, step below steps_max (2)
  proved handler prompt on vercel: parks: 1 wait site(s) sleep only after their call and every record before it are durable; 1 suspend site(s) end the run only with nothing buffered and are never appended twice
  proved handler prompt on vercel: worst_case: 2000 milliseconds in its longest wait <= 800000 milliseconds per wait (step below steps_max (2))
  limit waits: 800000 milliseconds per wait, checked on each wait on its own, since the run that holds it may be cut and resumed at a wait (vercel.com/docs/functions/limitations (a Function's duration, Pro and Enterprise, fluid compute; a Workflow step runs in one invocation, while a workflow sleep() has no limit, vercel.com/docs/workflows/pricing); a wait runs inside the step after its flush)
  not checked: duration (800 seconds per Function invocation) beyond waits, since other effects' durations are not modelled
$ hl prove --target native-jsonl --mutation suspend_before_flush docs/snippets/park.hl 2>&1 | grep -m 1 suspend_durable
docs/snippets/park.hl: error: handler 'prompt' on native-jsonl: suspend_durable: suspend(call.id) ends the run waiting while records are not durable: call of suspend(call.id), result of wait(poll_ms), reply of infer(context); path: loop step below steps_max (2) > match reply: calls > for call in requested (at most calls_max = 2) > suspend(call.id) suspends the turn; counterexample: the waiting status is lost with the process, so no resume can find the turn
$ hl prove --target native-jsonl --mutation resume_replays_suspend docs/snippets/park.hl 2>&1 | grep -m 1 seq_unique
docs/snippets/park.hl: error: handler 'prompt' on native-jsonl: seq_unique: suspend(call.id) appends a second call record for an id whose call is already in the journal; path: loop step below steps_max (2) > match reply: calls > for call in requested (at most calls_max = 2) > suspend(call.id) replayed from its recorded answer
```

The simulator draws the clock from the seed: a wait advances it to the
deadline, and a resume arrives up to eight hours after the turn reported
waiting, with another key one time in five. Crashes land while a turn waits
(before the resume's flush and after it) and in the middle of a wait. Four
invariants check it, each with a mutation it catches:

| Invariant | Holds | Planted bug |
| --- | --- | --- |
| `suspend_durable` | a run reports waiting only once its suspend's call record is durable | `suspend_before_flush` |
| `resume_matched` | a suspend's result is the answer of a resume sent with its key | `resume_unmatched` |
| `wait_until` | a wait sleeps until the deadline its durable call record holds, and wakes no earlier | `wait_rerun_skips`, `wait_oversleeps` |
| `wall_limit` | the waits of one run stay within the handler's worst case, and each wait within the target's limit | `examples/invalid/long_wait.hl` past the checker on cloudflare and vercel |

A suspend that runs again on resume appends its call twice, and a wait whose
result is not journaled leaves a finished turn that cannot replay:

```console
$ hl sim --target native-jsonl --seeds 0..100 docs/snippets/park.hl
ok target=native-jsonl mutation=none seeds=0..100 (100 seeds): turns=244 runs=1813 crashes=1088 power_losses=137 torn_writes=152 takeovers=0 fenced=0 dispatches=154 unknown=65 sleeps=781 suspensions=481
$ hl sim --target native-jsonl --seeds 0..100 --mutation resume_replays_suspend docs/snippets/park.hl
FAIL invariant=seq_unique seed=0 target=native-jsonl profile=errors mutation=resume_replays_suspend: turn 1 records seq 3 after seq 4
replay: hl sim --target native-jsonl --seeds 0..1 --mutation resume_replays_suspend docs/snippets/park.hl
$ hl sim --target native-jsonl --seeds 0..100 --mutation wait_unjournaled docs/snippets/park.hl
FAIL invariant=liveness seed=0 target=native-jsonl profile=errors mutation=wait_unjournaled: turn 4: replay of turn 3 needs seq 1, which the journal does not record
replay: hl sim --target native-jsonl --seeds 0..1 --mutation wait_unjournaled docs/snippets/park.hl
```

`examples/hooks.hl` uses both halves: in its `before tool` block, `"ask"` mode makes a
call other than `Read` wait for a person's `"allow"`, and any other answer is the reason the
call is denied; a Stop hook that blocks or stops the turn gives its reason by
resuming the suspend at that point.

### Spawn and join

`spawn name runs handler from "file"` declares a tool that starts a child
session of another harness, as nanocode's `spawn_agent` does. The checker
reads and checks the file, beside the parent, and gives the tool the
handler's parameters (`u64`, `bool` or `text`) and a `text` result: the
child's key. The model can call it like any tool, and a harness can call it by
name. It is external in class, so its call record is durable before the host
starts the child, and a crash then leaves it `unknown`, never started twice.

- The key is the spawn's place in the parent's journal, `T.S` for turn `T` and
  sequence number `S` (under the parent's own key in a [tree](#spawn-trees)): the host keeps the child's session and journal under the
  parent's session and that key, and the runtime records the key as the
  spawn's `ok` result whatever the host answers, so a replay names the same
  child.
- `join(agent)` waits for a child. It is a `suspend` keyed by the child's key, so
  it needs `on resume`; the run that reaches it ends instead of sleeping, and
  the host resumes it with the child's result, the JSON of the value its
  handler returned, once the child's status is durable.
- `hl check` counts the spawns a handler can reach, `child sessions`, and adds
  each child's worst case to the parent's. The prover counts the same spawns on
  the plan and checks the child's outbound requests against the target's limit
  per invocation, since each child runs in its own.
- A spawned harness may spawn children of its own, so a family is a tree
  ([below](#spawn-trees)).

```hl docs/snippets/spawn.hl
harness coordinator

const reviews_max: u64 = 4

type outcome = reviewed(text) | lost(text)

spawn reviewer runs review_change from "spawn_child.hl"

on resume

state {
  started: list<text, reviews_max>
}

invariant started_bounded: len(started) <= reviews_max

on subagent start(agent: text) {
  push started agent
}

on review(change: text) -> outcome
  requires len(started) < reviews_max
{
  match reviewer(change) {
    ok(agent) => {
      let verdict = join(agent)
      return reviewed(verdict)
    }
    failed(error) => { return lost(error) }
    unknown => { return lost("the spawn's outcome is unknown") }
  }
}
```

```hl docs/snippets/spawn_child.hl
harness worker

type verdict = approved(text) | rejected(text)

tool review(change: text) -> text effect idempotent

on review_change(change: text) -> verdict {
  match review(change) {
    ok(notes) => { return approved(notes) }
    failed(error) => { return rejected(error) }
  }
}
```

```console
$ hl check --target cloudflare docs/snippets/spawn.hl | grep child
    child sessions: 1, no loops
    with its child sessions: 3 effects, 12 journal records, 2 outbound requests
  proved handler review on cloudflare: worst_case: 1 child session(s) on the plan's longest path, as hl check counts (no loops), each in its own run; with them 3 effects, 12 journal records, 2 outbound requests
$ hl check docs/snippets/errors/join-undeclared.hl
docs/snippets/errors/join-undeclared.hl:6:16: error: 'join' waits for a spawned session, but the harness declares no spawn tool
```

```hl docs/snippets/errors/join-undeclared.hl
harness notes

on resume

on ping(agent: text) -> text {
  let result = join(agent)
  return result
}
```

The simulator is the host. A spawn it accepts runs the child as a session of
its own, on the same target with its own seed, faults and crashes, and checks
every invariant on the child's journal; a violation in a child fails the
parent's seed. It starts at most `CHILD_SESSIONS_PER_SEED_MAX` = 16 children per
seed, as nanocode keeps `DEFAULT_MAX_RESIDENT_SUBAGENTS` (16) resident, and
refuses more with a failure. A `join` on a child's key is resumed with that
child's result, and the model names a child it started in half its text
inputs (`CHILD_NAMED_PPM` = 500000 per million), so joins find their children.
`child_scoped` requires every recorded key to be its spawn's `T.S` and to name a
child the host started; `spawn_key_unscoped`, a runtime that keys a child by
its sequence number alone, is caught:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/spawn.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=839 runs=2322 crashes=1147 power_losses=166 torn_writes=230 takeovers=0 fenced=0 dispatches=877 unknown=184 sleeps=0 suspensions=336 children=335 joined=271 child_crashes=353
$ hl sim --target native-jsonl --seeds 0..200 --mutation spawn_key_unscoped docs/snippets/spawn.hl
FAIL invariant=child_scoped seed=0 target=native-jsonl profile=errors mutation=spawn_key_unscoped: spawn reviewer at turn 1 seq 1 records child key "1", but its call starts the child session "1.1"
replay: hl sim --target native-jsonl --seeds 0..1 --mutation spawn_key_unscoped docs/snippets/spawn.hl
```

`children` counts the child sessions started, `joined` the joins resumed with
a child's result and `child_crashes` the crashes inside children. The
compiled module needs nothing new for a spawn beyond recording the key: the
child is the host's, and the completion is a resume, so the differential runs
of `examples/subagents.hl` on every target replay the same answers.
`examples/subagents.hl` ports nanocode's subagent runtime: `spawn_agent` starts
`examples/subagent_child.hl`, `wait_agent` joins the child the model names, and
`on subagent start` and `on subagent stop` keep its roster.

`examples/claude_code.hl` offers the same tools to Claude: `spawn_agent`
starts `examples/claude_code_child.hl`'s `delegated(role, task)`, the Claude
harness without its MCP listing, which ends when the model calls
`submit_result`; `list_agents` asks the host; and a round that holds a
`wait_agent` call runs its calls one after another, joining at each
`wait_agent`, since the arms of a `parallel` for run no effects. `hl-host`
(`hosts/native/`) is its host: a spawn writes the child's arguments to
`children/T.S/spawn.json` under the parent's journal directory and runs the
child on a thread, with its own journal there; a join is answered once the
child's status is durable. After a kill, the parent's replay reaches its open
join and the host runs an unfinished child from its journal, whose open
external calls are reported `unknown`, or answers with a finished child's
result.

#### Spawn trees

A child may spawn children of its own. `docs/snippets/tree.hl` starts a
planner, which starts two reviewers of `spawn_child.hl` one after the other and
joins each, so one review is a tree of three sessions under the lead:

```hl docs/snippets/tree.hl
harness lead

const tree_sessions_max: u64 = 3

type outcome = planned(text) | lost(text)

spawn planner runs plan_change from "tree_planner.hl" sessions at most tree_sessions_max

on resume

on review(change: text) -> outcome {
  match planner(change) {
    ok(agent) => {
      let verdict = join(agent)
      return planned(verdict)
    }
    failed(error) => { return lost(error) }
    unknown => { return lost("the spawn's outcome is unknown") }
  }
}
```

```hl docs/snippets/tree_planner.hl
harness planner

const parts_max: u64 = 2

type verdict = planned(text) | lost(text)

spawn reviewer runs review_change from "spawn_child.hl"

on resume

state {
  notes: text
}

on plan_change(change: text) -> verdict {
  notes = ""
  loop part below parts_max {
    match reviewer(change) {
      ok(agent) => {
        let notes_of_part = join(agent)
        notes = notes ++ notes_of_part
      }
      failed(error) => { return lost(error) }
      unknown => { return lost("the spawn's outcome is unknown") }
    }
  }
  return planned(notes)
}
```

- Keys are scoped. A session spawned under key `K` records its own children
  as `K/T.S`, so every key of a family names one session and a host can keep
  each session under its key alone. The host tells a session its key:
  the turn request's `scope` field, on every host. A root session's scope is empty, so its keys stay
  `T.S`.
- A spawn whose child spawns says how many sessions one call may start, the
  child's and every descendant's: `sessions at most c`. `hl check` computes
  the family from the bottom up, each child's family times the spawns that
  reach it, refuses a spawn tool whose tree passes its `c`, and prints the
  whole tree's totals and its sessions.
- The prover lowers every descendant's handler for the same target and
  recomputes the family from the plans, independently of the IR: records,
  outbound requests and sessions on each plan's longest path, times the spawns
  on its parent's. It checks the result against what `hl check` counts, each
  descendant's outbound requests against the target's limit per invocation,
  and each spawn's tree against its `sessions at most`, with the path of
  spawns that passes it.
- The files of a tree form no cycle, and a tree is at most `spawn_depth_max`
  (16) levels deep, which bounds the checker's walk down the files. nanocode
  bounds no depth (`DEFAULT_MAX_SUBAGENTS` is `usize::MAX`) but keeps at most
  `DEFAULT_MAX_RESIDENT_SUBAGENTS` (16) subagents resident in a root
  session's tree.

```console
$ hl check --target cloudflare docs/snippets/tree.hl | grep sessions
    child sessions: 1, no loops
      planner runs planner.plan_change: worst case 6 effects, 21 journal records, 4 outbound requests each, 3 sessions with its descendants (sessions at most tree_sessions_max (3))
    with its child sessions: 8 effects, 28 journal records, 5 outbound requests
    sessions in its tree: 3, 2 levels below it
  proved handler review on cloudflare: worst_case: 3 sessions in its tree on the plans' longest paths, 28 journal records and 5 outbound requests with them, as hl check counts; every spawn within its sessions at most
$ hl check examples/invalid/family_sessions.hl
examples/invalid/family_sessions.hl:7:37: error: spawn tool 'planner' starts up to 3 sessions per call (planner.plan_change and 2 descendant sessions, part below parts_max (2)), over sessions at most two_sessions (2)
$ hl check docs/snippets/errors/spawn-sessions.hl
docs/snippets/errors/spawn-sessions.hl:3:37: error: handler 'plan_change' of harness 'planner' starts child sessions of its own: declare how many sessions one call of 'planner' starts, the child's and its descendants', as spawn planner runs plan_change from "../tree_planner.hl" sessions at most constant
$ hl check docs/snippets/errors/spawn-cycle.hl
docs/snippets/errors/spawn-cycle.hl:5:28: error: spawn tool 'again' cannot check "spawn-cycle.hl": docs/snippets/errors/spawn-cycle.hl: error: the spawn tree returns to a harness it is inside, so its sessions have no bound: docs/snippets/errors/spawn-cycle.hl spawns docs/snippets/errors/spawn-cycle.hl
```

```hl docs/snippets/errors/spawn-sessions.hl
harness lead

spawn planner runs plan_change from "../tree_planner.hl"

on ping() -> u64 { return 1 }
```

```hl docs/snippets/errors/spawn-cycle.hl
harness echo

const echoes_max: u64 = 4

spawn again runs ping from "spawn-cycle.hl" sessions at most echoes_max

on ping() -> u64 { return 1 }
```

The simulator runs a grandchild as it runs a child, as a session of its own
with a seed derived from its parent's, its own faults and every invariant,
under its key's scope. `CHILD_SESSIONS_PER_SEED_MAX` bounds the sessions of
the whole tree per seed, as nanocode's resident limit holds for a root
session's whole tree. `child_scoped` holds at every level, and
`family_bounded` requires the sessions a turn's tree starts to stay within its
handler's family worst case, and each spawn's within its `sessions at most`.
`grandchild_unscoped`, a runtime that keys a grandchild by `T.S` without its
parent's scope, and `family_one_level`, a worst case that counts only the
direct children, are caught:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/tree.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=1168 runs=3645 crashes=1793 power_losses=309 torn_writes=422 takeovers=0 fenced=0 dispatches=1276 unknown=186 sleeps=0 suspensions=684 children=664 joined=546 child_crashes=992
$ hl sim --target native-jsonl --seeds 0..200 --mutation grandchild_unscoped docs/snippets/tree.hl
FAIL invariant=child_scoped seed=1 target=native-jsonl profile=errors mutation=grandchild_unscoped: child session 3.1: spawn reviewer at turn 1 seq 1 records child key "1.1", but its call starts the child session "3.1/1.1"
replay: hl sim --target native-jsonl --seeds 1..2 --mutation grandchild_unscoped docs/snippets/tree.hl
$ hl sim --target native-jsonl --seeds 0..200 --mutation family_one_level docs/snippets/tree.hl
FAIL invariant=family_bounded seed=1 target=native-jsonl profile=errors mutation=family_one_level: turn 3 (review) started 3 sessions in its tree, over its worst case of 1 (1 child sessions, no loops)
replay: hl sim --target native-jsonl --seeds 1..2 --mutation family_one_level docs/snippets/tree.hl
```

`hl sim --scope K` runs a harness as if it were spawned under `K`, and the
differential runs use it, so the compiled module records scoped keys
as the interpreter does (`scripts/check.sh` runs `docs/snippets/tree_planner.hl`
under `1.1` on every target).

### Side questions

`on side name(param: type, ...) -> type [requires condition] { ... }`
declares a side handler: it answers a question about the session without
touching it, as nanocode's `side_question` answers `/btw`. A side handler is
not a turn. The host asks it between turns, while a turn is parked and while
one runs; it appends no record anywhere, writes no checkpoint, takes no lease
and leaves the session's state and sequence numbers as they were. Its model and
tool requests go to the host with turn `0`, which no turn has, so a host tells
them from a turn's; a crash loses the question and the host asks again.
`docs/snippets/side.hl` asks the model with nanocode's
`SIDE_QUESTION_REMINDER` in front of the question, the entry reaching the model
through `with`, never pushed:

```hl docs/snippets/side.hl
harness sidecar

const steps_max: u64 = 2
const calls_max: u64 = 2
const entries_max: u64 = 64

type outcome = completed(text) | exhausted

tool Bash(command: text) -> text effect external

model infer calls calls_max

on resume

state {
  context: list<entry, entries_max>
}

invariant context_bounded: len(context) <= entries_max

on prompt(input: text) -> outcome
  requires len(context) + 1 + steps_max * (1 + calls_max) <= entries_max
{
  push context user(input)
  loop step below steps_max {
    let reply = infer(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return completed(words) }
      calls(requested) => {
        for call in requested {
          let verdict = suspend(call.id)
          if verdict == "allow" {
            match dispatch(call) {
              ok(output) => { push context result(call, output) }
              failed(error) => { push context error(call, error) }
              unknown => { push context unknown(call) }
            }
          } else {
            push context error(call, "Denied: " ++ verdict)
          }
        }
      }
    }
  }
  return exhausted
}

on side ask(question: text) -> text {
  let reminder = "<system-reminder>This is a side question from the user. You must answer this question directly in a single response.\nIMPORTANT CONTEXT:\n- You are a separate, lightweight agent spawned to answer this one question\n- The main agent is NOT interrupted - it continues working independently in the background\n- You share the conversation context but are a completely separate instance\n- Do NOT reference being interrupted or what you were \"previously doing\" - that framing is incorrect\nCRITICAL CONSTRAINTS:\n- You have NO tools available - you cannot read files, run commands, search, or take any actions\n- Do NOT write tool calls or tool output as text (for example invoke or function_calls XML blocks) - nothing you write here is executed; if answering would need reading files, running commands, or searching, say that can't be checked from a side question and suggest asking in the main conversation\n- This is a one-off response - there will be no follow-up turns\n- You can ONLY provide information based on what you already know from the conversation context\n- NEVER say things like \"Let me try...\", \"I'll now...\", \"Let me check...\", or promise to take any action\n- If you don't know the answer, say so - do not offer to look it up or investigate\nSimply answer the question with the information you have.</system-reminder>\n\n"
  let reply = infer(context with user(reminder ++ question))
  match reply {
    answer(words) => { return words }
    calls(_) => { return "A side question cannot run tools: ask it in the main conversation." }
  }
}
```

It reads the state at the last finished turn: after the latest turn whose
`status` is durable, which is the state a running or parked turn started from.
A turn's changes are a function of records that may not be durable yet, so a
crash can roll them back and an answer from them would have seen a state that
never existed; a Durable Object request or a Vercel step cannot read another
run's memory; and this snapshot is the same on every target. Every target makes
a turn's `status` durable before it acknowledges the turn (durability point 2),
so the next turn is admitted only after it. This departs from nanocode, which
shows a side question the running turn's live transcript.

The checker refuses anything that would change the session or need a journal,
in the side handler and in every fn it calls: assigning, `push`, `clear`, `keep`, a
parallel for, `take` and `cancelled`, tools of effect `idempotent` or `external`
(spawn tools among them), `dispatch`, `join`, `suspend`, `wait`, `now()`,
`random()`, listing reads and code cells. The model and tools of effect `none`
remain. No hook runs in a side handler. `requires` is checked over the snapshot;
when it is false the answer is `failed` with `requires(name)`. Its worst case
counts no journal records, and Cloudflare's limit on outbound requests applies
to it, since a side question runs in its own invocation:

```console
$ hl check docs/snippets/side.hl | grep 'side handler'
docs/snippets/side.hl: harness sidecar: 1 tool(s), 1 state field(s), 1 handler(s), 1 side handler(s)
  side handler ask: worst case 1 effects, 0 journal records, 1 outbound requests
  proved side handler ask: finish_once: every path ends in exactly one of 2 return(s)
  proved side handler ask: no_overflow: 0 arithmetic operation(s) proved in range; invariant_preserved: 3 invariant check(s) at returns and trap points; push_bound: 0 push(es) proved below capacity (6 prover steps)
$ hl check docs/snippets/errors/side-push.hl
docs/snippets/errors/side-push.hl:12:3: error: side handler 'ask' cannot push onto 'context': a side question reads the session's state at its last finished turn and changes none of it
$ hl check docs/snippets/errors/side-dispatch.hl
docs/snippets/errors/side-dispatch.hl:10:9: error: side handler 'run' cannot call tool 'Bash' of effect external: a side question is journaled nowhere, so it calls only the model and tools of effect none
```

```hl docs/snippets/errors/side-push.hl
harness notes

const entries_max: u64 = 8

model infer calls entries_max

state {
  context: list<entry, entries_max>
}

fn remember(question: text) {
  push context user(question)
}

on prompt(input: text) -> text {
  return input
}

on side ask(question: text) -> text {
  remember(question)
  return question
}
```

```hl docs/snippets/errors/side-dispatch.hl
harness notes

tool Bash(command: text) -> text effect external

on prompt(input: text) -> text {
  return input
}

on side run(command: text) -> text {
  match Bash(command) {
    ok(output) => { return output }
    failed(error) => { return error }
    unknown => { return "unknown" }
  }
}
```

Each backend runs a side question as a scratch run: a fresh engine over a copy
of the snapshot whose flushes are discarded. The interpreter answers through
`run::side::answer` between runs and through `World::side`, which a running
turn polls before each effect it runs fresh. The module exports `hl_side`,
which a host calls over the session's journal without acquiring it; the Durable Object answers `POST /side` with
`{"handler", "arguments"}` from its rows, without queueing behind a running turn
or writing storage, its output gate holding the answer until rows a finishing
turn inserted commit; Vercel exports `harnessSide(lines, request)`, which folds
the run's journal lines outside any step.

The simulator asks side questions only of programs that declare a side handler:
before an effect a turn runs fresh (`SIDE_AT_EFFECT_PPM` = 100000 per million),
after each turn (`SIDE_BETWEEN_TURNS_PPM` = 500000) and while a turn is parked
(`SIDE_PARKED_PPM` = 500000), with replies drawn from generators keyed apart
from the session's, so no other draw moves. `side_isolated` requires the
journal, durable and buffered, to be the same after a question as before, and
the answer to equal an oracle run over the state that replaying the durable
journal's finished turns gives, with the same replies and the same requests.
`side_live_state`, a runtime that answers from the running turn's state, and
`side_journaled`, one that appends the side run's records to the session, are
caught:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/side.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=3131 crashes=1610 power_losses=184 torn_writes=286 takeovers=0 fenced=0 dispatches=314 unknown=160 sleeps=0 suspensions=1017 side_questions=947
$ hl sim --target native-jsonl --seeds 0..200 --mutation side_live_state docs/snippets/side.hl
FAIL invariant=side_isolated seed=0 target=native-jsonl profile=errors mutation=side_live_state: side question 5 (ask turn 2 before seq 3): the snapshot at the last durable status gives another run: request 1 of the oracle's run is a model call at seq 1 over 4 context entries (crc 1293496392), where the runtime's was a model call at seq 1 over 6 context entries (crc 256348170)
replay: hl sim --target native-jsonl --seeds 0..1 --mutation side_live_state docs/snippets/side.hl
$ hl sim --target native-jsonl --seeds 0..200 --mutation side_journaled docs/snippets/side.hl
FAIL invariant=side_isolated seed=0 target=native-jsonl profile=errors mutation=side_journaled: side question 1 (ask while turn 1 is parked) changed the session's journal: 443 bytes before it, 878 after
replay: hl sim --target native-jsonl --seeds 0..1 --mutation side_journaled docs/snippets/side.hl
```

`side_questions` counts the questions asked, printed only when there are some.
The differential runs carry them too: the compiled module answers
every question the interpreter asks, mid-turn and between runs, with the same
requests and answers.

### Fork and rewind

`fork name at latest` and `fork name at turn` declare a tool that starts a new
session of the same harness from this session's state at a finished turn, as
nanocode forks a conversation at a boundary and rewinds one into a new session.
`at latest` takes no input and forks at the last finished turn; `at turn`
takes `turn: u64`, a finished turn of this session or `0` for the entry
state, so a fork at an earlier turn is a rewind. The tool is external like a
spawn and its result is the child's key, the call's scoped `T.S`.

The runtime fills the call's input before its call record is written:
`{"discarded": D, "origin": KEY, "state": P, "turn": N}`, where `P` is the
state after turn `N`, taken from the durable journal as a side question's
snapshot is, then passed through `on forked`, and `D` counts the finished
turns after `N`. A host needs only the input to write the child's first two
records, a `forked` host record and the entry checkpoint:

```text
{"host":{"key":"forked","value":{"discarded":D,"origin":KEY,"turn":N}}}
{"checkpoint":{"payload":P,"revision":0,"state":NAME}}
```

Every target's load takes such a journal and starts turn 1 from `P`. A turn
that is neither `0` nor a finished turn is the call's `failed("no finished
turn N to fork from")` and never reaches the host. The same function, a
`fork_entry` over a session's records, a turn or `latest` and the origin,
lets a host fork a session it holds without a tool call: the interpreter's
`run::fork::fork_entry`, which returns the two records or `ForkRefused`, and Vercel's
`harness.fork(lines, at, origin)`. Vercel's `harnessSession` takes the
forked lines as its third argument, and a Durable Object loads the two rows a
host wrote before the first turn.

`on forked(origin: text, discarded: u64) { ... }` shapes the state a fork
starts from: it may assign, push, clear and keep, but runs no effect and does
not return. `docs/snippets/fork.hl` keeps the parent's key and, on a rewind,
adds nanocode's rewind notice under its list's capacity:

```hl docs/snippets/fork.hl
harness brancher

const steps_max: u64 = 2
const calls_max: u64 = 2
const entries_max: u64 = 64

type outcome = completed(text) | exhausted

tool Bash(command: text) -> text effect external

fork rewind at turn

model infer calls calls_max

state {
  context: list<entry, entries_max>
  parent: text
}

invariant context_bounded: len(context) <= entries_max

on prompt(input: text) -> outcome
  requires len(context) + 1 + steps_max * (1 + calls_max) <= entries_max
{
  push context user(input)
  loop step below steps_max {
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

on forked(origin: text, discarded: u64) {
  parent = origin
  if discarded > 0 and len(context) < entries_max {
    push context user("This conversation was explicitly rewound into a new session. External effects from discarded turns may still exist; reconcile their current state before repeating any action. Historical tool calls must not be replayed.")
  }
}
```

`hl check` counts each fork as one child session: the child runs no turn
inside the call. The prover proves the hook like a handler without
`requires`, over any state that satisfies the invariants, so its push sits
under `if len(context) < entries_max`. The fork site is an external effect,
durable before it runs:

```console
$ hl check --target cloudflare docs/snippets/fork.hl | grep -E 'child sessions|hook forked'
    child sessions: 4, step below steps_max (2) x call below calls_max (2)
    with its child sessions: 6 effects, 13 journal records, 6 outbound requests
  proved hook forked: no_overflow: 0 arithmetic operation(s) proved in range; invariant_preserved: 1 invariant check(s) at returns and trap points; push_bound: 1 push(es) proved below capacity (40 prover steps)
$ hl check docs/snippets/errors/forked-effect.hl
docs/snippets/errors/forked-effect.hl:16:9: error: 'on forked' cannot call tool 'Bash': it runs inside the fork's call, before the call is journaled, so it runs no effect
```

```hl docs/snippets/errors/forked-effect.hl
harness notes

tool Bash(command: text) -> text effect external

fork branch at latest

state {
  last: text
}

on prompt(input: text) -> text {
  return input
}

on forked(origin: text, discarded: u64) {
  match Bash(origin) {
    ok(output) => { last = output }
    failed(error) => { last = error }
    unknown => { last = "unknown" }
  }
}
```

The simulator's model forks at an earlier turn and, a quarter of the time
(`FORK_TURN_PAST_PPM` = 250000 per million), at one not finished yet. The
host writes the child's two records from the input and runs up to
`FORK_CHILD_TURNS_MAX` (2) turns of the harness over them, with faults and
every invariant, as a spawned child is run. A quarter of the seeds of a
harness with a fork tool (`FORKED_ENTRY_PPM` = 250000) start from a forked
entry themselves, as do a quarter of the seeds of a harness that declares
`on forked` without a fork tool, so every backend's load of one is checked in the
differential runs. `fork_state_equal` requires the
records to equal the hook applied to the state after turn `N` of the
parent's durable journal up to the call, with `N`, `D` and the key in the
`forked` record. `fork_live_state`, a runtime that forks from the running
turn's state, and `fork_hook_skipped`, one that skips the hook, are caught:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/fork.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=978 runs=2975 crashes=1997 power_losses=233 torn_writes=316 takeovers=0 fenced=0 dispatches=1030 unknown=300 forks=315
$ hl sim --target native-jsonl --seeds 0..200 --mutation fork_live_state docs/snippets/fork.hl
FAIL invariant=fork_state_equal seed=0 target=native-jsonl profile=errors mutation=fork_live_state: fork rewind at turn 4 seq 2 starts the child with 'context' = 20 entries in its entry checkpoint, where the hook over the parent's durable journal gives 13 entries
replay: hl sim --target native-jsonl --seeds 0..1 --mutation fork_live_state docs/snippets/fork.hl
$ hl sim --target native-jsonl --seeds 0..200 --mutation fork_hook_skipped docs/snippets/fork.hl
FAIL invariant=fork_state_equal seed=0 target=native-jsonl profile=errors mutation=fork_hook_skipped: fork rewind at turn 4 seq 2 starts the child with 'context' = 12 entries in its entry checkpoint, where the hook over the parent's durable journal gives 13 entries
replay: hl sim --target native-jsonl --seeds 0..1 --mutation fork_hook_skipped docs/snippets/fork.hl
```

`forks` counts the forked sessions the host started, printed only when there
are some. This departs from nanocode, which forks from a checkpoint the
running turn published: here the snapshot is a finished turn's, the same on
every target, and a fork never sees a turn a crash could roll back. A harness
with blobs cannot fork, since a checkpoint holds no blob's bytes.

A host forks a session it holds through the same `fork_entry` (the
interpreter's, or the fixed host's), so `on forked` needs no fork tool. The fixed
native host does not fork yet: the per-program host's `--fork-from J [--at T]`,
which wrote the two records from journal `J` at its latest finished turn, or at
turn `T`, as the first records of a new journal, left with it; `hl-host run
--turn N` runs an instruction as a later turn of the same journal, which gives
a parent turns to rewind.
`examples/claude_code.hl`'s `on forked` disarms the prompt cache refresh, whose
due belongs to the parent's last request, and with `discarded > 0` pushes
nanocode's rewind notice. A host refuses to fork a state that holds images,
whose bytes stay in the parent's journal.

### Idle turns

`on idle name() at field bound N -> type { ... }` declares the one handler the
host runs while the session idles: once every turn has finished and the clock
reaches the time, in the milliseconds `now()` reads, that the `u64` state field
`field` names, the host admits it as a turn of its own, with no arguments. A
field of 0 arms nothing. nanocode keeps its prompt cache warm this way: after
a reply the backend keeps its request and, while the session idles, sends it
again before the cache's lifetime runs out, at most a bounded number of times
per kept request, and stops when a refresh fails or finds the cache lapsed
(`agent/keep.rs`). `docs/snippets/keep.hl` writes that logic in the harness:
the due is the request's start plus the cache's lifetime less twice its reach
(the time from the request to its reply), and the idle turn refreshes, stops
when the cache read nothing or wrote something, and keeps the cache again from
its own readings.

```hl docs/snippets/keep.hl
harness keeper

const calls_max: u64 = 2
const entries_max: u64 = 64
const window_tokens: u64 = 200000
const lifetime_ms: u64 = 300000
const refreshes_max: u64 = 12
const clock_ms_max: u64 = 8640000000000000

type outcome = answered(text) | failed(text)
type kept = refreshed(u64) | lapsed(text)

model infer calls calls_max window window_tokens modes refresh

state {
  context: list<entry, entries_max>
  requested_ms: u64
  refresh_due: u64
}

invariant context_bounded: len(context) <= entries_max

fn kept_at(started: u64, opened: u64) {
  refresh_due = 0
  if started <= opened and opened <= clock_ms_max {
    if (opened - started) * 2 < lifetime_ms {
      requested_ms = started
      refresh_due = started + lifetime_ms - (opened - started) * 2
    }
  }
}

on prompt(input: text) -> outcome
  requires len(context) + 2 <= entries_max
{
  push context user(input)
  let started = now()
  match infer(context) {
    ok(reply, _, _, _, _, _) => {
      let opened = now()
      kept_at(started, opened)
      push context assistant(reply)
      match reply {
        answer(words) => { return answered(words) }
        calls(_) => { return answered("") }
      }
    }
    throttled(error, _) => { return failed(error) }
    transient(error, _) => { return failed(error) }
    rejected(error) => { return failed(error) }
  }
}

on idle refresh() at refresh_due bound refreshes_max -> kept {
  let started = now()
  if started > clock_ms_max or requested_ms > clock_ms_max {
    return lapsed("the clock reads past its range")
  }
  if started >= requested_ms + lifetime_ms {
    return lapsed("the cache outlived its lifetime")
  }
  match infer(context) mode refresh {
    ok(_, _, _, _, cache_read, cache_write) => {
      let opened = now()
      if cache_read == 0 or cache_write > 0 {
        return lapsed("the refresh found the cache lapsed")
      }
      kept_at(started, opened)
      return refreshed(cache_read)
    }
    throttled(error, _) => { return lapsed(error) }
    transient(error, _) => { return lapsed(error) }
    rejected(error) => { return lapsed(error) }
  }
}
```

How it runs:

- Arming. The idle turn is armed for `field`'s time when the field is not 0
  and fewer than `N` idle turns ran since the last other turn. The checker keeps
  that count in a hidden state field, `_idle_runs`, which every other handler
  sets to 0 where it starts and the idle handler counts up: its `requires`
  gains `_idle_runs < N`, and the invariant `_idle_bounded`, `_idle_runs <= N`,
  is proved with the program's own. The idle handler's body starts with the
  field set to 0, so an idle turn disarms its due unless it assigns a new one.
- Admission. A host's timer is advice. The runtime admits an idle request only
  when the session's last turn has finished (otherwise `busy`), the state arms
  it (`unarmed`) and the clock, read as a `wait` reads it, has reached the due
  (`early`). A refused request writes nothing and takes no turn number. An
  admitted idle turn's `admitted` record holds the clock and the due,
  `{"at":A,"due":D}`.
- Journal. An idle turn is a turn like any other: its effects are journaled
  under it, a crash reruns it as its classes say, and a replay returns its
  recorded results, so a refresh whose reply is in the journal is never sent
  again. A prompt that arrives before the due runs first, resets the count and
  computes its own due.
- Worst case. `hl check` prints the idle handler's worst case and that many
  times `N`, the most the idle turns after one other turn can cost.
- Hosts. Rust: `idle_due(start, records)` and `JsonlJournal::idle_due` give the
  armed due, `run_turn` returns `Ran::NotDue` for a refused request, and
  `hl-host run --keep-warm N` stays alive after its prompt and runs up to `N`
  idle turns. Cloudflare: after every finished turn the Durable Object sets its
  alarm to the due, and `alarm()` runs the idle turn when no turn is
  unfinished. Vercel: before each request the session reads the due in a step
  and races the next prompt's hook against a workflow `sleep()` until it.
- The model's `refresh` mode is a name the host interprets, as every mode is:
  `hl-agent` sends it as nanocode's refresh, the request that produced the
  context's last reply (the entries before its last `assistant` entry) with
  `max_tokens` 0. The Messages API accepts `max_tokens` 0 only unstreamed, so
  the refresh is not streamed.

```console
$ hl check docs/snippets/keep.hl | grep -E '^  handler refresh|runs when'
  handler refresh: worst case 3 effects, 8 journal records, 1 outbound requests
    runs when the session idles until refresh_due, at most refreshes_max (12) time(s) after each other turn: 36 effects, 96 journal records, 12 outbound requests in all
$ hl check docs/snippets/errors/idle-due.hl
docs/snippets/errors/idle-due.hl:9:22: error: an idle handler's due time is a u64 state field: 'kept' is not one
```

```hl docs/snippets/errors/idle-due.hl
harness keeper

const refreshes_max: u64 = 12

state {
  kept: text
}

on idle refresh() at kept bound refreshes_max -> u64 { return 1 }
```

The simulator is the host. After every turn of a harness with an idle handler
it reads the armed due. A prompt arrives before it in `IDLE_PREEMPT_PPM` =
300000 per million of the times it is armed (never after the session's last
prompt, so every session idles out), the host's timer fires early in
`IDLE_EARLY_PPM` = 150000, at most once per due, and late by up to
`IDLE_LATE_MS_MAX` (ten minutes, twice nanocode's five-minute cache lifetime)
in `IDLE_LATE_PPM` = 100000, and an idle turn's model call reads the cache it
refreshes and writes none in `CACHE_WARM_PPM` = 800000. It asks at most
`REQUESTS_PER_DUE_MAX` (2) times for each of `N` + 1 idle turns after a prompt
and fails a session still armed after that. Crashes, power losses and second
owners land in idle turns as in any other. These draws happen only for a
harness with an idle handler, so other programs' seeds are unchanged.
`idle_due` requires every idle turn to have been admitted at a clock at or
after its recorded due, for the due its state armed, at most `N` after each
other turn, and every early refusal to name a clock before its due;
`idle_early`, a runtime that admits an idle request before its due, is caught:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/keep.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=862 runs=2218 crashes=1300 power_losses=71 torn_writes=118 takeovers=0 fenced=0 dispatches=0 unknown=0 idle_turns=358 idle_refused=56
$ hl sim --target native-jsonl --seeds 0..200 --mutation idle_early docs/snippets/keep.hl
FAIL invariant=idle_due seed=4 target=native-jsonl profile=async mutation=idle_early: idle turn 4 was admitted at clock 302253, before its due 602071
replay: hl sim --target native-jsonl --seeds 4..5 --mutation idle_early docs/snippets/keep.hl
```

`idle_turns` counts the idle turns admitted and `idle_refused` the requests
the runtime refused. The compiled module makes the same
admission, so the differential runs refuse and admit the same requests as the
interpreter; the plant `wasm idle_early` removes the clock check and is caught
as `backend_equal` on every target.
`examples/claude_code.hl` keeps nanocode's five-minute `CACHE_LIFETIME`
(`cache_lifetime_ms`) and the twelve refreshes `ClaudeModel::cache_refreshes`
allows for it (`cache_refreshes`: a five-minute cache write costs 1.25 times
an input token and a read 0.1 times, so twelve reads cost what one more write
would).

### Effects in flight

Three of nanocode's behaviours concern an effect that is still running, or
that a crash left open: a cancel or steer that abandons a running tool or
model stream (nanocode's `agent/durable.rs`, the select on cancel around
`call_tool`), a Bash call that keeps running as a background job and reports
later (`claude_bash.rs`), and a host that answers for a call a crash left
open (`recover_tool`). `docs/snippets/in_flight.hl` uses all three:

```hl docs/snippets/in_flight.hl
harness worker

const rounds_max: u64 = 6
const calls_max: u64 = 3
const entries_max: u64 = 512
const steers_max: u64 = 2
const notices_max: u64 = 4
const jobs_max: u64 = 2
const window_tokens: u64 = 100000
const hold_ms: u64 = 30000

type outcome = done(text) | stopped(text) | exhausted

tool Bash(command: text) -> text effect external recoverable interruptible by cancel, steer jobs at most jobs_max
tool Read(path: text) -> text effect none interruptible by cancel
tool Write(path: text, body: text) -> text effect external recoverable

model infer calls calls_max window window_tokens interruptible by cancel

on cancel
on steer(message: text) bound steers_max
on job(notice: text) bound notices_max

state {
  context: list<entry, entries_max>
}

invariant context_bounded: len(context) <= entries_max

on prompt(input: text) -> outcome
  requires len(context) + 1 + rounds_max * (3 + calls_max + steers_max + notices_max) <= entries_max
{
  push context user(input)
  loop round below rounds_max {
    for steer in take steers { push context user(steer) }
    for notice in take jobs { push context user(notice) }
    match infer(context) {
      ok(reply, _, _, _, _, _) => {
        push context assistant(reply)
        match reply {
          answer(words) => {
            let woke = hold(hold_ms)
            if cancelled { return stopped("cancelled") }
            return done(words)
          }
          calls(requested) => {
            for call in requested parallel {
              match dispatch(call) {
                ok(output) => { push context result(call, output) }
                failed(error) => { push context error(call, error) }
                unknown => { push context unknown(call) }
              }
            }
            if cancelled {
              push context user("[Request interrupted by user]")
              return stopped("cancelled during tools")
            }
          }
        }
      }
      interrupted => {
        push context user("[Request interrupted by user]")
        return stopped("cancelled during the model")
      }
      throttled(error, _) => { return stopped(error) }
      transient(error, _) => { return stopped(error) }
      rejected(error) => { return stopped(error) }
    }
  }
  return exhausted
}
```

#### Interruptible effects

`interruptible by cancel`, `interruptible by steer` or both, after a tool's
effect class or at the end of a `model` declaration, lets those events, which
the harness must declare, abandon the effect:

- An interruptible effect that would start while an event that interrupts it
  is recorded in the turn (a cancel, or a steer `take steers` has not yet
  taken) does not run: a tool's result is `failed` with nanocode's
  `StartedToolCall::REASON` text ("Tool execution interrupted; outcome
  unknown. Do not assume it did not run or automatically repeat it."), and
  the model's is `interrupted`.
- An interruptible effect during which such an event is recorded (at its own
  sequence number) is abandoned: the host is told to stop it, an external
  tool's call is reported `unknown` with a `recovery` record, as nanocode
  reports a tool that raced the cancel, a none or idempotent tool's result is
  `failed` with the same text, and the model's is `interrupted`.
- An interruptible model needs a `window`: its `attempt` gains a fifth
  variant, `interrupted`, recorded as `{"interrupted":[]}`, so the match on
  it must say what the turn does then. nanocode pushes
  "[Request interrupted by user]" and ends the turn (`agent.rs`,
  `INTERRUPTED_BY_USER`), which the snippet's arm writes.
- In a parallel for, the calls of the group during which the event arrived are
  abandoned and the later groups' calls do not start, as nanocode answers the
  calls of an interrupted batch.

Replay needs nothing new: the result records already hold the outcome, and the
event records hold when each event arrived. On Vercel, whose events enter in a
drain step between steps, an interruptible effect flushes before it runs, so
an event that arrived before it is taken at that flush and preempts it; one
that arrives while it runs is taken after its result. A cell's nested calls are not interruptible one by one, but a
cancel can stop the whole cell: [Stopping a cell](#stopping-a-cell).

#### Jobs

`jobs at most N` on an external tool that returns text lets the host turn a
call into a background job, as nanocode's Bash does with `run_in_background` or
past its timeout: the host answers at once with `{"job": text}`, the program
reads `ok(text)`, and the job is open under the key `T.S` of the call's turn
and sequence number until a notice closes it. The request says whether the
tool has room (fewer than `N` of its jobs open in the session); a job started
without room is recorded as the call's failure. The harness takes the
notices with `on job(notice: text) bound M`, a third event beside cancel and
steer: `{"job":{"done":D,"id":I,"key":K,"notice":T}}`, recorded with `at`
like a steer, at most `M` per turn, and read with `take jobs`, the notices
recorded since the last `take jobs`, oldest first. A notice with `done`
false (nanocode's stall notice, a job waiting on interactive input) leaves the
job open; one with `done` true closes it. Notices that arrive while no turn
runs wait for the next turn, which records them during its first effects.

`hold(c)` or `hold(e) at most c` is the turn hold nanocode runs before a turn
ends: a wait, flushed first and journaled like one, that returns the clock as
soon as a job notice or a cancel arrives, or at its deadline, and at once
when no job is open. It records the events that woke it at its own sequence
number. `examples/claude_code.hl` holds for up to nanocode's
`DEFAULT_BACKGROUND_TIMEOUT_MS` (600 s, after which a moved job is stopped) when
it would answer, and goes on with another round when notices arrived.

A job does not outlive its host's process on every target, so whenever a turn
runs over a session with open jobs the runtime asks the host which of them
still run, and for each it no longer runs records nanocode's "killed, outcome
unknown" notice (`id` `lost:K`, `done` true) at the next effect, as
nanocode does at load.

#### Recovery hooks

`effect external recoverable` marks a tool whose host can tell what became
of a call a crash left open: a `Write` can compare the file's digest with the
one it would write, a `Bash` host can look for the job it started. Before
reporting such a call `unknown`, recovery asks the host with the call's own
turn, sequence number and input, and records its answer in the `recovery`
record:

| The host answers | Recorded | The turn sees |
| --- | --- | --- |
| the real reply | `"replied":["T/S"]`, then the call's `result` | the reply, `ok` or `failed` |
| not dispatched | `"not_dispatched":["T/S"]` | the call runs now, under the same sequence number and call record |
| unknown | `"unknown":["T/S"]` | `unknown` |

The two lists appear only when they hold a call, so other journals are
unchanged. On Vercel the step that finds a reopened external call past its
first attempt asks the host the same way. On Postgres an answer of not
dispatched claims the call for the asking owner's epoch, and the host refuses
the call from an owner of an older epoch, which would otherwise run it a
second time after a takeover.

#### Hosts

The module asks its host through the `world` import for `recover` (answered
with a reply, `not_dispatched` or `unknown`), `jobs` (the keys of open jobs it no
longer runs) and `hold`, and a host that uses none of them answers each with its
default; a tool request carries `job` (the tool has room for a job) and, with a
model request, `interrupts`, the events that abandon it, which the host
watches in its inbox. Job notices reach the module as `job` events. The Durable Object and the Vercel binding ask the host at
`HL_RECOVER_URL` and `HL_JOBS_URL`, abort an interruptible request's `fetch`
with an `AbortSignal` when a cancel or steer is posted, and take job notices
posted without a turn. `hl-agent` uses all of it for `examples/claude_code.hl`:
a Bash call past its timeout keeps running as a job and is held for at the end
of the turn, `run_in_background` jobs end with the turn as nanocode's
`ENDS_WITH_TURN` binding says, and recovery answers from the file a `Write`
or `Edit` left, or from the receipts `Bash` writes before and after it runs.

#### What the checker, prover and simulator do

The checker refuses each declaration that cannot hold:

```console
$ hl check docs/snippets/errors/interrupt-undeclared.hl
docs/snippets/errors/interrupt-undeclared.hl:3:67: error: 'interruptible by cancel' needs 'on cancel' on the harness
$ hl check docs/snippets/errors/model-interrupt-window.hl
docs/snippets/errors/model-interrupt-window.hl:5:46: error: model 'infer' is interruptible only with a window: its interrupted call is the 'interrupted' variant of attempt
$ hl check docs/snippets/errors/recoverable-none.hl
docs/snippets/errors/recoverable-none.hl:3:43: error: tool 'Read' has effect none: only an external tool is recoverable, since only its outcome can be unknown
$ hl check docs/snippets/errors/jobs-no-event.hl
docs/snippets/errors/jobs-no-event.hl:5:63: error: tool 'Bash' keeps jobs running, so the harness needs 'on job(notice: text) bound N' to take their notices
$ hl check docs/snippets/errors/hold-no-jobs.hl
docs/snippets/errors/hold-no-jobs.hl:6:14: error: 'hold' waits for job notices, so it needs 'on job(notice: text) bound N' on the harness
```

```hl docs/snippets/errors/interrupt-undeclared.hl
harness worker

tool Bash(command: text) -> text effect external interruptible by cancel

on run(command: text) -> text {
  match Bash(command) {
    ok(output) => { return output }
    failed(error) => { return error }
    unknown => { return "lost" }
  }
}
```

```hl docs/snippets/errors/model-interrupt-window.hl
harness worker

const calls_max: u64 = 4

model infer calls calls_max interruptible by cancel

on cancel

on run() -> u64 { return 1 }
```

```hl docs/snippets/errors/recoverable-none.hl
harness worker

tool Read(path: text) -> text effect none recoverable

on run(path: text) -> text {
  match Read(path) {
    ok(body) => { return body }
    failed(error) => { return error }
  }
}
```

```hl docs/snippets/errors/jobs-no-event.hl
harness worker

const jobs_max: u64 = 4

tool Bash(command: text) -> text effect external jobs at most jobs_max

on run(command: text) -> text {
  match Bash(command) {
    ok(output) => { return output }
    failed(error) => { return error }
    unknown => { return "lost" }
  }
}
```

```hl docs/snippets/errors/hold-no-jobs.hl
harness worker

const hold_ms: u64 = 1000

on run() -> u64 {
  let woke = hold(hold_ms)
  return woke
}
```

`hl check` counts job notices beside the steers and the cancel, and a hold's
milliseconds among the waits:

```console
$ hl check docs/snippets/in_flight.hl | sed -n 1,8p
docs/snippets/in_flight.hl: harness worker: 3 tool(s), 1 state field(s), 1 handler(s)
  handler prompt: worst case 24 effects, 45 journal records, 24 outbound requests
    effects: round below rounds_max (6) x call below calls_max (3)
    journal records: round below rounds_max (6) x call below calls_max (3)
    outbound requests: round below rounds_max (6) x call below calls_max (3)
    milliseconds of waits: 30000, no loops
    plus one recovery record per restart
    plus at most 7 event record(s) per turn (1 cancel, steers_max (2) steer(s), notices_max (4) job notice(s))
```

The prover adds three arms to every interruptible effect (it finishes, it is
abandoned while it runs, it never starts) and to a recoverable call reopened
after a crash (the host's three answers), and proves durability, finishing
once and the worst case on each. external_once holds unless the host attested
the call was not dispatched; trusting an `unknown` answer is caught:

```console
$ hl prove --target native-jsonl --mutation recover_trusts_lost docs/snippets/in_flight.hl 2>&1 | grep -m 1 -o 'external_once: [^;]*'
external_once: external dispatch(call) runs again after recovery found its call open
$ hl prove --target vercel --mutation recover_trusts_lost docs/snippets/in_flight.hl 2>&1 | grep -m 1 -o 'external_once: [^;]*'
external_once: external reopened dispatch(call) runs again on a retried step
```

The simulator sends cancels and steers that land while effects run, starts jobs
that stall, finish across turns or die in crashes, and answers recovery
truthfully (the reply the tool returned, not dispatched when it never ran) or
`unknown`:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/in_flight.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=1903 crashes=1399 power_losses=145 torn_writes=226 takeovers=0 fenced=0 dispatches=760 unknown=222 events=130 sleeps=35 suspensions=0 interrupted=36 recovered=140 not_dispatched=302 recover_unknown=192 jobs=67 job_notices=21 jobs_died=33
$ hl sim --target server-postgres --seeds 0..200 docs/snippets/in_flight.hl
ok target=server-postgres mutation=none seeds=0..200 (200 seeds): turns=504 runs=1396 crashes=823 power_losses=0 torn_writes=0 takeovers=115 fenced=69 dispatches=739 unknown=145 events=124 sleeps=36 suspensions=0 interrupted=28 recovered=110 not_dispatched=141 recover_unknown=105 jobs=58 job_notices=23 jobs_died=25
$ hl sim --target vercel --seeds 0..200 docs/snippets/in_flight.hl
ok target=vercel mutation=none seeds=0..200 (200 seeds): turns=504 runs=1680 crashes=1176 power_losses=0 torn_writes=0 takeovers=0 fenced=0 dispatches=813 unknown=93 events=146 sleeps=35 suspensions=0 interrupted=41 recovered=63 not_dispatched=51 recover_unknown=42 jobs=63 job_notices=14 jobs_died=28
```

| Invariant | Holds | Planted bug |
| --- | --- | --- |
| `interrupt_honored` | an interruptible effect whose outcome is recorded after an event that interrupts it arrived during it, or after a cancel, records an interrupted outcome, and no outcome is interrupted without such an event | `interrupt_ignored` |
| `external_once` | an abandoned external call is not sent again, and a recovered one runs again only after its host attested it never ran | `interrupt_reruns`, `recover_trusts_lost` |
| `job_once` | a job is closed at most once, and reported lost only after its host lost it | `job_lost_all` |
| `job_reported` | a job its host lost is reported lost by the next run that records events | `job_lost_dropped` |
| `unknown_reported` | a recovered reply is the reply the tool returned | |
| `wait_until` | a hold sleeps no later than its durable deadline | `wait_oversleeps` |

```console
$ hl sim --target native-jsonl --seeds 0..200 --mutation interrupt_ignored docs/snippets/in_flight.hl
FAIL invariant=interrupt_honored seed=5 target=native-jsonl profile=lying mutation=interrupt_ignored: 1/5 is interruptible by cancel and an event that interrupts it was recorded before its outcome, yet it records a finished outcome
replay: hl sim --target native-jsonl --seeds 5..6 --mutation interrupt_ignored docs/snippets/in_flight.hl
$ hl sim --target native-jsonl --seeds 0..200 --mutation interrupt_reruns docs/snippets/in_flight.hl
FAIL invariant=external_once seed=28 target=native-jsonl profile=errors mutation=interrupt_reruns: external call 1/4 (Bash) was dispatched 2 times
replay: hl sim --target native-jsonl --seeds 28..29 --mutation interrupt_reruns docs/snippets/in_flight.hl
$ hl sim --target vercel --seeds 0..200 --mutation recover_trusts_lost docs/snippets/in_flight.hl
FAIL invariant=external_once seed=4 target=vercel profile=errors mutation=recover_trusts_lost: external call 1/2 (Write) was dispatched 2 times
replay: hl sim --target vercel --seeds 4..5 --mutation recover_trusts_lost docs/snippets/in_flight.hl
$ hl sim --target native-jsonl --seeds 0..200 --mutation job_lost_all docs/snippets/in_flight.hl
FAIL invariant=job_once seed=19 target=native-jsonl profile=errors mutation=job_lost_all: job 3.4 is reported lost in turn 3, yet it was still running
replay: hl sim --target native-jsonl --seeds 19..20 --mutation job_lost_all docs/snippets/in_flight.hl
$ hl sim --target native-jsonl --seeds 0..100 --mutation job_lost_dropped docs/snippets/in_flight.hl
FAIL invariant=job_reported seed=29 target=native-jsonl profile=async mutation=job_lost_dropped: job 1.4 died in a crash and its host reported it lost, yet a later run took events without recording its lost notice
replay: hl sim --target native-jsonl --seeds 29..30 --mutation job_lost_dropped docs/snippets/in_flight.hl
```

### Payments

A harness pays for HTTP 402 Payment Required answers with the Machine Payments
Protocol (MPP), as nanocode's `nanocode-mpp` does: a tool, an MCP listing or an
HTTP fetch whose server asks for payment is paid from a wallet, and the call is
sent again with the payment. `wallet NAME spend at most C charge at most M
payments at most K` declares the wallet: the session spends at most `C`
units, one payment charges at most `M` and one paid call makes at most `K`
payments (nanocode's `DEFAULT_MAX_PAYMENT_RETRIES` is 3). `paid by NAME`
after a tool's or a listing's effect class makes its calls paid, and `spent()`
reads what the session has spent:

```hl docs/snippets/payments.hl
harness payments

const rounds_max: u64 = 4
const calls_max: u64 = 3
const context_max: u64 = 64
const spend_cap: u64 = 120000
const charge_max: u64 = 50000
const payments_max: u64 = 3

type outcome = done(text, u64) | exhausted(u64)

wallet tempo spend at most spend_cap charge at most charge_max payments at most payments_max

tool fetch(url: text) -> text effect idempotent paid by tempo
tool search(query: text) -> text effect external paid by tempo

model think calls calls_max

state {
  context: list<entry, context_max>
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  loop round below rounds_max {
    let reply = think(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return done(words, spent()) }
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
  return exhausted(spent())
}
```

A paid tool gains a `payment` parameter, empty on the first call, and a direct
call of it has type `maybe<T>` whatever its class, since a payment's outcome can
be unknown. The host of a paid tool answers a 402 as the call's failure, its
text `payment required: ` followed by the challenges as JSON
(`{"challenges":[{"id","realm","method","intent","amount",...}]}`). The checker
desugars each paid call, and every `dispatch` of a harness with a paid tool or
listing, into ordinary sites:

1. the call; a failure that starts with `payment required: ` for a paid tool
   starts a payment round, anything else is the outcome;
2. a round computes the room, `min(M, C - spent)`, and fails the call with
   "payment required, but the wallet has reached its spend cap" when it is 0;
   otherwise it calls the wallet, a synthesized tool `NAME(required: text, max:
   u64) -> u64 effect external recoverable`, which chooses a challenge it
   supports, pays at most `max` and returns the units it paid;
3. a paid amount within the room is added to the hidden state field `_spent`
   and the call runs again with `payment` set to the challenges paid, so the
   host presents the credential the wallet made for them; another 402 starts
   the next of the `K` rounds;
4. a wallet failure fails the call with the wallet's text, and an unknown wallet
   outcome makes the call unknown and adds the whole room to `_spent`.

The outcome is rebuilt from hidden state fields, so the arms of the program's
`match` appear once and push once per call whatever the rounds did. A run of
the snippet journals a paid search as three calls: the 402, the payment of 42524
units and the presentation, which this server refused:

```console
$ hl run docs/snippets/payments.hl --handler ask --arg hi --seed 0 | grep -o '"call_id":"1/[3-5]","[a-z]*":.\{0,48\}'
"call_id":"1/3","effect":"external","input":{"payment":"","query":"alpha-
"call_id":"1/3","output":{"failed":"payment required: {\"challenges\":[{\
"call_id":"1/4","effect":"external","input":{"max":50000,"required":"paym
"call_id":"1/4","output":{"ok":42524},"seq":4}}}
"call_id":"1/5","effect":"external","input":{"payment":"payment required:
"call_id":"1/5","output":{"failed":"HTTP 400: the server refused the paym
```

Each payment is a journaled external call, durable before the wallet runs:
recovery asks the wallet about an open one (`replied` with the units it paid,
or `not_dispatched`), and a replayed turn takes a recorded payment from the
journal, so no target pays twice for one call. The model cannot reach the wallet:
`dispatch` refuses its name as input that does not fit. The checker adds the
invariant `_spend_within_cap` (`_spent <= C`), which the prover proves at every
return and trap, and counts each round in the worst case:

```console
$ hl check docs/snippets/payments.hl | sed -n 2,3p
  handler ask: worst case 88 effects, 175 journal records, 88 outbound requests
    effects: round below rounds_max (4) x call below calls_max (3) x payment below payments_max (3)
```

```console
$ hl check docs/snippets/errors/wallet-charge.hl
docs/snippets/errors/wallet-charge.hl:7:47: error: wallet 'tempo' charges at most charge_max (5000) a payment, over its spend cap cap (1000)
$ hl check docs/snippets/errors/paid-undeclared.hl
docs/snippets/errors/paid-undeclared.hl:3:57: error: 'fetch' is paid by 'tempo', which is not a declared wallet
```

```hl docs/snippets/errors/wallet-charge.hl
harness shop

const cap: u64 = 1000
const charge_max: u64 = 5000
const payments_max: u64 = 2

wallet tempo spend at most cap charge at most charge_max payments at most payments_max

tool fetch(url: text) -> text effect idempotent paid by tempo

on ping() -> u64 { return 1 }
```

```hl docs/snippets/errors/paid-undeclared.hl
harness shop

tool fetch(url: text) -> text effect idempotent paid by tempo

on ping() -> u64 { return 1 }
```

A paid call in a parallel for pays in its lane, after the batch, as
`docs/snippets/paid_lanes.hl` does:

```hl docs/snippets/paid_lanes.hl
harness paid_lanes

const rounds_max: u64 = 4
const calls_max: u64 = 3
const context_max: u64 = 64
const spend_cap: u64 = 120000
const charge_max: u64 = 50000
const payments_max: u64 = 3

type outcome = done(text, u64) | exhausted(u64)

wallet tempo spend at most spend_cap charge at most charge_max payments at most payments_max

tool fetch(url: text) -> text effect idempotent paid by tempo
tool search(query: text) -> text effect external paid by tempo

model think calls calls_max

state {
  context: list<entry, context_max>
}

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max * (1 + calls_max) <= context_max
{
  push context user(question)
  loop round below rounds_max {
    let reply = think(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return done(words, spent()) }
      calls(requested) => {
        for call in requested parallel {
          match dispatch(call) {
            ok(output) => { push context result(call, output) }
            failed(error) => { push context error(call, error) }
            unknown => { push context unknown(call) }
          }
        }
      }
    }
  }
  return exhausted(spent())
}
```

The simulator plays an MPP server for every paid tool, a port of nanocode-sim's
`mpp` scenario: one to three challenges a 402, some of another method, expired,
a zero-amount proof for another realm, over the charge cap, of the `session`
intent or reused; and for a presented payment a success, another 402, a refusal,
a server error or a lost connection. Its wallet pays the first challenge it
supports within `max` and refuses one it already paid. Its invariants:

| Invariant | Holds | Planted bug |
| --- | --- | --- |
| `spend_within_cap` | the wallet never paid a session more than `C` | `cap_unchecked` (the wallet is given `M` as its max) |
| `spend_accounted` | every checkpoint's `_spent` covers what the wallet charged in the turns before it | `spent_unaccounted` |
| `payment_presented` | every payment was presented by the retry, unless the retry was refused before it left, or the payment or the retry is reported unknown | `credential_dropped` |
| `external_once` | no payment is made twice, across crashes, takeovers and retried steps | `recover_trusts_lost` |

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/payments.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=2128 crashes=1624 power_losses=160 torn_writes=258 takeovers=0 fenced=0 dispatches=1488 unknown=353 recovered=48 not_dispatched=356 recover_unknown=171 payment_required=365 pays=189 paid_units=4396624 presented=201
$ hl sim --target native-jsonl --seeds 0..200 --mutation cap_unchecked docs/snippets/payments.hl | head -n 1
FAIL invariant=payment_presented seed=0 target=native-jsonl profile=errors mutation=cap_unchecked: wallet tempo paid 24379 for c5 at 3/3, yet no request presented that payment and the journal does not report it unknown
$ hl sim --target native-jsonl --seeds 0..200 --mutation spent_unaccounted docs/snippets/payments.hl | head -n 1
FAIL invariant=spend_accounted seed=0 target=native-jsonl profile=errors mutation=spent_unaccounted: the checkpoint after turn 3 holds _spent = 0, yet wallet tempo charged 24379 by then
$ hl sim --target native-jsonl --seeds 0..200 --mutation credential_dropped docs/snippets/payments.hl | head -n 1
FAIL invariant=payment_presented seed=0 target=native-jsonl profile=errors mutation=credential_dropped: wallet tempo paid 9736 for c8 at 4/5, yet no request presented that payment and the journal does not report it unknown
```

The prover's `drop_pay_guard` drops the check that a wallet's amount fits the
room, and the cap no longer proves:

```console
$ hl prove --target native-jsonl --mutation drop_pay_guard docs/snippets/payments.hl 2>&1 | grep -m 1 -o 'invariant_preserved: [^;]*'
invariant_preserved: invariant '_spend_within_cap' (_spent <= spend_cap) may not hold when the reply of think(context) fails to decode and the turn traps
```

### Hill climbing

`climb NAME over FIELD ... { propose(base) { } measure(subject) { } }` is a
statement that improves a state field by trial: an autoresearch loop. Each cycle
the propose block offers candidates for the field's current value, the measure
block scores the baseline and each candidate in paired trials, an accept rule
decides, and a kept candidate becomes the field's value. Every evaluated
candidate, kept or not, is a row of a ledger the harness keeps in state, so a
run resumed after a crash, or a later turn, goes on from the ledger instead of
starting over.

The clauses:

| Clause | Meaning |
| --- | --- |
| `over field` | the text state field being improved; the propose block gets its value as `base` |
| `from e` | the field's value when the ledger is empty; with rows already there the climb goes on from the field |
| `ledger field` | a `list<cycle, N>` state field; one row per evaluated candidate |
| `end field` | a `climb_end` state field that says why the climb stopped |
| `cycles N` | at most N cycles |
| `candidates N` | at most N candidates per cycle (1 without the clause); one more is a breach |
| `trials N` | paired trials per candidate: each measures the baseline, then the candidate |
| `cost B propose P trial T` | a budget B over the costs the blocks yield, at most P per proposal and T per trial |
| `wall W` | stop once `now()` has moved W past the climb's start |
| `failures N` | stop after N cycles in a row keep nothing |
| `metric m lower at most MAX`, `higher`, `within S` | a score a trial yields, in declaration order; the first decides, a later one with `within` may not get worse than S per trial |
| `gate e` | a pure condition over the metric names that every trial must meet |
| `accept mean by M`, `best by M`, `sign at least W`, `when (b, c, w) e` | the accept rule over the first metric: the sums of the trials beat by M per trial, the best trial beats by M, at least W paired wins, or a pure condition over the baseline's sum, the candidate's sum and the wins |

Inside the blocks, `yield candidate(value, hypothesis)` offers a candidate,
`yield score(v, ...)` gives one trial's metrics and `yield cost c` charges the
budget. A block may run effects; the measure block's parameter is the revision
being measured, the baseline or a candidate. A trial that yields no score, a
value over its metric's maximum, or a candidate that fails a gate ends that
candidate's evaluation; a baseline that fails a gate, a cost over its cap or more
candidates than the cap end the climb with `breach`.

A ledger row is `cycle(round, evaluation, hypothesis, baseline, candidate,
baseline score, candidate score, wins, trials, cost, spent, verdict, reason,
gate failures)` with the verdict one of `failed` `kept` `worse` `gated`
`regressed` `cancelled`; `end` is one of `running` `cycles` `budget`
`wall` `failures` `breach` `cancelled` `full` (the ledger has no room
for another cycle's rows). Before each cycle the climb stops unless the budget
still covers a whole cycle at its caps, so `spent` never passes `cost`. With
`on cancel` a recorded cancel stops the climb between trials.

The checker lowers a climb into ordinary statements over hidden state fields
(names starting with `_climb_`), so the interpreter, the compiled module and
every proof see plain loops: `cycles x candidates x trials`
bounds the worst case, and the prover shows the climb's counters stay in range
and its pushes fit:

```hl docs/snippets/climb.hl
harness tune

const cycles_max: u64 = 4
const candidates_max: u64 = 2
const trials_max: u64 = 3
const failures_max: u64 = 3
const cost_max: u64 = 30
const propose_cost_max: u64 = 2
const trial_cost_max: u64 = 1
const nanos_max: u64 = 1000000
const bytes_max: u64 = 100000
const bytes_slack: u64 = 64
const failing_max: u64 = 100
const margin_nanos: u64 = 50
const ledger_max: u64 = 64

tool suggest(base: text) -> text effect idempotent
tool bench(subject: text) -> u64 effect idempotent
tool size(subject: text) -> u64 effect none
tool failing(subject: text) -> u64 effect idempotent

on cancel

state {
  config: text
  log: list<cycle, ledger_max>
  ended: climb_end
}

on prompt(start: text) -> text {
  climb speed over config from start ledger log end ended
    cycles cycles_max candidates candidates_max trials trials_max
    cost cost_max propose propose_cost_max trial trial_cost_max
    failures failures_max
    metric nanos lower at most nanos_max
    metric bytes lower at most bytes_max within bytes_slack
    metric broken lower at most failing_max
    gate broken == 0
    accept mean by margin_nanos
  {
    propose(base) {
      loop pick below candidates_max {
        match suggest(base) {
          ok(change) => { yield candidate(change, "try " ++ change) }
          failed(_) => {}
        }
        yield cost 1
      }
    }
    measure(subject) {
      match bench(subject) {
        ok(nanos) => {
          match failing(subject) {
            ok(count) => {
              match size(subject) {
                ok(bytes) => { yield score(nanos, bytes, count) }
                failed(_) => {}
              }
            }
            failed(_) => {}
          }
        }
        failed(_) => {}
      }
      yield cost trial_cost_max
    }
  }
  return config
}
```

```console
$ hl check docs/snippets/climb.hl | grep -E 'handler prompt: no_overflow|proved for'
  proved handler prompt: no_overflow: 40 arithmetic operation(s) proved in range; invariant_preserved: 0 invariant check(s) at returns and trap points; push_bound: 3 push(es) proved below capacity (474309 prover steps)
docs/snippets/climb.hl: proved for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
```

A noisy benchmark wants a rule that counts paired wins rather than one that
compares two numbers:

```hl docs/snippets/climb_noise.hl
harness noisy

const cycles_max: u64 = 6
const trials_max: u64 = 5
const wins_needed: u64 = 4
const cost_max: u64 = 16
const propose_cost_max: u64 = 1
const trial_cost_max: u64 = 1
const ledger_max: u64 = 16
const nanos_max: u64 = 60000000000

tool suggest(base: text) -> text effect idempotent
tool bench(subject: text) -> u64 effect idempotent

state {
  config: text
  log: list<cycle, ledger_max>
}

on prompt(start: text) -> text {
  climb speed over config from start ledger log
    cycles cycles_max trials trials_max
    cost cost_max propose propose_cost_max trial trial_cost_max
    metric nanos lower at most nanos_max
    accept sign at least wins_needed
  {
    propose(base) {
      match suggest(base) {
        ok(change) => { yield candidate(change, "try " ++ change) }
        failed(_) => {}
      }
      yield cost propose_cost_max
    }
    measure(subject) {
      match bench(subject) {
        ok(nanos) => { yield score(nanos) }
        failed(_) => {}
      }
      yield cost trial_cost_max
    }
  }
  return config
}
```

The simulator replays every journal it writes and checks the ledger against
four invariants: `climb_kept_valid` (a kept row ran all its trials, failed no
gate and beats its baseline under the accept rule, recomputed from the row's
scores), `climb_state_follows` (each row starts where the row before it left
the field, and the field holds what the last row left), `climb_ledger_complete`
(no evaluated candidate is missing) and `climb_cost_balanced` (each row's
running total is the sum of its run's costs). Mutations of the lowering show
each one catching what it is for:

```console
$ hl sim --target native-jsonl --seeds 0..200 --strict docs/snippets/climb_noise.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds, strict): turns=1315 runs=5930 crashes=4615 power_losses=81 torn_writes=129 takeovers=0 fenced=0 dispatches=16194 unknown=0
$ hl sim --target native-jsonl --seeds 0..200 --strict --mutation climb_accept_any docs/snippets/climb_noise.hl | head -n 1
FAIL invariant=climb_kept_valid seed=39 target=native-jsonl profile=errors mutation=climb_accept_any: climb 'speed': ledger row 2 is kept although candidate 5317 does not beat baseline 1811 under the accept rule
$ hl sim --target native-jsonl --seeds 0..200 --strict --mutation climb_revert_skipped docs/snippets/climb_noise.hl | head -n 1
FAIL invariant=climb_state_follows seed=0 target=native-jsonl profile=errors mutation=climb_revert_skipped: climb 'speed': ledger row 1 starts from Text("eta-19"), but the row before it left Text("theta-59")
$ hl sim --target native-jsonl --seeds 0..200 --strict --mutation climb_ledger_drops_losses docs/snippets/climb_noise.hl | head -n 1
FAIL invariant=climb_ledger_complete seed=0 target=native-jsonl profile=errors mutation=climb_ledger_drops_losses: climb 'speed': 3 candidate(s) were evaluated but the ledger's last row is evaluation 2
$ hl sim --target native-jsonl --seeds 0..200 --strict --mutation climb_cost_double_counted docs/snippets/climb_noise.hl | head -n 1
FAIL invariant=climb_cost_balanced seed=0 target=native-jsonl profile=errors mutation=climb_cost_double_counted: climb 'speed': ledger row 0 says 3 was spent, but its run's rows cost 2
```

A gate keeps a faster candidate that breaks something from being kept. The row
counts the candidate's trials that failed a gate whatever the climb then does,
so a lowering that ignored the gate is caught:

```hl docs/snippets/climb_gate.hl
harness gated

const cycles_max: u64 = 8
const trials_max: u64 = 1
const wins_needed: u64 = 1
const ledger_max: u64 = 16
const nanos_max: u64 = 60000000000
const failing_max: u64 = 1000000

tool suggest(base: text) -> text effect idempotent
tool bench(subject: text) -> u64 effect idempotent
tool failing(subject: text) -> u64 effect idempotent

state {
  config: text
  log: list<cycle, ledger_max>
}

on prompt(start: text) -> text {
  climb speed over config from start ledger log
    cycles cycles_max trials trials_max
    metric nanos lower at most nanos_max
    metric broken lower at most failing_max
    gate broken == 0
    accept sign at least wins_needed
  {
    propose(base) {
      match suggest(base) {
        ok(change) => { yield candidate(change, "try " ++ change) }
        failed(_) => {}
      }
    }
    measure(subject) {
      match bench(subject) {
        ok(nanos) => {
          match failing(subject) {
            ok(count) => { yield score(nanos, count) }
            failed(_) => {}
          }
        }
        failed(_) => {}
      }
    }
  }
  return config
}
```

```console
$ hl sim --target native-jsonl --seeds 0..200 --strict --mutation climb_gate_ignored docs/snippets/climb_gate.hl | head -n 1
FAIL invariant=climb_kept_valid seed=12 target=native-jsonl profile=async mutation=climb_gate_ignored: climb 'speed': ledger row 0 is kept although 1 candidate trial(s) failed a gate
```

`examples/autoresearch.hl` is a whole autoresearch harness: a baseline commit,
a propose block in which Claude edits a git working tree with Bash, Read and
Edit for up to `rounds_max` model calls and its final answer becomes the
candidate's commit message, and a measure block that benchmarks a revision and
counts failing tests, gated to zero. Its host is parked: `hl-climb` left the per-program host
when every program there moved to compiled WebAssembly.

```console
$ hl check examples/autoresearch.hl | grep -E 'handler prompt: no_overflow|proved for'
  proved handler prompt: no_overflow: 38 arithmetic operation(s) proved in range; invariant_preserved: 0 invariant check(s) at returns and trap points; push_bound: 8 push(es) proved below capacity (261650 prover steps)
examples/autoresearch.hl: proved for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
```

### Traps

A turn ends `failed` without running further when `requires` is false
(`requires(handler)`, state unchanged), a `u64` operation overflows or
underflows, a `push` finds its list full (`bound(list)`), or a reply of a model
without a declared reply type does not decode. A trap still writes `status` and is acknowledged once. The prover
rules out the overflow and full-list traps, so in a proving program only the
first and last remain.

## Bounds

Every loop and list has a named bound, and the checker multiplies them into
each handler's worst case: effects, journal records and outbound requests.
Journal records count `ADMISSION_RECORDS` = 1 per turn, `EFFECT_RECORDS` = 2 per
tool, `now` or `random` call (`call` and `result`), `MODEL_RECORDS` = 1 per model
reply and `FINISH_RECORDS` = 2 per finish (`status` and checkpoint); a
recovery adds one record per restart, reported apart. Outbound requests count
tool and model calls, not `now` or `random`. A `for` counts its list's
capacity, a `loop` its bound, a `match` or `if` its widest arm. A `return`
ends its path: a loop counts the iterations before a return in full and the
returning one only up to its return, so a lookup that returns from its first
iteration costs one request, not one per element:

```hl docs/snippets/early_return.hl
harness lookup

const notes_max: u64 = 20000

tool fetch(key: text) -> text effect idempotent

state {
  notes: list<text, notes_max>
}

on first() -> text {
  for note in notes {
    match fetch(note) {
      ok(body) => { return body }
      failed(error) => { return error }
    }
  }
  return "no notes"
}
```

```console
$ hl check --target cloudflare docs/snippets/early_return.hl | grep -E 'worst|ok for'
  handler first: worst case 1 effects, 5 journal records, 1 outbound requests
  proved handler first on cloudflare: worst_case: 5 journal records per run on the plan's longest path, as hl check counts (no loops)
  proved handler first on cloudflare: worst_case: 1 outbound requests <= 10000 outbound requests per invocation (no loops)
docs/snippets/early_return.hl: ok for cloudflare
```

The prover counts the same records again on each target's lowered plan, where
every path ends at its finish, and requires the two counts to agree.

The compiler's own bounds:

| Bound | Value | Source |
| --- | --- | --- |
| nesting of blocks, types, expressions | `NESTING_DEPTH_MAX` = 64 | policy; also bounds the blocks of a handler once its fns are inlined |
| statements per handler once its fns are inlined | `STATEMENTS_PER_HANDLER_MAX` = 65,536 | policy: over a thousand times the largest example handler; a program whose fns double at each level reaches it in milliseconds |
| prover steps per handler | `PROOF_STEPS_PER_HANDLER_MAX` = 8,000,000 | policy; reaching it fails the obligation and says so |
| constraints per prover step | `PROOF_CONSTRAINTS_MAX` = 4,096 | policy; same |
| nesting of a journal's JSON | `JSON_DEPTH_MAX` = 128 | policy |
| Cloudflare outbound requests per invocation | `CLOUDFLARE_SUBREQUESTS_PER_INVOCATION_MAX` = 10,000 | Workers Paid default, developers.cloudflare.com/workers/platform/limits |
| Cloudflare requests of a parallel for in flight | `CLOUDFLARE_CONNECTIONS_WAITING_MAX` = 6 | simultaneous open connections waiting for response headers, developers.cloudflare.com/workers/platform/limits |
| Cloudflare isolate memory | `CLOUDFLARE_ISOLATE_MEMORY_BYTES_MAX` = 134,217,728 | developers.cloudflare.com/durable-objects/platform/limits; reported, not checked |
| Vercel Function duration, seconds | `VERCEL_FUNCTION_DURATION_SECONDS_MAX` = 800 | Pro and Enterprise, vercel.com/docs/functions/limitations; reported, not checked |
| Vercel step attempts | `VERCEL_STEP_ATTEMPTS_MAX` = 4 | `maxRetries` 3 by default, workflow-sdk.dev |

## Invariants and requires

`requires` and `invariant` are what the arithmetic proofs may assume and must
keep. The prover treats every `u64` field, list length and local as a variable
in range, adds what assignments, conditions, `requires` and the invariants say,
and proves each obligation by showing its negation has no solution. A failure
names the handler, the property, the path and, where it can, values that
reach it. Those values solve the prover's facts; after a branch join or a loop
they can be values no run reaches.

Inside a loop the prover assumes an invariant at every iteration's start when
the invariant holds where the loop starts and every iteration that goes on to
the next keeps it. A counter that a guard keeps below its bound, such as a
retry count, then proves ([Model replies](#model-replies)); without the guard
it does not:

```console
$ cat examples/invalid/retry_unbounded.hl
harness retry_unbounded

const calls_max: u64 = 4
const rounds_max: u64 = 16
const attempts_max: u64 = 5
const context_max: u64 = 64

type outcome = done(text) | stopped(text) | exhausted
type turn_reply = answer(text) | calls(list<call, calls_max>)

model think calls calls_max reply turn_reply

state {
  context: list<entry, context_max>
  failures: u64
}

invariant failures_bounded: failures < attempts_max

on ask(question: text) -> outcome
  requires len(context) + 1 + rounds_max <= context_max
{
  push context user(question)
  failures = 0
  loop round below rounds_max {
    match think(context) {
      failed(error) => { failures = failures + 1 }
      ok(reply) => {
        failures = 0
        push context assistant(reply)
        match reply {
          answer(words) => { return done(words) }
          calls(requested) => { return stopped("no tools") }
        }
      }
    }
  }
  return exhausted
}
$ hl check examples/invalid/retry_unbounded.hl > /dev/null
examples/invalid/retry_unbounded.hl: error: handler 'ask': invariant_preserved: invariant 'failures_bounded' (failures < attempts_max) may not hold when the handler returns; path: entry; counterexample: failures = 16
examples/invalid/retry_unbounded.hl: error: 1 proof obligation(s) failed for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
```

The prover's own examples, under `examples/invalid/`:

```console
$ cat examples/invalid/underflow_elapsed.hl
harness underflow_elapsed

state {
  elapsed: u64
}

on time(note: text) -> u64 {
  let started = now()
  let finished = now()
  elapsed = finished - started
  return elapsed
}
$ hl check examples/invalid/underflow_elapsed.hl > /dev/null
examples/invalid/underflow_elapsed.hl: error: handler 'time': no_overflow: u64 underflow in 'finished - started' is possible; path: entry; counterexample: started = 1, finished = 0
examples/invalid/underflow_elapsed.hl: error: 1 proof obligation(s) failed for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
$ hl check examples/invalid/invariant_initial.hl > /dev/null
examples/invalid/invariant_initial.hl: error: handler '(zero state)': invariant_initial: invariant 'started' (turns >= turns_min) may not hold in the zero state; path: the zero state; counterexample: turns = 0
examples/invalid/invariant_initial.hl: error: 1 proof obligation(s) failed for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
$ hl check examples/invalid/invariant_trap.hl > /dev/null
examples/invalid/invariant_trap.hl: error: handler 'ask': invariant_preserved: invariant 'idle' (busy == 0) may not hold when the reply of think(history) fails to decode and the turn traps; path: entry; counterexample: busy = 1
examples/invalid/invariant_trap.hl: error: 1 proof obligation(s) failed for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
$ hl check examples/invalid/product_overflow.hl > /dev/null
examples/invalid/product_overflow.hl: error: handler 'measure': no_overflow: u64 overflow in 'across * down' is possible; path: match count(width): ok > match count(height): ok; counterexample: the factors reach 18446744073709551615 and 18446744073709551615
examples/invalid/product_overflow.hl: error: 1 proof obligation(s) failed for native-jsonl, native-sqlite, server-postgres, cloudflare, vercel
```

What `hl prove` and `hl check` prove, per handler, per target, on every path
including crash and rerun paths:

| Property | Holds when |
| --- | --- |
| `finish_once` | every path ends in one `return`, and on every target appends one `status`, flushes it and acknowledges, never twice |
| `durable_before_external` | an external effect runs only with nothing buffered and its own `call` flushed |
| `external_once` | an external call a crash left open never runs again |
| `unknown_reported`, `idempotent_known` | a reopened external call yields `unknown`; a reopened none or idempotent call runs again |
| `seq_unique` | no path writes a second `call` for a sequence number already in the journal |
| `output_gate` | on Cloudflare, nothing leaves the object while a record is uncommitted |
| `no_overflow` | every `+`, `-` and `*` stays in range, in `requires` too |
| `push_bound` | every `push` finds room |
| `invariant_initial`, `invariant_preserved` | invariants hold in the zero state, at every `return` and at every trap left |
| `worst_case` | journal records per run, counted on each target's plan, equal `hl check`'s count; outbound requests per invocation fit the target's limit |

What the proofs assume and leave out is in
[ARCHITECTURE.md](ARCHITECTURE.md#proofs).

## Targets

| Target | Runs in | Journal | Atomic unit | Who else may run | Recovery | Limits checked | Ships |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `native-jsonl` | a process | an append-only JSONL file per session, a CRC-32 per line | one line | nobody | at load | the program's own | `program.wasm`, run by the fixed native host (`harnesslang::wasm::native`), which answers its imports (tools, model, clock, randomness, acknowledgement) and stores the journal through a `Store` |
| `native-sqlite` | a process | SQLite rows through the host's `Sql` | one transaction | nobody | at load | the program's own | `program.wasm` compiled for `native-sqlite`, run by `hl-host run --store sqlite:<file>`, which keeps the journal in `hl_journal` rows under a lease |
| `server-postgres` | a server pool | Postgres rows through the host's `Sql` | one transaction, fenced by owner epoch | a second owner after a lease expires | at load, by the new owner | the program's own | as `native-sqlite`, with `hl-host run --store postgres:<host:port>/<database>`; the store refuses a flush whose epoch no longer owns the session |
| `cloudflare` | a Durable Object | the object's SQLite | one event | nobody; an alarm restarts the object after a reset | at load | outbound requests per invocation | `program.wasm`, run by the fixed host `hosts/cloudflare.mjs`: the `HarnessObject` Durable Object class and a Worker whose `fetch` routes each session to its object |
| `vercel` | a Function driving a Workflow run | the run's event log | one step | a retried step | in the step, by attempt number | none yet (duration is reported) | `program.mjs` (the module's bytes as an ES module), run by the fixed host `hosts/vercel.mjs` and its `"use workflow"` session and `"use step"` step in `hosts/vercel-session.mjs` |

```console
$ hl emit --format wat --target cloudflare examples/agent.hl | grep -oE '^\(import "hl" "[a-z]+"' | cut -d'"' -f4 | paste -sd' '
fail hook flush keep tool model now random attempt acknowledge world
```

A compiled module imports only the host functions above, and each fixed host
uses only its platform's own API. Every target's lowering is simulated by `hl sim` with its
own faults; `scripts/check.sh` sweeps every example on every target and runs
every planted bug, and `HL_ORACLE=1` and `HL_DRILL=1` add the nanocode oracle
and the kill -9 drills of the compiled module on real files, SQLite and Postgres.

## Commands

| Command | Does |
| --- | --- |
| `hl check [--target T] FILE` | parse, type-check, report each handler's worst case and prove it; with a target, also check the target's limits |
| `hl prove [--target T] [--mutation M] FILE` | the proofs alone; a mutation plants a lowering bug the proofs must catch |
| `hl sim --target T --seeds A..B [--twice] [--strict [--any-state]] [--mutation M] FILE` | simulate seeds on a target's fault model; `--twice` requires identical journals from two runs; `--strict` also checks what the proofs promise (no trap but requires and model replies, invariants after every turn, the worst case within the target's limit), and `--any-state` starts each session from a drawn state that satisfies the invariants |
| `hl sim --exhaustive [--depth N] [--cuts lines\|bytes] --target T --seeds A..B [--mutation M] FILE` | explore every crash point of each seed's workload up to N faults per session, cell hooks included; takes neither `--twice`, `--strict`, `--trace` nor `--dump-journal`, and refuses `--mutation divergence_dispatches`, since it injects no engine divergence |
| `hl run FILE --handler NAME [--arg VALUE | --arg-file PATH]... [--seed S]` | run one turn in the interpreter and print the journal; `--arg-file` passes a file's text as a text argument |
| `hl emit --target T [--format wasm\|wat\|flat\|source] FILE` | print the program as a WebAssembly module (the default), its text, the assembled module as hex in the flat layout `compiler/encode.hl` reads (docs/SELFHOST.md), or source |

`check`, `prove` and `sim` exit 1 when something fails and 2 on a usage error.

## Code mode

Code mode lets the model answer with a JavaScript cell that calls the
harness's tools as nested calls, as nanocode's Code Mode does. A `code`
declaration names the tool, the tools a cell may call (`tools`, a comma
separated list) and the engine's limits, each a named constant: `calls`,
`parallel`, `timers`, `timer_ms`, `journal_bytes` and `yield_ms`, in any order,
each once. The checker requires a model, a non-empty set of declared tools that
are not code tools, `parallel` at least 1 and at most `calls`. Calling the tool
(`exec(script)`) or reaching it through `dispatch` yields `maybe<text>`:
`unknown` only when the cell could not be resumed. A nested external call is
journaled and flushed with the cell's header before it runs and is never
dispatched twice. `examples/code_agent.hl` is a complete harness, and
[CODE_MODE.md](CODE_MODE.md) the whole design.

A nested tool returning a text enum is checked where the cell receives its
result, as a direct call is: a value outside the enum reaches the script as
the call's failure and is journaled so. `examples/code_tasks.hl` lists
`lookup(ticket: text) -> status` among its cell's tools, and shares its model
loop between two handlers through a fn, so each handler keeps its own cell
site. The planted bug `nested_enum_unchecked` takes the unlisted value as the
enum's last one instead:

```console
$ hl sim --target native-jsonl --seeds 0..200 --mutation nested_enum_unchecked examples/code_tasks.hl
FAIL invariant=nested_enum_valid seed=10 target=native-jsonl profile=async mutation=nested_enum_unchecked: nested call 1/6/code-2 (lookup) records "blocked", but the tool returned [Ok(String("beta-44"))]: a result outside its text enum must be recorded as failed
replay: hl sim --target native-jsonl --seeds 10..11 --mutation nested_enum_unchecked examples/code_tasks.hl
```

A harness with a code tool dispatches a reply's calls with a sequential
`for`: a dispatch there may run a cell, whose nested calls, timers and journal
updates run one cell at a time, so the checker refuses a parallel for:

```console
$ hl check examples/invalid/code_parallel_for.hl
examples/invalid/code_parallel_for.hl:39:7: error: a parallel for dispatches tool calls at once, but code 'exec' makes a dispatch able to run a cell, which runs one at a time with its journal: use a sequential for
```

### Cells across calls

A cell may outlive the call that started it, as nanocode's `exec` and `wait`
do: `waits NAME` in a `code` declaration (a field like the others, once)
adds a second tool the model calls with nanocode's parameters, `cell_id: text`
and `terminate: bool` (default `false`). `docs/snippets/code_wait.hl` names it
`wait`, nanocode's name:

```hl docs/snippets/code_wait.hl
harness worker

const steps_max: u64 = 6
const calls_max: u64 = 3
const entries_max: u64 = 256
const cell_calls_max: u64 = 4
const cell_parallel_max: u64 = 2
const cell_timers_max: u64 = 2
const cell_timer_ms_max: u64 = 60000
const cell_journal_bytes_max: u64 = 1048576
const cell_yield_ms: u64 = 10000

type outcome = done(text) | exhausted

tool read(path: text) -> text effect none
tool bash(command: text) -> text effect external

code exec {
  tools read, bash
  calls cell_calls_max
  parallel cell_parallel_max
  timers cell_timers_max
  timer_ms cell_timer_ms_max
  journal_bytes cell_journal_bytes_max
  yield_ms cell_yield_ms
  waits wait
}

model infer calls calls_max

state {
  context: list<entry, entries_max>
}

invariant context_bounded: len(context) <= entries_max

on prompt(input: text) -> outcome
  requires len(context) + 1 + steps_max * (1 + calls_max) <= entries_max
{
  push context user(input)
  loop step below steps_max {
    let reply = infer(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return done(words) }
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

- A site of the code tool goes live with a deadline `yield_ms` after its clock.
  When the cell waits with no nested call running and its next timer fires after
  that deadline, the site returns at once instead of sleeping: its result is
  `ok("Script running with cell ID N")`, nanocode's text, and the cell stays
  unfinished, its journal written so far. Nothing runs between sites.
- A site of the wait tool replays that cell from its journal, as a rerun after a
  crash does, and runs it on with a deadline of its own: it ends as an `exec`
  site's cell ends, or returns "Script running" again. A timer whose time passed
  fires at once. `terminate: true` finishes the cell without running it and
  returns `ok("Script terminated")`. An id that is not a cell this turn started
  and left running gets `failed("exec cell ID not found")`.
- When the turn ends, by any return or a trap, every cell it left running gets
  its finished update before the turn's status, as nanocode's Codex terminates
  a turn's cells when the turn completes.

The wait tool's call record has effect `none`, like the code tool's, and a crash
before its result reruns it, replaying the cell again. The model reaches it
through `dispatch`; a handler cannot call it, and with `waits wait` a handler's
`wait(c)` stays the sleep of Waits:

```console
$ hl check docs/snippets/errors/wait-direct.hl
docs/snippets/errors/wait-direct.hl:27:9: error: wait tool 'collect' of code 'exec' is called by the model, through dispatch: a handler cannot call it
```

```hl docs/snippets/errors/wait-direct.hl
harness worker

const calls_max: u64 = 2
const cell_calls_max: u64 = 4
const cell_parallel_max: u64 = 2
const cell_timers_max: u64 = 2
const cell_timer_ms_max: u64 = 60000
const cell_journal_bytes_max: u64 = 1048576
const cell_yield_ms: u64 = 10000

tool read(path: text) -> text effect none

code exec {
  tools read
  calls cell_calls_max
  parallel cell_parallel_max
  timers cell_timers_max
  timer_ms cell_timer_ms_max
  journal_bytes cell_journal_bytes_max
  yield_ms cell_yield_ms
  waits collect
}

model infer calls calls_max

on run(cell: text) -> text {
  match collect(cell) {
    ok(output) => { return output }
    failed(error) => { return error }
    unknown => { return "lost" }
  }
}
```

A wait site counts as a cell site in the worst case (the cell's work over
several sites is counted at each), so `hl check` counts as before:

```console
$ hl check docs/snippets/code_wait.hl | sed -n 1,3p
docs/snippets/code_wait.hl: harness worker: 2 tool(s), 1 state field(s), 1 handler(s)
  handler prompt: worst case 132 effects, 441 journal records, 78 outbound requests
    effects: step below steps_max (6) x call below calls_max (3)
```

The simulator's cells sleep past `yield_ms` sometimes, and after a reply whose
cell yielded its model calls the wait tool with that id, now and then with
`terminate` or a wrong id; `yields` counts the cells that returned "Script
running". `cell_settled_at_turn_end` requires every cell to have its one
finished update before its turn's status in every durable journal, and the
prover proves the same of the plan, where a cell site may leave its cell
running and a wait site may finish it. The planted bug `cell_leaks` skips the
stops at the turn's end:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/code_wait.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=2059 crashes=1555 power_losses=98 torn_writes=138 takeovers=0 fenced=0 dispatches=1678 unknown=367 cells=401 nested=479 divergences=5 waits=68 yields=30
$ hl sim --target native-jsonl --seeds 0..200 --mutation cell_leaks docs/snippets/code_wait.hl
FAIL invariant=cell_settled_at_turn_end seed=29 target=native-jsonl profile=async mutation=cell_leaks: cell 1 of turn 1 has no finished update before the turn's status
replay: hl sim --target native-jsonl --seeds 29..30 --mutation cell_leaks docs/snippets/code_wait.hl
$ hl prove --target native-jsonl --mutation cell_leaks docs/snippets/code_wait.hl 2>&1 | grep -m 1 -o 'cell_settled_at_turn_end: [^;]*'
cell_settled_at_turn_end: dispatch(call): the turn's status is appended while its cell is still running
```

#### Stopping a cell

`interruptible by cancel` in a `code` declaration (the harness declares
`on cancel`) lets a cancel stop a running cell, as nanocode's stop does:

```hl docs/snippets/code_stop.hl
harness worker

const steps_max: u64 = 6
const calls_max: u64 = 3
const entries_max: u64 = 256
const cell_calls_max: u64 = 4
const cell_parallel_max: u64 = 2
const cell_timers_max: u64 = 2
const cell_timer_ms_max: u64 = 60000
const cell_journal_bytes_max: u64 = 1048576
const cell_yield_ms: u64 = 10000

type outcome = done(text) | stopped | exhausted

tool read(path: text) -> text effect none
tool bash(command: text) -> text effect external

code exec {
  tools read, bash
  calls cell_calls_max
  parallel cell_parallel_max
  timers cell_timers_max
  timer_ms cell_timer_ms_max
  journal_bytes cell_journal_bytes_max
  yield_ms cell_yield_ms
  waits wait
  interruptible by cancel
}

model infer calls calls_max

on cancel

state {
  context: list<entry, entries_max>
}

invariant context_bounded: len(context) <= entries_max

on prompt(input: text) -> outcome
  requires len(context) + 1 + steps_max * (1 + calls_max) <= entries_max
{
  push context user(input)
  loop step below steps_max {
    let reply = infer(context)
    push context assistant(reply)
    match reply {
      answer(words) => { return done(words) }
      calls(requested) => {
        for call in requested {
          match dispatch(call) {
            ok(output) => { push context result(call, output) }
            failed(error) => { push context error(call, error) }
            unknown => { push context unknown(call) }
          }
        }
        if cancelled { return stopped }
      }
    }
  }
  return exhausted
}
```

- The runtime takes the turn's events after each nested call returns and after
  each timer wait of a cell, at the site's sequence number, as it does after
  an interruptible effect. A cancel recorded there stops the cell: no nested
  call starts after it, the cell gets its finished update, and the site's
  result is `ok` with nanocode's `StartedToolCall::STOPPED` text ("Tool
  execution stopped by the user. The cell was terminated and no nested tool
  call was left running.").
- When the cancel arrived during an external nested call, that call's result
  is not recorded: a `recovery` record reports it unknown, as for an
  abandoned external tool, and the site's text is `STOPPED_UNKNOWN` ("...
  terminated while a nested tool call was still running; that call's outcome
  is unknown. Do not assume it did not run or automatically repeat it.").
- An exec or wait site that would start while a cancel is recorded runs
  nothing and returns the `STOPPED` text; a wait site still finishes the
  cell it names. A site rerun after a crash goes on with its cell, which stops
  before it starts anything new. On Vercel, whose events enter at step
  boundaries, a cancel taken at the flush before an external nested call
  stops the cell when that call returns, which is then reported unknown.

Only a cancel stops a cell:

```console
$ hl check docs/snippets/errors/code-interrupt-steer.hl
docs/snippets/errors/code-interrupt-steer.hl:22:28: error: code 'exec' is interruptible by 'cancel' only: a cancel stops its cell, which nothing else does
```

```hl docs/snippets/errors/code-interrupt-steer.hl
harness worker

const calls_max: u64 = 2
const cell_calls_max: u64 = 4
const cell_parallel_max: u64 = 2
const cell_timers_max: u64 = 2
const cell_timer_ms_max: u64 = 60000
const cell_journal_bytes_max: u64 = 1048576
const cell_yield_ms: u64 = 10000
const steers_max: u64 = 2

tool read(path: text) -> text effect none

code exec {
  tools read
  calls cell_calls_max
  parallel cell_parallel_max
  timers cell_timers_max
  timer_ms cell_timer_ms_max
  journal_bytes cell_journal_bytes_max
  yield_ms cell_yield_ms
  interruptible by cancel, steer
}

model infer calls calls_max

on cancel
on steer(message: text) bound steers_max

on run(script: text) -> text {
  match exec(script) {
    ok(output) => { return output }
    failed(error) => { return error }
    unknown => { return "lost" }
  }
}
```

`interrupt_honored` counts a stopped site as interrupted, so a site that
records any other result after a cancel fails it, and the prover's plan gives
every interruptible cell site a stop arm (a recovery record when its tools
are external, then the finished update). The planted bug `cell_cancel_ignored`
lets cells run on after a cancel:

```console
$ hl sim --target native-jsonl --seeds 0..200 docs/snippets/code_stop.hl
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=1978 crashes=1474 power_losses=100 torn_writes=140 takeovers=0 fenced=0 dispatches=1481 unknown=334 cells=359 nested=403 divergences=5 waits=63 events=44 interrupted=18 yields=23
$ hl sim --target native-jsonl --seeds 0..200 --mutation cell_cancel_ignored docs/snippets/code_stop.hl
FAIL invariant=interrupt_honored seed=12 target=native-jsonl profile=async mutation=cell_cancel_ignored: 2/4 is interruptible by cancel and an event that interrupts it was recorded before its outcome, yet it records a finished outcome
replay: hl sim --target native-jsonl --seeds 12..13 --mutation cell_cancel_ignored docs/snippets/code_stop.hl
```
