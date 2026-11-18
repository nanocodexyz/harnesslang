# Development

Harnesslang is a language for agent harnesses. The `hl` compiler also checks programs, proves their bounds and simulates seeded execution. Fixed hosts are the reusable runtimes in `hosts/` that execute compiled programs and provide tools.

- Keep code simple and changes small. Match the surrounding code and preserve unrelated work.
- Zero dependencies. The crate uses only std, `Cargo.lock` lists `harnesslang` alone, and generated programs use only their language's standard library and the platform's own API. Code that checks harnesslang against other software lives in `oracle/`, its own workspace, never in the build.
- No code comments or doc comments. No `#[test]` functions: the checks are the simulator (`hl sim`, by seed), the examples and deliberate invalid-program/code changes that the checks must reject (mutations), and `scripts/check.sh` runs them all. A new invariant comes with a mutation the simulator catches, as tempoxyz/zempo requires.
- Follow [TigerStyle](TIGER_STYLE.md): assertions on what the code relies on, a named bound on every loop, queue and buffer, three kinds of failure never mixed (broken internal invariant, invalid external input, exhausted local bound), functions around 70 lines.
- No magic constants or arbitrary caps: a limit comes from a protocol, a platform, a measurement or caller policy. Surface invalid states; never hide them with silent truncation, clamping or fallbacks.
- Make invalid states unrepresentable: model a lifecycle as one enum whose variants carry only what that phase has.
- Deterministic: the compiler, interpreter and simulator read no clock, randomness or environment except through the simulator's seeded world, and iterate in a fixed order wherever order reaches output.
- In `.hl`, `and` and `or` evaluate both operands. They do not guard an index or slice: use nested `if` statements before a bounds-dependent access.
- A target is the compiler's model of an execution platform, such as native-jsonl or Cloudflare. A platform guarantee the compiler relies on is written once in `src/target/`, with where it comes from (the platform's documentation or a measurement), and drives the checker, the lowering and the simulator's faults alike.
- Validate with `scripts/check.sh` (build, every example through `hl check` and `hl emit`, a seed sweep per target, the mutations, and the differential runs of the compiled WebAssembly against the interpreter on every fixed host). Back performance changes with reproducible before/after measurements.
- Keep scratch work and artifacts in ignored `output/`; never commit them, credentials or personal data.
- Docs are this file, a short README, `docs/ARCHITECTURE.md`, `docs/CODE_MODE.md` and the language reference `docs/LANGUAGE.md`, whose programs live in `docs/snippets/` and whose commands `scripts/docs-check.sh` reruns, so every quoted output is real. No status reports, results tables, hashes, agent names or dates in them; those go in DECISIONS.md. Commit messages and names carry no ticket IDs.
- There is no CI and there are no pull requests. Each agent verifies its own change on a sandbox it gets itself (a boxctl box, an e2b sandbox or dev-georgios): run `scripts/ci.sh` there (`cargo build`, `scripts/check.sh` with every target's interpreter-vs-wasm differential run, `hl prove` on every program and the `scripts/gate.sh` ratchet), then rebase onto the latest master and push to master directly with a commit message titled `type(scope): summary` whose body has one line saying what ran and that it passed. If the push is refused, rebase, rerun the checks for files that changed, and push again. `HL_LONG=1` sweeps stay local.
- `gate.txt` is a ratchet on the road to one fixed host per platform: lines of emitted source, per-program host lines outside `hosts/`, program-target pairs not run as compiler-emitted wasm on a fixed host, and lines of Rust compiler (`rust_compiler_lines`: `src/syntax`, `check`, `ir`, `emit` and `prove`, on the road to the self-hosted compiler in docs/SELFHOST.md, which says when a language feature for the port may raise it). `scripts/gate.sh` measures them from the code; a count may only go down, and a change that lowers one commits the lower `gate.txt` (`scripts/gate.sh --write`).
- This file holds principles only and rarely changes. DECISIONS.md is the growing log: add what each change establishes and why, with the commands, dates and measurements behind it.

## Local reference builds and compiler components

Install Rust 1.99.0 with rustfmt, Node 24+, Python 3, Git, a C toolchain and PostgreSQL before checking. Preserve the build-wrapper settings `SHIPPY_REMOTE` and `CARGO_BUILD_RUSTC_WRAPPER`; disable remote builds with `SHIPPY_REMOTE_BUILD=0`. The compiler remains std-only. Separate reference workspaces compare its behavior against other implementations; their dependency sources and pinned API specifications are checked into `vendor/`, so reference builds must not download dependencies. Node, Python and PostgreSQL support the platform comparison checks, not the compiler build. Model evaluations and real deployments need separately provisioned credentials and network access.

Run fresh-cache offline builds of all reference workspaces, then the full local suite:

```sh
SHIPPY_REMOTE_BUILD=0 CARGO_BUILD_JOBS=2 scripts/offline-check.sh --build
SHIPPY_REMOTE_BUILD=0 CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 CHECK_JOBS=2 scripts/ci.sh
```

As a step toward a compiler written in its own language, `compiler/assemble*.hl` implements parts of converting WebAssembly text into binary. These components bound their loops and buffers by input length or checked source-size limits. The local suite compares their output against Rust, running the `.hl` code through both the language interpreter and the native host in `hosts/native/`. After building the compiler and native host, rerun the components that project static data, collect function signatures and assemble scalar integer expressions with:

```sh
SHIPPY_REMOTE_BUILD=0 CARGO_NET_OFFLINE=true scripts/selfhost-components.sh
```

These checks compare components; they do not establish a full self-hosted compiler or byte-identical compiler output when a compiler builds its successor. `scripts/ci.sh` runs locally, not in GitHub Actions. Follow any repository protections instead of bypassing them.
