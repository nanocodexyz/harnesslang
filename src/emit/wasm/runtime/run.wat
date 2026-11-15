(global $LINE_PREFIX_BYTES i32 (i32.const 7))
(text $s_handler "handler")
(text $s_arguments "arguments")
(func $line_record (param $bytes i32) (param $length i32) (result i32) (local $at i32) (local $digits_end i32) (local $crc i64) (local $body i32) (local $body_length i32) (local $middle i32) (local $node i32)
  (if (i32.lt_u (local.get $length) (call $text_len (i32.const $s_line_prefix))) (then (return (i32.const 0))))
  (if (i32.eqz (call $bytes_equal (local.get $bytes) (call $text_bytes (i32.const $s_line_prefix)) (call $text_len (i32.const $s_line_prefix))))
    (then (return (i32.const 0))))
  (local.set $middle (call $text_len (i32.const $s_line_middle)))
  (local.set $at (call $text_len (i32.const $s_line_prefix)))
  block $found
    loop $next
      (if (i32.gt_u (i32.add (local.get $at) (local.get $middle)) (local.get $length)) (then (return (i32.const 0))))
      (br_if $found (call $bytes_equal (i32.add (local.get $bytes) (local.get $at)) (call $text_bytes (i32.const $s_line_middle)) (local.get $middle)))
      (local.set $at (i32.add (local.get $at) (i32.const 1)))
      br $next
    end
  end
  (global.set $record_failed (i32.const 0))
  (local.set $crc (call $parse_u64_text (call $text_from (i32.add (local.get $bytes) (call $text_len (i32.const $s_line_prefix)))
    (i32.sub (local.get $at) (call $text_len (i32.const $s_line_prefix))))))
  (if (i32.or (global.get $record_failed) (i64.gt_u (local.get $crc) (global.get $U32_DECIMAL_MAX))) (then (return (i32.const 0))))
  (local.set $body (i32.add (local.get $bytes) (i32.add (local.get $at) (local.get $middle))))
  (local.set $body_length (i32.sub (local.get $length) (i32.add (local.get $at) (local.get $middle))))
  (if (i32.eqz (local.get $body_length)) (then (return (i32.const 0))))
  (if (i32.ne (i32.load8_u (i32.add (local.get $body) (i32.sub (local.get $body_length) (i32.const 1)))) (i32.const 125)) (then (return (i32.const 0))))
  (local.set $body_length (i32.sub (local.get $body_length) (i32.const 1)))
  (if (i64.ne (i64.extend_i32_u (call $crc32 (local.get $body) (local.get $body_length))) (local.get $crc)) (then (return (i32.const 0))))
  (call $row_record (local.get $body) (local.get $body_length)))
(func $row_record (param $bytes i32) (param $length i32) (result i32) (local $node i32)
  (local.set $node (call $json_parse_bytes (local.get $bytes) (local.get $length)))
  (if (global.get $parse_failed) (then (return (i32.const 0))))
  (call $record_from (local.get $node)))
(func $line_end (param $bytes i32) (param $from i32) (param $length i32) (result i32)
  block $found
    block $missing
      loop $next
        (br_if $missing (i32.ge_u (local.get $from) (local.get $length)))
        (br_if $found (i32.eq (i32.load8_u (i32.add (local.get $bytes) (local.get $from))) (global.get $LINE_END)))
        (local.set $from (i32.add (local.get $from) (i32.const 1)))
        br $next
      end
    end
    (return (i32.const -1))
  end
  (local.get $from))
(func $journal_records (param $bytes i32) (param $length i32) (result i32) (local $records i32) (local $offset i32) (local $end i32) (local $record i32)
  (local.set $records (call $vec_new (i32.const 4)))
  block $done
    loop $next
      (br_if $done (i32.ge_u (local.get $offset) (local.get $length)))
      (local.set $end (call $line_end (local.get $bytes) (local.get $offset) (local.get $length)))
      (if (i32.lt_s (local.get $end) (i32.const 0))
        (then
          (br_if $done (call $target_checksum))
          (call $corrupt (i32.const $s_row))
          (return (i32.const 0))))
      (if (call $target_checksum)
        (then (local.set $record (call $line_record (i32.add (local.get $bytes) (local.get $offset)) (i32.sub (local.get $end) (local.get $offset)))))
        (else (local.set $record (call $row_record (i32.add (local.get $bytes) (local.get $offset)) (i32.sub (local.get $end) (local.get $offset))))))
      (if (i32.eqz (local.get $record))
        (then
          (br_if $done (call $target_checksum))
          (call $corrupt (i32.const $s_row))
          (return (i32.const 0))))
      (i32.store (call $vec_push (local.get $records)) (local.get $record))
      (local.set $offset (i32.add (local.get $end) (i32.const 1)))
      br $next
    end
  end
  (if (i32.lt_u (local.get $offset) (local.get $length)) (then (call $host_keep (i64.extend_i32_u (local.get $offset)))))
  (local.get $records))
(func $fold (param $records i32) (result i32) (local $index i32)
  (call $session_reset)
  block $done
    loop $next
      (br_if $done (i32.ge_u (local.get $index) (call $vec_count (local.get $records))))
      (if (i32.eqz (call $apply (i32.load (call $vec_at (local.get $records) (local.get $index)))))
        (then (call $corrupt (global.get $fold_error)) (return (i32.const 0))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (i32.const 1))
(func $replay (param $turn i64) (param $log i32) (param $state i32) (result i32) (local $handler i32) (local $args i32) (local $finished i32)
  (local.set $handler (call $handler_named (i32.load (local.get $log))))
  (if (i32.eqz (local.get $handler)) (then (call $corrupt (i32.const $s_no_handler)) (return (i32.const 0))))
  (local.set $args (call $arguments_from_json (local.get $handler) (i32.load offset=4 (local.get $log))))
  (if (i32.eqz (local.get $args)) (then (call $corrupt (i32.const $s_arguments_lack)) (return (i32.const 0))))
  (local.set $finished (call $log_finished (local.get $log)))
  (if (i32.eqz (local.get $finished)) (then (call $corrupt (i32.const $s_not_finished)) (return (i32.const 0))))
  (if (i32.eqz (call $check_events (local.get $log))) (then (return (i32.const 0))))
  (call $engine_start (i32.const 0) (local.get $turn) (local.get $log) (local.get $state))
  (if (i32.eqz (call $engine_run (local.get $handler) (local.get $args))) (then (return (i32.const 0))))
  (if (i32.or (i32.ne (global.get $finish_outcome) (i32.load (local.get $finished)))
              (i32.eqz (call $json_equal (global.get $finish_result) (i32.load offset=4 (local.get $finished)))))
    (then (call $corrupt (i32.const $s_replay_differs)) (return (i32.const 0))))
  (i32.const 1))
(func $state_before (param $turn i64) (result i32) (local $index i32) (local $checkpoint i32) (local $best i32) (local $state i32) (local $earlier i64) (local $log i32) (local $node i32)
  block $scanned
    loop $next
      (br_if $scanned (i32.ge_u (local.get $index) (call $vec_count (global.get $checkpoints))))
      (local.set $checkpoint (call $vec_at (global.get $checkpoints) (local.get $index)))
      (if (i64.lt_u (i64.load (local.get $checkpoint)) (local.get $turn))
        (then
          (if (i32.eqz (local.get $best))
            (then (local.set $best (local.get $checkpoint)))
            (else
              (if (i64.ge_u (i64.load (local.get $checkpoint)) (i64.load (local.get $best))) (then (local.set $best (local.get $checkpoint))))))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (if (local.get $best)
    (then
      (local.set $node (call $json_parse (i32.load offset=8 (local.get $best))))
      (if (global.get $parse_failed) (then (call $corrupt (i32.const $s_checkpoint_json)) (return (i32.const 0))))
      (local.set $state (call $state_from_json (local.get $node)))
      (if (i32.eqz (local.get $state)) (then (call $corrupt (i32.const $s_state_shape)) (return (i32.const 0))))
      (local.set $earlier (i64.add (i64.load (local.get $best)) (i64.const 1))))
    (else
      (local.set $state (call $state_new))
      (local.set $earlier (global.get $FIRST_TURN))))
  block $replayed
    loop $turns
      (br_if $replayed (i64.ge_u (local.get $earlier) (local.get $turn)))
      (local.set $log (call $session_log (local.get $earlier)))
      (if (i32.eqz (local.get $log)) (then (call $corrupt (i32.const $s_turn_missing)) (return (i32.const 0))))
      (if (i32.eqz (call $replay (local.get $earlier) (local.get $log) (local.get $state))) (then (return (i32.const 0))))
      (local.set $earlier (i64.add (local.get $earlier) (i64.const 1)))
      br $turns
    end
  end
  (local.get $state))
(func $exit_corrupt (result i64) (local $buffer i32)
  (local.set $buffer (call $buf_new))
  (call $buf_text (local.get $buffer) (i32.const $s_x_corrupt))
  (call $json_write_string (local.get $buffer) (global.get $halt_text))
  (call $buf_byte (local.get $buffer) (i32.const 125))
  (call $packed (call $buf_finish (local.get $buffer))))
(func $exit_finish (param $open i32) (result i64) (local $buffer i32)
  (local.set $buffer (call $buf_new))
  (call $buf_text (local.get $buffer) (local.get $open))
  (call $buf_text (local.get $buffer) (call $finish_text))
  (call $buf_byte (local.get $buffer) (i32.const 125))
  (call $packed (call $buf_finish (local.get $buffer))))
(func $recover_at_load (param $log i32) (result i32) (local $unknown i32) (local $sites i32) (local $index i32) (local $site i32)
  (local.set $unknown (call $vec_new (i32.const 8)))
  (if (call $target_in_step) (then (return (local.get $unknown))))
  (local.set $sites (call $log_sites (local.get $log)))
  block $done
    loop $next
      (br_if $done (i32.ge_u (local.get $index) (call $vec_count (local.get $sites))))
      (local.set $site (call $vec_at (local.get $sites) (local.get $index)))
      (if (i32.and (i32.eq (i32.load offset=8 (local.get $site)) (global.get $SITE_OPEN))
                   (i32.eq (call $call_effect (i32.load offset=12 (local.get $site))) (global.get $EFFECT_EXTERNAL)))
        (then (call $recover_open (local.get $site) (local.get $unknown))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (local.get $unknown))
(func $run (export "hl_run") (param $request_bytes i32) (param $request_length i32) (param $journal_bytes i32) (param $journal_length i32) (param $epoch i64) (result i64)
  (local $request i32) (local $turn i64) (local $name i32) (local $handler i32) (local $args i32) (local $arguments i32) (local $records i32) (local $log i32) (local $state i32) (local $pending i32) (local $unknown i32) (local $finished i32) (local $next i64) (local $ran i32) (local $due i64) (local $clock i64) (local $nested i32)
  (global.set $halt (i32.const 0))
  (global.set $epoch (local.get $epoch))
  (local.set $request (call $json_parse_bytes (local.get $request_bytes) (local.get $request_length)))
  (call $assert (i32.eqz (global.get $parse_failed)) (i32.const $s_request))
  (global.set $record_failed (i32.const 0))
  (local.set $turn (call $field_number (local.get $request) (i32.const $s_turn)))
  (local.set $name (call $field_text (local.get $request) (i32.const $s_handler)))
  (global.set $scope (i32.const 0))
  (if (call $json_get (local.get $request) (i32.const $s_scope))
    (then (global.set $scope (call $field_text (local.get $request) (i32.const $s_scope)))))
  (call $assert (i32.eqz (global.get $record_failed)) (i32.const $s_request))
  (local.set $handler (call $handler_named (local.get $name)))
  (call $assert (i32.ne (local.get $handler) (i32.const 0)) (i32.const $s_no_handler))
  (local.set $args (call $arguments_from_json (local.get $handler) (call $json_get (local.get $request) (i32.const $s_arguments))))
  (call $assert (i32.ne (local.get $args) (i32.const 0)) (i32.const $s_request))
  (local.set $arguments (call $arguments_json (local.get $handler) (local.get $args)))
  (local.set $records (call $journal_records (local.get $journal_bytes) (local.get $journal_length)))
  (if (global.get $halt) (then (return (call $exit_corrupt))))
  (if (i32.eqz (call $fold (local.get $records))) (then (return (call $exit_corrupt))))
  (local.set $log (call $session_log (local.get $turn)))
  (if (local.get $log)
    (then
      (local.set $finished (call $log_finished (local.get $log)))
      (if (local.get $finished)
        (then
          (global.set $turn (local.get $turn))
          (global.set $finish_outcome (i32.load (local.get $finished)))
          (global.set $finish_result (i32.load offset=4 (local.get $finished)))
          (call $close_inbox (local.get $turn) (i64.load offset=8 (local.get $finished)) (local.get $log))
          (return (call $exit_finish (i32.const $s_x_already)))))))
  (if (i32.and (call $idles (local.get $handler)) (i32.and (i32.eqz (local.get $log)) (call $idle_busy)))
    (then (return (call $exit_not_due))))
  (local.set $state (call $state_before (local.get $turn)))
  (if (i32.eqz (local.get $state)) (then (return (call $exit_corrupt))))
  (if (i32.eqz (call $blobs_load (local.get $state))) (then (return (call $exit_corrupt))))
  (if (local.get $log)
    (then
      (if (i32.or (i32.eqz (call $text_eq (i32.load (local.get $log)) (local.get $name)))
                  (i32.and (i32.eqz (call $idles (local.get $handler))) (i32.eqz (call $json_equal (i32.load offset=4 (local.get $log)) (local.get $arguments)))))
        (then (call $corrupt (i32.const $s_other_arguments)) (return (call $exit_corrupt))))
      (if (i32.eqz (call $check_events (local.get $log))) (then (return (call $exit_corrupt))))
      (global.set $turn (local.get $turn))
      (local.set $unknown (call $recover_at_load (local.get $log)))
      (local.set $nested (call $recover_nested_at_load (local.get $log))))
    (else
      (local.set $next (select (i64.add (call $session_last) (i64.const 1)) (global.get $FIRST_TURN) (call $vec_count (global.get $turns))))
      (if (i64.ne (local.get $turn) (local.get $next)) (then (call $corrupt (i32.const $s_not_next)) (return (call $exit_corrupt))))
      (if (call $idles (local.get $handler))
        (then
          (local.set $due (call $idle_armed (local.get $state)))
          (if (i64.eqz (local.get $due)) (then (return (call $exit_not_due))))
          (local.set $clock (call $world_clock))
          (if (i64.lt_u (local.get $clock) (local.get $due)) (then (return (call $exit_not_due))))
          (local.set $arguments (call $idle_arguments (local.get $clock) (local.get $due)))))
      (local.set $pending (call $record_admitted (local.get $turn) (local.get $name) (local.get $arguments)))
      (local.set $log (call $log_new (local.get $name) (i32.const $json_null)))
      (local.set $unknown (call $vec_new (i32.const 8)))
      (local.set $nested (call $vec_new (global.get $PAIR_BYTES)))))
  (global.set $snapshot (call $state_copy (local.get $state)))
  (call $engine_start (i32.const 1) (local.get $turn) (local.get $log) (local.get $state))
  (global.set $next_cell (call $next_cell_of_session))
  (if (local.get $pending) (then (call $append (local.get $pending))))
  (if (i32.or (i32.or (call $vec_count (local.get $unknown)) (call $vec_count (local.get $nested))) (i32.or (call $vec_count (global.get $recover_replied)) (call $vec_count (global.get $recover_undispatched))))
    (then (call $append (call $record_recovery_attested (local.get $turn) (call $next_attempt) (local.get $unknown) (local.get $nested) (global.get $recover_replied) (global.get $recover_undispatched)))))
  (call $start_jobs (local.get $turn) (local.get $log))
  (global.set $lost (call $lost_jobs (local.get $turn)))
  (local.set $ran (call $engine_run (local.get $handler) (local.get $args)))
  (if (i32.eqz (local.get $ran)) (then (return (call $exit_corrupt))))
  (if (i32.eq (local.get $ran) (i32.const 2)) (then (return (call $exit_suspended))))
  (call $exit_finish (i32.const $s_x_finished)))
