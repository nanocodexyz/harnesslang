use std::collections::BTreeMap;
use std::path::Path;

use nanocode_mpp::protocol::{Challenge, Credential, Receipt};
use nanocode_turnlog::Record;
use serde_json::Value;

use crate::journal::{self, Lines};

const PAYMENT_REQUIRED_PREFIX: &str = "payment required: ";
const PAYMENT_PARAM: &str = "payment";
const REQUIRED_PARAM: &str = "required";
const MAX_PARAM: &str = "max";
const DID_PREFIX: &str = "did:pkh:eip155:";

#[derive(Default)]
pub struct Counts {
    wallet_calls: u64,
    challenges: u64,
    credentials: u64,
    retries: u64,
    responses: u64,
    receipts: u64,
    settled: u64,
}

impl Counts {
    pub fn records(&self) -> u64 {
        self.wallet_calls * 2 + self.retries + self.credentials + self.responses + self.settled
    }
}

impl std::fmt::Display for Counts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "mpp_records={} wallet_calls={} challenges={} credentials={} retries={} responses={} receipts={} settled={}",
            self.records(),
            self.wallet_calls,
            self.challenges,
            self.credentials,
            self.retries,
            self.responses,
            self.receipts,
            self.settled
        )
    }
}

struct Call {
    input: Value,
    position: usize,
}

fn challenge(json: &Value) -> Result<(String, Option<u128>), String> {
    let id = json["id"].as_str().unwrap_or_default().to_owned();
    let parsed: Challenge = serde_json::from_value(json.clone())
        .map_err(|error| format!("challenge {id}: nanocode's Challenge refuses it: {error}"))?;
    let amount = parsed.amount();
    if let Some(stated) = json.get("amount") {
        let stated: Option<u128> = stated.as_str().and_then(|text| text.parse().ok());
        if stated != amount {
            return Err(format!(
                "challenge {id}: the journal states amount {stated:?}, nanocode reads {amount:?} from its request"
            ));
        }
    }
    Ok((id, amount))
}

fn challenges(required: &str) -> Result<Vec<(String, Option<u128>)>, String> {
    let list: Value = required
        .strip_prefix(PAYMENT_REQUIRED_PREFIX)
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or_else(|| format!("a wallet call's required is no challenge list: {required}"))?;
    list["challenges"]
        .as_array()
        .ok_or("a challenge list without challenges")?
        .iter()
        .map(challenge)
        .collect()
}

fn ledger(path: &Path) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("{}: cannot read: {error}", path.display()))?;
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(|error| format!("{}: {error}", path.display())))
        .collect()
}

fn wallet_input(input: &Value) -> Option<(&str, u64)> {
    let object = input.as_object()?;
    if object.len() != 2 {
        return None;
    }
    let required = object.get(REQUIRED_PARAM)?.as_str()?;
    required.starts_with(PAYMENT_REQUIRED_PREFIX).then_some(())?;
    Some((required, object.get(MAX_PARAM)?.as_u64()?))
}

pub fn check(journal_path: &Path, ledger_path: &Path) -> Result<Counts, String> {
    let bytes = std::fs::read(journal_path)
        .map_err(|error| format!("{}: cannot read: {error}", journal_path.display()))?;
    let bodies = match journal::records(&bytes, true) {
        Lines::Records(records) => records,
        Lines::Invalid(error) => return Err(error),
    };
    let mut calls: BTreeMap<String, Call> = BTreeMap::new();
    let mut results: BTreeMap<String, Value> = BTreeMap::new();
    for (position, body) in bodies.iter().enumerate() {
        let record: Record = serde_json::from_str(body)
            .map_err(|error| format!("record {position}: nanocode-turnlog refuses it: {error}"))?;
        match record {
            Record::Call { call_id, input, .. } => {
                let input: Value = serde_json::from_str(input.get()).map_err(|error| error.to_string())?;
                calls.entry(call_id).or_insert(Call { input, position });
            }
            Record::Result { call_id, output } => {
                let output: Value = serde_json::from_str(output.get()).map_err(|error| error.to_string())?;
                results.insert(call_id, output);
            }
            _ => {}
        }
    }
    let ledger = ledger(ledger_path)?;
    let mut counts = Counts::default();
    for (call_id, call) in &calls {
        let Some((required, max)) = wallet_input(&call.input) else {
            continue;
        };
        let offered = challenges(required)?;
        counts.challenges += offered.len() as u64;
        let Some(result) = results.get(call_id) else {
            continue;
        };
        let Some(units) = result.get("ok").and_then(Value::as_u64) else {
            continue;
        };
        counts.wallet_calls += 1;
        if units > max {
            return Err(format!("{call_id}: the wallet paid {units}, over the payment's max {max}"));
        }
        let paid = ledger
            .iter()
            .find(|entry| entry["event"] == "paid" && entry["call"] == call_id.as_str())
            .ok_or_else(|| format!("{call_id}: the journal records a payment of {units} the payer's ledger does not hold"))?;
        let id = paid["challenge"]["id"].as_str().unwrap_or_default();
        let Some((_, amount)) = offered.iter().find(|(offered, _)| offered == id) else {
            return Err(format!("{call_id}: the payer paid {id}, which the 402 did not offer"));
        };
        if *amount != Some(u128::from(units)) {
            return Err(format!("{call_id}: the wallet paid {units} for {id}, whose request asks {amount:?}"));
        }
        let credential: Credential = serde_json::from_value(paid["credential"].clone())
            .map_err(|error| format!("{call_id}: nanocode's Credential refuses the credential: {error}"))?;
        let echoed = serde_json::to_value(&credential).map_err(|error| error.to_string())?;
        if echoed["challenge"]["id"] != id || !echoed["source"].as_str().is_some_and(|source| source.starts_with(DID_PREFIX)) {
            return Err(format!("{call_id}: the credential does not echo {id} from a did:pkh source: {echoed}"));
        }
        counts.credentials += 1;
        let retried = calls.values().any(|retry| {
            retry.position > call.position && retry.input.get(PAYMENT_PARAM).and_then(Value::as_str) == Some(required)
        });
        if !retried {
            return Err(format!("{call_id}: the payment was made but no later call presents it"));
        }
        counts.retries += 1;
    }
    for entry in &ledger {
        match entry["event"].as_str() {
            Some("payment_response") => {
                let (id, amount) = challenge(&entry["challenge"])?;
                let stated: Option<u128> = entry["amount"].as_str().and_then(|text| text.parse().ok());
                if stated != amount {
                    return Err(format!("payment_response for {id}: amount {stated:?}, nanocode charges {amount:?}"));
                }
                if !entry["receipt"].is_null() {
                    let receipt = Receipt::from_value(entry["receipt"].clone())
                        .ok_or_else(|| format!("payment_response for {id}: nanocode's Receipt refuses {}", entry["receipt"]))?;
                    if receipt.reference.is_empty() {
                        return Err(format!("payment_response for {id}: a receipt without a reference"));
                    }
                    counts.receipts += 1;
                }
                counts.responses += 1;
            }
            Some("settled") => counts.settled += 1,
            _ => {}
        }
    }
    Ok(counts)
}
