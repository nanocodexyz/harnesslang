use crate::json::Value;

pub enum Streamed {
    Message(Value),
    Error { kind: String, message: String },
}

struct Message {
    head: Value,
    blocks: Vec<Value>,
    inputs: Vec<String>,
    stop_reason: Option<Value>,
    usage: Vec<(String, Value)>,
}

fn data_lines(stream: &str) -> impl Iterator<Item = &str> {
    stream
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty())
}

fn field<'v>(value: &'v Value, key: &str, what: &str) -> Result<&'v Value, String> {
    value
        .get(key)
        .ok_or_else(|| format!("stream {what} event has no {key}"))
}

fn index(event: &Value, blocks: usize, what: &str) -> Result<usize, String> {
    let index = field(event, "index", what)?
        .as_u64()
        .ok_or_else(|| format!("stream {what} event's index is not a number"))?;
    usize::try_from(index)
        .ok()
        .filter(|index| *index < blocks)
        .ok_or_else(|| format!("stream {what} event names block {index}, which has not started"))
}

fn append(block: &mut Value, key: &str, delta: &str) {
    let mut text = block
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    text.push_str(delta);
    block.insert(key, Value::String(text));
}

impl Message {
    fn delta(&mut self, event: &Value) -> Result<(), String> {
        let at = index(event, self.blocks.len(), "content_block_delta")?;
        let delta = field(event, "delta", "content_block_delta")?;
        let text = |key: &str| {
            delta
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("stream delta has no {key}"))
        };
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => append(&mut self.blocks[at], "text", text("text")?),
            Some("thinking_delta") => append(&mut self.blocks[at], "thinking", text("thinking")?),
            Some("signature_delta") => {
                self.blocks[at].insert("signature", Value::text(text("signature")?))
            }
            Some("input_json_delta") => self.inputs[at].push_str(text("partial_json")?),
            Some("citations_delta") => {}
            other => return Err(format!("stream delta of unknown type {other:?}")),
        }
        Ok(())
    }

    fn stop(&mut self, event: &Value) -> Result<(), String> {
        let at = index(event, self.blocks.len(), "content_block_stop")?;
        if self.inputs[at].is_empty() {
            return Ok(());
        }
        let input = Value::decode(&self.inputs[at])
            .map_err(|error| format!("stream tool input is not JSON: {error}"))?;
        self.blocks[at].insert("input", input);
        Ok(())
    }

    fn finish(self) -> Value {
        let mut message = self.head;
        message.insert("content", Value::Array(self.blocks));
        message.insert("stop_reason", self.stop_reason.unwrap_or(Value::Null));
        message.insert("usage", Value::Object(self.usage));
        message
    }
}

fn merge_usage(usage: &mut Vec<(String, Value)>, update: Option<&Value>) {
    let Some(Value::Object(entries)) = update else {
        return;
    };
    for (key, value) in entries {
        match usage.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, slot)) => *slot = value.clone(),
            None => usage.push((key.clone(), value.clone())),
        }
    }
}

pub fn assemble(stream: &str) -> Result<Streamed, String> {
    let mut message: Option<Message> = None;
    for data in data_lines(stream) {
        let event =
            Value::decode(data).map_err(|error| format!("stream event is not JSON: {error}"))?;
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "error" {
            let error = event.get("error");
            let text = |key: &str| {
                error
                    .and_then(|error| error.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            return Ok(Streamed::Error {
                kind: text("type"),
                message: text("message"),
            });
        }
        if kind == "ping" {
            continue;
        }
        if kind == "message_start" {
            let head = field(&event, "message", "message_start")?.clone();
            let mut usage = Vec::new();
            merge_usage(&mut usage, head.get("usage"));
            message = Some(Message {
                head,
                blocks: Vec::new(),
                inputs: Vec::new(),
                stop_reason: None,
                usage,
            });
            continue;
        }
        let open = message
            .as_mut()
            .ok_or_else(|| format!("stream {kind} event before message_start"))?;
        match kind {
            "content_block_start" => {
                let block = field(&event, "content_block", kind)?.clone();
                let at = field(&event, "index", kind)?.as_u64();
                if at != Some(open.blocks.len() as u64) {
                    return Err(format!(
                        "stream starts block {at:?} after {} blocks",
                        open.blocks.len()
                    ));
                }
                open.blocks.push(block);
                open.inputs.push(String::new());
            }
            "content_block_delta" => open.delta(&event)?,
            "content_block_stop" => open.stop(&event)?,
            "message_delta" => {
                if let Some(reason) = event
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                {
                    open.stop_reason = Some(reason.clone());
                }
                merge_usage(&mut open.usage, event.get("usage"));
            }
            "message_stop" => {
                let done = message.take().expect("a message is open");
                return Ok(Streamed::Message(done.finish()));
            }
            other => return Err(format!("stream event of unknown type {other}")),
        }
    }
    Err("the stream ended before message_stop".to_owned())
}
