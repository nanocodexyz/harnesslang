use std::fmt::Write;

use crate::ir::{
    BinaryOp, Durability, Expr, Handler, Kept, Program, Site, SiteClass, SiteKind, Stmt, SumKind,
    Type,
};
use crate::target::{self, Target};

const RUNTIME: &str = include_str!("runtime.js");
const CELL_JOURNAL: &str = include_str!("cell_journal.js");
const CELL_RUNNER: &str = include_str!("cell_runner.js");
const CELL_ENGINE: &str = include_str!("cell_engine.js");
const CELL_ISOLATE: &str = include_str!("cell_isolate.js");
const ENGINE: &str = include_str!("engine.js");
const CLOUDFLARE: &str = include_str!("cloudflare.js");
const VERCEL: &str = include_str!("vercel.js");
const INDENT: &str = "  ";
const LINE_SEPARATOR: char = '\u{2028}';
const PARAGRAPH_SEPARATOR: char = '\u{2029}';
const CONTROL_END: u32 = 0x20;

pub fn emit(program: &Program, target: Target) -> Result<String, String> {
    let adapter = match target {
        Target::Cloudflare => CLOUDFLARE,
        Target::Vercel => VERCEL,
        other => {
            return Err(format!(
                "the JavaScript backend emits cloudflare and vercel, not {}",
                other.name()
            ));
        }
    };
    let model = target::model(target);
    let errors = target::check_limits(program, &model);
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    let mut out = target_literal(&model);
    let concurrent = target::model(target)
        .limits
        .concurrent_requests
        .map_or_else(|| "null".to_owned(), |limit| limit.value.to_string());
    writeln!(out, "const CONCURRENT_REQUESTS_MAX = {concurrent};")
        .expect("writing to a string succeeds");
    for constant in &program.constants {
        writeln!(
            out,
            "const constant_{} = {}n;",
            constant.name, constant.value
        )
        .expect("writing to a string succeeds");
    }
    out.push_str(&program_literal(program));
    out.push_str(RUNTIME);
    out.push_str(CELL_JOURNAL);
    out.push_str(CELL_RUNNER);
    out.push_str(CELL_ENGINE);
    if target == Target::Cloudflare {
        let isolate = [CELL_ISOLATE, RUNTIME, CELL_JOURNAL, CELL_ENGINE].concat();
        writeln!(out, "const CELL_ISOLATE_SOURCE = {};", js_string(&isolate))
            .expect("writing to a string succeeds");
    }
    out.push_str(ENGINE);
    for (index, handler) in program.handlers.iter().enumerate() {
        out.push_str(&handler_function(program, index, handler));
    }
    out.push_str(adapter);
    Ok(out)
}

fn target_literal(model: &target::Model) -> String {
    let recovery = match model.recovery {
        target::Recovery::AtLoad => "at_load",
        target::Recovery::InStep { .. } => "in_step",
    };
    format!(
        "const TARGET_MODEL = {{ recovery: \"{recovery}\", gateBeforeOutbound: {}, waitFlushes: {} }};\n",
        model.gate_before_outbound,
        model.wait_flushes()
    )
}

pub fn js_string(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            control
                if u32::from(control) < CONTROL_END
                    || control == LINE_SEPARATOR
                    || control == PARAGRAPH_SEPARATOR =>
            {
                write!(out, "\\u{:04x}", u32::from(control)).expect("writing to a string succeeds");
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn type_literal(ty: &Type) -> String {
    match ty {
        Type::U64 => "\"u64\"".to_owned(),
        Type::Bool => "\"bool\"".to_owned(),
        Type::Text => "\"text\"".to_owned(),
        Type::Sum(sum) => format!("{{ sum: {sum} }}"),
        Type::List { element, capacity } => format!(
            "{{ list: {}, capacity: {}n, bound: {} }}",
            type_literal(element),
            capacity.value,
            js_string(&capacity.name)
        ),
        Type::Known(inner) => format!("{{ known: {} }}", type_literal(inner)),
        Type::Maybe(inner) => format!("{{ maybe: {} }}", type_literal(inner)),
    }
}

fn defaults_literal(defaults: &[Option<crate::ir::Literal>]) -> String {
    let items: Vec<String> = defaults
        .iter()
        .map(|default| match default {
            None => "null".to_owned(),
            Some(crate::ir::Literal::U64(value)) => format!("{value}n"),
            Some(crate::ir::Literal::Bool(value)) => value.to_string(),
            Some(crate::ir::Literal::Text(value)) => js_string(value),
        })
        .collect();
    format!("[{}]", items.join(", "))
}

fn fields_literal(fields: &[crate::ir::Field]) -> String {
    let items: Vec<String> = fields
        .iter()
        .map(|field| {
            format!(
                "{{ name: {}, ty: {} }}",
                js_string(&field.name),
                type_literal(&field.ty)
            )
        })
        .collect();
    format!("[{}]", items.join(", "))
}

fn site_literal(program: &Program, site: &Site) -> String {
    let tool = match site.kind {
        SiteKind::Tool(tool) | SiteKind::Listing(tool) => tool.to_string(),
        SiteKind::Dispatch
        | SiteKind::Model
        | SiteKind::Now
        | SiteKind::Random
        | SiteKind::Wait(_)
        | SiteKind::Suspend
        | SiteKind::Deny
        | SiteKind::Cell(_) => "null".to_owned(),
    };
    let (ms, cap) = match site.kind {
        SiteKind::Wait(constant) => (
            format!("{}n", program.constants[constant].value),
            js_string(&program.constants[constant].name),
        ),
        _ => ("null".to_owned(), "null".to_owned()),
    };
    let code = match site.kind {
        SiteKind::Cell(code) => code.to_string(),
        _ => "null".to_owned(),
    };
    let class = match site.class {
        SiteClass::Fixed(effect) => effect.name(),
        SiteClass::Dispatched => "dispatched",
        SiteClass::Cell => "cell",
    };
    let durability = match site.durability {
        Durability::RidesNextFlush => "rides_next_flush",
        Durability::FlushBeforeRun => "flush_before_run",
        Durability::FlushBeforeRunWhenExternal => "flush_before_run_when_external",
        Durability::Cell => "cell",
    };
    let mode = site
        .mode
        .map_or("null".to_owned(), |mode| js_string(program.mode_name(mode)));
    format!(
        "{{ kind: \"{}\", tool: {tool}, code: {code}, ms: {ms}, cap: {cap}, class: \"{class}\", result: {}, durability: \"{durability}\", mode: {mode} }}",
        site.kind.name(),
        type_literal(&site.result)
    )
}

fn program_literal(program: &Program) -> String {
    let mut out = format!(
        "const PROGRAM = {{\n  name: {},\n  sums: [\n",
        js_string(&program.name)
    );
    for sum in &program.sums {
        let variants: Vec<String> = sum
            .variants
            .iter()
            .map(|variant| {
                let fields: Vec<String> = variant.fields.iter().map(type_literal).collect();
                format!(
                    "{{ name: {}, fields: [{}] }}",
                    js_string(&variant.name),
                    fields.join(", ")
                )
            })
            .collect();
        let text = match sum.kind {
            SumKind::Tagged => "",
            SumKind::Text => ", text: true",
        };
        writeln!(
            out,
            "    {{ name: {}, variants: [{}]{text} }},",
            js_string(&sum.name),
            variants.join(", ")
        )
        .expect("writing to a string succeeds");
    }
    out.push_str("  ],\n  tools: [\n");
    for tool in &program.tools {
        writeln!(
            out,
            "    {{ name: {}, params: {}, defaults: {}, returns: {}, effect: \"{}\"{} }},",
            js_string(&tool.name),
            fields_literal(&tool.params),
            defaults_literal(&tool.defaults),
            type_literal(&tool.returns),
            tool.effect.name(),
            if tool.spawn.is_some() {
                ", spawn: true"
            } else {
                ""
            }
        )
        .expect("writing to a string succeeds");
    }
    out.push_str("  ],\n  codes: [\n");
    for code in &program.codes {
        out.push_str(&code_literal(code));
    }
    out.push_str("  ],\n  listings: [\n");
    for listing in &program.listings {
        writeln!(
            out,
            "    {{ name: {}, bound: {}n, boundName: {}, params: {}, defaults: {}, effect: \"{}\" }},",
            js_string(&listing.name),
            listing.bound.value,
            js_string(&listing.bound.name),
            fields_literal(&listing.params),
            defaults_literal(&listing.defaults),
            listing.effect.name()
        )
        .expect("writing to a string succeeds");
    }
    let model = program.model.as_ref().map_or("null".to_owned(), |model| {
        let window = model.window.as_ref().map_or("null".to_owned(), |window| {
            format!(
                "{{ tokens: {}n, attempt: {} }}",
                window.tokens.value, window.attempt
            )
        });
        format!(
            "{{ name: {}, calls: {}n, declared: {}, window: {window} }}",
            js_string(&model.name),
            model.calls.value,
            model.declared()
        )
    });
    let events = &program.events;
    let steers = events.steers.as_ref().map_or("null".to_owned(), |bound| {
        format!(
            "{{ name: {}, max: {}n }}",
            js_string(&bound.name),
            bound.value
        )
    });
    write!(
        out,
        "  ],\n  model: {model},\n  events: {{ cancel: {}, steers: {steers}, recordsMax: {}n }},\n  state: {},\n  handlers: [\n",
        events.cancel,
        events.records_max(),
        fields_literal(&program.state)
    )
    .expect("writing to a string succeeds");
    for (index, handler) in program.handlers.iter().enumerate() {
        out.push_str(&handler_literal(program, index, handler));
    }
    out.push_str("  ],\n};\n");
    out
}

fn bound_literal(bound: &crate::ir::Bound) -> String {
    format!(
        "{{ name: {}, value: {}n }}",
        js_string(&bound.name),
        bound.value
    )
}

fn code_literal(code: &crate::ir::Code) -> String {
    let tools: Vec<String> = code.tools.iter().map(ToString::to_string).collect();
    format!(
        "    {{ name: {}, tools: [{}], calls: {}, parallel: {}, timers: {}, timerMs: {}, journalBytes: {}, yieldMs: {} }},\n",
        js_string(&code.name),
        tools.join(", "),
        bound_literal(&code.calls),
        bound_literal(&code.parallel),
        bound_literal(&code.timers),
        bound_literal(&code.timer_ms),
        bound_literal(&code.journal_bytes),
        bound_literal(&code.yield_ms)
    )
}

fn handler_literal(program: &Program, index: usize, handler: &Handler) -> String {
    let params: Vec<crate::ir::Field> = handler
        .params
        .iter()
        .map(|local| handler.locals[*local].clone())
        .collect();
    let sites: Vec<String> = handler
        .sites
        .iter()
        .map(|site| site_literal(program, site))
        .collect();
    format!(
        "    {{\n      name: {},\n      params: {},\n      returns: {},\n      effectsMax: {},\n      sites: [\n        {}\n      ],\n      run: handler_{index},\n    }},\n",
        js_string(&handler.name),
        fields_literal(&params),
        type_literal(&handler.returns),
        handler.worst.effects.total,
        sites.join(",\n        ")
    )
}

fn handler_function(program: &Program, index: usize, handler: &Handler) -> String {
    let mut out = format!("async function handler_{index}(rt, state, args) {{\n");
    if !handler.locals.is_empty() {
        let locals: Vec<String> = (0..handler.locals.len())
            .map(|local| format!("local_{local}"))
            .collect();
        writeln!(out, "{INDENT}let {};", locals.join(", ")).expect("writing to a string succeeds");
    }
    for (position, local) in handler.params.iter().enumerate() {
        writeln!(out, "{INDENT}local_{local} = args[{position}];")
            .expect("writing to a string succeeds");
    }
    if let Some(requires) = &handler.requires {
        writeln!(
            out,
            "{INDENT}if (!({})) throw new Trap({});",
            expr(program, requires),
            js_string(&format!("requires({})", handler.name))
        )
        .expect("writing to a string succeeds");
    }
    let mut writer = Writer {
        program,
        out,
        depth: 1,
    };
    writer.block(&handler.body);
    let mut out = writer.out;
    out.push_str("}\n\n");
    out
}

struct Writer<'a> {
    program: &'a Program,
    out: String,
    depth: usize,
}

impl Writer<'_> {
    fn line(&mut self, text: &str) {
        for _ in 0..self.depth {
            self.out.push_str(INDENT);
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn nested(&mut self, statements: &[Stmt]) {
        self.depth += 1;
        self.block(statements);
        self.depth -= 1;
    }

    fn block(&mut self, statements: &[Stmt]) {
        for statement in statements {
            self.statement(statement);
        }
    }

    fn statement(&mut self, statement: &Stmt) {
        let program = self.program;
        let depth = self.depth;
        match statement {
            Stmt::Let { local, value } => {
                self.line(&format!("local_{local} = {};", expr(program, value)))
            }
            Stmt::Assign { field, value } => {
                self.line(&format!("state[{field}] = {};", expr(program, value)))
            }
            Stmt::Push { field, value } => {
                self.line(&format!("rt.push({field}, {});", expr(program, value)))
            }
            Stmt::Shrink { field, kept } => match kept {
                Kept::Nothing => self.line(&format!("rt.keep({field}, 0n);")),
                Kept::Last(count) => {
                    self.line(&format!("rt.keep({field}, {});", expr(program, count)));
                }
                Kept::LastAfter { count, first } => self.line(&format!(
                    "rt.keepAfter({field}, {}, {});",
                    expr(program, count),
                    expr(program, first)
                )),
            },
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                self.line(&format!("if ({}) {{", expr(program, condition)));
                self.nested(then);
                self.line("} else {");
                self.nested(otherwise);
                self.line("}");
            }
            Stmt::Match { subject, arms, .. } => {
                self.line("{");
                self.depth += 1;
                self.line(&format!(
                    "const subject_{depth} = {};",
                    expr(program, subject)
                ));
                self.line(&format!("switch (subject_{depth}.v) {{"));
                for (position, arm) in arms.iter().enumerate() {
                    assert_eq!(arm.variant, position, "arms are in variant order");
                    self.line(&format!("case {position}: {{"));
                    self.depth += 1;
                    for (field, binding) in arm.bindings.iter().enumerate() {
                        if let Some(local) = binding {
                            self.line(&format!("local_{local} = subject_{depth}.f[{field}];"));
                        }
                    }
                    self.block(&arm.body);
                    self.line("break;");
                    self.depth -= 1;
                    self.line("}");
                }
                self.line("default:");
                self.line(&format!(
                    "{INDENT}throw new Panic(\"a match covers every variant\");"
                ));
                self.line("}");
                self.depth -= 1;
                self.line("}");
            }
            Stmt::Loop { index, bound, body } => {
                self.line(&format!(
                    "for (let index_{depth} = 0n; index_{depth} < {}n; index_{depth} += 1n) {{",
                    bound.value
                ));
                self.depth += 1;
                self.line(&format!("local_{index} = index_{depth};"));
                self.block(body);
                self.depth -= 1;
                self.line("}");
            }
            Stmt::For {
                item,
                list,
                bound,
                body,
            } => {
                self.line(&format!(
                    "for (const item_{depth} of within({}, {}n)) {{",
                    expr(program, list),
                    bound.value
                ));
                self.depth += 1;
                self.line(&format!("local_{item} = item_{depth};"));
                self.block(body);
                self.depth -= 1;
                self.line("}");
            }
            Stmt::Parallel(parallel) => {
                self.line("{");
                self.depth += 1;
                self.line(&format!(
                    "const items_{depth} = within({}, {}n);",
                    expr(program, &parallel.list),
                    parallel.bound.value
                ));
                self.line(&format!(
                    "const values_{depth} = await rt.parallel({}, items_{depth});",
                    parallel.site
                ));
                self.line(&format!(
                    "for (let index_{depth} = 0; index_{depth} < items_{depth}.length; index_{depth}++) {{"
                ));
                self.depth += 1;
                self.line(&format!(
                    "local_{} = items_{depth}[index_{depth}];",
                    parallel.item
                ));
                self.line(&format!(
                    "local_{} = values_{depth}[index_{depth}];",
                    parallel.result
                ));
                self.block(&parallel.body);
                self.depth -= 1;
                self.line("}");
                self.depth -= 1;
                self.line("}");
            }
            Stmt::Effect { site, args, result } => {
                let args: Vec<String> = args.iter().map(|arg| expr(program, arg)).collect();
                self.line(&format!(
                    "local_{result} = await rt.effect({site}, [{}]);",
                    args.join(", ")
                ));
            }
            Stmt::Take { result } => self.line(&format!("local_{result} = rt.take();")),
            Stmt::Finish { value } => self.line(&format!("return {};", expr(program, value))),
        }
    }
}

fn binary(op: BinaryOp, left: String, right: String) -> String {
    match op {
        BinaryOp::Add => format!("add({left}, {right})"),
        BinaryOp::Subtract => format!("subtract({left}, {right})"),
        BinaryOp::Multiply => format!("multiply({left}, {right})"),
        BinaryOp::Concat => format!("({left} + {right})"),
        BinaryOp::Equal => format!("valueEqual({left}, {right})"),
        BinaryOp::NotEqual => format!("!valueEqual({left}, {right})"),
        BinaryOp::Less => format!("({left} < {right})"),
        BinaryOp::LessEqual => format!("({left} <= {right})"),
        BinaryOp::Greater => format!("({left} > {right})"),
        BinaryOp::GreaterEqual => format!("({left} >= {right})"),
        BinaryOp::And => format!("both({left}, {right})"),
        BinaryOp::Or => format!("either({left}, {right})"),
    }
}

fn expr(program: &Program, expression: &Expr) -> String {
    match expression {
        Expr::U64(value) => format!("{value}n"),
        Expr::Bool(value) => value.to_string(),
        Expr::Text(value) => js_string(value),
        Expr::Constant(constant) => format!("constant_{}", program.constants[*constant].name),
        Expr::Local(local) => format!("local_{local}"),
        Expr::Field(field) => format!("state[{field}]"),
        Expr::Member { base, index } => format!("({}).f[{index}]", expr(program, base)),
        Expr::Construct {
            variant, fields, ..
        } => {
            let fields: Vec<String> = fields.iter().map(|field| expr(program, field)).collect();
            format!("variant({variant}, [{}])", fields.join(", "))
        }
        Expr::Binary {
            op, left, right, ..
        } => binary(*op, expr(program, left), expr(program, right)),
        Expr::Not(operand) => format!("!({})", expr(program, operand)),
        Expr::Len(operand) | Expr::TextLen(operand) => {
            format!("BigInt(({}).length)", expr(program, operand))
        }
        Expr::Truncate { text, tokens } => format!(
            "truncateMiddle({}, constant_{})",
            expr(program, text),
            program.constants[*tokens].name
        ),
        Expr::With { list, entry } => {
            format!("[...({}), {}]", expr(program, list), expr(program, entry))
        }
        Expr::TextBuiltin { op, args } => {
            let args: Vec<String> = args.iter().map(|arg| expr(program, arg)).collect();
            format!("text_{}({})", op.name(), args.join(", "))
        }
        Expr::Cancelled => "rt.cancelled()".to_owned(),
    }
}
