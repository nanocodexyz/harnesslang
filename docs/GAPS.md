# What nanocode's Claude agent loop does that harnesslang cannot write

`examples/claude_agent.hl`, `claude_code.hl`, `codex_agent.hl`, `hooks.hl`,
`subagents.hl` and `subagent_child.hl` port the shapes of nanocode's harnesses. Gaps 1 to 8 of the
earlier list are closed (the last two by `suspend` and `wait`, LANGUAGE.md), so
numbers 1 to 8 now list what a fresh read of nanocode's Claude backend loop,
`crates/backends/claude/src/agent.rs` and `agent/` at nanocode c7679e3, still
does that harnesslang cannot express. They are ranked by how much of a real
harness each blocks, each with the smallest language addition that would let
it be written and how the compiler would keep checking it. A gap the language
closes is removed and the others keep their numbers. Gaps 1 to 4 (token
accounting and truncation, summarizing compaction, recovery by stop reason,
retry and backoff) are closed by a model's `window`, the `attempt` and `stop`
builtins, `len` and `truncate_middle` on text, `keep ... after`, `with` and
`wait(e) at most c` (LANGUAGE.md); `examples/claude_code.hl` keeps their
behaviour, short of the backoff's jitter, which needs a remainder operator.
Gap 6 (tool catalogs that change within a turn) is closed by `name(context)`
listing notices and `finds` tool search, and the images and structured
results of gap 8 by `blob<N>`, `text or blob<N>` tools and `keep L blobs
last N` ([Listing notices and tool search](LANGUAGE.md#listing-notices-and-tool-search),
[Blobs](LANGUAGE.md#blobs)); what is left of gap 8 is the prompt cache's
lifetime. Numbers 9 to 11 are what
porting nanocode's Claude Code prompt and tools as `examples/claude_code.hl`
left out.

## 5. Hooks around every tool and every lifecycle event

`hooked_handler` runs a hook before every tool, which may allow it, rewrite
its input or deny it, and one after it (`agent.rs:562-621`); lifecycle hooks
fire at `SessionStart`, `UserPromptSubmit`, `Stop`, `PreCompact`,
`PostCompact`, `StopFailure`, `SubagentStart`, `SubagentStop` and
`SessionEnd`, with `additional_context` (`hooks.rs:49-151`). `hooks.hl` writes a
permission prompt and the `Stop` hook's reasons at the points it names, with
`suspend`; it cannot rewrite a call's input, which has no record type, or fire
at points that have no syntax, such as compaction or session end. Smallest
addition: `before tool` and `after tool` blocks inlined at every dispatch site
like a fn, whose `deny(text)` records the call's failure without running it,
and handlers for the session's own events.

## 7. Subagents, side questions and forks

The backend spawns and forks child agents with their own scoped journals
(`agent.rs:706-736`, `9204-9210`, `4590`) and answers a side question from a
snapshot of the transcript without touching the main turn
(`agent/side.rs:135-257`). `subagents.hl` calls `spawn_agent` as an external
tool and waits for a child with `wait`. Smallest addition: a `spawn` effect
that starts a child session of a declared harness, external in class and
journaled under a scoped key, and a child's completion as a resume of a
`suspend`, so a parent that waits on its children ends its run instead of
sleeping.

## 8. Prompt cache lifetime

The prompt cache's lifetime is tracked and kept warm: after a reply the
backend keeps its request and, while the session idles, sends it again
before the cache's lifetime (five minutes, or an hour) runs out, a bounded
number of times (`agent/keep.rs:24-177`). A harness has no notion of time
passing between turns and no effect that runs while no turn does. Smallest
addition: a `keep` timer on the model whose refreshes are model calls the
journal records under the session rather than a turn, bounded by a named
count, so the worst case counts them and replay skips them.
