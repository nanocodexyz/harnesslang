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


## 10. What codex_agent.hl on hl-agent leaves out of stock Codex

`hl-agent` now hosts `examples/codex_agent.hl` against the OpenAI Responses API (`agent/src/openai.rs`, `agent/src/codex_host.rs`, `agent/src/codex.rs`). The harness follows openai/codex 81b995a's `codex-rs`: gpt-5.5's instructions from `models-manager/models.json`, the `exec_command`, `write_stdin`, freeform `apply_patch` and `update_plan` tools (`core/src/tools/handlers/shell_spec.rs`, `apply_patch_spec.rs`, `plan_spec.rs`, the grammar in `core/assets/tools/apply_patch.lark`), stateless requests with encrypted reasoning (`core/src/client.rs`, `store: false`, `include: reasoning.encrypted_content`), five stream retries from 200 ms doubling and honouring the server's delay (`model-provider-info`, `async-utils/src/backoff.rs`, `core/src/responses_retry.rs`), the turn ending when a reply has no calls, no pending steer and no `end_turn: false` (`core/src/session/turn.rs`), compaction at nine tenths of the window keeping the user's messages (`protocol/src/openai_models.rs`, `core/src/compact.rs`), and the `<turn_aborted>` guidance after a cancel (`core/src/context/turn_aborted.rs`). nanocode's Codex backend (`crates/backends/codex`) offers the same five tools (`builtin_tools/standard.rs`). No real run has been made yet (no API key on the Hand), so the evidence below is the source and the fake Responses API's traces; the ranking is by how much of a real task each difference can change. A difference this lane closed is removed and the others keep their numbers: `tty: true` sessions now run under a PTY through the platform's `script`.

1. **Code mode for the newest models.** Every gpt-6 and gpt-5.6 entry of Codex's catalog has `tool_mode: code_mode_only`: the model gets a JavaScript `exec` cell that calls the tools, not the tools themselves. codex_agent.hl gives every model gpt-5.5's direct tools. Fix: a code-mode variant of the harness on `code` cells (LANGUAGE.md, Code mode), chosen by the model's catalog entry.
2. **External calls of one reply run one after another.** Codex runs a reply's parallel-safe calls concurrently (`core/src/tools/parallel.rs`); here a parallel for runs each external call alone after a flush (gap 2). In the fake API's smoke run two `exec_command` calls of one reply ran back to back (0.145 s, then 0.101 s). Fix: gap 2's `parallel when`.
3. **Local compaction only.** With an OpenAI provider Codex compacts through the Responses compact endpoint and keeps an encrypted compaction item (`core/src/compact_remote_v2.rs`); here the model writes a handoff summary from the local prompt. Fix: a host mode `compact` whose reply carries the compaction item in the reply's items.
5. **Sessions end with the process.** Codex keeps unified exec processes across turns ("may still be running in the background", `turn_aborted.rs`); hl-agent stops every session when its run ends, and a session started before a crash is gone (`write_stdin` answers "Unknown process id"). Fix: sessions as `jobs` with recovery, as Bash's background jobs already are.
6. **Retry details.** Codex retries `server_is_overloaded` only on the server's advice and connection failures without bound behind a feature flag, and falls back from WebSockets to HTTPS; hl-agent backs off on every throttled or transient attempt, at most five times, over HTTPS only.
7. **Missing tools.** `view_image` and the hosted `web_search` tool are not offered; sandboxing and approvals are not modelled (evals run in containers, where Codex runs without a sandbox too).

## Gaps measured against stock Claude Code

The sections above compare harnesslang with nanocode's source. This one compares what the agents do: hl-agent (claude_code.hl), nanocode and stock Claude Code 2.1.325 on the same 40 tasks, mined from the meter's records of every request and reply. `evals/gaps/mine.py DIR...` summarizes rows per agent (tool calls per reply, timeouts, background runs, how a turn ends, what the last commands checked, cache reads, dropped requests); `evals/gaps/transcript.py ROW` prints one row's exchange. Paths are under `output/eval/` of the eval-head runs (head, cc, main, second, fix, p-high, p-medium: 235 hl-agent, 140 nanocode and 38 Claude Code attempts scored). The per-agent figures in the next paragraph are from head and cc, the 40-task head-to-head.

What does not differ: tool calls per reply (Claude Code 0.99, hl-agent 0.98, nanocode 0.89 exec cells), replies with more than one call (7.8%, 5.9%, 0%), explicit Bash timeouts (0.70 and 0.68 per attempt), sleeps (0.30, 0.33), a check among the last three commands (70%, 80%, 85%), compaction (one attempt each shrank its history), and how a turn ends (end_turn or the agent timeout). The gaps are in what the agents do to the task's state, not in the loop. Ranked by attempts lost:

1. **Inputs destroyed by the first probe (closed).** Claude Code's system prompt has "Executing actions with care": prefer a reversible step, move things aside, measure twice. Its db-wal-recovery attempt read the files with `xxd` first, saw the WAL was XORed and passed in 9 calls (`cc/rows/terminal-bench-2.1.db-wal-recovery.claude-code.1`). hl-agent passed 1 of 5 and nanocode 0 of 3: in all seven failing attempts an early command (the second to fourth) opened `main.db` with sqlite3, which deleted the unreadable WAL on close, and the rest of the attempt searched the overlay filesystem and block devices for it (`head/rows/...db-wal-recovery.hl-agent.1` 59 calls to the timeout, `p-medium/rows/...hl-agent.1`, `second/rows/...hl-agent.2`, `second/rows/...nanocode.2`). Fix: an "# Inputs" block in `agent/prompts/claude-code-system.json`: read-only tools first, `cp -a` before anything that can write to an input, naming SQLite's WAL.
2. **The deliverable checked another way than its user checks it (closed).** sqlite-with-gcov failed in every arm (hl-agent 0 of 5, Claude Code 0 of 1, nanocode 1 of 3) with "sqlite3 not found in PATH": the agents installed under `/app/sqlite` or exported PATH in their own shell. polyglot-c-py: two hl-agent failures said "Python isn't installed in this sandbox, but the C side is fully verified" and stopped (`fix/rows/...polyglot-c-py.hl-agent.1`, `main/rows/...`); Claude Code and nanocode installed python3 and ran both halves. git-multibranch: hl-agent checked SSH with a key and PAM by reading `/etc/shadow`, the verifier pushes with the password through expect and failed (`head/rows/...git-multibranch.hl-agent.1`). Fix: the Finishing block now names a fresh login shell (`env -i bash -lc`), says a program the task puts on PATH goes in /usr/local/bin, says to install a missing interpreter and never report an unrun part as working, and to test a service through its user's protocol, account and authentication.
3. **Cleanup that undoes the result (closed).** configure-git-webserver: hl-agent's smoke test pushed hello.html, then it removed `/var/www/gitdeploy/hello.html` "so the repo is pristine for the real user"; the verifier curled it and got 404 (`head/rows/...configure-git-webserver.hl-agent.1`). Claude Code and nanocode left it. Fix: the Finishing block says to keep what the task produced and remove only scratch files.
4. **Long requests on a dropping egress (open, host).** On this Hand the egress drops streams: 89 of 3,109 hl-agent requests (2.9%, 16,899 s), 51 of 1,734 nanocode (2.9%), 7 of 459 Claude Code (1.5%). 22 of hl-agent's 28 requests longer than 300 s were dropped, and largest-eigenval lost every hl-agent attempt this way (6 of 6 timed out after 2 to 4 dropped requests that took 900 to 1,025 s together; `head/rows/...largest-eigenval.hl-agent.1` ran 3 requests, the last 742 s). Claude Code sends `max_tokens` 32,000 (hl-agent and nanocode 128,000), which bounds one request's thinking, and after a dropped stream it retries once without streaming (`stream: false`; `cc/rows/...cobol-modernization.claude-code.1` got a 200 that way). No hl-agent reply has ever used more than 30,149 output tokens. Fix, in hl-agent's host (`agent/src/anthropic.rs`), outside claude_code.hl: Claude Code's 32,000 default and one non-streaming retry after a stream drops.
5. **Request shape (open, small).** Claude Code and nanocode send `context_management: clear_thinking_20251015 keep all` and Claude Code `thinking.display: updates`; hl-agent sends neither. Claude Code's system prompt is 27,807 characters and its Bash description 12,809 (hl-agent 4,985 and 3,259); the extra text is git, memory and task-tool guidance that none of these tasks needed. Cache reads are 94% of Claude Code's prompt tokens, 89% of hl-agent's and 91% of nanocode's; hl-agent's lower share comes from shorter attempts where the 4,000-token prefix is a larger part. No attempt in the traces turns on these.
6. **Delegated answers accepted unchecked (open).** count-dataset-tokens: hl-agent handed the whole task to one subagent and wrote its 79566 without reading how it was counted; the expected 79586 is what Claude Code got by counting the three science domains itself (`head/rows/...count-dataset-tokens.hl-agent.1`). hl-agent spawned a subagent in 5 of 247 rows with a reply, all on this task, 4 of them passing, so this is one attempt.

7. **Thinking is most of the bill (changed: effort low by default).** Output tokens were 52% of hl-agent's cost at effort high, three quarters of them thinking, and thinking stays in the tool loop's context to be written and read again; Claude Code (effort high, `max_tokens` 32,000) spends the same 9,000 output tokens an attempt. At effort low hl-agent passed 29 of 30 attempts on 10 tasks at $0.090 an attempt against 30 of 30 at $0.245 (DECISIONS.md, eval-cost). Open: on count-dataset-tokens, git-multibranch and polyglot-c-py low passed 0 of 5 against 6 of 10 at high (polyglot-c-py stopped after 4 requests without running its Python half), so the full suite decides whether low or medium is the default.

Not harness gaps: cancel-async-tasks fails in every arm (0 of 8, 0 of 1, 1 of 4) on cleanup after a SIGINT that no agent tests; query-optimize and raman-fitting fail on a runtime margin and a fit in every arm.

Reruns of the closed gaps (hl-agent only, claude-sonnet-4-6, 2 attempts on the five tasks, then 3 on the two that still failed; paths under the gap-mining lane's `output/gaps/`). With the Inputs block and the first Finishing changes (run1, run2): db-wal-recovery 4 of 4 (before the fix 1 of 5), sqlite-with-gcov 2 of 4 (0 of 5), configure-git-webserver 3 of 4 (4 of 5), git-multibranch 3 of 4 (3 of 5), polyglot-c-py 2 of 4 (4 of 8); the five together 14 of 20 against 12 of 28, $4.41 for 20 attempts. The Finishing block then named the two failures it still let through (installing python3 with apt-get, and keeping what a smoke test produced): configure-git-webserver 3 of 3, polyglot-c-py 1 of 3 (run4, $1.44). A regression run on six tasks hl-agent passed every time before (run3: fix-git, git-leak-recovery, sqlite-db-truncate, build-pmars, regex-log, sanitize-git-repo, 1 attempt each, $1.57) passed 5 of 6; fix-git, 6 of 6 before, failed once. Status: 1 and 3 hold; 2 holds for sqlite-with-gcov and git-multibranch but polyglot-c-py still fails 4 of its 7 attempts since, so it stays open; 4 and 5 are open.

# What nanocode's MPP does that harnesslang cannot write

`wallet` and `paid by` (LANGUAGE.md, Payments) cover a 402 on a tool call, an
MCP tool of a listing and an HTTP fetch tool, paid as one-shot charges or session
vouchers, each payment an external recoverable call of the wallet, the spend cap a
proved invariant, and paid calls in parallel lanes. The list below comes from a read
of nanocode d83b08f's `crates/integrations/mpp` and nanocode-ts d5b21f1's
`src/machines`, ranked by how much of a real paying harness each blocks.

## 1. Paid model calls

nanocode pays the Tempo gateway's 402 on Messages and Responses requests through the
same flow, keeps the gateway's `payment-failed` and `verification-failed` 402s in
Claude Code's backoff (claude client lib.rs) and fails the turn after the last attempt.
Here `paid by` is refused on a model: the payment desugaring keeps the last attempt's
result in hidden state fields (a tag, the value, the failure text), which holds a tool's
`maybe<T>` but not a model's `reply` or `attempt`, whose variants carry lists and usage.
Smallest addition: a paid site's result slot as a typed hidden local the backends declare
mutable before the payment loop, so a paid model call desugars as a paid tool does, with
the credential carried in a hidden model setting the way `with {...}` carries settings.

## 2. Settlement and receipts

nanocode's `PendingPayment` commits, rolls back, invalidates (410 Gone) or abandons each
credential by how the server answered (flow.rs), and records the `payment-receipt`
header in its `PaymentResponse` event. Here the language tells the wallet only to pay; `hl-agent pay`'s host payer
settles each credential by nanocode's rules and records the receipt and the
`PaymentResponse` event in its own ledger (`scripts/mpp-drill.sh` catches
`receipt_on_refusal`), but the harness journal holds neither, so a program cannot
read a receipt and another host may settle differently.
Smallest addition: a `none` call of the wallet after each presentation carrying the
presentation's outcome and receipt, so the receipt is journaled once and a mutation that
settles a refused payment as committed (nanocode-sim's `receipt_on_refusal`) is caught.

## 3. Session channels beyond a voucher per 402

nanocode's `SessionProvider` opens and tops up an on-chain channel, sends prepaid
vouchers without a 402 round trip, retries once after a stale channel's 410 and closes
the channel (session.rs, store.rs). Here a session challenge is paid like a charge and the
channel lives in the wallet host; the program cannot see deposits or close a channel.

## 4. Challenge choice

nanocode ranks challenges by `Accept-Payment` preferences, refuses expired ones and a
zero-amount proof for another realm, and nanocode-ts orders MACH sessions first. Here the
wallet host chooses; the program cannot state a preference.

## 5. Payments made inside a host tool

nanocode's forward proxy lets shell commands pay `*.mpp.tempo.xyz` (proxy.rs) and its
identity pass answers an email-attestation challenge (identity.rs). A bash tool here that
pays through such a proxy pays outside the journal and the cap.

## 6. Spend across sessions

`spent()` is one session's: a fork copies the parent's spend and spends on from it, and a
spawned child has a cap of its own, so a session family can spend its cap once per session.
nanocode's wallet spends from one balance per key. Smallest addition: a family cap carried
in the spawn tool's `sessions at most` bound, with the child's spend reported in its result.

## 7. A real wallet in hl-agent

hl-agent has no wallet host: signing a Tempo payment needs secp256k1 and Keccak, which a
zero-dependency hl-agent would have to carry itself, and no payment may spend real funds.
The simulator's wallet and the kill -9 drill's host stand in for it.
