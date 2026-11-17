use std::collections::BTreeMap;

use crate::json::Value;

#[derive(Clone, Default)]
pub struct Texts {
    pub templates: BTreeMap<String, String>,
    pub numbers: BTreeMap<String, u64>,
    pub lists: BTreeMap<String, Vec<String>>,
}

impl Texts {
    pub fn fill(&self, name: &str, fields: &[(&str, &str)]) -> String {
        let Some(template) = self.templates.get(name) else {
            return raw(name, fields);
        };
        let mut out = String::with_capacity(template.len());
        let mut rest = template.as_str();
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let value = after.find('}').and_then(|close| {
                let key = &after[..close];
                fields
                    .iter()
                    .find(|(field, _)| *field == key)
                    .map(|(_, value)| (close, *value))
            });
            match value {
                Some((close, value)) => {
                    out.push_str(value);
                    rest = &after[close + 1..];
                }
                None => {
                    out.push('{');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    pub fn number(&self, name: &str) -> Option<u64> {
        self.numbers.get(name).copied()
    }

    pub fn list(&self, name: &str) -> &[String] {
        self.lists.get(name).map(Vec::as_slice).unwrap_or_default()
    }
}

fn raw(name: &str, fields: &[(&str, &str)]) -> String {
    let fields = fields.iter().map(|(key, value)| (*key, Value::text(value)));
    Value::object([(name, Value::object(fields))]).encode()
}
