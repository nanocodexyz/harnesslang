use std::collections::BTreeMap;

use crate::ir::{self, Program, Type, VariantId};
use crate::run::journal::Output;
use crate::run::json::Json;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    U64(u64),
    Bool(bool),
    Text(String),
    Variant(VariantId, Vec<Value>),
    List(Vec<Value>),
}

impl Value {
    pub fn zero(program: &Program, ty: &Type) -> Option<Self> {
        match ty {
            Type::U64 => Some(Self::U64(0)),
            Type::Bool => Some(Self::Bool(false)),
            Type::Text => Some(Self::Text(String::new())),
            Type::List { .. } => Some(Self::List(Vec::new())),
            Type::Sum(_) => program.text_enum(ty).map(|sum| {
                assert!(!sum.variants.is_empty(), "a text enum has a value");
                Self::Variant(0, Vec::new())
            }),
            Type::Known(_) | Type::Maybe(_) => None,
        }
    }

    pub fn as_u64(&self) -> u64 {
        match self {
            Self::U64(value) => *value,
            other => panic!("the checker types this value as u64, found {other:?}"),
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            other => panic!("the checker types this value as bool, found {other:?}"),
        }
    }

    pub fn as_text(&self) -> &str {
        match self {
            Self::Text(value) => value,
            other => panic!("the checker types this value as text, found {other:?}"),
        }
    }

    pub fn as_list(&self) -> &[Value] {
        match self {
            Self::List(items) => items,
            other => panic!("the checker types this value as a list, found {other:?}"),
        }
    }

    pub fn as_variant(&self) -> (VariantId, &[Value]) {
        match self {
            Self::Variant(variant, fields) => (*variant, fields),
            other => panic!("the checker types this value as a sum, found {other:?}"),
        }
    }
}

pub fn to_json(program: &Program, ty: &Type, value: &Value) -> Json {
    match (ty, value) {
        (Type::U64, Value::U64(number)) => Json::Number(*number),
        (Type::Bool, Value::Bool(flag)) => Json::Bool(*flag),
        (Type::Text, Value::Text(text)) => Json::String(text.clone()),
        (Type::List { element, .. }, Value::List(items)) => Json::Array(
            items
                .iter()
                .map(|item| to_json(program, element, item))
                .collect(),
        ),
        (Type::Sum(ir::CALL), Value::Variant(_, fields)) => {
            let mut map = BTreeMap::new();
            for (name, field) in ir::CALL_FIELDS.iter().zip(fields) {
                map.insert((*name).to_owned(), Json::String(field.as_text().to_owned()));
            }
            Json::Object(map)
        }
        (_, Value::Variant(variant, fields)) => {
            let variants = ty
                .variants(program)
                .expect("a variant value has a sum type");
            let definition = &variants[*variant];
            assert_eq!(
                definition.fields.len(),
                fields.len(),
                "a variant value carries its fields"
            );
            let name = definition.name.clone();
            match fields.len() {
                0 => Json::String(name),
                1 => Json::Object(BTreeMap::from([(
                    name,
                    to_json(program, &definition.fields[0], &fields[0]),
                )])),
                _ => {
                    let items = definition
                        .fields
                        .iter()
                        .zip(fields)
                        .map(|(field_type, field)| to_json(program, field_type, field))
                        .collect();
                    Json::Object(BTreeMap::from([(name, Json::Array(items))]))
                }
            }
        }
        (ty, value) => panic!("value {value:?} does not have type {ty:?}"),
    }
}

pub fn from_json(program: &Program, ty: &Type, json: &Json) -> Result<Value, String> {
    match ty {
        Type::U64 => json
            .as_u64()
            .map(Value::U64)
            .ok_or_else(|| format!("expected u64, found {}", json.encode())),
        Type::Bool => match json {
            Json::Bool(flag) => Ok(Value::Bool(*flag)),
            other => Err(format!("expected bool, found {}", other.encode())),
        },
        Type::Text => json
            .as_str()
            .map(|text| Value::Text(text.to_owned()))
            .ok_or_else(|| format!("expected text, found {}", json.encode())),
        Type::List { element, capacity } => {
            let Json::Array(items) = json else {
                return Err(format!("expected a list, found {}", json.encode()));
            };
            if items.len() as u64 > capacity.value {
                return Err(format!("bound({})", capacity.name));
            }
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(from_json(program, element, item)?);
            }
            Ok(Value::List(values))
        }
        Type::Sum(ir::CALL) => {
            let mut fields = Vec::new();
            for name in ir::CALL_FIELDS {
                let field = json
                    .get(name)
                    .and_then(Json::as_str)
                    .ok_or_else(|| format!("call lacks text field '{name}'"))?;
                fields.push(Value::Text(field.to_owned()));
            }
            if !matches!(json, Json::Object(map) if map.len() == ir::CALL_FIELDS.len()) {
                return Err("call has fields other than id, tool and input".to_owned());
            }
            Ok(Value::Variant(0, fields))
        }
        Type::Sum(_) | Type::Known(_) | Type::Maybe(_) => variant_from_json(program, ty, json),
    }
}

fn variant_from_json(program: &Program, ty: &Type, json: &Json) -> Result<Value, String> {
    let variants = ty.variants(program).expect("a sum type has variants");
    let (name, payload) = match json {
        Json::String(name) => (name.as_str(), None),
        other => match other.tagged() {
            Some((name, payload)) => (name, Some(payload)),
            None => {
                return Err(format!(
                    "expected a variant of {}, found {}",
                    ty.name(program),
                    json.encode()
                ));
            }
        },
    };
    let Some(index) = variants.iter().position(|variant| variant.name == name) else {
        return Err(format!("'{name}' is not a variant of {}", ty.name(program)));
    };
    let fields = &variants[index].fields;
    let values = match (fields.len(), payload) {
        (0, None) => Vec::new(),
        (1, Some(payload)) => vec![from_json(program, &fields[0], payload)?],
        (count, Some(Json::Array(items))) if count > 1 && items.len() == count => {
            let mut values = Vec::new();
            for (field, item) in fields.iter().zip(items) {
                values.push(from_json(program, field, item)?);
            }
            values
        }
        _ => return Err(format!("variant '{name}' has {} field(s)", fields.len())),
    };
    Ok(Value::Variant(index, values))
}

pub fn model_failure(response: &Json) -> Option<&str> {
    match response {
        Json::Object(map) if map.len() == 1 => map.get(ir::MODEL_FAILED).and_then(Json::as_str),
        _ => None,
    }
}

pub fn model_outcome(program: &Program, response: &Json) -> Value {
    if let Some(error) = model_failure(response) {
        return Value::Variant(ir::FAILED, vec![Value::Text(error.to_owned())]);
    }
    match from_json(program, &Type::Sum(ir::REPLY), response) {
        Ok(reply) => Value::Variant(ir::OK, vec![reply]),
        Err(error) => Value::Variant(
            ir::FAILED,
            vec![Value::Text(format!("model reply: {error}"))],
        ),
    }
}

pub fn usage_over(response: &Json, window: u64) -> Option<String> {
    let Json::Object(map) = response else {
        return None;
    };
    let Some(Json::Array(fields)) = map.get(ir::ATTEMPT_VARIANTS[ir::ATTEMPT_OK]) else {
        return None;
    };
    ir::USAGE_FIELDS
        .iter()
        .enumerate()
        .find_map(|(index, name)| {
            let used = fields.get(ir::ATTEMPT_USAGE + index)?.as_u64()?;
            (used > window).then(|| {
                format!(
                    "model reply: {name} usage of {used} tokens exceeds the window of {window} tokens"
                )
            })
        })
}

pub fn without_retry_after(response: &Json) -> Json {
    let Json::Object(map) = response else {
        return response.clone();
    };
    let delayed =
        [ir::ATTEMPT_THROTTLED, ir::ATTEMPT_TRANSIENT].map(|variant| ir::ATTEMPT_VARIANTS[variant]);
    let mut out = map.clone();
    for name in delayed {
        if let Some(Json::Array(fields)) = out.get_mut(name)
            && let Some(slot) = fields.get_mut(ir::ATTEMPT_RETRY_AFTER)
        {
            *slot = Json::Number(0);
        }
    }
    Json::Object(out)
}

pub fn attempt(program: &Program, response: &Json, usage_checked: bool) -> Value {
    let window = program
        .model
        .as_ref()
        .and_then(|model| model.window.as_ref())
        .expect("an attempt belongs to a model with a window");
    let rejected = |error: String| Value::Variant(ir::ATTEMPT_REJECTED, vec![Value::Text(error)]);
    match from_json(program, &Type::Sum(window.attempt), response) {
        Err(error) => rejected(format!("model reply: {error}")),
        Ok(value) => match usage_over(response, window.tokens.value) {
            Some(error) if usage_checked => rejected(error),
            _ => value,
        },
    }
}

pub fn returned(program: &Program, returns: &Type) -> String {
    match program.text_enum(returns) {
        Some(sum) => format!("a value of {}", sum.name),
        None => match returns {
            Type::U64 => "a u64".to_owned(),
            Type::Bool => "a bool".to_owned(),
            Type::Text => "text".to_owned(),
            other => {
                panic!("the checker admits only scalar or text enum tool results, found {other:?}")
            }
        },
    }
}

pub fn checked_output(program: &Program, tool: &str, returns: &Type, output: Output) -> Output {
    let Output::Ok(json) = &output else {
        return output;
    };
    if from_json(program, returns, json).is_ok() {
        return output;
    }
    Output::Failed(format!(
        "{tool} returned {}, which is not {}",
        json.encode(),
        returned(program, returns)
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Untyped,
    Typed(Json),
    Invalid(String),
}

pub fn tool_input(program: &Program, tool: &str, raw: &str) -> Input {
    let Some(params) = program.input_params(tool) else {
        return Input::Untyped;
    };
    match typed_input(program, params, raw) {
        Ok(input) => Input::Typed(input),
        Err(error) => Input::Invalid(format!("input of {tool}: {error}")),
    }
}

fn typed_input(program: &Program, params: &[ir::Field], raw: &str) -> Result<Json, String> {
    let Ok(Json::Object(fields)) = crate::run::json::decode(raw) else {
        return Err("not a JSON object".to_owned());
    };
    let input = Json::Object(fields);
    input_fits(program, params, &input)?;
    Ok(input)
}

pub fn input_fits(program: &Program, params: &[ir::Field], input: &Json) -> Result<(), String> {
    let Json::Object(fields) = input else {
        return Err("not a JSON object".to_owned());
    };
    for param in params {
        let Some(field) = fields.get(&param.name) else {
            return Err(format!("missing '{}'", param.name));
        };
        from_json(program, &param.ty, field)
            .map_err(|error| format!("'{}': {error}", param.name))?;
    }
    if let Some(extra) = fields
        .keys()
        .find(|key| !params.iter().any(|param| param.name == **key))
    {
        return Err(format!("'{extra}' is not a parameter"));
    }
    Ok(())
}

pub fn not_admitted(tool: &str, listing: &str, name: &str) -> String {
    format!("{tool} was not admitted: listing {listing} did not list {name} in this turn")
}

pub fn changed_since_admission(tool: &str) -> String {
    format!("{tool} changed since admission; rediscover before executing")
}

pub fn checked_listing(listing: &ir::Listing, output: Output) -> Output {
    let Output::Ok(json) = &output else {
        return output;
    };
    let fingerprints = match json {
        Json::Object(map)
            if map
                .iter()
                .all(|(name, fingerprint)| !name.is_empty() && fingerprint.as_str().is_some()) =>
        {
            map.len() as u64
        }
        other => {
            return Output::Failed(format!(
                "listing {} returned {}, which is not an object of fingerprints",
                listing.name,
                other.encode()
            ));
        }
    };
    if fingerprints > listing.bound.value {
        return Output::Failed(format!(
            "listing {} returned {fingerprints} tools, over its bound {} ({})",
            listing.name, listing.bound.name, listing.bound.value
        ));
    }
    output
}

pub fn admitted(json: &Json) -> Result<BTreeMap<String, String>, String> {
    let Json::Object(map) = json else {
        return Err(format!("a listing is an object, found {}", json.encode()));
    };
    let mut tools = BTreeMap::new();
    for (name, fingerprint) in map {
        let fingerprint = fingerprint.as_str().ok_or_else(|| {
            format!(
                "listed tool '{name}' has fingerprint {}, not text",
                fingerprint.encode()
            )
        })?;
        tools.insert(name.clone(), fingerprint.to_owned());
    }
    Ok(tools)
}

pub fn fallback_output(program: &Program, returns: &Type, output: Output) -> Output {
    match (&output, program.text_enum(returns)) {
        (Output::Ok(json), Some(sum)) if from_json(program, returns, json).is_err() => {
            let last = sum.variants.last().expect("a text enum has a value");
            Output::Ok(Json::String(last.name.clone()))
        }
        _ => output,
    }
}

pub fn state_json(program: &Program, state: &[Value]) -> Json {
    assert_eq!(
        state.len(),
        program.state.len(),
        "the state has one value per field"
    );
    let mut map = BTreeMap::new();
    for (field, value) in program.state.iter().zip(state) {
        map.insert(field.name.clone(), to_json(program, &field.ty, value));
    }
    Json::Object(map)
}

static HASHED_CHECKPOINTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn state_text_hashed(program: &Program, state: &[Value]) -> String {
    let mut fields: Vec<(&str, Json)> = program
        .state
        .iter()
        .zip(state)
        .map(|(field, value)| (field.name.as_str(), to_json(program, &field.ty, value)))
        .collect();
    let call = HASHED_CHECKPOINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    fields.sort_by_key(|(name, _)| {
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(&(call, name), &mut hasher);
        std::hash::Hasher::finish(&hasher)
    });
    let entries: Vec<String> = fields
        .iter()
        .map(|(name, value)| format!("{}:{}", Json::text(name).encode(), value.encode()))
        .collect();
    format!("{{{}}}", entries.join(","))
}

pub fn state_from_json(program: &Program, json: &Json) -> Result<Vec<Value>, String> {
    let Json::Object(map) = json else {
        return Err("state is not an object".to_owned());
    };
    if map.len() != program.state.len() {
        return Err(format!(
            "state has {} fields, the program has {}",
            map.len(),
            program.state.len()
        ));
    }
    let mut state = Vec::new();
    for field in &program.state {
        let json = map
            .get(&field.name)
            .ok_or_else(|| format!("state lacks field '{}'", field.name))?;
        state.push(from_json(program, &field.ty, json)?);
    }
    Ok(state)
}

pub fn initial_state(program: &Program) -> Vec<Value> {
    program
        .state
        .iter()
        .map(|field| {
            Value::zero(program, &field.ty)
                .expect("the checker admits only state fields with a zero value")
        })
        .collect()
}
