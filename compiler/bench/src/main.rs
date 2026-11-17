use std::time::{Duration, Instant};

use harnesslang::emit::wasm::{assemble, flat, shake};
use harnesslang::target::Target;
use harnesslang::wasm::js::Json;
use harnesslang::wasm::native::{self, Store, World};
use harnesslang::wasm::sim;

const RUNS: u32 = 20;
const ENCODE_RS_RUNS: u32 = 2000;
const INPUT_PATH: &str = "module.flat";
const OUTPUT_PATH: &str = "module.wasm";

struct Memory {
    bytes: Vec<u8>,
}

impl Store for Memory {
    fn read(&mut self) -> Result<Vec<u8>, String> {
        Ok(self.bytes.clone())
    }

    fn append(&mut self, _epoch: i64, bytes: &[u8]) -> Result<(), String> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn keep(&mut self, length: u64) -> Result<(), String> {
        self.bytes.truncate(length as usize);
        Ok(())
    }

    fn epoch(&self) -> i64 {
        native::EPOCH
    }
}

struct Files {
    flat: String,
    written: Option<String>,
}

fn ok(text: String) -> Json {
    Json::Object(vec![("ok".to_owned(), Json::Text(text))])
}

impl World for Files {
    fn tool(&mut self, request: &Json) -> Result<Json, String> {
        let input = request.get("input");
        if request.get("tool").is_text("read_file") {
            return Ok(ok(self.flat.clone()));
        }
        let Json::Text(hex) = input.get("hex") else {
            return Err(format!("unexpected tool request {}", request.encode()));
        };
        let length = hex.len() / 2;
        self.written = Some(hex.clone());
        Ok(ok(length.to_string()))
    }

    fn model(&mut self, _request: &Json) -> Result<Json, String> {
        Err("the encoder calls no model".to_owned())
    }

    fn now(&mut self) -> Result<u64, String> {
        Ok(0)
    }

    fn random(&mut self) -> Result<u64, String> {
        Ok(0)
    }

    fn world(&mut self, request: &Json) -> Result<Json, String> {
        Err(format!("unexpected world request {}", request.encode()))
    }

    fn acknowledge(&mut self, _finish: &Json) -> Result<(), String> {
        Ok(())
    }
}

fn request(handler: &str, arguments: Vec<(&str, Json)>) -> Json {
    let arguments = arguments
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    Json::Object(vec![
        ("arguments".to_owned(), Json::Object(arguments)),
        ("handler".to_owned(), Json::text(handler)),
        ("turn".to_owned(), Json::int(1)),
    ])
}

fn best<F: FnMut()>(runs: u32, mut body: F) -> Duration {
    (0..runs)
        .map(|_| {
            let start = Instant::now();
            body();
            start.elapsed()
        })
        .min()
        .expect("at least one run")
}

fn millis(duration: Duration) -> String {
    format!("{:.3} ms", duration.as_secs_f64() * 1000.0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn main() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().collect();
    let [_, program, encoder_wasm] = arguments.as_slice() else {
        return Err("usage: selfhost-bench <program.hl> <encode.wasm>".to_owned());
    };
    let target = Target::parse("native-jsonl").expect("native-jsonl is a target");
    let loaded = harnesslang::load::load(program)?;
    let text = harnesslang::emit::wasm::emit_text(&loaded, target)?;
    let module = assemble::assemble_folded(&text, shake::Arms::Taken)?;
    let expected = module.encode();
    let flat_hex = flat::hex(&flat::flatten(&module));

    let encode_rs = best(ENCODE_RS_RUNS, || {
        std::hint::black_box(module.encode());
    });

    let bytes = std::fs::read(encoder_wasm).map_err(|error| format!("{encoder_wasm}: {error}"))?;
    let load = best(RUNS, || {
        std::hint::black_box(sim::load(&bytes).expect("the encoder loads"));
    });
    let encoder = sim::load(&bytes)?;

    let compile_request = request(
        "compile",
        vec![
            ("input_path", Json::text(INPUT_PATH)),
            ("output_path", Json::text(OUTPUT_PATH)),
        ],
    );
    let mut written = None;
    let compile = best(RUNS, || {
        let mut files = Files {
            flat: flat_hex.clone(),
            written: None,
        };
        let mut store = Memory { bytes: Vec::new() };
        native::run(&encoder, &mut store, &compile_request, &mut files).expect("compile runs");
        written = files.written;
    });
    if written.as_deref() != Some(hex(&expected).as_str()) {
        return Err("compile wrote other bytes than encode.rs".to_owned());
    }

    let empty_request = request("encode", vec![("input", Json::text(""))]);
    let empty = best(RUNS, || {
        let mut files = Files {
            flat: String::new(),
            written: None,
        };
        let mut store = Memory { bytes: Vec::new() };
        native::run(&encoder, &mut store, &empty_request, &mut files).expect("encode runs");
    });

    println!("program {program}: flat {} bytes, module {} bytes", flat_hex.len() / 2, expected.len());
    println!("encode.rs {}", millis(encode_rs));
    println!("encoder load (decode and compile, startup) {}", millis(load));
    println!("encode.hl compile handler {} (empty handler run {})", millis(compile), millis(empty));
    let work = compile.saturating_sub(empty);
    println!(
        "encode.hl over encode.rs: {:.0}x (handler run less an empty run), {:.0}x (whole handler run)",
        work.as_secs_f64() / encode_rs.as_secs_f64(),
        compile.as_secs_f64() / encode_rs.as_secs_f64()
    );
    Ok(())
}
