pub mod assemble;
pub mod data;
pub mod encode;
pub mod lower;
pub mod shake;

use crate::ir::Program;
use crate::target::Target;

pub const RUNTIME: [&str; 22] = [
    include_str!("runtime/base.wat"),
    include_str!("runtime/text.wat"),
    include_str!("runtime/json.wat"),
    include_str!("runtime/value.wat"),
    include_str!("runtime/journal.wat"),
    include_str!("runtime/engine.wat"),
    include_str!("runtime/blob.wat"),
    include_str!("runtime/listing.wat"),
    include_str!("runtime/model.wat"),
    include_str!("runtime/events.wat"),
    include_str!("runtime/sides.wat"),
    include_str!("runtime/idle.wat"),
    include_str!("runtime/parallel.wat"),
    include_str!("runtime/jobs.wat"),
    include_str!("runtime/fork.wat"),
    include_str!("runtime/cell_journal.wat"),
    include_str!("runtime/cell.wat"),
    include_str!("runtime/cell_runner.wat"),
    include_str!("runtime/cell_dispatch.wat"),
    include_str!("runtime/cell_drive.wat"),
    include_str!("runtime/run.wat"),
    include_str!("runtime/host.wat"),
];

pub const PLANTS: [(&str, &str, &str); 38] = [
    (
        "flush_after_dispatch",
        "(if (i32.or (local.get $required) (i32.and (call $target_gate)",
        "(if (i32.and (i32.const 0) (i32.and (call $target_gate)",
    ),
    (
        "crc_skipped",
        "(call $buf_u64 (local.get $bytes) (i64.extend_i32_u (call $crc32 (call $text_bytes (local.get $record)) (call $text_len (local.get $record)))))",
        "(call $buf_u64 (local.get $bytes) (i64.extend_i32_u (i32.xor (i32.const 1) (call $crc32 (call $text_bytes (local.get $record)) (call $text_len (local.get $record))))))",
    ),
    (
        "recovery_skipped",
        "(if (i32.and (i32.eq (i32.load offset=8 (local.get $site)) (global.get $SITE_OPEN))",
        "(if (i32.and (i32.eq (i32.const 0) (i32.const 1))",
    ),
    (
        "recovery_unreported",
        "\n    (then (call $append (call $record_recovery_attested",
        "\n    (then (drop (call $record_recovery_attested",
    ),
    (
        "mode_dropped",
        "(if (call $site_mode (local.get $site))",
        "(if (i32.const 0)",
    ),
    (
        "settings_dropped",
        "(if (i32.ne (local.get $settings) (i32.const 0))",
        "(if (i32.const 0)",
    ),
    (
        "usage_unchecked",
        "(local.set $error (call $usage_over (local.get $response)))",
        "(local.set $error (i32.const 0))",
    ),
    (
        "model_failure_decoded",
        "(if (call $text_eq (call $json_key (local.get $response) (i32.const 0)) (i32.const $s_failed))",
        "(if (i32.const 0)",
    ),
    (
        "interrupted_undecoded",
        "(then (if (i32.eqz (call $json_count (local.get $payload))) (then (br $shape)))))))",
        "(then (if (i32.const 0) (then (br $shape)))))))",
    ),
    (
        "blob_unjournaled",
        "(if (i32.eqz (local.get $fresh)) (then (return",
        "(if (i32.const 1) (then (return",
    ),
    (
        "blobs_unevicted",
        "(if (i64.eqz (local.get $dropped)) (then (return (local.get $list))))",
        "(if (i32.const 1) (then (return (local.get $list))))",
    ),
    (
        "blob_bound_unchecked",
        "(if (i32.gt_u (call $text_len (local.get $decoded)) (i32.load offset=4 (local.get $more)))",
        "(if (i32.const 0)",
    ),
    (
        "listing_unchecked",
        "(if (i32.eqz (local.get $fingerprint))",
        "(if (i32.const 0)",
    ),
    (
        "discovery_unchecked",
        "(if (i32.and (i32.load offset=28 (local.get $listing)) (i32.eqz (call $discovered_has",
        "(if (i32.and (i32.const 0) (i32.eqz (call $discovered_has",
    ),
    (
        "search_unchecked",
        "(if (local.get $listing) (then (local.set $output (call $checked_search",
        "(if (i32.const 0) (then (local.set $output (call $checked_search",
    ),
    (
        "listing_bound_unchecked",
        "(if (i32.gt_u (local.get $count) (i32.load offset=20 (local.get $listing)))",
        "(if (i32.const 0)",
    ),
    (
        "rewrite_kept_lost",
        "(call $list_push (local.get $list) (local.get $item) (local.get $capacity) (local.get $name)))",
        "(local.get $list))",
    ),
    (
        "batch_flush_skipped",
        "(if (i32.or (local.get $external) (call $target_gate))",
        "(if (i32.and (i32.const 0) (call $target_gate))",
    ),
    (
        "fork_hook_skipped",
        "(drop (call $forked_run (local.get $args)))",
        "(drop (i64.const 0))",
    ),
    (
        "event_ack_early",
        "(if (local.get $recorded) (then (call $write_events)))",
        "(if (i32.const 0) (then (call $write_events)))",
    ),
    (
        "event_redelivered",
        "(if (call $events_recorded (global.get $log) (call $delivery_id (local.get $delivery))) (then (return (global.get $ANSWER_DUPLICATE))))",
        "(if (i32.const 0) (then (return (global.get $ANSWER_DUPLICATE))))",
    ),
    (
        "cancel_read_unstamped",
        "(global.get $DELIVERY_CANCEL) (i32.const 1))",
        "(global.get $DELIVERY_CANCEL) (i32.const 0))",
    ),
    (
        "suspend_before_flush",
        "(call $flush)\n  (call $assert (i32.eqz (call $vec_count (global.get $buffer))) (i32.const $e_suspend_durable))",
        "(call $assert (i32.const 1) (i32.const $e_suspend_durable))",
    ),
    (
        "resume_unmatched",
        "(local.set $matched (call $text_eq (call $field_text (local.get $resume) (i32.const $e_key)) (local.get $key)))",
        "(local.set $matched (i32.const 1))",
    ),
    (
        "wait_unjournaled",
        "(call $append (call $record_result (global.get $turn) (local.get $seq) (local.get $output)))\n  (call $site_put (global.get $log) (local.get $seq) (global.get $SITE_DONE) (local.get $call) (local.get $output))\n  (call $settle (local.get $site) (local.get $call) (local.get $output)))\n(func $suspend_site",
        "(call $settle (local.get $site) (local.get $call) (local.get $output)))\n(func $suspend_site",
    ),
    (
        "side_live_state",
        "(call $side_run (local.get $asked) (global.get $snapshot))",
        "(call $side_run (local.get $asked) (global.get $state))",
    ),
    (
        "batch_drain_skipped",
        "(if (call $seq_open (global.get $seq)) (then (call $drain_at (global.get $DRAIN_AFTER_RUN))))",
        "(if (i32.const 0) (then (call $drain_at (global.get $DRAIN_AFTER_RUN))))",
    ),
    (
        "grandchild_unscoped",
        "(call $json_string (call $spawn_key (global.get $scope) (global.get $turn) (local.get $seq)))",
        "(call $json_string (call $spawn_key (i32.const 0) (global.get $turn) (local.get $seq)))",
    ),
    (
        "idle_early",
        "(if (i64.lt_u (local.get $clock) (local.get $due)) (then (return (call $exit_not_due))))",
        "(if (i32.const 0) (then (return (call $exit_not_due))))",
    ),
    (
        "interrupt_ignored",
        "(i32.and (call $interrupting_since (local.get $before) (local.get $interrupts)) (i32.eqz (local.get $preempted))))",
        "(i32.and (i32.const 0) (i32.eqz (local.get $preempted))))",
    ),
    (
        "recover_reply_ignored",
        "(global.set $recovered_output (local.get $output))\n  (global.get $RECOVERED_REPLY))",
        "(global.set $recovered_output (local.get $output))\n  (global.get $RECOVERED_UNKNOWN))",
    ),
    (
        "job_unopened",
        "(if (call $job_started (i32.load offset=16 (local.get $site)))\n    (then (call $jobs_put (global.get $session_jobs)",
        "(if (i32.const 0)\n    (then (call $jobs_put (global.get $session_jobs)",
    ),
    (
        "job_lost_dropped",
        "(then (i32.store (call $vec_push (local.get $lost)) (call $lost_delivery (local.get $key)))))",
        "(then (drop (call $lost_delivery (local.get $key)))))",
    ),
    (
        "cell_leaks",
        "(if (i32.eqz (call $cells_yield)) (then (return)))",
        "(if (i32.const 1) (then (return)))",
    ),
    (
        "cell_cancel_ignored",
        "(if (i32.eqz (i32.and (i32.load offset=60 (global.get $c_code)) (global.get $INTERRUPTS_CANCEL))) (then (return (i32.const 0))))",
        "(if (i32.const 1) (then (return (i32.const 0))))",
    ),
    (
        "nested_recovery_skipped",
        "(if (call $target_in_step) (then (return (local.get $pairs))))",
        "(if (i32.const 1) (then (return (local.get $pairs))))",
    ),
    ("fold_arms_flipped", "", ""),
    (
        "nested_result_unjournaled",
        "(call $append (call $record_nested_result (local.get $call) (local.get $output)))",
        "(drop (local.get $output))",
    ),
];

const BYTE_VALUES: usize = 256;
const TABLE_WORD_BYTES: usize = 4;
const JSON_CONTROL_END: u8 = 0x20;

fn json_special(byte: u8) -> bool {
    byte < JSON_CONTROL_END || byte == b'"' || byte == b'\\'
}

fn json_special_table() -> String {
    let mut words = Vec::with_capacity(BYTE_VALUES / TABLE_WORD_BYTES);
    for first in (0..BYTE_VALUES).step_by(TABLE_WORD_BYTES) {
        let mut word = [0_u8; TABLE_WORD_BYTES];
        for (lane, byte) in word.iter_mut().enumerate() {
            let value = u8::try_from(first + lane).expect("a table index fits a byte");
            *byte = u8::from(json_special(value));
        }
        words.push(u32::from_le_bytes(word).to_string());
    }
    format!("(words $json_special {})\n", words.join(" "))
}

fn base64_table() -> String {
    let mut table = [0_u8; BYTE_VALUES];
    for (sextet, byte) in crate::run::blob::BASE64_ALPHABET.iter().enumerate() {
        table[usize::from(*byte)] = u8::try_from(sextet + 1).expect("a sextet fits a byte");
    }
    format!("(text $base64_table {})\n", data::wat_string(&table))
}

fn crc_table() -> String {
    let words: Vec<String> = crate::run::crc::TABLE
        .iter()
        .map(|word| word.to_string())
        .collect();
    format!("(words $crc_table {})\n", words.join(" "))
}

pub fn emit_text(program: &Program, target: Target) -> Result<String, String> {
    emit_text_planted(program, target, "none")
}

pub fn emit_text_planted(program: &Program, target: Target, plant: &str) -> Result<String, String> {
    let mut text = RUNTIME.concat();
    text.push_str(&crc_table());
    text.push_str(&base64_table());
    text.push_str(&json_special_table());
    if plant != "none" {
        let Some((_, from, to)) = PLANTS.iter().find(|(name, _, _)| *name == plant) else {
            return Err(format!("unknown wasm plant {plant}"));
        };
        if !text.contains(from) {
            return Err(format!("wasm plant {plant} matches nothing in the runtime"));
        }
        text = text.replacen(from, to, 1);
    }
    text.push_str(&lower::program_text(program, target)?);
    Ok(text)
}

pub fn module_js(bytes: &[u8]) -> String {
    format!(
        "export default \"{}\";\n",
        crate::run::blob::base64_encode(bytes)
    )
}

pub fn emit(program: &Program, target: Target) -> Result<Vec<u8>, String> {
    emit_planted(program, target, "none")
}

pub fn emit_planted(program: &Program, target: Target, plant: &str) -> Result<Vec<u8>, String> {
    let text = emit_text_planted(program, target, plant)?;
    let flips = PLANTS
        .iter()
        .any(|(name, from, _)| *name == plant && from.is_empty());
    let arms = if flips {
        shake::Arms::Flipped
    } else {
        shake::Arms::Taken
    };
    let module = assemble::assemble_folded(&text, arms)
        .unwrap_or_else(|error| panic!("the runtime and lowered program assemble: {error}"));
    Ok(module.encode())
}
