use std::fmt::Write;

use super::data::{Tables, context_type};
use crate::ir::{self, BinaryOp, Expr, Handler, Program, Stmt, Type};

const SLOT_BYTES: usize = 8;
const SUM_FIELDS_OFFSET: usize = 8;
const SUM_FIELDS_DIRECT_MAX: usize = 3;
const LOW_WORD: u64 = 0xffff_ffff;

struct Function<'t, 'p> {
    tables: &'t mut Tables<'p>,
    handler: &'p Handler,
    index: usize,
    body: Vec<String>,
    temps: usize,
    labels: usize,
}

fn not_yet<T>(what: &str) -> Result<T, String> {
    Err(format!("emit: not yet in wasm: {what}"))
}

impl<'p> Function<'_, 'p> {
    fn emit(&mut self, line: impl Into<String>) {
        self.body.push(line.into());
    }

    fn temp(&mut self) -> String {
        self.temps += 1;
        format!("$t{}", self.temps)
    }

    fn label(&mut self) -> usize {
        self.labels += 1;
        self.labels
    }

    fn halt_check(&mut self) {
        self.emit("global.get $halt");
        self.emit("br_if $exit");
    }

    fn program(&self) -> &'p Program {
        self.tables.program
    }

    fn expr(&mut self, expr: &Expr) -> Result<(), String> {
        match expr {
            Expr::U64(value) => self.emit(format!("i64.const {value}")),
            Expr::Bool(value) => self.emit(format!("i64.const {}", u64::from(*value))),
            Expr::Text(value) => {
                let text = self.tables.text(value);
                self.emit(format!("i64.const {text}"));
            }
            Expr::Constant(constant) => self.emit(format!(
                "i64.const {}",
                self.program().constants[*constant].value
            )),
            Expr::Local(local) => self.emit(format!("local.get $l{local}")),
            Expr::Field(field) => {
                self.emit("global.get $state");
                self.emit(format!("i64.load offset={}", field * SLOT_BYTES));
            }
            Expr::Member { base, index } => {
                self.expr(base)?;
                self.emit("i32.wrap_i64");
                self.emit(format!(
                    "i64.load offset={}",
                    SUM_FIELDS_OFFSET + index * SLOT_BYTES
                ));
            }
            Expr::Construct {
                variant, fields, ..
            } => {
                if fields.len() > SUM_FIELDS_DIRECT_MAX {
                    return not_yet("variants of more than three fields");
                }
                self.emit(format!("i32.const {variant}"));
                for field in fields {
                    self.expr(field)?;
                }
                self.emit(format!("call $sum_{}", fields.len()));
            }
            Expr::Binary {
                op,
                operand,
                left,
                right,
            } => self.binary(*op, operand, left, right)?,
            Expr::Not(operand) => {
                self.expr(operand)?;
                self.emit("i64.eqz");
                self.emit("i64.extend_i32_u");
            }
            Expr::Len(operand) => {
                self.expr(operand)?;
                self.emit(format!("i64.const {LOW_WORD}"));
                self.emit("i64.and");
            }
            Expr::TextLen(_) => return not_yet("len of text"),
            Expr::Truncate { .. } => return not_yet("truncate_middle"),
            Expr::With { .. } => return not_yet("with"),
            Expr::TextBuiltin { .. } => return not_yet("text builtins"),
            Expr::ListingNotice(_) => return not_yet("listing notices"),
            Expr::Cancelled => return not_yet("cancelled"),
        }
        Ok(())
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        operand: &Type,
        left: &Expr,
        right: &Expr,
    ) -> Result<(), String> {
        self.expr(left)?;
        if op == BinaryOp::Concat {
            self.emit("i32.wrap_i64");
        }
        self.expr(right)?;
        let compare = |name: &str| vec![name.to_owned(), "i64.extend_i32_u".to_owned()];
        let lines = match op {
            BinaryOp::Add => vec!["call $u64_add".to_owned()],
            BinaryOp::Subtract => vec!["call $u64_subtract".to_owned()],
            BinaryOp::Multiply => vec!["call $u64_multiply".to_owned()],
            BinaryOp::Concat => vec![
                "i32.wrap_i64".to_owned(),
                "call $text_concat".to_owned(),
                "i64.extend_i32_u".to_owned(),
            ],
            BinaryOp::Less => compare("i64.lt_u"),
            BinaryOp::LessEqual => compare("i64.le_u"),
            BinaryOp::Greater => compare("i64.gt_u"),
            BinaryOp::GreaterEqual => compare("i64.ge_u"),
            BinaryOp::And => vec!["i64.and".to_owned()],
            BinaryOp::Or => vec!["i64.or".to_owned()],
            BinaryOp::Equal | BinaryOp::NotEqual => {
                let mut lines = match operand {
                    Type::U64 | Type::Bool => vec!["i64.eq".to_owned()],
                    Type::Text => vec!["call $value_text_eq".to_owned()],
                    _ => return not_yet("equality of lists and sums"),
                };
                if op == BinaryOp::NotEqual {
                    lines.push("i32.eqz".to_owned());
                }
                lines.push("i64.extend_i32_u".to_owned());
                lines
            }
        };
        for line in lines {
            self.emit(line);
        }
        Ok(())
    }

    fn block(&mut self, statements: &[Stmt]) -> Result<(), String> {
        for statement in statements {
            self.statement(statement)?;
        }
        Ok(())
    }

    fn statement(&mut self, statement: &Stmt) -> Result<(), String> {
        match statement {
            Stmt::Let { local, value } => {
                self.expr(value)?;
                self.halt_check();
                self.emit(format!("local.set $l{local}"));
            }
            Stmt::Assign { field, value } => {
                self.emit("global.get $state");
                self.expr(value)?;
                self.halt_check();
                self.emit(format!("i64.store offset={}", field * SLOT_BYTES));
            }
            Stmt::Push { field, value } => self.push(*field, value)?,
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition)?;
                self.halt_check();
                self.emit("i32.wrap_i64");
                self.emit("if");
                self.block(then)?;
                self.emit("else");
                self.block(otherwise)?;
                self.emit("end");
            }
            Stmt::Match { subject, arms, .. } => self.matched(subject, arms)?,
            Stmt::Loop { index, bound, body } => self.counted(*index, bound.value, body)?,
            Stmt::For {
                item, list, body, ..
            } => self.each(*item, list, body)?,
            Stmt::Effect { site, args, result } => self.effect(*site, args, *result)?,
            Stmt::Finish { value } => {
                self.expr(value)?;
                self.halt_check();
                self.emit("local.set $result");
                self.emit("br $exit");
            }
            Stmt::Shrink { .. } => return not_yet("clear and keep"),
            Stmt::KeepBlobs { .. } => return not_yet("keep blobs"),
            Stmt::Rewrite { .. } => return not_yet("rewrite"),
            Stmt::Parallel(_) => return not_yet("parallel for"),
            Stmt::Take { .. } => return not_yet("take"),
        }
        Ok(())
    }

    fn push(&mut self, field: usize, value: &Expr) -> Result<(), String> {
        let definition = &self.program().state[field];
        let Type::List { capacity, .. } = &definition.ty else {
            panic!("the checker pushes only onto lists");
        };
        let capacity = capacity.value;
        let name = self.tables.text(&definition.name);
        self.emit("global.get $state");
        self.emit("global.get $state");
        self.emit(format!("i64.load offset={}", field * SLOT_BYTES));
        self.expr(value)?;
        self.emit(format!("i64.const {capacity}"));
        self.emit(format!("i32.const {name}"));
        self.emit("call $list_push");
        self.halt_check();
        self.emit(format!("i64.store offset={}", field * SLOT_BYTES));
        Ok(())
    }

    fn matched(&mut self, subject: &Expr, arms: &[ir::Arm]) -> Result<(), String> {
        let held = self.temp();
        self.expr(subject)?;
        self.halt_check();
        self.emit(format!("local.set {held}"));
        for (variant, arm) in arms.iter().enumerate() {
            assert_eq!(arm.variant, variant, "arms are in variant order");
            self.emit(format!("local.get {held}"));
            self.emit("call $sum_variant");
            self.emit(format!("i32.const {variant}"));
            self.emit("i32.eq");
            self.emit("if");
            for (position, binding) in arm.bindings.iter().enumerate() {
                if let Some(local) = binding {
                    self.emit(format!("local.get {held}"));
                    self.emit(format!("i32.const {position}"));
                    self.emit("call $sum_field");
                    self.emit(format!("local.set $l{local}"));
                }
            }
            self.block(&arm.body)?;
            self.emit("end");
        }
        Ok(())
    }

    fn counted(&mut self, index: usize, bound: u64, body: &[Stmt]) -> Result<(), String> {
        let label = self.label();
        self.emit("i64.const 0");
        self.emit(format!("local.set $l{index}"));
        self.emit(format!("block $break{label}"));
        self.emit(format!("loop $continue{label}"));
        self.emit(format!("local.get $l{index}"));
        self.emit(format!("i64.const {bound}"));
        self.emit("i64.ge_u");
        self.emit(format!("br_if $break{label}"));
        self.block(body)?;
        self.emit(format!("local.get $l{index}"));
        self.emit("i64.const 1");
        self.emit("i64.add");
        self.emit(format!("local.set $l{index}"));
        self.emit(format!("br $continue{label}"));
        self.emit("end");
        self.emit("end");
        Ok(())
    }

    fn each(&mut self, item: usize, list: &Expr, body: &[Stmt]) -> Result<(), String> {
        let label = self.label();
        let items = self.temp();
        let at = self.temp();
        self.expr(list)?;
        self.halt_check();
        self.emit(format!("local.set {items}"));
        self.emit("i64.const 0");
        self.emit(format!("local.set {at}"));
        self.emit(format!("block $break{label}"));
        self.emit(format!("loop $continue{label}"));
        self.emit(format!("local.get {at}"));
        self.emit(format!("local.get {items}"));
        self.emit(format!("i64.const {LOW_WORD}"));
        self.emit("i64.and");
        self.emit("i64.ge_u");
        self.emit(format!("br_if $break{label}"));
        self.emit(format!("local.get {items}"));
        self.emit(format!("local.get {at}"));
        self.emit("i32.wrap_i64");
        self.emit("call $list_get");
        self.emit(format!("local.set $l{item}"));
        self.block(body)?;
        self.emit(format!("local.get {at}"));
        self.emit("i64.const 1");
        self.emit("i64.add");
        self.emit(format!("local.set {at}"));
        self.emit(format!("br $continue{label}"));
        self.emit("end");
        self.emit("end");
        Ok(())
    }

    fn effect(&mut self, site: usize, args: &[Expr], result: usize) -> Result<(), String> {
        self.emit(format!("i32.const {}", (args.len().max(1)) * SLOT_BYTES));
        self.emit("call $alloc");
        self.emit("local.set $args");
        for (position, arg) in args.iter().enumerate() {
            self.emit("local.get $args");
            self.expr(arg)?;
            self.emit(format!("i64.store offset={}", position * SLOT_BYTES));
        }
        self.halt_check();
        self.emit(format!("i32.const $site{}_{site}", self.index));
        self.emit("local.get $args");
        self.emit("call $effect");
        self.halt_check();
        self.emit(format!("local.set $l{result}"));
        Ok(())
    }
}

fn handler_function(tables: &mut Tables<'_>, index: usize) -> Result<String, String> {
    let program = tables.program;
    let handler = &program.handlers[index];
    for (site_index, site) in handler.sites.iter().enumerate() {
        if site.kind == ir::SiteKind::Model && site.result != Type::Sum(ir::REPLY) {
            return not_yet("models with a declared reply type or attempts");
        }
        tables.site(&format!("$site{index}_{site_index}"), site)?;
    }
    let mut function = Function {
        tables,
        handler,
        index,
        body: Vec::new(),
        temps: 0,
        labels: 0,
    };
    for (position, local) in handler.params.iter().enumerate() {
        function.emit("local.get $arguments");
        function.emit(format!("i64.load offset={}", position * SLOT_BYTES));
        function.emit(format!("local.set $l{local}"));
    }
    function.emit("block $exit");
    if let Some(requires) = &handler.requires {
        let trap = function.tables.text(&format!("requires({})", handler.name));
        function.expr(requires)?;
        function.halt_check();
        function.emit("i64.eqz");
        function.emit("if");
        function.emit(format!("i32.const {trap}"));
        function.emit("call $trap");
        function.emit("br $exit");
        function.emit("end");
    }
    function.block(&handler.body)?;
    function.emit("unreachable");
    function.emit("end");
    let mut text = format!(
        "(func $handler{index}_run (param $arguments i32) (result i64) (local $result i64) (local $args i32)"
    );
    for local in 0..function.handler.locals.len() {
        write!(text, " (local $l{local} i64)").expect("writing to a string succeeds");
    }
    for temp in 1..=function.temps {
        write!(text, " (local $t{temp} i64)").expect("writing to a string succeeds");
    }
    text.push('\n');
    for line in &function.body {
        writeln!(text, "  {line}").expect("writing to a string succeeds");
    }
    text.push_str("  local.get $result)\n");
    Ok(text)
}

fn refuse_unsupported(program: &Program) -> Result<(), String> {
    let refused = [
        (!program.codes.is_empty(), "code cells"),
        (!program.listings.is_empty(), "listings"),
        (program.events.any() || program.events.resume, "events"),
        (program.idle.is_some(), "idle turns"),
        (program.forked.is_some(), "fork hooks"),
        (program.blobs.is_some(), "blobs"),
        (!program.sides.is_empty(), "side questions"),
        (program.any_interruptible(), "interruptible effects"),
        (
            program.tools.iter().any(|tool| tool.recoverable),
            "recoverable tools",
        ),
        (
            program.tools.iter().any(|tool| tool.jobs.is_some()),
            "background jobs",
        ),
        (
            program.tools.iter().any(|tool| tool.spawn.is_some()),
            "spawn tools",
        ),
        (
            program.tools.iter().any(|tool| tool.fork.is_some()),
            "fork tools",
        ),
        (
            program.tools.iter().any(|tool| tool.finds.is_some()),
            "search tools",
        ),
        (
            program.model.as_ref().is_some_and(|model| {
                model.window.is_some() || model.declared() || !model.settings.is_empty()
            }),
            "model windows, settings and declared replies",
        ),
    ];
    match refused.iter().find(|(refuses, _)| *refuses) {
        Some((_, what)) => not_yet(what),
        None => Ok(()),
    }
}

pub fn program_text(program: &Program, target: crate::target::Target) -> Result<String, String> {
    refuse_unsupported(program)?;
    let mut tables = Tables::new(program);
    tables.ty(&context_type())?;
    let mut functions = String::new();
    for index in 0..program.handlers.len() {
        functions.push_str(&handler_function(&mut tables, index)?);
    }
    tables.program(target)?;
    let mut run =
        String::from("(func $run_handler (param $index i32) (param $arguments i32) (result i64)\n");
    for index in 0..program.handlers.len() {
        writeln!(run, "  (if (i32.eq (local.get $index) (i32.const {index})) (then (return (call $handler{index}_run (local.get $arguments)))))").expect("writing to a string succeeds");
    }
    run.push_str("  unreachable)\n");
    Ok(format!("{}{functions}{run}", tables.out))
}
