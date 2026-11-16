pub mod rust;
pub mod sql;
pub mod wasm;

use crate::ir::Program;
use crate::target::Target;

pub fn emit(program: &Program, target: Target) -> Result<String, String> {
    if rust::TARGETS.contains(&target) {
        return rust::emit(program, target);
    }
    Err(format!(
        "emit: {} compiles to wasm only; harness '{}' has no source form there",
        target.name(),
        program.name
    ))
}
