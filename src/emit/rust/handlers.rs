use std::fmt::Write;

use crate::effect::EffectClass;
use crate::ir::{self, BinaryOp, Durability, Expr, Handler, Program, SiteKind, Stmt, Type};

use super::types::{Types, constant, ident, variant_ident};

pub struct Body<'a, 'p> {
    pub program: &'p Program,
    pub handler: &'p Handler,
    pub types: &'a mut Types<'p>,
    pub out: String,
    depth: usize,
}

impl<'a, 'p> Body<'a, 'p> {
    pub fn new(program: &'p Program, handler: &'p Handler, types: &'a mut Types<'p>) -> Self {
        Self {
            program,
            handler,
            types,
            out: String::new(),
            depth: 1,
        }
    }

    fn line(&mut self, text: &str) {
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    pub fn local(&self, local: ir::LocalId) -> String {
        format!("{}_{local}", ident(&self.handler.locals[local].name))
    }

    pub fn field(&self, field: ir::FieldId) -> String {
        ident(&self.program.state[field].name)
    }

    pub fn emit(&mut self) {
        if let Some(requires) = &self.handler.requires {
            let condition = self.expr(requires);
            self.line(&format!("if !{condition} {{"));
            self.line(&format!(
                "    return Err(Halt::Trap({:?}.to_owned()));",
                format!("requires({})", self.handler.name)
            ));
            self.line("}");
        }
        let body = &self.handler.body;
        self.block(body);
        self.line("panic!(\"the checker proves every handler returns\")");
    }

    fn block(&mut self, statements: &[Stmt]) {
        for statement in statements {
            self.statement(statement);
        }
    }

    fn nested(&mut self, open: &str, statements: &[Stmt], close: &str) {
        self.line(open);
        self.depth += 1;
        self.block(statements);
        self.depth -= 1;
        self.line(close);
    }

    fn statement(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Let { local, value } => {
                let value = self.expr(value);
                let local = self.local(*local);
                self.line(&format!("let {local} = {value};"));
            }
            Stmt::Assign { field, value } => {
                let value = self.expr(value);
                let field = self.field(*field);
                self.line(&format!("state.{field} = {value};"));
            }
            Stmt::Push { field, value } => self.push(*field, value),
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                let condition = self.expr(condition);
                self.nested(&format!("if {condition} {{"), then, "} else {");
                self.depth += 1;
                self.block(otherwise);
                self.depth -= 1;
                self.line("}");
            }
            Stmt::Match { subject, ty, arms } => self.matching(subject, ty, arms),
            Stmt::Loop { index, bound, body } => {
                let index = self.local(*index);
                let open = format!("for {index} in 0..{} {{", constant(&bound.name));
                self.nested(&open, body, "}");
            }
            Stmt::For {
                item,
                list,
                bound,
                body,
            } => {
                let list = self.expr(list);
                let item = self.local(*item);
                let items = format!("items_{}", self.depth);
                self.line("{");
                self.depth += 1;
                self.line(&format!("let {items} = {list};"));
                self.line(&format!(
                    "assert!({items}.len() as u64 <= {}, \"a list holds at most its capacity\");",
                    constant(&bound.name)
                ));
                self.nested(&format!("for {item} in {items} {{"), body, "}");
                self.depth -= 1;
                self.line("}");
            }
            Stmt::Parallel(parallel) => self.parallel(parallel),
            Stmt::Effect { site, args, result } => self.effect(*site, args, *result),
            Stmt::Take { result } => {
                let local = self.local(*result);
                self.line(&format!("let {local} = engine.take_steers();"));
            }
            Stmt::Finish { value } => {
                let value = self.expr(value);
                self.line(&format!("return Ok({value});"));
            }
        }
    }

    fn parallel(&mut self, parallel: &ir::Parallel) {
        let list = self.expr(&parallel.list);
        let item = self.local(parallel.item);
        let result = self.local(parallel.result);
        let items = format!("items_{}", self.depth);
        let answers = format!("answers_{}", self.depth);
        let call = super::types::sum_name(self.program, ir::CALL);
        let site = &self.handler.sites[parallel.site];
        assert_eq!(site.kind, SiteKind::Dispatch, "a parallel for dispatches");
        let Type::Maybe(inner) = &site.result else {
            panic!("a dispatch yields maybe, found {:?}", site.result);
        };
        let returns = self.types.returns(inner);
        let decode = self.types.from_json(inner);
        self.line("{");
        self.depth += 1;
        self.line(&format!("let {items} = {list};"));
        self.line(&format!(
            "assert!({items}.len() as u64 <= {}, \"a list holds at most its capacity\");",
            constant(&parallel.bound.name)
        ));
        self.line(&format!(
            "let calls: Vec<Call> = {items}.iter().map(|item| {{"
        ));
        self.line(&format!(
            "    let {call}::Call(_, tool, input) = item.clone();"
        ));
        self.line("    let effect = dispatch_class(&tool);");
        self.line("    Call { tool, effect, input: Json::String(input) }");
        self.line("}).collect();");
        self.line(&format!(
            "let {answers} = engine.parallel(calls, {returns}, CONCURRENT_REQUESTS_MAX)?;"
        ));
        self.line(&format!(
            "for ({item}, answer) in {items}.into_iter().zip({answers}) {{"
        ));
        self.depth += 1;
        self.line(&format!("let {result} = maybe(engine, answer, {decode})?;"));
        self.block(&parallel.body);
        self.depth -= 1;
        self.line("}");
        self.depth -= 1;
        self.line("}");
    }

    fn push(&mut self, field: ir::FieldId, value: &Expr) {
        let Type::List { capacity, .. } = &self.program.state[field].ty else {
            panic!("the checker pushes only onto lists");
        };
        let capacity = constant(&capacity.name);
        let value = self.expr(value);
        let name = &self.program.state[field].name;
        let field = self.field(field);
        self.line("{");
        self.line(&format!("    let value = {value};"));
        self.line(&format!(
            "    push(&mut state.{field}, value, {capacity}, {name:?})?;"
        ));
        self.line("}");
    }

    fn matching(&mut self, subject: &Expr, ty: &Type, arms: &[ir::Arm]) {
        let subject = self.expr(subject);
        let variants = ty.variants(self.program).expect("a match is over a sum");
        let prefix = match ty {
            Type::Sum(sum) => super::types::sum_name(self.program, *sum),
            Type::Known(_) => "Known".to_owned(),
            Type::Maybe(_) => "Maybe".to_owned(),
            other => panic!("the checker matches only sums, found {other:?}"),
        };
        self.line(&format!("match {subject} {{"));
        self.depth += 1;
        for (arm, variant) in arms.iter().zip(&variants) {
            let bindings: Vec<String> = arm
                .bindings
                .iter()
                .map(|binding| binding.map_or_else(|| "_".to_owned(), |local| self.local(local)))
                .collect();
            let path = format!("{prefix}::{}", variant_ident(&variant.name));
            let pattern = if variant.fields.is_empty() {
                path
            } else {
                format!("{path}({})", bindings.join(", "))
            };
            self.nested(&format!("{pattern} => {{"), &arm.body, "}");
        }
        self.depth -= 1;
        self.line("}");
    }

    fn effect(&mut self, site_id: ir::SiteId, args: &[Expr], result: ir::LocalId) {
        let site = &self.handler.sites[site_id];
        let result = self.local(result);
        self.line(&format!("let {result} = {{"));
        self.depth += 1;
        for (index, arg) in args.iter().enumerate() {
            let value = self.expr(arg);
            self.line(&format!("let argument{index} = {value};"));
        }
        let durability = match site.durability {
            Durability::RidesNextFlush => "Durability::RidesNextFlush",
            Durability::FlushBeforeRun => "Durability::FlushBeforeRun",
            Durability::FlushBeforeRunWhenExternal => "Durability::FlushBeforeRunWhenExternal",
        };
        let maybe = matches!(site.result, Type::Maybe(_));
        let (kind, run, decode) = self.site_call(site.kind, &site.result, args);
        self.line(&format!(
            "let site = Site {{ kind: Kind::{kind}, durability: {durability}, maybe: {maybe} }};"
        ));
        self.line(&format!("let answer = engine.effect(site, call, {run})?;"));
        self.line(&decode);
        self.depth -= 1;
        self.line("};");
    }

    fn site_call(
        &mut self,
        kind: SiteKind,
        result: &Type,
        args: &[Expr],
    ) -> (&'static str, String, String) {
        let decode = |types: &mut Types<'p>| match result {
            Type::Known(inner) => format!("known(engine, answer, {})?", types.from_json(inner)),
            Type::Maybe(inner) => format!("maybe(engine, answer, {})?", types.from_json(inner)),
            other => panic!("a tool site yields known or maybe, found {other:?}"),
        };
        let returns = |types: &Types<'p>, result: &Type| match result {
            Type::Known(inner) | Type::Maybe(inner) => types.returns(inner),
            other => panic!("a tool site yields known or maybe, found {other:?}"),
        };
        match kind {
            SiteKind::Tool(tool_id) => {
                let tool = &self.program.tools[tool_id];
                let mut entries = Vec::new();
                for (index, param) in tool.params.iter().enumerate() {
                    let json = self.types.to_json(&param.ty);
                    entries.push(format!("({:?}, {json}(&argument{index}))", param.name));
                }
                self.line(&format!(
                    "let call = Call {{ tool: {:?}.to_owned(), effect: EffectClass::{:?}, input: Json::object(vec![{}]) }};",
                    tool.name,
                    tool.effect,
                    entries.join(", ")
                ));
                let run = format!("Run::Tool({})", returns(self.types, result));
                ("Tool", run, decode(self.types))
            }
            SiteKind::Dispatch => {
                let call = super::types::sum_name(self.program, ir::CALL);
                self.line(&format!("let {call}::Call(_, tool, input) = argument0;"));
                self.line("let effect = dispatch_class(&tool);");
                self.line("let call = Call { tool, effect, input: Json::String(input) };");
                let run = format!("Run::Tool({})", returns(self.types, result));
                ("Dispatch", run, decode(self.types))
            }
            SiteKind::Model => {
                let model = self
                    .program
                    .model
                    .as_ref()
                    .expect("the checker admits model calls only with a model");
                let context_type = self.type_of(&args[0]);
                assert!(
                    matches!(&context_type, Type::List { element, .. } if **element == Type::Sum(ir::ENTRY)),
                    "the checker passes the model a list of entries"
                );
                let context = self.types.to_json(&context_type);
                self.line(&format!(
                    "let call = Call {{ tool: MODEL_NAME.to_owned(), effect: EffectClass::{:?}, input: Json::Null }};",
                    EffectClass::Idempotent
                ));
                self.line(&format!("let context = || {context}(&argument0);"));
                let run = format!(
                    "Run::Model {{ calls_max: {}, context: &context }}",
                    constant(&model.calls.name)
                );
                let decode = match result {
                    Type::Known(inner) => {
                        format!("known_reply(answer, {})", self.types.from_json(inner))
                    }
                    other => format!("reply(answer, {})?", self.types.from_json(other)),
                };
                ("Model", run, decode)
            }
            SiteKind::Now | SiteKind::Random => {
                let (name, variant) = if kind == SiteKind::Now {
                    ("now", "Now")
                } else {
                    ("random", "Random")
                };
                self.line(&format!(
                    "let call = Call {{ tool: {name:?}.to_owned(), effect: EffectClass::{:?}, input: Json::Null }};",
                    EffectClass::None
                ));
                (
                    variant,
                    format!("Run::{variant}"),
                    "scalar(engine, answer)?".to_owned(),
                )
            }
        }
    }

    fn type_of(&self, expr: &Expr) -> Type {
        match expr {
            Expr::U64(_) | Expr::Constant(_) | Expr::Len(_) => Type::U64,
            Expr::Bool(_) | Expr::Not(_) | Expr::Cancelled => Type::Bool,
            Expr::Text(_) | Expr::Member { .. } => Type::Text,
            Expr::Local(local) => self.handler.locals[*local].ty.clone(),
            Expr::Field(field) => self.program.state[*field].ty.clone(),
            Expr::Construct { ty, .. } => ty.clone(),
            Expr::Binary { op, .. } => match op {
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply => Type::U64,
                BinaryOp::Concat => Type::Text,
                _ => Type::Bool,
            },
        }
    }

    pub fn expr(&mut self, expr: &Expr) -> String {
        match expr {
            Expr::U64(value) => format!("{value}_u64"),
            Expr::Bool(value) => value.to_string(),
            Expr::Text(value) => format!("String::from({value:?})"),
            Expr::Constant(id) => constant(&self.program.constants[*id].name),
            Expr::Local(local) => {
                let name = self.local(*local);
                if Types::copy(&self.handler.locals[*local].ty) {
                    name
                } else {
                    format!("{name}.clone()")
                }
            }
            Expr::Field(field) => {
                let name = self.field(*field);
                if Types::copy(&self.program.state[*field].ty) {
                    format!("state.{name}")
                } else {
                    format!("state.{name}.clone()")
                }
            }
            Expr::Member { base, index } => {
                let base = self.expr(base);
                let call = super::types::sum_name(self.program, ir::CALL);
                let fields: Vec<&str> = (0..ir::CALL_FIELDS.len())
                    .map(|field| if field == *index { "field" } else { "_" })
                    .collect();
                format!(
                    "{{ let {call}::Call({}) = {base}; field }}",
                    fields.join(", ")
                )
            }
            Expr::Construct {
                ty,
                variant,
                fields,
            } => self.construct(ty, *variant, fields),
            Expr::Binary {
                op, left, right, ..
            } => self.binary(*op, left, right),
            Expr::Not(operand) => format!("(!{})", self.expr(operand)),
            Expr::Len(operand) => format!("({}.len() as u64)", self.expr(operand)),
            Expr::Cancelled => "engine.cancelled()".to_owned(),
        }
    }

    fn construct(&mut self, ty: &Type, variant: ir::VariantId, fields: &[Expr]) -> String {
        let variants = ty
            .variants(self.program)
            .expect("a constructor builds a sum");
        let prefix = match ty {
            Type::Sum(sum) => super::types::sum_name(self.program, *sum),
            Type::Known(_) => "Known".to_owned(),
            Type::Maybe(_) => "Maybe".to_owned(),
            other => panic!("only sums have constructors, found {other:?}"),
        };
        let path = format!("{prefix}::{}", variant_ident(&variants[variant].name));
        if fields.is_empty() {
            return path;
        }
        let values: Vec<String> = fields.iter().map(|field| self.expr(field)).collect();
        format!("{path}({})", values.join(", "))
    }

    fn binary(&mut self, op: BinaryOp, left: &Expr, right: &Expr) -> String {
        let operand = self.type_of(left);
        let left = self.expr(left);
        let right = self.expr(right);
        let mut out = String::new();
        let written = match op {
            BinaryOp::Add => write!(out, "add({left}, {right})?"),
            BinaryOp::Subtract => write!(out, "subtract({left}, {right})?"),
            BinaryOp::Multiply => write!(out, "multiply({left}, {right})?"),
            BinaryOp::Concat => write!(out, "concat({left}, {right})"),
            BinaryOp::Equal => write!(out, "({left} == {right})"),
            BinaryOp::NotEqual => write!(out, "({left} != {right})"),
            BinaryOp::Less => write!(out, "({left} < {right})"),
            BinaryOp::LessEqual => write!(out, "({left} <= {right})"),
            BinaryOp::Greater => write!(out, "({left} > {right})"),
            BinaryOp::GreaterEqual => write!(out, "({left} >= {right})"),
            BinaryOp::And => write!(out, "({left} & {right})"),
            BinaryOp::Or => write!(out, "({left} | {right})"),
        };
        written.expect("writing to a string succeeds");
        if matches!(
            op,
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual
        ) {
            assert_eq!(operand, Type::U64, "the checker orders only u64 values");
        }
        out
    }
}
