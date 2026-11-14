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


## 3. Interrupts and steering are polled through none tools

nanocode delivers a cancellation (`Cancellation`, `TurnCancelled`) and
steer messages (`accept_steer`, Codex's `pending_steers`) into a running
turn between rounds. harnesslang has no inbound events inside a turn, so
the ports poll: `cancel_requested()`, `steer_waiting()` and `steer_take()`
are declared none tools. Each poll is a `call` and a `result` record, two
records and one sequence number per poll: `claude_agent.hl` spends up to 3
of its 21 effects per round on polls (worst case 1,344 outbound requests
against 1,152 without them), and a cancel that arrives during a long tool
call is seen only after the whole batch.

Smallest addition: inbound events declared on the harness, `on cancel` and
`on steer(message: text)`, read in a handler with `if cancelled { ... }` and
`take steers` (a `list<text, N>`). The runtime appends one `cancel` or
`steer` record when an event arrives (nanocode's turn inbox does this), and
a read returns the events recorded before the current sequence number, so
replay stays deterministic and a read costs no record when nothing arrived.


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
inbound event (gap 3) arrives, as nanocode's parked turns
(`agent/parked.rs`) do.

## 8. Timers and waits

nanocode's `wait_agent` blocks for a child and code-mode cells wait on
timers (`yield_time_ms`). In `subagents.hl` `wait_agent` is a none tool
that returns at once; a real wait blocks the handler for a wall time the
Vercel and Cloudflare models cannot check. Smallest addition: `wait(ms)` as
an effect whose result is journaled and whose duration enters the target's
wall-time limit, with the C2 durability point code mode specifies for timer
waits.
