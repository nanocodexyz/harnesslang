# gpt-6-luna escalating to gpt-6.1-terra, November 2026

`codex_agent.hl` on gpt-6-luna at effort medium, escalating to gpt-6.1-terra
(`evals/luna-terra.sh --effort medium --escalate-model gpt-6.1-terra`), on
`evals/suite-60.txt`, 3 attempts per task, ChatGPT plan. Each row is one attempt in
the task's own container, scored by the task's verifier; scored with
`evals/lunaterra.py`. Cost is what the meter priced the plan's tokens at.

Three arms:

- **baseline**: the earlier Rust host (`agent/`, before 623fbee) with escalation, an
  unpushed build. It stopped at 136 of 180 rows (135 scored, 1 errored).
- **623fbee**: `hl-host` at 623fbee, the full 180 rows.
- **66a2bec**: `hl-host` at 66a2bec, a rerun of 15 tasks (below), 3 attempts each.

## Full run (623fbee)

| Arm | Passed / scored | Pass rate (95% Wilson) | Cost/attempt | Escalated rows (passed) |
| --- | --- | --- | --- | --- |
| 623fbee, all rows | 138 / 180 | 76.7% (70.0 to 82.2) | $0.0182 | 28 (15) |
| baseline, all rows | 108 / 135 (1 errored) | 80.0% (72.5 to 85.9) | $0.0157 | 21 (11) |
| 623fbee, paired rows | 104 / 135 | 77.0% (69.3 to 83.3) | $0.0128 | 17 (6) |
| baseline, paired rows | 108 / 135 | 80.0% (72.5 to 85.9) | $0.0157 | 21 (11) |

Paired by task and attempt, 135 pairs: both passed 96, both failed 19, only 623fbee 8,
only baseline 12, sign test p = 0.503. The sign test sees no difference.

- Only 623fbee passed: adaptive-rejection-sampler.3, bn-fit-modify.2, bn-fit-modify.3,
  caffe-cifar-10.2, circuit-fibsqrt.1, largest-eigenval.1, largest-eigenval.2,
  tune-mjcf.3.
- Only baseline passed: build-pov-ray.2, build-pov-ray.3, caffe-cifar-10.1,
  cancel-async-tasks.2, chess-best-move.2, db-wal-recovery.3, fix-code-vulnerability.1,
  fix-code-vulnerability.3, gcode-to-text.1, gcode-to-text.2, gcode-to-text.3,
  largest-eigenval.3.

The last three regressions to land were not harness problems. build-pov-ray.2 and .3
built POV-Ray from the wrong source release. caffe-cifar-10.1 trained the model but
built the binary outside the paths the test looks in.

## Escalation: run versus baseline

Partway through the run, 11 escalated rows had 1 pass, against the baseline's 21 with
11. That gap came from which rows escalated, not from escalation doing worse. On the 10
task-attempts both arms escalated, 623fbee passed 5 and the baseline 4.
break-filter-js-from-html.1-.3 and feal-differential-cryptanalysis.1 passed in both, and
circuit-fibsqrt.1 passed only in 623fbee. db-wal-recovery.1-.2, extract-elf.2-.3 and
sanitize-git-repo.1-.2 failed in both. 5 of the baseline's 11 escalation passes came from
chess-best-move and gcode-to-text. In 623fbee those rows never escalated: Luna read the
answer off an image by eye, the finish check accepted it, and it reported
`VERDICT: PASS`.

Differences found in the journals (`requests.jsonl`, `last-request.json`) and the
baseline binary's strings:

| | baseline | 623fbee |
| --- | --- | --- |
| Escalation fires | after the second failed finish check (`CfCfE`) | after the first failed finish check (`CfE`), or the second when the first passed |
| Finish checks for Terra after escalating | 0 | 1 (the one left of the two allowed) |
| Context Terra gets | full history, but Luna's encrypted reasoning items dropped at the switch (input items fall, e.g. 70 to 59, and 1 to 9 reasoning items remain) | full history with all of Luna's reasoning items (6 to 34) |
| Summary or compaction at the switch | none | none |
| `view_image` tool | not offered | offered (chess-best-move and gcode-to-text used it) |
| ESCALATE prompt (`Your last report does not show…`) | same text | same text |
| Model and effort after escalating | gpt-6.1-terra, medium | gpt-6.1-terra, medium |
| `max_output_tokens`, turn limits | unset; no row exhausted (101 requests at most) | unset; rounds_max 512, no row exhausted (150 requests at most) |

The causes:

1. **The image readings.** With `view_image`, Luna renders the image, reads it by eye,
   reports PASS on its own reading and never escalates. That cost gcode-to-text 3/3 and
   chess-best-move 2/3. 66a2bec adds one sentence to the finish check: a value read off
   an image by looking at it is not verified until a second method that does not rely
   on the model's eyes (OCR, or parsing the data the image was drawn from) gives the
   same value. An answer picked from options the task lists must also be one of them,
   spelled as listed.
2. **Escalating on the first failed check.** Luna gets one fewer round to fix its own
   work before Terra takes over, and Terra gets a finish check of its own. On these
   rows it made no measurable difference (5 against 4 on the shared attempts), so it
   stays as is.
3. **Luna's reasoning carried over to Terra.** The requests succeed and the shared
   attempts do no worse, so this stays as is too.

## Rerun on 66a2bec

The tasks of the 9 regressed task-attempts, plus every task with an escalated row in
either arm: 15 tasks, 3 attempts each. They ran on a fresh master build (66a2bec,
`hl-host` built from `hosts/native`, root from `evals/agent-root.sh`), with their own
output directory and meter ports.

| Arm | Passed / scored | Pass rate (95% Wilson) | Cost/attempt | Escalated rows (passed) |
| --- | --- | --- | --- | --- |
| 66a2bec | 29 / 45 | 64.4% (49.8 to 76.8) | $0.0276 | 22 (15) |
| 623fbee, same 45 rows | 20 / 45 | 44.4% (30.9 to 58.8) | $0.0240 | 21 (10) |
| 66a2bec, 41 rows paired with baseline | 25 / 41 | 61.0% (45.7 to 74.3) | $0.0256 | 18 (11) |
| baseline, same 41 rows | 21 / 41 | 51.2% (36.5 to 65.7) | $0.0319 | 21 (11) |

- Against 623fbee, 45 pairs: both passed 20, both failed 16, only 66a2bec 9, only
  623fbee 0, sign test p = 0.004. Only 66a2bec passed: cancel-async-tasks.3,
  chess-best-move.1, chess-best-move.2, fix-code-vulnerability.1,
  fix-code-vulnerability.3, gcode-to-text.1, gcode-to-text.3, largest-eigenval.3,
  sanitize-git-repo.3.
- Against the baseline, 41 pairs: both passed 18, both failed 13, only 66a2bec 7, only
  baseline 3, sign test p = 0.344. Only the baseline passed: cancel-async-tasks.2,
  db-wal-recovery.3, gcode-to-text.2.

| Task | baseline | 623fbee | 66a2bec |
| --- | --- | --- | --- |
| break-filter-js-from-html | PPP | PPP | PPP |
| cancel-async-tasks | PPF | PFF | PFP |
| chess-best-move | FPP | FFP | PPP |
| circuit-fibsqrt | F-- | PPP | PPP |
| configure-git-webserver | PPP | PPP | PPP |
| db-wal-recovery | FFP | FFF | FFF |
| extract-elf | FFF | FFF | FFF |
| feal-differential-cryptanalysis | P-- | PPP | PPP |
| fix-code-vulnerability | PPP | FPF | PPP |
| gcode-to-text | PPP | FFF | PFP |
| largest-eigenval | FFP | PPF | PPP |
| query-optimize | FFF | FFF | FFF |
| raman-fitting | FFF | FFF | FFF |
| sanitize-git-repo | FFF | FFF | FFP |
| tune-mjcf | PPF | PPP | PPP |

(- is a row the baseline never reached.)

gcode-to-text went from 0/3 to 2/3; gcode-to-text.1 escalated and passed. In the one it
still failed, gcode-to-text.2, the model cross-checked against the object name in the
G-code (`M486 AEmbossed text`) instead of the drawn path, and both methods agreed on
the wrong answer. chess-best-move went from 1/3 to 3/3. Some of the 9 wins over 623fbee
(fix-code-vulnerability, cancel-async-tasks, largest-eigenval) are on tasks the change
does not touch. Expect run-to-run noise of that size at 3 attempts.

## Commits

- 66a2bec fix(codex): do not let the finish check accept image readings by eye alone.
  `scripts/ci.sh` and `scripts/codex-drill.sh` passed.

## Full run on master (1d5b56d)

1d5b56d is 364f1ed (subscription sign-in and MPP in `.hl`) plus a docs commit, so it carries
66a2bec. `hl-host` built from `hosts/native` with Rust 1.100.0 (hl-agent sha256
`2672b1e4…`), root from `evals/agent-root.sh`, then `evals/luna-terra.sh --effort medium
--escalate-model gpt-6.1-terra` on all 60 tasks x 3 attempts, the same launch as the 623fbee
run but on its own meter and relay ports. The eval's explicit `OPENAI_BASE_URL` meter path
took priority over the new sign-in routes, as before. 180 of 180 rows scored, none errored.

| Arm | Passed / scored | Pass rate (95% Wilson) | Cost/attempt | Escalated rows (passed) |
| --- | --- | --- | --- | --- |
| 1d5b56d, all rows | 149 / 180 | 82.8% (76.6 to 87.6) | $0.0203 | 29 (19) |
| 623fbee, all rows | 138 / 180 | 76.7% (70.0 to 82.2) | $0.0182 | 28 (15) |
| 1d5b56d, rows paired with baseline | 113 / 135 | 83.7% (76.6 to 89.0) | $0.0147 | 18 (11) |
| baseline, paired rows | 108 / 135 | 80.0% (72.5 to 85.9) | $0.0157 | 21 (11) |

- Against 623fbee, 180 pairs: both passed 134, both failed 27, only 1d5b56d 15, only 623fbee 4,
  sign test p = 0.019. Only 1d5b56d passed: build-pov-ray.2, build-pov-ray.3, caffe-cifar-10.1,
  caffe-cifar-10.3, cancel-async-tasks.3, chess-best-move.2, compile-compcert.2, dna-assembly.1,
  financial-document-processor.3, fix-code-vulnerability.3, gcode-to-text.1, gcode-to-text.3,
  largest-eigenval.3, query-optimize.3, sanitize-git-repo.2. Only 623fbee passed:
  adaptive-rejection-sampler.1, bn-fit-modify.3, caffe-cifar-10.2, largest-eigenval.2.
- Against the baseline, 135 pairs: both passed 103, both failed 17, only 1d5b56d 10, only
  baseline 5, sign test p = 0.302. Only 1d5b56d passed: adaptive-rejection-sampler.3,
  bn-fit-modify.2, caffe-cifar-10.3, cancel-async-tasks.3, circuit-fibsqrt.1, dna-assembly.1,
  largest-eigenval.1, query-optimize.3, sanitize-git-repo.2, tune-mjcf.3. Only the baseline
  passed: adaptive-rejection-sampler.1, cancel-async-tasks.2, db-wal-recovery.3,
  fix-code-vulnerability.1, gcode-to-text.2.

The full run confirms the 66a2bec rerun: 1d5b56d passes more than 623fbee on the same 180 rows
(p = 0.019), and is level with or above the baseline on the 135 rows the baseline reached
(83.7% against 80.0%, p = 0.302), at a lower cost on those rows ($0.0147 against $0.0157).

## Where the cost goes

All figures are from the meter's per-request usage in `requests.jsonl`, priced with
`evals/report.py`. The 1d5b56d run cost $3.658 over 180 scored rows.

| | gpt-6-luna | gpt-6.1-terra |
| --- | --- | --- |
| Requests | 3461 | 390 |
| Uncached input | 6.06M tokens, $0.606 | 1.17M tokens, $1.172 |
| Cache reads | 52.6M tokens, $0.526 | 7.6M tokens, $0.378 |
| Output (reasoning) | 0.99M (0.28M), $0.494 | 0.10M (0.02M), $0.482 |
| Total | $1.626 (44%) | $2.032 (56%) |

- **Escalated rows are 63% of the spend**: 29 rows, $2.290, $0.079 per row (Luna $0.009, Terra
  $0.070), 19 passed. The other 151 rows cost $0.0091 each. The 10 escalated rows that failed
  cost $0.699 (19% of all spend), $0.626 of it Terra.
- **Terra's first request** re-reads Luna's whole transcript uncached (mean 14.6K uncached and
  2.6K cached tokens): $0.425, 11.6% of the spend.
- **Terra after its first report.** Terra always gets the finish check left over, so its work
  splits at its first report. Its later turns cost $0.727 (20%): 10 rows whose first Terra
  report ended `VERDICT: PASS` spent $0.085 there and all passed; 9 rows whose first report
  ended `VERDICT: FAIL` and that passed spent $0.446 (extract-moves-from-video made edits after
  its FAIL report in all three attempts); 10 rows that reported FAIL and failed spent $0.196.
- **Luna's reasoning items do not reach Terra's bill.** At the switch, Terra's first request
  context is smaller than Luna's last context plus Luna's last output by a median of 1,982
  tokens, against a median of 2,055 reasoning tokens Luna produced before the switch, row by
  row (-7,112 against 7,159, -6,234 against 6,707, …). The backend drops them, as the baseline
  did on its side (-3,797 against 3,858). Dropping them in the agent would save nothing.
- **Cache misses on the backend.** 321 Luna requests and 32 Terra requests read less from the
  cache than the previous request's prefix, with the same request shape, mid-turn and within
  seconds: $0.181 and $0.325 of extra input, 13.8% of the spend. `prompt_cache_key` is already
  set, so the agent has no clear lever here.
- **Images** are not a cost driver: requests that follow a `view_image` call added $0.042 of
  uncached input in all.

Why $/attempt went from $0.0157 (baseline) to $0.0203: on the 135 rows the baseline reached,
1d5b56d costs $0.0147, less than the baseline. The 45 rows the baseline never reached cost
$0.0372 each ($1.675; extract-moves-from-video alone $0.66, 18% of all spend, 3/3 passed after
escalating), which adds $0.0056 per attempt over 180 rows. Against 623fbee ($0.0182), Terra
spent $0.45 more (19 escalation passes against 15, 390 Terra requests against 348, Terra cache
misses $0.325 against $0.122) and Luna $0.07 less.

## Proposal (not applied)

Skip Terra's leftover finish check when Terra's own report already ends `VERDICT: PASS`. In
`turn_done` in `examples/codex_agent.hl`, the `checks_left > 0` branch would also require
`not (escalated and verdict_pass)`. Across the three runs with this escalation (623fbee, the
66a2bec rerun, 1d5b56d), Terra's first report ended PASS in 36 rows. The finish check that
followed made an edit in 1 of them and never turned a PASS into a FAIL. 35 of the 36 rows
passed, and the one that failed (largest-eigenval.3 on 623fbee) passed its check again, so the
check did not catch it either. On this run the change saves $0.085, about $0.0005 per attempt
(2.3%), with no pass the data shows it would lose.

Nothing bigger is safe on this evidence. Skipping Terra's check after a FAIL report would save
$0.64, but 9 passes came from rows in that state. Handing Terra a summary instead of Luna's
transcript would save up to about $0.73 (the first-request replay plus its later cache reads),
but how that affects passes is untested.

