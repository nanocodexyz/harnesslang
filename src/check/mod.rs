mod handler;
mod inline;

pub use inline::STATEMENTS_PER_HANDLER_MAX;

pub type Loader<'l> = dyn FnMut(&str) -> Result<ir::Program, String> + 'l;

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{self, Bound, Sum, SumKind, Type, Variant};
use crate::syntax::ast::{self, Item, TypeBody, TypeExpr};
use crate::syntax::{Diagnostic, Position};

pub const BUILTIN_TYPES: [&str; 6] = ["u64", "bool", "text", "list", "known", "maybe"];
pub const BUILTIN_SUM_NAMES: [&str; 3] = ["call", "reply", "entry"];
pub const BUILTIN_FUNCTIONS: [&str; 15] = [
    "dispatch",
    "now",
    "random",
    "len",
    "wait",
    "hold",
    "suspend",
    "contains",
    "before",
    "after",
    "trim",
    "truncate_middle",
    "deny",
    "allow",
    "join",
];
pub const DENY: &str = "deny";
pub const ALLOW: &str = "allow";
pub const JOIN: &str = "join";
pub const WILDCARD: &str = "_";
pub const EVENT_CANCEL: &str = "cancel";
pub const EVENT_STEER: &str = "steer";
pub const EVENT_RESUME: &str = "resume";
pub const STEER_PARAMETER: &str = "message";
pub const CANCELLED_READ: &str = "cancelled";
pub const STEERS_READ: &str = "steers";
pub const EVENT_JOB: &str = "job";
pub const JOB_PARAMETER: &str = "notice";
pub const JOBS_READ: &str = "jobs";

pub struct Globals {
    constants: BTreeMap<String, ir::ConstId>,
    types: BTreeMap<String, ir::SumId>,
    tools: BTreeMap<String, ir::ToolId>,
    codes: BTreeMap<String, ir::CodeId>,
    listings: BTreeMap<String, ir::ListingId>,
    fields: BTreeMap<String, ir::FieldId>,
    model: Option<String>,
    hooks: BTreeMap<ast::HookKind, ast::Hook>,
}

impl Globals {
    fn effect(&self, name: &str) -> bool {
        self.tools.contains_key(name)
            || self.listings.contains_key(name)
            || self.codes.contains_key(name)
            || self.model.as_deref() == Some(name)
    }
}

type Checked<T> = Result<T, Diagnostic>;

fn error<T>(at: Position, message: impl Into<String>) -> Checked<T> {
    Err(Diagnostic::new(at, message))
}

pub fn check(source: &ast::Program) -> Checked<ir::Program> {
    check_with(source, &mut |file: &str| {
        Err(format!(
            "this check reads no other file, so it cannot load \"{file}\""
        ))
    })
}

pub fn check_with(source: &ast::Program, load: &mut Loader<'_>) -> Checked<ir::Program> {
    let mut program = ir::Program {
        name: source.name.text.clone(),
        constants: Vec::new(),
        sums: Vec::new(),
        tools: Vec::new(),
        codes: Vec::new(),
        listings: Vec::new(),
        model: None,
        state: Vec::new(),
        invariants: Vec::new(),
        handlers: Vec::new(),
        sides: Vec::new(),
        events: ir::Events::default(),
        cuts: Vec::new(),
        notice: None,
        blobs: None,
        idle: None,
    };
    let mut globals = Globals {
        constants: BTreeMap::new(),
        types: BTreeMap::new(),
        tools: BTreeMap::new(),
        codes: BTreeMap::new(),
        listings: BTreeMap::new(),
        fields: BTreeMap::new(),
        model: None,
        hooks: BTreeMap::new(),
    };
    let mut values: BTreeMap<String, Position> = BTreeMap::new();
    constants(source, &mut program, &mut globals, &mut values)?;
    model(source, &mut program, &mut globals, &mut values)?;
    sums(source, &mut program, &mut globals)?;
    tools(source, &mut program, &mut globals, &mut values)?;
    spawns(source, &mut program, &mut globals, &mut values, load)?;
    codes(source, &mut program, &mut globals, &mut values)?;
    listings(source, &mut program, &mut globals, &mut values)?;
    state(source, &mut program, &mut globals, &mut values)?;
    events(source, &mut program, &globals, &mut values)?;
    interrupts(source, &mut program)?;
    let expanded = inline::expand(source, &program, &globals, &mut values)?;
    globals.hooks = hooks(&expanded, &program, &globals)?;
    let expanded = session_end(&expanded).unwrap_or(expanded);
    let expanded = idle(&expanded, &mut program, &globals)?.unwrap_or(expanded);
    invariants(&expanded, &mut program, &globals)?;
    handlers(&expanded, &mut program, &globals)?;
    program.cuts = program.find_cuts();
    ir::validate::program(&program);
    Ok(program)
}

fn claim(values: &mut BTreeMap<String, Position>, name: &ast::Name, what: &str) -> Checked<()> {
    if name.text.starts_with(WILDCARD) {
        return error(
            name.at,
            format!(
                "{what} '{}': names starting with '_' are reserved",
                name.text
            ),
        );
    }
    if BUILTIN_FUNCTIONS.contains(&name.text.as_str()) {
        return error(
            name.at,
            format!("{what} '{}' has the name of a builtin", name.text),
        );
    }
    if let Some(previous) = values.get(&name.text) {
        return error(
            name.at,
            format!("'{}' is already declared at {previous}", name.text),
        );
    }
    values.insert(name.text.clone(), name.at);
    Ok(())
}

fn constants(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    for item in &source.items {
        let Item::Const(constant) = item else {
            continue;
        };
        claim(values, &constant.name, "constant")?;
        match &constant.ty {
            TypeExpr::Named(name) if name.text == "u64" => {}
            other => return error(other.at(), "constants are u64"),
        }
        globals
            .constants
            .insert(constant.name.text.clone(), program.constants.len());
        program.constants.push(ir::Constant {
            name: constant.name.text.clone(),
            value: constant.value,
        });
    }
    Ok(())
}

fn bound(program: &ir::Program, globals: &Globals, name: &ast::Name, what: &str) -> Checked<Bound> {
    match globals.constants.get(&name.text) {
        Some(id) => Ok(Bound {
            name: name.text.clone(),
            value: program.constants[*id].value,
        }),
        None => error(
            name.at,
            format!(
                "{what} must be a named constant: '{}' is not a constant",
                name.text
            ),
        ),
    }
}

fn model(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    for item in &source.items {
        let Item::Model(model) = item else { continue };
        if let Some(previous) = &program.model {
            return error(
                model.name.at,
                format!(
                    "a harness has one model; '{}' is already declared",
                    previous.name
                ),
            );
        }
        claim(values, &model.name, "model")?;
        let calls = bound(program, globals, &model.calls, "the model's calls bound")?;
        let mut modes: Vec<String> = Vec::with_capacity(model.modes.len());
        for mode in &model.modes {
            if modes.contains(&mode.text) {
                return error(
                    mode.at,
                    format!(
                        "model '{}' declares mode '{}' twice",
                        model.name.text, mode.text
                    ),
                );
            }
            modes.push(mode.text.clone());
        }
        if let (Some(event), None) = (model.interrupts.first(), &model.window) {
            return error(
                event.at,
                format!(
                    "model '{}' is interruptible only with a window: its interrupted call is the 'interrupted' variant of attempt",
                    model.name.text
                ),
            );
        }
        globals.model = Some(model.name.text.clone());
        program.model = Some(ir::Model {
            name: model.name.text.clone(),
            calls,
            reply: ir::Reply::Builtin,
            window: None,
            modes,
            interrupts: ir::Interrupts::default(),
        });
    }
    Ok(())
}

fn declared_reply(source: &ast::Program) -> Option<&ast::Name> {
    source.items.iter().find_map(|item| match item {
        Item::Model(model) => model.reply.as_ref(),
        _ => None,
    })
}

fn builtin_sums(program: &ir::Program, declared: Option<&str>) -> Vec<Sum> {
    let call = Sum {
        name: BUILTIN_SUM_NAMES[ir::CALL].to_owned(),
        kind: SumKind::Tagged,
        variants: vec![Variant {
            name: BUILTIN_SUM_NAMES[ir::CALL].to_owned(),
            fields: vec![Type::Text, Type::Text, Type::Text],
        }],
    };
    let Some(model) = &program.model else {
        let placeholder = |name: &str| Sum {
            name: name.to_owned(),
            kind: SumKind::Tagged,
            variants: Vec::new(),
        };
        return vec![
            call,
            placeholder(BUILTIN_SUM_NAMES[ir::REPLY]),
            placeholder(BUILTIN_SUM_NAMES[ir::ENTRY]),
        ];
    };
    let variant = |name: &str, fields: Vec<Type>| Variant {
        name: name.to_owned(),
        fields,
    };
    let calls = Type::List {
        element: Box::new(Type::Sum(ir::CALL)),
        capacity: model.calls.clone(),
    };
    let reply = match declared {
        Some(name) => Sum {
            name: name.to_owned(),
            kind: SumKind::Tagged,
            variants: Vec::new(),
        },
        None => Sum {
            name: BUILTIN_SUM_NAMES[ir::REPLY].to_owned(),
            kind: SumKind::Tagged,
            variants: vec![
                variant("answer", vec![Type::Text]),
                variant("calls", vec![calls]),
            ],
        },
    };
    let call_type = Type::Sum(ir::CALL);
    let entry = Sum {
        name: BUILTIN_SUM_NAMES[ir::ENTRY].to_owned(),
        kind: SumKind::Tagged,
        variants: vec![
            variant("user", vec![Type::Text]),
            variant("assistant", vec![Type::Sum(ir::REPLY)]),
            variant("result", vec![call_type.clone(), Type::Text]),
            variant("error", vec![call_type.clone(), Type::Text]),
            variant("unknown", vec![call_type]),
        ],
    };
    vec![call, reply, entry]
}

fn declared_interruptible(source: &ast::Program) -> bool {
    source.items.iter().any(|item| match item {
        Item::Model(model) => !model.interrupts.is_empty(),
        _ => false,
    })
}

fn declared_window(source: &ast::Program) -> Option<&ast::Name> {
    source.items.iter().find_map(|item| match item {
        Item::Model(model) => model.window.as_ref(),
        _ => None,
    })
}

fn window(source: &ast::Program, program: &mut ir::Program, globals: &mut Globals) -> Checked<()> {
    let Some(name) = declared_window(source) else {
        return Ok(());
    };
    let tokens = bound(program, globals, name, "the model's context window")?;
    if tokens.value == 0 {
        return error(
            name.at,
            format!("the model's context window '{}' is 0 tokens", name.text),
        );
    }
    let stop = program.sums.len();
    program.sums.push(Sum {
        name: ir::STOP_TYPE.to_owned(),
        kind: SumKind::Text,
        variants: ir::STOP_VALUES
            .iter()
            .map(|value| Variant {
                name: (*value).to_owned(),
                fields: Vec::new(),
            })
            .collect(),
    });
    let attempt = program.sums.len();
    program.sums.push(Sum {
        name: ir::ATTEMPT_TYPE.to_owned(),
        kind: SumKind::Tagged,
        variants: ir::attempt_variants(stop, declared_interruptible(source)),
    });
    globals.types.insert(ir::STOP_TYPE.to_owned(), stop);
    globals.types.insert(ir::ATTEMPT_TYPE.to_owned(), attempt);
    program
        .model
        .as_mut()
        .expect("a window belongs to the model")
        .window = Some(ir::Window {
        tokens,
        stop,
        attempt,
    });
    Ok(())
}

fn notice(source: &ast::Program, program: &mut ir::Program) {
    let listed = source
        .items
        .iter()
        .any(|item| matches!(item, Item::Listing(_)));
    if program.model.is_none() || !listed {
        return;
    }
    let entry = &mut program.sums[ir::ENTRY];
    program.notice = Some(entry.variants.len());
    entry.variants.push(Variant {
        name: ir::NOTICE_VARIANT.to_owned(),
        fields: vec![Type::Text],
    });
}

fn blob_bound(source: &ast::Program) -> Checked<Option<&ast::Name>> {
    let mut found: Option<&ast::Name> = None;
    for item in &source.items {
        let Item::Tool(tool) = item else { continue };
        let returned = [Some(&tool.returns), tool.attaches.as_ref()];
        let Some(TypeExpr::Blob { bytes, .. }) = returned
            .into_iter()
            .flatten()
            .find(|ty| matches!(ty, TypeExpr::Blob { .. }))
        else {
            continue;
        };
        match found {
            Some(first) if first.text != bytes.text => {
                return error(
                    bytes.at,
                    format!(
                        "tool '{}' returns blob<{}>, but another tool returns blob<{}>: the blobs a dispatch returns share one bound",
                        tool.name.text, bytes.text, first.text
                    ),
                );
            }
            _ => found = Some(bytes),
        }
    }
    Ok(found)
}

fn blobs(source: &ast::Program, program: &mut ir::Program, globals: &Globals) -> Checked<()> {
    let Some(name) = blob_bound(source)? else {
        return Ok(());
    };
    if program.model.is_none() {
        return error(
            name.at,
            "a tool that returns a blob needs a model declaration: its blobs reach the model as context entries",
        );
    }
    let bytes = bound(program, globals, name, "the bytes a blob may hold")?;
    if bytes.value == 0 {
        return error(name.at, format!("blob bound '{}' is 0 bytes", name.text));
    }
    let blob = Type::Blob(bytes.clone());
    let fields = [
        vec![Type::Text],
        vec![Type::Text],
        Vec::new(),
        vec![blob.clone()],
    ];
    let dispatched = program.sums.len();
    program.sums.push(Sum {
        name: ir::DISPATCHED_TYPE.to_owned(),
        kind: SumKind::Tagged,
        variants: ir::DISPATCHED_VARIANTS
            .iter()
            .zip(fields)
            .map(|(name, fields)| Variant {
                name: (*name).to_owned(),
                fields,
            })
            .collect(),
    });
    let entry = &mut program.sums[ir::ENTRY];
    let attached = entry.variants.len();
    entry.variants.push(Variant {
        name: ir::ATTACHED_VARIANT.to_owned(),
        fields: vec![Type::Sum(ir::CALL), blob],
    });
    program.blobs = Some(ir::Blobs {
        bytes,
        dispatched,
        attached,
    });
    Ok(())
}

fn sums(source: &ast::Program, program: &mut ir::Program, globals: &mut Globals) -> Checked<()> {
    let reply = declared_reply(source);
    program.sums = builtin_sums(program, reply.map(|name| name.text.as_str()));
    for (id, name) in BUILTIN_SUM_NAMES.iter().enumerate() {
        if id != ir::REPLY || reply.is_none() {
            globals.types.insert((*name).to_owned(), id);
        }
    }
    window(source, program, globals)?;
    notice(source, program);
    blobs(source, program, globals)?;
    let mut declared = Vec::new();
    for item in &source.items {
        let Item::Type(definition) = item else {
            continue;
        };
        register(program, globals, definition, reply)?;
        declared.push(definition);
    }
    if let Some(reply) = reply
        && globals.types.get(&reply.text) != Some(&ir::REPLY)
    {
        return error(
            reply.at,
            format!(
                "the model's reply type '{}' is not a type this harness declares",
                reply.text
            ),
        );
    }
    for definition in &declared {
        let id = globals.types[&definition.name.text];
        program.sums[id].variants = match &definition.body {
            TypeBody::Variants(variants) => {
                tagged_variants(program, globals, definition, variants)?
            }
            TypeBody::Values(values) => text_values(definition, values)?,
        };
    }
    acyclic(source, program, globals)?;
    let Some(reply) = reply else {
        return Ok(());
    };
    let definition = declared
        .iter()
        .find(|definition| definition.name.text == reply.text)
        .expect("the reply type is declared, checked above");
    let shape = reply_shape(program, definition)?;
    program
        .model
        .as_mut()
        .expect("a declared reply belongs to the model")
        .reply = shape;
    Ok(())
}

fn register(
    program: &mut ir::Program,
    globals: &mut Globals,
    definition: &ast::TypeDef,
    reply: Option<&ast::Name>,
) -> Checked<()> {
    let name = &definition.name;
    if BUILTIN_TYPES.contains(&name.text.as_str()) || globals.types.contains_key(&name.text) {
        return error(
            name.at,
            format!("type '{}' is already declared or builtin", name.text),
        );
    }
    let kind = match definition.body {
        TypeBody::Variants(_) => SumKind::Tagged,
        TypeBody::Values(_) => SumKind::Text,
    };
    if !reply.is_some_and(|reply| reply.text == name.text) {
        globals.types.insert(name.text.clone(), program.sums.len());
        program.sums.push(Sum {
            name: name.text.clone(),
            kind,
            variants: Vec::new(),
        });
        return Ok(());
    }
    if kind == SumKind::Text {
        return error(
            name.at,
            format!(
                "reply type '{}' is a text enum: a reply is a sum whose variants carry fields",
                name.text
            ),
        );
    }
    globals.types.insert(name.text.clone(), ir::REPLY);
    Ok(())
}

fn reply_shape(program: &ir::Program, definition: &ast::TypeDef) -> Checked<ir::Reply> {
    let model = program
        .model
        .as_ref()
        .expect("a declared reply belongs to the model");
    let TypeBody::Variants(declared) = &definition.body else {
        unreachable!("a text enum reply is refused when it is declared");
    };
    let reply = &definition.name.text;
    let calls = Type::List {
        element: Box::new(Type::Sum(ir::CALL)),
        capacity: model.calls.clone(),
    };
    let sum = &program.sums[ir::REPLY];
    let mut carrier: Option<(usize, usize)> = None;
    for (index, (variant, source)) in sum.variants.iter().zip(declared).enumerate() {
        if variant.name == ir::MODEL_FAILED {
            return error(
                source.name.at,
                format!(
                    "reply type '{reply}' has a variant named '{}', the name a failed model call is recorded under",
                    ir::MODEL_FAILED
                ),
            );
        }
        for (field_index, (field, at)) in variant.fields.iter().zip(&source.fields).enumerate() {
            if *field == calls {
                if let Some((previous, _)) = carrier {
                    return error(
                        at.at(),
                        format!(
                            "reply type '{reply}' carries {} in '{}' and '{}': exactly one variant carries the model's calls",
                            field.name(program),
                            sum.variants[previous].name,
                            variant.name
                        ),
                    );
                }
                carrier = Some((index, field_index));
                continue;
            }
            reply_field(program, reply, &variant.name, field, &calls, at.at())?;
        }
    }
    let Some((calls_variant, field)) = carrier else {
        return error(
            definition.name.at,
            format!(
                "reply type '{reply}' has no variant carrying {}: exactly one variant carries the model's calls",
                calls.name(program)
            ),
        );
    };
    Ok(ir::Reply::Declared {
        calls: calls_variant,
        field,
    })
}

fn reply_field(
    program: &ir::Program,
    reply: &str,
    variant: &str,
    field: &Type,
    calls: &Type,
    at: Position,
) -> Checked<()> {
    let scalar = matches!(field, Type::U64 | Type::Bool | Type::Text);
    if scalar || program.text_enum(field).is_some() {
        return Ok(());
    }
    let what = match field {
        Type::List { element, .. } if **element == Type::Sum(ir::CALL) => format!(
            "carries {}, but the model's calls bound is {}",
            field.name(program),
            calls.name(program)
        ),
        _ => format!(
            "carries {}: a reply's fields are u64, bool, text, text enums and the one {}",
            field.name(program),
            calls.name(program)
        ),
    };
    error(
        at,
        format!("variant '{variant}' of reply type '{reply}' {what}"),
    )
}

pub fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn tagged_variants(
    program: &ir::Program,
    globals: &Globals,
    definition: &ast::TypeDef,
    declared: &[ast::VariantDef],
) -> Checked<Vec<Variant>> {
    let mut seen = BTreeSet::new();
    let mut variants = Vec::new();
    for variant in declared {
        if !seen.insert(variant.name.text.clone()) {
            return error(
                variant.name.at,
                format!(
                    "variant '{}' appears twice in '{}'",
                    variant.name.text, definition.name.text
                ),
            );
        }
        let mut fields = Vec::new();
        for field in &variant.fields {
            fields.push(resolve(program, globals, field)?);
        }
        variants.push(Variant {
            name: variant.name.text.clone(),
            fields,
        });
    }
    Ok(variants)
}

fn text_values(definition: &ast::TypeDef, values: &[ast::Name]) -> Checked<Vec<Variant>> {
    let mut seen = BTreeSet::new();
    let mut variants = Vec::new();
    for value in values {
        if !seen.insert(value.text.clone()) {
            return error(
                value.at,
                format!(
                    "value {} appears twice in '{}'",
                    quoted(&value.text),
                    definition.name.text
                ),
            );
        }
        variants.push(Variant {
            name: value.text.clone(),
            fields: Vec::new(),
        });
    }
    Ok(variants)
}

fn contained(ty: &Type, out: &mut Vec<ir::SumId>) {
    match ty {
        Type::U64 | Type::Bool | Type::Text | Type::Blob(_) => {}
        Type::Sum(sum) => out.push(*sum),
        Type::List { element, .. } => contained(element, out),
        Type::Known(inner) | Type::Maybe(inner) => contained(inner, out),
    }
}

fn acyclic(source: &ast::Program, program: &ir::Program, globals: &Globals) -> Checked<()> {
    for item in &source.items {
        let Item::Type(definition) = item else {
            continue;
        };
        let start = globals.types[&definition.name.text];
        let mut stack = vec![start];
        let mut visited = BTreeSet::new();
        while let Some(sum) = stack.pop() {
            assert!(
                visited.len() <= program.sums.len(),
                "a walk visits each sum at most once"
            );
            let mut next = Vec::new();
            for variant in &program.sums[sum].variants {
                variant
                    .fields
                    .iter()
                    .for_each(|field| contained(field, &mut next));
            }
            for inner in next {
                if inner == start {
                    return error(
                        definition.name.at,
                        format!(
                            "type '{}' contains itself, so its values have no bound",
                            definition.name.text
                        ),
                    );
                }
                if visited.insert(inner) {
                    stack.push(inner);
                }
            }
        }
    }
    Ok(())
}

pub fn resolve(program: &ir::Program, globals: &Globals, ty: &TypeExpr) -> Checked<Type> {
    match ty {
        TypeExpr::Named(name) => match name.text.as_str() {
            "u64" => Ok(Type::U64),
            "bool" => Ok(Type::Bool),
            "text" => Ok(Type::Text),
            other => match globals.types.get(other) {
                Some(&sum) if (sum == ir::REPLY || sum == ir::ENTRY) && program.model.is_none() => {
                    error(
                        name.at,
                        format!(
                            "type '{other}' needs a model declaration: its replies are bounded by the model's calls"
                        ),
                    )
                }
                Some(&sum) => Ok(Type::Sum(sum)),
                None if other == BUILTIN_SUM_NAMES[ir::REPLY]
                    && program.model.as_ref().is_some_and(ir::Model::declared) =>
                {
                    error(
                        name.at,
                        format!(
                            "unknown type 'reply': the model replies with '{}'",
                            program.sums[ir::REPLY].name
                        ),
                    )
                }
                None => error(name.at, format!("unknown type '{other}'")),
            },
        },
        TypeExpr::List {
            element, capacity, ..
        } => Ok(Type::List {
            element: Box::new(resolve(program, globals, element)?),
            capacity: bound(program, globals, capacity, "a list's capacity")?,
        }),
        TypeExpr::Known { inner, .. } => {
            Ok(Type::Known(Box::new(resolve(program, globals, inner)?)))
        }
        TypeExpr::Maybe { inner, .. } => {
            Ok(Type::Maybe(Box::new(resolve(program, globals, inner)?)))
        }
        TypeExpr::Blob { bytes, .. } => {
            let bytes = bound(program, globals, bytes, "the bytes a blob may hold")?;
            Ok(Type::Blob(bytes))
        }
    }
}

fn no_blob(program: &ir::Program, at: Position, ty: &Type, what: &str) -> Checked<()> {
    if ty.holds_blob(program) {
        return error(
            at,
            format!(
                "{what} cannot hold a blob: a blob comes from a tool and goes to the model, and stays in state lists between them"
            ),
        );
    }
    Ok(())
}

fn variant_names(program: &ir::Program) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = ir::OUTCOME_VARIANTS
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    for sum in program
        .sums
        .iter()
        .filter(|sum| sum.kind == SumKind::Tagged)
    {
        names.extend(sum.variants.iter().map(|variant| variant.name.clone()));
    }
    names
}

fn tools(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    let variants = variant_names(program);
    for item in &source.items {
        let Item::Tool(tool) = item else { continue };
        claim(values, &tool.name, "tool")?;
        if variants.contains(&tool.name.text) {
            return error(
                tool.name.at,
                format!("tool '{}' has the name of a variant", tool.name.text),
            );
        }
        let params = params(program, globals, &tool.params)?;
        let defaults = defaults(program, globals, &params, &tool.defaults)?;
        let returns = resolve(program, globals, &tool.returns)?;
        if !matches!(returns, Type::U64 | Type::Bool | Type::Text | Type::Blob(_))
            && program.text_enum(&returns).is_none()
        {
            return error(
                tool.returns.at(),
                "a tool returns u64, bool, text, a text enum or a blob",
            );
        }
        let attaches = match &tool.attaches {
            None => None,
            Some(ty) => match (&returns, resolve(program, globals, ty)?) {
                (Type::Text, Type::Blob(bytes)) => Some(bytes),
                _ => {
                    return error(
                        ty.at(),
                        "a tool returns 'text or blob<N>': text, or a blob the model sees as an attachment",
                    );
                }
            },
        };
        if let Some(at) = tool.recoverable
            && tool.effect != crate::effect::EffectClass::External
        {
            return error(
                at,
                format!(
                    "tool '{}' has effect {}: only an external tool is recoverable, since only its outcome can be unknown",
                    tool.name.text,
                    tool.effect.name()
                ),
            );
        }
        let jobs = tool_jobs(source, program, globals, tool, &returns)?;
        globals
            .tools
            .insert(tool.name.text.clone(), program.tools.len());
        program.tools.push(ir::Tool {
            name: tool.name.text.clone(),
            params,
            defaults,
            returns,
            attaches,
            effect: tool.effect,
            recoverable: tool.recoverable.is_some(),
            interrupts: ir::Interrupts::default(),
            jobs,
            spawn: None,
            finds: None,
        });
    }
    Ok(())
}

fn tool_jobs(
    source: &ast::Program,
    program: &ir::Program,
    globals: &Globals,
    tool: &ast::Tool,
    returns: &Type,
) -> Checked<Option<Bound>> {
    let Some(name) = &tool.jobs else {
        return Ok(None);
    };
    if tool.effect != crate::effect::EffectClass::External || *returns != Type::Text {
        return error(
            name.at,
            format!(
                "tool '{}' keeps jobs running: it must be external and return text, the message the model reads when its call becomes a job",
                tool.name.text
            ),
        );
    }
    let noted = source
        .items
        .iter()
        .any(|item| matches!(item, Item::Event(event) if event.name.text == EVENT_JOB));
    if !noted {
        return error(
            name.at,
            format!(
                "tool '{}' keeps jobs running, so the harness needs 'on {EVENT_JOB}({JOB_PARAMETER}: text) bound N' to take their notices",
                tool.name.text
            ),
        );
    }
    let jobs = bound(
        program,
        globals,
        name,
        "the jobs a tool keeps running at once",
    )?;
    if jobs.value == 0 {
        return error(
            name.at,
            format!(
                "tool '{}' keeps at most 0 jobs: drop 'jobs at most'",
                tool.name.text
            ),
        );
    }
    Ok(Some(jobs))
}

fn interrupts(source: &ast::Program, program: &mut ir::Program) -> Checked<()> {
    for item in &source.items {
        let (names, owner) = match item {
            Item::Tool(tool) => (&tool.interrupts, Some(tool.name.text.as_str())),
            Item::Model(model) => (&model.interrupts, None),
            _ => continue,
        };
        let mut interrupts = ir::Interrupts::default();
        for name in names {
            let (declared, slot) = match name.text.as_str() {
                EVENT_CANCEL => (program.events.cancel, &mut interrupts.cancel),
                EVENT_STEER => (program.events.steers.is_some(), &mut interrupts.steer),
                other => {
                    return error(
                        name.at,
                        format!(
                            "'{other}' cannot interrupt an effect: write 'interruptible by {EVENT_CANCEL}', '{EVENT_STEER}' or both"
                        ),
                    );
                }
            };
            if !declared {
                return error(
                    name.at,
                    format!(
                        "'interruptible by {}' needs 'on {}' on the harness",
                        name.text, name.text
                    ),
                );
            }
            if *slot {
                return error(
                    name.at,
                    format!("'{}' interrupts the effect twice", name.text),
                );
            }
            *slot = true;
        }
        match owner {
            Some(tool) => {
                let (id, _) = program
                    .tool(tool)
                    .expect("tools are checked before interrupts");
                program.tools[id].interrupts = interrupts;
            }
            None => {
                program
                    .model
                    .as_mut()
                    .expect("the model is checked before interrupts")
                    .interrupts = interrupts;
            }
        }
    }
    Ok(())
}

fn spawns(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
    load: &mut Loader<'_>,
) -> Checked<()> {
    let variants = variant_names(program);
    for item in &source.items {
        let Item::Spawn(spawn) = item else { continue };
        claim(values, &spawn.name, "spawn tool")?;
        if variants.contains(&spawn.name.text) {
            return error(
                spawn.name.at,
                format!("spawn tool '{}' has the name of a variant", spawn.name.text),
            );
        }
        let child = load(&spawn.file.text).map_err(|message| {
            Diagnostic::new(
                spawn.file.at,
                format!(
                    "spawn tool '{}' cannot check \"{}\": {message}",
                    spawn.name.text, spawn.file.text
                ),
            )
        })?;
        let Some(handler) = child.handler(&spawn.handler.text) else {
            return error(
                spawn.handler.at,
                format!(
                    "harness '{}' has no handler '{}'",
                    child.name, spawn.handler.text
                ),
            );
        };
        let mut params = Vec::with_capacity(handler.params.len());
        for local in &handler.params {
            let field = &handler.locals[*local];
            if !matches!(field.ty, Type::U64 | Type::Bool | Type::Text) {
                return error(
                    spawn.handler.at,
                    format!(
                        "handler '{}' of harness '{}' takes '{}' of type {}: a spawned handler takes u64, bool or text, which its parent's tool input carries",
                        spawn.handler.text,
                        child.name,
                        field.name,
                        field.ty.name(&child)
                    ),
                );
            }
            params.push(field.clone());
        }
        let sessions = spawn_sessions(program, globals, spawn, &child)?;
        globals
            .tools
            .insert(spawn.name.text.clone(), program.tools.len());
        program.tools.push(ir::Tool {
            name: spawn.name.text.clone(),
            defaults: vec![None; params.len()],
            params,
            returns: Type::Text,
            attaches: None,
            effect: crate::effect::EffectClass::External,
            recoverable: false,
            interrupts: ir::Interrupts::default(),
            jobs: None,
            finds: None,
            spawn: Some(ir::Spawn {
                file: spawn.file.text.clone(),
                handler: spawn.handler.text.clone(),
                program: Box::new(child),
                sessions,
            }),
        });
    }
    Ok(())
}

fn spawn_sessions(
    program: &ir::Program,
    globals: &Globals,
    spawn: &ast::Spawn,
    child: &ir::Program,
) -> Checked<Option<Bound>> {
    let sessions = match &spawn.sessions {
        Some(name) => Some(bound(
            program,
            globals,
            name,
            "the most sessions one spawn starts",
        )?),
        None => None,
    };
    let Some(handler) = child.handler(&spawn.handler.text) else {
        return Ok(sessions);
    };
    let Some(family) = &handler.worst.family else {
        return Ok(sessions);
    };
    let Some(limit) = &sessions else {
        return error(
            spawn.file.at,
            format!(
                "handler '{}' of harness '{}' starts child sessions of its own: declare how many sessions one call of '{}' starts, the child's and its descendants', as spawn {} runs {} from \"{}\" sessions at most constant",
                spawn.handler.text,
                child.name,
                spawn.name.text,
                spawn.name.text,
                spawn.handler.text,
                spawn.file.text
            ),
        );
    };
    let depth = family.depth + 1;
    if depth > ir::SPAWN_DEPTH_MAX {
        return error(
            spawn.file.at,
            format!(
                "spawn tool '{}' starts a tree {depth} levels deep, over spawn_depth_max ({}) levels",
                spawn.name.text,
                ir::SPAWN_DEPTH_MAX
            ),
        );
    }
    let Some(tree) = family.sessions.checked_add(1) else {
        return error(
            spawn.file.at,
            format!(
                "spawn tool '{}' starts more sessions per call than u64 counts",
                spawn.name.text
            ),
        );
    };
    if tree > limit.value {
        return error(
            spawn.file.at,
            format!(
                "spawn tool '{}' starts up to {tree} sessions per call ({}.{} and {} descendant sessions, {}), over sessions at most {} ({})",
                spawn.name.text,
                child.name,
                spawn.handler.text,
                family.sessions,
                handler.worst.spawns.describe_loops(),
                limit.name,
                limit.value
            ),
        );
    }
    Ok(sessions)
}

fn hooks(
    source: &ast::Program,
    program: &ir::Program,
    globals: &Globals,
) -> Checked<BTreeMap<ast::HookKind, ast::Hook>> {
    use ast::HookKind;
    let mut hooks: BTreeMap<HookKind, ast::Hook> = BTreeMap::new();
    for item in &source.items {
        let Item::Hook(hook) = item else { continue };
        if let Some(previous) = hooks.get(&hook.kind) {
            return error(
                hook.at,
                format!(
                    "'{}' is already declared at {}",
                    hook.kind.describe(),
                    previous.at
                ),
            );
        }
        let call = Type::Sum(ir::CALL);
        let outcome = Type::Maybe(Box::new(Type::Text));
        let wanted: Option<Vec<Type>> = match hook.kind {
            HookKind::BeforeTool => Some(vec![call]),
            HookKind::AfterTool => Some(vec![call, outcome]),
            HookKind::SessionStart
            | HookKind::PreCompact
            | HookKind::PostCompact
            | HookKind::Stop => Some(Vec::new()),
            HookKind::PromptSubmit => None,
            HookKind::SubagentStart => Some(vec![Type::Text]),
            HookKind::SubagentStop => Some(vec![Type::Text, Type::Text]),
        };
        let mut found = Vec::with_capacity(hook.params.len());
        for param in &hook.params {
            found.push(resolve(program, globals, &param.ty)?);
        }
        let shape = match hook.kind {
            HookKind::BeforeTool => "(c: call)",
            HookKind::AfterTool => "(c: call, outcome: maybe<text>)",
            HookKind::PromptSubmit => "(p: T), naming a parameter of the handlers it runs in",
            HookKind::SubagentStart => "(agent: text)",
            HookKind::SubagentStop => "(agent: text, result: text)",
            HookKind::SessionStart
            | HookKind::PreCompact
            | HookKind::PostCompact
            | HookKind::Stop => "no parameters",
        };
        let fits = match &wanted {
            Some(wanted) => *wanted == found,
            None => found.len() == 1,
        };
        if !fits {
            return error(hook.at, format!("'{}' takes {shape}", hook.kind.describe()));
        }
        if matches!(hook.kind, HookKind::SubagentStart | HookKind::SubagentStop)
            && program.spawns().next().is_none()
        {
            return error(
                hook.at,
                format!(
                    "'{}' runs around spawned sessions, but the harness declares no spawn tool",
                    hook.kind.describe()
                ),
            );
        }
        hooks.insert(hook.kind, hook.clone());
    }
    Ok(hooks)
}

fn params(
    program: &ir::Program,
    globals: &Globals,
    declared: &[ast::Param],
) -> Checked<Vec<ir::Field>> {
    let mut params = Vec::new();
    let mut seen = BTreeSet::new();
    for param in declared {
        if !seen.insert(param.name.text.clone()) {
            return error(
                param.name.at,
                format!("parameter '{}' appears twice", param.name.text),
            );
        }
        let ty = resolve(program, globals, &param.ty)?;
        no_blob(
            program,
            param.ty.at(),
            &ty,
            &format!("parameter '{}'", param.name.text),
        )?;
        params.push(ir::Field {
            name: param.name.text.clone(),
            ty,
        });
    }
    Ok(params)
}

fn defaults(
    program: &ir::Program,
    globals: &Globals,
    params: &[ir::Field],
    declared: &[Option<ast::Expr>],
) -> Checked<Vec<Option<ir::Literal>>> {
    assert_eq!(
        params.len(),
        declared.len(),
        "the parser gives every tool parameter a default slot"
    );
    params
        .iter()
        .zip(declared)
        .map(|(param, default)| {
            default
                .as_ref()
                .map(|default| default_value(program, globals, param, default))
                .transpose()
        })
        .collect()
}

fn default_value(
    program: &ir::Program,
    globals: &Globals,
    param: &ir::Field,
    default: &ast::Expr,
) -> Checked<ir::Literal> {
    let literal = match (&param.ty, default) {
        (Type::U64, ast::Expr::Number(value, _)) => Some(ir::Literal::U64(*value)),
        (Type::U64, ast::Expr::Name(name)) => globals
            .constants
            .get(&name.text)
            .map(|id| ir::Literal::U64(program.constants[*id].value)),
        (Type::Bool, ast::Expr::Bool(value, _)) => Some(ir::Literal::Bool(*value)),
        (Type::Text, ast::Expr::Text(value, _)) => Some(ir::Literal::Text(value.clone())),
        (ty, ast::Expr::Text(value, _)) => program
            .text_enum(ty)
            .filter(|sum| sum.variants.iter().any(|variant| variant.name == *value))
            .map(|_| ir::Literal::Text(value.clone())),
        _ => None,
    };
    literal.ok_or_else(|| {
        Diagnostic::new(
            default.at(),
            format!(
                "the default of parameter '{}' must be a literal of {} or, for u64, a named constant",
                param.name,
                param.ty.name(program)
            ),
        )
    })
}

fn listings(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    let variants = variant_names(program);
    for item in &source.items {
        let Item::Listing(listing) = item else {
            continue;
        };
        let name = &listing.name;
        claim(values, name, "listing")?;
        if variants.contains(&name.text) {
            return error(
                name.at,
                format!("listing '{}' has the name of a variant", name.text),
            );
        }
        if name.text.contains(ir::LISTED_TOOL_SEPARATOR) || name.text.ends_with(WILDCARD) {
            return error(
                name.at,
                format!(
                    "listing '{}' names its tools '{}{}NAME', so its name holds no '{}' and does not end with '_'",
                    name.text,
                    name.text,
                    ir::LISTED_TOOL_SEPARATOR,
                    ir::LISTED_TOOL_SEPARATOR
                ),
            );
        }
        let bound = bound(program, globals, &listing.bound, "a listing's bound")?;
        let params = params(program, globals, &listing.params)?;
        let defaults = defaults(program, globals, &params, &listing.defaults)?;
        globals
            .listings
            .insert(name.text.clone(), program.listings.len());
        program.listings.push(ir::Listing {
            name: name.text.clone(),
            bound,
            params,
            defaults,
            effect: listing.effect,
        });
    }
    for item in &source.items {
        let Item::Tool(tool) = item else { continue };
        finds(program, globals, tool)?;
        if let Some((id, _)) = program.listed(&tool.name.text) {
            return error(
                tool.name.at,
                format!(
                    "tool '{}' is named like a tool of listing '{}'",
                    tool.name.text, program.listings[id].name
                ),
            );
        }
    }
    Ok(())
}

fn finds(program: &mut ir::Program, globals: &Globals, tool: &ast::Tool) -> Checked<()> {
    let Some(name) = &tool.finds else {
        return Ok(());
    };
    let Some(&listing) = globals.listings.get(&name.text) else {
        return error(
            name.at,
            format!(
                "tool '{}' finds tools of '{}', which is not a listing",
                tool.name.text, name.text
            ),
        );
    };
    let id = globals.tools[&tool.name.text];
    if program.tools[id].effect == crate::effect::EffectClass::External {
        return error(
            tool.name.at,
            format!(
                "tool '{}' finds tools of listing '{}': a search changes nothing, so its effect is none or idempotent",
                tool.name.text, name.text
            ),
        );
    }
    if program.tools[id].returns != Type::Text {
        return error(
            tool.returns.at(),
            format!(
                "tool '{}' finds tools of listing '{}', so it returns text: the names it found, one per line",
                tool.name.text, name.text
            ),
        );
    }
    program.tools[id].finds = Some(listing);
    Ok(())
}

pub const CODE_FIELDS: [&str; 6] = [
    "calls",
    "parallel",
    "timers",
    "timer_ms",
    "journal_bytes",
    "yield_ms",
];

fn codes(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    let variants = variant_names(program);
    let declared: Vec<&ast::Code> = source
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Code(code) => Some(code),
            _ => None,
        })
        .collect();
    for (id, code) in declared.iter().enumerate() {
        claim(values, &code.name, "code tool")?;
        if variants.contains(&code.name.text) {
            return error(
                code.name.at,
                format!("code tool '{}' has the name of a variant", code.name.text),
            );
        }
        globals.codes.insert(code.name.text.clone(), id);
    }
    let mut waits: BTreeMap<String, Position> = BTreeMap::new();
    for code in declared {
        let checked = code_tool(program, globals, code)?;
        if let Some(name) = code
            .fields
            .iter()
            .find(|field| field.key.text == WAITS_FIELD)
        {
            wait_tool(globals, values, &mut waits, &name.value)?;
        }
        program.codes.push(checked);
    }
    Ok(())
}

pub const WAITS_FIELD: &str = "waits";

fn wait_tool(
    globals: &Globals,
    values: &mut BTreeMap<String, Position>,
    waits: &mut BTreeMap<String, Position>,
    name: &ast::Name,
) -> Checked<()> {
    if let Some(previous) = waits.get(&name.text) {
        return error(
            name.at,
            format!(
                "wait tool '{}' is already declared at {previous}",
                name.text
            ),
        );
    }
    if globals.effect(&name.text) {
        return error(
            name.at,
            format!(
                "wait tool '{}' has the name of another tool, code tool, listing or model",
                name.text
            ),
        );
    }
    if name.text != ir::WAIT_TOOL {
        claim(values, name, "wait tool")?;
    }
    waits.insert(name.text.clone(), name.at);
    Ok(())
}

fn code_tool(program: &ir::Program, globals: &Globals, code: &ast::Code) -> Checked<ir::Code> {
    let name = &code.name.text;
    if program.model.is_none() {
        return error(
            code.name.at,
            format!("code '{name}' needs a model declaration: its cells are written by the model"),
        );
    }
    let tools = nested_tools(globals, code)?;
    if let Some((tool, at)) = tools.iter().zip(&code.tools).find(|(tool, _)| {
        matches!(program.tools[**tool].returns, Type::Blob(_))
            || program.tools[**tool].attaches.is_some()
    }) {
        return error(
            at.at,
            format!(
                "code '{name}' lists '{}', which returns a blob: a cell's results are text",
                program.tools[*tool].name
            ),
        );
    }
    let mut limits: BTreeMap<&str, Bound> = BTreeMap::new();
    let mut waits = None;
    for field in &code.fields {
        if field.key.text == WAITS_FIELD {
            if waits.is_some() {
                return error(
                    field.key.at,
                    format!("code '{name}' gives '{WAITS_FIELD}' twice"),
                );
            }
            waits = Some(ir::Waits::new(field.value.text.clone()));
            continue;
        }
        let Some(key) = CODE_FIELDS.iter().find(|key| **key == field.key.text) else {
            return error(
                field.key.at,
                format!(
                    "code '{name}' has no field '{}'; its fields are tools, {}, {WAITS_FIELD}",
                    field.key.text,
                    CODE_FIELDS.join(", ")
                ),
            );
        };
        let what = format!("field '{key}' of code '{name}'");
        if limits
            .insert(key, bound(program, globals, &field.value, &what)?)
            .is_some()
        {
            return error(field.key.at, format!("code '{name}' gives '{key}' twice"));
        }
    }
    let mut take = |key: &str| match limits.remove(key) {
        Some(limit) => Ok(limit),
        None => error(code.name.at, format!("code '{name}' lacks '{key}'")),
    };
    let checked = ir::Code {
        name: name.clone(),
        tools,
        calls: take(CODE_FIELDS[0])?,
        parallel: take(CODE_FIELDS[1])?,
        timers: take(CODE_FIELDS[2])?,
        timer_ms: take(CODE_FIELDS[3])?,
        journal_bytes: take(CODE_FIELDS[4])?,
        yield_ms: take(CODE_FIELDS[5])?,
        waits,
    };
    code_bounds(code, &checked)?;
    Ok(checked)
}

fn nested_tools(globals: &Globals, code: &ast::Code) -> Checked<Vec<ir::ToolId>> {
    let name = &code.name.text;
    if code.tools.is_empty() {
        return error(
            code.name.at,
            format!("code '{name}' lists no tools: a cell's every effect is a nested call"),
        );
    }
    let mut tools = Vec::new();
    for tool in &code.tools {
        if globals.codes.contains_key(&tool.text) {
            return error(
                tool.at,
                format!(
                    "code '{name}' lists code tool '{}': cells call declared tools, not cells",
                    tool.text
                ),
            );
        }
        let Some(&id) = globals.tools.get(&tool.text) else {
            return error(
                tool.at,
                format!(
                    "code '{name}' lists '{}', which is not a declared tool",
                    tool.text
                ),
            );
        };
        if tools.contains(&id) {
            return error(
                tool.at,
                format!("code '{name}' lists tool '{}' twice", tool.text),
            );
        }
        tools.push(id);
    }
    Ok(tools)
}

fn code_bounds(code: &ast::Code, checked: &ir::Code) -> Checked<()> {
    let name = &code.name.text;
    if checked.parallel.value > checked.calls.value {
        return error(
            code.name.at,
            format!(
                "code '{name}': parallel ({} = {}) exceeds calls ({} = {})",
                checked.parallel.name,
                checked.parallel.value,
                checked.calls.name,
                checked.calls.value
            ),
        );
    }
    if checked.parallel.value == 0 {
        return error(
            code.name.at,
            format!(
                "code '{name}': parallel ({} = 0) lets no nested call start",
                checked.parallel.name
            ),
        );
    }
    Ok(())
}

fn state(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &mut Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    let mut seen_state = None;
    for item in &source.items {
        let Item::State(state) = item else { continue };
        if let Some(previous) = seen_state {
            return error(
                state.at,
                format!("a harness has one state block; one is already declared at {previous}"),
            );
        }
        seen_state = Some(state.at);
        for field in &state.fields {
            claim(values, &field.name, "state field")?;
            let ty = resolve(program, globals, &field.ty)?;
            if crate::run::value::Value::zero(program, &ty).is_none() {
                return error(
                    field.ty.at(),
                    format!(
                        "state field '{}' has no initial value: state fields are u64, bool, text, text enums or lists",
                        field.name.text
                    ),
                );
            }
            globals
                .fields
                .insert(field.name.text.clone(), program.state.len());
            program.state.push(ir::Field {
                name: field.name.text.clone(),
                ty,
            });
        }
    }
    let starts = source
        .items
        .iter()
        .any(|item| matches!(item, Item::Hook(hook) if hook.kind == ast::HookKind::SessionStart));
    let ends = source
        .items
        .iter()
        .any(|item| matches!(item, Item::Handler(handler) if handler.ends_session));
    let idles = source
        .items
        .iter()
        .any(|item| matches!(item, Item::Handler(handler) if handler.idle.is_some()));
    let decides = source.items.iter().any(|item| {
        matches!(item, Item::Hook(hook) if matches!(hook.kind, ast::HookKind::BeforeTool | ast::HookKind::AfterTool))
    });
    for (hidden, ty, wanted) in [
        (ir::SESSION_STARTED_FIELD, Type::Bool, starts),
        (ir::SESSION_ENDED_FIELD, Type::Bool, ends),
        (ir::HOOK_DECIDED_FIELD, Type::Bool, decides),
        (ir::HOOK_DENIED_FIELD, Type::Bool, decides),
        (ir::HOOK_REASON_FIELD, Type::Text, decides),
        (ir::HOOK_INPUT_FIELD, Type::Text, decides),
        (ir::IDLE_RUNS_FIELD, Type::U64, idles),
    ] {
        if !wanted {
            continue;
        }
        globals
            .fields
            .insert(hidden.to_owned(), program.state.len());
        program.state.push(ir::Field {
            name: hidden.to_owned(),
            ty,
        });
    }
    Ok(())
}

fn idle_handler<'s>(
    source: &'s ast::Program,
    program: &ir::Program,
    globals: &Globals,
) -> Checked<Option<(&'s ast::Handler, ir::FieldId, Bound)>> {
    let mut found: Option<(&ast::Handler, ir::FieldId, Bound)> = None;
    for item in &source.items {
        let Item::Handler(handler) = item else {
            continue;
        };
        let Some(clause) = &handler.idle else {
            continue;
        };
        if let Some((previous, _, _)) = &found {
            return error(
                handler.name.at,
                format!(
                    "a harness has one idle handler; '{}' is already declared at {}",
                    previous.name.text, previous.name.at
                ),
            );
        }
        if !handler.params.is_empty() {
            return error(
                handler.name.at,
                format!(
                    "idle handler '{}' takes no parameters: the host admits it with none",
                    handler.name.text
                ),
            );
        }
        let due = globals
            .fields
            .get(&clause.due.text)
            .copied()
            .filter(|field| program.state[*field].ty == Type::U64);
        let Some(due) = due else {
            return error(
                clause.due.at,
                format!(
                    "an idle handler's due time is a u64 state field: '{}' is not one",
                    clause.due.text
                ),
            );
        };
        let limit = bound(
            program,
            globals,
            &clause.bound,
            "the idle turns after each other turn",
        )?;
        found = Some((handler, due, limit));
    }
    Ok(found)
}

fn idle(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &Globals,
) -> Checked<Option<ast::Program>> {
    let Some((idler, due, limit)) = idle_handler(source, program, globals)? else {
        return Ok(None);
    };
    let at = idler.name.at;
    let named = |text: &str| ast::Name {
        text: text.to_owned(),
        at,
    };
    let name = |text: &str| ast::Expr::Name(named(text));
    let binary = |op, left, right| ast::Expr::Binary {
        at,
        op,
        left: Box::new(left),
        right: Box::new(right),
    };
    let bound_name = idler
        .idle
        .as_ref()
        .expect("the idle handler has its clause")
        .bound
        .clone();
    let room = binary(
        ast::BinaryOp::Less,
        name(ir::IDLE_RUNS_FIELD),
        ast::Expr::Name(bound_name.clone()),
    );
    let idle_name = idler.name.text.clone();
    let mut items: Vec<Item> = source
        .items
        .iter()
        .map(|item| match item {
            Item::Handler(handler) if handler.name.text == idle_name => {
                let mut handler = handler.clone();
                handler.requires = Some(match handler.requires.take() {
                    Some(requires) => binary(ast::BinaryOp::And, requires, room.clone()),
                    None => room.clone(),
                });
                let prologue = [
                    ast::Stmt::Assign {
                        name: named(ir::IDLE_RUNS_FIELD),
                        value: binary(
                            ast::BinaryOp::Add,
                            name(ir::IDLE_RUNS_FIELD),
                            ast::Expr::Number(1, at),
                        ),
                    },
                    ast::Stmt::Assign {
                        name: handler
                            .idle
                            .as_ref()
                            .expect("the idle handler has its clause")
                            .due
                            .clone(),
                        value: ast::Expr::Number(0, at),
                    },
                ];
                handler.body.statements.splice(0..0, prologue);
                Item::Handler(handler)
            }
            Item::Handler(handler) => {
                let mut handler = handler.clone();
                handler.body.statements.insert(
                    0,
                    ast::Stmt::Assign {
                        name: named(ir::IDLE_RUNS_FIELD),
                        value: ast::Expr::Number(0, at),
                    },
                );
                Item::Handler(handler)
            }
            other => other.clone(),
        })
        .collect();
    items.push(Item::Invariant(ast::Invariant {
        name: named(ir::IDLE_BOUNDED_INVARIANT),
        condition: binary(
            ast::BinaryOp::LessEqual,
            name(ir::IDLE_RUNS_FIELD),
            ast::Expr::Name(bound_name),
        ),
    }));
    program.idle = Some(ir::Idle {
        handler: idle_name,
        due,
        bound: limit,
        runs: globals.fields[ir::IDLE_RUNS_FIELD],
    });
    Ok(Some(ast::Program {
        name: source.name.clone(),
        items,
    }))
}

fn session_end(source: &ast::Program) -> Option<ast::Program> {
    let Some(ending) = source.items.iter().find_map(|item| match item {
        Item::Handler(handler) if handler.ends_session => Some(handler),
        _ => None,
    }) else {
        return None;
    };
    let at = ending.name.at;
    let ended = ast::Name {
        text: ir::SESSION_ENDED_FIELD.to_owned(),
        at,
    };
    let open = ast::Expr::Not {
        at,
        operand: Box::new(ast::Expr::Name(ended.clone())),
    };
    let items = source
        .items
        .iter()
        .map(|item| match item {
            Item::Handler(handler) => {
                let mut handler = handler.clone();
                handler.requires = Some(match handler.requires.take() {
                    Some(requires) => ast::Expr::Binary {
                        at,
                        op: ast::BinaryOp::And,
                        left: Box::new(requires),
                        right: Box::new(open.clone()),
                    },
                    None => open.clone(),
                });
                if handler.ends_session {
                    handler.body.statements.insert(
                        0,
                        ast::Stmt::Assign {
                            name: ended.clone(),
                            value: ast::Expr::Bool(true, at),
                        },
                    );
                }
                Item::Handler(handler)
            }
            other => other.clone(),
        })
        .collect();
    Some(ast::Program {
        name: source.name.clone(),
        items,
    })
}

fn events(
    source: &ast::Program,
    program: &mut ir::Program,
    globals: &Globals,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    for item in &source.items {
        let Item::Event(event) = item else { continue };
        match event.name.text.as_str() {
            EVENT_CANCEL => cancel_event(event, program, values)?,
            EVENT_STEER => steer_event(event, program, globals)?,
            EVENT_JOB => job_event(event, program, globals)?,
            EVENT_RESUME => resume_event(event, program)?,
            other => {
                return error(
                    event.name.at,
                    format!(
                        "unknown event '{other}': a harness takes '{EVENT_CANCEL}', '{EVENT_STEER}', '{EVENT_JOB}' and '{EVENT_RESUME}'"
                    ),
                );
            }
        }
    }
    if program.events.cancel && variant_names(program).contains(CANCELLED_READ) {
        let at = source
            .items
            .iter()
            .find_map(|item| match item {
                Item::Type(definition) => match &definition.body {
                    TypeBody::Variants(variants) => variants
                        .iter()
                        .find(|variant| variant.name.text == CANCELLED_READ)
                        .map(|variant| variant.name.at),
                    TypeBody::Values(_) => None,
                },
                _ => None,
            })
            .expect("a variant name comes from a type definition");
        return error(
            at,
            format!("variant '{CANCELLED_READ}' hides the cancel event's read: rename it"),
        );
    }
    Ok(())
}

fn cancel_event(
    event: &ast::Event,
    program: &mut ir::Program,
    values: &mut BTreeMap<String, Position>,
) -> Checked<()> {
    if !event.params.is_empty() || event.bound.is_some() {
        return error(
            event.name.at,
            format!("event '{EVENT_CANCEL}' carries nothing: write 'on {EVENT_CANCEL}'"),
        );
    }
    if program.events.cancel {
        return error(
            event.name.at,
            format!("event '{EVENT_CANCEL}' is already declared"),
        );
    }
    let read = ast::Name {
        text: CANCELLED_READ.to_owned(),
        at: event.name.at,
    };
    claim(values, &read, "the cancel event's read")?;
    program.events.cancel = true;
    Ok(())
}

fn resume_event(event: &ast::Event, program: &mut ir::Program) -> Checked<()> {
    if !event.params.is_empty() || event.bound.is_some() {
        return error(
            event.name.at,
            format!(
                "event '{EVENT_RESUME}' carries the key of a suspend and a text answer: write 'on {EVENT_RESUME}'"
            ),
        );
    }
    if program.events.resume {
        return error(
            event.name.at,
            format!("event '{EVENT_RESUME}' is already declared"),
        );
    }
    program.events.resume = true;
    Ok(())
}

fn steer_event(event: &ast::Event, program: &mut ir::Program, globals: &Globals) -> Checked<()> {
    let shape = format!("on {EVENT_STEER}({STEER_PARAMETER}: text) bound N");
    let one_text = match event.params.as_slice() {
        [param] => {
            param.name.text == STEER_PARAMETER
                && matches!(&param.ty, TypeExpr::Named(name) if name.text == "text")
        }
        _ => false,
    };
    let Some(limit) = event.bound.as_ref().filter(|_| one_text) else {
        return error(
            event.name.at,
            format!("event '{EVENT_STEER}' carries one text: write '{shape}'"),
        );
    };
    if program.events.steers.is_some() {
        return error(
            event.name.at,
            format!("event '{EVENT_STEER}' is already declared"),
        );
    }
    let bound = bound(program, globals, limit, "the steers one turn records")?;
    program.events.steers = Some(bound);
    Ok(())
}

fn job_event(event: &ast::Event, program: &mut ir::Program, globals: &Globals) -> Checked<()> {
    let shape = format!("on {EVENT_JOB}({JOB_PARAMETER}: text) bound N");
    let one_text = match event.params.as_slice() {
        [param] => {
            param.name.text == JOB_PARAMETER
                && matches!(&param.ty, TypeExpr::Named(name) if name.text == "text")
        }
        _ => false,
    };
    let Some(limit) = event.bound.as_ref().filter(|_| one_text) else {
        return error(
            event.name.at,
            format!("event '{EVENT_JOB}' carries one text: write '{shape}'"),
        );
    };
    if program.events.jobs.is_some() {
        return error(
            event.name.at,
            format!("event '{EVENT_JOB}' is already declared"),
        );
    }
    let bound = bound(program, globals, limit, "the job notices one turn records")?;
    program.events.jobs = Some(bound);
    Ok(())
}

fn invariants(source: &ast::Program, program: &mut ir::Program, globals: &Globals) -> Checked<()> {
    let mut names: BTreeMap<String, Position> = BTreeMap::new();
    for item in &source.items {
        let Item::Invariant(definition) = item else {
            continue;
        };
        if let Some(previous) = names.insert(definition.name.text.clone(), definition.name.at) {
            return error(
                definition.name.at,
                format!(
                    "invariant '{}' is already declared at {previous}",
                    definition.name.text
                ),
            );
        }
        let checked = handler::invariant(program, globals, definition)?;
        program.invariants.push(checked);
    }
    Ok(())
}

fn handlers(source: &ast::Program, program: &mut ir::Program, globals: &Globals) -> Checked<()> {
    let mut used = BTreeSet::new();
    let mut names: BTreeMap<String, Position> = BTreeMap::new();
    for item in &source.items {
        let Item::Handler(definition) = item else {
            continue;
        };
        if let Some(previous) = names.insert(definition.name.text.clone(), definition.name.at) {
            return error(
                definition.name.at,
                format!(
                    "handler '{}' is already declared at {previous}",
                    definition.name.text
                ),
            );
        }
        let (checked, ran) = handler::check(program, globals, definition)?;
        if definition.side {
            assert!(ran.is_empty(), "no hook runs in a side handler");
            program.sides.push(checked);
            continue;
        }
        used.extend(ran);
        program.handlers.push(checked);
    }
    if let Some(hook) = globals
        .hooks
        .values()
        .find(|hook| !used.contains(&hook.kind))
    {
        let reason = match hook.kind {
            ast::HookKind::BeforeTool | ast::HookKind::AfterTool => {
                "no handler dispatches a call".to_owned()
            }
            ast::HookKind::PromptSubmit => format!(
                "no handler takes '{}'",
                hook.params
                    .first()
                    .map_or("", |param| param.name.text.as_str())
            ),
            ast::HookKind::PreCompact | ast::HookKind::PostCompact => {
                "no handler keeps or clears a list of entries".to_owned()
            }
            ast::HookKind::SubagentStart => "no handler matches on a spawn's result".to_owned(),
            ast::HookKind::SubagentStop => "no handler joins a spawned session".to_owned(),
            ast::HookKind::SessionStart | ast::HookKind::Stop => "no handler runs it".to_owned(),
        };
        return error(
            hook.at,
            format!("'{}' runs nowhere: {reason}", hook.kind.describe()),
        );
    }
    if program.handlers.is_empty() {
        return error(
            source.name.at,
            format!("harness '{}' has no handler", source.name.text),
        );
    }
    Ok(())
}
