use std::fmt::Write;

use super::data::{Tables, context_type};
use crate::ir::{self, BinaryOp, Expr, Handler, Program, Stmt, Type};

const ANSWER_BYTES: usize = 4;
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
    facts: Vec<(usize, Option<u64>)>,
    fixed: Vec<bool>,
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

    fn fact(&self, local: usize) -> Option<u64> {
        self.facts
            .iter()
            .rev()
            .find(|(known, _)| *known == local)
            .and_then(|(_, most)| *most)
    }

    fn learn(&mut self, local: usize, most: Option<u64>) {
        if !self.fixed.get(local).copied().unwrap_or(false) {
            return;
        }
        let most = match (self.fact(local), most) {
            (Some(known), Some(new)) => Some(known.min(new)),
            (_, new) => new,
        };
        self.facts.push((local, most));
    }

    fn scoped<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let kept = self.facts.len();
        let result = body(self);
        self.facts.truncate(kept);
        result
    }

    fn capacity_of(&self, expr: &Expr) -> Option<u64> {
        let Expr::Local(local) = expr else {
            return None;
        };
        match &self.handler.locals[*local].ty {
            Type::Bytes(capacity) | Type::List { capacity, .. } => Some(capacity.value),
            _ => None,
        }
    }

    fn divisor(&self, expr: &Expr) -> Option<u64> {
        match expr {
            Expr::U64(value) => Some(*value),
            Expr::Constant(constant) => Some(self.program().constants[*constant].value),
            _ => None,
        }
        .filter(|value| *value > 0)
    }

    fn most(&self, expr: &Expr) -> Option<u64> {
        match expr {
            Expr::U64(value) => Some(*value),
            Expr::Bool(_) | Expr::Not(_) => Some(1),
            Expr::Constant(constant) => Some(self.program().constants[*constant].value),
            Expr::Local(local) => self.fact(*local),
            Expr::Len(operand) => self.capacity_of(operand),
            Expr::Bytes {
                op: ir::BytesOp::Get,
                ..
            } => Some(u64::from(u8::MAX)),
            Expr::Binary {
                op, left, right, ..
            } => match op {
                BinaryOp::Add => self.most(left)?.checked_add(self.most(right)?),
                BinaryOp::Multiply => self.most(left)?.checked_mul(self.most(right)?),
                BinaryOp::Divide => self.divisor(right).and(self.most(left)),
                BinaryOp::Remainder => {
                    let below = self.divisor(right)? - 1;
                    Some(self.most(left).map_or(below, |most| most.min(below)))
                }
                BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual
                | BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::And
                | BinaryOp::Or => Some(1),
                BinaryOp::Subtract | BinaryOp::Concat => None,
            },
            _ => None,
        }
    }

    fn proven(&self, op: BinaryOp, left: &Expr, right: &Expr) -> bool {
        match op {
            BinaryOp::Add => self
                .most(left)
                .zip(self.most(right))
                .is_some_and(|(left, right)| left.checked_add(right).is_some()),
            BinaryOp::Multiply => self
                .most(left)
                .zip(self.most(right))
                .is_some_and(|(left, right)| left.checked_mul(right).is_some()),
            BinaryOp::Divide | BinaryOp::Remainder => self.divisor(right).is_some(),
            _ => false,
        }
    }

    fn may_halt(&self, expr: &Expr) -> bool {
        match expr {
            Expr::U64(_)
            | Expr::Bool(_)
            | Expr::Text(_)
            | Expr::Constant(_)
            | Expr::Local(_)
            | Expr::Field(_) => false,
            Expr::Not(operand) | Expr::Len(operand) => self.may_halt(operand),
            Expr::Binary {
                op,
                operand,
                left,
                right,
            } => {
                let quiet = match op {
                    BinaryOp::Add | BinaryOp::Multiply | BinaryOp::Divide | BinaryOp::Remainder => {
                        self.proven(*op, left, right)
                    }
                    BinaryOp::Subtract | BinaryOp::Concat => false,
                    BinaryOp::Equal | BinaryOp::NotEqual => {
                        matches!(operand, Type::U64 | Type::Bool)
                    }
                    BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual
                    | BinaryOp::And
                    | BinaryOp::Or => true,
                };
                !quiet || self.may_halt(left) || self.may_halt(right)
            }
            _ => true,
        }
    }

    fn checked(&mut self, expr: &Expr) -> Result<(), String> {
        self.expr(expr)?;
        if self.may_halt(expr) {
            self.halt_check();
        }
        Ok(())
    }

    fn learn_index(&mut self, base: &Expr, index: &Expr) {
        if let (Some(capacity), Expr::Local(local)) = (self.capacity_of(base), index) {
            self.learn(*local, capacity.checked_sub(1));
        }
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
                    let sum = self.temp();
                    self.emit(format!("i32.const {variant}"));
                    self.emit(format!("i32.const {}", fields.len()));
                    self.emit("call $sum_new");
                    self.emit("i64.extend_i32_u");
                    self.emit(format!("local.set {sum}"));
                    for (position, field) in fields.iter().enumerate() {
                        self.emit(format!("local.get {sum}"));
                        self.emit("i32.wrap_i64");
                        self.emit(format!("i32.const {position}"));
                        self.expr(field)?;
                        self.emit("call $sum_set");
                    }
                    self.emit(format!("local.get {sum}"));
                } else {
                    self.emit(format!("i32.const {variant}"));
                    for field in fields {
                        self.expr(field)?;
                    }
                    self.emit(format!("call $sum_{}", fields.len()));
                }
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
            Expr::TextLen(operand) => {
                self.expr(operand)?;
                self.emit("i32.wrap_i64");
                self.emit("call $text_utf16_len");
            }
            Expr::Truncate { text, tokens } => {
                self.expr(text)?;
                self.emit(format!(
                    "i64.const {}",
                    self.program().constants[*tokens].value
                ));
                self.emit("call $truncate_middle");
            }
            Expr::With { list, entry } => {
                self.expr(list)?;
                self.expr(entry)?;
                self.emit("call $list_append");
            }
            Expr::TextBuiltin { op, args } => self.text_builtin(*op, args)?,
            Expr::Call { function, args } => {
                for arg in args {
                    self.expr(arg)?;
                }
                self.emit(format!("call $fn{function}"));
            }
            Expr::Index { base, index } => {
                self.expr(base)?;
                self.expr(index)?;
                self.trap_name("index");
                self.emit("call $list_index");
            }
            Expr::Bytes { op, args } => self.bytes(*op, args)?,
            Expr::Copy { value, bytes } => {
                self.expr(value)?;
                self.emit(if *bytes {
                    "call $bytes_copy"
                } else {
                    "call $list_copy"
                });
            }
            Expr::Outcome {
                tag,
                value,
                reason,
                original,
                ..
            } => self.outcome(tag, value, reason, original)?,
            Expr::ListingNotice(listing) => {
                self.emit(format!("i32.const $listing{listing}"));
                self.emit("call $listing_notice_value");
            }
            Expr::Cancelled => self.emit("call $cancelled"),
        }
        Ok(())
    }

    fn outcome(
        &mut self,
        tag: &Expr,
        value: &Expr,
        reason: &Expr,
        original: &Expr,
    ) -> Result<(), String> {
        let chosen = self.temp();
        self.expr(tag)?;
        self.emit(format!("local.set {chosen}"));
        self.emit(format!("local.get {chosen}"));
        self.emit("i64.eqz");
        self.emit("if (result i64)");
        self.emit(format!("i32.const {}", ir::OK));
        self.expr(value)?;
        self.emit("call $sum_1");
        self.emit("else");
        self.emit(format!("local.get {chosen}"));
        self.emit("i64.const 1");
        self.emit("i64.eq");
        self.emit("if (result i64)");
        self.emit(format!("i32.const {}", ir::FAILED));
        self.expr(reason)?;
        self.emit("call $sum_1");
        self.emit("else");
        self.emit(format!("local.get {chosen}"));
        self.emit(format!("i64.const {}", ir::OUTCOME_TAG_UNKNOWN));
        self.emit("i64.eq");
        self.emit("if (result i64)");
        self.emit(format!("i32.const {}", ir::UNKNOWN));
        self.emit("call $sum_0");
        self.emit("else");
        self.expr(original)?;
        self.emit("end");
        self.emit("end");
        self.emit("end");
        Ok(())
    }

    fn text_builtin(&mut self, op: ir::TextOp, args: &[Expr]) -> Result<(), String> {
        assert_eq!(args.len(), op.arity(), "a text builtin has its arity");
        self.emit(format!("i32.const {}", op as usize));
        for arg in args {
            self.expr(arg)?;
        }
        if op.arity() == 1 {
            self.emit("i64.const 0");
        }
        self.emit("call $text_op");
        Ok(())
    }

    fn trap_name(&mut self, text: &str) {
        let name = self.tables.text(text);
        self.emit(format!("i32.const {name}"));
    }

    fn bytes(&mut self, op: ir::BytesOp, args: &[Expr]) -> Result<(), String> {
        match op {
            ir::BytesOp::Empty | ir::BytesOp::EmptyList => {
                self.emit("i64.const 0");
                return Ok(());
            }
            ir::BytesOp::OfText | ir::BytesOp::OfHex => {
                let Expr::Constant(capacity) = &args[1] else {
                    panic!("the checker passes bytes_of its capacity constant");
                };
                let constant = &self.program().constants[*capacity];
                self.expr(&args[0])?;
                self.emit(format!("i64.const {}", constant.value));
                self.trap_name(&constant.name.clone());
                if op == ir::BytesOp::OfHex {
                    self.trap_name(crate::run::pure::HEX_TRAP);
                    self.emit("call $bytes_of_hex");
                } else {
                    self.emit("call $bytes_of_text");
                }
                return Ok(());
            }
            ir::BytesOp::ToHex => {
                self.expr(&args[0])?;
                self.emit("call $bytes_hex");
                return Ok(());
            }
            ir::BytesOp::Get | ir::BytesOp::Slice | ir::BytesOp::ToText => {}
        }
        for arg in args {
            self.expr(arg)?;
        }
        if op == ir::BytesOp::Get {
            self.trap_name("index");
            self.emit("call $bytes_get");
            self.learn_index(&args[0], &args[1]);
            return Ok(());
        }
        let (trap, call) = match op {
            ir::BytesOp::Get => ("index", "call $bytes_get"),
            ir::BytesOp::Slice => ("index", "call $bytes_slice"),
            _ => ("text_of", "call $bytes_to_text"),
        };
        self.trap_name(trap);
        self.emit(call);
        Ok(())
    }

    fn local_capacity(&self, local: usize) -> (&'p Type, &'p ir::Bound) {
        let ty = &self.handler.locals[local].ty;
        match ty {
            Type::List { capacity, .. } | Type::Bytes(capacity) => (ty, capacity),
            other => panic!("the checker sets and pushes only lists and bytes, found {other:?}"),
        }
    }

    fn set_index(&mut self, local: usize, index: &Expr, value: &Expr) -> Result<(), String> {
        let (ty, _) = self.local_capacity(local);
        self.emit(format!("local.get $l{local}"));
        self.expr(index)?;
        self.expr(value)?;
        self.trap_name("index");
        if matches!(ty, Type::Bytes(_)) {
            self.trap_name("byte");
            self.emit("call $bytes_set");
        } else {
            self.emit("call $list_set");
        }
        self.halt_check();
        Ok(())
    }

    fn push_local(&mut self, local: usize, value: &Expr, append: bool) -> Result<(), String> {
        let (ty, capacity) = self.local_capacity(local);
        self.emit(format!("local.get $l{local}"));
        self.expr(value)?;
        self.emit(format!("i64.const {}", capacity.value));
        self.trap_name(&capacity.name);
        match (ty, append) {
            (Type::Bytes(_), true) => self.emit("call $bytes_append"),
            (Type::Bytes(_), false)
                if self
                    .most(value)
                    .is_some_and(|most| most <= u64::from(u8::MAX)) =>
            {
                self.emit("call $bytes_push_byte");
            }
            (Type::Bytes(_), false) => {
                self.trap_name("byte");
                self.emit("call $bytes_push");
            }
            _ => self.emit("call $list_push"),
        }
        self.halt_check();
        self.emit(format!("local.set $l{local}"));
        Ok(())
    }

    fn guarded(
        &mut self,
        index: usize,
        bound: u64,
        condition: &Expr,
        body: &[Stmt],
    ) -> Result<(), String> {
        let label = self.label();
        self.emit("i64.const 0");
        self.emit(format!("local.set $l{index}"));
        self.emit(format!("block $break{label}"));
        self.emit(format!("loop $continue{label}"));
        self.emit(format!("local.get $l{index}"));
        self.emit(format!("i64.const {bound}"));
        self.emit("i64.ge_u");
        self.emit(format!("br_if $break{label}"));
        self.scoped(|function| {
            function.learn(index, bound.checked_sub(1));
            function.checked(condition)?;
            function.emit("i64.eqz");
            function.emit(format!("br_if $break{label}"));
            function.block(body)
        })?;
        self.emit(format!("local.get $l{index}"));
        self.emit("i64.const 1");
        self.emit("i64.add");
        self.emit(format!("local.set $l{index}"));
        self.emit(format!("br $continue{label}"));
        self.emit("end");
        self.emit("end");
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
            BinaryOp::Add | BinaryOp::Multiply | BinaryOp::Divide | BinaryOp::Remainder
                if self.proven(op, left, right) =>
            {
                let raw = match op {
                    BinaryOp::Add => "i64.add",
                    BinaryOp::Multiply => "i64.mul",
                    BinaryOp::Divide => "i64.div_u",
                    _ => "i64.rem_u",
                };
                vec![raw.to_owned()]
            }
            BinaryOp::Add => vec!["call $u64_add".to_owned()],
            BinaryOp::Subtract => vec!["call $u64_subtract".to_owned()],
            BinaryOp::Multiply => vec!["call $u64_multiply".to_owned()],
            BinaryOp::Divide | BinaryOp::Remainder => {
                let name = self
                    .tables
                    .text(&format!("u64 division by zero in '{}'", op.symbol()));
                let call = if op == BinaryOp::Divide {
                    "call $u64_divide"
                } else {
                    "call $u64_remainder"
                };
                vec![format!("i32.const {name}"), call.to_owned()]
            }
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
                self.checked(value)?;
                self.emit(format!("local.set $l{local}"));
                let most = self.most(value);
                self.learn(*local, most);
            }
            Stmt::Set { local, value } => {
                self.checked(value)?;
                self.emit(format!("local.set $l{local}"));
            }
            Stmt::SetIndex {
                local,
                index,
                value,
            } => self.set_index(*local, index, value)?,
            Stmt::PushLocal {
                local,
                value,
                append,
            } => self.push_local(*local, value, *append)?,
            Stmt::While {
                index,
                bound,
                condition,
                body,
            } => self.guarded(*index, bound.value, condition, body)?,
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
                self.checked(condition)?;
                self.emit("i32.wrap_i64");
                self.emit("if");
                self.scoped(|function| function.block(then))?;
                self.emit("else");
                self.scoped(|function| function.block(otherwise))?;
                self.emit("end");
            }
            Stmt::Match { subject, arms, .. } => self.matched(subject, arms)?,
            Stmt::Loop { index, bound, body } => self.counted(*index, bound.value, body)?,
            Stmt::For {
                item, list, body, ..
            } => self.each(*item, list, body)?,
            Stmt::Effect { site, args, result } => self.effect(*site, args, *result)?,
            Stmt::Finish { value } => {
                self.checked(value)?;
                self.emit("local.set $result");
                self.emit("br $exit");
            }
            Stmt::Shrink { field, kept } => self.shrink(*field, kept)?,
            Stmt::KeepBlobs { field, count } => {
                self.emit("global.get $state");
                self.emit("global.get $state");
                self.emit(format!("i64.load offset={}", field * SLOT_BYTES));
                self.emit(format!("i64.const {}", count.value));
                self.emit("call $keep_blobs");
                self.emit(format!("i64.store offset={}", field * SLOT_BYTES));
            }
            Stmt::Rewrite { field, arms } => self.rewrite(*field, arms)?,
            Stmt::Parallel(parallel) => self.parallel(parallel)?,
            Stmt::Take { result, taken } => {
                self.emit(match taken {
                    ir::Taken::Steers => "call $take_steers",
                    ir::Taken::Jobs => "call $take_jobs",
                });
                self.emit(format!("local.set $l{result}"));
            }
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

    fn shrink(&mut self, field: usize, kept: &ir::Kept) -> Result<(), String> {
        let definition = &self.program().state[field];
        let Type::List { capacity, .. } = &definition.ty else {
            panic!("the checker clears and keeps only lists");
        };
        let capacity = capacity.value;
        let name = self.tables.text(&definition.name);
        self.emit("global.get $state");
        self.emit("global.get $state");
        self.emit(format!("i64.load offset={}", field * SLOT_BYTES));
        match kept {
            ir::Kept::Nothing => {
                self.emit("i64.const 0");
                self.emit("i32.const 0");
                self.emit("i64.const 0");
            }
            ir::Kept::Last(count) => {
                self.expr(count)?;
                self.emit("i32.const 0");
                self.emit("i64.const 0");
            }
            ir::Kept::LastAfter { count, first } => {
                self.expr(count)?;
                self.emit("i32.const 1");
                self.expr(first)?;
            }
        }
        self.emit(format!("i64.const {capacity}"));
        self.emit(format!("i32.const {name}"));
        self.emit("call $list_keep");
        self.halt_check();
        self.emit(format!("i64.store offset={}", field * SLOT_BYTES));
        Ok(())
    }

    fn parallel(&mut self, parallel: &ir::Parallel) -> Result<(), String> {
        let label = self.label();
        let items = self.temp();
        let values = self.temp();
        let at = self.temp();
        self.expr(&parallel.list)?;
        self.halt_check();
        self.emit(format!("local.set {items}"));
        let answers = self.temp();
        self.emit("i64.const 0");
        self.emit(format!("local.set {answers}"));
        if let Some(answer) = &parallel.answer {
            self.answers(parallel, answer, &items, &answers)?;
        }
        self.emit(format!("i32.const $site{}_{}", self.index, parallel.site));
        self.emit(format!("local.get {items}"));
        match &parallel.answer {
            Some(answer) => self.emit(format!("i32.const $site{}_{}", self.index, answer.site)),
            None => self.emit("i32.const 0"),
        }
        self.emit(format!("local.get {answers}"));
        self.emit("i32.wrap_i64");
        self.emit("call $parallel");
        self.emit("i64.extend_i32_u");
        self.emit(format!("local.set {values}"));
        self.halt_check();
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
        self.emit(format!("local.set $l{}", parallel.item));
        self.emit(format!("local.get {values}"));
        self.emit(format!("local.get {at}"));
        self.emit(format!("i64.const {SLOT_BYTES}"));
        self.emit("i64.mul");
        self.emit("i64.add");
        self.emit("i32.wrap_i64");
        self.emit("i64.load");
        self.emit(format!("local.set $l{}", parallel.result));
        self.block(&parallel.body)?;
        self.emit(format!("local.get {at}"));
        self.emit("i64.const 1");
        self.emit("i64.add");
        self.emit(format!("local.set {at}"));
        self.emit(format!("br $continue{label}"));
        self.emit("end");
        self.emit("end");
        Ok(())
    }

    fn answers(
        &mut self,
        parallel: &ir::Parallel,
        answer: &ir::Answer,
        items: &str,
        answers: &str,
    ) -> Result<(), String> {
        let label = self.label();
        let at = self.temp();
        self.emit(format!("local.get {items}"));
        self.emit("call $list_len");
        self.emit(format!("i32.const {ANSWER_BYTES}"));
        self.emit("i32.mul");
        self.emit("call $alloc");
        self.emit("i64.extend_i32_u");
        self.emit(format!("local.set {answers}"));
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
        self.emit(format!("local.set $l{}", parallel.item));
        self.block(&answer.prelude)?;
        self.expr(&answer.decided)?;
        self.emit("i64.const 0");
        self.emit("i64.ne");
        self.emit("if");
        self.emit(format!(
            "i32.const {}",
            answer.args.len().max(1) * SLOT_BYTES
        ));
        self.emit("call $alloc");
        self.emit("local.set $args");
        for (position, arg) in answer.args.iter().enumerate() {
            self.emit("local.get $args");
            self.expr(arg)?;
            self.emit(format!("i64.store offset={}", position * SLOT_BYTES));
        }
        self.emit(format!("local.get {answers}"));
        self.emit(format!("local.get {at}"));
        self.emit(format!("i64.const {ANSWER_BYTES}"));
        self.emit("i64.mul");
        self.emit("i64.add");
        self.emit("i32.wrap_i64");
        self.emit("local.get $args");
        self.emit("i32.store");
        self.emit("end");
        self.halt_check();
        self.emit(format!("local.get {at}"));
        self.emit("i64.const 1");
        self.emit("i64.add");
        self.emit(format!("local.set {at}"));
        self.emit(format!("br $continue{label}"));
        self.emit("end");
        self.emit("end");
        Ok(())
    }

    fn rewrite(&mut self, field: usize, arms: &[ir::RewriteArm]) -> Result<(), String> {
        let definition = &self.program().state[field];
        let Type::List { capacity, .. } = &definition.ty else {
            panic!("the checker rewrites only lists");
        };
        let capacity = capacity.value;
        let name = self.tables.text(&definition.name);
        let label = self.label();
        let items = self.temp();
        let rewritten = self.temp();
        let at = self.temp();
        let item = self.temp();
        self.emit("global.get $state");
        self.emit(format!("i64.load offset={}", field * SLOT_BYTES));
        self.emit(format!("local.set {items}"));
        self.emit("global.get $state");
        self.emit("i64.const 0");
        self.emit(format!("i64.store offset={}", field * SLOT_BYTES));
        self.emit("i64.const 0");
        self.emit(format!("local.set {rewritten}"));
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
        self.emit(format!("local.set {item}"));
        for (variant, arm) in arms.iter().enumerate() {
            assert_eq!(arm.variant, variant, "rewrite arms are in variant order");
            if matches!(arm.entry, ir::Rewritten::Dropped) {
                continue;
            }
            self.emit(format!("local.get {item}"));
            self.emit("call $sum_variant");
            self.emit(format!("i32.const {variant}"));
            self.emit("i32.eq");
            self.emit("if");
            self.emit(format!("local.get {rewritten}"));
            match &arm.entry {
                ir::Rewritten::Kept => self.emit(format!("local.get {item}")),
                ir::Rewritten::Into(entry) => {
                    for (position, binding) in arm.bindings.iter().enumerate() {
                        if let Some(local) = binding {
                            self.emit(format!("local.get {item}"));
                            self.emit(format!("i32.const {position}"));
                            self.emit("call $sum_field");
                            self.emit(format!("local.set $l{local}"));
                        }
                    }
                    self.expr(entry)?;
                }
                ir::Rewritten::Dropped => unreachable!("dropped arms push nothing"),
            }
            self.emit(format!("i64.const {capacity}"));
            self.emit(format!("i32.const {name}"));
            if matches!(arm.entry, ir::Rewritten::Kept) {
                self.emit("call $rewrite_keep");
            } else {
                self.emit("call $list_push");
            }
            self.emit(format!("local.set {rewritten}"));
            self.halt_check();
            self.emit("end");
        }
        self.emit(format!("local.get {at}"));
        self.emit("i64.const 1");
        self.emit("i64.add");
        self.emit(format!("local.set {at}"));
        self.emit(format!("br $continue{label}"));
        self.emit("end");
        self.emit("end");
        self.emit("global.get $state");
        self.emit(format!("local.get {rewritten}"));
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
            self.scoped(|function| function.block(&arm.body))?;
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
        self.scoped(|function| {
            function.learn(index, bound.checked_sub(1));
            function.block(body)
        })?;
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
        self.scoped(|function| function.block(body))?;
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
        self.sleep_cap(site)?;
        self.emit(format!("i32.const $site{}_{site}", self.index));
        self.emit("local.get $args");
        self.emit("call $effect");
        self.halt_check();
        self.emit(format!("local.set $l{result}"));
        Ok(())
    }
}

impl Function<'_, '_> {
    fn sleep_cap(&mut self, site: usize) -> Result<(), String> {
        let Some(cap) = self.handler.sites[site].kind.sleeps() else {
            return Ok(());
        };
        let constant = &self.program().constants[cap];
        let trap = self.tables.text(&format!("bound({})", constant.name));
        self.emit("local.get $args");
        self.emit("i64.load");
        self.emit(format!("i64.const {}", constant.value));
        self.emit("i64.gt_u");
        self.emit("if");
        self.emit(format!("i32.const {trap}"));
        self.emit("call $trap");
        self.emit("br $exit");
        self.emit("end");
        Ok(())
    }
}

fn handler_function<'p>(
    tables: &mut Tables<'p>,
    handler: &'p Handler,
    index: usize,
    name: &str,
) -> Result<String, String> {
    for (site_index, site) in handler.sites.iter().enumerate() {
        tables.site(&format!("$site{index}_{site_index}"), site)?;
    }
    let mut function = Function {
        tables,
        handler,
        index,
        body: Vec::new(),
        temps: 0,
        labels: 0,
        facts: Vec::new(),
        fixed: vec![false; handler.locals.len()],
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
        "(func {name} (param $arguments i32) (result i64) (local $result i64) (local $args i32)"
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

fn pure_function<'p>(
    tables: &mut Tables<'p>,
    definition: &'p Handler,
    id: usize,
) -> Result<String, String> {
    let mut function = Function {
        tables,
        handler: definition,
        index: 0,
        body: Vec::new(),
        temps: 0,
        labels: 0,
        facts: Vec::new(),
        fixed: fixed_locals(definition),
    };
    function.emit("block $exit");
    function.block(&definition.body)?;
    function.emit("unreachable");
    function.emit("end");
    let mut text = format!("(func $fn{id}");
    for param in &definition.params {
        write!(text, " (param $l{param} i64)").expect("writing to a string succeeds");
    }
    text.push_str(" (result i64) (local $result i64)");
    for local in 0..definition.locals.len() {
        if !definition.params.contains(&local) {
            write!(text, " (local $l{local} i64)").expect("writing to a string succeeds");
        }
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

fn assigned(statements: &[Stmt], fixed: &mut [bool]) {
    for statement in statements {
        match statement {
            Stmt::Set { local, .. } => fixed[*local] = false,
            Stmt::Effect { result, .. } | Stmt::Take { result, .. } => fixed[*result] = false,
            Stmt::If {
                then, otherwise, ..
            } => {
                assigned(then, fixed);
                assigned(otherwise, fixed);
            }
            Stmt::Match { arms, .. } => {
                for arm in arms {
                    assigned(&arm.body, fixed);
                }
            }
            Stmt::Loop { body, .. } | Stmt::For { body, .. } | Stmt::While { body, .. } => {
                assigned(body, fixed)
            }
            Stmt::Let { .. }
            | Stmt::SetIndex { .. }
            | Stmt::PushLocal { .. }
            | Stmt::Finish { .. } => {}
            _ => fixed.fill(false),
        }
    }
}

fn fixed_locals(definition: &Handler) -> Vec<bool> {
    let mut fixed = vec![true; definition.locals.len()];
    assigned(&definition.body, &mut fixed);
    fixed
}

fn refuse_unsupported(program: &Program) -> Result<(), String> {
    let refused = [(
        program.codes.iter().any(|code| code.interrupts.steer),
        "interruptible effects",
    )];
    let what: Vec<&str> = refused
        .iter()
        .filter(|(refuses, _)| *refuses)
        .map(|(_, what)| *what)
        .collect();
    if what.is_empty() {
        return Ok(());
    }
    not_yet(&what.join(", "))
}

pub fn program_text(program: &Program, target: crate::target::Target) -> Result<String, String> {
    refuse_unsupported(program)?;
    let mut tables = Tables::new(program);
    tables.ty(&context_type())?;
    let mut functions = String::new();
    for (id, definition) in program.functions.iter().enumerate() {
        functions.push_str(&pure_function(&mut tables, definition, id)?);
    }
    let functions_count = program.handlers.len() + program.sides.len();
    for index in 0..functions_count {
        let handler = program
            .handlers
            .get(index)
            .unwrap_or_else(|| &program.sides[index - program.handlers.len()]);
        functions.push_str(&handler_function(
            &mut tables,
            handler,
            index,
            &format!("$handler{index}_run"),
        )?);
    }
    match &program.forked {
        Some(hook) => {
            assert!(hook.sites.is_empty(), "a state hook runs no effect");
            functions.push_str(&handler_function(
                &mut tables,
                hook,
                functions_count,
                "$forked_run",
            )?);
            functions.push_str("(words $forked_hook 1)\n");
        }
        None => functions.push_str(
            "(words $forked_hook 0)\n(func $forked_run (param $arguments i32) (result i64) unreachable)\n",
        ),
    }
    tables.program(target)?;
    let mut run =
        String::from("(func $run_handler (param $index i32) (param $arguments i32) (result i64)\n");
    for index in 0..functions_count {
        writeln!(run, "  (if (i32.eq (local.get $index) (i32.const {index})) (then (return (call $handler{index}_run (local.get $arguments)))))").expect("writing to a string succeeds");
    }
    run.push_str("  unreachable)\n");
    Ok(format!("{}{functions}{run}", tables.out))
}
