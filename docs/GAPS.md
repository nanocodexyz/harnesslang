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
behaviour, short of the backoff's jitter, which needs a remainder operator. Gaps 5 (hooks around every tool and lifecycle event) and 7 (subagents) are closed by `before tool`, `after tool`, the lifecycle hooks, `spawn` and `join` (LANGUAGE.md). Numbers 9 to 11 are what
porting nanocode's Claude Code prompt and tools as `examples/claude_code.hl`
left out.

## 6. Tool catalogs that change within a turn

`refresh_dynamic_tools` reads the tool list again every round
(`agent.rs:6225-6278`), a notice tells the model what changed
(`agent.rs:4774-4825`), and tool search discovers deferred tools from the
model's own search results (`agent.rs:1537-1558`, `8478-8520`). A listing's
read admits tools for the rest of the turn, but nothing reports to the model
what a later read added or removed, and a tool named inside a search result
would have to be parsed out of text. Smallest addition: a built-in notice entry
that a listing read appends when its fingerprints changed, and a search tool
whose results are names the listing must hold, checked at run time.

## 8. Images, structured results and cache control

Tool results are blocks (text, images, structured results, metadata;
`agent.rs:106-157`, `2022-2060`), images are inlined and evicted to stay in the
window (`agent/images.rs:14-225`), and the prompt cache's lifetime is tracked
and refreshed (`agent/keep.rs:24-177`). A tool returns `u64`, `bool`, `text`
or a text enum. Smallest addition: an opaque `blob` type journaled by its
digest, which a tool returns and a list holds, with `keep last N` bounding how
many stay.
