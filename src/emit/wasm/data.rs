use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::effect::EffectClass;
use crate::ir::{self, Program, SiteKind, Type};
use crate::run::json::Json;
use crate::target::{self, Target};

pub const KIND_TOOL: u32 = 0;
pub const KIND_DISPATCH: u32 = 1;
pub const KIND_MODEL: u32 = 2;
pub const KIND_NOW: u32 = 3;
pub const KIND_RANDOM: u32 = 4;
const FORK_NONE: u32 = 0;
const FORK_LATEST: u32 = 1;
const FORK_TURN: u32 = 2;
pub const KIND_WAIT: u32 = 7;
pub const KIND_HOLD: u32 = 8;
pub const KIND_SUSPEND: u32 = 9;
pub const KIND_DENY: u32 = 10;
const INTERRUPT_CANCEL: u32 = 1;
const INTERRUPT_STEER: u32 = 2;
const MODEL_TRAP: u32 = 0;
const WIRE_NONE: u32 = 0;
const WIRE_ANTHROPIC: u32 = 1;
const WIRE_OPENAI: u32 = 2;
const MODEL_KNOWN: u32 = 1;
const MODEL_ATTEMPT: u32 = 2;
pub const KIND_LISTING: u32 = 5;
pub const KIND_CELL: u32 = 6;
const CLASS_CELL: u32 = 4;
const DURABILITY_CELL: u32 = 3;
const TYPE_U64: u32 = 0;
const TYPE_BOOL: u32 = 1;
const TYPE_TEXT: u32 = 2;
const TYPE_LIST: u32 = 3;
const TYPE_SUM: u32 = 4;
const TYPE_KNOWN: u32 = 5;
const TYPE_MAYBE: u32 = 6;
const TYPE_BLOB: u32 = 7;
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

fn interrupt_bits(interrupts: ir::Interrupts) -> u32 {
    u32::from(interrupts.cancel) * INTERRUPT_CANCEL + u32::from(interrupts.steer) * INTERRUPT_STEER
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
            Type::Bytes(_) => return Err("emit: bytes live only inside pure fns".to_owned()),
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
            Type::Blob(bytes) => {
                let bound = self.text(&bytes.name);
                format!(
                    "{TYPE_BLOB} {} 0 {bound}",
                    word(bytes.value, "a blob's bound")?
                )
            }
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
        let fork = match tool.fork {
            None => FORK_NONE,
            Some(ir::ForkAt::Latest) => FORK_LATEST,
            Some(ir::ForkAt::Turn) => FORK_TURN,
        };
        let (jobs, jobs_max) = match &tool.jobs {
            Some(bound) => (1, word(bound.value, "a tool's job bound")?),
            None => (0, 0),
        };
        writeln!(
            self.out,
            "(words {label} {name} {} {} {label}_params {returns} {fork} {} {} {jobs} {jobs_max})",
            effect_code(tool.effect),
            tool.params.len(),
            interrupt_bits(tool.interrupts),
            u32::from(tool.recoverable)
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
            SiteKind::Listing(listing) => (KIND_LISTING, format!("$listing{listing}")),
            SiteKind::Cell(code) => (KIND_CELL, format!("$code{code}")),
            SiteKind::Wait(_) => (KIND_WAIT, "0".to_owned()),
            SiteKind::Hold(_) => (KIND_HOLD, "0".to_owned()),
            SiteKind::Suspend => (KIND_SUSPEND, "0".to_owned()),
            SiteKind::Deny => (KIND_DENY, "0".to_owned()),
        };
        let class = match site.class {
            ir::SiteClass::Fixed(effect) => effect_code(effect),
            ir::SiteClass::Dispatched => CLASS_DISPATCHED,
            ir::SiteClass::Cell => CLASS_CELL,
        };
        let durability = match site.durability {
            ir::Durability::RidesNextFlush => 0,
            ir::Durability::FlushBeforeRun => 1,
            ir::Durability::FlushBeforeRunWhenExternal => 2,
            ir::Durability::Cell => DURABILITY_CELL,
        };
        let result = self.ty(&site.result)?;
        let argument = if kind == KIND_MODEL {
            self.ty(&context_type())?
        } else {
            "0".to_owned()
        };
        let mode = match site.mode {
            Some(mode) => self.text(self.program.mode_name(mode)),
            None => "0".to_owned(),
        };
        writeln!(
            self.out,
            "(words {label} {kind} {class} {durability} {result} {tool} {argument} {mode})"
        )
        .expect("writing to a string succeeds");
        Ok(())
    }

    fn model(&mut self) -> Result<(), String> {
        let Some(model) = &self.program.model else {
            writeln!(self.out, "(words $model {MODEL_TRAP} 0 0 0 0)")
                .expect("writing to a string succeeds");
            return Ok(());
        };
        let outcome = match model.outcome() {
            ir::ModelOutcome::Trap => MODEL_TRAP,
            ir::ModelOutcome::Known => MODEL_KNOWN,
            ir::ModelOutcome::Attempt => MODEL_ATTEMPT,
        };
        let tokens = match &model.window {
            Some(window) => word(window.tokens.value, "a model's window")?,
            None => 0,
        };
        let mut settings = Vec::new();
        for (index, setting) in model.settings.iter().enumerate() {
            let field = ir::Field {
                name: setting.name.clone(),
                ty: setting.ty.clone(),
            };
            settings.push(self.field(&format!("$setting{index}"), &field)?);
        }
        writeln!(self.out, "(words $model_settings {})", settings.join(" "))
            .expect("writing to a string succeeds");
        let wire = match model.wire {
            None => WIRE_NONE,
            Some(ir::Wire::Anthropic) => WIRE_ANTHROPIC,
            Some(ir::Wire::Openai) => WIRE_OPENAI,
        };
        writeln!(
            self.out,
            "(words $model {outcome} {tokens} {} $model_settings {wire})",
            settings.len()
        )
        .expect("writing to a string succeeds");
        Ok(())
    }

    fn listing(&mut self, index: usize, listing: &ir::Listing) -> Result<String, String> {
        let label = format!("$listing{index}");
        let mut params = Vec::new();
        for (position, (param, default)) in listing.params.iter().zip(&listing.defaults).enumerate()
        {
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
        let name = self.text(&listing.name);
        let returns = self.ty(&Type::Text)?;
        let bound = self.text(&listing.bound.name);
        writeln!(self.out, "(words {label}_params {})", params.join(" "))
            .expect("writing to a string succeeds");
        writeln!(
            self.out,
            "(words {label} {name} {} {} {label}_params {returns} {} {bound} {} {index})",
            effect_code(listing.effect),
            listing.params.len(),
            word(listing.bound.value, "a listing's bound")?,
            u32::from(self.program.deferred(index))
        )
        .expect("writing to a string succeeds");
        Ok(label)
    }

    fn extras(&mut self) -> Result<(), String> {
        let program = self.program;
        let mut listings = Vec::new();
        for (index, listing) in program.listings.iter().enumerate() {
            listings.push(self.listing(index, listing)?);
        }
        writeln!(self.out, "(words $listings_items {})", listings.join(" "))
            .expect("writing to a string succeeds");
        writeln!(
            self.out,
            "(words $listings {} $listings_items)",
            listings.len()
        )
        .expect("writing to a string succeeds");
        let mut more = Vec::new();
        for (index, tool) in program.tools.iter().enumerate() {
            let label = format!("$tool{index}_more");
            let blob = match crate::run::blob::returned_bound(program, &tool.name) {
                Some(bytes) => format!(
                    "1 {} {}",
                    word(bytes.value, "a blob's bound")?,
                    self.text(&bytes.name)
                ),
                None => "0 0 0".to_owned(),
            };
            let finds = tool
                .finds
                .map_or_else(|| "0".to_owned(), |listing| format!("$listing{listing}"));
            writeln!(self.out, "(words {label} {blob} {finds})")
                .expect("writing to a string succeeds");
            more.push(label);
        }
        writeln!(self.out, "(words $tools_more {})", more.join(" "))
            .expect("writing to a string succeeds");
        let blobs = match &program.blobs {
            Some(blobs) => format!(
                "1 {} {} {}",
                self.sum(blobs.dispatched)?,
                blobs.attached,
                ir::ENTRY_RESULT
            ),
            None => "0 0 0 0".to_owned(),
        };
        writeln!(self.out, "(words $blobs {blobs})").expect("writing to a string succeeds");
        let wallet = program
            .wallet
            .as_ref()
            .map_or_else(|| "0".to_owned(), |wallet| self.text(&wallet.name));
        writeln!(self.out, "(words $wallet {wallet})").expect("writing to a string succeeds");
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
        let spawns: Vec<&String> = program
            .tools
            .iter()
            .zip(&tools)
            .filter(|(tool, _)| tool.spawn.is_some())
            .map(|(_, label)| label)
            .collect();
        writeln!(
            self.out,
            "(words $spawn_tools {} {})",
            spawns.len(),
            spawns
                .iter()
                .map(|label| label.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )
        .expect("writing to a string succeeds");
        let mut handlers = Vec::new();
        for (index, handler) in program.handlers.iter().enumerate() {
            handlers.push(self.handler(index, handler)?);
        }
        let mut sides = Vec::new();
        for (position, side) in program.sides.iter().enumerate() {
            sides.push(self.handler(program.handlers.len() + position, side)?);
        }
        writeln!(
            self.out,
            "(words $program_sides {} {})",
            sides.len(),
            sides.join(" ")
        )
        .expect("writing to a string succeeds");
        let (model, calls) = match &program.model {
            Some(model) => (
                self.text(&model.name),
                word(model.calls.value, "a model's calls bound")?,
            ),
            None => ("0".to_owned(), 0),
        };
        self.model()?;
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
        self.extras()?;
        self.codes()?;
        let model = target::model(target);
        let checksum = u32::from(target == Target::NativeJsonl);
        let in_step = u32::from(matches!(model.recovery, target::Recovery::InStep { .. }));
        let (limit, limited) = match model.limits.concurrent_requests {
            Some(limit) => (word(limit.value, "a concurrent request limit")?, 1),
            None => (0, 0),
        };
        writeln!(
            self.out,
            "(words $target {checksum} {} {in_step} {limit} {limited})",
            u32::from(model.gate_before_outbound)
        )
        .expect("writing to a string succeeds");
        self.idle()?;
        self.events(target)
    }

    fn bound(&mut self, bound: &ir::Bound, what: &str) -> Result<String, String> {
        Ok(format!(
            "{} {}",
            word(bound.value, what)?,
            self.text(&bound.name)
        ))
    }

    fn waits(&mut self, index: usize, waits: &ir::Waits) -> Result<String, String> {
        let label = format!("$waits{index}");
        let mut params = Vec::new();
        for (position, (param, default)) in waits.params.iter().zip(&waits.defaults).enumerate() {
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
        let name = self.text(&waits.name);
        let returns = self.ty(&Type::Text)?;
        writeln!(self.out, "(words {label}_params {})", params.join(" "))
            .expect("writing to a string succeeds");
        writeln!(
            self.out,
            "(words {label} {name} {} {} {label}_params {returns} {FORK_NONE} 0)",
            effect_code(EffectClass::None),
            waits.params.len()
        )
        .expect("writing to a string succeeds");
        Ok(label)
    }

    fn codes(&mut self) -> Result<(), String> {
        let program = self.program;
        let mut codes = Vec::new();
        for (index, code) in program.codes.iter().enumerate() {
            let label = format!("$code{index}");
            let tools: Vec<String> = code
                .tools
                .iter()
                .map(|tool| format!("$tool{tool}"))
                .collect();
            writeln!(self.out, "(words {label}_tools {})", tools.join(" "))
                .expect("writing to a string succeeds");
            let name = self.text(&code.name);
            let calls = self.bound(&code.calls, "a cell's calls bound")?;
            let parallel = self.bound(&code.parallel, "a cell's parallel bound")?;
            let timers = self.bound(&code.timers, "a cell's timers bound")?;
            let timer_ms = self.bound(&code.timer_ms, "a cell's timer_ms bound")?;
            let journal_bytes = self.bound(&code.journal_bytes, "a cell's journal_bytes bound")?;
            let yield_ms = word(code.yield_ms.value, "a cell's yield_ms")?;
            let waits = match &code.waits {
                Some(waits) => self.waits(index, waits)?,
                None => "0".to_owned(),
            };
            let header = Json::Array(crate::run::cell::header_tools(program, code)).encode();
            let header = self.text(&header);
            writeln!(
                self.out,
                "(words {label} {name} {} {label}_tools {calls} {parallel} {timers} {timer_ms} {journal_bytes} {yield_ms} {waits} {} {header})",
                tools.len(),
                interrupt_bits(code.interrupts)
            )
            .expect("writing to a string succeeds");
            codes.push(label);
        }
        writeln!(self.out, "(words $codes_items {})", codes.join(" "))
            .expect("writing to a string succeeds");
        writeln!(
            self.out,
            "(words $codes {} $codes_items {})",
            codes.len(),
            u32::from(program.cells_yield())
        )
        .expect("writing to a string succeeds");
        Ok(())
    }

    fn idle(&mut self) -> Result<(), String> {
        let program = self.program;
        let words = match &program.idle {
            Some(idle) => {
                let handler = program
                    .handlers
                    .iter()
                    .position(|handler| handler.name == idle.handler)
                    .expect("the idle handler is a handler of the program");
                format!(
                    "1 {handler} {} {} {}",
                    idle.due,
                    idle.runs,
                    word(idle.bound.value, "an idle turn's bound")?
                )
            }
            None => "0 0 0 0 0".to_owned(),
        };
        writeln!(self.out, "(words $idle {words})").expect("writing to a string succeeds");
        Ok(())
    }

    fn handler(&mut self, index: usize, handler: &ir::Handler) -> Result<String, String> {
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
            "(words $handler{index} {name} {} $handler{index}_params {returns} {index} {})",
            params.len(),
            word(handler.worst.effects.total, "a handler's effects")?
        )
        .expect("writing to a string succeeds");
        Ok(format!("$handler{index}"))
    }

    fn events(&mut self, target: Target) -> Result<(), String> {
        let events = &self.program.events;
        let bound = |bound: Option<&ir::Bound>, what: &str| {
            bound.map_or(Ok::<(u32, u32), String>((0, 0)), |bound| {
                Ok((1, word(bound.value, what)?))
            })
        };
        let (steers, steers_max) = bound(events.steers.as_ref(), "a steer bound")?;
        let (jobs, jobs_max) = bound(events.jobs.as_ref(), "a job bound")?;
        let records_max = word(events.records_max(), "a turn's event records")?;
        let after_flush =
            u32::from(target::inbound::of(target).drain == target::inbound::Drain::AfterFlush);
        let model = self
            .program
            .model
            .as_ref()
            .map_or(0, |model| interrupt_bits(model.interrupts));
        writeln!(
            self.out,
            "(words $events {} {steers} {steers_max} {jobs} {jobs_max} {records_max} {after_flush} {} {model})",
            u32::from(events.cancel),
            u32::from(events.resume)
        )
        .expect("writing to a string succeeds");
        Ok(())
    }
}
