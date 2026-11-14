# What nanocode's harnesses do that harnesslang cannot write

`examples/claude_agent.hl`, `codex_agent.hl`, `subagents.hl` and
`subagent_child.hl` port the shapes of nanocode's Claude backend
(`crates/backends/claude/src/agent.rs`), Codex backend
(`crates/backends/codex/src/model/run/`) and subagent runtime
(`crates/runtime/subagents/`). Everything below is what the ports had to
leave out or approximate, ranked by how much of a real harness it blocks,
each with the smallest language addition that would let it be written and
how the compiler would keep checking it. A gap the language closes is removed
and the others keep their numbers.

## 2. Compaction

nanocode compacts on a token threshold (`auto_compact_window_tokens`), on
context exhaustion and on request: a summarizing model call
(`compact_locked`, `prompts/compact.md`) whose summary replaces the
history, keeping the most recent messages verbatim (`COMPACT_KEPT`). Codex
installs pre- and mid-turn compactions (`lifecycle.rs`). harnesslang lists
only grow: there is no way to replace or clear a list, and no model whose
reply is plain text. `examples/claude_agent.hl` handles
`model_context_window_exceeded` once by sending the context again, where
nanocode compacts. So every port carries
`requires len(context) + 1 + rounds_max * (1 + calls_per_round_max) <= context_entries_max`
and refuses a turn once the context lacks room, and `subagents.hl`'s roster
of spawned agents never shrinks.

Smallest addition: `clear list` and `keep list last N` statements (the
prover sets the length to 0 or to `min(len, N)`, both linear), plus a model
declared with `reply text` for summaries (its record is a `model` record
like any other). Compaction is then a handler branch:
`let summary = summarize(context)`, `keep context last kept_max`,
`push context user(summary)`. nanocode-sim's `retire_skipped` and
`seal_skipped` mutations then have a counterpart: the simulator checks that
a checkpoint after `clear` no longer replays the cleared prefix.

## 6. Typed tool input and dynamic tool lists

A model call's input is one `text`, and `dispatch` finds the tool by name
with classes fixed at compile time. nanocode validates JSON input against
each tool's schema, discovers deferred tools (`ToolSearch`), reads MCP
listings (`McpToolListing`) and refuses a dynamic tool that changed since
admission. Smallest addition: `tools from listing(name) effect C`, a family
of tools named at run time whose class is fixed by the declaration, so the
effect rule stays a compile-time fact.

## 7. Permission prompts and lifecycle hooks

nanocode's hooks (`hooks.rs`: `UserPromptSubmit`, `Stop`, `PreCompact`,
`SubagentStop` and others) can block a stop and force one more round
(`stop_hook_active`), and a tool call can wait for a person's approval. A
hook ports as a declared tool called at the same point: `examples/hooks.hl`
ports the `Stop` hook's decision (`ClaudeLifecycleDecision`: continue, block,
stop) as a tool returning a text enum, without the reasons `Block` and
`Stop` carry. An approval that
takes hours does not, because a handler cannot end a turn and resume where
it was. Smallest addition: a `suspend` effect that finishes the turn with a
durable waiting status and resumes it from its journal when a matching
inbound event (an `on` event, LANGUAGE.md) arrives, as nanocode's parked turns
(`agent/parked.rs`) do.

## 8. Timers and waits

nanocode's `wait_agent` blocks for a child and code-mode cells wait on
timers (`yield_time_ms`). In `subagents.hl` `wait_agent` is a none tool
that returns at once; a real wait blocks the handler for a wall time the
Vercel and Cloudflare models cannot check. Smallest addition: `wait(ms)` as
an effect whose result is journaled and whose duration enters the target's
wall-time limit, with the C2 durability point code mode specifies for timer
waits.
