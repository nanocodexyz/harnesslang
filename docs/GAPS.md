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
questions (`on side`); the prompt cache's lifetime (`on idle`); and effects in
flight (`interruptible by cancel, steer`, background jobs with `jobs at most`,
`on job` and `hold`, and recovery hooks with `external recoverable`), all in
LANGUAGE.md. `hl-agent` runs real child sessions of `claude_code_child.hl`.

The list below comes from a fresh read of nanocode c7679e3's Claude backend
(`crates/backends/claude/src`) and Codex backend (`crates/backends/codex/src`),
checked against LANGUAGE.md. Gaps are ranked by how much of a real harness each
blocks, each with what nanocode does, why harnesslang cannot write it, and the
smallest language addition that would let it be written and how the compiler
would keep checking it. A gap the language closes is removed and the others
keep their numbers.

## 2. A parallel batch whose calls each run their own effects

Claude groups a reply's calls into batches by name: every call when parallel_tools is on, otherwise consecutive parallel_safe_tools (agent.rs:1148-1162, tool_batches 2932-2946). It runs each batch's durable_tool calls concurrently (agent.rs:8015-8094), and each one runs hooked_handler inside it: a PreToolUse hook, the handler, then a PostToolUse hook (agent.rs:562-624). Codex runs every call of a reply concurrently, external ones included, as a FuturesOrdered (model/run/tool_calls.rs:659-820). Each call has its own durable step tool-{index}-{id} and its own before_tool_call hook (tool_calls.rs:680-757, 1064-1073). Results land in reply order.

Why not: `for call in requested parallel` must be one `match dispatch(call)` whose arms only push, bind and assign. Its batch groups by effect class: an external call always runs alone after a flush. The checker refuses a parallel for in a harness with `before tool`, `after tool`, `on subagent start` or a `code` tool, and the arms cannot `join`, `suspend` or cut (LANGUAGE.md, Parallel tool calls and Hooks; claude_code.hl run_joining falls back to a sequential for). So a hooked or approval-gated harness, or Codex's concurrent shell calls, is serial.

Smallest addition: Allow a parallel for's body to be a lane: straight-line code per call that may include the inlined hooks, `suspend`, `join` and further effects. Each lane's effects take a sub-sequence T.S/k, where S is the batch's seq and k the call's index, so concurrent completion order never changes numbering. Add a grouping predicate, `parallel when pure_cond(call)`, chosen by the program instead of by effect class. Durability point 1 then applies per external within its lane. The prover lowers each lane as a sequential plan; since flushes are per record, a lane-local flush-before-external proves durable_before_external and external_once. The worst case for records and requests is the sum of the lanes, and for waits their maximum. The simulator completes lanes in random order with crashes between their effects. It extends sequential_equal per lane (each lane's journal equals the sequential run of that call). Mutation: lane_seq_shared, which numbers lane effects from one counter.

## 3. Code cells that outlive their tool call (yield, wait, detach)

Claude's exec and wait race a running cell's progress against cancel. On a stop it runs stop_code_cells and returns STOPPED or STOPPED_UNKNOWN, with the output gathered before the stop (agent.rs:6965-7044, stop_code_cells and settle_code_turn 6154-6190). Each cell's nested-call summaries are kept across rounds (agent.rs:8157-8165). Codex terminates every turn cell when a turn completes or is cancelled (model/run/turn.rs:288, 303, 383-396). A failed turn with no active call detaches its cells instead, so they outlive the turn (turn.rs:335-357, 366-381). After a crash it reattaches a cell still running on the host with recover_code, and runs it fresh when the call was never sent (tool_calls.rs:1277-1287).

Why not: CODE_MODE.md, Not in this spec, says it: a harnesslang turn blocks on its cell, and yield_ms only goes into the header. A cell cannot return "Script running" and be collected by a later `wait` call in another round. There is also no lifecycle for a cell that is still running when its site, round or turn ends: terminated or detached, with nested calls left open. LANGUAGE.md's Code mode section has no second site for the same cell.

Smallest addition: A `waits name` clause on `code`. A site may finish with running(cell) after yield_ms, and a site of the wait tool continues that cell's journal. Every running cell is stopped at its turn's `return`, with open nested calls recorded unknown, unless the program says `detach`. Cell records are already keyed by cell, so C1 and C2 hold unchanged. cell_finish_once is extended to: exactly one `finished` per cell across all its sites. A new property, cell_settled_at_turn_end, requires each cell to be finished or detached when its turn's status is flushed, with open nested ids carried as unknown. The calls and timers bounds become per cell, summed over its sites in the worst case. The simulator yields cells and drives waits across rounds and crashes. Mutation: cell_leaks, a cell left running past its turn.

## 5. Rewriting entries already in the history

On provider errors the Claude loop retries once after transforming the whole pending history. It drops thinking blocks whose signatures belong to another organization (agent.rs:7563-7586). It turns refused compaction blocks into plain-text summaries (7587-7608), and strips appended tool-change blocks while resending the current tool list (7609-7643). It cuts the unfinished server turn out as evidence (2862-2884, 7688, 7975), and omits images when a continuation produced at most one token (8256). Codex repairs a rejected request by replacing images or a tool definition in history (model/run/turn.rs:120, 337, 939-945) and reconciles reasoning items after a repair (turn.rs:969). Local compaction drops the first history item on context_exceeded and retries (lifecycle.rs:530-536).

Why not: A list changes only through push, clear, `keep L last n [after e]` and `keep L blobs last N` (LANGUAGE.md, Statements, clear and keep, Blobs). No statement maps or filters the entries a list already holds, such as replacing a field of every assistant(...) entry or removing entries of one variant. A provider-error recovery that edits the history is not writable. Only dropping the first entry can be approximated with keep.

Smallest addition: A statement `rewrite L { pattern => entry | drop, ... }`: a pure, total map over a state list, with patterns on entry and reply variants and no effects in the arms. The prover sets the new length to at most the old one, or equal when no arm drops, and checks text bounds such as cut lengths through the arm expressions. It writes no record: state is derived and the checkpoint written with the status holds the result, so checkpoint_compacted already covers it. The simulator replays it, with the mutation rewrite_live_only (the live run rewrites but replay does not), caught by checkpoint_compacted and replay_equal.

## 7. Forking or rewinding a session at a boundary

Claude forks a session, or a side conversation, from the latest boundary, a completed turn or a checkpoint (agent.rs:3190-3257, fork_point 4292-4311). Boundaries are published per round (agent.rs:4245-4265). The child's state is scrubbed: no tasks, pending continuation, previous message id or container, lifecycle_started false so SessionStart hooks run again, and auto-compaction off for side conversations. Its first record is a checkpoint under a branched policy. A rewind picks an earlier checkpoint, keeps admitted tool ids and recovery notices, and adds a notice that effects from discarded turns may still exist (agent/durable.rs:1640-1675). Codex publishes a fork snapshot before every model call (model/run/turn.rs:834-841, 922) and branches sessions (agent/driver/branch.rs:97-160).

Why not: spawn starts a fresh session of another handler, with only typed arguments (LANGUAGE.md, Spawn and join). A side question reads a snapshot but writes nothing. Nothing creates a session of the same harness that starts from another session's state at a chosen finished turn, and no hook decides what a fork keeps and resets.

Smallest addition: A `fork name at latest|turn` tool, external like spawn and keyed T.S. The child's journal starts with a `forked` record and a checkpoint holding the parent's state at that turn, taken from the parent's durable journal as a side question's snapshot is. An `on forked(origin: text)` hook may assign, clear, keep and push but run no effects, and resets fields such as job handles or flags. The checker counts forks as sessions in the spawn tree's worst case. The simulator checks fork_state_equal (the child's initial state is the replay of the parent's durable journal at that turn, after the hook). Mutation: fork_live_state, which forks from the running turn's state.

## 8. Request settings held as session state

Claude keeps effort, thinking, fast mode and the auto-compact limit as session settings that hosts change between turns (agent.rs:8939-8997). A turn pins the effort in effect, and when the setting differs from the history it appends an effort-change message (agent.rs:7420-7442). Context-recovery compaction overrides thinking and effort for that one request (agent.rs:6383-6405). Codex appends a reasoning sampling update when the effort changes (model/run/turn.rs:906-916). It takes set_thinking, set_auto_compact_token_limit, set_fast_mode and service tier (agent/backend.rs:330-370), and picks a separate compaction effort (lifecycle.rs:118-135). The model itself is fixed per thread in both (agent.rs:8629-8647, codex agent/driver/control.rs:536-561).

Why not: A model call carries at most one `mode`, a name fixed at the call site (LANGUAGE.md, model). It cannot be a state value, and modes from separate dimensions (effort by fast by text_only or refresh) do not combine, so each combination needs its own call site. Thresholds and pins can be state fields, but the request settings the host must send cannot follow them.

Smallest addition: Typed model settings: `model m ... settings effort: effort_enum, fast: bool`, and `m(context) with {effort: e}`, where e is any expression of that text enum or bool and omitted settings take declared defaults. The model record journals the settings as it journals mode, and a replay whose settings differ diverges. The worst case is unchanged, since there is still one site. The simulator draws settings changes between turns through ordinary handlers and checks settings_sent, the analogue of mode_sent. Mutation: settings_dropped.
