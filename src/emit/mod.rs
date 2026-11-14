pub mod rust;
pub mod sql;

use crate::ir::Program;
use crate::target::Target;

pub fn emit(program: &Program, target: Target) -> Result<String, String> {
    if rust::TARGETS.contains(&target) {
        return rust::emit(program, target);
    }
    Err(format!(
        "emit: not yet: code generation for {} of harness '{}' is not built",
        target.name(),
        program.name
    ))
}
