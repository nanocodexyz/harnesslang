use std::fmt::Write;

use crate::ir::{self, Program};

use super::handlers::Body;
use super::types::Types;

pub fn emit<'p>(out: &mut String, program: &'p Program, types: &mut Types<'p>) {
    let mut arms = String::new();
    for tool in &program.tools {
        let Some(at) = tool.fork else { continue };
        let variant = match at {
            ir::ForkAt::Latest => "Latest",
            ir::ForkAt::Turn => "Turn",
        };
        writeln!(arms, "        {:?} => Some(ForkAt::{variant}),", tool.name)
            .expect("writing to a string succeeds");
    }
    writeln!(
        out,
        "pub const FORKS: bool = {};\n\npub fn fork_of(tool: &str) -> Option<ForkAt> {{\n    match tool {{\n{arms}        _ => None,\n    }}\n}}\n",
        program.forks()
    )
    .expect("writing to a string succeeds");
    let Some(hook) = &program.forked else {
        writeln!(
            out,
            "pub fn run_forked(state: &mut State, origin: String, discarded: u64) -> Result<(), Halt> {{\n    Ok(())\n}}\n"
        )
        .expect("writing to a string succeeds");
        return;
    };
    assert!(
        hook.sites.is_empty(),
        "the checker refuses every effect in on forked"
    );
    let mut body = Body::new(program, hook, types);
    let params: Vec<String> = hook
        .params
        .iter()
        .map(|local| {
            format!(
                ", {}: {}",
                body.local(*local),
                body.types.rust(&hook.locals[*local].ty)
            )
        })
        .collect();
    body.emit();
    writeln!(
        out,
        "fn forked_hook(state: &mut State{}) -> Result<bool, Halt> {{\n{}}}\n\npub fn run_forked(state: &mut State, origin: String, discarded: u64) -> Result<(), Halt> {{\n    forked_hook(state, origin, discarded)?;\n    Ok(())\n}}\n",
        params.concat(),
        body.out
    )
    .expect("writing to a string succeeds");
}
