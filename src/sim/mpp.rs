use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{self, Program};
use crate::run::engine::ToolRequest;
use crate::run::journal::{Output, Record, call_id};
use crate::run::json::{self, Json};
use crate::sim::invariants::{Invariant, Violation};
use crate::sim::rng::Rng;
use crate::sim::world::SimWorld;

const FREE_PPM: u64 = 100_000;
const FOREIGN_METHOD_PPM: u64 = 200_000;
const EXPIRED_PPM: u64 = 100_000;
const PROOF_PPM: u64 = 50_000;
const REUSE_ID_PPM: u64 = 100_000;
const PAY_FAILS_PPM: u64 = 50_000;
const OVER_CHARGE_PPM: u64 = 100_000;
const CHALLENGES_MAX: u64 = 3;
const FATES: u64 = 5;
const METHOD: &str = "tempo";
const FOREIGN_METHOD: &str = "card";
const INTENT: &str = "charge";
const SESSION_INTENT: &str = "session";
const SESSION_PPM: u64 = 300_000;
const REALM: &str = "pay.sim";
const FOREIGN_REALM: &str = "other.sim";
const EXPIRED: &str = "2001-01-01T00:00:00Z";
const REFUSED_STATUSES: [u64; 3] = [400, 403, 409];
const SERVER_STATUSES: [u64; 2] = [500, 503];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payment {
    pub scope: String,
    pub turn: u64,
    pub seq: u64,
    pub challenge: String,
    pub amount: u64,
    pub required: String,
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    pub payments: Vec<Payment>,
    pub presented: BTreeSet<(String, String)>,
    pub offered: BTreeMap<String, Vec<Json>>,
    pub refused: BTreeSet<String>,
    next: u64,
}

fn challenge_json(id: &str, method: &str, realm: &str, amount: u64, expired: bool) -> Json {
    challenge_with(id, method, INTENT, realm, amount, expired)
}

fn challenge_with(
    id: &str,
    method: &str,
    intent: &str,
    realm: &str,
    amount: u64,
    expired: bool,
) -> Json {
    let mut fields = BTreeMap::new();
    fields.insert("amount".to_owned(), Json::String(amount.to_string()));
    fields.insert("id".to_owned(), Json::text(id));
    fields.insert("intent".to_owned(), Json::text(intent));
    fields.insert("method".to_owned(), Json::text(method));
    fields.insert("realm".to_owned(), Json::text(realm));
    if expired {
        fields.insert("expires".to_owned(), Json::text(EXPIRED));
    }
    Json::Object(fields)
}

pub fn required_text(challenges: &[Json]) -> String {
    format!(
        "{}{}",
        ir::PAYMENT_REQUIRED_PREFIX,
        Json::object([("challenges", Json::Array(challenges.to_vec()))]).encode()
    )
}

fn field<'j>(json: &'j Json, key: &str) -> Option<&'j str> {
    match json.get(key) {
        Some(Json::String(text)) => Some(text),
        _ => None,
    }
}

impl Ledger {
    fn offers(&mut self, program: &Program, rng: &mut Rng, previous: &[Json]) -> Vec<Json> {
        let charge = program
            .wallet
            .as_ref()
            .expect("only a harness with a wallet offers challenges")
            .charge
            .value;
        (0..rng.range(1, CHALLENGES_MAX))
            .map(|_| {
                if !previous.is_empty() && rng.chance(REUSE_ID_PPM) {
                    return previous[rng.pick(previous.len())].clone();
                }
                self.next += 1;
                let id = format!("c{}", self.next);
                if rng.chance(PROOF_PPM) {
                    return challenge_json(&id, METHOD, FOREIGN_REALM, 0, false);
                }
                let method = if rng.chance(FOREIGN_METHOD_PPM) {
                    FOREIGN_METHOD
                } else {
                    METHOD
                };
                let amount = if rng.chance(OVER_CHARGE_PPM) {
                    charge.saturating_add(rng.range(1, charge))
                } else {
                    rng.range(1, charge)
                };
                let intent = if rng.chance(SESSION_PPM) {
                    SESSION_INTENT
                } else {
                    INTENT
                };
                challenge_with(&id, method, intent, REALM, amount, rng.chance(EXPIRED_PPM))
            })
            .collect()
    }

    pub fn total(&self, scope: &str) -> u64 {
        self.payments
            .iter()
            .filter(|payment| payment.scope == scope)
            .map(|payment| payment.amount)
            .sum()
    }
}

fn payable(challenge: &Json, max: u64) -> Result<u64, String> {
    let id = field(challenge, "id").unwrap_or_default();
    let intent = field(challenge, "intent");
    if field(challenge, "method") != Some(METHOD)
        || (intent != Some(INTENT) && intent != Some(SESSION_INTENT))
    {
        return Err(format!(
            "{id} is not {METHOD}.{INTENT} or {METHOD}.{SESSION_INTENT}"
        ));
    }
    if challenge.get("expires").is_some() {
        return Err(format!("{id} expired"));
    }
    let amount: u64 = field(challenge, "amount")
        .and_then(|amount| amount.parse().ok())
        .ok_or_else(|| format!("{id} names no amount"))?;
    if amount == 0 && field(challenge, "realm") != Some(REALM) {
        return Err(format!("{id} asks a proof for another realm"));
    }
    if amount > max {
        return Err(format!("{id} asks {amount}, over the payment's max {max}"));
    }
    Ok(amount)
}

pub fn answer(
    world: &mut SimWorld<'_>,
    request: &ToolRequest<'_>,
    rng: &mut Rng,
) -> Option<Output> {
    if world.program.is_wallet(request.tool) {
        return Some(pay(world, request, rng));
    }
    if !world.program.paid(request.tool) {
        return None;
    }
    let payment = field(request.input, ir::PAYMENT_PARAM)?.to_owned();
    let scope = world.scope.clone();
    if payment.is_empty() {
        if rng.chance(FREE_PPM) {
            return None;
        }
        let offers = world.mpp.offers(world.program, rng, &[]);
        let required = required_text(&offers);
        world.mpp.offered.insert(required.clone(), offers);
        world.stats.payment_required += 1;
        return Some(Output::Failed(required));
    }
    let paid = world
        .mpp
        .payments
        .iter()
        .any(|paid| paid.scope == scope && paid.required == payment);
    if !paid {
        return Some(Output::Failed(format!(
            "{}: no credential was made for this payment",
            request.tool
        )));
    }
    world.mpp.presented.insert((scope, payment.clone()));
    world.stats.presented += 1;
    Some(match rng.below(FATES) {
        0 => return None,
        1 => {
            let previous = world.mpp.offered.get(&payment).cloned().unwrap_or_default();
            let offers = world.mpp.offers(world.program, rng, &previous);
            let required = required_text(&offers);
            world.mpp.offered.insert(required.clone(), offers);
            world.stats.payment_required += 1;
            Output::Failed(required)
        }
        2 => Output::Failed(format!(
            "HTTP {}: the server refused the payment",
            REFUSED_STATUSES[rng.pick(REFUSED_STATUSES.len())]
        )),
        3 => Output::Failed(format!(
            "HTTP {}: server error",
            SERVER_STATUSES[rng.pick(SERVER_STATUSES.len())]
        )),
        _ => Output::Failed("connection reset after the request was sent".to_owned()),
    })
}

fn pay(world: &mut SimWorld<'_>, request: &ToolRequest<'_>, rng: &mut Rng) -> Output {
    let required = field(request.input, ir::WALLET_REQUIRED_PARAM)
        .unwrap_or_default()
        .to_owned();
    let max = match request.input.get(ir::WALLET_MAX_PARAM) {
        Some(Json::Number(max)) => *max,
        _ => return Output::Failed("the payment names no max".to_owned()),
    };
    let max = if world.mutation == crate::run::mutation::Mutation::CapUnchecked {
        world
            .program
            .wallet
            .as_ref()
            .expect("a wallet call belongs to a harness with a wallet")
            .charge
            .value
    } else {
        max
    };
    let challenges = match required
        .strip_prefix(ir::PAYMENT_REQUIRED_PREFIX)
        .map(json::decode)
    {
        Some(Ok(decoded)) => match decoded.get("challenges") {
            Some(Json::Array(challenges)) => challenges.clone(),
            _ => return Output::Failed("the payment required names no challenges".to_owned()),
        },
        _ => return Output::Failed("the payment required is not a challenge list".to_owned()),
    };
    let scope = world.scope.clone();
    let mut refusals = Vec::new();
    for challenge in &challenges {
        let id = field(challenge, "id").unwrap_or_default().to_owned();
        if world
            .mpp
            .payments
            .iter()
            .any(|paid| paid.scope == scope && paid.challenge == id)
        {
            refusals.push(format!("{id} is already paid"));
            continue;
        }
        let amount = match payable(challenge, max) {
            Ok(amount) => amount,
            Err(refusal) => {
                refusals.push(refusal);
                continue;
            }
        };
        if rng.chance(PAY_FAILS_PPM) {
            return Output::Failed(format!(
                "insufficient balance: the wallet holds less than {amount}"
            ));
        }
        world.mpp.payments.push(Payment {
            scope,
            turn: request.turn,
            seq: request.seq,
            challenge: id,
            amount,
            required,
        });
        world.stats.pays += 1;
        world.stats.paid_units += amount;
        return Output::Ok(Json::Number(amount));
    }
    Output::Failed(format!(
        "No method found for challenges: {}",
        refusals.join("; ")
    ))
}

fn violation<T>(invariant: Invariant, detail: String) -> Result<T, Violation> {
    Err(Violation { invariant, detail })
}

pub fn check(world: &SimWorld<'_>, records: &[Record]) -> Result<(), Violation> {
    let Some(wallet) = &world.program.wallet else {
        return Ok(());
    };
    let scope = world.scope.as_str();
    let total = world.mpp.total(scope);
    if total > wallet.spend.value {
        return violation(
            Invariant::SpendWithinCap,
            format!(
                "wallet {} paid {total} in session {scope:?}, over its spend cap {} ({})",
                wallet.name, wallet.spend.name, wallet.spend.value
            ),
        );
    }
    let mut unknown = BTreeSet::new();
    let mut spent_at: BTreeMap<u64, u64> = BTreeMap::new();
    let mut turn_of_checkpoint = 0;
    for record in records {
        match record {
            Record::Recovery {
                turn, unknown: ids, ..
            } => {
                unknown.extend(ids.iter().map(|seq| call_id(*turn, *seq)));
            }
            Record::Status { turn, .. } => turn_of_checkpoint = *turn,
            Record::Checkpoint { payload, .. } => {
                let spent =
                    json::decode(payload)
                        .ok()
                        .and_then(|state| match state.get(ir::SPENT_FIELD) {
                            Some(Json::Number(spent)) => Some(*spent),
                            _ => None,
                        });
                if let Some(spent) = spent {
                    spent_at.insert(turn_of_checkpoint, spent);
                }
            }
            _ => {}
        }
    }
    for (turn, spent) in &spent_at {
        let charged: u64 = world
            .mpp
            .payments
            .iter()
            .filter(|payment| payment.scope == scope && payment.turn <= *turn)
            .map(|payment| payment.amount)
            .sum();
        if *spent < charged {
            return violation(
                Invariant::SpendAccounted,
                format!(
                    "the checkpoint after turn {turn} holds {} = {spent}, yet wallet {} charged {charged} by then",
                    ir::SPENT_FIELD,
                    wallet.name
                ),
            );
        }
    }
    let finished: BTreeSet<u64> = records
        .iter()
        .filter_map(|record| match record {
            Record::Status { turn, .. } => Some(*turn),
            _ => None,
        })
        .collect();
    for payment in world
        .mpp
        .payments
        .iter()
        .filter(|payment| payment.scope == scope)
    {
        let id = call_id(payment.turn, payment.seq);
        let presented = world
            .mpp
            .presented
            .contains(&(payment.scope.clone(), payment.required.clone()));
        let retry = call_id(payment.turn, payment.seq + 1);
        let refused = world.mpp.refused.contains(&retry)
            || world
                .dispatched
                .get(&retry)
                .is_none_or(|retried| retried.external + retried.other == 0);
        let lost = unknown.contains(&id) || unknown.contains(&retry);
        if !presented && !refused && !lost && finished.contains(&payment.turn) {
            return violation(
                Invariant::PaymentPresented,
                format!(
                    "wallet {} paid {} for {} at {id}, yet no request presented that payment and the journal does not report it unknown",
                    wallet.name, payment.amount, payment.challenge
                ),
            );
        }
    }
    Ok(())
}
