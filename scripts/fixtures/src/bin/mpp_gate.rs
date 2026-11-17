#[path = "../json.rs"]
#[allow(dead_code)]
mod json;
#[path = "../mpp.rs"]
#[allow(dead_code)]
mod mpp;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use json::Value;

const USAGE: &str = "usage: mpp-gate serve PORT_FILE UPSTREAM_PORT LEDGER AMOUNT | mpp-gate judge LEDGER CHARGE_MAX";
const REALM: &str = "mpp.fixture";
const METHOD: &str = "tempo";
const INTENT: &str = "charge";
const CURRENCY: &str = "0x20c0000000000000000000000000000000000000";
const RECIPIENT: &str = "0x00000000000000000000000000000000000000f1";
const TRANSFER_PATH: &str = "/chain/transfer";
const DROPPED_HEADERS: [&str; 5] = [
    "authorization",
    "x-api-key",
    "connection",
    "host",
    "content-length",
];

struct Gate {
    upstream: u16,
    amount: u64,
    ledger: File,
    next: u64,
    issued: BTreeMap<String, u64>,
    transfers: BTreeMap<String, u64>,
    used: BTreeSet<String>,
}

impl Gate {
    fn record(&mut self, line: &str) {
        self.ledger
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| self.ledger.sync_data())
            .expect("write the ledger");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["serve", port_file, upstream, ledger, amount] => {
            serve(port_file, upstream, ledger, amount)
        }
        ["judge", ledger, charge_max] => judge(ledger, charge_max),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn serve(port_file: &str, upstream: &str, ledger: &str, amount: &str) {
    let ledger = OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger)
        .expect("open the ledger");
    let gate = Arc::new(Mutex::new(Gate {
        upstream: upstream.parse().expect("UPSTREAM_PORT is a port"),
        amount: amount.parse().expect("AMOUNT is a number"),
        ledger,
        next: 0,
        issued: BTreeMap::new(),
        transfers: BTreeMap::new(),
        used: BTreeSet::new(),
    }));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    let port = listener.local_addr().expect("a bound address").port();
    std::fs::write(format!("{port_file}.new"), port.to_string()).expect("write the port");
    std::fs::rename(format!("{port_file}.new"), port_file).expect("publish the port");
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || answer(stream, &gate));
    }
}

fn respond(stream: &mut TcpStream, status: &str, extra: &str, body: &Value) {
    let text = body.encode();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn error(message: &str, code: &str) -> Value {
    Value::object([(
        "error",
        Value::object([
            ("message", Value::text(message)),
            ("type", Value::text(code)),
            ("code", Value::text(code)),
        ]),
    )])
}

fn answer(mut stream: TcpStream, gate: &Mutex<Gate>) {
    let Some(request) = mpp::read_request(&stream) else {
        return;
    };
    if request.path == TRANSFER_PATH {
        transfer(&mut stream, &request, gate);
        return;
    }
    let credential = mpp::header(&request.headers, "authorization")
        .and_then(|value| value.strip_prefix(mpp::SCHEME))
        .and_then(mpp::base64_url_decode)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| Value::decode(&text).ok());
    let id = credential
        .as_ref()
        .and_then(|credential| credential.get("challenge"))
        .and_then(|challenge| challenge.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let upstream = {
        let mut gate = gate.lock().expect("the gate is never poisoned");
        match id {
            Some(id)
                if gate.transfers.get(&id).copied().unwrap_or(0) > 0
                    && !gate.used.contains(&id) =>
            {
                gate.used.insert(id.clone());
                gate.record(&format!("used {id}"));
                Some(gate.upstream)
            }
            _ => {
                gate.next += 1;
                let id = format!("ch-{}-{}", std::process::id(), gate.next);
                let amount = gate.amount;
                gate.issued.insert(id.clone(), amount);
                gate.record(&format!("issued {id} {amount}"));
                let detail = Value::object([
                    ("amount", Value::text(&amount.to_string())),
                    ("currency", Value::text(CURRENCY)),
                    ("recipient", Value::text(RECIPIENT)),
                ]);
                let challenge = format!(
                    "{}id=\"{id}\", realm=\"{REALM}\", method=\"{METHOD}\", intent=\"{INTENT}\", request=\"{}\"",
                    mpp::SCHEME,
                    mpp::base64_url(detail.encode().as_bytes())
                );
                drop(gate);
                eprintln!("mpp-gate: 402 {id}");
                respond(
                    &mut stream,
                    "402 Payment Required",
                    &format!("WWW-Authenticate: {challenge}\r\n"),
                    &error("Payment required", "payment_required"),
                );
                None
            }
        }
    };
    if let Some(port) = upstream {
        forward(stream, &request, port);
    }
}

fn transfer(stream: &mut TcpStream, request: &mpp::Request, gate: &Mutex<Gate>) {
    let body = Value::decode(&String::from_utf8_lossy(&request.body)).ok();
    let field = |name: &str| {
        body.as_ref()
            .and_then(|body| body.get(name))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let (Some(id), Some(amount)) = (field("id"), field("amount")) else {
        respond(
            stream,
            "400 Bad Request",
            "",
            &error("id and amount are required", "bad_transfer"),
        );
        return;
    };
    let mut gate = gate.lock().expect("the gate is never poisoned");
    if gate
        .issued
        .get(&id)
        .is_none_or(|issued| issued.to_string() != amount)
    {
        drop(gate);
        respond(
            stream,
            "400 Bad Request",
            "",
            &error("no such challenge or amount", "bad_transfer"),
        );
        return;
    }
    let next = gate.next;
    let count = gate.transfers.entry(id.clone()).or_insert(0);
    *count += 1;
    let hash = format!("0x{:064x}", (u128::from(next) << 64) | u128::from(*count));
    gate.record(&format!("transfer {id} {amount} {hash}"));
    drop(gate);
    eprintln!("mpp-gate: transfer {id} {amount}");
    respond(
        stream,
        "200 OK",
        "",
        &Value::object([("hash", Value::text(&hash))]),
    );
}

fn forward(mut stream: TcpStream, request: &mpp::Request, port: u16) {
    let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", port)) else {
        respond(
            &mut stream,
            "502 Bad Gateway",
            "",
            &error("upstream is down", "upstream"),
        );
        return;
    };
    let mut head = format!(
        "POST {} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n",
        request.path
    );
    for line in &request.headers {
        let name = line.split_once(':').map_or("", |(name, _)| name.trim());
        if !DROPPED_HEADERS
            .iter()
            .any(|dropped| name.eq_ignore_ascii_case(dropped))
        {
            head.push_str(line);
            head.push_str("\r\n");
        }
    }
    head.push_str(&format!(
        "Authorization: Bearer mock\r\nX-Api-Key: mock\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        request.body.len()
    ));
    if upstream.write_all(head.as_bytes()).is_err() || upstream.write_all(&request.body).is_err() {
        return;
    }
    let _ = std::io::copy(&mut upstream, &mut stream);
}

fn judge(ledger: &str, charge_max: &str) {
    let charge_max: u64 = charge_max.parse().expect("CHARGE_MAX is a number");
    let text = std::fs::read_to_string(ledger).unwrap_or_default();
    let mut issued = 0_u64;
    let mut used = BTreeSet::new();
    let mut transfers: BTreeMap<String, u64> = BTreeMap::new();
    let mut spent = 0_u64;
    let mut over = 0_u64;
    for line in text.lines() {
        let words: Vec<&str> = line.split(' ').collect();
        match words.as_slice() {
            ["issued", _, _] => issued += 1,
            ["used", id] => {
                used.insert((*id).to_owned());
            }
            ["transfer", id, amount, _] => {
                *transfers.entry((*id).to_owned()).or_insert(0) += 1;
                let amount: u64 = amount.parse().unwrap_or(u64::MAX);
                spent = spent.saturating_add(amount);
                if amount > charge_max {
                    over += 1;
                }
            }
            _ => {}
        }
    }
    let twice = transfers.values().filter(|count| **count > 1).count();
    let paid = transfers.len();
    let unused = transfers.keys().filter(|id| !used.contains(*id)).count();
    println!(
        "mpp-gate judge: challenges={issued} paid={paid} transfers={} served_after_payment={} paid_unserved={unused} paid_twice={twice} over_charge={over} spent_units={spent}",
        transfers.values().sum::<u64>(),
        used.len()
    );
    if twice > 0 || over > 0 {
        std::process::exit(1);
    }
}
