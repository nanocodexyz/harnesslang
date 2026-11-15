#[path = "../json.rs"]
#[allow(dead_code)]
mod json;

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use json::Value;

const RETRIES_MAX: u64 = 4;
const CHALLENGES_MAX: u64 = 3;
const AMOUNT_MAX: u64 = 1000;
const FREE_PPM: u64 = 100_000;
const FOREIGN_METHOD_PPM: u64 = 200_000;
const EXPIRED_PPM: u64 = 100_000;
const PROOF_PPM: u64 = 50_000;
const RECEIPT_PPM: u64 = 500_000;
const REUSE_ID_PPM: u64 = 100_000;
const PPM: u64 = 1_000_000;
const FATES: u64 = 5;
const REALM: &str = "pay.sim";
const FOREIGN_REALM: &str = "other.sim";
const METHOD: &str = "tempo";
const FOREIGN_METHOD: &str = "card";
const INTENT: &str = "charge";
const EXPIRED: &str = "2001-01-01T00:00:00Z";
const RECEIPT_TIME: &str = "2026-11-14T00:00:00Z";
const SCHEME: &str = "Payment ";
const TOOK_STATUSES: [u16; 2] = [200, 201];
const REFUSED_STATUSES: [u16; 3] = [400, 403, 409];
const SERVER_STATUSES: [u16; 2] = [500, 503];
const PAYMENT_REQUIRED: u16 = 402;
const HEADER_LINES_MAX: usize = 128;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;
const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
const SPLITMIX_MIX_1: u64 = 0xbf58_476d_1ce4_e5b9;
const SPLITMIX_MIX_2: u64 = 0x94d0_49bb_1331_11eb;
const BASE64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const USAGE: &str = "usage: mpp-server serve PORT_FILE LEDGER SEED [MUTATION] | mpp-server judge LEDGER PAYER_LEDGER SPEND_CAP [MUTATION]";
const MUTATIONS: [&str; 2] = ["none", "receipt_on_refusal"];

struct Rng(u64);

impl Rng {
    fn derive(seed: u64, label: &str) -> Self {
        Self(fnv(fnv(FNV_OFFSET, &seed.to_le_bytes()), label.as_bytes()))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(SPLITMIX_GAMMA);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(SPLITMIX_MIX_1);
        z = (z ^ (z >> 27)).wrapping_mul(SPLITMIX_MIX_2);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "a draw below zero has no outcome");
        self.next() % bound
    }

    fn range(&mut self, low: u64, high: u64) -> u64 {
        assert!(low <= high, "a range runs upward");
        low + self.below(high - low + 1)
    }

    fn chance(&mut self, ppm: u64) -> bool {
        self.below(PPM) < ppm
    }

    fn pick(&mut self, length: usize) -> usize {
        usize::try_from(self.below(u64::try_from(length).expect("a length fits in u64")))
            .expect("an index fits in usize")
    }
}

fn fnv(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn base64_url(bytes: &[u8]) -> String {
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

fn base64_url_decode(text: &str) -> Option<Vec<u8>> {
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

#[derive(Clone, Debug)]
struct Offer {
    id: String,
    method: &'static str,
    realm: &'static str,
    amount: u64,
    expired: bool,
}

impl Offer {
    fn payable(&self) -> bool {
        self.method == METHOD && !self.expired && !self.proof()
    }

    fn proof(&self) -> bool {
        self.amount == 0 && self.realm != REALM
    }

    fn request(&self) -> String {
        let request = Value::object([
            ("amount", Value::String(self.amount.to_string())),
            ("currency", Value::text("usd")),
            ("nonce", Value::text(&self.id)),
        ]);
        base64_url(request.encode().as_bytes())
    }

    fn header(&self) -> String {
        let mut value = format!(
            "Payment id=\"{}\", realm=\"{}\", method=\"{}\", intent=\"{INTENT}\", request=\"{}\"",
            self.id,
            self.realm,
            self.method,
            self.request()
        );
        if self.expired {
            value.push_str(&format!(", expires=\"{EXPIRED}\""));
        }
        value
    }

    fn json(&self) -> Value {
        Value::object([
            ("id", Value::text(&self.id)),
            ("method", Value::text(self.method)),
            ("realm", Value::text(self.realm)),
            ("amount", Value::number(self.amount)),
            ("expired", Value::Bool(self.expired)),
            ("payable", Value::Bool(self.payable())),
        ])
    }
}

#[derive(Clone, Debug)]
enum Fate {
    Took { status: u16, receipt: bool },
    Again { receipt: bool, offers: Vec<Offer> },
    Refused { status: u16, receipt: bool },
    Server { status: u16, receipt: bool },
    Network,
}

impl Fate {
    const fn name(&self) -> &'static str {
        match self {
            Self::Took { .. } => "took",
            Self::Again { .. } => "again",
            Self::Refused { .. } => "refused",
            Self::Server { .. } => "server",
            Self::Network => "network",
        }
    }

    const fn settlement(&self, receipt_on_refusal: bool) -> &'static str {
        match self {
            Self::Took { .. }
            | Self::Again { receipt: true, .. }
            | Self::Server { receipt: true, .. } => "commit",
            Self::Refused { receipt, .. } => {
                if *receipt && !receipt_on_refusal {
                    "commit"
                } else {
                    "rollback"
                }
            }
            Self::Again { receipt: false, .. } => "rollback",
            Self::Server { receipt: false, .. } | Self::Network => "abandon",
        }
    }
}

struct Script {
    first: Option<Vec<Offer>>,
    fates: Vec<Fate>,
}

fn offers(rng: &mut Rng, next: &mut u64, prefix: &str, last: &[Offer]) -> Vec<Offer> {
    (0..rng.range(1, CHALLENGES_MAX))
        .map(|_| {
            if !last.is_empty() && rng.chance(REUSE_ID_PPM) {
                return last[rng.pick(last.len())].clone();
            }
            *next += 1;
            let proof = rng.chance(PROOF_PPM);
            Offer {
                id: format!("{prefix}c{next}"),
                method: if rng.chance(FOREIGN_METHOD_PPM) {
                    FOREIGN_METHOD
                } else {
                    METHOD
                },
                realm: if proof { FOREIGN_REALM } else { REALM },
                amount: if proof { 0 } else { rng.range(1, AMOUNT_MAX) },
                expired: rng.chance(EXPIRED_PPM),
            }
        })
        .collect()
}

impl Script {
    fn of(seed: u64, resource: &str) -> Self {
        let mut rng = Rng::derive(seed, resource);
        let mut next = 0;
        let prefix = format!("{resource}-");
        let retries = rng.range(1, RETRIES_MAX);
        let first = (!rng.chance(FREE_PPM)).then(|| offers(&mut rng, &mut next, &prefix, &[]));
        let mut last = first.clone().unwrap_or_default();
        let fates = (0..retries)
            .map(|_| {
                let receipt = rng.chance(RECEIPT_PPM);
                match rng.below(FATES) {
                    0 => Fate::Took {
                        status: TOOK_STATUSES[rng.pick(TOOK_STATUSES.len())],
                        receipt,
                    },
                    1 => {
                        let fresh = offers(&mut rng, &mut next, &prefix, &last);
                        last.clone_from(&fresh);
                        Fate::Again {
                            receipt,
                            offers: fresh,
                        }
                    }
                    2 => Fate::Refused {
                        status: REFUSED_STATUSES[rng.pick(REFUSED_STATUSES.len())],
                        receipt,
                    },
                    3 => Fate::Server {
                        status: SERVER_STATUSES[rng.pick(SERVER_STATUSES.len())],
                        receipt,
                    },
                    _ => Fate::Network,
                }
            })
            .collect();
        Self { first, fates }
    }

    fn all_offers(&self) -> Vec<&Offer> {
        self.first
            .iter()
            .flatten()
            .chain(self.fates.iter().flat_map(|fate| match fate {
                Fate::Again { offers, .. } => offers.as_slice(),
                _ => &[],
            }))
            .collect()
    }
}

struct Resource {
    script: Script,
    presented: Vec<String>,
    answers: BTreeMap<String, Answer>,
}

#[derive(Clone)]
struct Answer {
    status: Option<u16>,
    offers: Vec<Offer>,
    receipt: Option<String>,
}

struct Server {
    seed: u64,
    receipt_on_refusal: bool,
    ledger: std::fs::File,
    resources: BTreeMap<String, Resource>,
}

impl Server {
    fn log(&mut self, entry: Value) {
        let mut text = entry.encode();
        text.push('\n');
        self.ledger
            .write_all(text.as_bytes())
            .expect("append to the server ledger");
        self.ledger.sync_data().expect("sync the server ledger");
    }

    fn resource(&mut self, name: &str) -> &mut Resource {
        if !self.resources.contains_key(name) {
            let script = Script::of(self.seed, name);
            let offered: Vec<Value> = script
                .all_offers()
                .iter()
                .map(|offer| offer.json())
                .collect();
            let fates: Vec<Value> = script
                .fates
                .iter()
                .map(|fate| Value::text(fate.name()))
                .collect();
            self.log(Value::object([
                ("kind", Value::text("offered")),
                ("resource", Value::text(name)),
                ("free", Value::Bool(script.first.is_none())),
                ("offers", Value::Array(offered)),
                ("fates", Value::Array(fates)),
            ]));
            self.resources.insert(
                name.to_owned(),
                Resource {
                    script,
                    presented: Vec::new(),
                    answers: BTreeMap::new(),
                },
            );
        }
        self.resources
            .get_mut(name)
            .expect("the resource was just inserted")
    }

    fn answer(&mut self, name: &str, credential: Option<&str>) -> Answer {
        let Some(encoded) = credential else {
            let resource = self.resource(name);
            return match &resource.script.first {
                None => Answer {
                    status: Some(200),
                    offers: Vec::new(),
                    receipt: None,
                },
                Some(offers) => Answer {
                    status: Some(PAYMENT_REQUIRED),
                    offers: offers.clone(),
                    receipt: None,
                },
            };
        };
        let decoded = base64_url_decode(encoded)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| Value::decode(&text).ok());
        let id = decoded
            .as_ref()
            .and_then(|credential| credential.get("challenge")?.get("id")?.as_str())
            .unwrap_or_default()
            .to_owned();
        let receipt_on_refusal = self.receipt_on_refusal;
        let resource = self.resource(name);
        if let Some(answer) = resource.answers.get(&id) {
            let answer = answer.clone();
            self.log(Value::object([
                ("kind", Value::text("presented")),
                ("resource", Value::text(name)),
                ("challenge", Value::String(id)),
                ("replay", Value::Bool(true)),
            ]));
            return answer;
        }
        let attempt = resource.presented.len();
        resource.presented.push(id.clone());
        let fate = resource
            .script
            .fates
            .get(attempt)
            .cloned()
            .unwrap_or(Fate::Network);
        let with = |receipt: bool| receipt.then(|| receipt_for(&id));
        let answer = match &fate {
            Fate::Took { status, receipt } => Answer {
                status: Some(*status),
                offers: Vec::new(),
                receipt: with(*receipt),
            },
            Fate::Again { receipt, offers } => Answer {
                status: Some(PAYMENT_REQUIRED),
                offers: offers.clone(),
                receipt: with(*receipt),
            },
            Fate::Refused { status, receipt } => Answer {
                status: Some(*status),
                offers: Vec::new(),
                receipt: with(*receipt || receipt_on_refusal),
            },
            Fate::Server { status, receipt } => Answer {
                status: Some(*status),
                offers: Vec::new(),
                receipt: with(*receipt),
            },
            Fate::Network => Answer {
                status: None,
                offers: Vec::new(),
                receipt: None,
            },
        };
        resource.answers.insert(id.clone(), answer.clone());
        let source = decoded
            .as_ref()
            .and_then(|credential| credential.get("source")?.as_str())
            .unwrap_or_default()
            .to_owned();
        self.log(Value::object([
            ("kind", Value::text("presented")),
            ("resource", Value::text(name)),
            ("challenge", Value::String(id)),
            ("replay", Value::Bool(false)),
            ("source", Value::String(source)),
            ("fate", Value::text(fate.name())),
            (
                "expected",
                Value::text(fate.settlement(self.receipt_on_refusal)),
            ),
        ]));
        answer
    }
}

fn receipt_for(id: &str) -> String {
    let receipt = Value::object([
        ("status", Value::text("success")),
        ("method", Value::text(METHOD)),
        ("timestamp", Value::text(RECEIPT_TIME)),
        ("reference", Value::String(format!("0xref{id}"))),
    ]);
    base64_url(receipt.encode().as_bytes())
}

fn read_head(stream: &TcpStream) -> Option<(String, Option<String>)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let path = line.split_whitespace().nth(1)?.to_owned();
    let mut credential = None;
    for _ in 0..HEADER_LINES_MAX {
        line.clear();
        reader.read_line(&mut line).ok()?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            return Some((path, credential));
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("authorization")
        {
            credential = value.trim().strip_prefix(SCHEME).map(str::to_owned);
        }
    }
    None
}

fn serve(mut stream: TcpStream, server: &Mutex<Server>) {
    let Some((path, credential)) = read_head(&stream) else {
        return;
    };
    let name = path.trim_start_matches('/').replace('/', "-");
    let answer = server
        .lock()
        .expect("the server state is never poisoned")
        .answer(&name, credential.as_deref());
    let Some(status) = answer.status else {
        return;
    };
    let body = format!("{name}: status {status}");
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        if status < 300 { "OK" } else { "Payment" },
        body.len()
    );
    for offer in &answer.offers {
        head.push_str(&format!("WWW-Authenticate: {}\r\n", offer.header()));
    }
    if let Some(receipt) = &answer.receipt {
        head.push_str(&format!("Payment-Receipt: {receipt}\r\n"));
    }
    head.push_str("\r\n");
    head.push_str(&body);
    let _ = stream.write_all(head.as_bytes());
}

fn mutation(name: Option<&String>) -> bool {
    match name.map(String::as_str) {
        None | Some("none") => false,
        Some("receipt_on_refusal") => true,
        Some(other) => {
            eprintln!("mpp-server: unknown mutation {other} (one of {MUTATIONS:?})");
            std::process::exit(2)
        }
    }
}

fn lines(path: &str) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| Value::decode(line).ok())
        .collect()
}

fn text<'v>(value: &'v Value, key: &str) -> &'v str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn judge(server: &[Value], payer: &[Value], cap: u64) -> Result<String, String> {
    let offers: BTreeMap<String, Value> = server
        .iter()
        .filter(|entry| text(entry, "kind") == "offered")
        .flat_map(|entry| {
            entry
                .get("offers")
                .and_then(Value::as_array)
                .unwrap_or(&[])
                .to_vec()
        })
        .map(|offer| (text(&offer, "id").to_owned(), offer))
        .collect();
    let presented: Vec<&Value> = server
        .iter()
        .filter(|entry| {
            text(entry, "kind") == "presented" && entry.get("replay") == Some(&Value::Bool(false))
        })
        .collect();
    let paid: Vec<&Value> = payer
        .iter()
        .filter(|entry| text(entry, "event") == "paid")
        .collect();
    let mut total = 0u64;
    let mut seen: Vec<&str> = Vec::new();
    for entry in &paid {
        let challenge = entry
            .get("challenge")
            .map_or("", |challenge| text(challenge, "id"));
        if seen.contains(&challenge) {
            return Err(format!("the payer paid challenge {challenge} twice"));
        }
        seen.push(challenge);
        let Some(offer) = offers.get(challenge) else {
            return Err(format!(
                "the payer paid {challenge}, which no server offered"
            ));
        };
        if offer.get("payable") != Some(&Value::Bool(true)) {
            return Err(format!(
                "the payer paid challenge {challenge}, which it must not pay: {}",
                offer.encode()
            ));
        }
        let amount: u64 = text(entry, "amount")
            .parse()
            .map_err(|_| format!("payment {challenge} names no amount"))?;
        if Some(amount) != offer.get("amount").and_then(Value::as_u64) {
            return Err(format!(
                "the payer paid {amount} for {challenge}, which asked {}",
                offer.encode()
            ));
        }
        total += amount;
        let settlements: Vec<&str> = payer
            .iter()
            .filter(|settled| {
                text(settled, "event") == "settled" && text(settled, "challenge") == challenge
            })
            .map(|settled| text(settled, "settled"))
            .collect();
        let shown: Vec<&&Value> = presented
            .iter()
            .filter(|shown| text(shown, "challenge") == challenge)
            .collect();
        if shown.len() != 1 {
            return Err(format!(
                "the credential for {challenge} reached the server {} times where it was paid once",
                shown.len()
            ));
        }
        if settlements.len() != 1 {
            return Err(format!(
                "payment for {challenge} was settled {} times ({settlements:?}) where it must be settled once",
                settlements.len()
            ));
        }
        let wanted = text(shown[0], "expected");
        if settlements[0] != wanted {
            return Err(format!(
                "payment for {challenge} was settled {} after the server answered {}, where it must be {wanted}",
                settlements[0],
                text(shown[0], "fate")
            ));
        }
        if !text(shown[0], "source").starts_with("did:pkh:eip155:") {
            return Err(format!(
                "the credential for {challenge} names no did:pkh source"
            ));
        }
    }
    for shown in &presented {
        if !seen.contains(&text(shown, "challenge")) {
            return Err(format!(
                "the server took a credential for {}, which the payer never paid",
                text(shown, "challenge")
            ));
        }
    }
    if total > cap {
        return Err(format!("the payer paid {total}, over the spend cap {cap}"));
    }
    let count = |kind: &str| {
        payer
            .iter()
            .filter(|entry| text(entry, "settled") == kind)
            .count()
    };
    Ok(format!(
        "pays={} units={total} commits={} rollbacks={} abandons={} responses={} replays={}",
        paid.len(),
        count("commit"),
        count("rollback"),
        count("abandon"),
        payer
            .iter()
            .filter(|entry| text(entry, "event") == "payment_response")
            .count(),
        server
            .iter()
            .filter(|entry| entry.get("replay") == Some(&Value::Bool(true)))
            .count()
    ))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") if (4..=5).contains(&args.len()) => {
            let seed: u64 = args[3].parse().expect("SEED is a number");
            let ledger = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&args[2])
                .expect("open the server ledger");
            let server = Arc::new(Mutex::new(Server {
                seed,
                receipt_on_refusal: mutation(args.get(4)),
                ledger,
                resources: BTreeMap::new(),
            }));
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
            let port = listener.local_addr().expect("a bound address").port();
            std::fs::write(format!("{}.new", args[1]), port.to_string()).expect("write the port");
            std::fs::rename(format!("{}.new", args[1]), &args[1]).expect("publish the port");
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let server = Arc::clone(&server);
                std::thread::spawn(move || serve(stream, &server));
            }
        }
        Some("judge") if (4..=5).contains(&args.len()) => {
            let cap: u64 = args[3].parse().expect("SPEND_CAP is a number");
            let _ = mutation(args.get(4));
            match judge(&lines(&args[1]), &lines(&args[2]), cap) {
                Ok(stats) => println!("ok {stats}"),
                Err(wrong) => {
                    println!("FAIL {wrong}");
                    std::process::exit(1)
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2)
        }
    }
}
