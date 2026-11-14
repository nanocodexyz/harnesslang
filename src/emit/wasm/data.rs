use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::effect::EffectClass;
use crate::ir::{self, Program, SiteKind, Type};
use crate::target::{self, Target};

pub const KIND_TOOL: u32 = 0;
pub const KIND_DISPATCH: u32 = 1;
pub const KIND_MODEL: u32 = 2;
pub const KIND_NOW: u32 = 3;
pub const KIND_RANDOM: u32 = 4;
const TYPE_U64: u32 = 0;
const TYPE_BOOL: u32 = 1;
const TYPE_TEXT: u32 = 2;
const TYPE_LIST: u32 = 3;
const TYPE_SUM: u32 = 4;
const TYPE_KNOWN: u32 = 5;
const TYPE_MAYBE: u32 = 6;
const SUM_TAGGED: u32 = 0;
const SUM_TEXT: u32 = 1;
const SUM_CALL: u32 = 2;
const CLASS_DISPATCHED: u32 = 3;
const PRINTABLE_FIRST: u8 = 0x20;
const PRINTABLE_LAST: u8 = 0x7e;

pub struct Tables<'p> {
    pub program: &'p Program,
    pub out: String,
    texts: BTreeMap<String, String>,
    types: Vec<(Type, String)>,
    sums: BTreeSet<usize>,
}

pub fn effect_code(effect: EffectClass) -> u32 {
    match effect {
        EffectClass::None => 0,
        EffectClass::Idempotent => 1,
        EffectClass::External => 2,
    }
}

pub fn wat_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('"');
    for byte in bytes {
        if (PRINTABLE_FIRST..=PRINTABLE_LAST).contains(byte) && *byte != b'"' && *byte != b'\\' {
            out.push(char::from(*byte));
        } else {
            write!(out, "\\{byte:02x}").expect("writing to a string succeeds");
        }
    }
    out.push('"');
    out
}

pub fn context_type() -> Type {
    Type::List {
        element: Box::new(Type::Sum(ir::ENTRY)),
        capacity: ir::Bound {
            name: String::new(),
            value: 0,
        },
    }
}

fn word(value: u64, what: &str) -> Result<u32, String> {
    u32::try_from(value)
        .map_err(|_| format!("emit: not yet in wasm: {what} of {value} does not fit a 32-bit word"))
}

impl<'p> Tables<'p> {
    pub fn new(program: &'p Program) -> Self {
        Self {
            program,
            out: String::new(),
            texts: BTreeMap::new(),
            types: Vec::new(),
            sums: BTreeSet::new(),
        }
    }

    pub fn text(&mut self, value: &str) -> String {
        if let Some(name) = self.texts.get(value) {
            return name.clone();
        }
        let name = format!("$t{}", self.texts.len());
        writeln!(self.out, "(text {name} {})", wat_string(value.as_bytes()))
            .expect("writing to a string succeeds");
        self.texts.insert(value.to_owned(), name.clone());
        name
    }

    pub fn ty(&mut self, ty: &Type) -> Result<String, String> {
        if let Some((_, name)) = self.types.iter().find(|(known, _)| known == ty) {
            return Ok(name.clone());
        }
        let name = format!("$type{}", self.types.len());
        self.types.push((ty.clone(), name.clone()));
        let words = match ty {
            Type::U64 => format!("{TYPE_U64} 0 0 0"),
            Type::Bool => format!("{TYPE_BOOL} 0 0 0"),
            Type::Text => format!("{TYPE_TEXT} 0 0 0"),
            Type::List { element, capacity } => {
                let element = self.ty(element)?;
                let bound = self.text(&capacity.name);
                format!(
                    "{TYPE_LIST} {element} {} {bound}",
                    word(capacity.value, "a list's capacity")?
                )
            }
            Type::Sum(sum) => format!("{TYPE_SUM} {} 0 0", self.sum(*sum)?),
            Type::Known(inner) => format!("{TYPE_KNOWN} {} 0 0", self.ty(inner)?),
            Type::Maybe(inner) => format!("{TYPE_MAYBE} {} 0 0", self.ty(inner)?),
            Type::Blob(_) => return Err("emit: not yet in wasm: blob values".to_owned()),
        };
        writeln!(self.out, "(words {name} {words})").expect("writing to a string succeeds");
        Ok(name)
    }

    fn sum(&mut self, sum: usize) -> Result<String, String> {
        let name = format!("$sum{sum}");
        if !self.sums.insert(sum) {
            return Ok(name);
        }
        let definition = &self.program.sums[sum];
        let kind = match (sum, definition.kind) {
            (ir::CALL, _) => SUM_CALL,
            (_, ir::SumKind::Text) => SUM_TEXT,
            (_, ir::SumKind::Tagged) => SUM_TAGGED,
        };
        let sum_name = self.text(&definition.name);
        writeln!(
            self.out,
            "(words {name} {sum_name} {kind} {} {name}_variants)",
            definition.variants.len()
        )
        .expect("writing to a string succeeds");
        let mut variants = Vec::new();
        for (index, variant) in definition.variants.iter().enumerate() {
            let variant_name = self.text(&variant.name);
            let mut fields = Vec::new();
            for field in &variant.fields {
                fields.push(self.ty(field)?);
            }
            let label = format!("{name}_{index}");
            writeln!(self.out, "(words {label}_fields {})", fields.join(" "))
                .expect("writing to a string succeeds");
            writeln!(
                self.out,
                "(words {label} {variant_name} {} {label}_fields)",
                variant.fields.len()
            )
            .expect("writing to a string succeeds");
            variants.push(label);
        }
        writeln!(self.out, "(words {name}_variants {})", variants.join(" "))
            .expect("writing to a string succeeds");
        Ok(name)
    }

    fn field(&mut self, label: &str, field: &ir::Field) -> Result<String, String> {
        let name = self.text(&field.name);
        let ty = self.ty(&field.ty)?;
        writeln!(self.out, "(words {label} {name} {ty})").expect("writing to a string succeeds");
        Ok(label.to_owned())
    }

    fn tool(&mut self, index: usize, tool: &ir::Tool) -> Result<String, String> {
        let label = format!("$tool{index}");
        let mut params = Vec::new();
        for (position, (param, default)) in tool.params.iter().zip(&tool.defaults).enumerate() {
            let name = self.text(&param.name);
            let ty = self.ty(&param.ty)?;
            let default = match default {
                Some(literal) => self.text(&crate::run::value::literal_json(literal).encode()),
                None => "0".to_owned(),
            };
            let param_label = format!("{label}_param{position}");
            writeln!(self.out, "(words {param_label} {name} {ty} {default})")
                .expect("writing to a string succeeds");
            params.push(param_label);
        }
        let name = self.text(&tool.name);
        let returns = self.ty(&tool.returns)?;
        writeln!(self.out, "(words {label}_params {})", params.join(" "))
            .expect("writing to a string succeeds");
        writeln!(
            self.out,
            "(words {label} {name} {} {} {label}_params {returns})",
            effect_code(tool.effect),
            tool.params.len()
        )
        .expect("writing to a string succeeds");
        Ok(label)
    }

    pub fn site(&mut self, label: &str, site: &ir::Site) -> Result<(), String> {
        let (kind, tool) = match site.kind {
            SiteKind::Tool(tool) => (KIND_TOOL, format!("$tool{tool}")),
            SiteKind::Dispatch => (KIND_DISPATCH, "0".to_owned()),
            SiteKind::Model => (KIND_MODEL, "0".to_owned()),
            SiteKind::Now => (KIND_NOW, "0".to_owned()),
            SiteKind::Random => (KIND_RANDOM, "0".to_owned()),
            other => return Err(format!("emit: not yet in wasm: {} sites", other.name())),
        };
        if site.mode.is_some() {
            return Err("emit: not yet in wasm: model call modes".to_owned());
        }
        let class = match site.class {
            ir::SiteClass::Fixed(effect) => effect_code(effect),
            ir::SiteClass::Dispatched => CLASS_DISPATCHED,
            ir::SiteClass::Cell => return Err("emit: not yet in wasm: code cells".to_owned()),
        };
        let durability = match site.durability {
            ir::Durability::RidesNextFlush => 0,
            ir::Durability::FlushBeforeRun => 1,
            ir::Durability::FlushBeforeRunWhenExternal => 2,
            ir::Durability::Cell => return Err("emit: not yet in wasm: code cells".to_owned()),
        };
        let result = self.ty(&site.result)?;
        let argument = if kind == KIND_MODEL {
            self.ty(&context_type())?
        } else {
            "0".to_owned()
        };
        writeln!(
            self.out,
            "(words {label} {kind} {class} {durability} {result} {tool} {argument})"
        )
        .expect("writing to a string succeeds");
        Ok(())
    }

    pub fn program(&mut self, target: Target) -> Result<(), String> {
        let program = self.program;
        let mut fields = Vec::new();
        for (index, field) in program.state.iter().enumerate() {
            fields.push(self.field(&format!("$field{index}"), field)?);
        }
        let mut tools = Vec::new();
        for (index, tool) in program.tools.iter().enumerate() {
            tools.push(self.tool(index, tool)?);
        }
        let mut handlers = Vec::new();
        for (index, handler) in program.handlers.iter().enumerate() {
            let mut params = Vec::new();
            for (position, local) in handler.params.iter().enumerate() {
                params.push(self.field(
                    &format!("$handler{index}_param{position}"),
                    &handler.locals[*local],
                )?);
            }
            let name = self.text(&handler.name);
            let returns = self.ty(&handler.returns)?;
            writeln!(
                self.out,
                "(words $handler{index}_params {})",
                params.join(" ")
            )
            .expect("writing to a string succeeds");
            writeln!(
                self.out,
                "(words $handler{index} {name} {} $handler{index}_params {returns} {index})",
                params.len()
            )
            .expect("writing to a string succeeds");
            handlers.push(format!("$handler{index}"));
        }
        let (model, calls) = match &program.model {
            Some(model) => (
                self.text(&model.name),
                word(model.calls.value, "a model's calls bound")?,
            ),
            None => ("0".to_owned(), 0),
        };
        let name = self.text(&program.name);
        writeln!(self.out, "(words $program_handlers {})", handlers.join(" "))
            .expect("writing to a string succeeds");
        writeln!(self.out, "(words $program_fields {})", fields.join(" "))
            .expect("writing to a string succeeds");
        writeln!(self.out, "(words $program_tools {})", tools.join(" "))
            .expect("writing to a string succeeds");
        writeln!(
            self.out,
            "(words $program {name} {} $program_handlers {} $program_fields {model} {calls} {} $program_tools)",
            handlers.len(),
            fields.len(),
            tools.len()
        )
        .expect("writing to a string succeeds");
        let model = target::model(target);
        let checksum = u32::from(target == Target::NativeJsonl);
        let in_step = u32::from(matches!(model.recovery, target::Recovery::InStep { .. }));
        writeln!(
            self.out,
            "(words $target {checksum} {} {in_step})",
            u32::from(model.gate_before_outbound)
        )
        .expect("writing to a string succeeds");
        Ok(())
    }
}
