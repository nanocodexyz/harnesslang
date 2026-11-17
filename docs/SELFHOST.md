# Self-hosting the harnesslang compiler

## Terms

- **harnesslang** programs are harnesses: handlers that call tools and
  models and keep state fields, described in docs/LANGUAGE.md. Each effect
  (a tool or model call, a state change, a clock read) writes a **journal
  record**, so a turn can be replayed after a crash.
- **hl** is the Rust compiler and interpreter (`hl check`, `hl run`, `hl
  emit`). **hl-host** (`hosts/native`) is the fixed native host: it runs a
  compiled WebAssembly module and answers its tool calls (reading files,
  running commands, calling models).
- **The pipeline** is parse (`src/syntax`), check (`src/check`), lower to IR
  (`src/ir`), prove (`src/prove`), lower the IR to WebAssembly text (wat),
  assemble and shake it (`src/emit/wasm`) and encode it to bytes.
- **Phase 1** is the set of language features that landed with this plan to
  make step 1 possible; it is listed under Porting order.
- **The flat module** is the assembled module written as hex in a plain
  little-endian layout (`hl emit --format flat`), the input step 1 reads until
  the assembler is ported.
- **The 295 modules** are every example and snippet compiled for each target
  that lowers it to WebAssembly (`native-jsonl`, `native-sqlite`,
  `server-postgres`, `cloudflare`, `vercel`).

## Goal

Today the compiler is Rust. The goal is a compiler written in harnesslang and
compiled to WebAssembly by itself. Rust stays in three places:

- **stage0**, today's Rust compiler, frozen as the bootstrap.
- **The fixed hosts** (`hosts/native` and the platform hosts), which give a
  compiled module its files, clock and network.
- **The wasm engine** (`src/wasm`), which runs every compiled module.

The language's core property does not change: everything is bounded, and what
can be proved is proved. A compiler is a bounded program over a source of at
most N bytes:

- Trees are arenas. A node is a `u64` index into a fixed-capacity list, never
  a recursive type.
- Recursion becomes an explicit stack: a fixed-capacity list with a named
  bound.
- Every capacity is a named const derived from N (tokens per byte, nodes per
  token, functions per module).
- Input past a capacity is a named failure, never a truncation.

Compiler code is pure compute. A pure fn, a `var` and a `bytes<N>` buffer touch
no tool, model, state field, event or clock, so the compiler writes no journal
record while it computes. Only the compile entry's two host calls, reading the
source and writing the module, are effects.

## Bootstrap

```
stage0  the Rust compiler, frozen     compiles compiler/*.hl -> stage1.wasm
stage1  stage1.wasm, built by stage0  compiles compiler/*.hl -> stage2.wasm
stage2  stage2.wasm, built by stage1  must equal stage1.wasm byte for byte
```

- **stage0** is today's Rust compiler, frozen at the commit where stage1 first
  builds the whole compiler. It stays buildable and takes only the fixes that
  keep it building `compiler/*.hl`. It moves to its own directory and leaves
  the live Rust count.
- **stage1** is `compiler/*.hl` compiled by stage0 to wasm and checked in.
- **stage2** is the same source compiled by stage1, run under hl-host's
  compile entry.
- **The fixed point:** stage2 must be byte-identical to stage1. A gate script
  builds both and fails on any difference, which also proves the compiler is
  deterministic.

After the switch, a language feature lands in three steps:

1. The compiler learns the feature, written using only features stage1
   already has.
2. stage1 compiles that source into stage1'. Programs may now use the
   feature.
3. stage1' compiles itself, and the result must equal stage1'. stage1' is
   checked in as the new stage1, with a DECISIONS.md entry. Only then may
   `compiler/*.hl` itself use the feature.

The checked-in stage1 is the trust root. If it is ever lost, stage0 rebuilds
the first stage1 from the source at the freeze commit, and replaying each
later re-bless from DECISIONS.md, the repository's log of decisions, rebuilds
the current one.

## How each step is tested

Each step ports one Rust module to one `compiler/*.hl` file and is tested
against the Rust module on every module and program the repo builds: every
example and snippet on every target, under both the interpreter (`hl run`) and
the compiled wasm (hl-host). The two must produce identical output (bytes for
encoders, a canonical text dump for structures). The Rust module leaves the
live tree only when its .hl port has replaced it in the pipeline.

Step 1 shows the shape: `scripts/selfhost-encode.sh` emits the flat module of
all 295 modules the repo builds (`hl emit --format flat`), runs
`compiler/encode.hl` on each with `hl run --arg-file` and, compiled to
`encode.wasm`, under `hl-host run --handler compile`, and fails unless both
write exactly the bytes `encode.rs` writes. `check.sh` runs it on every
program.

## Porting order

Lines are Rust today, after phase 1.

1. **encode** (`src/emit/wasm/encode.rs`, 305 lines, done): the wasm binary
   encoder, almost entirely byte work. `compiler/encode.hl` is 355 lines and
   byte-identical on all 295 modules. Needed: phase 1.
2. **assemble** (`assemble.rs`, 1,074): wat text to a module. A tokenizer over
   bytes and name resolution. Needs: text-keyed lookup without maps (a sorted
   table with binary search, or open addressing in fixed lists), interning
   names into one bytes arena, and records or sums holding node indexes for
   instructions. Once it lands, encode takes the assembled module directly and
   the flat layout goes.
3. **shake** (`shake.rs`, 1,186): dead code removal, a reachability walk.
   Needs: an explicit work stack and index remapping tables.
4. **lower** (`lower.rs`, 1,016): IR to wat. Expression lowering recurses in
   Rust. Needs: an explicit work stack over expression nodes and a decimal
   formatter (expressible today; see docs/snippets/pure.hl).
5. **data** (`data.rs`, 650): static tables. Needs: bytes arenas and
   sorted-table lookup.
6. **IR** (`src/ir`, 3,062): the typed IR, validation and worst cases. Needs:
   arenas of sums holding node indexes, and larger statement bounds per pure
   fn.
7. **checker** (`src/check`, 9,274): names, types, effects and inlining, the
   largest port. Needs everything above.
8. **parser** (`src/syntax`, 2,591): lexer and recursive-descent parser.
   Needs: an explicit stack for nesting, interning and node arenas. It goes
   late because every other module can be tested on Rust-parsed input until
   then.
9. **prover** (`src/prove`, 6,828): last. Until it is ported, stage0's Rust
   prover proves the .hl compiler.

The rest of `src/emit` (1,344 lines of Rust, JavaScript and SQL source
emitters) is ported or retired with the counts that track it.

### Phase 1 (landed)

- Pure fns with a result, `fn f(x: T) -> U`: checked once, lowered to one
  function and called as an expression, out of line. A pure fn calls only the
  pure fns declared above it, so it never recurses. The checker refuses a
  tool, model, effect, clock, state field or event inside one.
- `var` locals and reassignment, and `loop i below N while c`, a bounded loop
  that stops early.
- `/` and `%`, hex literals (`0xff`) and byte literals (`'a'`).
- `bytes<N>`: `b[i]`, `set b[i] = v`, `push b x` (a byte or a buffer),
  `len`, `slice(b, from, to)`, `bytes_of(text)` and `text_of(bytes)`.
- Lists in pure fns: `xs[i]`, `set xs[i] = v`, `push` onto a `var` list, and
  `[]`.
- A compile entry: hl-host's `read_file` and `write_bytes` syscalls, `hl run
  --arg-file`, and `hl emit --format flat`.

Inside a pure fn, overflow, a zero divisor, an index past the end and a push
past a capacity are checked at run time and trap by name, identically in the
interpreter and in wasm. Termination is still proved by construction (named
loop bounds, fixed capacities, no recursion), and a handler's own arithmetic
is still proved.

### Later features (estimates)

Each is a guess until its step starts, and each lands as its own change.

- Proof obligations inside pure fns: index below length, divisor at least
  one, push below capacity, discharged by the prover from loop guards, as it
  already does for handler arithmetic. Phase 1 traps instead.
- Text-keyed lookup without maps, as a library of pure fns over bytes.
- Sums and small records holding `u64` node indexes, so arenas can be typed.
- Multiple results, or records as results.
- Moving a buffer into a call without a copy, so pushing one section onto
  another stops copying (most of encode.hl's time today).
- Larger per-fn statement bounds once compiler fns grow.

## What Rust remains

| Part | Lines today | At the end |
| --- | --- | --- |
| Compiler: `src/syntax`, `check`, `ir`, `emit`, `prove` | 27,814 | 0 |
| stage0, frozen | (the line above) | not counted, in its own directory |
| `src/wasm` engine | 7,515 | about 7,500 |
| `hosts/native` | 6,049 | about 6,000 |
| `src/wire`, `src/deploy` | 2,832 + 4,561 | the host side, about 4,000 (est.) |
| CLI and loader (`main.rs`, `load.rs`, `lib.rs`) | 993 | about 500 (est.) |

The interpreter (`src/run`, 10,172), the simulator (`src/sim`, 9,580),
`src/generate` (2,482) and `src/target` (934) are not part of this plan. The
interpreter is the executable specification and the simulator checks
everything else; they stay the independent oracle that makes the differential
tests and the fixed point meaningful. The target for live Rust outside them is
about 19,000 lines (est.), down from 72,962 for all of `src` and `hosts` today.

## The gate

`scripts/gate.sh` counts `rust_compiler_lines`: the lines of tracked Rust
under `src/syntax`, `src/check`, `src/ir`, `src/emit` and `src/prove`. It was
26,125 at the start of this plan and is 27,814 after phase 1, which added the
language features the port needs.

It ratchets like the other counts: a change may not raise it. A change that
lands a language feature the port needs may raise it, and records the new
value in `gate.txt` and the reason in its DECISIONS.md entry. A change that
ports a module and deletes its Rust lowers it with `scripts/gate.sh --write`.
After the switch it is 0, and the gate also compares stage1 and stage2.

## Summary

| Step | Rust module | Rust lines | .hl file | Needs | Status |
| --- | --- | --- | --- | --- | --- |
| 1 | `emit/wasm/encode.rs` | 305 | `compiler/encode.hl` (355) | phase 1 | done, 295/295 modules identical |
| 2 | `emit/wasm/assemble.rs` | 1,074 | `compiler/assemble.hl` | lookup, interning, index sums | next |
| 3 | `emit/wasm/shake.rs` | 1,186 | `compiler/shake.hl` | work stack, remap tables | planned |
| 4 | `emit/wasm/lower.rs` | 1,016 | `compiler/lower.hl` | work stack | planned |
| 5 | `emit/wasm/data.rs` | 650 | `compiler/data.hl` | bytes arenas | planned |
| 6 | `src/ir` | 3,062 | `compiler/ir.hl` | typed arenas | planned |
| 7 | `src/check` | 9,274 | `compiler/check.hl` | all above | planned |
| 8 | `src/syntax` | 2,591 | `compiler/parse.hl` | stack, interning | planned |
| 9 | `src/prove` | 6,828 | `compiler/prove.hl` | all above | planned |
## How fast compiled .hl runs

`compiler/bench` times `compiler/encode.hl` compiled for hl-host against
encode.rs, in process, on examples/claude_code.hl (145,064 flat bytes in,
137,209 module bytes out), and checks that the bytes match. Best of 20 on a
hosted Hand:

```console
$ cargo build --release --locked --manifest-path compiler/bench/Cargo.toml
$ hl emit --format wasm --target native-jsonl compiler/encode.hl > encode.wasm
$ compiler/bench/target/release/selfhost-bench examples/claude_code.hl encode.wasm
```

| | encode.hl handler | over encode.rs (0.054 ms) | hl-host wall |
| --- | --- | --- | --- |
| before (061d508) | 300 ms | 5,578x | 440 ms |
| after | 13.8 ms | 251x | 47 ms |

Startup is outside those: decoding and compiling the encoder's wasm takes
1.4 ms in process, and an hl-host run of an empty handler takes 9 ms wall,
process start and journal included.

What it took, in the order the profile showed it:

- Hex. Over 95% of executed wasm ops were the .hl hex loops. `bytes_of_hex`
  and `hex_of` are now pure builtins, done by the runtime in two table loops
  that the engine fuses into one instruction each (Inst::Unhex, Inst::Hex),
  as it already fused byte scans and crc.
- The engine. Scan, crc and the hex loops run over a slice when every access
  is in bounds, and `write_string` copies runs of plain bytes at once.
- Checks a pure fn proves away (see Pure fns in LANGUAGE.md): unchecked add,
  multiply, divide and remainder where the operands' largest values allow, a
  push without the byte-width check, and no halt check after an expression
  that cannot trap.

Copies on push were not a cost: a push copies only a shared buffer and grows
in place otherwise.

The 20x goal is not met, and an interpreter will not meet it: the handler
still dispatches about 2.1 million wasm instructions, which take about 8 ms
at the engine's speed, against 0.054 ms of native code. What remains, by
size: the dispatch loop itself (half the time; closing the gap needs
compiling wasm to native code, or the encoder linked as native code); the
journal carrying the 290 KB hex input and output, which the runtime crc's,
escapes, parses, re-encodes and compares byte for byte (about a quarter of
the dispatched instructions are that comparison); and bytes copied into and
out of buffers. A raw bytes argument and result for pure entries and tools,
which the hosts must carry, removes the hex and halves the journal work.


## Beyond the compiler: what else moves to .hl

The compiler is not the only Rust that a .hl program could replace. Ordered by
value: the first item removes code kept twice and kept in step by hand, the
last only shortens tooling. Line counts are measured at the commit that adds
this section; (est.) marks an estimate of what the .hl version costs.

| # | What | Lines it removes | Needs |
| --- | --- | --- | --- |
| 1 | Code implemented twice: wire codec, JSON, journal | 8,485 | an embedding of pure .hl in the interpreter, recursion-free trees, raw bytes in and out |
| 2 | Pure text work left in hosts | about 380 | pure entries a host calls, text builtins |
| 3 | The compiler | 21,266 | the porting order above |
| 4 | The prover | 6,828 | the compiler's features, plus rationals or bounded big integers |
| 5 | The rest of the .wat runtime | 6,002 | effects and imports declared in .hl, control of memory layout |
| 6 | Tooling as .hl programs | about 10,000 | file and process tools, a const-from-file include |

1. **Code implemented twice.** Each of these exists once in Rust for the
   interpreter and once in .wat for compiled programs, and wasm-diff is what
   keeps the two in step:
   - the wire codec: `src/wire` (2,936, of which `diff.rs` is 775) and
     `runtime/wire.wat` (1,965);
   - JSON: `src/run/json.rs` (398) and `runtime/json.wat` (520), with
     `src/wire/json.rs` (342, counted above) a third copy;
   - journal encode and fold: `src/run/journal.rs` (908),
     `src/run/cell_journal.rs` (636) and `src/run/crc.rs` (34), against
     `runtime/journal.wat` (675) and `runtime/cell_journal.wat` (413).

   Together 8,485 lines (4,912 Rust and 3,573 .wat), which
   would become one .hl implementation of about 3,000 lines (est.) that the
   interpreter runs and the compiler compiles. It needs: the interpreter
   calling a pure .hl fn from Rust (an embedding, the reverse of today's
   `hl run`); JSON trees without recursion (an arena of nodes and an explicit
   work stack, the same `bytes` arenas the compiler port needs); decimal
   u64 text and UTF-16 escapes; the crc table as a const; and bytes in and
   out of a pure entry without hex. `bytes_of_hex` and `hex_of` make hex
   cheap today (see How fast compiled .hl runs above) but a raw bytes argument and
   result, which the hosts must carry, removes the doubling.
2. **Pure text work in hosts.** Lane 23 moved SSE splitting into the wasm
   wire codec and Bash and codex exec rendering into .hl pure fns, which cut
   hosts from 7,787 to 7,573 lines. What is left: the apply_patch grammar
   (`hosts/native/src/codex.rs`, 195 lines of parsing and matching from
   `parse_patch` to `updated`, 315 with its file writes), Read paging
   (`first_page` in `hosts/native/src/tools.rs`), stall-line detection
   (`prompting` in `hosts/native/src/jobs.rs`) and `${VAR}` expansion
   (`wireExpand` in `hosts/cloudflare.mjs` and `hosts/vercel.mjs`, 20 lines
   each). DECISIONS.md says why each of the first three stays for now: Read
   paging rounds f64 twice per step, stall detection decides what the
   journal admits, and apply_patch needs every touched file's contents, so
   moving it is a new tool protocol. They need: an f64-exact integer
   formulation or a decimal type, and a read-then-write tool shape.
3. **The compiler.** `src/syntax` (2,591), `src/check` (9,289), `src/ir`
   (3,072) and `src/emit` (6,314): 21,266 lines, by the porting order above.
4. **The prover.** `src/prove` (6,828). It needs everything the compiler
   does, and its arithmetic (bounds and sums of rationals or wide integers)
   needs either a bounded big-integer type or rationals built from u64 pairs.
5. **The rest of the .wat runtime.** `src/emit/wasm/runtime` holds 9,575
   lines of hand-written .wat; less item 1's 3,573 that leaves 6,002 (value
   and text helpers, the engine, cells, events, blobs, jobs, forks). Written
   in .hl, the compiler would compile its own runtime and the shake would see
   one language. It needs what pure fns lack today: declared host imports,
   effects and globals, and control of memory layout (the value encoding
   every host reads).
6. **Tooling as .hl programs.** The fuzz generator `src/generate` (2,482);
   deploy and bundle `src/deploy` (4,561, of which `http.rs`, `websocket.rs`,
   `sha1.rs` and `multipart.rs`, 973, are the host side); CLI dispatch
   `src/main.rs` (926); the eval driver and scorer `evals/*.py` (1,645); and
   `scripts/program-literals.py` (636), which a const-from-file include
   (`const prompt: text = include("prompts/x.md")`) replaces outright. About
   10,000 lines; these are programs that read files, run processes and talk
   HTTP, so they need the tool and file effects harness programs already
   have, run under the native host.
