use std::path::PathBuf;
use std::process::Command;

const PROGRAM_DEFAULT: &str = "../examples/claude_code.hl";
const HL_DEFAULT: &str = "../target/release/hl";

fn main() {
    let program = std::env::var("HL_AGENT_PROGRAM").unwrap_or_else(|_| PROGRAM_DEFAULT.to_owned());
    let hl = std::env::var("HL_BIN").unwrap_or_else(|_| HL_DEFAULT.to_owned());
    println!("cargo::rerun-if-env-changed=HL_AGENT_PROGRAM");
    println!("cargo::rerun-if-env-changed=HL_BIN");
    println!("cargo::rerun-if-env-changed=HL_AGENT_PLANT");
    println!("cargo::rerun-if-changed={program}");
    println!("cargo::rerun-if-changed={hl}");
    let output = Command::new(&hl)
        .args(["emit", "--target", "native-jsonl", &program])
        .output()
        .unwrap_or_else(|error| panic!("run {hl} (build it with cargo build --release at the repository root, or set HL_BIN): {error}"));
    assert!(
        output.status.success(),
        "hl emit {program} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut emitted = String::from_utf8(output.stdout).expect("hl emits UTF-8");
    if let Some((from, to)) = std::env::var("HL_AGENT_PLANT")
        .ok()
        .filter(|plant| !plant.is_empty())
        .as_deref()
        .and_then(|plant| plant.split_once(" ==> "))
    {
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
    let wrapped = format!("#{attributes}\npub mod harness {{\n{body}\n}}\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("harness_mod.rs"), wrapped).expect("write the wrapped harness");
}
