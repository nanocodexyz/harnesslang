use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;

const BASE64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const HEADER_LINES_MAX: usize = 128;
pub const SCHEME: &str = "Payment ";

pub fn base64_url(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, byte)| {
            word | (u32::from(*byte) << (16 - 8 * index))
        });
        for index in 0..=chunk.len() {
            out.push(char::from(
                BASE64_URL[((word >> (18 - 6 * index)) & 0x3f) as usize],
            ));
        }
    }
    out
}

pub fn base64_url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut word, mut bits) = (0u32, 0);
    for character in text.trim_end_matches('=').bytes() {
        let value = BASE64_URL.iter().position(|known| *known == character)?;
        word = ((word << 6) | u32::try_from(value).ok()?) & 0xff_ffff;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((word >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

pub fn params(challenge: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = challenge.strip_prefix(SCHEME).unwrap_or(challenge);
    while let Some((key, after)) = rest.split_once("=\"") {
        let Some((value, tail)) = after.split_once('"') else {
            break;
        };
        out.push((key.trim_matches([' ', ',']).to_owned(), value.to_owned()));
        rest = tail;
    }
    out
}

pub fn param<'p>(params: &'p [(String, String)], name: &str) -> Option<&'p str> {
    params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

pub struct Request {
    pub path: String,
    pub headers: Vec<String>,
    pub body: Vec<u8>,
}

pub fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let mut headers = Vec::new();
    for _ in 0..HEADER_LINES_MAX {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end().to_owned();
        if line.is_empty() {
            break;
        }
        headers.push(line);
    }
    let length = header(&headers, "content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).ok()?;
    let path = first.split_whitespace().nth(1)?.to_owned();
    Some(Request {
        path,
        headers,
        body,
    })
}

pub fn header<'h>(headers: &'h [String], name: &str) -> Option<&'h str> {
    headers.iter().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

pub fn post(base: &str, path: &str, body: &str) -> Result<(u16, String), String> {
    let address = base
        .strip_prefix("http://")
        .ok_or("the URL is not http://")?;
    let address = address.trim_end_matches('/');
    let mut stream = TcpStream::connect(address).map_err(|error| error.to_string())?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut answer = String::new();
    stream
        .read_to_string(&mut answer)
        .map_err(|error| error.to_string())?;
    let (head, body) = answer
        .split_once("\r\n\r\n")
        .ok_or("the answer has no body")?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or("the answer has no status")?;
    Ok((status, body.to_owned()))
}
