pub mod js;
pub mod rust;
pub mod sql;

use crate::ir::Program;
use crate::target::Target;

pub fn emit(program: &Program, target: Target) -> Result<String, String> {
    if matches!(target, Target::Cloudflare | Target::Vercel) {
        return js::emit(program, target);
    }
    if let Some(code) = program.codes.first() {
        return Err(format!(
            "emit: not yet: code tool '{}' of harness '{}' needs cell support in the {} backend",
            code.name,
            program.name,
            target.name()
        ));
    }
    if rust::TARGETS.contains(&target) {
        return rust::emit(program, target);
    }
    Err(format!(
        "emit: not yet: code generation for {} of harness '{}' is not built",
        target.name(),
        program.name
    ))
}
