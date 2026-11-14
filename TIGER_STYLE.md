# TigerStyle

How harnesslang applies [TigerBeetle's TigerStyle](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md), as tempoxyz/zempo and nanocodexyz/nanocode do. AGENTS.md wins where the two differ. A change that departs from a rule says so in its DECISIONS.md entry, with why and what bounds it.

## Safety

- Assert what the code relies on: arguments, results and the invariants of each lifecycle move. A broken invariant of our own is a panic, never a value that flows on.
- Pair assertions across a boundary: check a property where it is written and again where it is read back, such as a journal record encoded and decoded, or a durability point placed and then found before its effect.
- Assert the positive and the negative space: what must hold, and what must not happen, such as an external call dispatched twice.
- Split compound assertions so a failure names the one condition that broke.
- Three kinds of failure, never mixed: our invariant broken (panic); external input that is wrong, from a source file, a journal, a model or a store (a typed error naming what was wrong and where); local exhaustion (a typed error naming the bound that was reached). Never assert on external input, and never hide any of the three behind truncation, clamping or a fallback.

## Limits

- Every queue, buffer, list, retry, depth and loop has a bound. Its constant is named for what it bounds, its unit last (`tokens_max`, `restarts_max`), and its value comes from a protocol, a platform, a measurement or caller policy.
- Reaching a bound is an error the caller sees, never a silent drop.
- Walk input-shaped data (source text, journals, JSON) iteratively, or with an explicit depth bound that returns an error.

## Memory

- Size buffers from known bounds up front and reuse them on hot paths.
- Back every memory or allocation change with before and after measurements in DECISIONS.md.

## Control flow

- Model each lifecycle as one enum whose variants carry only what that phase has.
- Push `if`s up and `for`s down: decide once at the top, then run straight-line loops over the batch.
- Handle every `Result`. Dropping one is a decision with a reason in DECISIONS.md.
- Keep functions short enough to read at once, around 70 lines; split a longer one when touching it.

## Determinism

The simulator replays a seed exactly only if the code under it is deterministic.

- Read time and randomness only through the simulator's seeded world, never `SystemTime::now`, `Instant::now` or environment variables, outside `main`'s argument parsing.
- Iterate in an order that does not depend on a random hasher wherever the order reaches output, a journal or a decision: `BTreeMap` or a sorted `Vec`.
- An error carries no addresses, timestamps or other values that differ between two runs of the same seed.

## Naming

- Whole words, no abbreviations except established ones (`id`, `url`, `sql`, `ir`). Units and qualifiers last, so related names line up: `bytes_max`, `bytes_written`.
- Name a constant for the bound or protocol value it is; a bare number in code is a constant waiting for a name.
