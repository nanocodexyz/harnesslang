use std::path::{Path, PathBuf};

use harnesslang::target::Target;

const PROGRAMS: [&str; 3] = [
    "../../examples/claude_code.hl",
    "../../examples/claude_code_child.hl",
    "../../examples/codex_agent.hl",
];
const PLANT_NONE: &str = "none";
const PROFILE_DEPTH: usize = 3;

fn main() {
    let plant = std::env::var("HL_AGENT_PLANT")
        .ok()
        .filter(|plant| !plant.is_empty())
        .unwrap_or_else(|| PLANT_NONE.to_owned());
    println!("cargo::rerun-if-env-changed=HL_AGENT_PLANT");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let profile = out
        .ancestors()
        .nth(PROFILE_DEPTH)
        .expect("OUT_DIR is target/<profile>/build/<crate>/out")
        .to_path_buf();
    for path in PROGRAMS {
        println!("cargo::rerun-if-changed={path}");
        emit(path, &plant, &out, &profile);
    }
}

fn emit(path: &str, plant: &str, out: &Path, profile: &Path) {
    let program = harnesslang::load::load(path).unwrap_or_else(|error| panic!("{error}"));
    let module = harnesslang::emit::wasm::emit_planted(&program, Target::NativeJsonl, plant)
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    let stem = Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("a program path ends in <stem>.hl");
    let name = format!("{stem}.wasm");
    std::fs::write(out.join(&name), &module).expect("write the module into OUT_DIR");
    std::fs::write(profile.join(&name), &module).expect("write the module next to the binary");
}
