use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::json::Value;

pub const FETCH_TOOL: &str = "fetch";
pub const PAYMENT_PARAM: &str = "payment";
pub const REQUIRED_PARAM: &str = "required";
pub const MAX_PARAM: &str = "max";
pub const PAYMENT_REQUIRED_PREFIX: &str = "payment required: ";
pub const METHOD: &str = "tempo";
pub const INTENT_CHARGE: &str = "charge";
pub const INTENT_SESSION: &str = "session";
pub const SCHEME: &str = "Payment";
pub const RECEIPT_HEADER: &str = "payment-receipt";
pub const CHALLENGE_HEADER: &str = "www-authenticate";
pub const PAYMENT_REQUIRED: u16 = 402;
pub const LEDGER_FILE: &str = "payments.jsonl";
pub const IDENTITY_FILE: &str = "wallet.json";
pub const MODERATO_CHAIN_ID: u64 = 42431;
pub const ADDRESS_BYTES: usize = 20;
pub const HEADER_LINES_MAX: usize = 128;
pub const FETCH_BODY_BYTES_MAX: u64 = 1 << 20;
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
pub const BALANCE_ENV: &str = "HL_AGENT_WALLET_BALANCE";
pub const MUTATION_ENV: &str = "HL_AGENT_PAYER_MUTATION";
const PCG_MULTIPLIER: u64 = 6_364_136_223_846_793_005;
const PCG_INCREMENT: u64 = 1_442_695_040_888_963_407;
const CHALLENGE_FIELDS: [&str; 9] = [
    "id",
    "realm",
    "method",
    "intent",
    "request",
    "expires",
    "description",
    "digest",
    "opaque",
];
const HTTP_SUCCESS: std::ops::Range<u16> = 200..300;
const HTTP_SERVER_ERROR: std::ops::Range<u16> = 500..600;
const SECONDS_PER_DAY: u64 = 86_400;
const DAYS_FROM_0000_TO_1970: i64 = 719_468;
const DAYS_PER_ERA: i64 = 146_097;
const BASE64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub enum Ran {
    Ok(String),
    Failed(String),
    Units(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mutation {
    None,
    PayTwice,
    SettleTwice,
    CommitOnRefusal,
}

impl Mutation {
    fn from_env() -> Result<Self, String> {
        match std::env::var(MUTATION_ENV).ok().as_deref() {
            None | Some("" | "none") => Ok(Self::None),
            Some("pay_twice") => Ok(Self::PayTwice),
            Some("settle_twice") => Ok(Self::SettleTwice),
            Some("commit_on_refusal") => Ok(Self::CommitOnRefusal),
            Some(other) => Err(format!(
                "{MUTATION_ENV}={other}: expected none, pay_twice, settle_twice or commit_on_refusal"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    Commit,
    Rollback,
    Abandon,
}

impl Settled {
    const fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Rollback => "rollback",
            Self::Abandon => "abandon",
        }
    }
}

pub struct Identity {
    pub address: String,
    pub chain_id: u64,
    pub balance: u64,
}

impl Identity {
    pub fn did(&self) -> String {
        format!("did:pkh:eip155:{}:{}", self.chain_id, self.address)
    }
}

pub struct Payer {
    pub wallet: String,
    identity: Identity,
    ledger: PathBuf,
    entries: Vec<Value>,
    mutation: Mutation,
}

pub fn base64_url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, byte)| {
            word | (u32::from(*byte) << (16 - 8 * index))
        });
        for index in 0..=chunk.len() {
            let sextet = (word >> (18 - 6 * index)) & 0x3f;
            out.push(char::from(BASE64_URL[sextet as usize]));
        }
    }
    out
}

pub fn base64_url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut word = 0u32;
    let mut bits = 0;
    for character in text.trim_end_matches('=').bytes() {
        let character = match character {
            b'+' => b'-',
            b'/' => b'_',
            other => other,
        };
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

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs()
}

pub fn rfc3339(seconds: u64) -> String {
    let days = i64::try_from(seconds / SECONDS_PER_DAY).expect("days since 1970 fit in i64");
    let rest = seconds % SECONDS_PER_DAY;
    let shifted = days + DAYS_FROM_0000_TO_1970;
    let era = shifted.div_euclid(DAYS_PER_ERA);
    let day_of_era = shifted.rem_euclid(DAYS_PER_ERA);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

pub fn decode_json_url(text: &str) -> Option<Value> {
    let bytes = base64_url_decode(text)?;
    Value::decode(std::str::from_utf8(&bytes).ok()?).ok()
}

pub fn challenge_amount(challenge: &Value) -> Option<u64> {
    let request = decode_json_url(challenge.get("request")?.as_str()?)?;
    request.get("amount")?.as_str()?.parse().ok()
}

fn auth_params(rest: &str) -> Option<Vec<(String, String)>> {
    let bytes = rest.as_bytes();
    let mut at = 0;
    let mut fields = Vec::new();
    for _ in 0..bytes.len() {
        while at < bytes.len() && matches!(bytes[at], b' ' | b',' | b'\t') {
            at += 1;
        }
        if at >= bytes.len() {
            return Some(fields);
        }
        let start = at;
        while at < bytes.len() && bytes[at] != b'=' {
            at += 1;
        }
        let name = rest.get(start..at)?.trim().to_ascii_lowercase();
        at += 1;
        let mut value = Vec::new();
        if bytes.get(at) == Some(&b'"') {
            at += 1;
            let mut closed = false;
            while at < bytes.len() {
                match bytes[at] {
                    b'\\' if at + 1 < bytes.len() => {
                        value.push(bytes[at + 1]);
                        at += 2;
                    }
                    b'"' => {
                        at += 1;
                        closed = true;
                        break;
                    }
                    byte => {
                        value.push(byte);
                        at += 1;
                    }
                }
            }
            if !closed {
                return None;
            }
        } else {
            while at < bytes.len() && bytes[at] != b',' {
                value.push(bytes[at]);
                at += 1;
            }
        }
        fields.push((name, String::from_utf8(value).ok()?.trim().to_owned()));
    }
    Some(fields)
}

pub fn parse_challenge(header: &str) -> Option<Value> {
    let (scheme, rest) = header.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    let fields = auth_params(rest)?;
    let mut challenge = Value::object(CHALLENGE_FIELDS.iter().filter_map(|field| {
        let (_, value) = fields.iter().find(|(name, _)| name == field)?;
        Some((*field, Value::text(value)))
    }));
    challenge.get("id")?;
    if let Some(amount) = challenge_amount(&challenge) {
        challenge.insert("amount", Value::String(amount.to_string()));
    }
    Some(challenge)
}

pub fn required_text(challenges: &[Value]) -> String {
    format!(
        "{PAYMENT_REQUIRED_PREFIX}{}",
        Value::object([("challenges", Value::Array(challenges.to_vec()))]).encode()
    )
}

pub fn echo(challenge: &Value) -> Value {
    Value::object(
        CHALLENGE_FIELDS
            .iter()
            .filter_map(|field| Some((*field, challenge.get(field)?.clone()))),
    )
}

pub struct Response {
    pub status: u16,
    pub challenges: Vec<Value>,
    pub receipt: Option<String>,
    pub body: String,
}

fn split_url(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("{url}: this host fetches only http:// URLs"))?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let authority = if authority.contains(':') {
        authority.to_owned()
    } else {
        format!("{authority}:80")
    };
    Ok((authority, path.to_owned()))
}

pub fn http_get(url: &str, authorization: Option<&str>) -> Result<Response, String> {
    let (authority, path) = split_url(url)?;
    let mut stream =
        TcpStream::connect(&authority).map_err(|error| format!("connect {authority}: {error}"))?;
    stream
        .set_read_timeout(Some(FETCH_TIMEOUT))
        .map_err(|error| error.to_string())?;
    let mut head = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
    if let Some(authorization) = authorization {
        head.push_str(&format!("Authorization: {authorization}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .map_err(|error| format!("send to {authority}: {error}"))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let reset = |detail: String| format!("connection reset after the request was sent: {detail}");
    reader
        .read_line(&mut line)
        .map_err(|error| reset(format!("{authority}: {error}")))?;
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| reset(format!("{authority} answered no status line")))?;
    let mut challenges = Vec::new();
    let mut receipt = None;
    let mut length = None;
    for _ in 0..HEADER_LINES_MAX {
        line.clear();
        reader
            .read_line(&mut line)
            .map_err(|error| reset(format!("{authority}: {error}")))?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            CHALLENGE_HEADER => challenges.extend(parse_challenge(value)),
            RECEIPT_HEADER => receipt = Some(value.to_owned()),
            "content-length" => length = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    let bound = length
        .unwrap_or(FETCH_BODY_BYTES_MAX)
        .min(FETCH_BODY_BYTES_MAX);
    let mut body = Vec::new();
    reader
        .take(bound)
        .read_to_end(&mut body)
        .map_err(|error| reset(format!("{authority}: {error}")))?;
    Ok(Response {
        status,
        challenges,
        receipt,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn new_identity(random: u64) -> Result<Value, String> {
    let mut address = String::from("0x");
    let mut word = random;
    for _ in 0..ADDRESS_BYTES {
        word = word
            .wrapping_mul(PCG_MULTIPLIER)
            .wrapping_add(PCG_INCREMENT);
        address.push_str(&format!("{:02x}", word >> 56));
    }
    let balance = match std::env::var(BALANCE_ENV) {
        Ok(text) => text
            .parse()
            .map_err(|_| format!("{BALANCE_ENV}={text} is not a number of units"))?,
        Err(_) => u64::MAX,
    };
    Ok(Value::object([
        ("address", Value::String(address)),
        ("chain_id", Value::number(MODERATO_CHAIN_ID)),
        ("balance", Value::String(balance.to_string())),
    ]))
}

fn load_identity(state: &Path, random: u64) -> Result<Identity, String> {
    let path = state.join(IDENTITY_FILE);
    let saved = match std::fs::read_to_string(&path) {
        Ok(text) => Value::decode(&text).map_err(|error| format!("{}: {error}", path.display()))?,
        Err(_) => {
            let identity = new_identity(random)?;
            let temporary = state.join(format!("{IDENTITY_FILE}.new"));
            std::fs::write(&temporary, identity.encode())
                .map_err(|error| format!("{}: {error}", temporary.display()))?;
            std::fs::rename(&temporary, &path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            identity
        }
    };
    let text = |name: &str| {
        saved
            .get(name)
            .and_then(Value::as_str)
            .ok_or(format!("{IDENTITY_FILE} has no {name}"))
    };
    Ok(Identity {
        address: text("address")?.to_owned(),
        chain_id: saved
            .get("chain_id")
            .and_then(Value::as_u64)
            .ok_or(format!("{IDENTITY_FILE} has no chain_id"))?,
        balance: text("balance")?
            .parse()
            .map_err(|_| format!("{IDENTITY_FILE}: balance is not a number"))?,
    })
}

fn event(entry: &Value) -> Option<&str> {
    entry.get("event").and_then(Value::as_str)
}

impl Payer {
    pub fn open(wallet: &str, state: &Path, random: u64) -> Result<Self, String> {
        let identity = load_identity(state, random)?;
        let ledger = state.join(LEDGER_FILE);
        let entries = match std::fs::read_to_string(&ledger) {
            Ok(text) => text
                .lines()
                .filter_map(|line| Value::decode(line).ok())
                .collect(),
            Err(_) => Vec::new(),
        };
        Ok(Self {
            wallet: wallet.to_owned(),
            identity,
            ledger,
            entries,
            mutation: Mutation::from_env()?,
        })
    }

    fn append(&mut self, entry: Value) {
        let mut text = entry.encode();
        text.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.ledger)
            .expect("open the payment ledger");
        file.write_all(text.as_bytes())
            .expect("append to the payment ledger");
        file.sync_data().expect("sync the payment ledger");
        self.entries.push(entry);
    }

    fn paid(&self) -> impl Iterator<Item = &Value> {
        self.entries
            .iter()
            .filter(|entry| event(entry) == Some("paid"))
    }

    fn spent(&self) -> u64 {
        self.paid()
            .filter_map(|entry| entry.get("amount")?.as_str()?.parse::<u64>().ok())
            .sum()
    }

    fn already_paid(&self, id: &str) -> bool {
        self.mutation != Mutation::PayTwice
            && self.paid().any(|entry| {
                entry
                    .get("challenge")
                    .and_then(|challenge| challenge.get("id"))
                    .and_then(Value::as_str)
                    == Some(id)
            })
    }

    fn payable(&self, challenge: &Value, max: u64) -> Result<u64, String> {
        let text = |name: &str| challenge.get(name).and_then(Value::as_str);
        let id = text("id").unwrap_or_default();
        if self.already_paid(id) {
            return Err(format!("{id} is already paid"));
        }
        let intent = text("intent");
        if text("method") != Some(METHOD)
            || (intent != Some(INTENT_CHARGE) && intent != Some(INTENT_SESSION))
        {
            return Err(format!(
                "{id} is not {METHOD}.{INTENT_CHARGE} or {METHOD}.{INTENT_SESSION}"
            ));
        }
        if let Some(expires) = text("expires")
            && expires <= rfc3339(now_seconds()).as_str()
        {
            return Err(format!("{id} expired"));
        }
        let amount = challenge_amount(challenge).ok_or_else(|| format!("{id} names no amount"))?;
        if amount == 0 {
            return Err(format!("{id} asks a proof, which this payer does not sign"));
        }
        if amount > max {
            return Err(format!("{id} asks {amount}, over the payment's max {max}"));
        }
        Ok(amount)
    }

    fn challenges_of(required: &str) -> Result<Vec<Value>, String> {
        match required
            .strip_prefix(PAYMENT_REQUIRED_PREFIX)
            .map(Value::decode)
        {
            Some(Ok(decoded)) => decoded
                .get("challenges")
                .and_then(Value::as_array)
                .map(<[Value]>::to_vec)
                .ok_or_else(|| "the payment required names no challenges".to_owned()),
            _ => Err("the payment required is not a challenge list".to_owned()),
        }
    }

    pub fn pay(&mut self, call: &str, input: &Value) -> Ran {
        let required = input
            .get(REQUIRED_PARAM)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(max) = input.get(MAX_PARAM).and_then(Value::as_u64) else {
            return Ran::Failed("the payment names no max".to_owned());
        };
        let challenges = match Self::challenges_of(&required) {
            Ok(challenges) => challenges,
            Err(error) => return Ran::Failed(error),
        };
        let mut refusals = Vec::new();
        for challenge in &challenges {
            let amount = match self.payable(challenge, max) {
                Ok(amount) => amount,
                Err(refusal) => {
                    refusals.push(refusal);
                    continue;
                }
            };
            let spent = self.spent();
            if spent.saturating_add(amount) > self.identity.balance {
                return Ran::Failed(format!(
                    "insufficient balance: the wallet holds {} of {amount}",
                    self.identity.balance.saturating_sub(spent)
                ));
            }
            let id = challenge
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let credential = Value::object([
                ("challenge", echo(challenge)),
                ("source", Value::String(self.identity.did())),
                (
                    "payload",
                    Value::object([
                        ("type", Value::text("transaction")),
                        ("signature", Value::String(format!("0xsim{id}"))),
                    ]),
                ),
            ]);
            self.append(Value::object([
                ("event", Value::text("paid")),
                ("call", Value::text(call)),
                ("required", Value::String(required.clone())),
                ("challenge", challenge.clone()),
                ("amount", Value::String(amount.to_string())),
                ("credential", credential),
            ]));
            return Ran::Units(amount);
        }
        Ran::Failed(format!(
            "No method found for challenges: {}",
            refusals.join("; ")
        ))
    }

    pub fn recover_pay(&self, call: &str) -> Option<u64> {
        self.paid()
            .find(|entry| entry.get("call").and_then(Value::as_str) == Some(call))
            .and_then(|entry| entry.get("amount")?.as_str()?.parse().ok())
    }

    fn settled(&self, id: &str) -> bool {
        self.mutation != Mutation::SettleTwice
            && self.entries.iter().any(|entry| {
                event(entry) == Some("settled")
                    && entry.get("challenge").and_then(Value::as_str) == Some(id)
            })
    }

    fn settle(&mut self, call: &str, id: &str, settled: Settled) {
        if self.settled(id) {
            return;
        }
        self.append(Value::object([
            ("event", Value::text("settled")),
            ("call", Value::text(call)),
            ("challenge", Value::text(id)),
            ("settled", Value::text(settled.name())),
        ]));
    }

    fn credential_for(&self, payment: &str) -> Option<Value> {
        self.paid()
            .filter(|entry| entry.get("required").and_then(Value::as_str) == Some(payment))
            .last()
            .cloned()
    }

    pub fn fetch(&mut self, call: &str, input: &Value) -> Ran {
        let Some(url) = input.get("url").and_then(Value::as_str) else {
            return Ran::Failed(
                "<tool_use_error>missing or invalid url</tool_use_error>".to_owned(),
            );
        };
        let payment = input
            .get(PAYMENT_PARAM)
            .and_then(Value::as_str)
            .unwrap_or_default();
        if payment.is_empty() {
            return match http_get(url, None) {
                Ok(response) => answer(response),
                Err(error) => Ran::Failed(error),
            };
        }
        let Some(paid) = self.credential_for(payment) else {
            return Ran::Failed(format!(
                "{FETCH_TOOL}: no credential was made for this payment"
            ));
        };
        let authorization = paid
            .get("credential")
            .map(|credential| format!("{SCHEME} {}", base64_url(credential.encode().as_bytes())));
        let challenge = paid.get("challenge").cloned().unwrap_or(Value::Null);
        let id = challenge
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let response = match http_get(url, authorization.as_deref()) {
            Ok(response) => response,
            Err(error) => {
                self.settle(call, &id, Settled::Abandon);
                return Ran::Failed(error);
            }
        };
        let has_receipt = response.receipt.is_some();
        let settled = if HTTP_SUCCESS.contains(&response.status) {
            let receipt = response.receipt.as_deref().and_then(decode_json_url);
            self.append(Value::object([
                ("event", Value::text("payment_response")),
                ("call", Value::text(call)),
                ("url", Value::text(url)),
                ("challenge", challenge),
                ("receipt", receipt.unwrap_or(Value::Null)),
                ("amount", paid.get("amount").cloned().unwrap_or(Value::Null)),
                ("funding_currency", Value::Null),
            ]));
            Settled::Commit
        } else if HTTP_SERVER_ERROR.contains(&response.status) {
            if has_receipt {
                Settled::Commit
            } else {
                Settled::Abandon
            }
        } else if has_receipt || self.mutation == Mutation::CommitOnRefusal {
            Settled::Commit
        } else {
            Settled::Rollback
        };
        self.settle(call, &id, settled);
        answer(response)
    }
}

pub fn plain_reply(reply: &Value) -> Option<Value> {
    let first = reply.get("ok")?.as_array()?.first()?;
    if let Some(fields) = first.get("calls").and_then(Value::as_array) {
        return Some(Value::object([("calls", fields.first()?.clone())]));
    }
    let fields = first.get("answer")?.as_array()?;
    Some(Value::object([("answer", fields.first()?.clone())]))
}

fn answer(response: Response) -> Ran {
    if HTTP_SUCCESS.contains(&response.status) {
        return Ran::Ok(response.body);
    }
    if response.status == PAYMENT_REQUIRED && !response.challenges.is_empty() {
        return Ran::Failed(required_text(&response.challenges));
    }
    Ran::Failed(format!("HTTP {}: {}", response.status, response.body))
}
