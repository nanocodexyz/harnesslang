use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use harnesslang::target::Target;

const PROGRAMS: [&str; 4] = [
    "../../examples/claude_code.hl",
    "../../examples/claude_code_child.hl",
    "../../examples/codex_agent.hl",
    "../../examples/entry.hl",
];
const PLANT_NONE: &str = "none";
const EMBEDDED_FILE: &str = "embedded.rs";

fn main() {
    let plant = std::env::var("HL_AGENT_PLANT")
        .ok()
        .filter(|plant| !plant.is_empty())
        .unwrap_or_else(|| PLANT_NONE.to_owned());
    println!("cargo::rerun-if-env-changed=HL_AGENT_PLANT");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let mut embedded = format!(
        "pub const EMBEDDED: [(&str, &[u8]); {}] = [\n",
        PROGRAMS.len()
    );
    for path in PROGRAMS {
        println!("cargo::rerun-if-changed={path}");
        let stem = emit(path, &plant, &out);
        writeln!(
            embedded,
            "    ({stem:?}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{stem}.wasm\"))),"
        )
        .expect("String write");
    }
    embedded.push_str("];\n");
    std::fs::write(out.join(EMBEDDED_FILE), embedded).expect("embedded table write");
}

fn emit(path: &str, plant: &str, out: &Path) -> String {
    let program = harnesslang::load::load(path).unwrap_or_else(|error| panic!("{error}"));
    let module = harnesslang::emit::wasm::emit_planted(&program, Target::NativeJsonl, plant)
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    let stem = Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("program stem")
        .to_owned();
    std::fs::write(out.join(format!("{stem}.wasm")), &module).expect("module write");
    stem
}
