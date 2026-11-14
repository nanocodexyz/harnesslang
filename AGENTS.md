# Development

- Keep code simple and changes small. Match the surrounding code and preserve unrelated work.
- Zero dependencies. The crate uses only std, `Cargo.lock` lists `harnesslang` alone, and generated programs use only their language's standard library and the platform's own API. Code that checks harnesslang against other software lives in `oracle/`, its own workspace, never in the build.
- No code comments or doc comments. No `#[test]` functions: the checks are the simulator (`hl sim`, by seed), the examples and the mutations, and `scripts/check.sh` runs them all. A new invariant comes with a mutation the simulator catches, as tempoxyz/zempo requires.
- Follow [TigerStyle](TIGER_STYLE.md): assertions on what the code relies on, a named bound on every loop, queue and buffer, three kinds of failure never mixed, functions around 70 lines.
- No magic constants or arbitrary caps: a limit comes from a protocol, a platform, a measurement or caller policy. Surface invalid states; never hide them with silent truncation, clamping or fallbacks.
- Make invalid states unrepresentable: model a lifecycle as one enum whose variants carry only what that phase has.
- Deterministic: the compiler, interpreter and simulator read no clock, randomness or environment except through the simulator's seeded world, and iterate in a fixed order wherever order reaches output.
- A target is a model. A platform guarantee the compiler relies on is written once in `src/target/`, with where it comes from (the platform's documentation or a measurement), and drives the checker, the lowering and the simulator's faults alike.
- Validate with `scripts/check.sh` (build, every example through `hl check` and `hl emit`, a seed sweep per target, the mutations, and the differential runs of generated code against the interpreter). Back performance changes with reproducible before/after measurements.
- Keep scratch work and artifacts in ignored `output/`; never commit them, credentials or personal data.
- Docs are this file, a short README, `docs/ARCHITECTURE.md`, `docs/CODE_MODE.md` and the language reference `docs/LANGUAGE.md`, whose programs live in `docs/snippets/` and whose commands `scripts/docs-check.sh` reruns, so every quoted output is real. No status reports, results tables, hashes, agent names or dates in them; those go in DECISIONS.md. Commit messages and names carry no ticket IDs.
- Work on master, commit only your own changes and push each one; no pull requests. There is no CI: run `scripts/check.sh` before pushing.
- This file holds principles only and rarely changes. DECISIONS.md is the growing log: add what each change establishes and why, with the commands, dates and measurements behind it.
