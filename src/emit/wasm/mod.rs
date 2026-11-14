pub mod assemble;
pub mod data;
pub mod encode;
pub mod lower;

use crate::ir::Program;
use crate::target::Target;

pub const RUNTIME: [&str; 7] = [
    include_str!("runtime/base.wat"),
    include_str!("runtime/text.wat"),
    include_str!("runtime/json.wat"),
    include_str!("runtime/value.wat"),
    include_str!("runtime/journal.wat"),
    include_str!("runtime/engine.wat"),
    include_str!("runtime/run.wat"),
];

pub const PLANTS: [(&str, &str, &str); 3] = [
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
        "(if (i32.and (i32.const 0) (global.get $SITE_OPEN))",
    ),
];

pub fn emit_text(program: &Program, target: Target) -> Result<String, String> {
    emit_text_planted(program, target, "none")
}

pub fn emit_text_planted(program: &Program, target: Target, plant: &str) -> Result<String, String> {
    let mut text = RUNTIME.concat();
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

pub fn emit(program: &Program, target: Target) -> Result<Vec<u8>, String> {
    emit_planted(program, target, "none")
}

pub fn emit_planted(program: &Program, target: Target, plant: &str) -> Result<Vec<u8>, String> {
    let text = emit_text_planted(program, target, plant)?;
    let module = assemble::assemble(&text)
        .unwrap_or_else(|error| panic!("the runtime and lowered program assemble: {error}"));
    Ok(module.encode())
}
