use crate::ir::{Handler, Program, Stmt};
use crate::prove::text::Names;
use crate::prove::{Finding, Report};

pub fn prove(program: &Program, report: &mut Report) {
    for handler in program.handlers.iter().chain(&program.sides) {
        let names = Names::new(program, Some(handler));
        let mut walk = Walk {
            program,
            handler,
            names: &names,
            path: Vec::new(),
            findings: Vec::new(),
            finishes: 0,
        };
        if walk.block(&handler.body) {
            walk.path.push("the end of the handler".to_owned());
            walk.finding("a path reaches the end of the handler without a return");
        }
        let clean = walk.findings.is_empty();
        let finishes = walk.finishes;
        for finding in walk.findings {
            report.add(finding);
        }
        if clean {
            report.proved.push(format!(
                "{} {}: finish_once: every path ends in exactly one of {finishes} return(s)",
                program.kind_of(handler),
                handler.name
            ));
        }
    }
}

struct Walk<'a> {
    program: &'a Program,
    handler: &'a Handler,
    names: &'a Names<'a>,
    path: Vec<String>,
    findings: Vec<Finding>,
    finishes: usize,
}

impl Walk<'_> {
    fn finding(&mut self, message: &str) {
        let path = if self.path.is_empty() {
            "entry".to_owned()
        } else {
            self.path.join(" > ")
        };
        self.findings.push(Finding {
            handler: self.handler.name.clone(),
            target: None,
            property: "finish_once",
            message: message.to_owned(),
            path,
            counterexample: None,
        });
    }

    fn block(&mut self, statements: &[Stmt]) -> bool {
        for (index, statement) in statements.iter().enumerate() {
            if !self.statement(statement) {
                if index + 1 < statements.len() {
                    self.finding("a statement follows a return, so a path would finish twice");
                }
                return false;
            }
        }
        true
    }

    fn branch(&mut self, label: String, statements: &[Stmt]) -> bool {
        self.path.push(label);
        let falls = self.block(statements);
        self.path.pop();
        falls
    }

    fn statement(&mut self, statement: &Stmt) -> bool {
        match statement {
            Stmt::Set { .. }
            | Stmt::SetIndex { .. }
            | Stmt::PushLocal { .. }
            | Stmt::While { .. } => {
                panic!(
                    "vars and while loops live only in pure fns, which the prover reads as calls"
                )
            }
            Stmt::Finish { .. } => {
                self.finishes += 1;
                false
            }
            Stmt::If {
                condition,
                then,
                otherwise,
            } => {
                let text = self.names.expr(condition);
                let then_falls = self.branch(format!("if {text}"), then);
                let otherwise_falls = self.branch(format!("else of if {text}"), otherwise);
                then_falls || otherwise_falls
            }
            Stmt::Match { subject, ty, arms } => {
                let text = self.names.expr(subject);
                let variants = ty.variants(self.program).expect("a match subject is a sum");
                let mut falls = false;
                for arm in arms {
                    let label = format!("match {text}: {}", variants[arm.variant].name);
                    falls |= self.branch(label, &arm.body);
                }
                falls
            }
            Stmt::Loop { index, bound, body } => {
                let label = format!(
                    "loop {} below {}",
                    self.handler.locals[*index].name, bound.name
                );
                self.branch(label, body);
                true
            }
            Stmt::For {
                item, list, body, ..
            } => {
                let label = format!(
                    "for {} in {}",
                    self.handler.locals[*item].name,
                    self.names.expr(list)
                );
                self.branch(label, body);
                true
            }
            Stmt::Parallel(parallel) => self.statement(&parallel.sequential()),
            Stmt::Let { .. }
            | Stmt::Assign { .. }
            | Stmt::Push { .. }
            | Stmt::Shrink { .. }
            | Stmt::KeepBlobs { .. }
            | Stmt::Rewrite { .. }
            | Stmt::Effect { .. }
            | Stmt::Take { .. } => true,
        }
    }
}
