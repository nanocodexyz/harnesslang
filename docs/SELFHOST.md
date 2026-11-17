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
