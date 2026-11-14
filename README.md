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

## Use

    cargo build --release
    target/release/hl check examples/agent.hl
    target/release/hl emit --target cloudflare examples/agent.hl
    target/release/hl sim --target vercel --seeds 0..1000 examples/agent.hl
    scripts/check.sh

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) holds the design, and `DECISIONS.md` records what each change established.

## License

[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
