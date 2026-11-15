use super::Checker;
use crate::effect::EffectClass;
use crate::ir::{self, BinaryOp, Expr, LocalId, SiteClass, SiteKind, Stmt, Type};

const TAG_OK: u64 = 0;
const TAG_FAILED: u64 = 1;
const TAG_ORIGINAL: u64 = 3;
const CAP_REACHED: &str = "payment required, but the wallet has reached its spend cap";
const ATTACHED_REFUSED: &str = "a paid call returned an attachment, which a payment does not carry";

#[derive(Clone, Copy)]
pub(super) enum Paid {
    Tool { payment: usize },
    Dispatch,
}

#[derive(Clone, Copy)]
struct Fields {
    spent: ir::FieldId,
    tag: ir::FieldId,
    more: ir::FieldId,
    reference: ir::FieldId,
    text: ir::FieldId,
}

#[derive(Clone)]
struct Paying<'p> {
    kind: SiteKind,
    class: SiteClass,
    result: &'p Type,
    args: &'p [Expr],
    paid: Paid,
    fields: Fields,
}

fn binary(op: BinaryOp, operand: Type, left: Expr, right: Expr) -> Expr {
    Expr::Binary {
        op,
        operand,
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn and(left: Expr, right: Expr) -> Expr {
    binary(BinaryOp::And, Type::Bool, left, right)
}

fn or(left: Expr, right: Expr) -> Expr {
    binary(BinaryOp::Or, Type::Bool, left, right)
}

fn assign(field: ir::FieldId, value: Expr) -> Stmt {
    Stmt::Assign { field, value }
}

fn text_op(op: ir::TextOp, args: Vec<Expr>) -> Expr {
    Expr::TextBuiltin { op, args }
}

fn member(base: &Expr, index: usize) -> Expr {
    Expr::Member {
        base: Box::new(base.clone()),
        index,
    }
}

fn inner_of(result: &Type) -> Type {
    match result {
        Type::Maybe(inner) => (**inner).clone(),
        _ => Type::Text,
    }
}

pub fn value_field_name(program: &ir::Program, ty: &Type) -> Option<String> {
    match ty {
        Type::Text => None,
        other => Some(format!("{}{}", ir::PAID_VALUE_FIELD, other.name(program))),
    }
}

impl Checker<'_> {
    pub(super) fn dispatch_pays(&self) -> bool {
        self.program.wallet.is_some()
            && (self.program.tools.iter().any(|tool| tool.paid)
                || self.program.listings.iter().any(|listing| listing.paid))
    }

    fn paid_fields(&self) -> Fields {
        let field = |name: &str| self.globals.fields[name];
        Fields {
            spent: field(ir::SPENT_FIELD),
            tag: field(ir::PAID_TAG_FIELD),
            more: field(ir::PAID_MORE_FIELD),
            reference: field(ir::PAID_REF_FIELD),
            text: field(ir::PAID_TEXT_FIELD),
        }
    }

    fn value_field(&self, inner: &Type) -> ir::FieldId {
        match value_field_name(self.program, inner) {
            None => self.paid_fields().text,
            Some(name) => self.globals.fields[&name],
        }
    }

    fn payable(&self, reason: Expr, paying: &Paying<'_>) -> Expr {
        let prefix = || Expr::Text(ir::PAYMENT_REQUIRED_PREFIX.to_owned());
        let marked = and(
            text_op(ir::TextOp::Contains, vec![reason.clone(), prefix()]),
            binary(
                BinaryOp::Equal,
                Type::Text,
                text_op(ir::TextOp::Before, vec![reason, prefix()]),
                Expr::Text(String::new()),
            ),
        );
        match paying.paid {
            Paid::Tool { .. } => marked,
            Paid::Dispatch => and(
                marked,
                self.paid_tool(&member(&paying.args[0], ir::CALL_TOOL)),
            ),
        }
    }

    fn paid_tool(&self, tool: &Expr) -> Expr {
        let separator = || Expr::Text(ir::LISTED_TOOL_SEPARATOR.to_owned());
        let named = self
            .program
            .tools
            .iter()
            .filter(|declared| declared.paid)
            .map(|declared| {
                binary(
                    BinaryOp::Equal,
                    Type::Text,
                    tool.clone(),
                    Expr::Text(declared.name.clone()),
                )
            });
        let listed = self
            .program
            .listings
            .iter()
            .filter(|listing| listing.paid)
            .map(|listing| {
                and(
                    text_op(ir::TextOp::Contains, vec![tool.clone(), separator()]),
                    binary(
                        BinaryOp::Equal,
                        Type::Text,
                        text_op(ir::TextOp::Before, vec![tool.clone(), separator()]),
                        Expr::Text(listing.name.clone()),
                    ),
                )
            });
        named
            .chain(listed)
            .reduce(or)
            .expect("a harness whose dispatch pays declares a paid tool or listing")
    }

    fn retried_args(&self, paying: &Paying<'_>) -> Vec<Expr> {
        let reference = Expr::Field(paying.fields.reference);
        match paying.paid {
            Paid::Tool { payment } => {
                let mut retried = paying.args.to_vec();
                retried[payment] = reference;
                retried
            }
            Paid::Dispatch => {
                let call = &paying.args[0];
                vec![Expr::Construct {
                    ty: Type::Sum(ir::CALL),
                    variant: 0,
                    fields: vec![
                        member(call, ir::CALL_ID),
                        member(call, ir::CALL_TOOL),
                        text_op(
                            ir::TextOp::WithPayment,
                            vec![member(call, ir::CALL_INPUT), reference],
                        ),
                    ],
                }]
            }
        }
    }

    pub(super) fn paid_site(
        &mut self,
        kind: SiteKind,
        class: SiteClass,
        result: Type,
        args: Vec<Expr>,
        paid: Paid,
        out: &mut Vec<Stmt>,
    ) -> (Expr, Type) {
        let fields = self.paid_fields();
        out.push(assign(fields.tag, Expr::U64(TAG_ORIGINAL)));
        out.push(assign(fields.more, Expr::Bool(false)));
        out.push(assign(fields.reference, Expr::Text(String::new())));
        let (original, _) = self.site(kind, class, result.clone(), args.clone(), out);
        self.paid_sites.insert(self.sites.len() - 1);
        let paying = Paying {
            kind,
            class,
            result: &result,
            args: &args,
            paid,
            fields,
        };
        let mut arms = self.empty_arms(&result);
        let failed = self.hidden("required", Type::Text);
        arms[ir::FAILED].bindings = vec![Some(failed)];
        arms[ir::FAILED].body = vec![Stmt::If {
            condition: self.payable(Expr::Local(failed), &paying),
            then: vec![
                assign(fields.more, Expr::Bool(true)),
                assign(fields.text, Expr::Local(failed)),
            ],
            otherwise: Vec::new(),
        }];
        out.push(Stmt::Match {
            subject: original.clone(),
            ty: result.clone(),
            arms,
        });
        let round = self.round(&paying);
        let payments = self
            .program
            .wallet
            .as_ref()
            .expect("a paid site belongs to a harness with a wallet")
            .payments
            .clone();
        let index = self.locals.len();
        self.locals.push(ir::Field {
            name: "payment".to_owned(),
            ty: Type::U64,
        });
        out.push(Stmt::Loop {
            index,
            bound: payments,
            body: vec![Stmt::If {
                condition: Expr::Field(fields.more),
                then: round,
                otherwise: Vec::new(),
            }],
        });
        let value = self.value_field(&inner_of(&result));
        let outcome = self.hidden("paid", result.clone());
        out.push(Stmt::Let {
            local: outcome,
            value: Expr::Outcome {
                ty: result.clone(),
                tag: Box::new(Expr::Field(fields.tag)),
                value: Box::new(Expr::Field(value)),
                reason: Box::new(Expr::Field(fields.text)),
                original: Box::new(original),
            },
        });
        (Expr::Local(outcome), result)
    }

    fn empty_arms(&self, ty: &Type) -> Vec<ir::Arm> {
        let variants = ty.variants(self.program).expect("a paid site yields a sum");
        variants
            .iter()
            .enumerate()
            .map(|(variant, declared)| ir::Arm {
                variant,
                bindings: vec![None; declared.fields.len()],
                body: Vec::new(),
            })
            .collect()
    }

    fn round(&mut self, paying: &Paying<'_>) -> Vec<Stmt> {
        let fields = paying.fields;
        let wallet = self
            .program
            .wallet
            .clone()
            .expect("a paid site belongs to a harness with a wallet");
        let spend = Expr::Constant(self.globals.constants[&wallet.spend.name]);
        let charge = Expr::Constant(self.globals.constants[&wallet.charge.name]);
        let roomy = binary(
            BinaryOp::LessEqual,
            Type::U64,
            Expr::Field(fields.spent),
            binary(BinaryOp::Subtract, Type::U64, spend.clone(), charge.clone()),
        );
        let full = self.pay(paying, charge);
        let rest = binary(
            BinaryOp::Subtract,
            Type::U64,
            spend,
            Expr::Field(fields.spent),
        );
        let tight = vec![Stmt::If {
            condition: binary(BinaryOp::Equal, Type::U64, rest.clone(), Expr::U64(0)),
            then: vec![
                assign(fields.tag, Expr::U64(TAG_FAILED)),
                assign(fields.text, Expr::Text(CAP_REACHED.to_owned())),
            ],
            otherwise: self.pay(paying, rest),
        }];
        vec![
            assign(fields.more, Expr::Bool(false)),
            Stmt::If {
                condition: roomy,
                then: full,
                otherwise: tight,
            },
        ]
    }

    fn pay(&mut self, paying: &Paying<'_>, room: Expr) -> Vec<Stmt> {
        let fields = paying.fields;
        let tool = self
            .program
            .wallet
            .as_ref()
            .expect("a paid site belongs to a harness with a wallet")
            .tool;
        let mut body = Vec::new();
        let (charged, charged_ty) = self.site(
            SiteKind::Tool(tool),
            SiteClass::Fixed(EffectClass::External),
            Type::Maybe(Box::new(Type::U64)),
            vec![Expr::Field(fields.text), room.clone()],
            &mut body,
        );
        let amount = self.hidden("charged", Type::U64);
        let refused = self.hidden("refused", Type::Text);
        let mut accepted = vec![
            assign(
                fields.spent,
                binary(
                    BinaryOp::Add,
                    Type::U64,
                    Expr::Field(fields.spent),
                    Expr::Local(amount),
                ),
            ),
            assign(fields.reference, Expr::Field(fields.text)),
        ];
        accepted.extend(self.retry(paying));
        let arms = vec![
            ir::Arm {
                variant: ir::OK,
                bindings: vec![Some(amount)],
                body: vec![Stmt::If {
                    condition: binary(
                        BinaryOp::LessEqual,
                        Type::U64,
                        Expr::Local(amount),
                        room.clone(),
                    ),
                    then: accepted,
                    otherwise: vec![
                        assign(fields.tag, Expr::U64(TAG_FAILED)),
                        assign(fields.text, Expr::Text(ir::PAYMENT_OVERCHARGED.to_owned())),
                    ],
                }],
            },
            ir::Arm {
                variant: ir::FAILED,
                bindings: vec![Some(refused)],
                body: vec![
                    assign(fields.tag, Expr::U64(TAG_FAILED)),
                    assign(fields.text, Expr::Local(refused)),
                ],
            },
            ir::Arm {
                variant: ir::UNKNOWN,
                bindings: Vec::new(),
                body: vec![
                    assign(
                        fields.spent,
                        binary(BinaryOp::Add, Type::U64, Expr::Field(fields.spent), room),
                    ),
                    assign(fields.tag, Expr::U64(ir::OUTCOME_TAG_UNKNOWN)),
                ],
            },
        ];
        body.push(Stmt::Match {
            subject: charged,
            ty: charged_ty,
            arms,
        });
        body
    }

    fn retry(&mut self, paying: &Paying<'_>) -> Vec<Stmt> {
        let fields = paying.fields;
        let mut body = Vec::new();
        let retried = self.retried_args(paying);
        let (answer, _) = self.site(
            paying.kind,
            paying.class,
            paying.result.clone(),
            retried,
            &mut body,
        );
        self.paid_sites.insert(self.sites.len() - 1);
        let inner = inner_of(paying.result);
        let value_field = self.value_field(&inner);
        let mut arms = self.empty_arms(paying.result);
        let value = self.hidden("paid_value", inner);
        arms[ir::OK].bindings = vec![Some(value)];
        arms[ir::OK].body = vec![
            assign(fields.tag, Expr::U64(TAG_OK)),
            assign(value_field, Expr::Local(value)),
        ];
        let failed: LocalId = self.hidden("paid_failed", Type::Text);
        arms[ir::FAILED].bindings = vec![Some(failed)];
        arms[ir::FAILED].body = vec![
            assign(fields.tag, Expr::U64(TAG_FAILED)),
            assign(fields.text, Expr::Local(failed)),
            Stmt::If {
                condition: self.payable(Expr::Local(failed), paying),
                then: vec![assign(fields.more, Expr::Bool(true))],
                otherwise: Vec::new(),
            },
        ];
        arms[ir::UNKNOWN].body = vec![assign(fields.tag, Expr::U64(ir::OUTCOME_TAG_UNKNOWN))];
        for arm in arms.iter_mut().skip(ir::UNKNOWN + 1) {
            arm.body = vec![
                assign(fields.tag, Expr::U64(TAG_FAILED)),
                assign(fields.text, Expr::Text(ATTACHED_REFUSED.to_owned())),
            ];
        }
        body.push(Stmt::Match {
            subject: answer,
            ty: paying.result.clone(),
            arms,
        });
        body
    }
}
