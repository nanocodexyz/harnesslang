# What nanocode's harnesses do that harnesslang cannot write

`examples/claude_agent.hl`, `claude_code.hl`, `codex_agent.hl`, `subagents.hl` and
`subagent_child.hl` port the shapes of nanocode's Claude backend
(`crates/backends/claude/src/agent.rs`), Codex backend
(`crates/backends/codex/src/model/run/`) and subagent runtime
(`crates/runtime/subagents/`). Everything below is what the ports had to
leave out or approximate, ranked by how much of a real harness it blocks,
each with the smallest language addition that would let it be written and
how the compiler would keep checking it. A gap the language closes is removed
and the others keep their numbers.

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

## 9. Optional tool parameters

Claude's tools have optional parameters (Bash's `timeout`, Read's `offset` and `limit`, Edit's `replace_all`), and typed input requires exactly the declared parameters. `examples/claude_code.hl` declares every parameter and `agent/`'s host fills a missing one with the schema's default before the engine sees the call, so the journal holds the defaulted input rather than what the model sent. Smallest addition: `param: type = default` in a tool's parameters, filled by `dispatch` and journaled as filled, with the simulator drawing inputs that leave it out.

## 10. Options on a model call

nanocode's compaction call offers no tools (`disable_tools`) and retries once forcing text; a harnesslang model call has no options, so `claude_code.hl`'s compaction keeps the tools offered and fails the turn if the summary calls one. Smallest addition: a declared, finite set of call modes (`messages(context) mode text_only`) passed to the host and journaled with the call.

## 11. Text processing

nanocode strips the summary's `<analysis>` block and rewrites its `<summary>` tag (`compact_summary`); harnesslang text has only `++`, so the summary enters the context as written. Smallest addition: pure, bounded text builtins (find, slice) that the interpreter and every backend implement identically.
