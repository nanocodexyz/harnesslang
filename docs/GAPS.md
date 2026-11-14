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
as session state (`settings` and `with {...}` on a model call), all in LANGUAGE.md. `hl-agent` runs real child sessions of `claude_code_child.hl`.

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
## 3. Code cells that outlive their tool call (yield, wait, detach)

Claude's exec and wait race a running cell's progress against cancel. On a stop it runs stop_code_cells and returns STOPPED or STOPPED_UNKNOWN, with the output gathered before the stop (agent.rs:6965-7044, stop_code_cells and settle_code_turn 6154-6190). Each cell's nested-call summaries are kept across rounds (agent.rs:8157-8165). Codex terminates every turn cell when a turn completes or is cancelled (model/run/turn.rs:288, 303, 383-396). A failed turn with no active call detaches its cells instead, so they outlive the turn (turn.rs:335-357, 366-381). After a crash it reattaches a cell still running on the host with recover_code, and runs it fresh when the call was never sent (tool_calls.rs:1277-1287).

Why not: CODE_MODE.md, Not in this spec, says it: a harnesslang turn blocks on its cell, and yield_ms only goes into the header. A cell cannot return "Script running" and be collected by a later `wait` call in another round. There is also no lifecycle for a cell that is still running when its site, round or turn ends: terminated or detached, with nested calls left open. LANGUAGE.md's Code mode section has no second site for the same cell.

Smallest addition: A `waits name` clause on `code`. A site may finish with running(cell) after yield_ms, and a site of the wait tool continues that cell's journal. Every running cell is stopped at its turn's `return`, with open nested calls recorded unknown, unless the program says `detach`. Cell records are already keyed by cell, so C1 and C2 hold unchanged. cell_finish_once is extended to: exactly one `finished` per cell across all its sites. A new property, cell_settled_at_turn_end, requires each cell to be finished or detached when its turn's status is flushed, with open nested ids carried as unknown. The calls and timers bounds become per cell, summed over its sites in the worst case. The simulator yields cells and drives waits across rounds and crashes. Mutation: cell_leaks, a cell left running past its turn.

## 7. Forking or rewinding a session at a boundary

Claude forks a session, or a side conversation, from the latest boundary, a completed turn or a checkpoint (agent.rs:3190-3257, fork_point 4292-4311). Boundaries are published per round (agent.rs:4245-4265). The child's state is scrubbed: no tasks, pending continuation, previous message id or container, lifecycle_started false so SessionStart hooks run again, and auto-compaction off for side conversations. Its first record is a checkpoint under a branched policy. A rewind picks an earlier checkpoint, keeps admitted tool ids and recovery notices, and adds a notice that effects from discarded turns may still exist (agent/durable.rs:1640-1675). Codex publishes a fork snapshot before every model call (model/run/turn.rs:834-841, 922) and branches sessions (agent/driver/branch.rs:97-160).

Why not: spawn starts a fresh session of another handler, with only typed arguments (LANGUAGE.md, Spawn and join). A side question reads a snapshot but writes nothing. Nothing creates a session of the same harness that starts from another session's state at a chosen finished turn, and no hook decides what a fork keeps and resets.

Smallest addition: A `fork name at latest|turn` tool, external like spawn and keyed T.S. The child's journal starts with a `forked` record and a checkpoint holding the parent's state at that turn, taken from the parent's durable journal as a side question's snapshot is. An `on forked(origin: text)` hook may assign, clear, keep and push but run no effects, and resets fields such as job handles or flags. The checker counts forks as sessions in the spawn tree's worst case. The simulator checks fork_state_equal (the child's initial state is the replay of the parent's durable journal at that turn, after the hook). Mutation: fork_live_state, which forks from the running turn's state.

## 9. What effects in flight leave out

Gaps 1, 4 and 6 of the earlier list are closed (LANGUAGE.md, Effects in flight). What
nanocode does there that the additions do not write:

- A `wait`, a cell's nested calls and a running code cell are not interruptible:
  only tools and the model are. nanocode's retry sleep, cell stop and turn hold
  race the cancel; here only `hold` wakes on a cancel.
- A steer cannot be withdrawn once recorded (Codex's skipped withdrawn steers).
- A job notice that arrives while no turn runs is recorded by the next turn the
  host admits; nothing admits a turn because a job finished (nanocode's
  `Delivery::Prompted`). `examples/claude_code.hl` takes notices only when it
  would answer, after its `hold`, not at every round (taking them every round cost
  3.1M prover steps more).
- hl-agent sends no stall notices: its jobs report only their end.

