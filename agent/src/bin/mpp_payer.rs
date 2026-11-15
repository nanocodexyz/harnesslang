#[path = "../json.rs"]
#[allow(dead_code)]
mod json;
#[path = "../mpp.rs"]
#[allow(dead_code)]
mod mpp;

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::{SystemTime, UNIX_EPOCH};

use json::Value;

const HEADER_LINES_MAX: usize = 128;
const REQUEST_BYTES_MAX: u64 = 1 << 20;
const USAGE: &str = "usage: mpp-payer PORT_FILE STATE_DIR WALLET";

fn read_request(stream: &TcpStream) -> Option<(String, String)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let path = line.split_whitespace().nth(1)?.to_owned();
    let mut length = 0u64;
    for _ in 0..HEADER_LINES_MAX {
        line.clear();
        reader.read_line(&mut line).ok()?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            let mut body = Vec::new();
            reader
                .take(length.min(REQUEST_BYTES_MAX))
                .read_to_end(&mut body)
                .ok()?;
            return Some((path, String::from_utf8(body).ok()?));
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok()?;
        }
    }
    None
}

fn output(ran: mpp::Ran) -> Value {
    match ran {
        mpp::Ran::Ok(text) => Value::object([("ok", Value::String(text))]),
        mpp::Ran::Units(units) => Value::object([("ok", Value::number(units))]),
        mpp::Ran::Failed(text) => Value::object([("failed", Value::String(text))]),
    }
}

fn answer(payer: &mut mpp::Payer, path: &str, body: &str) -> Result<Value, String> {
    let request =
        Value::decode(body).map_err(|error| format!("the request is not JSON: {error}"))?;
    let tool = request
        .get("tool")
        .and_then(Value::as_str)
        .ok_or("the request names no tool")?
        .to_owned();
    let number = |key: &str| {
        request
            .get(key)
            .and_then(Value::as_u64)
            .ok_or(format!("the request names no {key}"))
    };
    let call = format!("{}/{}", number("turn")?, number("seq")?);
    let input = request.get("input").cloned().unwrap_or(Value::Null);
    let wallet = tool == payer.wallet;
    match path {
        "/tool" if wallet => Ok(output(payer.pay(&call, &input))),
        "/tool" if tool == mpp::FETCH_TOOL => Ok(output(payer.fetch(&call, &input))),
        "/recover" if wallet => Ok(match payer.recover_pay(&call) {
            Some(units) => {
                Value::object([("reply", Value::object([("ok", Value::number(units))]))])
            }
            None => Value::text("not_dispatched"),
        }),
        "/recover" => Ok(Value::text("unknown")),
        "/tool" => Ok(Value::object([(
            "failed",
            Value::String(format!(
                "this payer host serves only {} and {}",
                payer.wallet,
                mpp::FETCH_TOOL
            )),
        )])),
        other => Err(format!("no endpoint {other}")),
    }
}

fn respond(mut stream: TcpStream, status: u16, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        if status == 200 { "OK" } else { "Bad Request" },
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [port_file, state, wallet] = args.as_slice() else {
        eprintln!("{USAGE}");
        std::process::exit(2)
    };
    std::fs::create_dir_all(state).expect("create the payer's state directory");
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970");
    let random = elapsed.as_secs() ^ u64::from(elapsed.subsec_nanos()).rotate_left(u32::BITS);
    let mut payer =
        mpp::Payer::open(wallet, std::path::Path::new(state), random).unwrap_or_else(|error| {
            eprintln!("mpp-payer: {error}");
            std::process::exit(1)
        });
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    let port = listener.local_addr().expect("a bound address").port();
    std::fs::write(format!("{port_file}.new"), port.to_string()).expect("write the port");
    std::fs::rename(format!("{port_file}.new"), port_file).expect("publish the port");
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let Some((path, body)) = read_request(&stream) else {
            continue;
        };
        match answer(&mut payer, &path, &body) {
            Ok(value) => respond(stream, 200, &value.encode()),
            Err(error) => respond(stream, 400, &Value::String(error).encode()),
        }
    }
}
