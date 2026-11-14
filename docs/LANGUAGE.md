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
  proved handler prompt: no_overflow: 5 arithmetic operation(s) proved in range; invariant_preserved: 6 invariant check(s) at returns and trap points; push_bound: 5 push(es) proved below capacity (4202 prover steps)
  proved handler prompt on cloudflare: durability: 1 external effect site(s) run only after a flush of their call and every record before it; 10 acknowledgement point(s) follow their status flush
  proved handler prompt on cloudflare: worst_case: 2115 journal records per run on the plan's longest path, as hl check counts (step below turn_steps_max (64) x call below calls_per_step_max (16))
  proved handler prompt on cloudflare: worst_case: 1088 outbound requests <= 10000 outbound requests per invocation (step below turn_steps_max (64) x call below calls_per_step_max (16))
examples/agent.hl: proved for cloudflare
target cloudflare: runs in a Durable Object; journal: the object's SQLite; flush: OutputGate; atomic unit: Event; who else may run: AlarmAfterReset
  limit memory: 134217728 bytes per isolate (developers.cloudflare.com/durable-objects/platform/limits)
  limit outbound requests: 10000 outbound requests per invocation (developers.cloudflare.com/workers/platform/limits (Workers Paid default))
  limit concurrent requests: 6 requests waiting for response headers per invocation (developers.cloudflare.com/workers/platform/limits (simultaneous open connections; relaxed to the wait for headers on 2026-04-09); a seventh fetch is queued by the runtime)
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

`hl emit` prints the program for a target. For `native-jsonl` it is one Rust
file using only std, which a host drives through `run_jsonl`:

```console
$ hl emit --target native-jsonl examples/agent.hl | grep -E -A 4 '^pub fn run_jsonl'
pub fn run_jsonl<H: Host>(
    host: &mut H,
    disk: &mut dyn Disk,
    request: &TurnRequest,
) -> Result<Ran, RunError> {
```

[Targets](#targets) lists what every target generates.

## Lexical structure

A source file is UTF-8 text. Whitespace (space, tab, carriage return, newline)
separates tokens and is otherwise ignored. There are no comments.

- **Names**: an ASCII letter or `_`, then ASCII letters, digits and `_`. Names
  starting with `_` are reserved for the compiler; a lone `_` in a pattern
  ignores a field.
- **Numbers**: decimal digits, with `_` allowed between them (`1_000_000`).
  A number must fit in `u64` and may not be followed by a letter. There are
  no negative, fractional or hexadecimal literals.
- **Text**: between double quotes, on one line. The escapes are `\"`, `\\`
  and `\n`; any other backslash is an error.
- **Symbols**, longest match first:

Symbols: `{` `}` `(` `)` `<` `<=` `>` `>=` `,` `:` `=` `==` `!=` `->` `=>` `+` `++` `-` `*` `.` `|`

Keywords, which cannot be used as names:

Keywords: `harness` `const` `type` `tool` `model` `state` `on` `let` `push` `clear` `keep` `if` `else` `match` `loop` `for` `return` `true` `false` `and` `or` `not` `list` `effect` `invariant` `requires`

Words that are keywords only where the grammar expects them, and names
elsewhere:

Contextual words: `below` `calls` `fn` `from` `in` `known` `last` `listing` `maybe` `parallel` `reply` `tools`

Builtin functions, whose names declarations may not take:

Builtins: `dispatch` `len` `now` `random`

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
item       = const | type | tool | listing | model | state | invariant | function | handler ;
const      = "const" name ":" type_expr "=" number ;
type       = "type" name "=" ( variant { "|" variant } | text { "|" text } ) ;
variant    = name [ "(" type_expr { "," type_expr } ")" ] ;
tool       = "tool" name params "->" type_expr "effect" effect_class ;
effect_class = "none" | "idempotent" | "external" ;
listing    = "tools" "from" "listing" "(" name "," bound ")" params "effect" effect_class ;
model      = "model" name "calls" bound [ "reply" name ] ;
state      = "state" "{" { name ":" type_expr } "}" ;
invariant  = "invariant" name ":" expr ;
function   = "fn" name params block ;
handler    = "on" name params "->" type_expr [ "requires" expr ] block ;
params     = "(" [ name ":" type_expr { "," name ":" type_expr } ] ")" ;
bound      = name ;
type_expr  = "list" "<" type_expr "," bound ">"
           | "known" "<" type_expr ">"
           | "maybe" "<" type_expr ">"
           | name ;
block      = "{" { statement } "}" ;
statement  = "let" name [ ":" type_expr ] "=" expr
           | "push" name expr
           | "clear" name
           | "keep" name "last" expr
           | if
           | "match" expr "{" { arm [ "," ] } "}"
           | "loop" name "below" bound block
           | "for" name "in" expr [ "parallel" ] block
           | "return" expr
           | name "=" expr
           | name "(" [ expr { "," expr } ] ")" ;
if         = "if" expr block [ "else" ( if | block ) ] ;
arm        = ( name [ "(" name { "," name } ")" ] | text ) "=>" block ;
expr       = and { "or" and } ;
and        = not { "and" not } ;
not        = "not" not | comparison ;
comparison = sum [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) sum ] ;
sum        = product { ( "+" | "-" | "++" ) product } ;
product    = postfix { "*" postfix } ;
postfix    = primary { "." name } ;
primary    = number | text | "true" | "false" | "(" expr ")"
           | name [ "(" [ expr { "," expr } ] ")" ] ;
```

Notes the grammar leaves out:

- Items may come in any order and refer to each other in any order, but a
  harness has at most one `model` and one `state` block and at least one
  handler.
- Operators bind, loosest first: `or`, `and`, `not`, comparisons, `+ - ++`,
  `*`, `.`. Binary operators are left associative; a comparison does not chain
  (`a < b < c` is a parse error).
- `known` and `maybe` start a type only when `<` follows. A field name after
  `.` may be any name.
- A `bound` is a name: a number there is an error that names the number.
- Nesting of blocks, types and expressions is bounded by
  `NESTING_DEPTH_MAX` = 64 (policy: deeper than any harness, shallow enough for
  the default thread stack); deeper input is an error, not a crash.

## Declarations

### harness

`harness name` opens the file. The name labels the journal's checkpoints and
the generated code.

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

```console
$ hl check docs/snippets/errors/tool-returns.hl
docs/snippets/errors/tool-returns.hl:7:26: error: a tool returns u64, bool, text or a text enum
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
FAIL invariant=tool_admitted seed=0 target=native-jsonl profile=errors mutation=listing_unchecked: listed call 2/54 (mcp__beta) ran as Some("beta-v37") while listing mcp admitted None in turn 2: a tool that changed since admission must be refused
$ hl sim --target native-jsonl --seeds 0..200 --mutation input_unchecked examples/codex_agent.hl | head -n 1
FAIL invariant=tool_admitted seed=0 target=native-jsonl profile=errors mutation=input_unchecked: call 1/7 (steer_waiting) ran with input "{\"eta-51\":true}", which does not fit its declared input: not a JSON object
$ hl sim --target native-jsonl --seeds 0..200 --mutation listed_class_dropped examples/codex_agent.hl | head -n 1
FAIL invariant=external_once seed=0 target=native-jsonl profile=errors mutation=listed_class_dropped: external call 1/18 (mcp__beta) was dispatched 4 times
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
docs/snippets/errors/listing-arguments.hl:8:9: error: listing 'docs' takes 0 argument(s), found 1
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
$ for target in native-jsonl native-sqlite server-postgres cloudflare vercel; do hl emit --target $target docs/snippets/fn_inline.hl > output/fn_inline.out; hl emit --target $target docs/snippets/fn.hl | cmp - output/fn_inline.out && echo "$target: identical"; done
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

## Types

| Type | Values | Operations |
| --- | --- | --- |
| `u64` | 0 to 18446744073709551615 | `+ - *` (trap on overflow and underflow), `< <= > >=`, `== !=` |
| `bool` | `true`, `false` | `and`, `or`, `not`, `== !=` |
| `text` | UTF-8 text | `++` (concatenate), `== !=` |
| a declared sum | its variants | construct, `match` |
| a text enum | its listed texts | a text literal where it is expected, `match` |
| `list<T, N>` | up to `N` values of `T` | `push` (state fields only), `for`, `len(list)` |
| `known<T>` | `ok(T)`, `failed(text)` | `match` |
| `maybe<T>` | `ok(T)`, `failed(text)`, `unknown` | `match`, which must have an `unknown` arm |

Builtin sum types:

- `call = call(text, text, text)`, read with `.id`, `.tool` and `.input`.
- `reply = answer(text) | calls(list<call, M>)`, where `M` is the model's `calls`,
  unless the model declares its reply type, which then takes `reply`'s place.
- `entry = user(text) | assistant(reply) | result(call, text) | error(call, text) | unknown(call)`.

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
  values is refused: `hl run` and the generated code's argument decoding
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
FAIL invariant=enum_valid seed=6 target=native-jsonl profile=clean mutation=enum_unchecked: call 2/10 (stop_hook) records Ok(String("stop")), but the tool returned [Ok(String("beta-1"))]: a result outside its text enum must be recorded as failed
replay: hl sim --target native-jsonl --seeds 6..7 --mutation enum_unchecked examples/hooks.hl
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
  proved handler ask: no_overflow: 7 arithmetic operation(s) proved in range; invariant_preserved: 10 invariant check(s) at returns and trap points; push_bound: 5 push(es) proved below capacity (17036 prover steps)
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
(`crates/backends/codex/src/model/run/turn.rs`): its reply is
`message(text, bool) | calls(list<call, calls_per_round_max>)`, and a message
whose `end_turn` is false sends the context again instead of finishing; its
calls run with `parallel`, listed MCP tools included. Each
continuation spends one of `rounds_max` rounds, so the prover bounds it like
any other round and the worst case counts it.

The planted bug `model_failure_unrecorded` writes no record for a failed
call, as a harness that treats provider errors as transient might. A rerun
asks the model again, and the journal it leaves records a sequence number
the turn has already passed:

```console
$ hl sim --target native-jsonl --seeds 0..200 --mutation model_failure_unrecorded examples/claude_agent.hl
FAIL invariant=seq_unique seed=1 target=native-jsonl profile=errors mutation=model_failure_unrecorded: turn 3 records seq 1 after seq 9
replay: hl sim --target native-jsonl --seeds 1..2 --mutation model_failure_unrecorded examples/claude_agent.hl
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

## Statements

| Statement | Meaning |
| --- | --- |
| `let x = e` | bind a new local |
| `let x: T = e` | bind a new local of type `T`; `e` is checked against `T`, so a variant constructor takes its type from it |
| `field = e` | assign a state field |
| `push field e` | append to a state list; a full list traps with `bound(field)` |
| `clear field` | empty a state list |
| `keep field last n` | drop all but the last `n` entries of a state list, `n` a `u64`; a list of `n` entries or fewer is unchanged ([clear and keep](#clear-and-keep)) |
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

`examples/claude_agent.hl`, `codex_agent.hl`, `subagents.hl` and
`subagent_child.hl` compact this way before every model request, keeping
`compact_kept` entries. `claude_agent.hl` writes compaction once as a
[fn](#fn) and also calls it when the model reports the context window
exhausted, and `subagents.hl` also keeps the `agents_kept` most recent agents
of its roster when a child completes.

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
is one `match dispatch(call)` on the item it walks, whose arms only push
values that run no effect. A turn runs it in three steps:

1. In list order, each call takes the next sequence number and appends its
   `call` record (a call the journal already holds is replayed). Nothing is
   dispatched yet.
2. The calls are dispatched in groups, in list order. Consecutive none and
   idempotent calls form one group, dispatched at once; an external call is
   a group of its own, as nanocode runs writers exclusively. Before the first
   external call runs, one flush makes every `call` record of the batch
   durable (durability point 1, once for the whole batch). Each group's
   results are appended in list order, whatever order they completed in.
3. The arms run in list order with each call's result.

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
on something other than the item's dispatch, an arm that does more than push,
and a parallel for inside another.

```console
$ hl check docs/snippets/errors/parallel-statement.hl
docs/snippets/errors/parallel-statement.hl:11:9: error: the body of a parallel for is only a match on dispatch(call): found a let
$ hl check docs/snippets/errors/parallel-dispatch.hl
docs/snippets/errors/parallel-dispatch.hl:13:11: error: a parallel for matches on dispatch(call), the item it walks
$ hl check docs/snippets/errors/parallel-arm.hl
docs/snippets/errors/parallel-arm.hl:12:23: error: the arms of a parallel for only push: found a return
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
| prover steps per handler | `PROOF_STEPS_PER_HANDLER_MAX` = 4,000,000 | policy; reaching it fails the obligation and says so |
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

| Target | Runs in | Journal | Atomic unit | Who else may run | Recovery | Limits checked | Generated code |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `native-jsonl` | a process | an append-only JSONL file per session, a CRC-32 per line | one line | nobody | at load | the program's own | one std-only Rust file; the host implements `Host` (tools, model, clock, randomness, acknowledgement) and `Disk`, and calls `run_jsonl` per turn |
| `native-sqlite` | a process | SQLite rows through the host's `Sql` | one transaction | nobody | at load | the program's own | one std-only Rust file; the host implements `Host` and `Sql` and calls `run_sql` |
| `server-postgres` | a server pool | Postgres rows through the host's `Sql` | one transaction, fenced by owner epoch | a second owner after a lease expires | at load, by the new owner | the program's own | as `native-sqlite`, with the epoch fence in every flush |
| `cloudflare` | a Durable Object | the object's SQLite | one event | nobody; an alarm restarts the object after a reset | at load | outbound requests per invocation | one ES module: the `HarnessObject` Durable Object class and a Worker whose `fetch` routes each session to its object |
| `vercel` | a Function driving a Workflow run | the run's event log | one step | a retried step | in the step, by attempt number | none yet (duration is reported) | one ES module exporting `harness`, which holds the `"use workflow"` session and the `"use step"` step to bind |

```console
$ hl emit --target server-postgres examples/agent.hl | grep -E '^pub fn run_sql|^pub trait Sql'
pub trait Sql {
pub fn run_sql<H: Host>(host: &mut H, sql: &mut dyn Sql, journal: &mut SqlJournal, request: &TurnRequest) -> Result<Ran, RunError> {
$ hl emit --target cloudflare examples/agent.hl | grep -E '^export'
export { HarnessObject, harness };
export default worker;
$ hl emit --target vercel examples/agent.hl | grep -E '^export'
export { harness };
```

Generated programs use only their language's standard library and the
platform's own API. Every target's lowering is simulated by `hl sim` with its
own faults; `scripts/check.sh` sweeps every example on every target and runs
every planted bug, and `HL_ORACLE=1` and `HL_DRILL=1` add the nanocode oracle
and the kill -9 drill of generated code.

## Commands

| Command | Does |
| --- | --- |
| `hl check [--target T] FILE` | parse, type-check, report each handler's worst case and prove it; with a target, also check the target's limits |
| `hl prove [--target T] [--mutation M] FILE` | the proofs alone; a mutation plants a lowering bug the proofs must catch |
| `hl sim --target T --seeds A..B [--twice] [--strict [--any-state]] [--mutation M] FILE` | simulate seeds on a target's fault model; `--twice` requires identical journals from two runs; `--strict` also checks what the proofs promise (no trap but requires and model replies, invariants after every turn, the worst case within the target's limit), and `--any-state` starts each session from a drawn state that satisfies the invariants |
| `hl sim --exhaustive [--depth N] [--cuts lines\|bytes] --target T --seeds A..B [--mutation M] FILE` | explore every crash point of each seed's workload up to N faults per session; takes neither `--twice`, `--strict`, `--trace` nor `--dump-journal` |
| `hl run FILE --handler NAME [--arg VALUE]... [--seed S]` | run one turn in the interpreter and print the journal |
| `hl emit --target T FILE` | print the generated program |

`check`, `prove` and `sim` exit 1 when something fails and 2 on a usage error.

## Code mode (in progress)

Code mode lets the model answer with a JavaScript cell that calls the
harness's tools as nested calls, as nanocode's Code Mode does. A `code`
declaration names the tool, the tools a cell may call and the engine's
limits; a nested external call is journaled with the cell's header before it
runs and is never dispatched twice. It is specified in
[CODE_MODE.md](CODE_MODE.md) and being built; the parser does not accept
`code` yet, so this reference does not document it.
