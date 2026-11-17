#[path = "../json.rs"]
#[allow(dead_code)]
mod json;
#[path = "../mpp.rs"]
#[allow(dead_code)]
mod mpp;

use std::io::Read as _;
use std::time::Duration;

use json::Value;

const METHOD: &str = "tempo";
const INTENT: &str = "charge";
const TRANSFER_PATH: &str = "/chain/transfer";

fn refuse(reason: &str) -> ! {
    print!("@@hl-refused@@{reason}@@hl-end@@");
    std::process::exit(0);
}

fn pause() {
    let ms = std::env::var("MPP_PAY_SLEEP_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(0);
    std::thread::sleep(Duration::from_millis(ms));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [max] = args.as_slice() else {
        eprintln!("usage: mpp-pay MAX_UNITS < CHALLENGE");
        std::process::exit(2);
    };
    let Ok(max) = max.parse::<u64>() else {
        refuse("MAX_UNITS is not a number");
    };
    let Ok(chain) = std::env::var("MPP_CHAIN_URL") else {
        refuse("MPP_CHAIN_URL is not set");
    };
    let mut challenge = String::new();
    if std::io::stdin().read_to_string(&mut challenge).is_err()
        || !challenge.starts_with(mpp::SCHEME)
    {
        refuse("the challenge is not a Payment challenge");
    }
    let params = mpp::params(challenge.trim());
    if mpp::param(&params, "method") != Some(METHOD)
        || mpp::param(&params, "intent") != Some(INTENT)
    {
        refuse("only tempo charge challenges are paid");
    }
    let (Some(id), Some(request)) = (mpp::param(&params, "id"), mpp::param(&params, "request"))
    else {
        refuse("the challenge has no id or request");
    };
    let detail = mpp::base64_url_decode(request)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| Value::decode(&text).ok());
    let Some(amount) = detail
        .as_ref()
        .and_then(|detail| detail.get("amount"))
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        refuse("the challenge request has no amount");
    };
    match amount.parse::<u64>() {
        Ok(units) if units <= max => {}
        _ => refuse(&format!(
            "the charge {amount} is above the {max} the harness allows"
        )),
    }
    pause();
    let body = Value::object([("id", Value::text(id)), ("amount", Value::text(&amount))]).encode();
    let hash = match mpp::post(&chain, TRANSFER_PATH, &body) {
        Ok((200, body)) => Value::decode(&body)
            .ok()
            .and_then(|body| body.get("hash").and_then(Value::as_str).map(str::to_owned)),
        Ok((status, body)) => refuse(&format!("the transfer answered {status} {}", body.trim())),
        Err(error) => refuse(&format!("the transfer failed: {error}")),
    };
    let Some(hash) = hash else {
        refuse("the transfer answered no hash");
    };
    pause();
    let echoed = Value::object(
        params
            .iter()
            .filter(|(key, _)| {
                ["id", "realm", "method", "intent", "request"].contains(&key.as_str())
            })
            .map(|(key, value)| (key.as_str(), Value::text(value))),
    );
    let credential = Value::object([
        ("challenge", echoed),
        (
            "payload",
            Value::object([("type", Value::text("hash")), ("hash", Value::text(&hash))]),
        ),
    ]);
    print!(
        "@@hl-paid@@{amount}@@hl-credential@@{}@@hl-end@@",
        mpp::base64_url(credential.encode().as_bytes())
    );
}
