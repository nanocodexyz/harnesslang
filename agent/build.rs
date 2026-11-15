use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use harnesslang::ir::Type;
use harnesslang::target::Target;

const PROGRAM_DEFAULT: &str = "../examples/claude_code.hl";
const CHILD_PROGRAM_DEFAULT: &str = "../examples/claude_code_child.hl";
const PLANT_NONE: &str = "none";
const READ_TOOL: &str = "Read";
const CODEX_PROGRAM_DEFAULT: &str = "../examples/codex_agent.hl";
const MPP_PROGRAM_DEFAULT: &str = "../examples/mpp_agent.hl";
const CLIMB_PROGRAM_DEFAULT: &str = "../examples/autoresearch.hl";
const TEXT_PLANT_SEPARATOR: &str = " ==> ";

fn main() {
    let program = std::env::var("HL_AGENT_PROGRAM").unwrap_or_else(|_| PROGRAM_DEFAULT.to_owned());
    let child = std::env::var("HL_AGENT_CHILD_PROGRAM")
        .unwrap_or_else(|_| CHILD_PROGRAM_DEFAULT.to_owned());
    let paying =
        std::env::var("HL_AGENT_MPP_PROGRAM").unwrap_or_else(|_| MPP_PROGRAM_DEFAULT.to_owned());
    let climb =
        std::env::var("HL_CLIMB_PROGRAM").unwrap_or_else(|_| CLIMB_PROGRAM_DEFAULT.to_owned());
    let codex = std::env::var("HL_AGENT_CODEX_PROGRAM")
        .unwrap_or_else(|_| CODEX_PROGRAM_DEFAULT.to_owned());
    let requested = std::env::var("HL_AGENT_PLANT")
        .ok()
        .filter(|plant| !plant.is_empty());
    let text_plant = requested
        .as_deref()
        .and_then(|plant| plant.split_once(TEXT_PLANT_SEPARATOR));
    let plant = match (&requested, text_plant) {
        (Some(name), None) => name.clone(),
        _ => PLANT_NONE.to_owned(),
    };
    println!("cargo::rerun-if-env-changed=HL_AGENT_PROGRAM");
    println!("cargo::rerun-if-env-changed=HL_AGENT_CHILD_PROGRAM");
    println!("cargo::rerun-if-env-changed=HL_AGENT_MPP_PROGRAM");
    println!("cargo::rerun-if-env-changed=HL_CLIMB_PROGRAM");
    println!("cargo::rerun-if-env-changed=HL_AGENT_CODEX_PROGRAM");
    println!("cargo::rerun-if-env-changed=HL_AGENT_PLANT");
    println!("cargo::rerun-if-changed={program}");
    println!("cargo::rerun-if-changed={child}");
    println!("cargo::rerun-if-changed={paying}");
    println!("cargo::rerun-if-changed={climb}");
    println!("cargo::rerun-if-changed={codex}");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let mut constants = String::new();
    emit(&program, "harness", &plant, &out, &mut constants);
    emit(&child, "child_harness", &plant, &out, &mut constants);
    std::fs::write(out.join("constants.rs"), constants).expect("write the harness constants");
    emit_source(&paying, "mpp_harness", text_plant, &out.join("mpp_mod.rs"));
    emit_source(
        &codex,
        "codex_harness",
        text_plant,
        &out.join("codex_mod.rs"),
    );
    let climb_mod = out.join("climb_mod.rs");
    if Path::new(&climb).is_file() {
        emit_source(&climb, "climb_harness", None, &climb_mod);
    } else {
        let missing = format!(
            "compile_error!(\"hl-climb hosts {climb}, which does not exist: write it or set HL_CLIMB_PROGRAM\");\n"
        );
        std::fs::write(&climb_mod, missing).expect("write the missing climb harness");
    }
}

fn emit_source(path: &str, module: &str, plant: Option<(&str, &str)>, out: &Path) {
    let program = harnesslang::load::load(path).unwrap_or_else(|error| panic!("{error}"));
    let mut emitted = harnesslang::emit::emit(&program, Target::NativeJsonl)
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    if let Some((from, to)) = plant {
        assert_eq!(
            emitted.matches(from).count(),
            1,
            "the plant's text must appear exactly once in the emitted program: {from}"
        );
        emitted = emitted.replacen(from, to, 1);
    }
    let (attributes, body) = emitted
        .split_once('\n')
        .expect("the emitted program starts with its attributes line");
    let attributes = attributes
        .strip_prefix("#!")
        .expect("the emitted program starts with an inner attribute");
    let wrapped = format!("#{attributes}\npub mod {module} {{\n{body}\n}}\n");
    std::fs::write(out, wrapped).expect("write the wrapped harness");
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
        "pub mod {name} {{\n{declared}    pub const READ_BYTES_MAX: Option<u64> = {read_bytes_max:?};\n    pub const PROGRAM: &str = {:?};\n    pub const MODULE: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{name}.wasm\"));\n}}",
        canonical.display().to_string()
    )
    .expect("writing to a string succeeds");
}
