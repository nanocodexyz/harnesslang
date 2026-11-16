use std::fmt::Write as _;
use std::ops::Range;

use crate::emit::wasm::{assemble, shake};
use crate::ir::{Program, Wire};
use crate::run::json::Json;
use crate::sim::rng::Rng;
use crate::wasm::decode::ExportKind;
use crate::wasm::exec::Limits;
use crate::wasm::{Error, Host, Instance, Module, Value};
use crate::wire::json::Value as Raw;

pub const CASES_PER_INSTANCE: u64 = 64;
pub const ENTRIES_MAX: u64 = 12;
pub const CALLS_MAX: u64 = 3;
const WORD_BITS: u32 = 32;
const LOW_WORD: u64 = 0xffff_ffff;
const PERCENT: u64 = 100;

const SOURCE_HEAD: &str = "harness probe\n\nconst calls_max: u64 = 4\nconst window_tokens: u64 = 1000\nconst context_max: u64 = 8\n\n";
const SOURCE_ANTHROPIC: &str = "type probe_reply = answer(text, text) | calls(list<call, calls_max>, text, text)\n\nmodel probe calls calls_max reply probe_reply window window_tokens settings(url: text = \"\", headers: text = \"[]\", head: text = \"\", tail: text = \"\", preamble: text = \"\") wire anthropic\n";
const SOURCE_OPENAI: &str = "type probe_reply = message(text, text, bool) | calls(list<call, calls_max>, text, text)\n\nmodel probe calls calls_max reply probe_reply window window_tokens settings(url: text = \"\", headers: text = \"[]\", head: text = \"\", tail: text = \"\", preamble: text = \"\") wire openai\n";
const SOURCE_TAIL: &str = "\nstate {\n  context: list<entry, context_max>\n}\n\non ask(question: text) -> text {\n  match probe(context) {\n    ok(_, _, _, _, _, _) => { return question }\n    throttled(error, _) => { return error }\n    transient(error, _) => { return error }\n    rejected(error) => { return error }\n  }\n}\n";

pub const PROBE: &str = r#"
(text $p_op "op")
(text $p_wire "wire")
(text $p_context "context")
(text $p_settings "settings")
(text $p_mode "mode")
(text $p_seq "seq")
(text $p_turn "turn")
(text $p_answer "answer")
(text $p_interruptible "interruptible")
(text $p_request "request")
(text $p_error "error")
(text $p_blobs "blobs")
(func $probe_hold (param $blobs i32) (local $index i32)
  (global.set $blob_held (call $vec_new (global.get $BLOB_ENTRY_BYTES)))
  (if (i32.eqz (local.get $blobs)) (then (return)))
  block $done
    loop $next
      (br_if $done (i32.ge_u (local.get $index) (call $json_count (local.get $blobs))))
      (call $blob_hold (call $json_key (local.get $blobs) (local.get $index)) (call $json_text (call $json_value (local.get $blobs) (local.get $index))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end)
(func $wire_probe (export "hl_wire_probe") (param $bytes i32) (param $length i32) (result i64) (local $input i32) (local $wire i32) (local $mode i32) (local $buffer i32)
  (local.set $input (call $json_parse_bytes (local.get $bytes) (local.get $length)))
  (local.set $wire (i32.wrap_i64 (call $json_number_value (call $json_get (local.get $input) (i32.const $p_wire)))))
  (if (call $text_eq (call $json_text (call $json_get (local.get $input) (i32.const $p_op))) (i32.const $p_request))
    (then
      (global.set $turn (call $json_number_value (call $json_get (local.get $input) (i32.const $p_turn))))
      (call $probe_hold (call $json_get (local.get $input) (i32.const $p_blobs)))
      (local.set $mode (call $json_get (local.get $input) (i32.const $p_mode)))
      (if (local.get $mode) (then (local.set $mode (call $json_text (local.get $mode)))))
      (local.set $buffer (call $wire_request (local.get $wire)
        (call $json_get (local.get $input) (i32.const $p_context))
        (call $json_get (local.get $input) (i32.const $p_settings))
        (local.get $mode)
        (call $json_number_value (call $json_get (local.get $input) (i32.const $p_seq)))))
      (if (i32.eqz (local.get $buffer))
        (then (return (call $packed (call $json_encode (call $json_one (i32.const $p_error) (call $json_string (global.get $wire_error))))))))
      (return (call $packed (call $buf_finish (local.get $buffer))))))
  (call $packed (call $json_encode (call $wire_decode (local.get $wire)
    (call $json_get (local.get $input) (i32.const $p_answer))
    (i32.wrap_i64 (call $json_number_value (call $json_get (local.get $input) (i32.const $p_interruptible))))))))
"#;

struct Refusing;

impl Host for Refusing {
    type Stop = String;

    fn call(
        &mut self,
        import: usize,
        _args: &[Value],
        _instance: &mut Instance<'_>,
    ) -> Result<Option<Value>, Error<String>> {
        Err(Error::Stop(format!(
            "the wire probe called import {import}"
        )))
    }
}

pub fn program(wire: Wire) -> Result<Program, String> {
    let model = match wire {
        Wire::Anthropic => SOURCE_ANTHROPIC,
        Wire::Openai => SOURCE_OPENAI,
    };
    let source = format!("{SOURCE_HEAD}{model}{SOURCE_TAIL}");
    let parsed =
        crate::syntax::parse(&source).map_err(|diagnostic| diagnostic.render("probe.hl"))?;
    crate::check::check(&parsed).map_err(|diagnostic| diagnostic.render("probe.hl"))
}

pub fn module(wire: Wire) -> Result<Module, String> {
    let program = program(wire)?;
    let mut text = crate::emit::wasm::emit_text(&program, crate::target::Target::NativeJsonl)?;
    text.push_str(PROBE);
    let assembled = assemble::assemble_folded(&text, shake::Arms::Taken)?;
    Module::decode(&assembled.encode())
}

fn export(module: &Module, name: &str) -> Result<u32, String> {
    module
        .exports
        .iter()
        .find(|export| export.name == name && export.kind == ExportKind::Function)
        .map(|export| export.index)
        .ok_or_else(|| format!("the wire probe module exports no {name}"))
}

struct Probe<'m> {
    module: &'m Module,
    instance: Instance<'m>,
    alloc: u32,
    probe: u32,
}

impl<'m> Probe<'m> {
    fn new(module: &'m Module) -> Result<Self, String> {
        Ok(Probe {
            module,
            instance: Instance::new(module, Limits::NODE),
            alloc: export(module, "hl_alloc")?,
            probe: export(module, "hl_wire_probe")?,
        })
    }

    fn fresh(&mut self) {
        self.instance = Instance::new(self.module, Limits::NODE);
    }

    fn ask(&mut self, input: &str) -> Result<String, String> {
        let stop = |error: Error<String>| match error {
            Error::Trap(trap) => format!("the wire probe trapped: {trap}"),
            Error::Stop(stop) => stop,
        };
        let length = i32::try_from(input.len()).map_err(|_| "a probe input fits i32")?;
        let at = match self
            .instance
            .invoke(&mut Refusing, self.alloc, &[Value::I32(length)])
            .map_err(stop)?
        {
            Some(Value::I32(at)) => at,
            _ => return Err("hl_alloc returns an i32".to_owned()),
        };
        self.instance
            .memory_mut()
            .write(at as u32, input.as_bytes())
            .map_err(|trap| format!("the probe input does not fit: {trap}"))?;
        let packed = match self
            .instance
            .invoke(
                &mut Refusing,
                self.probe,
                &[Value::I32(at), Value::I32(length)],
            )
            .map_err(stop)?
        {
            Some(Value::I64(packed)) => packed as u64,
            _ => return Err("hl_wire_probe returns an i64".to_owned()),
        };
        let bytes = self
            .instance
            .memory()
            .read((packed >> WORD_BITS) as u32, (packed & LOW_WORD) as u32)
            .map_err(|trap| format!("the probe answer is out of bounds: {trap}"))?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "the probe answer is not UTF-8".to_owned())
    }
}

const TEXTS: [&str; 14] = [
    "",
    "hello",
    "line one\nline two",
    "quote \" and backslash \\",
    "tab\tand\rreturn",
    "control \u{1}\u{1f}",
    "unicode Ã© â ð",
    "{\"json\":\"looking\"}",
    "Please try again in 1.5s.",
    "rate limited, try again in 250ms",
    "prompt is too long: 210000 tokens > 200000 maximum",
    "try again in 3s and later",
    "</tail>",
    "a slightly longer piece of prose that a model might write back as its answer",
];

const NUMBERS: [&str; 10] = [
    "0",
    "1",
    "42",
    "100000",
    "18446744073709551615",
    "18446744073709551616",
    "1.5",
    "-3",
    "1e3",
    "007",
];

const CODES: [&str; 16] = [
    "",
    "insufficient_quota",
    "usage_not_included",
    "rate_limit_exceeded",
    "server_is_overloaded",
    "invalid_prompt",
    "cyber_policy",
    "context_length_exceeded",
    "server_error",
    "invalid_request_error",
    "rate_limit_error",
    "overloaded_error",
    "api_error",
    "authentication_error",
    "not_found_error",
    "slow_down",
];

const STATUSES: [u64; 12] = [200, 400, 401, 404, 408, 409, 413, 429, 500, 502, 503, 529];

const BLOBS: [(&str, &[u8]); 10] = [
    ("sha256:png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
    ("sha256:png-cut", b"\x89PNG\r\n"),
    ("sha256:jpeg", b"\xff\xd8\xff\xe0\0\x10JFIF"),
    ("sha256:gif87", b"GIF87a\x01\0"),
    ("sha256:gif89", b"GIF89a"),
    ("sha256:webp", b"RIFF\x24\0\0\0WEBPVP8 "),
    ("sha256:wave", b"RIFF\x24\0\0\0WAVEfmt "),
    ("sha256:riff-cut", b"RIFF\x24\0\0\0WEB"),
    ("sha256:text", b"plain words"),
    ("sha256:empty", b""),
];

const BLOB_ABSENT: &str = "sha256:absent";

fn blobs() -> Json {
    Json::Object(
        BLOBS
            .iter()
            .map(|(digest, bytes)| {
                (
                    (*digest).to_owned(),
                    Json::String(crate::run::blob::base64_encode(bytes)),
                )
            })
            .collect(),
    )
}

struct Gen {
    rng: Rng,
}

impl Gen {
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.rng.pick(items.len())]
    }

    fn percent(&mut self, percent: u64) -> bool {
        self.rng.below(PERCENT) < percent
    }

    fn text(&mut self) -> String {
        self.pick(&TEXTS).to_owned()
    }

    fn quoted(&mut self) -> String {
        Raw::String(self.text()).encode()
    }

    fn id(&mut self) -> String {
        format!("toolu_{}", self.rng.below(5))
    }

    fn input(&mut self) -> String {
        match self.rng.below(6) {
            0 => "{}".to_owned(),
            1 => "not json".to_owned(),
            2 => format!("{{\"path\":{}}}", self.quoted()),
            3 => "{\"list\":[1,2.5,{\"deep\":null}],\"flag\":true}".to_owned(),
            4 => "  {\"spaced\" : \"yes\" }  ".to_owned(),
            _ => {
                let command = self.quoted();
                let number = self.pick(&NUMBERS);
                format!("{{\"command\":{command},\"n\":{number}}}")
            }
        }
    }

    fn call(&mut self) -> Json {
        let tool = self.pick(&["read", "bash", "apply_patch", "web_search"]);
        let id = self.id();
        let input = self.input();
        crate::wire::call(&id, tool, &input)
    }

    fn calls(&mut self) -> Json {
        let count = self.rng.range(1, CALLS_MAX);
        Json::Array((0..count).map(|_| self.call()).collect())
    }

    fn anthropic_block(&mut self) -> Raw {
        match self.rng.below(6) {
            0 => Raw::object([
                ("type", Raw::text("text")),
                ("text", Raw::String(self.text())),
            ]),
            1 => Raw::object([("type", Raw::text("text")), ("text", Raw::text(""))]),
            2 => {
                let id = self.id();
                let input = Raw::decode(&self.input()).unwrap_or(Raw::Object(Vec::new()));
                Raw::object([
                    ("type", Raw::text("tool_use")),
                    ("id", Raw::String(id)),
                    ("name", Raw::text("bash")),
                    ("input", input),
                ])
            }
            3 => Raw::object([
                ("type", Raw::text("thinking")),
                ("thinking", Raw::String(self.text())),
                ("signature", Raw::text("c2ln")),
            ]),
            4 => Raw::object([
                ("type", Raw::text("redacted_thinking")),
                ("data", Raw::text("ZGF0YQ==")),
            ]),
            _ => Raw::object([("type", Raw::text("text"))]),
        }
    }

    fn part(kind: &str, key: &str, text: String) -> Raw {
        Raw::object([("type", Raw::text(kind)), (key, Raw::String(text))])
    }

    fn openai_item(&mut self) -> Raw {
        match self.rng.below(8) {
            0 => Raw::object([
                ("type", Raw::text("reasoning")),
                ("id", Raw::text("rs_1")),
                (
                    "summary",
                    Raw::Array(vec![Self::part("summary_text", "text", self.text())]),
                ),
                ("encrypted_content", Raw::text("ZW5j")),
            ]),
            1 => Raw::object([("type", Raw::text("reasoning")), ("id", Raw::text("rs_2"))]),
            2 => {
                let parts = vec![
                    Self::part("output_text", "text", self.text()),
                    Self::part("refusal", "refusal", self.text()),
                    Self::part("annotation", "text", self.text()),
                ];
                Raw::object([
                    ("type", Raw::text("message")),
                    ("id", Raw::text("msg_1")),
                    ("role", Raw::text("assistant")),
                    ("content", Raw::Array(parts)),
                ])
            }
            3 => Raw::object([
                ("type", Raw::text("message")),
                ("role", Raw::text("assistant")),
                (
                    "content",
                    Raw::Array(vec![Self::part("output_text", "text", self.text())]),
                ),
            ]),
            4 => {
                let id = self.id();
                let arguments = self.input();
                Raw::object([
                    ("type", Raw::text("function_call")),
                    ("id", Raw::text("fc_1")),
                    ("call_id", Raw::String(id)),
                    ("name", Raw::text("bash")),
                    ("arguments", Raw::String(arguments)),
                ])
            }
            5 => {
                let id = self.id();
                Raw::object([
                    ("type", Raw::text("custom_tool_call")),
                    ("call_id", Raw::String(id)),
                    ("name", Raw::text("apply_patch")),
                    ("input", Raw::String(self.text())),
                ])
            }
            6 => Raw::object([
                ("type", Raw::text("web_search_call")),
                ("id", Raw::text("ws_1")),
            ]),
            _ => Raw::object([
                ("type", Raw::text("message")),
                ("content", Raw::Array(Vec::new())),
            ]),
        }
    }

    fn raw(&mut self, wire: Wire) -> String {
        match self.rng.below(5) {
            0 | 1 => String::new(),
            2 => "not json".to_owned(),
            _ => {
                let count = self.rng.below(4);
                let items = (0..count)
                    .map(|_| match wire {
                        Wire::Anthropic => self.anthropic_block(),
                        Wire::Openai => self.openai_item(),
                    })
                    .collect();
                Raw::Array(items).encode()
            }
        }
    }

    fn reply(&mut self, wire: Wire) -> Json {
        let words = Json::String(self.text());
        if self.percent(50) {
            let calls = self.calls();
            let raw = Json::String(self.raw(wire));
            return crate::wire::tagged_json("calls", vec![calls, words, raw]);
        }
        let raw = Json::String(self.raw(wire));
        match wire {
            Wire::Anthropic => crate::wire::tagged_json("answer", vec![words, raw]),
            Wire::Openai => {
                let end = Json::Bool(self.percent(50));
                crate::wire::tagged_json("message", vec![words, raw, end])
            }
        }
    }

    fn entry(&mut self, wire: Wire) -> Json {
        let (name, value) = match self.rng.below(8) {
            0 => ("user", Json::String(self.text())),
            1 | 2 => ("assistant", self.reply(wire)),
            3 => (
                "result",
                Json::Array(vec![self.call(), Json::String(self.text())]),
            ),
            4 => (
                "error",
                Json::Array(vec![self.call(), Json::String(self.text())]),
            ),
            5 => ("unknown", self.call()),
            6 => {
                let digest = if self.percent(10) {
                    BLOB_ABSENT
                } else {
                    BLOBS[self.rng.pick(BLOBS.len())].0
                };
                let blob = Json::object([("blob", Json::text(digest))]);
                ("attached", Json::Array(vec![self.call(), blob]))
            }
            _ => ("notice", Json::String(self.text())),
        };
        Json::object([(name, value)])
    }

    fn context(&mut self, wire: Wire) -> Json {
        let count = self.rng.below(ENTRIES_MAX);
        Json::Array((0..count).map(|_| self.entry(wire)).collect())
    }

    fn settings(&mut self) -> Json {
        let headers = match self.rng.below(5) {
            0 => "[]",
            1 => {
                "[[\"x-api-key\",\"${ANTHROPIC_API_KEY}\"],[\"anthropic-version\",\"2023-06-01\"]]"
            }
            2 => "[[\"a\",\"b\",\"c\"]]",
            3 => "{\"x\":\"y\"}",
            _ => " [ [\"authorization\" , \"Bearer ${OPENAI_API_KEY:-none}\"] ] ",
        };
        let note = self.quoted();
        let mut fields = vec![
            ("url", Json::text("https://api.example.com/v1")),
            ("headers", Json::text(headers)),
            (
                "head",
                Json::String(format!("{{\"model\":\"m\",\"note\":{note},\"messages\":")),
            ),
            ("tail", Json::text("}")),
        ];
        match self.rng.below(4) {
            0 => fields.push(("preamble", Json::text(""))),
            1 => fields.push((
                "preamble",
                Json::text("{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"AGENTS.md\"}]}"),
            )),
            _ => {}
        }
        if self.percent(10) {
            fields.retain(|(name, _)| *name != "tail");
        }
        object_of(fields)
    }

    fn usage(&mut self) -> Json {
        if self.percent(10) {
            return Json::Number(u64::MAX - self.rng.below(3));
        }
        Json::Number(self.rng.below(100_000))
    }

    fn attempt(&mut self, wire: Wire) -> Json {
        match self.rng.below(10) {
            0 => {
                let text = self.text();
                crate::wire::throttled(&text, self.rng.below(3) * 500)
            }
            1 => {
                let text = self.text();
                crate::wire::transient(&text, self.rng.below(3) * 250)
            }
            2 => crate::wire::rejected(&self.text()),
            3 => crate::wire::interrupted(),
            _ => {
                let stop = self.pick(&[
                    "end",
                    "tool_use",
                    "max_tokens",
                    "pause",
                    "refusal",
                    "context_exceeded",
                ]);
                let fields = vec![
                    self.reply(wire),
                    Json::text(stop),
                    self.usage(),
                    self.usage(),
                    self.usage(),
                    self.usage(),
                ];
                crate::wire::tagged_json("ok", fields)
            }
        }
    }

    fn headers(&mut self) -> Json {
        let mut pairs = Vec::new();
        if self.percent(30) {
            let name = self.pick(&["retry-after", "retry-after-ms", "Retry-After"]);
            let value = self.pick(&["12", " 7 ", "1.5", "abc", "", "18446744073709551616"]);
            pairs.push(Json::Array(vec![Json::text(name), Json::text(value)]));
        }
        if self.percent(50) {
            pairs.push(Json::Array(vec![
                Json::text("content-type"),
                Json::text("application/json"),
            ]));
        }
        Json::Array(pairs)
    }

    fn error_body(&mut self) -> String {
        let code = self.pick(&CODES);
        let message = self.text();
        let error = |key: &str| {
            Raw::object([
                (key, Raw::text(code)),
                ("message", Raw::String(message.clone())),
            ])
        };
        match self.rng.below(5) {
            0 => Raw::object([("error", error("type"))]).encode(),
            1 => Raw::object([("type", Raw::text("error")), ("error", error("type"))]).encode(),
            2 => Raw::object([("error", error("code"))]).encode(),
            3 => message,
            _ => "{\"error\":null}".to_owned(),
        }
    }

    fn mutate_events(&mut self, events: &mut Vec<Json>) {
        let changes = self.rng.below(3);
        for _ in 0..changes {
            let data = match self.rng.below(8) {
                0 => "{\"type\":\"ping\"}".to_owned(),
                1 => "not json".to_owned(),
                2 => "[DONE]".to_owned(),
                3 => "{\"type\":\"surprise\"}".to_owned(),
                4 => {
                    let code = self.pick(&CODES);
                    format!("{{\"type\":\"error\",\"error\":{{\"type\":\"{code}\",\"message\":\"boom\"}}}}")
                }
                5 => "{\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_error\",\"message\":\"try again in 2s\"}}}".to_owned(),
                6 => "   ".to_owned(),
                _ => {
                    if !events.is_empty() {
                        let at = self.rng.pick(events.len());
                        events.remove(at);
                    }
                    continue;
                }
            };
            let at = self.rng.pick(events.len() + 1);
            events.insert(
                at,
                Json::Array(vec![Json::text("event"), Json::String(data)]),
            );
        }
    }

    fn answer(&mut self, wire: Wire) -> Json {
        let drawn = self.attempt(wire);
        let encoded = crate::wire::encode(wire, &drawn);
        match self.rng.below(12) {
            0 => Json::object([("transport", Json::String(self.text()))]),
            1 => Json::object([("stopped", Json::Bool(true))]),
            2 => Json::object([("stopped", Json::Bool(false))]),
            3 => Json::object([("status", Json::Number(200))]),
            4 | 5 => {
                let status = STATUSES[self.rng.pick(STATUSES.len())];
                let headers = self.headers();
                let body = Json::String(self.error_body());
                Json::object([
                    ("status", Json::Number(status)),
                    ("headers", headers),
                    ("body", body),
                ])
            }
            6 | 7 => {
                let Json::Object(mut map) = encoded else {
                    return encoded;
                };
                if let Some(Json::Array(events)) = map.get_mut("events") {
                    self.mutate_events(events);
                }
                Json::Object(map)
            }
            _ => encoded,
        }
    }
}

fn object_of(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn request_rust(wire: Wire, input: &Json) -> String {
    let field = |key: &str| input.get(key);
    let request = crate::wire::Request {
        wire,
        turn: field("turn").and_then(Json::as_u64).unwrap_or(0),
        seq: field("seq").and_then(Json::as_u64).unwrap_or(0),
        mode: field("mode").and_then(Json::as_str),
        settings: field("settings"),
        context: field("context").expect("a probe carries a context"),
    };
    let held = field("blobs");
    let attached = |digest: &str| {
        let data = held?.get(digest)?.as_str()?;
        let bytes = crate::run::blob::base64_decode(data).ok()?;
        let media_type = crate::wire::media_type(&bytes).unwrap_or(crate::wire::MEDIA_TYPE_DEFAULT);
        Some((media_type.to_owned(), data.to_owned()))
    };
    match crate::wire::request(&request, &attached) {
        Ok(json) => json.encode(),
        Err(error) => Json::object([("error", Json::String(error))]).encode(),
    }
}

fn decode_rust(wire: Wire, input: &Json) -> String {
    let answer = input.get("answer").expect("a probe carries an answer");
    let interruptible = input.get("interruptible").and_then(Json::as_u64) == Some(1);
    crate::wire::decode(wire, answer, interruptible).encode()
}

fn wire_number(wire: Wire) -> u64 {
    match wire {
        Wire::Anthropic => 1,
        Wire::Openai => 2,
    }
}

pub fn case(seed: u64, wire: Wire) -> (Json, Json) {
    let mut generator = Gen {
        rng: Rng::derive_keyed(seed, "wire diff", &[wire_number(wire)]),
    };
    let mut request = vec![
        ("op", Json::text("request")),
        ("wire", Json::Number(wire_number(wire))),
        ("turn", Json::Number(generator.rng.below(50))),
        ("seq", Json::Number(generator.rng.below(500))),
        ("context", generator.context(wire)),
        ("settings", generator.settings()),
        ("blobs", blobs()),
    ];
    match generator.rng.below(3) {
        0 => request.push(("mode", Json::text("refresh"))),
        1 => request.push(("mode", Json::text("normal"))),
        _ => {}
    }
    let decode = Json::object([
        ("op", Json::text("decode")),
        ("wire", Json::Number(wire_number(wire))),
        ("answer", generator.answer(wire)),
        ("interruptible", Json::Number(generator.rng.below(2))),
    ]);
    (object_of(request), decode)
}

pub fn run(seeds: Range<u64>) -> Result<String, String> {
    let mut summary = String::new();
    for wire in [Wire::Anthropic, Wire::Openai] {
        let module = module(wire)?;
        let mut probe = Probe::new(&module)?;
        let mut cases = 0_u64;
        for seed in seeds.clone() {
            if seed % CASES_PER_INSTANCE == 0 {
                probe.fresh();
            }
            let (request, decode) = case(seed, wire);
            let expected = [request_rust(wire, &request), decode_rust(wire, &decode)];
            for (input, expected) in [&request, &decode].into_iter().zip(expected) {
                let encoded = input.encode();
                let actual = probe.ask(&encoded).map_err(|error| {
                    format!(
                        "{error}\n  wire: {}\n  seed: {seed}\n  input: {encoded}",
                        wire.name()
                    )
                })?;
                if actual != expected {
                    return Err(format!(
                        "wire-diff: {} seed {seed} differs\n  input: {encoded}\n  rust:  {expected}\n  wasm:  {actual}",
                        wire.name()
                    ));
                }
                cases += 1;
            }
        }
        writeln!(summary, "ok wire={} cases={cases}", wire.name())
            .expect("writing to a string succeeds");
    }
    Ok(summary)
}
