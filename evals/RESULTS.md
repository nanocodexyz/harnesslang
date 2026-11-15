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

## The closed gaps on the full suite (running)

hl-agent at this branch (the Inputs and Finishing blocks, the `/bin/sh` fallback) against nanocode c7679e3 and Claude Code 2.1.325, 40 tasks x 2 attempts, parallel 3, started 2026-11-15 04:20 UTC under a $50 cap on metered spend, into `output/eval/r5`. Its table goes here when it finishes.

## Earlier runs

Over the eval-head lane's main and second suites (100 attempts per agent each, nanocode and hl-agent in one pool, before the Bash kill fix and the Finishing block), hl-agent passed 78.6% at $0.239 an attempt against nanocode's 73.0% at $0.272, 11 tasks to 7 (p = 0.481). DECISIONS.md (2026-11-14, hl-agent on 40 Terminal-Bench tasks) has the effort pilot and the kill-fix rerun.

## Gap reruns

hl-agent alone on the tasks behind the gaps mined from these traces, before and after each fix: docs/GAPS.md, "Gaps measured against stock Claude Code".

## Stock Codex, hl-agent's codex_agent.hl and nanocode's Codex backend

Not run on a real model: the Hand has no OpenAI API key. The wiring is checked by `evals/codex-drill.sh` against a fake Responses API (each arm on the 3 nanoeval tasks; the meter's key, usage and WebSocket refusal are checked on every row): codex 3/3, nanocode 3/3, hl-agent 3/3 (it was 1/3 until `exec_command` fell back to `/bin/sh` in images without bash). With a key, the comparison is `python3 evals/run.py --suite evals/suite.txt --tasks output/eval/tasks --agent-root output/eval/agent-root --out output/eval/codex --agents codex,nanocode,hl-agent --attempts 1 --model gpt-5.5 --parallel 3 --limits none`, then `python3 evals/report.py output/eval/codex`.
