(global $PENDING_BYTES i32 (i32.const 24))
(text $s_batch "batch")
(text $s_p_event "event")
(text $s_p_limit "limit")
(text $s_requests "requests")
(text $s_batch_answer "a batch answer lists each sent request once")
(text $s_batch_external "an external call of a parallel for runs alone")
(text $s_batch_durable "every call record of a parallel for is durable before an external call of it runs")
(func $target_limit (result i32) (i32.load offset=12 (i32.const $target)))
(func $target_limited (result i32) (i32.load offset=16 (i32.const $target)))
(func $dispatch_returns (param $site i32) (result i32)
  (select (i32.const $type_text) (i32.load offset=4 (call $site_result (local.get $site))) (i32.eq (call $site_kind (local.get $site)) (global.get $KIND_DISPATCH))))
(func $pending_push (param $pending i32) (param $index i32) (param $seq i64) (param $call i32) (param $reopened i32) (local $entry i32)
  (local.set $entry (call $vec_push (local.get $pending)))
  (i64.store (local.get $entry) (local.get $seq))
  (i32.store offset=8 (local.get $entry) (local.get $index))
  (i32.store offset=12 (local.get $entry) (local.get $call))
  (i32.store offset=16 (local.get $entry) (local.get $reopened)))
(func $pending_external (param $pending i32) (param $at i32) (result i32)
  (i32.eq (call $call_effect (i32.load offset=12 (call $vec_at (local.get $pending) (local.get $at)))) (global.get $EFFECT_EXTERNAL)))
(func $parallel (param $site i32) (param $items i64) (param $answer_site i32) (param $answers i32) (result i32) (local $answer i32) (local $count i32) (local $values i32) (local $pending i32) (local $index i32) (local $seq i64) (local $args i32) (local $call i32) (local $entry i32) (local $state i32) (local $recorded i32)
  (local.set $count (call $list_len (local.get $items)))
  (local.set $values (call $alloc (i32.shl (i32.add (local.get $count) (i32.const 1)) (i32.const 3))))
  (local.set $pending (call $vec_new (global.get $PENDING_BYTES)))
  block $resolved
    loop $next
      (br_if $resolved (i32.ge_u (local.get $index) (local.get $count)))
      (local.set $answer (if (result i32) (local.get $answers) (then (i32.load (i32.add (local.get $answers) (i32.shl (local.get $index) (i32.const 2))))) (else (i32.const 0))))
      (if (local.get $answer)
        (then
          (call $parallel_groups (local.get $site) (local.get $pending) (local.get $values))
          (if (global.get $halt) (then (return (local.get $values))))
          (local.set $pending (call $vec_new (global.get $PENDING_BYTES)))
          (i64.store (i32.add (local.get $values) (i32.shl (local.get $index) (i32.const 3))) (call $effect (local.get $answer_site) (local.get $answer)))
          (if (global.get $halt) (then (return (local.get $values))))
          (local.set $index (i32.add (local.get $index) (i32.const 1)))
          (br $next)))
      (global.set $seq (i64.add (global.get $seq) (i64.const 1)))
      (local.set $seq (global.get $seq))
      (local.set $args (call $alloc (i32.const 8)))
      (i64.store (local.get $args) (call $list_get (local.get $items) (local.get $index)))
      (local.set $call (call $resolve (local.get $site) (local.get $args)))
      (local.set $entry (call $log_site (global.get $log) (local.get $seq)))
      (if (i32.eqz (local.get $entry))
        (then
          (if (i32.eqz (call $require_live)) (then (return (local.get $values))))
          (local.set $call (call $fork_call (local.get $seq) (local.get $call)))
          (if (global.get $halt) (then (return (local.get $values))))
          (call $append (call $record_call (global.get $turn) (local.get $seq) (call $call_tool (local.get $call)) (call $call_effect (local.get $call)) (call $call_input (local.get $call))))
          (call $site_put (global.get $log) (local.get $seq) (global.get $SITE_OPEN) (local.get $call) (i32.const 0))
          (call $pending_push (local.get $pending) (local.get $index) (local.get $seq) (local.get $call) (i32.const 0)))
        (else
          (local.set $state (i32.load offset=8 (local.get $entry)))
          (if (i32.eq (local.get $state) (global.get $SITE_REPLY)) (then (call $corrupt (i32.const $s_diverged)) (return (local.get $values))))
          (local.set $recorded (i32.load offset=12 (local.get $entry)))
          (if (i32.eqz (call $same (local.get $recorded) (local.get $call))) (then (return (local.get $values))))
          (if (i32.eq (local.get $state) (global.get $SITE_DONE))
            (then (i64.store (i32.add (local.get $values) (i32.shl (local.get $index) (i32.const 3))) (call $settle (local.get $site) (local.get $recorded) (i32.load offset=16 (local.get $entry))))))
          (if (i32.eq (local.get $state) (global.get $SITE_UNKNOWN))
            (then (i64.store (i32.add (local.get $values) (i32.shl (local.get $index) (i32.const 3))) (call $sum_0 (global.get $VARIANT_UNKNOWN)))))
          (if (i32.eq (local.get $state) (global.get $SITE_OPEN))
            (then
              (if (i32.eqz (call $require_live)) (then (return (local.get $values))))
              (call $assert (i32.or (i32.or (i32.ne (call $call_effect (local.get $call)) (global.get $EFFECT_EXTERNAL)) (call $target_in_step)) (call $attested_take (local.get $seq))) (i32.const $s_reopen_external))
              (call $pending_push (local.get $pending) (local.get $index) (local.get $seq) (local.get $recorded) (i32.const 1))))))
      (if (global.get $halt) (then (return (local.get $values))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (call $parallel_groups (local.get $site) (local.get $pending) (local.get $values))
  (local.get $values))
(func $parallel_groups (param $site i32) (param $pending i32) (param $values i32) (local $start i32) (local $end i32)
  block $grouped
    loop $group
      (br_if $grouped (i32.ge_u (local.get $start) (call $vec_count (local.get $pending))))
      (local.set $end (i32.add (local.get $start) (i32.const 1)))
      (if (i32.eqz (call $pending_external (local.get $pending) (local.get $start)))
        (then
          block $wide
            loop $widen
              (br_if $wide (i32.ge_u (local.get $end) (call $vec_count (local.get $pending))))
              (br_if $wide (call $pending_external (local.get $pending) (local.get $end)))
              (local.set $end (i32.add (local.get $end) (i32.const 1)))
              br $widen
            end
          end))
      (call $batch_group (local.get $site) (local.get $pending) (local.get $start) (local.get $end) (local.get $values))
      (if (global.get $halt) (then (return)))
      (local.set $start (local.get $end))
      br $group
    end
  end)
(func $batch_request (param $seq i64) (param $call i32) (param $admitted i32) (param $room i32) (result i32) (local $node i32) (local $at i32)
  (local.set $node (call $json_object (i32.add (i32.add (i32.const 5) (i32.ne (local.get $admitted) (i32.const 0))) (i32.ne (local.get $room) (i32.const 0)))))
  (if (local.get $room) (then (call $json_object_set (local.get $node) (i32.add (i32.const 5) (i32.ne (local.get $admitted) (i32.const 0))) (i32.const $e_job) (call $json_bool (i32.const 1)))))
  (if (local.get $admitted)
    (then
      (call $json_object_set (local.get $node) (i32.const 0) (i32.const $s_admitted) (call $json_string (local.get $admitted)))
      (local.set $at (i32.const 1))))
  (call $json_object_set (local.get $node) (local.get $at) (i32.const $s_effect) (call $json_string (call $effect_name (call $call_effect (local.get $call)))))
  (call $json_object_set (local.get $node) (i32.add (local.get $at) (i32.const 1)) (i32.const $s_input) (call $call_input (local.get $call)))
  (call $json_object_set (local.get $node) (i32.add (local.get $at) (i32.const 2)) (i32.const $s_seq) (call $json_number (local.get $seq)))
  (call $json_object_set (local.get $node) (i32.add (local.get $at) (i32.const 3)) (i32.const $s_tool) (call $json_string (call $call_tool (local.get $call))))
  (call $json_object_set (local.get $node) (i32.add (local.get $at) (i32.const 4)) (i32.const $s_turn) (call $json_number (global.get $turn)))
  (call $json_object_sort (local.get $node))
  (local.get $node))
(func $batch_group (param $site i32) (param $pending i32) (param $start i32) (param $end i32) (param $values i32) (local $interrupts i32) (local $interrupting i32) (local $preempted i32) (local $rooms i32) (local $before i32) (local $tool_name i32) (local $external i32) (local $in_step i32) (local $first i32) (local $attempt i64) (local $unknown i32) (local $count i32) (local $outputs i32) (local $sent i32) (local $position i32) (local $entry i32) (local $refused i32) (local $requests i32) (local $request i32) (local $answer i32) (local $completed i32) (local $item i32) (local $output i32) (local $call i32) (local $admitted i32)
  (local.set $count (i32.sub (local.get $end) (local.get $start)))
  (local.set $first (call $vec_at (local.get $pending) (local.get $start)))
  (local.set $external (call $pending_external (local.get $pending) (local.get $start)))
  (call $assert (i32.or (i32.eqz (local.get $external)) (i32.eq (local.get $count) (i32.const 1))) (i32.const $s_batch_external))
  (local.set $in_step (i32.and (local.get $external) (call $target_in_step)))
  (local.set $interrupts (call $alloc (i32.shl (i32.add (local.get $count) (i32.const 1)) (i32.const 2))))
  (local.set $preempted (call $alloc (i32.shl (i32.add (local.get $count) (i32.const 1)) (i32.const 2))))
  (local.set $rooms (call $alloc (i32.shl (i32.add (local.get $count) (i32.const 1)) (i32.const 2))))
  block $masked
    loop $mask
      (br_if $masked (i32.ge_u (local.get $position) (local.get $count)))
      (i32.store (i32.add (local.get $interrupts) (i32.shl (local.get $position) (i32.const 2)))
        (call $tool_interrupts (call $call_tool (i32.load offset=12 (call $vec_at (local.get $pending) (i32.add (local.get $start) (local.get $position)))))))
      (local.set $interrupting (i32.or (local.get $interrupting) (i32.load (i32.add (local.get $interrupts) (i32.shl (local.get $position) (i32.const 2))))))
      (local.set $position (i32.add (local.get $position) (i32.const 1)))
      br $mask
    end
  end
  (local.set $position (i32.const 0))
  (if (call $flushes_to_interrupt (local.get $interrupting)) (then (call $flush)))
  (if (i32.or (local.get $external) (call $target_gate))
    (then
      (call $flush)
      (call $assert (i32.eqz (call $vec_count (global.get $buffer))) (i32.const $s_batch_durable))))
  (if (local.get $in_step)
    (then
      (local.set $attempt (call $host_attempt (global.get $turn) (i64.load (local.get $first))))
      (if (i32.and (i32.load offset=16 (local.get $first)) (i64.gt_u (local.get $attempt) (i64.const 1)))
        (then
          (if (i32.eq (call $retried (local.get $site) (i64.load (local.get $first)) (i32.load offset=12 (local.get $first))) (global.get $RETRIED_SETTLED))
            (then
              (i64.store (i32.add (local.get $values) (i32.shl (i32.load offset=8 (local.get $first)) (i32.const 3))) (global.get $retried_value))
              (return)))))
      (call $assert (i32.or (i32.load offset=16 (local.get $first)) (i64.eq (local.get $attempt) (i64.const 1))) (i32.const $s_first_attempt))))
  (local.set $outputs (call $alloc (i32.shl (i32.add (local.get $count) (i32.const 1)) (i32.const 2))))
  (local.set $admitted (call $alloc (i32.shl (i32.add (local.get $count) (i32.const 1)) (i32.const 2))))
  (local.set $sent (call $vec_new (i32.const 4)))
  (local.set $admitted (call $alloc (i32.shl (i32.add (local.get $count) (i32.const 1)) (i32.const 2))))
  block $gated
    loop $gate
      (br_if $gated (i32.ge_u (local.get $position) (local.get $count)))
      (local.set $entry (call $vec_at (local.get $pending) (i32.add (local.get $start) (local.get $position))))
      (i32.store (i32.add (local.get $preempted) (i32.shl (local.get $position) (i32.const 2)))
        (call $preempted (i32.load (i32.add (local.get $interrupts) (i32.shl (local.get $position) (i32.const 2))))))
      (i32.store (i32.add (local.get $rooms) (i32.shl (local.get $position) (i32.const 2))) (call $job_room (call $call_tool (i32.load offset=12 (local.get $entry)))))
      (local.set $refused (call $refusal (local.get $site) (i32.load offset=12 (local.get $entry))))
      (if (i32.load (i32.add (local.get $preempted) (i32.shl (local.get $position) (i32.const 2)))) (then (local.set $refused (i32.const $j_interrupted))))
      (i32.store (i32.add (local.get $admitted) (i32.shl (local.get $position) (i32.const 2))) (global.get $gate_admitted))
      (if (local.get $refused)
        (then (i32.store (i32.add (local.get $outputs) (i32.shl (local.get $position) (i32.const 2))) (call $output_new (global.get $OUTPUT_FAILED) (local.get $refused))))
        (else
          (i32.store (call $vec_push (local.get $sent)) (local.get $position))
          (i32.store (i32.add (local.get $admitted) (i32.shl (local.get $position) (i32.const 2))) (global.get $gate_admitted))))
      (local.set $position (i32.add (local.get $position) (i32.const 1)))
      br $gate
    end
  end
  (if (call $vec_count (local.get $sent))
    (then
      (local.set $requests (call $json_array (call $vec_count (local.get $sent))))
      (local.set $position (i32.const 0))
      block $built
        loop $build
          (br_if $built (i32.ge_u (local.get $position) (call $vec_count (local.get $sent))))
          (local.set $entry (call $vec_at (local.get $pending) (i32.add (local.get $start) (i32.load (call $vec_at (local.get $sent) (local.get $position))))))
          (call $json_array_set (local.get $requests) (local.get $position) (call $batch_request (i64.load (local.get $entry)) (i32.load offset=12 (local.get $entry))
            (i32.load (i32.add (local.get $admitted) (i32.shl (i32.load (call $vec_at (local.get $sent) (local.get $position))) (i32.const 2))))
            (i32.load (i32.add (local.get $rooms) (i32.shl (i32.load (call $vec_at (local.get $sent) (local.get $position))) (i32.const 2))))))
          (local.set $position (i32.add (local.get $position) (i32.const 1)))
          br $build
        end
      end
      (local.set $request (call $json_object (i32.const 3)))
      (call $json_object_set (local.get $request) (i32.const 0) (i32.const $s_p_event) (call $json_string (i32.const $s_batch)))
      (call $json_object_set (local.get $request) (i32.const 1) (i32.const $s_p_limit)
        (select (call $json_number (i64.extend_i32_u (call $target_limit))) (i32.const $json_null) (call $target_limited)))
      (call $json_object_set (local.get $request) (i32.const 2) (i32.const $s_requests) (local.get $requests))
      (local.set $answer (call $world (local.get $request)))
      (local.set $completed (call $json_get (local.get $answer) (i32.const $s_completed)))
      (call $assert (i32.ne (local.get $completed) (i32.const 0)) (i32.const $s_batch_answer))
      (call $assert (i32.eq (call $json_count (local.get $completed)) (call $vec_count (local.get $sent))) (i32.const $s_batch_answer))
      (local.set $position (i32.const 0))
      block $answered
        loop $answer_each
          (br_if $answered (i32.ge_u (local.get $position) (call $json_count (local.get $completed))))
          (local.set $item (call $json_item (local.get $completed) (local.get $position)))
          (local.set $refused (i32.wrap_i64 (call $json_number_value (call $json_item (local.get $item) (i32.const 0)))))
          (call $assert (i32.lt_u (local.get $refused) (call $vec_count (local.get $sent))) (i32.const $s_batch_answer))
          (local.set $output (call $output_from_json (call $json_item (local.get $item) (i32.const 1))))
          (call $assert (i32.ne (local.get $output) (i32.const 0)) (i32.const $s_not_output))
          (local.set $refused (i32.load (call $vec_at (local.get $sent) (local.get $refused))))
          (call $assert (i32.eqz (i32.load (i32.add (local.get $outputs) (i32.shl (local.get $refused) (i32.const 2))))) (i32.const $s_batch_answer))
          (i32.store (i32.add (local.get $outputs) (i32.shl (local.get $refused) (i32.const 2))) (local.get $output))
          (local.set $position (i32.add (local.get $position) (i32.const 1)))
          br $answer_each
        end
      end))
  (local.set $position (i32.const 0))
  block $checked_all
    loop $check
      (br_if $checked_all (i32.ge_u (local.get $position) (local.get $count)))
      (local.set $entry (call $vec_at (local.get $pending) (i32.add (local.get $start) (local.get $position))))
      (local.set $call (i32.load offset=12 (local.get $entry)))
      (i32.store (i32.add (local.get $outputs) (i32.shl (local.get $position) (i32.const 2)))
        (call $spawn_output (i64.load (local.get $entry)) (local.get $call)
          (call $job_checked (call $call_tool (local.get $call)) (i64.load (local.get $entry)) (i32.load (i32.add (local.get $rooms) (i32.shl (local.get $position) (i32.const 2))))
            (call $batch_returns (local.get $site) (local.get $call)) (i32.load (i32.add (local.get $outputs) (i32.shl (local.get $position) (i32.const 2)))))))
      (local.set $position (i32.add (local.get $position) (i32.const 1)))
      br $check
    end
  end
  (local.set $before (call $events_now))
  (if (call $seq_open (global.get $seq)) (then (call $drain_at (global.get $DRAIN_AFTER_RUN))))
  (local.set $position (i32.const 0))
  block $recorded
    loop $record
      (br_if $recorded (i32.ge_u (local.get $position) (local.get $count)))
      (local.set $entry (call $vec_at (local.get $pending) (i32.add (local.get $start) (local.get $position))))
      (local.set $call (i32.load offset=12 (local.get $entry)))
      (local.set $output (i32.load (i32.add (local.get $outputs) (i32.shl (local.get $position) (i32.const 2)))))
      (block $settled
        (if (call $interrupted_by (local.get $before) (i32.load (i32.add (local.get $interrupts) (i32.shl (local.get $position) (i32.const 2))))
              (i32.load (i32.add (local.get $preempted) (i32.shl (local.get $position) (i32.const 2)))))
          (then
            (call $jobs_remove (global.get $jobs) (call $job_key (global.get $turn) (i64.load (local.get $entry))))
            (if (i32.eq (call $call_effect (local.get $call)) (global.get $EFFECT_EXTERNAL))
              (then
                (i64.store (i32.add (local.get $values) (i32.shl (i32.load offset=8 (local.get $entry)) (i32.const 3))) (call $unknown_site (i64.load (local.get $entry)) (local.get $call)))
                (br $settled)))
            (local.set $output (call $interrupted_output))))
      (call $append (call $record_result (global.get $turn) (i64.load (local.get $entry)) (local.get $output)))
      (call $site_put (global.get $log) (i64.load (local.get $entry)) (global.get $SITE_DONE) (local.get $call) (local.get $output))
      (i64.store (i32.add (local.get $values) (i32.shl (i32.load offset=8 (local.get $entry)) (i32.const 3))) (call $settle (local.get $site) (local.get $call) (local.get $output))))
      (if (global.get $halt) (then (return)))
      (local.set $position (i32.add (local.get $position) (i32.const 1)))
      br $record
    end
  end
  (if (local.get $in_step) (then (call $flush))))
(func $batch_returns (param $site i32) (param $call i32) (result i32)
  (if (call $dispatched_sum (call $site_result (local.get $site))) (then (return (call $site_returns (local.get $site) (local.get $call)))))
  (call $dispatch_returns (local.get $site)))
