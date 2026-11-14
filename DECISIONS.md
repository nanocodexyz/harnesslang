# Decisions

## 2026-11-14: harnesslang starts from tempolang's method and nanocode's durability

harnesslang takes tempolang's method (an interpreter as the executable spec, generated code checked against it, deterministic simulation from a seed, invariants as compiler checks) and applies it to nanocode's problem: making an agent harness durable on hosts that promise different things. The journal follows nanocode-turnlog's records, the effect classes are nanocode's `EffectClass`, and the simulator's invariants, fault profiles and mutation discipline are nanocode-sim's, so that nanocode is the oracle.

The compiler is one std-only Rust crate. Generated programs are std-only too. Targets are models in `src/target/` that the checker, the lowering and the simulator share.
