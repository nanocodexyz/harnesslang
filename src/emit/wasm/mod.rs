pub mod assemble;
pub mod data;
pub mod encode;
pub mod lower;

use crate::ir::Program;
use crate::target::Target;

pub const RUNTIME: [&str; 6] = [
    include_str!("runtime/base.wat"),
    include_str!("runtime/json.wat"),
    include_str!("runtime/value.wat"),
    include_str!("runtime/journal.wat"),
    include_str!("runtime/engine.wat"),
    include_str!("runtime/run.wat"),
];

pub fn emit_text(program: &Program, target: Target) -> Result<String, String> {
    let mut text = RUNTIME.concat();
    text.push_str(&lower::program_text(program, target)?);
    Ok(text)
}

pub fn emit(program: &Program, target: Target) -> Result<Vec<u8>, String> {
    let text = emit_text(program, target)?;
    let module = assemble::assemble(&text).unwrap_or_else(|error| panic!("the runtime and lowered program assemble: {error}"));
    Ok(module.encode())
}
