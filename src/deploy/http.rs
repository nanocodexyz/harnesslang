use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::json::{self, Value};

pub const CONNECT_SECONDS_MAX: u64 = 30;
pub const REQUEST_SECONDS_MAX: u64 = 600;
pub const STREAM_LINE_BYTES_MAX: usize = 1 << 20;
pub const RESPONSE_HEADER_BLOCKS_MAX: usize = 8;
const SCRATCH_MODE: u32 = 0o700;
const BODY_MODE: u32 = 0o600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub fn new(method: Method, url: String) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn json(self, value: &Value) -> Self {
        let mut request = self.header("Content-Type", "application/json");
        request.body = Some(value.encode().into_bytes());
        request
    }

    pub fn bytes(mut self, content_type: &str, bytes: Vec<u8>) -> Self {
        self = self.header("Content-Type", content_type);
        self.body = Some(bytes);
        self
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Result<Value, String> {
        let text = std::str::from_utf8(&self.body)
            .map_err(|_| format!("HTTP {} answered a body that is not UTF-8", self.status))?;
        json::decode(text).map_err(|error| {
            format!(
                "HTTP {} answered a body that is not JSON ({error}): {}",
                self.status,
                text.chars().take(200).collect::<String>()
            )
        })
    }
}

pub struct Client {
    pub certificate: Option<String>,
    scratch: PathBuf,
    sequence: std::sync::atomic::AtomicU64,
}

pub enum Flow {
    More,
    Stop,
}

impl Client {
    pub fn new(certificate: Option<String>, scratch: PathBuf) -> Self {
        Self {
            certificate,
            scratch,
            sequence: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn config(&self, request: &Request) -> Result<String, String> {
        let mut config = String::new();
        line(&mut config, "url", &request.url)?;
        line(&mut config, "request", request.method.name())?;
        for (name, value) in &request.headers {
            line(&mut config, "header", &format!("{name}: {value}"))?;
        }
        if let Some(certificate) = &self.certificate {
            line(&mut config, "cacert", certificate)?;
        }
        line(
            &mut config,
            "connect-timeout",
            &CONNECT_SECONDS_MAX.to_string(),
        )?;
        config.push_str("silent\nshow-error\n");
        Ok(config)
    }

    fn files(&self) -> Result<(PathBuf, PathBuf, PathBuf), String> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(SCRATCH_MODE)
            .create(&self.scratch)
            .map_err(|error| format!("cannot create {}: {error}", self.scratch.display()))?;
        let sequence = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let named = |kind: &str| self.scratch.join(format!("{sequence}.{kind}"));
        Ok((named("request"), named("headers"), named("response")))
    }

    pub fn send(&self, request: &Request) -> Result<Response, String> {
        let (body_path, headers_path, response_path) = self.files()?;
        let mut config = self.config(request)?;
        if let Some(body) = &request.body {
            write_private(&body_path, body)?;
            line(
                &mut config,
                "data-binary",
                &format!("@{}", body_path.display()),
            )?;
        }
        line(
            &mut config,
            "dump-header",
            &headers_path.display().to_string(),
        )?;
        line(&mut config, "output", &response_path.display().to_string())?;
        line(&mut config, "max-time", &REQUEST_SECONDS_MAX.to_string())?;
        line(&mut config, "write-out", "%{http_code}")?;
        let output = run_curl(&config)?;
        let read = (|| {
            let status_text = String::from_utf8_lossy(&output).trim().to_owned();
            let status: u16 = status_text
                .parse()
                .map_err(|_| format!("curl wrote no HTTP status: {status_text}"))?;
            let headers_text = std::fs::read_to_string(&headers_path).unwrap_or_default();
            let body = std::fs::read(&response_path).unwrap_or_default();
            Ok(Response {
                status,
                headers: parse_headers(&headers_text)?,
                body,
            })
        })();
        for path in [&body_path, &headers_path, &response_path] {
            remove_scratch(path)?;
        }
        read
    }

    pub fn stream(
        &self,
        request: &Request,
        lines_max: u64,
        on_line: &mut dyn FnMut(&str) -> Flow,
    ) -> Result<u64, String> {
        let mut config = self.config(request)?;
        config.push_str("no-buffer\nfail-with-body\n");
        let mut child = curl()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot run curl: {error}"))?;
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(config.as_bytes())
            .map_err(|error| format!("cannot configure curl: {error}"))?;
        let mut reader = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut lines = 0;
        let mut seen = Vec::new();
        let mut stopped = false;
        while lines < lines_max {
            seen.clear();
            let read = (&mut reader)
                .take(u64::try_from(STREAM_LINE_BYTES_MAX).expect("fits u64") + 1)
                .read_until(b'\n', &mut seen)
                .map_err(|error| format!("cannot read curl's output: {error}"))?;
            if read == 0 {
                break;
            }
            if seen.len() > STREAM_LINE_BYTES_MAX {
                stop_child(&mut child)?;
                return Err(format!(
                    "a streamed line is longer than stream_line_bytes_max ({STREAM_LINE_BYTES_MAX})"
                ));
            }
            let text = String::from_utf8_lossy(&seen);
            let text = text.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                continue;
            }
            lines += 1;
            if let Flow::Stop = on_line(text) {
                stopped = true;
                break;
            }
        }
        if stopped || lines == lines_max {
            stop_child(&mut child)?;
            return Ok(lines);
        }
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("stderr is piped")
            .read_to_string(&mut stderr)
            .map_err(|error| format!("cannot read curl's errors: {error}"))?;
        let status = child
            .wait()
            .map_err(|error| format!("cannot wait for curl: {error}"))?;
        if status.success() {
            Ok(lines)
        } else {
            Err(format!("curl failed ({status}): {}", stderr.trim()))
        }
    }
}

fn stop_child(child: &mut std::process::Child) -> Result<(), String> {
    child
        .kill()
        .map_err(|error| format!("cannot stop curl: {error}"))?;
    child
        .wait()
        .map(drop)
        .map_err(|error| format!("cannot wait for curl: {error}"))
}

fn remove_scratch(path: &PathBuf) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

fn curl() -> Command {
    let mut command = Command::new("curl");
    command.args(["--config", "-"]);
    command
}

fn run_curl(config: &str) -> Result<Vec<u8>, String> {
    let mut child = curl()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run curl: {error}"))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(config.as_bytes())
        .map_err(|error| format!("cannot configure curl: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot wait for curl: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "curl failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn line(config: &mut String, option: &str, value: &str) -> Result<(), String> {
    if value.contains(['\n', '\r']) {
        return Err(format!("a value for curl's {option} holds a line break"));
    }
    config.push_str(option);
    config.push_str(" = \"");
    for character in value.chars() {
        match character {
            '"' => config.push_str("\\\""),
            '\\' => config.push_str("\\\\"),
            '\t' => config.push_str("\\t"),
            other => config.push(other),
        }
    }
    config.push_str("\"\n");
    Ok(())
}

fn parse_headers(text: &str) -> Result<Vec<(String, String)>, String> {
    let blocks: Vec<&str> = text
        .split("\r\n\r\n")
        .filter(|block| !block.trim().is_empty())
        .collect();
    if blocks.len() > RESPONSE_HEADER_BLOCKS_MAX {
        return Err(format!(
            "the response has more than response_header_blocks_max ({RESPONSE_HEADER_BLOCKS_MAX}) header blocks"
        ));
    }
    let Some(last) = blocks.last() else {
        return Ok(Vec::new());
    };
    Ok(last
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect())
}

pub fn query_escape(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn write_private(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(BODY_MODE)
        .open(path)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}
