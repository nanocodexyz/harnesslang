use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const STREAM_IDLE_SECONDS_MAX: u64 = 300;
pub const STREAM_IDLE_BYTES_PER_SECOND: u64 = 1;
pub const CURL_TIMED_OUT: i32 = 28;
pub const RESPONSE_BYTES_MAX: usize = 32 * 1024 * 1024;
pub const HTTP_PORT: u16 = 80;
pub const READ_CHUNK_BYTES: usize = 8192;
pub const MILLISECONDS_PER_SECOND: u64 = 1000;
pub const STOP_POLL: Duration = Duration::from_millis(50);
const HEADER_END: &[u8] = b"\r\n\r\n";

pub struct Post<'a> {
    pub base_url: &'a str,
    pub path: &'a str,
    pub headers: &'a [(&'a str, String)],
    pub body: &'a str,
    pub scratch: &'a Path,
    pub partial: &'a Path,
    pub stop: &'a dyn Fn() -> bool,
}

pub enum Sent {
    Done(u16, String, u64),
    Stopped,
}

pub fn post(request: &Post<'_>) -> Result<Sent, String> {
    if let Some(rest) = request.base_url.strip_prefix("http://") {
        std::fs::write(request.scratch.join("request.json"), request.body)
            .map_err(|error| format!("write request: {error}"))?;
        return plain(rest, request);
    }
    curl(request)
}

fn curl(request: &Post<'_>) -> Result<Sent, String> {
    let body = request.scratch.join("request.json");
    std::fs::write(&body, request.body).map_err(|error| format!("write request: {error}"))?;
    let headers = request.scratch.join("headers.txt");
    let _ = std::fs::remove_file(&headers);
    let mut config = format!(
        "url = \"{}{}\"\n",
        request.base_url.trim_end_matches('/'),
        request.path
    );
    for (name, value) in request.headers {
        config.push_str(&format!("header = \"{name}: {value}\"\n"));
    }
    config.push_str(&format!(
        "data-binary = \"@{}\"\noutput = \"{}\"\ndump-header = \"{}\"\n",
        body.display(),
        request.partial.display(),
        headers.display()
    ));
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--speed-limit",
            &STREAM_IDLE_BYTES_PER_SECOND.to_string(),
            "--speed-time",
            &STREAM_IDLE_SECONDS_MAX.to_string(),
            "-w",
            "%{http_code}",
            "-K",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn curl: {error}"))?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(config.as_bytes())
        .map_err(|error| format!("curl stdin: {error}"))?;
    loop {
        if child
            .try_wait()
            .map_err(|error| format!("curl: {error}"))?
            .is_some()
        {
            break;
        }
        if (request.stop)() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Sent::Stopped);
        }
        std::thread::sleep(STOP_POLL);
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("curl: {error}"))?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let reason = if code == CURL_TIMED_OUT {
            format!(
                " (the stream stalled for STREAM_IDLE_SECONDS_MAX, {STREAM_IDLE_SECONDS_MAX} s)"
            )
        } else {
            String::new()
        };
        return Err(format!(
            "curl exited {code}{reason}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let status: u16 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "curl printed no HTTP status".to_owned())?;
    let size = std::fs::metadata(request.partial)
        .map_err(|error| format!("read response: {error}"))?
        .len();
    if size > RESPONSE_BYTES_MAX as u64 {
        return Err(format!(
            "response exceeds RESPONSE_BYTES_MAX ({RESPONSE_BYTES_MAX} bytes)"
        ));
    }
    let text = std::fs::read_to_string(request.partial)
        .map_err(|error| format!("read response: {error}"))?;
    let head = std::fs::read_to_string(&headers).unwrap_or_default();
    Ok(Sent::Done(status, text, retry_after_ms(&head)))
}

pub fn retry_after_ms(head: &str) -> u64 {
    let header = |name: &str| {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    if let Some(ms) = header("retry-after-ms").and_then(|value| value.parse::<u64>().ok()) {
        return ms;
    }
    header("retry-after")
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(0, |seconds| seconds.saturating_mul(MILLISECONDS_PER_SECOND))
}

fn plain(rest: &str, request: &Post<'_>) -> Result<Sent, String> {
    let (authority, prefix) = rest
        .split_once('/')
        .map_or((rest, ""), |(authority, path)| (authority, path));
    let path = format!("/{}{}", prefix.trim_end_matches('/'), request.path).replace("//", "/");
    let address = if authority.contains(':') {
        authority.to_owned()
    } else {
        format!("{authority}:{HTTP_PORT}")
    };
    let mut stream =
        TcpStream::connect(&address).map_err(|error| format!("connect {address}: {error}"))?;
    stream
        .set_read_timeout(Some(STOP_POLL))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(STREAM_IDLE_SECONDS_MAX)))
        .map_err(|error| error.to_string())?;
    let mut head = format!("POST {path} HTTP/1.1\r\nHost: {authority}\r\n");
    for (name, value) in request.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        request.body.len()
    ));
    stream
        .write_all(head.as_bytes())
        .map_err(|error| format!("send: {error}"))?;
    stream
        .write_all(request.body.as_bytes())
        .map_err(|error| format!("send: {error}"))?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(request.partial)
        .map_err(|error| format!("{}: {error}", request.partial.display()))?;
    let mut response = Vec::new();
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    let mut heard = Instant::now();
    loop {
        if (request.stop)() {
            return Ok(Sent::Stopped);
        }
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if heard.elapsed() >= Duration::from_secs(STREAM_IDLE_SECONDS_MAX) {
                    return Err(format!(
                        "receive: the stream stalled for STREAM_IDLE_SECONDS_MAX, {STREAM_IDLE_SECONDS_MAX} s"
                    ));
                }
                continue;
            }
            Err(error) => return Err(format!("receive: {error}")),
        };
        if read == 0 {
            break;
        }
        heard = Instant::now();
        if response.len() + read > RESPONSE_BYTES_MAX {
            return Err(format!(
                "response exceeds RESPONSE_BYTES_MAX ({RESPONSE_BYTES_MAX} bytes)"
            ));
        }
        response.extend_from_slice(&chunk[..read]);
        file.write_all(&chunk[..read])
            .map_err(|error| format!("{}: {error}", request.partial.display()))?;
    }
    let (status, body, retry_after) = parse(&response)?;
    Ok(Sent::Done(status, body, retry_after))
}

fn parse(response: &[u8]) -> Result<(u16, String, u64), String> {
    let end = response
        .windows(HEADER_END.len())
        .position(|window| window == HEADER_END)
        .ok_or("response has no header end")?;
    let head = String::from_utf8_lossy(&response[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("bad status line in {head}"))?;
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_ascii_lowercase())
        })
    };
    let raw = &response[end + HEADER_END.len()..];
    let body = if header("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
        dechunk(raw)?
    } else if let Some(length) = header("content-length") {
        let length: usize = length
            .parse()
            .map_err(|_| format!("bad content-length {length}"))?;
        raw.get(..length)
            .ok_or("response shorter than its content-length")?
            .to_vec()
    } else {
        raw.to_vec()
    };
    Ok((
        status,
        String::from_utf8(body).map_err(|error| format!("response body: {error}"))?,
        retry_after_ms(&head),
    ))
}

fn dechunk(mut raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = Vec::with_capacity(raw.len());
    loop {
        let line_end = raw
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("chunk without a size line")?;
        let size_text = String::from_utf8_lossy(&raw[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| format!("bad chunk size {size_text}"))?;
        raw = &raw[line_end + 2..];
        if size == 0 {
            return Ok(body);
        }
        let chunk = raw.get(..size).ok_or("chunk shorter than its size")?;
        body.extend_from_slice(chunk);
        raw = raw.get(size + 2..).ok_or("chunk without its line end")?;
    }
}
