use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use harnesslang::ir::Type;
use harnesslang::target::Target;

const PROGRAM_DEFAULT: &str = "../examples/claude_code.hl";
const CHILD_PROGRAM_DEFAULT: &str = "../examples/claude_code_child.hl";
const CODEX_PROGRAM_DEFAULT: &str = "../examples/codex_agent.hl";
const PLANT_NONE: &str = "none";
const READ_TOOL: &str = "Read";

fn main() {
    let program = std::env::var("HL_AGENT_PROGRAM").unwrap_or_else(|_| PROGRAM_DEFAULT.to_owned());
    let child = std::env::var("HL_AGENT_CHILD_PROGRAM")
        .unwrap_or_else(|_| CHILD_PROGRAM_DEFAULT.to_owned());
    let codex = std::env::var("HL_AGENT_CODEX_PROGRAM")
        .unwrap_or_else(|_| CODEX_PROGRAM_DEFAULT.to_owned());
    let plant = std::env::var("HL_AGENT_PLANT")
        .ok()
        .filter(|plant| !plant.is_empty())
        .unwrap_or_else(|| PLANT_NONE.to_owned());
    println!("cargo::rerun-if-env-changed=HL_AGENT_PROGRAM");
    println!("cargo::rerun-if-env-changed=HL_AGENT_CHILD_PROGRAM");
    println!("cargo::rerun-if-env-changed=HL_AGENT_CODEX_PROGRAM");
    println!("cargo::rerun-if-env-changed=HL_AGENT_PLANT");
    println!("cargo::rerun-if-changed={program}");
    println!("cargo::rerun-if-changed={child}");
    println!("cargo::rerun-if-changed={codex}");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let mut constants = String::new();
    emit(&program, "harness", &plant, &out, &mut constants);
    emit(&child, "child_harness", &plant, &out, &mut constants);
    emit(&codex, "codex_harness", &plant, &out, &mut constants);
    std::fs::write(out.join("constants.rs"), constants).expect("write the harness constants");
}

fn emit(path: &str, name: &str, plant: &str, out: &Path, constants: &mut String) {
    let program = harnesslang::load::load(path).unwrap_or_else(|error| panic!("{error}"));
    let module = harnesslang::emit::wasm::emit_planted(&program, Target::NativeJsonl, plant)
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    std::fs::write(out.join(format!("{name}.wasm")), module).expect("write the harness module");
    let read_bytes_max = program
        .tools
        .iter()
        .find(|tool| tool.name == READ_TOOL)
        .and_then(|tool| match &tool.returns {
            Type::Blob(bytes) => Some(bytes.value),
            _ => tool.attaches.as_ref().map(|bytes| bytes.value),
        });
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let mut declared = String::new();
    for constant in &program.constants {
        writeln!(
            declared,
            "    pub const {}: u64 = {};",
            constant.name.to_uppercase(),
            constant.value
        )
        .expect("writing to a string succeeds");
    }
    writeln!(
        constants,
        "#[allow(dead_code)]\npub mod {name} {{\n{declared}    pub const READ_BYTES_MAX: Option<u64> = {read_bytes_max:?};\n    pub const PROGRAM: &str = {:?};\n    pub const MODULE: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{name}.wasm\"));\n}}",
        canonical.display().to_string()
    )
    .expect("writing to a string succeeds");
}
