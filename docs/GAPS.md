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

## 1. The model's reply type is fixed

nanocode branches on more than "answer or calls". Claude's loop reads
`stop_reason`: `max_tokens` appends a continuation message and loops, at most
three times (`output_continuations`); `pause_turn` re-sends the pending
messages; `model_context_window_exceeded` compacts once and continues
(`context_recovery_attempted`); `refusal` stops the turn; tool calls without
a `tool_use` or `max_tokens` stop are an error. Codex reads `end_turn`:
`end_turn == Some(false)` with no calls continues the loop after an optional
mid-turn compaction (`turn.rs`). Both read `usage` to decide when to compact.
In harnesslang a reply is the builtin `answer(text) | calls(list<call, M>)`,
so none of these branches can be written: the ports treat every text reply
as the end of the turn.

Smallest addition: a declared reply sum on the model,
`model messages calls M reply claude_reply` with
`type claude_reply = answer(text, u64) | calls(list<call, M>, u64) | truncated(text) | paused | context_full | refused(text)`
(the `u64` is the usage the reply reports), where the variant carrying
`list<call, M>` is the one the worst-case walk multiplies. Matching it is
exhaustive like any sum, so the continuation counter and its bound
(`continuations_max`, nanocode's 3) are ordinary state the prover bounds.
The simulator draws variants from the declared sum. The journal is
unchanged: the `model` record already holds the reply as JSON.

## 2. Compaction

nanocode compacts on a token threshold (`auto_compact_window_tokens`), on
context exhaustion and on request: a summarizing model call
(`compact_locked`, `prompts/compact.md`) whose summary replaces the
history, keeping the most recent messages verbatim (`COMPACT_KEPT`). Codex
installs pre- and mid-turn compactions (`lifecycle.rs`). harnesslang lists
only grow: there is no way to replace or clear a list, and no model whose
reply is plain text. So every port carries
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

## 5. Parallel tool calls

nanocode runs a reply's tool calls concurrently when the tools allow it
(`supports_parallel_tool_calls` in `tool-host`, writers marked exclusive in
`code_mode_policy.rs`). `for call in requested` dispatches one at a time.
Smallest addition: `for call in requested parallel { ... }` whose body may
only dispatch and push; sequence numbers are assigned in list order before
any dispatch, every call record of the batch is flushed in one durability
point before the first external dispatch, and results are appended in list
order, so the journal equals the sequential one. Code mode's `parallel`
bound (CODE_MODE.md) is the same idea for nested calls.

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

## 9. Model failures and retries

nanocode retries throttled model calls (`THROTTLED_ATTEMPTS` 10,
`RETRY_BASE_MS` 500 to `RETRY_CAP_MS` 32,000 with jitter) and reports
`StopFailure` to a hook. A harnesslang model call returns `reply`, not
`known<reply>`, so a provider error cannot be handled in the program.
Smallest addition: the model returns `known<reply>` (it is idempotent),
leaving retries to the program under a named bound.
