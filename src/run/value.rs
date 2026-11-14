use std::collections::BTreeMap;

use crate::ir::{self, Program, Type, VariantId};
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
    pub fn zero(ty: &Type) -> Option<Self> {
        match ty {
            Type::U64 => Some(Self::U64(0)),
            Type::Bool => Some(Self::Bool(false)),
            Type::Text => Some(Self::Text(String::new())),
            Type::List { .. } => Some(Self::List(Vec::new())),
            Type::Sum(_) | Type::Known(_) | Type::Maybe(_) => None,
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
        (Type::List { element, .. }, Value::List(items)) => {
            Json::Array(items.iter().map(|item| to_json(program, element, item)).collect())
        }
        (Type::Sum(ir::CALL), Value::Variant(_, fields)) => {
            let mut map = BTreeMap::new();
            for (name, field) in ir::CALL_FIELDS.iter().zip(fields) {
                map.insert((*name).to_owned(), Json::String(field.as_text().to_owned()));
            }
            Json::Object(map)
        }
        (_, Value::Variant(variant, fields)) => {
            let variants = ty.variants(program).expect("a variant value has a sum type");
            let definition = &variants[*variant];
            assert_eq!(definition.fields.len(), fields.len(), "a variant value carries its fields");
            let name = definition.name.clone();
            match fields.len() {
                0 => Json::String(name),
                1 => Json::Object(BTreeMap::from([(name, to_json(program, &definition.fields[0], &fields[0]))])),
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
        Type::U64 => json.as_u64().map(Value::U64).ok_or_else(|| format!("expected u64, found {}", json.encode())),
        Type::Bool => match json {
            Json::Bool(flag) => Ok(Value::Bool(*flag)),
            other => Err(format!("expected bool, found {}", other.encode())),
        },
        Type::Text => json.as_str().map(|text| Value::Text(text.to_owned())).ok_or_else(|| format!("expected text, found {}", json.encode())),
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
                let field = json.get(name).and_then(Json::as_str).ok_or_else(|| format!("call lacks text field '{name}'"))?;
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
            None => return Err(format!("expected a variant of {}, found {}", ty.name(program), json.encode())),
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

pub fn state_json(program: &Program, state: &[Value]) -> Json {
    assert_eq!(state.len(), program.state.len(), "the state has one value per field");
    let mut map = BTreeMap::new();
    for (field, value) in program.state.iter().zip(state) {
        map.insert(field.name.clone(), to_json(program, &field.ty, value));
    }
    Json::Object(map)
}

pub fn state_from_json(program: &Program, json: &Json) -> Result<Vec<Value>, String> {
    let Json::Object(map) = json else {
        return Err("state is not an object".to_owned());
    };
    if map.len() != program.state.len() {
        return Err(format!("state has {} fields, the program has {}", map.len(), program.state.len()));
    }
    let mut state = Vec::new();
    for field in &program.state {
        let json = map.get(&field.name).ok_or_else(|| format!("state lacks field '{}'", field.name))?;
        state.push(from_json(program, &field.ty, json)?);
    }
    Ok(state)
}

pub fn initial_state(program: &Program) -> Vec<Value> {
    program
        .state
        .iter()
        .map(|field| Value::zero(&field.ty).expect("the checker admits only state fields with a zero value"))
        .collect()
}
