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
  proved handler prompt: no_overflow: 1 arithmetic operation(s) proved in range; invariant_preserved: 0 invariant check(s) at returns and trap points; push_bound: 2 push(es) proved below capacity (14 prover steps)
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
  proved handler prompt: no_overflow: 5 arithmetic operation(s) proved in range; invariant_preserved: 6 invariant check(s) at returns and trap points; push_bound: 5 push(es) proved below capacity (4098 prover steps)
  proved handler prompt on cloudflare: durability: 1 external effect site(s) run only after a flush of their call and every record before it; 10 acknowledgement point(s) follow their status flush
  proved handler prompt on cloudflare: worst_case: 2115 journal records per run on the plan's longest path, as hl check counts (step below turn_steps_max (64) x call below calls_per_step_max (16))
  proved handler prompt on cloudflare: worst_case: 1088 outbound requests <= 10000 outbound requests per invocation (step below turn_steps_max (64) x call below calls_per_step_max (16))
examples/agent.hl: proved for cloudflare
target cloudflare: runs in a Durable Object; journal: the object's SQLite; flush: OutputGate; atomic unit: Event; who else may run: AlarmAfterReset
  limit memory: 134217728 bytes per isolate (developers.cloudflare.com/durable-objects/platform/limits)
  limit outbound requests: 10000 outbound requests per invocation (developers.cloudflare.com/workers/platform/limits (Workers Paid default))
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
ok target=native-jsonl mutation=none seeds=0..200 (200 seeds): turns=504 runs=2776 crashes=2272 power_losses=191 torn_writes=323 takeovers=0 fenced=0 dispatches=7785 unknown=746
```

Each invariant has a planted bug the simulator must catch. Here recovery sends
open external calls again instead of reporting them unknown:

```console
$ hl sim --target native-jsonl --seeds 0..200 --mutation recovery_redispatches_external examples/agent.hl
FAIL invariant=external_once seed=0 target=native-jsonl profile=errors mutation=recovery_redispatches_external: external call 1/2 (spawn_agent) was dispatched 5 times
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

Keywords: `harness` `const` `type` `tool` `model` `state` `on` `let` `push` `if` `else` `match` `loop` `for` `return` `true` `false` `and` `or` `not` `list` `effect` `invariant` `requires`

Words that are keywords only where the grammar expects them, and names
elsewhere:

Contextual words: `below` `calls` `in` `known` `maybe`

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
item       = const | type | tool | model | state | invariant | handler ;
const      = "const" name ":" type_expr "=" number ;
type       = "type" name "=" variant { "|" variant } ;
variant    = name [ "(" type_expr { "," type_expr } ")" ] ;
tool       = "tool" name params "->" type_expr "effect" effect_class ;
effect_class = "none" | "idempotent" | "external" ;
model      = "model" name "calls" bound ;
state      = "state" "{" { name ":" type_expr } "}" ;
invariant  = "invariant" name ":" expr ;
handler    = "on" name params "->" type_expr [ "requires" expr ] block ;
params     = "(" [ name ":" type_expr { "," name ":" type_expr } ] ")" ;
bound      = name ;
type_expr  = "list" "<" type_expr "," bound ">"
           | "known" "<" type_expr ">"
           | "maybe" "<" type_expr ">"
           | name ;
block      = "{" { statement } "}" ;
statement  = "let" name "=" expr
           | "push" name expr
           | if
           | "match" expr "{" { arm [ "," ] } "}"
           | "loop" name "below" bound block
           | "for" name "in" expr block
           | "return" expr
           | name "=" expr ;
if         = "if" expr block [ "else" ( if | block ) ] ;
arm        = name [ "(" name { "," name } ")" ] "=>" block ;
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
its values would have no bound.

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
may call and the program may call directly. It returns `u64`, `bool` or
`text`, so that every target and the simulator can carry its result. The class
is `none`, `idempotent` or `external` ([Effects](#effects)).

```console
$ hl check docs/snippets/errors/tool-returns.hl
docs/snippets/errors/tool-returns.hl:7:26: error: a tool returns u64, bool or text
```

```hl docs/snippets/errors/tool-returns.hl
harness notes

const calls_max: u64 = 4

model think calls calls_max

tool plan(goal: text) -> reply effect none

on ping() -> u64 { return 1 }
```

### model

`model name calls bound` declares the model. Calling it, `name(context)`, takes a
`list<entry, N>` and returns a `reply`; `bound` caps the tool calls in one
reply, so `calls(requested)` binds a `list<call, bound>`. A reply with more
calls, or one that does not decode, traps the turn. The `reply` and `entry` types
exist only when a model is declared:

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
across turns. Fields start at zero (`0`, `false`, empty text, an empty list),
so a field is `u64`, `bool`, `text` or a list. State is not journaled: it is a
function of the journal, rebuilt by replay from the latest checkpoint.

```console
$ hl check docs/snippets/errors/state-sum.hl
docs/snippets/errors/state-sum.hl:6:12: error: state field 'current' has no initial value: state fields are u64, bool, text or lists
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

## Types

| Type | Values | Operations |
| --- | --- | --- |
| `u64` | 0 to 18446744073709551615 | `+ - *` (trap on overflow and underflow), `< <= > >=`, `== !=` |
| `bool` | `true`, `false` | `and`, `or`, `not`, `== !=` |
| `text` | UTF-8 text | `++` (concatenate), `== !=` |
| a declared sum | its variants | construct, `match` |
| `list<T, N>` | up to `N` values of `T` | `push` (state fields only), `for`, `len(list)` |
| `known<T>` | `ok(T)`, `failed(text)` | `match` |
| `maybe<T>` | `ok(T)`, `failed(text)`, `unknown` | `match`, which must have an `unknown` arm |

Builtin sum types:

- `call = call(text, text, text)`, read with `.id`, `.tool` and `.input`.
- `reply = answer(text) | calls(list<call, M>)`, where `M` is the model's `calls`.
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
  a state field's, constant's, tool's or the model's name.
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

## Statements

| Statement | Meaning |
| --- | --- |
| `let x = e` | bind a new local |
| `field = e` | assign a state field |
| `push field e` | append to a state list; a full list traps with `bound(field)` |
| `if c { } else if c { } else { }` | `c` is `bool` |
| `match e { variant(a, _) => { } ... }` | one arm per variant, no more, no fewer; arms may be separated by commas |
| `loop i below N { }` | runs the body with `i` = 0 to N - 1, `N` a constant; there is no `break` |
| `for x in list { }` | runs the body once per element of the list as it was when the loop started |
| `return e` | finish the turn with `e`, of the handler's result type |

There is no `while`, no recursion and no user function: a handler is
straight-line code over bounded loops, so its worst case is a number.

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

## Effects

An effect is anything whose result the program cannot recompute: a tool, the
model, `now()` and `random()`. Each effect site is journaled. When a handler
runs again after a crash, each site takes the next sequence number in its turn
and, if the journal holds its result, returns that result without running.
Effects inside an expression run before the expression, left to right.

| Class | Declared by | Result | When a crash lost its result |
| --- | --- | --- | --- |
| `none` | `effect none`, `now()`, `random()` | `known<T>` for a tool; `now()` and `random()` give a plain `u64` | runs again |
| `idempotent` | `effect idempotent`, the model | `known<T>` for a tool; the model gives a plain `reply` | runs again with the same turn and sequence number, its idempotency key |
| `external` | `effect external`, a tool the program does not declare | `maybe<T>` | never runs again: the result is `unknown` |

These are nanocode's `EffectClass`. `dispatch(call)` looks `call.tool` up among
the declared tools at run time, defaults an undeclared tool to `external`, and
always has type `maybe<text>`; a none or idempotent tool reached through it
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

### Traps

A turn ends `failed` without running further when `requires` is false
(`requires(handler)`, state unchanged), a `u64` operation overflows or
underflows, a `push` finds its list full (`bound(list)`), or a model reply does
not decode. A trap still writes `status` and is acknowledged once. The prover
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
| nesting of blocks, types, expressions | `NESTING_DEPTH_MAX` = 64 | policy |
| prover steps per handler | `PROOF_STEPS_PER_HANDLER_MAX` = 4,000,000 | policy; reaching it fails the obligation and says so |
| constraints per prover step | `PROOF_CONSTRAINTS_MAX` = 4,096 | policy; same |
| nesting of a journal's JSON | `JSON_DEPTH_MAX` = 128 | policy |
| Cloudflare outbound requests per invocation | `CLOUDFLARE_SUBREQUESTS_PER_INVOCATION_MAX` = 10,000 | Workers Paid default, developers.cloudflare.com/workers/platform/limits |
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
| `hl sim --target T --seeds A..B [--twice] [--mutation M] FILE` | simulate seeds on a target's fault model; `--twice` requires identical journals from two runs |
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
