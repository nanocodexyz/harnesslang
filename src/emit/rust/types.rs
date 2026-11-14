use std::fmt::Write;

use crate::ir::{self, Program, Type};

pub const RUST_KEYWORDS: [&str; 52] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];

pub fn ident(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if RUST_KEYWORDS.contains(&out.as_str()) || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.push('_');
    }
    out
}

pub fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for character in name.chars() {
        if character == '_' {
            upper = true;
        } else if upper {
            out.extend(character.to_uppercase());
            upper = false;
        } else {
            out.push(character);
        }
    }
    if RUST_KEYWORDS.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

pub fn variant_ident(name: &str) -> String {
    let plain = name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        return camel(name);
    }
    let mut out = String::from("X_");
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character);
        } else {
            write!(out, "_{:x}_", u32::from(character)).expect("writing to a string succeeds");
        }
    }
    out
}

pub fn constant(name: &str) -> String {
    ident(name).to_uppercase()
}

pub fn sum_name(program: &Program, sum: ir::SumId) -> String {
    format!("Sum{}", camel(&program.sums[sum].name))
}

pub struct Types<'p> {
    program: &'p Program,
    table: Vec<Type>,
    emitted: usize,
}

impl<'p> Types<'p> {
    pub fn new(program: &'p Program) -> Self {
        Self {
            program,
            table: Vec::new(),
            emitted: 0,
        }
    }

    pub const fn program(&self) -> &'p Program {
        self.program
    }

    pub fn rust(&self, ty: &Type) -> String {
        match ty {
            Type::U64 => "u64".to_owned(),
            Type::Bool => "bool".to_owned(),
            Type::Text => "String".to_owned(),
            Type::Sum(sum) => sum_name(self.program, *sum),
            Type::List { element, .. } => format!("Vec<{}>", self.rust(element)),
            Type::Known(inner) => format!("Known<{}>", self.rust(inner)),
            Type::Maybe(inner) => format!("Maybe<{}>", self.rust(inner)),
        }
    }

    fn index(&mut self, ty: &Type) -> usize {
        if let Some(index) = self.table.iter().position(|known| known == ty) {
            return index;
        }
        self.table.push(ty.clone());
        self.table.len() - 1
    }

    pub fn to_json(&mut self, ty: &Type) -> String {
        format!("json_{}", self.index(ty))
    }

    pub fn from_json(&mut self, ty: &Type) -> String {
        match ty {
            Type::U64 => "parse_u64".to_owned(),
            Type::Bool => "parse_bool".to_owned(),
            Type::Text => "parse_text".to_owned(),
            _ => format!("parse_{}", self.index(ty)),
        }
    }

    pub fn copy(ty: &Type) -> bool {
        matches!(ty, Type::U64 | Type::Bool)
    }

    pub fn returns(&self, ty: &Type) -> String {
        if let Some(sum) = self.program.text_enum(ty) {
            let values: Vec<String> = sum
                .variants
                .iter()
                .map(|value| format!("{:?}", value.name))
                .collect();
            return format!(
                "Returns::Values {{ name: {:?}, values: &[{}] }}",
                sum.name,
                values.join(", ")
            );
        }
        match ty {
            Type::U64 => "Returns::U64".to_owned(),
            Type::Bool => "Returns::Bool".to_owned(),
            Type::Text => "Returns::Text".to_owned(),
            other => panic!("the checker admits only scalar tool results, found {other:?}"),
        }
    }

    pub fn emit_codecs(&mut self, out: &mut String) {
        while self.emitted < self.table.len() {
            let index = self.emitted;
            self.emitted += 1;
            let ty = self.table[index].clone();
            self.emit_to_json(out, index, &ty);
            if !matches!(ty, Type::U64 | Type::Bool | Type::Text) {
                self.emit_from_json(out, index, &ty);
            }
        }
    }

    fn variants(&self, ty: &Type) -> Vec<(String, ir::Variant)> {
        let variants = ty.variants(self.program).expect("a sum type has variants");
        let prefix = match ty {
            Type::Sum(sum) => sum_name(self.program, *sum),
            Type::Known(_) => "Known".to_owned(),
            Type::Maybe(_) => "Maybe".to_owned(),
            other => panic!("only sums have variants, found {other:?}"),
        };
        variants
            .into_iter()
            .map(|variant| {
                (
                    format!("{prefix}::{}", variant_ident(&variant.name)),
                    variant,
                )
            })
            .collect()
    }

    fn emit_to_json(&mut self, out: &mut String, index: usize, ty: &Type) {
        let rust = self.rust(ty);
        let body = match ty {
            Type::U64 => "Json::Number(*value)".to_owned(),
            Type::Bool => "Json::Bool(*value)".to_owned(),
            Type::Text => "Json::String(value.clone())".to_owned(),
            Type::List { element, .. } => {
                format!(
                    "Json::Array(value.iter().map({}).collect())",
                    self.to_json(element)
                )
            }
            Type::Sum(ir::CALL) => format!(
                "let {name}::Call(id, tool, input) = value;\n    Json::object(vec![(\"id\", Json::String(id.clone())), (\"tool\", Json::String(tool.clone())), (\"input\", Json::String(input.clone()))])",
                name = sum_name(self.program, ir::CALL)
            ),
            _ => {
                let mut arms = String::new();
                for (path, variant) in self.variants(ty) {
                    let bindings: Vec<String> = (0..variant.fields.len())
                        .map(|field| format!("f{field}"))
                        .collect();
                    let pattern = if bindings.is_empty() {
                        path
                    } else {
                        format!("{path}({})", bindings.join(", "))
                    };
                    let fields: Vec<String> = variant
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(field, field_ty)| format!("{}(f{field})", self.to_json(field_ty)))
                        .collect();
                    writeln!(
                        arms,
                        "        {pattern} => tagged({:?}, vec![{}]),",
                        variant.name,
                        fields.join(", ")
                    )
                    .expect("writing to a string succeeds");
                }
                format!("match value {{\n{arms}    }}")
            }
        };
        writeln!(
            out,
            "pub fn json_{index}(value: &{rust}) -> Json {{\n    {body}\n}}\n"
        )
        .expect("writing to a string succeeds");
    }

    fn emit_from_json(&mut self, out: &mut String, index: usize, ty: &Type) {
        let rust = self.rust(ty);
        let body = match ty {
            Type::List { element, capacity } => format!(
                "parse_list(json, {}, {:?}, {})",
                constant(&capacity.name),
                capacity.name,
                self.from_json(element)
            ),
            Type::Sum(ir::CALL) => {
                let mut lines = String::new();
                for name in ir::CALL_FIELDS {
                    writeln!(
                        lines,
                        "    let {name} = json.get({name:?}).and_then(Json::as_str).ok_or_else(|| \"call lacks text field '{name}'\".to_owned())?.to_owned();"
                    )
                    .expect("writing to a string succeeds");
                }
                format!(
                    "{lines}    if !matches!(json, Json::Object(map) if map.len() == {count}) {{\n        return Err(\"call has fields other than id, tool and input\".to_owned());\n    }}\n    Ok({name}::Call(id, tool, input))",
                    count = ir::CALL_FIELDS.len(),
                    name = sum_name(self.program, ir::CALL)
                )
            }
            _ => self.variant_parser(ty),
        };
        writeln!(
            out,
            "pub fn parse_{index}(json: &Json) -> Result<{rust}, String> {{\n    {body}\n}}\n"
        )
        .expect("writing to a string succeeds");
    }

    fn variant_parser(&mut self, ty: &Type) -> String {
        let variants = self.variants(ty);
        let names: Vec<String> = variants
            .iter()
            .map(|(_, variant)| format!("{:?}", variant.name))
            .collect();
        let mut arms = String::new();
        for (index, (path, variant)) in variants.iter().enumerate() {
            let fields: Vec<String> = variant
                .fields
                .iter()
                .enumerate()
                .map(|(field, field_ty)| format!("{}(fields[{field}])?", self.from_json(field_ty)))
                .collect();
            let value = if fields.is_empty() {
                path.clone()
            } else {
                format!("{path}({})", fields.join(", "))
            };
            writeln!(
                arms,
                "        {index} => {{\n            let fields = variant_fields(payload, {:?}, {})?;\n            Ok({value})\n        }}",
                variant.name,
                variant.fields.len()
            )
            .expect("writing to a string succeeds");
        }
        format!(
            "let (index, payload) = variant_of(json, {:?}, &[{}])?;\n    match index {{\n{arms}        _ => unreachable!(\"variant_of returns a listed variant\"),\n    }}",
            ty.name(self.program),
            names.join(", ")
        )
    }
}
