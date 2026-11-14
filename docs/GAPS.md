# What nanocode's Claude and Codex loops do that harnesslang cannot write

`examples/claude_code.hl`, `claude_code_child.hl`, `claude_agent.hl`, `codex_agent.hl`,
`hooks.hl`, `subagents.hl` and `subagent_child.hl` port the shapes of nanocode's harnesses.
Every gap of the earlier lists is closed: token accounting, truncation and
summarizing compaction, recovery by stop reason, retry and backoff (a model's
`window`, `attempt`, `stop`, `len`, `truncate_middle`, `keep ... after`,
`with`, `wait(e) at most c`); hooks around tools and lifecycle events
(`before tool`, `after tool`, the lifecycle hooks); subagents (`spawn`, `join`,
spawn trees with `sessions at most`); tool catalogs that change within a turn
(listing notices, `finds`); images and structured results (`blob<N>`); side
questions (`on side`); the prompt cache's lifetime (`on idle`); effects in
flight (`interruptible by cancel, steer`, background jobs with `jobs at most`,
`on job` and `hold`, and recovery hooks with `external recoverable`);
rewriting entries already in the history (`rewrite`); and request settings held
as session state (`settings` and `with {...}` on a model call); code cells across calls (`waits` with yield, wait, terminate, turn-end stops, and a cancel that stops a cell with STOPPED or STOPPED_UNKNOWN); and forking or rewinding a session at a finished turn (`fork NAME at latest|turn`, `on forked`, and a host's `fork_entry`, which `hl-agent --fork-from` uses), all in LANGUAGE.md. `hl-agent` runs real child sessions of `claude_code_child.hl`.

The list below comes from a fresh read of nanocode c7679e3's Claude backend
(`crates/backends/claude/src`) and Codex backend (`crates/backends/codex/src`),
checked against LANGUAGE.md. Gaps are ranked by how much of a real harness each
blocks, each with what nanocode does, why harnesslang cannot write it, and the
smallest language addition that would let it be written and how the compiler
would keep checking it. A gap the language closes is removed and the others
keep their numbers.

## 2. What a parallel batch's lanes leave out

A parallel for's arms are now each call's lane: they run effects of their own and the `after tool` hook, after the batch and in list order (LANGUAGE.md, Parallel tool calls, Lanes). What nanocode's batches do beyond that:

- Each Claude `durable_tool` runs its PreToolUse hook inside the batch (agent.rs:562-624, 8015-8094), and each Codex call its own `before_tool_call` hook (tool_calls.rs:680-757, 1064-1073). A `before tool` hook decides a call before the batch records it, and its decision lives in hidden state fields one call at a time, so a harness with one is refused a parallel for.
- Lanes run one after another once the batch's dispatches are done; nanocode runs each call's hooks concurrently with the other calls.
- Claude groups a reply's calls by name, every call when parallel_tools is on, otherwise consecutive parallel_safe_tools (agent.rs:1148-1162, tool_batches 2932-2946), and Codex runs every call of a reply at once, external ones included (model/run/tool_calls.rs:659-820). The batch here still groups by effect class: an external call runs alone after a flush.

Smallest addition: `parallel when cond(call)`, a pure grouping predicate chosen by the program instead of the effect class (consecutive calls for which it holds form one group, externals included, after the one flush of the batch's call records), and a lane prelude that runs a `before tool` hook per call before the batch records it, with the hook's decision carried per call instead of in shared hidden fields.
## 3. What cells across calls leave out

A cell now yields after `yield_ms`, a site of the `waits` tool continues it, `terminate` and every turn's end stop it, and a cancel stops an interruptible one (LANGUAGE.md, Code mode, Cells across calls and Stopping a cell). What nanocode does beyond that:

- Codex detaches a failed turn's cells instead of terminating them, so they outlive the turn and a later turn may wait on them (model/run/turn.rs:335-357, 366-381). Here a trap stops every cell of its turn. A detached cell's later records would belong to a turn whose status is already written, which every fold here and nanocode's turnlog Fold refuse; its header sits before that turn's checkpoint, where every target's load starts; and recovery reports open calls unknown only for unfinished turns.
- A yielded cell does not run between sites: nanocode's cell keeps running on the host while the model works, and `wait` collects what it did meanwhile. Here a cell is a coroutine of its sites, and only its timers move on.
- The STOPPED text carries no output gathered before the stop, since the engine reports output only at a cell's end.
- After a crash Codex reattaches a cell still running on the host with recover_code (tool_calls.rs:1277-1287); here a cell is replayed from its journal.

Smallest addition for detach: a `detached` cell record written at a trap, which a later turn's wait site continues under nested ids of the continuing turn, with open detached calls listed by a session-level recovery record and the journal read whole when a program detaches; cell_settled_at_turn_end would accept a detached cell, and a mutation that drops the detached record would be caught when a later wait finds no cell.

## 9. What effects in flight leave out

Gaps 1, 4 and 6 of the earlier list are closed (LANGUAGE.md, Effects in flight), a cancel stops an interruptible code cell, and hl-agent sends nanocode's stall notice when a background job's output stays quiet for 45 s on what looks like an interactive prompt. What
nanocode does there that the additions do not write:

- A `wait` is not interruptible: nanocode's retry sleep races the cancel;
  here only `hold` and an interruptible code cell's timer waits and nested
  calls end early on one.
- A steer cannot be withdrawn once recorded (Codex's skipped withdrawn steers).
- A job notice that arrives while no turn runs is recorded by the next turn the
  host admits; nothing admits a turn because a job finished (nanocode's
  `Delivery::Prompted`). `examples/claude_code.hl` takes notices only when it
  would answer, after its `hold`, not at every round (taking them every round cost
  3.1M prover steps more).

