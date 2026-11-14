# harnesslang

A language for agent harnesses, compiled to each platform's durability guarantees. A program says what the harness does; a target says where it runs and what that place promises when it crashes. The compiler checks the program against the target and emits the code that is best there, for [nanocode](https://github.com/nanocodexyz/nanocode) and anything like it.

| Target | Runs in | Journal |
| --- | --- | --- |
| `native-jsonl` | a process | an append-only JSONL file |
| `native-sqlite` | a process | SQLite |
| `server-postgres` | a server pool | Postgres, fenced by owner epoch |
| `cloudflare` | a Durable Object | the object's SQLite |
| `vercel` | a Vercel Function | a Workflow run's event log |

- **Unknown outcomes are types.** An external tool returns `ok`, `failed` or `unknown`, and a harness that ignores `unknown` does not compile.
- **Recovery is replay.** Every effect is journaled, so a handler re-run over its journal takes the same path and finishes the turn.
- **Bounded.** Every loop and list has a named bound, and the compiler checks each handler's worst case against the target's limits.
- **Simulated.** `hl sim` runs a harness on each target's fault model from a seed, and every invariant has a planted bug it must catch.
- **Zero dependencies.** The compiler and the code it emits use only their standard library.

## What harnesslang proves

`hl check` and `hl prove` prove, for every handler on every target and every path, crash and rerun paths included:

- **Durable before external.** An external effect runs only after its call record, and every record before it, is durable; recovery never runs an external call a crash left open again, and reports it `unknown`.
- **Finished once.** Every path, traps included, finishes in exactly one status, acknowledged only after the status is durable.
- **No traps in your code.** No `u64` arithmetic overflows and no `push` finds its list full, given the handler's `requires`.
- **Invariants.** Each `invariant` holds in the zero state and wherever a turn can end.
- **Within limits.** The worst case of outbound requests fits the platform's limit.

The method is a reachability walk over each target's model of the lowering and abstract interpretation over linear integer constraints, with a path and a counterexample on failure. What the prover assumes (the target models are true of the platform, and generated code does what the plan says, which the simulator checks) and what it does not prove (the generated code itself, memory, wall time) is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#proofs). Every property has a planted bug `scripts/prove-mutations.sh` requires it to catch. On one core, `hl prove` on all five targets takes about 7 ms for `examples/minimal.hl`, 11 ms for `examples/agent.hl` and 190 ms for `examples/reviewer.hl`.

## Use

    cargo build --release
    target/release/hl check examples/agent.hl
    target/release/hl prove --target vercel examples/reviewer.hl
    target/release/hl emit --target cloudflare examples/agent.hl
    target/release/hl sim --target vercel --seeds 0..1000 examples/agent.hl
    scripts/check.sh

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) holds the design, and `DECISIONS.md` records what each change established.

## License

[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
