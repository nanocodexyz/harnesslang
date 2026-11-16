# Eval results

Every row is one attempt of one agent on one task, run by `evals/run.py` in the task's own container with `--limits none` (Docker here cannot apply cgroup limits), scored by the task's verifier, and metered by `evals/meter.py`, which relays every model request and records its usage. Cost is the metered usage at list prices (`evals/report.py` names its sources: the Anthropic pricing page read 2026-11-14 and the gpt-5.5 model page read 2026-11-15). Pass rates leave errored rows out (no model request succeeded); intervals are 95% Wilson; the sign test is exact and two-sided over the pairs of the same task and attempt number where exactly one agent passed.

Suites: suite 1 is `evals/suite.txt` (3 nanoeval tasks and 17 Terminal-Bench 2.1 tasks), suite 2 is `evals/suite2.txt` (20 more Terminal-Bench 2.1 tasks). Model: claude-sonnet-4-6 for every arm below.

Heavy load: every run in this file was made on a shared 5-core Hand while other builds, checks and eval runners were running. The head-to-head ran at a 1-minute load average of 20 to 33; the three-attempt run's sampled load averaged 21 (peak 57). Wall times and the 900 s agent timeout are therefore not comparable with a quiet machine, and a timeout counts as a failure.

## Agents

| Agent | Version | Notes |
| --- | --- | --- |
| hl-agent | claude_code.hl on hl-agent, built at d6c75ed for the head-to-head and at e378df9 for the three-attempt run (same harness and prompt: the commits between them touch evals and docs only), effort high | system prompt with the Finishing block; the Bash kill fix |
| nanocode | c7679e3, its own Claude agent | |
| claude-code | stock Claude Code 2.1.325, `claude -p --dangerously-skip-permissions --output-format stream-json` | runs on the image's own libc; sends its own 27,807-character system prompt and `max_tokens` 32,000 |
| codex | stock Codex 0.169.0 (`codex exec`, static musl build) | not run on a real model: no OpenAI API key on this Hand |

## Head-to-head: hl-agent, nanocode and stock Claude Code, 1 attempt each (heavy load)

nanocode and hl-agent interleaved in one pool (`output/eval/head`), Claude Code on the same 40 tasks in the same hour (`output/eval/cc`), 2026-11-14.

### suite 1 (evals/suite.txt), 20 tasks:

| Agent | Passed / scored (errored) | Pass rate (95% Wilson) | Cost/attempt | Wall/attempt |
| --- | ---: | --- | ---: | ---: |
| hl-agent | 18 / 20 (0) | 90.0% (69.9-97.2%) | $0.174 | 165 s |
| nanocode | 16 / 20 (0) | 80.0% (58.4-91.9%) | $0.287 | 248 s |
| claude-code | 19 / 20 (0) | 95.0% (76.4-99.1%) | $0.283 | 175 s |

| Pair | Pairs scored | Only the first passed | Only the second passed | Sign test p |
| --- | ---: | ---: | ---: | ---: |
| hl-agent vs nanocode | 20 | 2 | 0 | 0.500 |
| hl-agent vs claude-code | 20 | 0 | 1 | 1.000 |
| claude-code vs nanocode | 20 | 3 | 0 | 0.250 |

### suite 2 (evals/suite2.txt), 20 tasks:

| Agent | Passed / scored (errored) | Pass rate (95% Wilson) | Cost/attempt | Wall/attempt |
| --- | ---: | --- | ---: | ---: |
| hl-agent | 10 / 19 (1) | 52.6% (31.7-72.7%) | $0.378 | 421 s |
| nanocode | 11 / 20 (0) | 55.0% (34.2-74.2%) | $0.287 | 418 s |
| claude-code | 10 / 18 (2) | 55.6% (33.7-75.4%) | $0.299 | 341 s |

| Pair | Pairs scored | Only the first passed | Only the second passed | Sign test p |
| --- | ---: | ---: | ---: | ---: |
| hl-agent vs nanocode | 19 | 2 | 3 | 1.000 |
| hl-agent vs claude-code | 18 | 3 | 4 | 1.000 |
| claude-code vs nanocode | 18 | 2 | 2 | 1.000 |

### both, 40 tasks:

| Agent | Passed / scored (errored) | Pass rate (95% Wilson) | Cost/attempt | Wall/attempt |
| --- | ---: | --- | ---: | ---: |
| hl-agent | 28 / 39 (1) | 71.8% (56.2-83.5%) | $0.276 | 293 s |
| nanocode | 27 / 40 (0) | 67.5% (52.0-79.9%) | $0.287 | 333 s |
| claude-code | 29 / 38 (2) | 76.3% (60.8-87.0%) | $0.291 | 258 s |

| Pair | Pairs scored | Only the first passed | Only the second passed | Sign test p |
| --- | ---: | ---: | ---: | ---: |
| hl-agent vs nanocode | 39 | 4 | 3 | 1.000 |
| hl-agent vs claude-code | 38 | 3 | 5 | 0.727 |
| claude-code vs nanocode | 38 | 5 | 2 | 0.453 |

## Suite 1, three attempts: hl-agent, nanocode and stock Claude Code (heavy load)

20 tasks x 3 attempts x 3 agents, 180 rows, in `output/eval/s1x3`, 2026-11-15 06:00 to 07:47 UTC, parallel 4 under `nice`, 1-minute load average 14 to 26. Attempts 1 and 2 of 59 rows come from the int-evals run `output/eval/r5` (same builds, same flags), copied in; `run.py` ran the other 121. Builds: hl-agent at 1672187 (claude_code.hl, effort high), nanocode c7679e3, Claude Code 2.1.325. Costs are recomputed from each row's raw `responses.jsonl` usage with the fixed `usage_of` (the meter on master recorded 0 cache tokens for every Claude reply; see DECISIONS.md), so they are higher than the meter fields in `result.json` and than the earlier tables in this file, which used the broken meter.

| Agent | Passed / scored (errored) | Pass rate (95% Wilson) | Cost/attempt | Wall/attempt | Requests/attempt |
| --- | ---: | --- | ---: | ---: | ---: |
| hl-agent | 52 / 60 (0) | 86.7% (75.8-93.1%) | $0.224 | 183 s | 11.7 |
| nanocode | 53 / 60 (0) | 88.3% (77.8-94.2%) | $0.227 | 171 s | 8.4 |
| claude-code | 52 / 59 (1) | 88.1% (77.5-94.1%) | $0.201 | 158 s | 9.1 |

| Pair | Pairs scored | Only the first passed | Only the second passed | Sign test p |
| --- | ---: | ---: | ---: | ---: |
| hl-agent vs nanocode | 60 | 1 | 2 | 1.000 |
| hl-agent vs claude-code | 59 | 2 | 3 | 1.000 |
| claude-code vs nanocode | 59 | 4 | 4 | 1.000 |

Per task, the arms differ on 7 of 20 tasks: cancel-async-tasks (nanocode 1/3, Claude Code 0/3, hl-agent 0/3), fix-git (1/3, 3/3, 2/3), largest-eigenval (0/3, 2/3, 0/3, the hl-agent and nanocode failures are 900 s timeouts), polyglot-c-py (3/3, 2/3, 2/3), build-cython-ext and cobol-modernization (Claude Code 2/3, the others 3/3), regex-log (Claude Code 2/2 with one row errored: no model request succeeded). Paired by task and attempt, hl-agent minus nanocode: cost -$0.003 (-0.065 to 0.060), wall +12.6 s (-7.9 to 33.0), requests +3.3 (1.0 to 5.6).

## Suite 1, three attempts: hl-agent at effort low and medium (eval-rerun lane, heavy load)

20 tasks x 3 attempts, 2026-11-15 08:24 to about 09:25 UTC, parallel 4 under `nice` on the shared 5-core Hand (1-minute load average 17 to 25). hl-agent built at 3679d15 (master with eval-cost: effort low by default), run as `hl-agent` (no flag, every request `effort: low`) and as `hl-agent-medium` (`--effort medium`); 120 new rows, metered with the fixed meter. The nanocode and Claude Code rows are the 120 of the eval-runner run above (`output/eval/s1x3r`, re-costed from raw usage), copied in, so every arm is costed the same way. `python3 evals/report.py output/eval/s1r`:

| Agent | Attempts | Passed | Failed | Errored | Pass rate (95% Wilson) | Cost/attempt | Wall/attempt | Requests/attempt | Input+cache tokens/attempt | Output tokens/attempt |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| hl-agent (effort low) | 60 | 57 | 3 | 0 | 95.0% (86.3-98.3%) | $0.082 | 64 s | 8.6 | 79,288 | 2,804 |
| hl-agent-medium | 60 | 53 | 7 | 0 | 88.3% (77.8-94.2%) | $0.172 | 132 s | 10.9 | 163,072 | 6,114 |
| nanocode | 60 | 53 | 7 | 0 | 88.3% (77.8-94.2%) | $0.227 | 171 s | 8.4 | 226,369 | 7,601 |
| claude-code | 60 | 52 | 7 | 1 | 88.1% (77.5-94.1%) | $0.201 | 158 s | 9.1 | 264,552 | 5,145 |

Paired by task and attempt (exact two-sided sign test on the discordant pairs; cost is the paired mean difference with its 95% interval):

| Pair | Pairs scored | Only the first passed | Only the second passed | p | Cost difference/attempt |
| --- | ---: | ---: | ---: | ---: | --- |
| hl-agent (low) vs nanocode | 60 | 6 | 2 | 0.289 | -$0.144 (-0.206 to -0.082) |
| hl-agent (low) vs claude-code | 59 | 6 | 2 | 0.289 | -$0.118 (-0.161 to -0.075) |
| hl-agent-medium vs nanocode | 60 | 3 | 3 | 1.000 | -$0.054 (-0.091 to -0.018) |
| hl-agent-medium vs claude-code | 59 | 3 | 3 | 1.000 | -$0.029 (-0.077 to 0.019) |
| hl-agent (low) vs hl-agent-medium | 60 | 5 | 1 | 0.219 | -$0.090 (-0.133 to -0.047) |

Losses (rows where the other arm of the pair passed and this one failed):

- low vs nanocode: cancel-async-tasks.3, polyglot-c-py.3
- low vs claude-code: fix-git.1, polyglot-c-py.3
- medium vs nanocode: cancel-async-tasks.3, openssl-selfsigned-cert.3, polyglot-c-py.1
- medium vs claude-code: fix-git.1, largest-eigenval.3 (agent timeout), openssl-selfsigned-cert.3
- low vs medium: polyglot-c-py.3; medium vs low: cancel-async-tasks.1, cancel-async-tasks.2, largest-eigenval.3, openssl-selfsigned-cert.3, polyglot-c-py.1

Tasks where any arm lost an attempt (passed/scored; the other 12 tasks are 3/3 for every arm):

| Task | nanocode | claude-code | hl-agent (low) | hl-agent-medium | low $/attempt | medium $/attempt |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| build-cython-ext | 3/3 | 2/3 | 3/3 | 3/3 | $0.360 | $0.686 |
| cancel-async-tasks | 1/3 | 0/3 | 2/3 | 0/3 | $0.042 | $0.055 |
| cobol-modernization | 3/3 | 2/3 | 3/3 | 3/3 | $0.158 | $0.489 |
| fix-git | 1/3 | 3/3 | 2/3 | 2/3 | $0.042 | $0.080 |
| largest-eigenval | 0/3 | 2/3 | 3/3 | 2/3 | $0.250 | $0.757 |
| openssl-selfsigned-cert | 3/3 | 3/3 | 3/3 | 2/3 | $0.055 | $0.060 |
| polyglot-c-py | 3/3 | 2/3 | 2/3 | 2/3 | $0.071 | $0.189 |
| regex-log | 3/3 | 2/2 | 3/3 | 3/3 | $0.126 | $0.317 |

The full report, with per-task cost, wall, requests and tool calls for every arm, is `output/eval/s1r/report.txt` of the lane (not committed).

## Three attempts: hl-agent and nanocode (heavy load, still running)

The eval-head lane's rerun with 3 attempts per task in one pool (its `output/eval/r3`, started 2026-11-15 01:26 UTC, parallel 6), the same builds as the head-to-head. 191 of 240 rows had finished when this table was made; it is replaced when the run completes. Rows are paired by task and attempt number.

### suite 1 (evals/suite.txt), 20 tasks:

| Agent | Passed / scored (errored) | Pass rate (95% Wilson) | Cost/attempt | Wall/attempt |
| --- | ---: | --- | ---: | ---: |
| hl-agent | 51 / 56 (0) | 91.1% (80.7-96.1%) | $0.170 | 158 s |
| nanocode | 46 / 55 (0) | 83.6% (71.7-91.1%) | $0.176 | 168 s |

| Pair | Pairs scored | Only the first passed | Only the second passed | Sign test p |
| --- | ---: | ---: | ---: | ---: |
| hl-agent vs nanocode | 55 | 5 | 1 | 0.219 |

### suite 2 (evals/suite2.txt), 20 tasks:

| Agent | Passed / scored (errored) | Pass rate (95% Wilson) | Cost/attempt | Wall/attempt |
| --- | ---: | --- | ---: | ---: |
| hl-agent | 24 / 37 (3) | 64.9% (48.8-78.2%) | $0.374 | 405 s |
| nanocode | 22 / 40 (0) | 55.0% (39.8-69.3%) | $0.390 | 459 s |

| Pair | Pairs scored | Only the first passed | Only the second passed | Sign test p |
| --- | ---: | ---: | ---: | ---: |
| hl-agent vs nanocode | 37 | 4 | 2 | 0.688 |

### both, 40 tasks:

| Agent | Passed / scored (errored) | Pass rate (95% Wilson) | Cost/attempt | Wall/attempt |
| --- | ---: | --- | ---: | ---: |
| hl-agent | 75 / 93 (3) | 80.6% (71.5-87.4%) | $0.255 | 261 s |
| nanocode | 68 / 95 (0) | 71.6% (61.8-79.7%) | $0.266 | 291 s |

| Pair | Pairs scored | Only the first passed | Only the second passed | Sign test p |
| --- | ---: | ---: | ---: | ---: |
| hl-agent vs nanocode | 92 | 9 | 3 | 0.146 |

## Effort low: hl-agent cost on 10 tasks x 3 (eval-cost lane)

hl-agent with effort low (now the default) on regex-log, sqlite-db-truncate, vulnerable-secret, extract-elf, fix-code-vulnerability, headless-terminal, multi-source-data-merger, build-pmars, prove-plus-comm and pytorch-model-recovery, 3 attempts each, parallel 2, against the same tasks' rows of the three-attempt run above (effort high), 2026-11-15 (`output/eval/low` of the eval-cost lane).

| Arm | Passed / scored | Cost/attempt | Requests/attempt | Output tokens/attempt |
| --- | ---: | ---: | ---: | ---: |
| hl-agent, effort high | 30 / 30 | $0.245 | 11.2 | 10,069 |
| hl-agent, effort low | 29 / 30 | $0.090 | 8.8 | 2,961 |

An 8-task regression sample at the default build passed 10 of 10 at $0.045 an attempt; three harder tasks passed 0 of 5 against 6 of 10 at high (DECISIONS.md, eval-cost). Rows metered after about 04:20 UTC 2026-11-15 by a meter without the eval-cost fix record no cache tokens; `evals/remeter.py` recomputes them from `responses.jsonl`.

## The closed gaps on the full suite (running)

hl-agent at this branch (the Inputs and Finishing blocks, the `/bin/sh` fallback) against nanocode c7679e3 and Claude Code 2.1.325, 40 tasks x 2 attempts, parallel 3, started 2026-11-15 04:20 UTC under a $50 cap on metered spend, into `output/eval/r5`. Its table goes here when it finishes.

## Earlier runs

Over the eval-head lane's main and second suites (100 attempts per agent each, nanocode and hl-agent in one pool, before the Bash kill fix and the Finishing block), hl-agent passed 78.6% at $0.239 an attempt against nanocode's 73.0% at $0.272, 11 tasks to 7 (p = 0.481). DECISIONS.md (2026-11-14, hl-agent on 40 Terminal-Bench tasks) has the effort pilot and the kill-fix rerun.

## Gap reruns

hl-agent alone on the tasks behind the gaps mined from these traces, before and after each fix: docs/GAPS.md, "Gaps measured against stock Claude Code".

## Stock Codex, hl-agent's codex_agent.hl and nanocode's Codex backend

Not run on a real model: the Hand has no OpenAI API key. The wiring is checked by `evals/codex-drill.sh` against a fake Responses API (each arm on the 3 nanoeval tasks; the meter's key, usage and WebSocket refusal are checked on every row): codex 3/3, nanocode 3/3, hl-agent 3/3 (it was 1/3 until `exec_command` fell back to `/bin/sh` in images without bash). With a key, the comparison is `python3 evals/run.py --suite evals/suite.txt --tasks output/eval/tasks --agent-root output/eval/agent-root --out output/eval/codex --agents codex,nanocode,hl-agent --attempts 1 --model gpt-5.5 --parallel 3 --limits none`, then `python3 evals/report.py output/eval/codex`.

<!-- eval-confirm:begin -->
## eval-confirm (2026-11-15): suite 60 x 3 on claude-sonnet-4-6, then suites 1+2 x 2 on claude-haiku-4-5

Status: final, 446 new rows, real spend on new rows $128.69 (remetered from responses.jsonl; suite-1 rows of hl-agent, nanocode and Claude Code copied from eval-rerun's s1r).

#### claude-sonnet-4-6, all suites

Model: claude-sonnet-4-6. Prices: https://docs.anthropic.com/en/docs/about-claude/pricing, read 2026-11-14; https://developers.openai.com/api/docs/models/gpt-5.5 (prompts under 272K tokens), read 2026-11-15.

| Agent | Attempts | Passed | Failed | Errored | Pass rate (95% Wilson) | Cost | Cost/attempt | Wall/attempt | Requests/attempt | Model tool calls/attempt | Inner tool calls/attempt | Input+cache tokens/attempt | Output tokens/attempt |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| nanocode | 147 | 97 | 50 | 0 | 66.0% (58.0-73.2%) | $50.80 | $0.346 | 499 s | 15.7 | 13.7 | 20.1 | 407585 | 10565 |
| claude-code | 147 | 97 | 42 | 8 | 69.8% (61.7-76.8%) | $48.55 | $0.330 | 440 s | 14.8 | 15.1 | 0.0 | 485174 | 7689 |
| hl-agent | 147 | 119 | 28 | 0 | 81.0% (73.8-86.5%) | $28.89 | $0.197 | 272 s | 13.9 | 12.7 | 11.4 | 174873 | 6382 |

| Task | nanocode pass | nanocode $ | nanocode s | nanocode req | nanocode tools | claude-code pass | claude-code $ | claude-code s | claude-code req | claude-code tools | hl-agent pass | hl-agent $ | hl-agent s | hl-agent req | hl-agent tools |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| extract-todos | 3/3 | 0.018 | 12 | 2.7 | 2.0 | 3/3 | 0.029 | 10 | 3.0 | 0.0 | 3/3 | 0.010 | 9 | 3.0 | 2.0 |
| uppercase-message | 3/3 | 0.018 | 10 | 3.0 | 2.7 | 3/3 | 0.030 | 10 | 3.0 | 0.0 | 3/3 | 0.010 | 9 | 3.0 | 2.0 |
| write-greeting | 3/3 | 0.011 | 8 | 2.0 | 1.0 | 3/3 | 0.022 | 6 | 2.0 | 0.0 | 3/3 | 0.006 | 5 | 2.0 | 1.0 |
| adaptive-rejection-sampler | 0/3 | 0.016 | 900 | 4.0 | 0.0 | 0/1 (2 err) | 0.177 | 900 | 2.7 | 0.0 | 3/3 | 0.472 | 689 | 22.0 | 19.0 |
| bn-fit-modify | 3/3 | 0.254 | 212 | 13.3 | 13.3 | 3/3 | 0.399 | 261 | 17.7 | 0.0 | 3/3 | 0.172 | 161 | 17.0 | 16.0 |
| break-filter-js-from-html | 0/1 | 0.034 | 1200 | 6.0 | 3.0 | 1/1 | 0.680 | 1184 | 15.0 | 0.0 | 1/1 | 0.307 | 271 | 11.0 | 10.0 |
| build-cython-ext | 3/3 | 1.011 | 419 | 40.0 | 73.3 | 2/3 | 0.806 | 340 | 44.3 | 0.0 | 3/3 | 0.360 | 235 | 37.3 | 36.3 |
| build-pmars | 3/3 | 0.153 | 132 | 17.7 | 21.3 | 2/3 | 0.264 | 134 | 22.0 | 0.0 | 2/3 | 0.077 | 91 | 15.0 | 14.0 |
| build-pov-ray | 3/3 | 2.112 | 1069 | 96.0 | 127.7 | 3/3 | 0.679 | 281 | 41.7 | 0.0 | 2/3 | 0.164 | 158 | 25.0 | 24.0 |
| caffe-cifar-10 | 2/3 | 1.195 | 1968 | 87.0 | 131.3 | 3/3 | 2.093 | 2120 | 102.0 | 0.0 | 3/3 | 0.500 | 2097 | 51.7 | 49.7 |
| cancel-async-tasks | 1/3 | 0.107 | 80 | 4.0 | 2.7 | 0/3 | 0.062 | 37 | 2.0 | 0.0 | 2/3 | 0.042 | 35 | 4.0 | 3.0 |
| chess-best-move | 0/3 | 0.638 | 900 | 15.7 | 14.0 | 0/3 | 0.135 | 704 | 3.7 | 0.0 | 0/3 | 0.535 | 524 | 19.0 | 15.0 |
| circuit-fibsqrt | 0/2 | 0.172 | 3610 | 16.5 | 2.0 | 0/2 | 0.037 | 3584 | 12.0 | 0.0 | 1/2 | 0.419 | 2109 | 10.5 | 4.5 |
| cobol-modernization | 3/3 | 0.617 | 453 | 12.0 | 14.7 | 2/3 | 0.656 | 745 | 15.3 | 0.0 | 3/3 | 0.158 | 118 | 11.0 | 11.0 |
| code-from-image | 1/1 | 0.039 | 24 | 4.0 | 3.0 | 1/1 | 0.058 | 20 | 4.0 | 0.0 | 1/1 | 0.020 | 11 | 3.0 | 2.0 |
| compile-compcert | 1/1 | 0.767 | 1143 | 48.0 | 45.0 | 1/1 | 0.818 | 1339 | 50.0 | 0.0 | 1/1 | 0.149 | 1044 | 21.0 | 20.0 |
| configure-git-webserver | 3/3 | 0.110 | 122 | 11.3 | 10.7 | 3/3 | 0.184 | 141 | 16.7 | 0.0 | 3/3 | 0.092 | 171 | 15.7 | 14.7 |
| constraints-scheduling | 1/1 | 0.224 | 519 | 6.0 | 8.0 | 1/1 | 0.161 | 415 | 4.0 | 0.0 | 1/1 | 0.067 | 51 | 3.0 | 2.0 |
| count-dataset-tokens | 1/3 | 0.122 | 132 | 8.7 | 6.7 | 1/3 | 0.147 | 114 | 11.0 | 0.0 | 1/3 | 0.185 | 140 | 15.3 | 14.0 |
| crack-7z-hash | 1/1 | 0.165 | 157 | 21.0 | 28.0 | 1/1 | 0.206 | 139 | 19.0 | 0.0 | 1/1 | 0.044 | 112 | 10.0 | 9.0 |
| custom-memory-heap-crash | 0/1 | 0.426 | 234 | 17.0 | 26.0 | 0/1 | 0.569 | 346 | 16.0 | 0.0 | 0/1 | 0.103 | 87 | 9.0 | 8.0 |
| db-wal-recovery | 0/3 | 0.634 | 900 | 16.7 | 18.3 | 0/3 | 1.093 | 900 | 28.7 | 0.0 | 3/3 | 0.045 | 39 | 6.0 | 5.0 |
| distribution-search | 0/1 | 0.047 | 3611 | 16.0 | 0.0 | 0/0 (1 err) | 0.006 | 3610 | 11.0 | 0.0 | 1/1 | 0.487 | 466 | 13.0 | 9.0 |
| dna-assembly | 0/1 | 0.063 | 1800 | 10.0 | 1.0 | 0/1 | 0.062 | 1800 | 7.0 | 0.0 | 0/1 | 0.917 | 1800 | 21.0 | nan |
| dna-insert | 0/1 | 0.809 | 834 | 30.0 | 76.0 | 0/1 | 0.637 | 408 | 14.0 | 0.0 | 0/1 | 0.216 | 175 | 12.0 | 11.0 |
| extract-elf | 2/3 | 0.395 | 282 | 11.3 | 13.0 | 2/3 | 0.235 | 242 | 8.7 | 0.0 | 3/3 | 0.086 | 67 | 8.3 | 7.3 |
| extract-moves-from-video | 0/1 | 0.548 | 1800 | 29.0 | 39.0 | 0/1 | 2.278 | 1800 | 66.0 | 0.0 | 0/1 | 3.723 | 1800 | 118.0 | nan |
| feal-differential-cryptanalysis | 0/1 | 0.042 | 1810 | 10.0 | 1.0 | 0/1 | 0.029 | 1810 | 6.0 | 0.0 | 1/1 | 0.619 | 1213 | 11.0 | 5.0 |
| feal-linear-cryptanalysis | 0/1 | 0.061 | 1810 | 9.0 | 4.0 | 0/1 | 0.056 | 1810 | 7.0 | 0.0 | 1/1 | 0.328 | 330 | 7.0 | 7.0 |
| filter-js-from-html | 0/1 | 0.904 | 1002 | 17.0 | 21.0 | 0/1 | 0.158 | 493 | 4.0 | 0.0 | 0/1 | 0.067 | 54 | 3.0 | 2.0 |
| financial-document-processor | 1/1 | 0.381 | 295 | 17.0 | 30.0 | 1/1 | 0.353 | 124 | 8.0 | 0.0 | 1/1 | 0.585 | 199 | 49.0 | 20.0 |
| fix-code-vulnerability | 3/3 | 0.417 | 165 | 28.0 | 31.7 | 3/3 | 0.361 | 100 | 20.0 | 0.0 | 3/3 | 0.140 | 61 | 19.0 | 19.3 |
| fix-git | 1/3 | 0.042 | 28 | 4.7 | 6.3 | 3/3 | 0.122 | 43 | 9.3 | 0.0 | 2/3 | 0.042 | 31 | 6.3 | 5.3 |
| fix-ocaml-gc | 1/1 | 0.894 | 1628 | 30.0 | 21.0 | 1/1 | 0.605 | 1153 | 17.0 | 0.0 | 1/1 | 0.297 | 484 | 20.0 | 18.0 |
| gcode-to-text | 1/3 | 0.859 | 752 | 23.3 | 36.3 | 0/3 | 1.147 | 910 | 30.3 | 0.0 | 3/3 | 0.403 | 396 | 24.3 | 23.7 |
| git-leak-recovery | 3/3 | 0.059 | 36 | 5.0 | 15.3 | 3/3 | 0.086 | 29 | 7.7 | 0.0 | 3/3 | 0.022 | 20 | 5.0 | 4.0 |
| git-multibranch | 3/3 | 0.246 | 150 | 20.7 | 29.7 | 2/3 | 0.269 | 110 | 22.3 | 0.0 | 2/3 | 0.121 | 94 | 16.0 | 15.0 |
| gpt2-codegolf | 0/1 | 0.015 | 900 | 4.0 | 0.0 | 0/0 (1 err) | 0.017 | 900 | 2.0 | 0.0 | 0/1 | 0.033 | 42 | 1.0 | 0.0 |
| headless-terminal | 3/3 | 0.454 | 340 | 13.0 | 10.3 | 3/3 | 0.134 | 80 | 7.0 | 0.0 | 3/3 | 0.045 | 43 | 5.3 | 4.3 |
| hf-model-inference | 3/3 | 0.092 | 81 | 9.0 | 16.0 | 3/3 | 0.094 | 61 | 8.0 | 0.0 | 3/3 | 0.074 | 79 | 11.7 | 10.7 |
| kv-store-grpc | 3/3 | 0.064 | 46 | 6.3 | 9.0 | 3/3 | 0.089 | 44 | 7.3 | 0.0 | 3/3 | 0.049 | 43 | 9.7 | 8.7 |
| largest-eigenval | 0/3 | 0.884 | 911 | 25.3 | 24.0 | 2/3 | 0.396 | 611 | 5.7 | 0.0 | 3/3 | 0.250 | 177 | 22.0 | 22.0 |
| log-summary-date-ranges | 3/3 | 0.070 | 35 | 4.3 | 5.0 | 3/3 | 0.070 | 24 | 4.0 | 0.0 | 3/3 | 0.032 | 20 | 3.0 | 2.0 |
| modernize-scientific-stack | 3/3 | 0.082 | 62 | 4.0 | 6.3 | 3/3 | 0.059 | 24 | 4.0 | 0.0 | 3/3 | 0.056 | 43 | 5.0 | 6.0 |
| multi-source-data-merger | 3/3 | 0.089 | 55 | 4.7 | 7.7 | 3/3 | 0.131 | 60 | 7.3 | 0.0 | 3/3 | 0.058 | 42 | 5.7 | 5.0 |
| nginx-request-logging | 3/3 | 0.078 | 61 | 5.7 | 15.0 | 3/3 | 0.127 | 60 | 10.7 | 0.0 | 3/3 | 0.046 | 49 | 7.7 | 6.7 |
| openssl-selfsigned-cert | 3/3 | 0.060 | 42 | 2.7 | 13.0 | 3/3 | 0.102 | 34 | 9.3 | 0.0 | 3/3 | 0.055 | 41 | 6.0 | 5.0 |
| polyglot-c-py | 3/3 | 0.178 | 142 | 6.7 | 23.0 | 2/3 | 0.152 | 208 | 6.7 | 0.0 | 2/3 | 0.071 | 70 | 6.7 | 5.7 |
| prove-plus-comm | 3/3 | 0.080 | 54 | 5.3 | 5.0 | 3/3 | 0.150 | 67 | 6.7 | 0.0 | 3/3 | 0.022 | 19 | 5.0 | 4.0 |
| pypi-server | 3/3 | 0.103 | 70 | 9.7 | 18.0 | 3/3 | 0.149 | 66 | 13.0 | 0.0 | 3/3 | 0.042 | 51 | 7.7 | 6.7 |
| pytorch-model-cli | 1/3 | 0.449 | 800 | 32.0 | 30.3 | 2/3 | 0.709 | 757 | 35.3 | 0.0 | 0/3 | 0.222 | 431 | 25.3 | 23.7 |
| pytorch-model-recovery | 2/3 | 0.320 | 630 | 14.7 | 10.3 | 0/0 (3 err) | 0.000 | 1 | 0.0 | 0.0 | 3/3 | 0.150 | 809 | 15.0 | 13.7 |
| query-optimize | 0/3 | 0.381 | 584 | 15.3 | 19.7 | 2/3 | 0.334 | 271 | 12.7 | 0.0 | 0/3 | 0.091 | 653 | 10.3 | 8.7 |
| raman-fitting | 0/3 | 0.716 | 910 | 16.0 | 13.7 | 0/3 | 0.446 | 910 | 15.0 | 0.0 | 0/3 | 0.562 | 437 | 19.3 | 18.3 |
| regex-log | 3/3 | 0.433 | 457 | 9.0 | 6.3 | 2/2 (1 err) | 0.272 | 483 | 3.3 | 0.0 | 3/3 | 0.126 | 102 | 6.7 | 5.7 |
| sanitize-git-repo | 2/3 | 0.410 | 121 | 8.7 | 26.7 | 3/3 | 0.428 | 115 | 14.3 | 0.0 | 3/3 | 0.255 | 79 | 11.3 | 12.7 |
| sqlite-db-truncate | 3/3 | 0.373 | 244 | 6.0 | 7.7 | 3/3 | 0.240 | 123 | 7.0 | 0.0 | 3/3 | 0.134 | 125 | 6.3 | 5.0 |
| sqlite-with-gcov | 0/3 | 0.194 | 241 | 21.7 | 24.0 | 0/3 | 0.229 | 180 | 21.0 | 0.0 | 3/3 | 0.065 | 181 | 12.3 | 11.0 |
| tune-mjcf | 0/3 | 0.234 | 910 | 9.3 | 6.7 | 0/3 | 0.297 | 910 | 6.3 | 0.0 | 2/3 | 0.537 | 513 | 27.7 | 11.0 |
| vulnerable-secret | 3/3 | 0.236 | 255 | 9.3 | 13.3 | 3/3 | 0.326 | 153 | 14.0 | 0.0 | 3/3 | 0.111 | 74 | 13.7 | 12.7 |

Tasks where nanocode passes more often: build-cython-ext, build-pmars, cancel-async-tasks, cobol-modernization, gcode-to-text, git-multibranch, polyglot-c-py
Tasks where claude-code passes more often: break-filter-js-from-html, caffe-cifar-10, fix-git, largest-eigenval, pytorch-model-cli, query-optimize, sanitize-git-repo

Paired by task and attempt (147 pairs), claude-code minus nanocode, mean with its 95% normal interval:
- cost ($): -0.0153 (-0.0820 to 0.0514)
- wall (s): -59.0 (-95.4 to -22.5)
- requests: -0.90 (-3.04 to 1.24)
- prompt tokens: 77588 (-56996 to 212173)
Paired outcomes (139 pairs both scored): both passed 85, both failed 33, only claude-code passed 12, only nanocode passed 9; exact two-sided sign test on the discordant pairs p = 0.664
Only nanocode passed: build-cython-ext.2, build-pmars.1, cancel-async-tasks.3, cobol-modernization.2, count-dataset-tokens.2, extract-elf.2, gcode-to-text.2, git-multibranch.2, polyglot-c-py.1
Only claude-code passed: break-filter-js-from-html.1, caffe-cifar-10.1, count-dataset-tokens.1, extract-elf.1, fix-git.1, fix-git.3, largest-eigenval.1, largest-eigenval.3, pytorch-model-cli.3, query-optimize.1, query-optimize.2, sanitize-git-repo.3

Tasks where nanocode passes more often: build-pmars, build-pov-ray, git-multibranch, polyglot-c-py, pytorch-model-cli
Tasks where hl-agent passes more often: adaptive-rejection-sampler, break-filter-js-from-html, caffe-cifar-10, cancel-async-tasks, circuit-fibsqrt, db-wal-recovery, distribution-search, extract-elf, feal-differential-cryptanalysis, feal-linear-cryptanalysis, fix-git, gcode-to-text, largest-eigenval, pytorch-model-recovery, sanitize-git-repo, sqlite-with-gcov, tune-mjcf

Paired by task and attempt (147 pairs), hl-agent minus nanocode, mean with its 95% normal interval:
- cost ($): -0.1491 (-0.2313 to -0.0668)
- wall (s): -226.4 (-300.3 to -152.5)
- requests: -1.79 (-4.46 to 0.88)
- prompt tokens: -232713 (-364534 to -100891)
Paired outcomes (147 pairs both scored): both passed 90, both failed 21, only hl-agent passed 29, only nanocode passed 7; exact two-sided sign test on the discordant pairs p = 0.000
Only nanocode passed: build-pmars.3, build-pov-ray.3, cancel-async-tasks.3, count-dataset-tokens.2, git-multibranch.2, polyglot-c-py.3, pytorch-model-cli.2
Only hl-agent passed: adaptive-rejection-sampler.1, adaptive-rejection-sampler.2, adaptive-rejection-sampler.3, break-filter-js-from-html.1, caffe-cifar-10.1, cancel-async-tasks.1, cancel-async-tasks.2, circuit-fibsqrt.2, count-dataset-tokens.3, db-wal-recovery.1, db-wal-recovery.2, db-wal-recovery.3, distribution-search.1, extract-elf.1, feal-differential-cryptanalysis.1, feal-linear-cryptanalysis.1, fix-git.3, gcode-to-text.1, gcode-to-text.3, largest-eigenval.1, largest-eigenval.2, largest-eigenval.3, pytorch-model-recovery.3, sanitize-git-repo.3, sqlite-with-gcov.1, sqlite-with-gcov.2, sqlite-with-gcov.3, tune-mjcf.1, tune-mjcf.3

Tasks where claude-code passes more often: build-pov-ray, fix-git, pytorch-model-cli, query-optimize
Tasks where hl-agent passes more often: adaptive-rejection-sampler, build-cython-ext, cancel-async-tasks, circuit-fibsqrt, cobol-modernization, db-wal-recovery, extract-elf, feal-differential-cryptanalysis, feal-linear-cryptanalysis, gcode-to-text, largest-eigenval, sqlite-with-gcov, tune-mjcf

Paired by task and attempt (147 pairs), hl-agent minus claude-code, mean with its 95% normal interval:
- cost ($): -0.1338 (-0.2033 to -0.0642)
- wall (s): -167.4 (-242.0 to -92.8)
- requests: -0.89 (-3.37 to 1.59)
- prompt tokens: -310301 (-436564 to -184038)
Paired outcomes (139 pairs both scored): both passed 88, both failed 18, only hl-agent passed 24, only claude-code passed 9; exact two-sided sign test on the discordant pairs p = 0.014
Only claude-code passed: build-pmars.3, build-pov-ray.3, count-dataset-tokens.1, fix-git.1, polyglot-c-py.3, pytorch-model-cli.2, pytorch-model-cli.3, query-optimize.1, query-optimize.2
Only hl-agent passed: adaptive-rejection-sampler.3, build-cython-ext.2, build-pmars.1, cancel-async-tasks.1, cancel-async-tasks.2, circuit-fibsqrt.2, cobol-modernization.2, count-dataset-tokens.3, db-wal-recovery.1, db-wal-recovery.2, db-wal-recovery.3, extract-elf.2, feal-differential-cryptanalysis.1, feal-linear-cryptanalysis.1, gcode-to-text.1, gcode-to-text.2, gcode-to-text.3, largest-eigenval.2, polyglot-c-py.1, sqlite-with-gcov.1, sqlite-with-gcov.2, sqlite-with-gcov.3, tune-mjcf.1, tune-mjcf.3

nanocode rows with a request the upstream connection dropped: 39 terminal-bench-2.1.adaptive-rejection-sampler.nanocode.1 terminal-bench-2.1.adaptive-rejection-sampler.nanocode.2 terminal-bench-2.1.adaptive-rejection-sampler.nanocode.3 terminal-bench-2.1.break-filter-js-from-html.nanocode.1 terminal-bench-2.1.chess-best-move.nanocode.1 terminal-bench-2.1.chess-best-move.nanocode.2 terminal-bench-2.1.chess-best-move.nanocode.3 terminal-bench-2.1.circuit-fibsqrt.nanocode.1 terminal-bench-2.1.circuit-fibsqrt.nanocode.2 terminal-bench-2.1.constraints-scheduling.nanocode.1 terminal-bench-2.1.db-wal-recovery.nanocode.1 terminal-bench-2.1.db-wal-recovery.nanocode.2 terminal-bench-2.1.db-wal-recovery.nanocode.3 terminal-bench-2.1.distribution-search.nanocode.1 terminal-bench-2.1.dna-assembly.nanocode.1 terminal-bench-2.1.dna-insert.nanocode.1 terminal-bench-2.1.extract-moves-from-video.nanocode.1 terminal-bench-2.1.feal-differential-cryptanalysis.nanocode.1 terminal-bench-2.1.feal-linear-cryptanalysis.nanocode.1 terminal-bench-2.1.filter-js-from-html.nanocode.1 terminal-bench-2.1.fix-ocaml-gc.nanocode.1 terminal-bench-2.1.gcode-to-text.nanocode.1 terminal-bench-2.1.gcode-to-text.nanocode.3 terminal-bench-2.1.gpt2-codegolf.nanocode.1 terminal-bench-2.1.largest-eigenval.nanocode.1 terminal-bench-2.1.largest-eigenval.nanocode.2 terminal-bench-2.1.largest-eigenval.nanocode.3 terminal-bench-2.1.pytorch-model-cli.nanocode.2 terminal-bench-2.1.pytorch-model-cli.nanocode.3 terminal-bench-2.1.pytorch-model-recovery.nanocode.3 terminal-bench-2.1.query-optimize.nanocode.3 terminal-bench-2.1.raman-fitting.nanocode.1 terminal-bench-2.1.raman-fitting.nanocode.2 terminal-bench-2.1.raman-fitting.nanocode.3 terminal-bench-2.1.regex-log.nanocode.2 terminal-bench-2.1.tune-mjcf.nanocode.1 terminal-bench-2.1.tune-mjcf.nanocode.2 terminal-bench-2.1.tune-mjcf.nanocode.3 terminal-bench-2.1.vulnerable-secret.nanocode.3
claude-code rows with a request the upstream connection dropped: 35 terminal-bench-2.1.adaptive-rejection-sampler.claude-code.1 terminal-bench-2.1.adaptive-rejection-sampler.claude-code.2 terminal-bench-2.1.adaptive-rejection-sampler.claude-code.3 terminal-bench-2.1.break-filter-js-from-html.claude-code.1 terminal-bench-2.1.chess-best-move.claude-code.1 terminal-bench-2.1.chess-best-move.claude-code.2 terminal-bench-2.1.circuit-fibsqrt.claude-code.1 terminal-bench-2.1.circuit-fibsqrt.claude-code.2 terminal-bench-2.1.cobol-modernization.claude-code.1 terminal-bench-2.1.cobol-modernization.claude-code.2 terminal-bench-2.1.constraints-scheduling.claude-code.1 terminal-bench-2.1.db-wal-recovery.claude-code.1 terminal-bench-2.1.db-wal-recovery.claude-code.2 terminal-bench-2.1.db-wal-recovery.claude-code.3 terminal-bench-2.1.distribution-search.claude-code.1 terminal-bench-2.1.dna-assembly.claude-code.1 terminal-bench-2.1.extract-elf.claude-code.3 terminal-bench-2.1.extract-moves-from-video.claude-code.1 terminal-bench-2.1.feal-differential-cryptanalysis.claude-code.1 terminal-bench-2.1.feal-linear-cryptanalysis.claude-code.1 terminal-bench-2.1.filter-js-from-html.claude-code.1 terminal-bench-2.1.fix-ocaml-gc.claude-code.1 terminal-bench-2.1.gcode-to-text.claude-code.1 terminal-bench-2.1.gcode-to-text.claude-code.2 terminal-bench-2.1.gcode-to-text.claude-code.3 terminal-bench-2.1.gpt2-codegolf.claude-code.1 terminal-bench-2.1.largest-eigenval.claude-code.2 terminal-bench-2.1.polyglot-c-py.claude-code.3 terminal-bench-2.1.raman-fitting.claude-code.1 terminal-bench-2.1.raman-fitting.claude-code.2 terminal-bench-2.1.raman-fitting.claude-code.3 terminal-bench-2.1.regex-log.claude-code.2 terminal-bench-2.1.tune-mjcf.claude-code.1 terminal-bench-2.1.tune-mjcf.claude-code.2 terminal-bench-2.1.tune-mjcf.claude-code.3
hl-agent rows with a request the upstream connection dropped: 11 terminal-bench-2.1.adaptive-rejection-sampler.hl-agent.1 terminal-bench-2.1.adaptive-rejection-sampler.hl-agent.2 terminal-bench-2.1.chess-best-move.hl-agent.2 terminal-bench-2.1.circuit-fibsqrt.hl-agent.1 terminal-bench-2.1.circuit-fibsqrt.hl-agent.2 terminal-bench-2.1.distribution-search.hl-agent.1 terminal-bench-2.1.dna-assembly.hl-agent.1 terminal-bench-2.1.extract-moves-from-video.hl-agent.1 terminal-bench-2.1.feal-differential-cryptanalysis.hl-agent.1 terminal-bench-2.1.fix-ocaml-gc.hl-agent.1 terminal-bench-2.1.sqlite-db-truncate.hl-agent.2
errored: terminal-bench-2.1.adaptive-rejection-sampler.claude-code.1: no model request succeeded
errored: terminal-bench-2.1.adaptive-rejection-sampler.claude-code.2: no model request succeeded
errored: terminal-bench-2.1.distribution-search.claude-code.1: no model request succeeded
errored: terminal-bench-2.1.gpt2-codegolf.claude-code.1: no model request succeeded
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.1: no model request succeeded
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.2: no model request succeeded
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.3: no model request succeeded
errored: terminal-bench-2.1.regex-log.claude-code.2: no model request succeeded

#### claude-sonnet-4-6, suite 1

Model: claude-sonnet-4-6. Prices: https://docs.anthropic.com/en/docs/about-claude/pricing, read 2026-11-14; https://developers.openai.com/api/docs/models/gpt-5.5 (prompts under 272K tokens), read 2026-11-15.

| Agent | Attempts | Passed | Failed | Errored | Pass rate (95% Wilson) | Cost | Cost/attempt | Wall/attempt | Requests/attempt | Model tool calls/attempt | Inner tool calls/attempt | Input+cache tokens/attempt | Output tokens/attempt |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| nanocode | 60 | 53 | 7 | 0 | 88.3% (77.8-94.2%) | $13.59 | $0.227 | 171 s | 8.4 | 7.3 | 13.2 | 226369 | 7601 |
| claude-code | 60 | 52 | 7 | 1 | 88.1% (77.5-94.1%) | $12.05 | $0.201 | 158 s | 9.1 | 9.1 | 0.0 | 264552 | 5145 |
| hl-agent | 60 | 57 | 3 | 0 | 95.0% (86.3-98.3%) | $4.94 | $0.082 | 64 s | 8.6 | 7.7 | 7.7 | 79288 | 2804 |

| Task | nanocode pass | nanocode $ | nanocode s | nanocode req | nanocode tools | claude-code pass | claude-code $ | claude-code s | claude-code req | claude-code tools | hl-agent pass | hl-agent $ | hl-agent s | hl-agent req | hl-agent tools |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| extract-todos | 3/3 | 0.018 | 12 | 2.7 | 2.0 | 3/3 | 0.029 | 10 | 3.0 | 0.0 | 3/3 | 0.010 | 9 | 3.0 | 2.0 |
| uppercase-message | 3/3 | 0.018 | 10 | 3.0 | 2.7 | 3/3 | 0.030 | 10 | 3.0 | 0.0 | 3/3 | 0.010 | 9 | 3.0 | 2.0 |
| write-greeting | 3/3 | 0.011 | 8 | 2.0 | 1.0 | 3/3 | 0.022 | 6 | 2.0 | 0.0 | 3/3 | 0.006 | 5 | 2.0 | 1.0 |
| build-cython-ext | 3/3 | 1.011 | 419 | 40.0 | 73.3 | 2/3 | 0.806 | 340 | 44.3 | 0.0 | 3/3 | 0.360 | 235 | 37.3 | 36.3 |
| cancel-async-tasks | 1/3 | 0.107 | 80 | 4.0 | 2.7 | 0/3 | 0.062 | 37 | 2.0 | 0.0 | 2/3 | 0.042 | 35 | 4.0 | 3.0 |
| cobol-modernization | 3/3 | 0.617 | 453 | 12.0 | 14.7 | 2/3 | 0.656 | 745 | 15.3 | 0.0 | 3/3 | 0.158 | 118 | 11.0 | 11.0 |
| fix-git | 1/3 | 0.042 | 28 | 4.7 | 6.3 | 3/3 | 0.122 | 43 | 9.3 | 0.0 | 2/3 | 0.042 | 31 | 6.3 | 5.3 |
| git-leak-recovery | 3/3 | 0.059 | 36 | 5.0 | 15.3 | 3/3 | 0.086 | 29 | 7.7 | 0.0 | 3/3 | 0.022 | 20 | 5.0 | 4.0 |
| kv-store-grpc | 3/3 | 0.064 | 46 | 6.3 | 9.0 | 3/3 | 0.089 | 44 | 7.3 | 0.0 | 3/3 | 0.049 | 43 | 9.7 | 8.7 |
| largest-eigenval | 0/3 | 0.884 | 911 | 25.3 | 24.0 | 2/3 | 0.396 | 611 | 5.7 | 0.0 | 3/3 | 0.250 | 177 | 22.0 | 22.0 |
| log-summary-date-ranges | 3/3 | 0.070 | 35 | 4.3 | 5.0 | 3/3 | 0.070 | 24 | 4.0 | 0.0 | 3/3 | 0.032 | 20 | 3.0 | 2.0 |
| multi-source-data-merger | 3/3 | 0.089 | 55 | 4.7 | 7.7 | 3/3 | 0.131 | 60 | 7.3 | 0.0 | 3/3 | 0.058 | 42 | 5.7 | 5.0 |
| nginx-request-logging | 3/3 | 0.078 | 61 | 5.7 | 15.0 | 3/3 | 0.127 | 60 | 10.7 | 0.0 | 3/3 | 0.046 | 49 | 7.7 | 6.7 |
| openssl-selfsigned-cert | 3/3 | 0.060 | 42 | 2.7 | 13.0 | 3/3 | 0.102 | 34 | 9.3 | 0.0 | 3/3 | 0.055 | 41 | 6.0 | 5.0 |
| polyglot-c-py | 3/3 | 0.178 | 142 | 6.7 | 23.0 | 2/3 | 0.152 | 208 | 6.7 | 0.0 | 2/3 | 0.071 | 70 | 6.7 | 5.7 |
| prove-plus-comm | 3/3 | 0.080 | 54 | 5.3 | 5.0 | 3/3 | 0.150 | 67 | 6.7 | 0.0 | 3/3 | 0.022 | 19 | 5.0 | 4.0 |
| pypi-server | 3/3 | 0.103 | 70 | 9.7 | 18.0 | 3/3 | 0.149 | 66 | 13.0 | 0.0 | 3/3 | 0.042 | 51 | 7.7 | 6.7 |
| regex-log | 3/3 | 0.433 | 457 | 9.0 | 6.3 | 2/2 (1 err) | 0.272 | 483 | 3.3 | 0.0 | 3/3 | 0.126 | 102 | 6.7 | 5.7 |
| sqlite-db-truncate | 3/3 | 0.373 | 244 | 6.0 | 7.7 | 3/3 | 0.240 | 123 | 7.0 | 0.0 | 3/3 | 0.134 | 125 | 6.3 | 5.0 |
| vulnerable-secret | 3/3 | 0.236 | 255 | 9.3 | 13.3 | 3/3 | 0.326 | 153 | 14.0 | 0.0 | 3/3 | 0.111 | 74 | 13.7 | 12.7 |

Tasks where nanocode passes more often: build-cython-ext, cancel-async-tasks, cobol-modernization, polyglot-c-py
Tasks where claude-code passes more often: fix-git, largest-eigenval

Paired by task and attempt (60 pairs), claude-code minus nanocode, mean with its 95% normal interval:
- cost ($): -0.0258 (-0.0784 to 0.0269)
- wall (s): -13.2 (-48.2 to 21.7)
- requests: 0.67 (-1.06 to 2.39)
- prompt tokens: 38182 (-49862 to 126227)
Paired outcomes (59 pairs both scored): both passed 48, both failed 3, only claude-code passed 4, only nanocode passed 4; exact two-sided sign test on the discordant pairs p = 1.000
Only nanocode passed: build-cython-ext.2, cancel-async-tasks.3, cobol-modernization.2, polyglot-c-py.1
Only claude-code passed: fix-git.1, fix-git.3, largest-eigenval.1, largest-eigenval.3

Tasks where nanocode passes more often: polyglot-c-py
Tasks where hl-agent passes more often: cancel-async-tasks, fix-git, largest-eigenval

Paired by task and attempt (60 pairs), hl-agent minus nanocode, mean with its 95% normal interval:
- cost ($): -0.1442 (-0.2059 to -0.0824)
- wall (s): -107.1 (-156.0 to -58.1)
- requests: 0.17 (-0.52 to 0.85)
- prompt tokens: -147081 (-236809 to -57354)
Paired outcomes (60 pairs both scored): both passed 51, both failed 1, only hl-agent passed 6, only nanocode passed 2; exact two-sided sign test on the discordant pairs p = 0.289
Only nanocode passed: cancel-async-tasks.3, polyglot-c-py.3
Only hl-agent passed: cancel-async-tasks.1, cancel-async-tasks.2, fix-git.3, largest-eigenval.1, largest-eigenval.2, largest-eigenval.3

Tasks where claude-code passes more often: fix-git
Tasks where hl-agent passes more often: build-cython-ext, cancel-async-tasks, cobol-modernization, largest-eigenval

Paired by task and attempt (60 pairs), hl-agent minus claude-code, mean with its 95% normal interval:
- cost ($): -0.1184 (-0.1614 to -0.0754)
- wall (s): -93.8 (-146.0 to -41.6)
- requests: -0.50 (-1.91 to 0.91)
- prompt tokens: -185263 (-263013 to -107513)
Paired outcomes (59 pairs both scored): both passed 50, both failed 1, only hl-agent passed 6, only claude-code passed 2; exact two-sided sign test on the discordant pairs p = 0.289
Only claude-code passed: fix-git.1, polyglot-c-py.3
Only hl-agent passed: build-cython-ext.2, cancel-async-tasks.1, cancel-async-tasks.2, cobol-modernization.2, largest-eigenval.2, polyglot-c-py.1

nanocode rows with a request the upstream connection dropped: 5 terminal-bench-2.1.largest-eigenval.nanocode.1 terminal-bench-2.1.largest-eigenval.nanocode.2 terminal-bench-2.1.largest-eigenval.nanocode.3 terminal-bench-2.1.regex-log.nanocode.2 terminal-bench-2.1.vulnerable-secret.nanocode.3
claude-code rows with a request the upstream connection dropped: 5 terminal-bench-2.1.cobol-modernization.claude-code.1 terminal-bench-2.1.cobol-modernization.claude-code.2 terminal-bench-2.1.largest-eigenval.claude-code.2 terminal-bench-2.1.polyglot-c-py.claude-code.3 terminal-bench-2.1.regex-log.claude-code.2
hl-agent rows with a request the upstream connection dropped: 1 terminal-bench-2.1.sqlite-db-truncate.hl-agent.2
errored: terminal-bench-2.1.regex-log.claude-code.2: no model request succeeded

#### claude-sonnet-4-6, suite 2

Model: claude-sonnet-4-6. Prices: https://docs.anthropic.com/en/docs/about-claude/pricing, read 2026-11-14; https://developers.openai.com/api/docs/models/gpt-5.5 (prompts under 272K tokens), read 2026-11-15.

| Agent | Attempts | Passed | Failed | Errored | Pass rate (95% Wilson) | Cost | Cost/attempt | Wall/attempt | Requests/attempt | Model tool calls/attempt | Inner tool calls/attempt | Input+cache tokens/attempt | Output tokens/attempt |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| nanocode | 60 | 30 | 30 | 0 | 50.0% (37.7-62.3%) | $20.76 | $0.346 | 456 s | 15.1 | 13.5 | 17.3 | 305906 | 12319 |
| claude-code | 60 | 29 | 26 | 5 | 52.7% (39.8-65.3%) | $20.22 | $0.337 | 378 s | 14.5 | 14.8 | 0.0 | 450181 | 8968 |
| hl-agent | 60 | 43 | 17 | 0 | 71.7% (59.2-81.5%) | $12.64 | $0.211 | 277 s | 15.2 | 14.5 | 13.4 | 188199 | 7106 |

| Task | nanocode pass | nanocode $ | nanocode s | nanocode req | nanocode tools | claude-code pass | claude-code $ | claude-code s | claude-code req | claude-code tools | hl-agent pass | hl-agent $ | hl-agent s | hl-agent req | hl-agent tools |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| adaptive-rejection-sampler | 0/3 | 0.016 | 900 | 4.0 | 0.0 | 0/1 (2 err) | 0.177 | 900 | 2.7 | 0.0 | 3/3 | 0.472 | 689 | 22.0 | 19.0 |
| build-pmars | 3/3 | 0.153 | 132 | 17.7 | 21.3 | 2/3 | 0.264 | 134 | 22.0 | 0.0 | 2/3 | 0.077 | 91 | 15.0 | 14.0 |
| chess-best-move | 0/3 | 0.638 | 900 | 15.7 | 14.0 | 0/3 | 0.135 | 704 | 3.7 | 0.0 | 0/3 | 0.535 | 524 | 19.0 | 15.0 |
| configure-git-webserver | 3/3 | 0.110 | 122 | 11.3 | 10.7 | 3/3 | 0.184 | 141 | 16.7 | 0.0 | 3/3 | 0.092 | 171 | 15.7 | 14.7 |
| count-dataset-tokens | 1/3 | 0.122 | 132 | 8.7 | 6.7 | 1/3 | 0.147 | 114 | 11.0 | 0.0 | 1/3 | 0.185 | 140 | 15.3 | 14.0 |
| db-wal-recovery | 0/3 | 0.634 | 900 | 16.7 | 18.3 | 0/3 | 1.093 | 900 | 28.7 | 0.0 | 3/3 | 0.045 | 39 | 6.0 | 5.0 |
| extract-elf | 2/3 | 0.395 | 282 | 11.3 | 13.0 | 2/3 | 0.235 | 242 | 8.7 | 0.0 | 3/3 | 0.086 | 67 | 8.3 | 7.3 |
| fix-code-vulnerability | 3/3 | 0.417 | 165 | 28.0 | 31.7 | 3/3 | 0.361 | 100 | 20.0 | 0.0 | 3/3 | 0.140 | 61 | 19.0 | 19.3 |
| gcode-to-text | 1/3 | 0.859 | 752 | 23.3 | 36.3 | 0/3 | 1.147 | 910 | 30.3 | 0.0 | 3/3 | 0.403 | 396 | 24.3 | 23.7 |
| git-multibranch | 3/3 | 0.246 | 150 | 20.7 | 29.7 | 2/3 | 0.269 | 110 | 22.3 | 0.0 | 2/3 | 0.121 | 94 | 16.0 | 15.0 |
| headless-terminal | 3/3 | 0.454 | 340 | 13.0 | 10.3 | 3/3 | 0.134 | 80 | 7.0 | 0.0 | 3/3 | 0.045 | 43 | 5.3 | 4.3 |
| hf-model-inference | 3/3 | 0.092 | 81 | 9.0 | 16.0 | 3/3 | 0.094 | 61 | 8.0 | 0.0 | 3/3 | 0.074 | 79 | 11.7 | 10.7 |
| modernize-scientific-stack | 3/3 | 0.082 | 62 | 4.0 | 6.3 | 3/3 | 0.059 | 24 | 4.0 | 0.0 | 3/3 | 0.056 | 43 | 5.0 | 6.0 |
| pytorch-model-cli | 1/3 | 0.449 | 800 | 32.0 | 30.3 | 2/3 | 0.709 | 757 | 35.3 | 0.0 | 0/3 | 0.222 | 431 | 25.3 | 23.7 |
| pytorch-model-recovery | 2/3 | 0.320 | 630 | 14.7 | 10.3 | 0/0 (3 err) | 0.000 | 1 | 0.0 | 0.0 | 3/3 | 0.150 | 809 | 15.0 | 13.7 |
| query-optimize | 0/3 | 0.381 | 584 | 15.3 | 19.7 | 2/3 | 0.334 | 271 | 12.7 | 0.0 | 0/3 | 0.091 | 653 | 10.3 | 8.7 |
| raman-fitting | 0/3 | 0.716 | 910 | 16.0 | 13.7 | 0/3 | 0.446 | 910 | 15.0 | 0.0 | 0/3 | 0.562 | 437 | 19.3 | 18.3 |
| sanitize-git-repo | 2/3 | 0.410 | 121 | 8.7 | 26.7 | 3/3 | 0.428 | 115 | 14.3 | 0.0 | 3/3 | 0.255 | 79 | 11.3 | 12.7 |
| sqlite-with-gcov | 0/3 | 0.194 | 241 | 21.7 | 24.0 | 0/3 | 0.229 | 180 | 21.0 | 0.0 | 3/3 | 0.065 | 181 | 12.3 | 11.0 |
| tune-mjcf | 0/3 | 0.234 | 910 | 9.3 | 6.7 | 0/3 | 0.297 | 910 | 6.3 | 0.0 | 2/3 | 0.537 | 513 | 27.7 | 11.0 |

Tasks where nanocode passes more often: build-pmars, gcode-to-text, git-multibranch
Tasks where claude-code passes more often: pytorch-model-cli, query-optimize, sanitize-git-repo

Paired by task and attempt (60 pairs), claude-code minus nanocode, mean with its 95% normal interval:
- cost ($): -0.0090 (-0.0813 to 0.0633)
- wall (s): -77.5 (-129.3 to -25.7)
- requests: -0.57 (-2.57 to 1.43)
- prompt tokens: 144276 (52368 to 236183)
Paired outcomes (55 pairs both scored): both passed 23, both failed 21, only claude-code passed 6, only nanocode passed 5; exact two-sided sign test on the discordant pairs p = 1.000
Only nanocode passed: build-pmars.1, count-dataset-tokens.2, extract-elf.2, gcode-to-text.2, git-multibranch.2
Only claude-code passed: count-dataset-tokens.1, extract-elf.1, pytorch-model-cli.3, query-optimize.1, query-optimize.2, sanitize-git-repo.3

Tasks where nanocode passes more often: build-pmars, git-multibranch, pytorch-model-cli
Tasks where hl-agent passes more often: adaptive-rejection-sampler, db-wal-recovery, extract-elf, gcode-to-text, pytorch-model-recovery, sanitize-git-repo, sqlite-with-gcov, tune-mjcf

Paired by task and attempt (60 pairs), hl-agent minus nanocode, mean with its 95% normal interval:
- cost ($): -0.1354 (-0.2145 to -0.0563)
- wall (s): -178.8 (-253.9 to -103.7)
- requests: 0.15 (-2.61 to 2.91)
- prompt tokens: -117707 (-195044 to -40370)
Paired outcomes (60 pairs both scored): both passed 26, both failed 13, only hl-agent passed 17, only nanocode passed 4; exact two-sided sign test on the discordant pairs p = 0.007
Only nanocode passed: build-pmars.3, count-dataset-tokens.2, git-multibranch.2, pytorch-model-cli.2
Only hl-agent passed: adaptive-rejection-sampler.1, adaptive-rejection-sampler.2, adaptive-rejection-sampler.3, count-dataset-tokens.3, db-wal-recovery.1, db-wal-recovery.2, db-wal-recovery.3, extract-elf.1, gcode-to-text.1, gcode-to-text.3, pytorch-model-recovery.3, sanitize-git-repo.3, sqlite-with-gcov.1, sqlite-with-gcov.2, sqlite-with-gcov.3, tune-mjcf.1, tune-mjcf.3

Tasks where claude-code passes more often: pytorch-model-cli, query-optimize
Tasks where hl-agent passes more often: adaptive-rejection-sampler, db-wal-recovery, extract-elf, gcode-to-text, sqlite-with-gcov, tune-mjcf

Paired by task and attempt (60 pairs), hl-agent minus claude-code, mean with its 95% normal interval:
- cost ($): -0.1264 (-0.2287 to -0.0242)
- wall (s): -101.3 (-195.3 to -7.2)
- requests: 0.72 (-2.70 to 4.14)
- prompt tokens: -261983 (-390942 to -133024)
Paired outcomes (55 pairs both scored): both passed 23, both failed 11, only hl-agent passed 15, only claude-code passed 6; exact two-sided sign test on the discordant pairs p = 0.078
Only claude-code passed: build-pmars.3, count-dataset-tokens.1, pytorch-model-cli.2, pytorch-model-cli.3, query-optimize.1, query-optimize.2
Only hl-agent passed: adaptive-rejection-sampler.3, build-pmars.1, count-dataset-tokens.3, db-wal-recovery.1, db-wal-recovery.2, db-wal-recovery.3, extract-elf.2, gcode-to-text.1, gcode-to-text.2, gcode-to-text.3, sqlite-with-gcov.1, sqlite-with-gcov.2, sqlite-with-gcov.3, tune-mjcf.1, tune-mjcf.3

nanocode rows with a request the upstream connection dropped: 21 terminal-bench-2.1.adaptive-rejection-sampler.nanocode.1 terminal-bench-2.1.adaptive-rejection-sampler.nanocode.2 terminal-bench-2.1.adaptive-rejection-sampler.nanocode.3 terminal-bench-2.1.chess-best-move.nanocode.1 terminal-bench-2.1.chess-best-move.nanocode.2 terminal-bench-2.1.chess-best-move.nanocode.3 terminal-bench-2.1.db-wal-recovery.nanocode.1 terminal-bench-2.1.db-wal-recovery.nanocode.2 terminal-bench-2.1.db-wal-recovery.nanocode.3 terminal-bench-2.1.gcode-to-text.nanocode.1 terminal-bench-2.1.gcode-to-text.nanocode.3 terminal-bench-2.1.pytorch-model-cli.nanocode.2 terminal-bench-2.1.pytorch-model-cli.nanocode.3 terminal-bench-2.1.pytorch-model-recovery.nanocode.3 terminal-bench-2.1.query-optimize.nanocode.3 terminal-bench-2.1.raman-fitting.nanocode.1 terminal-bench-2.1.raman-fitting.nanocode.2 terminal-bench-2.1.raman-fitting.nanocode.3 terminal-bench-2.1.tune-mjcf.nanocode.1 terminal-bench-2.1.tune-mjcf.nanocode.2 terminal-bench-2.1.tune-mjcf.nanocode.3
claude-code rows with a request the upstream connection dropped: 18 terminal-bench-2.1.adaptive-rejection-sampler.claude-code.1 terminal-bench-2.1.adaptive-rejection-sampler.claude-code.2 terminal-bench-2.1.adaptive-rejection-sampler.claude-code.3 terminal-bench-2.1.chess-best-move.claude-code.1 terminal-bench-2.1.chess-best-move.claude-code.2 terminal-bench-2.1.db-wal-recovery.claude-code.1 terminal-bench-2.1.db-wal-recovery.claude-code.2 terminal-bench-2.1.db-wal-recovery.claude-code.3 terminal-bench-2.1.extract-elf.claude-code.3 terminal-bench-2.1.gcode-to-text.claude-code.1 terminal-bench-2.1.gcode-to-text.claude-code.2 terminal-bench-2.1.gcode-to-text.claude-code.3 terminal-bench-2.1.raman-fitting.claude-code.1 terminal-bench-2.1.raman-fitting.claude-code.2 terminal-bench-2.1.raman-fitting.claude-code.3 terminal-bench-2.1.tune-mjcf.claude-code.1 terminal-bench-2.1.tune-mjcf.claude-code.2 terminal-bench-2.1.tune-mjcf.claude-code.3
hl-agent rows with a request the upstream connection dropped: 3 terminal-bench-2.1.adaptive-rejection-sampler.hl-agent.1 terminal-bench-2.1.adaptive-rejection-sampler.hl-agent.2 terminal-bench-2.1.chess-best-move.hl-agent.2
errored: terminal-bench-2.1.adaptive-rejection-sampler.claude-code.1: no model request succeeded
errored: terminal-bench-2.1.adaptive-rejection-sampler.claude-code.2: no model request succeeded
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.1: no model request succeeded
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.2: no model request succeeded
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.3: no model request succeeded

#### claude-sonnet-4-6, suite 3

Model: claude-sonnet-4-6. Prices: https://docs.anthropic.com/en/docs/about-claude/pricing, read 2026-11-14; https://developers.openai.com/api/docs/models/gpt-5.5 (prompts under 272K tokens), read 2026-11-15.

| Agent | Attempts | Passed | Failed | Errored | Pass rate (95% Wilson) | Cost | Cost/attempt | Wall/attempt | Requests/attempt | Model tool calls/attempt | Inner tool calls/attempt | Input+cache tokens/attempt | Output tokens/attempt |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| nanocode | 27 | 14 | 13 | 0 | 51.9% (34.0-69.3%) | $16.45 | $0.609 | 1324 s | 33.2 | 28.6 | 41.7 | 1036244 | 13254 |
| claude-code | 27 | 16 | 9 | 2 | 64.0% (44.5-79.8%) | $16.28 | $0.603 | 1204 s | 28.1 | 28.9 | 0.0 | 1053207 | 10501 |
| hl-agent | 27 | 19 | 8 | 0 | 70.4% (51.5-84.1%) | $11.31 | $0.419 | 726 s | 22.7 | 20.1 | 16.0 | 357670 | 12725 |

| Task | nanocode pass | nanocode $ | nanocode s | nanocode req | nanocode tools | claude-code pass | claude-code $ | claude-code s | claude-code req | claude-code tools | hl-agent pass | hl-agent $ | hl-agent s | hl-agent req | hl-agent tools |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| bn-fit-modify | 3/3 | 0.254 | 212 | 13.3 | 13.3 | 3/3 | 0.399 | 261 | 17.7 | 0.0 | 3/3 | 0.172 | 161 | 17.0 | 16.0 |
| break-filter-js-from-html | 0/1 | 0.034 | 1200 | 6.0 | 3.0 | 1/1 | 0.680 | 1184 | 15.0 | 0.0 | 1/1 | 0.307 | 271 | 11.0 | 10.0 |
| build-pov-ray | 3/3 | 2.112 | 1069 | 96.0 | 127.7 | 3/3 | 0.679 | 281 | 41.7 | 0.0 | 2/3 | 0.164 | 158 | 25.0 | 24.0 |
| caffe-cifar-10 | 2/3 | 1.195 | 1968 | 87.0 | 131.3 | 3/3 | 2.093 | 2120 | 102.0 | 0.0 | 3/3 | 0.500 | 2097 | 51.7 | 49.7 |
| circuit-fibsqrt | 0/2 | 0.172 | 3610 | 16.5 | 2.0 | 0/2 | 0.037 | 3584 | 12.0 | 0.0 | 1/2 | 0.419 | 2109 | 10.5 | 4.5 |
| code-from-image | 1/1 | 0.039 | 24 | 4.0 | 3.0 | 1/1 | 0.058 | 20 | 4.0 | 0.0 | 1/1 | 0.020 | 11 | 3.0 | 2.0 |
| compile-compcert | 1/1 | 0.767 | 1143 | 48.0 | 45.0 | 1/1 | 0.818 | 1339 | 50.0 | 0.0 | 1/1 | 0.149 | 1044 | 21.0 | 20.0 |
| constraints-scheduling | 1/1 | 0.224 | 519 | 6.0 | 8.0 | 1/1 | 0.161 | 415 | 4.0 | 0.0 | 1/1 | 0.067 | 51 | 3.0 | 2.0 |
| crack-7z-hash | 1/1 | 0.165 | 157 | 21.0 | 28.0 | 1/1 | 0.206 | 139 | 19.0 | 0.0 | 1/1 | 0.044 | 112 | 10.0 | 9.0 |
| custom-memory-heap-crash | 0/1 | 0.426 | 234 | 17.0 | 26.0 | 0/1 | 0.569 | 346 | 16.0 | 0.0 | 0/1 | 0.103 | 87 | 9.0 | 8.0 |
| distribution-search | 0/1 | 0.047 | 3611 | 16.0 | 0.0 | 0/0 (1 err) | 0.006 | 3610 | 11.0 | 0.0 | 1/1 | 0.487 | 466 | 13.0 | 9.0 |
| dna-assembly | 0/1 | 0.063 | 1800 | 10.0 | 1.0 | 0/1 | 0.062 | 1800 | 7.0 | 0.0 | 0/1 | 0.917 | 1800 | 21.0 | nan |
| dna-insert | 0/1 | 0.809 | 834 | 30.0 | 76.0 | 0/1 | 0.637 | 408 | 14.0 | 0.0 | 0/1 | 0.216 | 175 | 12.0 | 11.0 |
| extract-moves-from-video | 0/1 | 0.548 | 1800 | 29.0 | 39.0 | 0/1 | 2.278 | 1800 | 66.0 | 0.0 | 0/1 | 3.723 | 1800 | 118.0 | nan |
| feal-differential-cryptanalysis | 0/1 | 0.042 | 1810 | 10.0 | 1.0 | 0/1 | 0.029 | 1810 | 6.0 | 0.0 | 1/1 | 0.619 | 1213 | 11.0 | 5.0 |
| feal-linear-cryptanalysis | 0/1 | 0.061 | 1810 | 9.0 | 4.0 | 0/1 | 0.056 | 1810 | 7.0 | 0.0 | 1/1 | 0.328 | 330 | 7.0 | 7.0 |
| filter-js-from-html | 0/1 | 0.904 | 1002 | 17.0 | 21.0 | 0/1 | 0.158 | 493 | 4.0 | 0.0 | 0/1 | 0.067 | 54 | 3.0 | 2.0 |
| financial-document-processor | 1/1 | 0.381 | 295 | 17.0 | 30.0 | 1/1 | 0.353 | 124 | 8.0 | 0.0 | 1/1 | 0.585 | 199 | 49.0 | 20.0 |
| fix-ocaml-gc | 1/1 | 0.894 | 1628 | 30.0 | 21.0 | 1/1 | 0.605 | 1153 | 17.0 | 0.0 | 1/1 | 0.297 | 484 | 20.0 | 18.0 |
| gpt2-codegolf | 0/1 | 0.015 | 900 | 4.0 | 0.0 | 0/0 (1 err) | 0.017 | 900 | 2.0 | 0.0 | 0/1 | 0.033 | 42 | 1.0 | 0.0 |

Tasks where nanocode passes more often: none
Tasks where claude-code passes more often: break-filter-js-from-html, caffe-cifar-10

Paired by task and attempt (27 pairs), claude-code minus nanocode, mean with its 95% normal interval:
- cost ($): -0.0060 (-0.3155 to 0.3035)
- wall (s): -119.5 (-259.3 to 20.3)
- requests: -5.11 (-15.18 to 4.95)
- prompt tokens: 16964 (-668283 to 702211)
Paired outcomes (25 pairs both scored): both passed 14, both failed 9, only claude-code passed 2, only nanocode passed 0; exact two-sided sign test on the discordant pairs p = 0.500
Only nanocode passed: none
Only claude-code passed: break-filter-js-from-html.1, caffe-cifar-10.1

Tasks where nanocode passes more often: build-pov-ray
Tasks where hl-agent passes more often: break-filter-js-from-html, caffe-cifar-10, circuit-fibsqrt, distribution-search, feal-differential-cryptanalysis, feal-linear-cryptanalysis

Paired by task and attempt (27 pairs), hl-agent minus nanocode, mean with its 95% normal interval:
- cost ($): -0.1903 (-0.5850 to 0.2044)
- wall (s): -597.4 (-914.8 to -280.1)
- requests: -10.44 (-23.26 to 2.37)
- prompt tokens: -678574 (-1330016 to -27132)
Paired outcomes (27 pairs both scored): both passed 13, both failed 7, only hl-agent passed 6, only nanocode passed 1; exact two-sided sign test on the discordant pairs p = 0.125
Only nanocode passed: build-pov-ray.3
Only hl-agent passed: break-filter-js-from-html.1, caffe-cifar-10.1, circuit-fibsqrt.2, distribution-search.1, feal-differential-cryptanalysis.1, feal-linear-cryptanalysis.1

Tasks where claude-code passes more often: build-pov-ray
Tasks where hl-agent passes more often: circuit-fibsqrt, feal-differential-cryptanalysis, feal-linear-cryptanalysis

Paired by task and attempt (27 pairs), hl-agent minus claude-code, mean with its 95% normal interval:
- cost ($): -0.1843 (-0.4764 to 0.1079)
- wall (s): -478.0 (-785.1 to -170.9)
- requests: -5.33 (-16.06 to 5.39)
- prompt tokens: -695537 (-1283097 to -107978)
Paired outcomes (25 pairs both scored): both passed 15, both failed 6, only hl-agent passed 3, only claude-code passed 1; exact two-sided sign test on the discordant pairs p = 0.625
Only claude-code passed: build-pov-ray.3
Only hl-agent passed: circuit-fibsqrt.2, feal-differential-cryptanalysis.1, feal-linear-cryptanalysis.1

nanocode rows with a request the upstream connection dropped: 13 terminal-bench-2.1.break-filter-js-from-html.nanocode.1 terminal-bench-2.1.circuit-fibsqrt.nanocode.1 terminal-bench-2.1.circuit-fibsqrt.nanocode.2 terminal-bench-2.1.constraints-scheduling.nanocode.1 terminal-bench-2.1.distribution-search.nanocode.1 terminal-bench-2.1.dna-assembly.nanocode.1 terminal-bench-2.1.dna-insert.nanocode.1 terminal-bench-2.1.extract-moves-from-video.nanocode.1 terminal-bench-2.1.feal-differential-cryptanalysis.nanocode.1 terminal-bench-2.1.feal-linear-cryptanalysis.nanocode.1 terminal-bench-2.1.filter-js-from-html.nanocode.1 terminal-bench-2.1.fix-ocaml-gc.nanocode.1 terminal-bench-2.1.gpt2-codegolf.nanocode.1
claude-code rows with a request the upstream connection dropped: 12 terminal-bench-2.1.break-filter-js-from-html.claude-code.1 terminal-bench-2.1.circuit-fibsqrt.claude-code.1 terminal-bench-2.1.circuit-fibsqrt.claude-code.2 terminal-bench-2.1.constraints-scheduling.claude-code.1 terminal-bench-2.1.distribution-search.claude-code.1 terminal-bench-2.1.dna-assembly.claude-code.1 terminal-bench-2.1.extract-moves-from-video.claude-code.1 terminal-bench-2.1.feal-differential-cryptanalysis.claude-code.1 terminal-bench-2.1.feal-linear-cryptanalysis.claude-code.1 terminal-bench-2.1.filter-js-from-html.claude-code.1 terminal-bench-2.1.fix-ocaml-gc.claude-code.1 terminal-bench-2.1.gpt2-codegolf.claude-code.1
hl-agent rows with a request the upstream connection dropped: 7 terminal-bench-2.1.circuit-fibsqrt.hl-agent.1 terminal-bench-2.1.circuit-fibsqrt.hl-agent.2 terminal-bench-2.1.distribution-search.hl-agent.1 terminal-bench-2.1.dna-assembly.hl-agent.1 terminal-bench-2.1.extract-moves-from-video.hl-agent.1 terminal-bench-2.1.feal-differential-cryptanalysis.hl-agent.1 terminal-bench-2.1.fix-ocaml-gc.hl-agent.1
errored: terminal-bench-2.1.distribution-search.claude-code.1: no model request succeeded
errored: terminal-bench-2.1.gpt2-codegolf.claude-code.1: no model request succeeded

#### claude-haiku-4-5, all suites

Model: claude-haiku-4-5. Prices: https://docs.anthropic.com/en/docs/about-claude/pricing, read 2026-11-14; https://developers.openai.com/api/docs/models/gpt-5.5 (prompts under 272K tokens), read 2026-11-15.

| Agent | Attempts | Passed | Failed | Errored | Pass rate (95% Wilson) | Cost | Cost/attempt | Wall/attempt | Requests/attempt | Model tool calls/attempt | Inner tool calls/attempt | Input+cache tokens/attempt | Output tokens/attempt |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| nanocode | 62 | 33 | 29 | 0 | 53.2% (41.0-65.1%) | $9.83 | $0.159 | 244 s | 27.0 | 25.8 | 37.9 | 654452 | 13226 |
| claude-code | 61 | 34 | 25 | 2 | 57.6% (44.9-69.4%) | $8.39 | $0.138 | 198 s | 18.2 | 18.0 | 0.0 | 647313 | 9380 |
| hl-agent | 62 | 38 | 24 | 0 | 61.3% (48.8-72.4%) | $11.14 | $0.180 | 198 s | 32.0 | 31.5 | 30.8 | 816983 | 13384 |

| Task | nanocode pass | nanocode $ | nanocode s | nanocode req | nanocode tools | claude-code pass | claude-code $ | claude-code s | claude-code req | claude-code tools | hl-agent pass | hl-agent $ | hl-agent s | hl-agent req | hl-agent tools |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| adaptive-rejection-sampler | 1/2 | 0.570 | 712 | 52.0 | 58.0 | 1/2 | 0.346 | 655 | 37.5 | 0.0 | 1/2 | 0.659 | 891 | 55.5 | 57.0 |
| build-cython-ext | 0/2 | 0.416 | 301 | 62.0 | 71.5 | 0/2 | 0.409 | 229 | 60.5 | 0.0 | 0/2 | 0.503 | 354 | 82.0 | 82.5 |
| build-pmars | 2/2 | 0.127 | 142 | 27.5 | 55.5 | 2/2 | 0.124 | 107 | 29.0 | 0.0 | 2/2 | 0.082 | 106 | 30.5 | 29.5 |
| cancel-async-tasks | 1/2 | 0.015 | 17 | 5.0 | 3.5 | 1/2 | 0.026 | 22 | 2.0 | 0.0 | 1/2 | 0.032 | 32 | 8.0 | 7.0 |
| chess-best-move | 0/2 | 0.097 | 101 | 13.5 | 2.0 | 0/2 | 0.152 | 374 | 3.5 | 0.0 | 0/2 | 0.081 | 102 | 12.5 | 11.5 |
| cobol-modernization | 2/2 | 0.353 | 265 | 42.0 | 76.5 | 2/2 | 0.236 | 156 | 26.5 | 0.0 | 2/2 | 0.456 | 322 | 83.5 | 90.0 |
| configure-git-webserver | 0/2 | 0.063 | 900 | 15.5 | 24.0 | 0/2 | 0.026 | 14 | 2.0 | 0.0 | 1/2 | 0.027 | 54 | 6.5 | 5.5 |
| count-dataset-tokens | 0/2 | 0.045 | 96 | 11.0 | 12.0 | 0/2 | 0.066 | 147 | 13.5 | 0.0 | 0/2 | 0.062 | 97 | 14.5 | 13.5 |
| db-wal-recovery | 0/2 | 0.228 | 228 | 48.5 | 60.5 | 0/2 | 0.218 | 176 | 36.0 | 0.0 | 0/2 | 0.070 | 81 | 29.5 | 28.5 |
| extract-elf | 0/2 | 0.240 | 188 | 42.5 | 47.0 | 2/2 | 0.109 | 101 | 14.5 | 0.0 | 2/2 | 0.074 | 74 | 19.0 | 18.0 |
| fix-code-vulnerability | 2/2 | 0.353 | 205 | 63.0 | 76.0 | 1/2 | 0.250 | 126 | 43.0 | 0.0 | 2/2 | 0.114 | 89 | 36.5 | 35.0 |
| fix-git | 2/2 | 0.027 | 21 | 8.5 | 7.5 | 2/2 | 0.054 | 31 | 10.0 | 0.0 | 2/2 | 0.020 | 23 | 11.0 | 10.0 |
| gcode-to-text | 0/2 | 0.070 | 66 | 16.5 | 15.5 | 0/2 | 0.081 | 84 | 9.5 | 0.0 | 0/2 | 0.085 | 76 | 19.0 | 18.0 |
| git-leak-recovery | 2/2 | 0.028 | 36 | 9.5 | 19.0 | 2/2 | 0.041 | 33 | 11.0 | 0.0 | 2/2 | 0.018 | 24 | 12.5 | 11.5 |
| git-multibranch | 1/2 | 0.279 | 260 | 53.5 | 104.5 | 2/2 | 0.268 | 224 | 46.0 | 0.0 | 1/2 | 0.376 | 376 | 49.5 | 48.5 |
| headless-terminal | 2/2 | 0.065 | 82 | 14.5 | 15.5 | 2/2 | 0.044 | 46 | 6.5 | 0.0 | 2/2 | 0.067 | 132 | 15.0 | 14.0 |
| hf-model-inference | 2/2 | 0.197 | 529 | 40.0 | 77.5 | 1/2 | 0.116 | 782 | 15.0 | 0.0 | 1/2 | 0.195 | 292 | 30.0 | 30.0 |
| kv-store-grpc | 1/2 | 0.047 | 910 | 13.0 | 14.5 | 2/2 | 0.122 | 910 | 15.0 | 0.0 | 2/2 | 0.089 | 198 | 22.0 | 20.5 |
| largest-eigenval | 1/2 | 0.286 | 279 | 50.0 | 57.5 | 0/2 | 0.618 | 403 | 63.5 | 0.0 | 2/2 | 0.243 | 282 | 47.5 | 47.5 |
| log-summary-date-ranges | 2/2 | 0.046 | 41 | 7.5 | 55.5 | 2/2 | 0.028 | 23 | 5.0 | 0.0 | 2/2 | 0.023 | 22 | 7.5 | 6.5 |
| multi-source-data-merger | 2/2 | 0.133 | 123 | 17.5 | 18.5 | 2/2 | 0.057 | 48 | 8.5 | 0.0 | 2/2 | 0.109 | 115 | 16.5 | 17.5 |
| nginx-request-logging | 2/2 | 0.104 | 108 | 18.5 | 54.0 | 2/2 | 0.073 | 61 | 14.0 | 0.0 | 2/2 | 0.061 | 74 | 19.0 | 18.0 |
| openssl-selfsigned-cert | 1/2 | 0.060 | 62 | 8.5 | 25.5 | 1/2 | 0.052 | 46 | 11.0 | 0.0 | 0/2 | 0.026 | 34 | 9.0 | 9.0 |
| polyglot-c-py | 0/2 | 0.119 | 92 | 25.0 | 61.5 | 0/2 | 0.126 | 650 | 7.0 | 0.0 | 1/2 | 1.118 | 508 | 158.5 | 157.5 |
| prove-plus-comm | 2/2 | 0.059 | 50 | 16.5 | 18.5 | 2/2 | 0.132 | 115 | 13.0 | 0.0 | 2/2 | 0.033 | 37 | 15.0 | 14.0 |
| pypi-server | 2/2 | 0.151 | 910 | 38.5 | 36.5 | 2/2 | 0.072 | 52 | 14.5 | 0.0 | 2/2 | 0.160 | 173 | 41.0 | 40.0 |
| pytorch-model-cli | 0/1 | 0.133 | 267 | 37.0 | 35.0 | 0/0 | nan | nan | nan | nan | 1/1 | 0.067 | 162 | 23.0 | 23.0 |
| pytorch-model-recovery | 2/2 | 0.204 | 262 | 25.0 | 14.5 | 0/0 (2 err) | 0.000 | 0 | 0.0 | 0.0 | 1/2 | 0.297 | 910 | 39.5 | nan |
| query-optimize | 0/1 | 0.058 | 131 | 20.0 | 10.0 | 1/1 | 0.091 | 296 | 11.0 | 0.0 | 0/1 | 0.052 | 331 | 21.0 | 20.0 |
| regex-log | 0/2 | 0.262 | 209 | 29.5 | 27.0 | 0/2 | 0.108 | 112 | 2.0 | 0.0 | 0/2 | 0.222 | 214 | 42.0 | 41.0 |
| sqlite-db-truncate | 0/2 | 0.078 | 87 | 13.0 | 13.0 | 0/2 | 0.090 | 78 | 11.5 | 0.0 | 0/2 | 0.145 | 127 | 16.0 | 15.0 |
| vulnerable-secret | 1/2 | 0.099 | 79 | 22.5 | 28.5 | 2/2 | 0.106 | 75 | 20.0 | 0.0 | 2/2 | 0.063 | 59 | 21.5 | 20.5 |

Tasks where nanocode passes more often: fix-code-vulnerability, hf-model-inference, largest-eigenval
Tasks where claude-code passes more often: extract-elf, git-multibranch, kv-store-grpc, query-optimize, vulnerable-secret

Paired by task and attempt (61 pairs), claude-code minus nanocode, mean with its 95% normal interval:
- cost ($): -0.0214 (-0.0508 to 0.0079)
- wall (s): -45.7 (-115.2 to 23.8)
- requests: -8.64 (-11.83 to -5.45)
- prompt tokens: -8481 (-164497 to 147534)
Paired outcomes (59 pairs both scored): both passed 28, both failed 22, only claude-code passed 6, only nanocode passed 3; exact two-sided sign test on the discordant pairs p = 0.508
Only nanocode passed: fix-code-vulnerability.2, hf-model-inference.1, largest-eigenval.2
Only claude-code passed: extract-elf.1, extract-elf.2, git-multibranch.2, kv-store-grpc.1, query-optimize.1, vulnerable-secret.2

Tasks where nanocode passes more often: hf-model-inference, openssl-selfsigned-cert, pytorch-model-recovery
Tasks where hl-agent passes more often: configure-git-webserver, extract-elf, kv-store-grpc, largest-eigenval, polyglot-c-py, pytorch-model-cli, vulnerable-secret

Paired by task and attempt (62 pairs), hl-agent minus nanocode, mean with its 95% normal interval:
- cost ($): 0.0210 (-0.0396 to 0.0817)
- wall (s): -46.2 (-120.3 to 27.8)
- requests: 4.95 (-2.65 to 12.55)
- prompt tokens: 162531 (-279246 to 604307)
Paired outcomes (62 pairs both scored): both passed 29, both failed 20, only hl-agent passed 9, only nanocode passed 4; exact two-sided sign test on the discordant pairs p = 0.267
Only nanocode passed: cancel-async-tasks.2, hf-model-inference.1, openssl-selfsigned-cert.2, pytorch-model-recovery.2
Only hl-agent passed: cancel-async-tasks.1, configure-git-webserver.1, extract-elf.1, extract-elf.2, kv-store-grpc.1, largest-eigenval.1, polyglot-c-py.2, pytorch-model-cli.1, vulnerable-secret.2

Tasks where claude-code passes more often: git-multibranch, openssl-selfsigned-cert, query-optimize
Tasks where hl-agent passes more often: configure-git-webserver, fix-code-vulnerability, largest-eigenval, polyglot-c-py

Paired by task and attempt (61 pairs), hl-agent minus claude-code, mean with its 95% normal interval:
- cost ($): 0.0439 (-0.0211 to 0.1089)
- wall (s): 0.4 (-66.0 to 66.8)
- requests: 13.90 (5.90 to 21.90)
- prompt tokens: 179480 (-290518 to 649478)
Paired outcomes (59 pairs both scored): both passed 30, both failed 19, only hl-agent passed 6, only claude-code passed 4; exact two-sided sign test on the discordant pairs p = 0.754
Only claude-code passed: cancel-async-tasks.2, git-multibranch.2, openssl-selfsigned-cert.2, query-optimize.1
Only hl-agent passed: cancel-async-tasks.1, configure-git-webserver.1, fix-code-vulnerability.2, largest-eigenval.1, largest-eigenval.2, polyglot-c-py.2

nanocode rows with a request the upstream connection dropped: 0 
claude-code rows with a request the upstream connection dropped: 4 terminal-bench-2.1.adaptive-rejection-sampler.claude-code.1 terminal-bench-2.1.chess-best-move.claude-code.2 terminal-bench-2.1.polyglot-c-py.claude-code.1 terminal-bench-2.1.polyglot-c-py.claude-code.2
hl-agent rows with a request the upstream connection dropped: 1 terminal-bench-2.1.adaptive-rejection-sampler.hl-agent.2
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.1: no model request succeeded
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.2: no model request succeeded

#### claude-haiku-4-5, suite 1

Model: claude-haiku-4-5. Prices: https://docs.anthropic.com/en/docs/about-claude/pricing, read 2026-11-14; https://developers.openai.com/api/docs/models/gpt-5.5 (prompts under 272K tokens), read 2026-11-15.

| Agent | Attempts | Passed | Failed | Errored | Pass rate (95% Wilson) | Cost | Cost/attempt | Wall/attempt | Requests/attempt | Model tool calls/attempt | Inner tool calls/attempt | Input+cache tokens/attempt | Output tokens/attempt |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| nanocode | 34 | 21 | 13 | 0 | 61.8% (45.0-76.1%) | $4.57 | $0.134 | 211 s | 22.8 | 21.6 | 34.6 | 535609 | 11276 |
| claude-code | 34 | 22 | 12 | 0 | 64.7% (47.9-78.5%) | $4.70 | $0.138 | 179 s | 17.4 | 17.6 | 0.0 | 653910 | 9457 |
| hl-agent | 34 | 24 | 10 | 0 | 70.6% (53.8-83.2%) | $6.64 | $0.195 | 153 s | 36.0 | 35.8 | 35.8 | 1032238 | 12371 |

| Task | nanocode pass | nanocode $ | nanocode s | nanocode req | nanocode tools | claude-code pass | claude-code $ | claude-code s | claude-code req | claude-code tools | hl-agent pass | hl-agent $ | hl-agent s | hl-agent req | hl-agent tools |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| build-cython-ext | 0/2 | 0.416 | 301 | 62.0 | 71.5 | 0/2 | 0.409 | 229 | 60.5 | 0.0 | 0/2 | 0.503 | 354 | 82.0 | 82.5 |
| cancel-async-tasks | 1/2 | 0.015 | 17 | 5.0 | 3.5 | 1/2 | 0.026 | 22 | 2.0 | 0.0 | 1/2 | 0.032 | 32 | 8.0 | 7.0 |
| cobol-modernization | 2/2 | 0.353 | 265 | 42.0 | 76.5 | 2/2 | 0.236 | 156 | 26.5 | 0.0 | 2/2 | 0.456 | 322 | 83.5 | 90.0 |
| fix-git | 2/2 | 0.027 | 21 | 8.5 | 7.5 | 2/2 | 0.054 | 31 | 10.0 | 0.0 | 2/2 | 0.020 | 23 | 11.0 | 10.0 |
| git-leak-recovery | 2/2 | 0.028 | 36 | 9.5 | 19.0 | 2/2 | 0.041 | 33 | 11.0 | 0.0 | 2/2 | 0.018 | 24 | 12.5 | 11.5 |
| kv-store-grpc | 1/2 | 0.047 | 910 | 13.0 | 14.5 | 2/2 | 0.122 | 910 | 15.0 | 0.0 | 2/2 | 0.089 | 198 | 22.0 | 20.5 |
| largest-eigenval | 1/2 | 0.286 | 279 | 50.0 | 57.5 | 0/2 | 0.618 | 403 | 63.5 | 0.0 | 2/2 | 0.243 | 282 | 47.5 | 47.5 |
| log-summary-date-ranges | 2/2 | 0.046 | 41 | 7.5 | 55.5 | 2/2 | 0.028 | 23 | 5.0 | 0.0 | 2/2 | 0.023 | 22 | 7.5 | 6.5 |
| multi-source-data-merger | 2/2 | 0.133 | 123 | 17.5 | 18.5 | 2/2 | 0.057 | 48 | 8.5 | 0.0 | 2/2 | 0.109 | 115 | 16.5 | 17.5 |
| nginx-request-logging | 2/2 | 0.104 | 108 | 18.5 | 54.0 | 2/2 | 0.073 | 61 | 14.0 | 0.0 | 2/2 | 0.061 | 74 | 19.0 | 18.0 |
| openssl-selfsigned-cert | 1/2 | 0.060 | 62 | 8.5 | 25.5 | 1/2 | 0.052 | 46 | 11.0 | 0.0 | 0/2 | 0.026 | 34 | 9.0 | 9.0 |
| polyglot-c-py | 0/2 | 0.119 | 92 | 25.0 | 61.5 | 0/2 | 0.126 | 650 | 7.0 | 0.0 | 1/2 | 1.118 | 508 | 158.5 | 157.5 |
| prove-plus-comm | 2/2 | 0.059 | 50 | 16.5 | 18.5 | 2/2 | 0.132 | 115 | 13.0 | 0.0 | 2/2 | 0.033 | 37 | 15.0 | 14.0 |
| pypi-server | 2/2 | 0.151 | 910 | 38.5 | 36.5 | 2/2 | 0.072 | 52 | 14.5 | 0.0 | 2/2 | 0.160 | 173 | 41.0 | 40.0 |
| regex-log | 0/2 | 0.262 | 209 | 29.5 | 27.0 | 0/2 | 0.108 | 112 | 2.0 | 0.0 | 0/2 | 0.222 | 214 | 42.0 | 41.0 |
| sqlite-db-truncate | 0/2 | 0.078 | 87 | 13.0 | 13.0 | 0/2 | 0.090 | 78 | 11.5 | 0.0 | 0/2 | 0.145 | 127 | 16.0 | 15.0 |
| vulnerable-secret | 1/2 | 0.099 | 79 | 22.5 | 28.5 | 2/2 | 0.106 | 75 | 20.0 | 0.0 | 2/2 | 0.063 | 59 | 21.5 | 20.5 |

Tasks where nanocode passes more often: largest-eigenval
Tasks where claude-code passes more often: kv-store-grpc, vulnerable-secret

Paired by task and attempt (34 pairs), claude-code minus nanocode, mean with its 95% normal interval:
- cost ($): 0.0039 (-0.0360 to 0.0438)
- wall (s): -31.9 (-119.9 to 56.0)
- requests: -5.41 (-9.62 to -1.20)
- prompt tokens: 118300 (-114240 to 350841)
Paired outcomes (34 pairs both scored): both passed 20, both failed 11, only claude-code passed 2, only nanocode passed 1; exact two-sided sign test on the discordant pairs p = 1.000
Only nanocode passed: largest-eigenval.2
Only claude-code passed: kv-store-grpc.1, vulnerable-secret.2

Tasks where nanocode passes more often: openssl-selfsigned-cert
Tasks where hl-agent passes more often: kv-store-grpc, largest-eigenval, polyglot-c-py, vulnerable-secret

Paired by task and attempt (34 pairs), hl-agent minus nanocode, mean with its 95% normal interval:
- cost ($): 0.0609 (-0.0426 to 0.1645)
- wall (s): -58.3 (-151.6 to 35.1)
- requests: 13.26 (0.68 to 25.85)
- prompt tokens: 496629 (-268542 to 1261800)
Paired outcomes (34 pairs both scored): both passed 19, both failed 8, only hl-agent passed 5, only nanocode passed 2; exact two-sided sign test on the discordant pairs p = 0.453
Only nanocode passed: cancel-async-tasks.2, openssl-selfsigned-cert.2
Only hl-agent passed: cancel-async-tasks.1, kv-store-grpc.1, largest-eigenval.1, polyglot-c-py.2, vulnerable-secret.2

Tasks where claude-code passes more often: openssl-selfsigned-cert
Tasks where hl-agent passes more often: largest-eigenval, polyglot-c-py

Paired by task and attempt (34 pairs), hl-agent minus claude-code, mean with its 95% normal interval:
- cost ($): 0.0570 (-0.0503 to 0.1644)
- wall (s): -26.3 (-93.1 to 40.4)
- requests: 18.68 (5.11 to 32.24)
- prompt tokens: 378329 (-437895 to 1194552)
Paired outcomes (34 pairs both scored): both passed 20, both failed 8, only hl-agent passed 4, only claude-code passed 2; exact two-sided sign test on the discordant pairs p = 0.688
Only claude-code passed: cancel-async-tasks.2, openssl-selfsigned-cert.2
Only hl-agent passed: cancel-async-tasks.1, largest-eigenval.1, largest-eigenval.2, polyglot-c-py.2

nanocode rows with a request the upstream connection dropped: 0 
claude-code rows with a request the upstream connection dropped: 2 terminal-bench-2.1.polyglot-c-py.claude-code.1 terminal-bench-2.1.polyglot-c-py.claude-code.2
hl-agent rows with a request the upstream connection dropped: 0

#### claude-haiku-4-5, suite 2

Model: claude-haiku-4-5. Prices: https://docs.anthropic.com/en/docs/about-claude/pricing, read 2026-11-14; https://developers.openai.com/api/docs/models/gpt-5.5 (prompts under 272K tokens), read 2026-11-15.

| Agent | Attempts | Passed | Failed | Errored | Pass rate (95% Wilson) | Cost | Cost/attempt | Wall/attempt | Requests/attempt | Model tool calls/attempt | Inner tool calls/attempt | Input+cache tokens/attempt | Output tokens/attempt |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| nanocode | 28 | 12 | 16 | 0 | 42.9% (26.5-60.9%) | $5.27 | $0.188 | 284 s | 32.2 | 31.0 | 41.8 | 798762 | 15594 |
| claude-code | 27 | 12 | 13 | 2 | 48.0% (30.0-66.5%) | $3.69 | $0.137 | 221 s | 19.4 | 18.4 | 0.0 | 639005 | 9283 |
| hl-agent | 28 | 14 | 14 | 0 | 50.0% (32.6-67.4%) | $4.50 | $0.161 | 252 s | 27.1 | 26.2 | 24.2 | 555602 | 14614 |

| Task | nanocode pass | nanocode $ | nanocode s | nanocode req | nanocode tools | claude-code pass | claude-code $ | claude-code s | claude-code req | claude-code tools | hl-agent pass | hl-agent $ | hl-agent s | hl-agent req | hl-agent tools |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| adaptive-rejection-sampler | 1/2 | 0.570 | 712 | 52.0 | 58.0 | 1/2 | 0.346 | 655 | 37.5 | 0.0 | 1/2 | 0.659 | 891 | 55.5 | 57.0 |
| build-pmars | 2/2 | 0.127 | 142 | 27.5 | 55.5 | 2/2 | 0.124 | 107 | 29.0 | 0.0 | 2/2 | 0.082 | 106 | 30.5 | 29.5 |
| chess-best-move | 0/2 | 0.097 | 101 | 13.5 | 2.0 | 0/2 | 0.152 | 374 | 3.5 | 0.0 | 0/2 | 0.081 | 102 | 12.5 | 11.5 |
| configure-git-webserver | 0/2 | 0.063 | 900 | 15.5 | 24.0 | 0/2 | 0.026 | 14 | 2.0 | 0.0 | 1/2 | 0.027 | 54 | 6.5 | 5.5 |
| count-dataset-tokens | 0/2 | 0.045 | 96 | 11.0 | 12.0 | 0/2 | 0.066 | 147 | 13.5 | 0.0 | 0/2 | 0.062 | 97 | 14.5 | 13.5 |
| db-wal-recovery | 0/2 | 0.228 | 228 | 48.5 | 60.5 | 0/2 | 0.218 | 176 | 36.0 | 0.0 | 0/2 | 0.070 | 81 | 29.5 | 28.5 |
| extract-elf | 0/2 | 0.240 | 188 | 42.5 | 47.0 | 2/2 | 0.109 | 101 | 14.5 | 0.0 | 2/2 | 0.074 | 74 | 19.0 | 18.0 |
| fix-code-vulnerability | 2/2 | 0.353 | 205 | 63.0 | 76.0 | 1/2 | 0.250 | 126 | 43.0 | 0.0 | 2/2 | 0.114 | 89 | 36.5 | 35.0 |
| gcode-to-text | 0/2 | 0.070 | 66 | 16.5 | 15.5 | 0/2 | 0.081 | 84 | 9.5 | 0.0 | 0/2 | 0.085 | 76 | 19.0 | 18.0 |
| git-multibranch | 1/2 | 0.279 | 260 | 53.5 | 104.5 | 2/2 | 0.268 | 224 | 46.0 | 0.0 | 1/2 | 0.376 | 376 | 49.5 | 48.5 |
| headless-terminal | 2/2 | 0.065 | 82 | 14.5 | 15.5 | 2/2 | 0.044 | 46 | 6.5 | 0.0 | 2/2 | 0.067 | 132 | 15.0 | 14.0 |
| hf-model-inference | 2/2 | 0.197 | 529 | 40.0 | 77.5 | 1/2 | 0.116 | 782 | 15.0 | 0.0 | 1/2 | 0.195 | 292 | 30.0 | 30.0 |
| pytorch-model-cli | 0/1 | 0.133 | 267 | 37.0 | 35.0 | 0/0 | nan | nan | nan | nan | 1/1 | 0.067 | 162 | 23.0 | 23.0 |
| pytorch-model-recovery | 2/2 | 0.204 | 262 | 25.0 | 14.5 | 0/0 (2 err) | 0.000 | 0 | 0.0 | 0.0 | 1/2 | 0.297 | 910 | 39.5 | nan |
| query-optimize | 0/1 | 0.058 | 131 | 20.0 | 10.0 | 1/1 | 0.091 | 296 | 11.0 | 0.0 | 0/1 | 0.052 | 331 | 21.0 | 20.0 |

Tasks where nanocode passes more often: fix-code-vulnerability, hf-model-inference
Tasks where claude-code passes more often: extract-elf, git-multibranch, query-optimize

Paired by task and attempt (27 pairs), claude-code minus nanocode, mean with its 95% normal interval:
- cost ($): -0.0533 (-0.0944 to -0.0123)
- wall (s): -63.0 (-175.8 to 49.9)
- requests: -12.70 (-17.21 to -8.19)
- prompt tokens: -168133 (-351767 to 15501)
Paired outcomes (25 pairs both scored): both passed 8, both failed 11, only claude-code passed 4, only nanocode passed 2; exact two-sided sign test on the discordant pairs p = 0.688
Only nanocode passed: fix-code-vulnerability.2, hf-model-inference.1
Only claude-code passed: extract-elf.1, extract-elf.2, git-multibranch.2, query-optimize.1

Tasks where nanocode passes more often: hf-model-inference, pytorch-model-recovery
Tasks where hl-agent passes more often: configure-git-webserver, extract-elf, pytorch-model-cli

Paired by task and attempt (28 pairs), hl-agent minus nanocode, mean with its 95% normal interval:
- cost ($): -0.0274 (-0.0708 to 0.0160)
- wall (s): -31.6 (-151.8 to 88.6)
- requests: -5.14 (-10.38 to 0.10)
- prompt tokens: -243160 (-497542 to 11222)
Paired outcomes (28 pairs both scored): both passed 10, both failed 12, only hl-agent passed 4, only nanocode passed 2; exact two-sided sign test on the discordant pairs p = 0.688
Only nanocode passed: hf-model-inference.1, pytorch-model-recovery.2
Only hl-agent passed: configure-git-webserver.1, extract-elf.1, extract-elf.2, pytorch-model-cli.1

Tasks where claude-code passes more often: git-multibranch, query-optimize
Tasks where hl-agent passes more often: configure-git-webserver, fix-code-vulnerability

Paired by task and attempt (27 pairs), hl-agent minus claude-code, mean with its 95% normal interval:
- cost ($): 0.0274 (-0.0321 to 0.0868)
- wall (s): 34.1 (-90.7 to 158.9)
- requests: 7.89 (2.44 to 13.33)
- prompt tokens: -70922 (-335981 to 194136)
Paired outcomes (25 pairs both scored): both passed 10, both failed 11, only hl-agent passed 2, only claude-code passed 2; exact two-sided sign test on the discordant pairs p = 1.000
Only claude-code passed: git-multibranch.2, query-optimize.1
Only hl-agent passed: configure-git-webserver.1, fix-code-vulnerability.2

nanocode rows with a request the upstream connection dropped: 0 
claude-code rows with a request the upstream connection dropped: 2 terminal-bench-2.1.adaptive-rejection-sampler.claude-code.1 terminal-bench-2.1.chess-best-move.claude-code.2
hl-agent rows with a request the upstream connection dropped: 1 terminal-bench-2.1.adaptive-rejection-sampler.hl-agent.2
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.1: no model request succeeded
errored: terminal-bench-2.1.pytorch-model-recovery.claude-code.2: no model request succeeded

<!-- eval-confirm:end -->

## Three Codex arms on gpt-5.6-sol through a ChatGPT plan

Suite 1 (`evals/suite.txt`, 20 tasks), one attempt per task and arm, `--model gpt-5.6-sol --limits none --openai-route chatgpt`, every request through the meter to the ChatGPT backend with one plan's login, which the meter reads per request on the eval machine; the agents in the containers get a placeholder key. Stock Codex is `codex exec` 0.169.0 with its code-mode host; nanocode is its Codex backend through `NANOCODE_OPENAI_BASE_URL`; hl-agent runs `examples/codex_agent.hl`. Cost is gpt-5.6-sol's list price ($4 and $20 per million input and output tokens, $0.40 cached); the plan bills none. The ChatGPT backend refuses gpt-5.5 for plan accounts, so gpt-5.6-sol stands in. Rows `hl-agent (low effort)` and `hl-agent (low effort, one gate turn)` are reruns of the hl-agent arm with the fixes from docs/GAPS.md, Gaps measured against stock Codex.

| Agent | Attempts | Passed | Pass rate (95% Wilson) | Cost/attempt | Wall/attempt | Requests/attempt | Tool calls/attempt | Input+cache tokens/attempt | Output tokens/attempt |
| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Stock Codex | 20 | 18 | 90.0% (69.9-97.2%) | $0.136 | 57 s | 8.5 | 7.5 | 138998 | 1939 |
| nanocode | 20 | 20 | 100.0% (83.9-100.0%) | $0.095 | 56 s | 6.5 | 6.9 | 54605 | 2025 |
| hl-agent | 20 | 20 | 100.0% (83.9-100.0%) | $0.618 | 198 s | 20.7 | 27.7 | 411414 | 7640 |
| hl-agent (low effort) | 20 | 19 | 95.0% (76.4-99.1%) | $0.457 | 154 s | 16.1 | 21.5 | 233997 | 5465 |
| hl-agent (low effort, one gate turn) | 20 | 20 | 100.0% (83.9-100.0%) | $0.363 | 119 s | 13.6 | 19.9 | 202875 | 3890 |

Stock Codex failed pypi-server and kv-store-grpc (the server it started was gone at verification); hl-agent with low effort failed largest-eigenval's speedup test. No request returned anything but 200 and no attempt timed out.
