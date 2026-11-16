use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::anthropic::{self, Usage};
use crate::json::Value;

const SYSTEM: &str = include_str!("../prompts/reviewer-system.md");
pub const REVIEWS_MAX: u64 = 2;
pub const MODEL: &str = "claude-sonnet-4-6";
const INPUT_NANO_USD_PER_TOKEN: u64 = 3000;
const OUTPUT_NANO_USD_PER_TOKEN: u64 = 15000;
const CACHE_READ_NANO_USD_PER_TOKEN: u64 = 300;
const CACHE_WRITE_NANO_USD_PER_TOKEN: u64 = 3750;
const BRIEF_TOKENS_BUDGET: usize = 6000;
const CHARS_PER_TOKEN: usize = 4;
const BRIEF_CHARS: usize = BRIEF_TOKENS_BUDGET * CHARS_PER_TOKEN;
const INSTRUCTION_CHARS: usize = BRIEF_CHARS / 3;
const EXCERPT_ENTRIES: usize = 16;
const ENTRY_CHARS: usize = (BRIEF_CHARS - INSTRUCTION_CHARS) / EXCERPT_ENTRIES;
const REVIEW_TOKENS_MAX: u64 = 1500;
const DONE_LINE: &str = "DONE";
const RELAY_WAIT_MS: u64 = 900_000;
const RELAY_POLL_MS: u64 = 1000;
const RELAY_POLLS_MAX: u64 = RELAY_WAIT_MS / RELAY_POLL_MS;
const RELAY_DIR_MODE: u32 = 0o777;

pub struct Reviewer {
    pub relay: PathBuf,
    pub instruction: String,
}

pub enum Verdict {
    Done(String),
    NotDone(String),
}

pub fn cost_nano_usd(used: Usage) -> u64 {
    used.input * INPUT_NANO_USD_PER_TOKEN
        + used.output * OUTPUT_NANO_USD_PER_TOKEN
        + used.cache_read * CACHE_READ_NANO_USD_PER_TOKEN
        + used.cache_write * CACHE_WRITE_NANO_USD_PER_TOKEN
}

fn clipped(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    let half = limit / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(count - half).collect();
    format!(
        "{head}\n[... {} characters cut ...]\n{tail}",
        count - 2 * half
    )
}

fn excerpt(context: &Value, instruction: &str) -> String {
    let entries = context.as_array().unwrap_or(&[]);
    let mut kept = Vec::new();
    for entry in entries.iter().rev() {
        if kept.len() == EXCERPT_ENTRIES {
            break;
        }
        if entry
            .get("user")
            .and_then(Value::as_str)
            .is_some_and(|text| text == instruction)
        {
            continue;
        }
        let shown = match entry.get("user").and_then(Value::as_str) {
            Some(text) => format!("[user] {text}"),
            None => entry.encode(),
        };
        kept.push(clipped(&shown, ENTRY_CHARS));
    }
    kept.reverse();
    kept.join("\n\n")
}

impl Reviewer {
    pub fn review(&self, context: &Value, index: u64) -> (Result<Verdict, String>, Usage) {
        let brief = format!(
            "<task>\n{}\n</task>\n\n<recent_transcript>\n{}\n</recent_transcript>",
            clipped(&self.instruction, INSTRUCTION_CHARS),
            excerpt(context, &self.instruction)
        );
        let body = Value::object([
            ("model", Value::text(MODEL)),
            ("max_tokens", Value::number(REVIEW_TOKENS_MAX)),
            ("system", Value::text(SYSTEM.trim_end())),
            (
                "messages",
                Value::Array(vec![Value::object([
                    ("role", Value::text("user")),
                    ("content", Value::text(&brief)),
                ])]),
            ),
        ]);
        let text = match relayed(&self.relay, index, &body) {
            Ok(text) => text,
            Err(error) => {
                return (
                    Err(format!("the reviewer did not answer: {error}")),
                    Usage::default(),
                );
            }
        };
        let Ok(value) = Value::decode(&text) else {
            return (
                Err("the reviewer's reply is not JSON".to_owned()),
                Usage::default(),
            );
        };
        if let Some(error) = value.get("error") {
            return (
                Err(format!(
                    "the reviewer did not answer: {}",
                    clipped(&error.encode(), ENTRY_CHARS)
                )),
                Usage::default(),
            );
        }
        let used = anthropic::usage(&value);
        let review: String = value
            .get("content")
            .and_then(Value::as_array)
            .unwrap_or(&[])
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect();
        let verdict = match review.trim_start().lines().next().map(str::trim) {
            Some(DONE_LINE) => Ok(Verdict::Done(review)),
            Some(_) => Ok(Verdict::NotDone(review)),
            None => Err("the reviewer answered with no text".to_owned()),
        };
        (verdict, used)
    }
}

fn relayed(relay: &Path, index: u64, body: &Value) -> Result<String, String> {
    std::fs::create_dir_all(relay).map_err(|error| format!("{}: {error}", relay.display()))?;
    std::fs::set_permissions(relay, std::fs::Permissions::from_mode(RELAY_DIR_MODE))
        .map_err(|error| format!("{}: {error}", relay.display()))?;
    let request = relay.join(format!("request-{index}.json"));
    let staged = relay.join(format!("request-{index}.json.new"));
    let response = relay.join(format!("response-{index}.json"));
    std::fs::write(&staged, body.encode())
        .map_err(|error| format!("{}: {error}", staged.display()))?;
    std::fs::rename(&staged, &request)
        .map_err(|error| format!("{}: {error}", request.display()))?;
    for _ in 0..RELAY_POLLS_MAX {
        if let Ok(text) = std::fs::read_to_string(&response) {
            return Ok(text);
        }
        std::thread::sleep(std::time::Duration::from_millis(RELAY_POLL_MS));
    }
    Err(format!(
        "no {} after {RELAY_WAIT_MS} ms",
        response.display()
    ))
}
