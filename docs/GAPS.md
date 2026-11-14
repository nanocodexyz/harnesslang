# What nanocode's Claude agent loop does that harnesslang cannot write

`examples/claude_agent.hl`, `claude_code.hl`, `codex_agent.hl`, `hooks.hl`,
`subagents.hl` and `subagent_child.hl` port the shapes of nanocode's harnesses. Gaps 1 to 8 of the
earlier list are closed (the last two by `suspend` and `wait`, LANGUAGE.md), so
numbers 1 to 8 now list what a fresh read of nanocode's Claude backend loop,
`crates/backends/claude/src/agent.rs` and `agent/` at nanocode c7679e3, still
does that harnesslang cannot express; gaps 5 (hooks around every tool and lifecycle event) and 7 (subagents) are closed by `before tool`, `after tool`, the lifecycle hooks, `spawn` and `join` (LANGUAGE.md). They are ranked by how much of a real
harness each blocks, each with the smallest language addition that would let
it be written and how the compiler would keep checking it. A gap the language
closes is removed and the others keep their numbers. Numbers 9 to 11 are what
porting nanocode's Claude Code prompt and tools as `examples/claude_code.hl`
left out.

## 1. Token accounting and truncation

Compaction starts when the provider's usage (input, cache reads and writes,
output; `agent.rs:8229-8236`) plus an estimate of the next prompt
(`estimate_text_tokens`, a quarter of the UTF-16 length, `agent.rs:60-62`)
reaches the window less a reserve (`compaction_threshold`,
`agent.rs:6279-6292`). Tool results are cut to `TOOL_RESULT_TOKEN_LIMIT` and
`DIRECT_TOOL_RESULT_TOKEN_LIMIT` (12,000 and 25,000 tokens) by keeping their
head and tail (`agent.rs:64-65`, `74-157`). harnesslang compacts by list
length only: a reply carries no usage, and text has no length and cannot be
cut. Smallest addition: journaled `u64` usage fields on a model reply, a pure
`len(text)`, and `truncate_middle(text, n)` with a constant `n`, so the
threshold is ordinary `u64` arithmetic the prover already checks and every cut
text has a declared bound.

## 2. Summarizing compaction

`summarize_locked` asks the model, or the server's compaction, for a summary of
the history it packs, runs the `PreCompact` hook that may block or stop
(`agent.rs:6350-6364`), retries without thinking blocks, fires `PostCompact`
(`agent.rs:6590`), and compacts once more when the window is exceeded
(`agent.rs:8264-8290`). `subagents.hl` writes the summary by hand: one model
call, then `keep context last N`, with the summary entry an `assistant`
entry rather than the replacement of what was dropped. Smallest addition:
`compact list with model keeping last N`, one journaled model step whose text
answer becomes the list's first entry, counted as a model call in the worst
case.

## 3. Recovery by stop reason

On `max_tokens` the loop appends "Output token limit hit. Resume directly"
and continues, at most three times (`agent.rs:8248-8262`); on a refusal it
keeps the finished text and asks once more (`agent.rs:7741-7760`); a
`pause_turn` continues the server turn. A declared reply sum names the
variants a model returns, but the stop reason and the partial text that came
with it are not part of a reply. Smallest addition: a built-in `stop` text
enum on every reply (`end`, `tool_use`, `max_tokens`, `refusal`,
`context_exceeded`, `pause`), matched exhaustively, with the partial text kept;
the continuation counters are then state fields a `requires` bounds.

## 4. Retry and backoff of model calls

`request_response` retries with exponential backoff from `RETRY_BASE_MS` to
`RETRY_CAP_MS` (500 ms to 32 s) with 25% jitter, honours `Retry-After`, keeps a
separate budget for throttling (`THROTTLED_ATTEMPTS`, 10, within 180 s), and
tells a request that was never sent from one that may have been accepted
(`agent.rs:66-70`, `5940-6030`). A failed model call is `failed(text)` for a
declared reply type, with no error class, no `Retry-After`, and `wait` takes a
constant. Smallest addition: a closed error enum on a model failure
(`throttled`, `transient`, `rejected`) with a journaled `retry_after_ms`, and
`wait` of a `u64` expression bounded by a constant (`wait(delay) at most
cap_ms`), so the worst case still multiplies a constant.

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
