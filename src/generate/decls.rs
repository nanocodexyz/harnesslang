use crate::effect::EffectClass;
use crate::generate::names::fresh;
use crate::generate::{
    BUILTIN_SUMS, CALL, Decls, ENTRY, REPLY, SumDef, ToolDef, Ty, chance, weighted,
};
use crate::sim::rng::Rng;

pub const HANDLERS_MAX: u64 = 3;
const LOOP_BOUNDS_MAX: u64 = 3;
const LOOP_BOUND_VALUE_MAX: u64 = 3;
const CAPACITIES_MAX: u64 = 3;
const CAPACITY_VALUES: [u64; 8] = [0, 1, 2, 3, 8, 16, 64, 4096];
const MODEL_CALLS_VALUES: [u64; 5] = [0, 1, 2, 3, 3];
const ARITHMETIC_CONSTANTS_MAX: u64 = 2;
const SUMS_MAX: u64 = 3;
const VARIANTS_MAX: u64 = 3;
const FIELDS_PER_VARIANT_MAX: u64 = 3;
const TOOLS_MAX: u64 = 4;
const PARAMS_PER_TOOL_MAX: u64 = 3;
const STATE_FIELDS_MAX: u64 = 5;
const MODEL_PERCENT: u64 = 80;
const PROVABLE_PERCENT: u64 = 40;
const UNDERSCORE_NUMBER_PERCENT: u64 = 20;
const ARITHMETIC_VALUES: [u64; 6] = [0, 1, 7, 1 << 32, u64::MAX - 1, u64::MAX];
const WAIT_PERCENT: u64 = 30;
const MILLISECONDS_PER_SECOND: u64 = 1000;
const WAIT_MS_VALUES: [u64; 4] = [
    0,
    1,
    MILLISECONDS_PER_SECOND,
    crate::target::VERCEL_FUNCTION_DURATION_SECONDS_MAX * MILLISECONDS_PER_SECOND,
];
const CUT_PERCENT: u64 = 30;
const CUT_TOKENS_VALUES: [u64; 5] = [0, 1, 2, 10, 25000];
const DEFAULT_PERCENT: u64 = 30;
const DISPATCH_HOOKS_PERCENT: u64 = 25;

#[derive(Clone, Copy)]
enum Shape {
    U64,
    Bool,
    Text,
    UserSum,
    Call,
    Known,
    Maybe,
    Entry,
    Reply,
    List,
}

const FIELD_SHAPES: [(Shape, u64); 9] = [
    (Shape::U64, 4),
    (Shape::Bool, 2),
    (Shape::Text, 3),
    (Shape::UserSum, 3),
    (Shape::Call, 1),
    (Shape::Known, 1),
    (Shape::Maybe, 1),
    (Shape::Entry, 1),
    (Shape::List, 1),
];

const STATE_SHAPES: [(Shape, u64); 4] = [
    (Shape::U64, 4),
    (Shape::Bool, 2),
    (Shape::Text, 3),
    (Shape::List, 4),
];

const ELEMENT_SHAPES: [(Shape, u64); 7] = [
    (Shape::U64, 3),
    (Shape::Bool, 1),
    (Shape::Text, 3),
    (Shape::UserSum, 2),
    (Shape::Call, 1),
    (Shape::Entry, 2),
    (Shape::Reply, 1),
];

pub fn scalar(rng: &mut Rng) -> Ty {
    [Ty::U64, Ty::Bool, Ty::Text][rng.pick(3)].clone()
}

fn shaped(rng: &mut Rng, decls: &Decls, shape: Shape, sums_below: usize) -> Ty {
    let user_sums = sums_below.saturating_sub(BUILTIN_SUMS);
    match shape {
        Shape::U64 => Ty::U64,
        Shape::Bool => Ty::Bool,
        Shape::Text => Ty::Text,
        Shape::UserSum if user_sums > 0 => Ty::Sum(BUILTIN_SUMS + rng.pick(user_sums)),
        Shape::Call => Ty::Sum(CALL),
        Shape::Known => Ty::Known(Box::new(scalar(rng))),
        Shape::Maybe => Ty::Maybe(Box::new(scalar(rng))),
        Shape::Entry if decls.has_model() => Ty::Sum(ENTRY),
        Shape::Reply if decls.has_model() => Ty::Sum(REPLY),
        Shape::List if !decls.capacities.is_empty() => {
            let shape = weighted(rng, &ELEMENT_SHAPES);
            let element = shaped(rng, decls, shape, sums_below);
            let capacity = decls.capacities[rng.pick(decls.capacities.len())];
            Ty::List(Box::new(element), capacity)
        }
        Shape::UserSum | Shape::Entry | Shape::Reply | Shape::List => scalar(rng),
    }
}

pub fn field_type(rng: &mut Rng, decls: &Decls, sums_below: usize) -> Ty {
    let shape = weighted(rng, &FIELD_SHAPES);
    shaped(rng, decls, shape, sums_below)
}

pub fn generate(rng: &mut Rng) -> Decls {
    let mut decls = Decls {
        provable: chance(rng, PROVABLE_PERCENT),
        ..Decls::default()
    };
    constants(rng, &mut decls);
    if chance(rng, MODEL_PERCENT) {
        decls.model = Some(fresh(rng, &mut decls.names, "m".to_owned()));
    }
    builtin_sums(&mut decls);
    let sums = rng.below(SUMS_MAX + 1);
    for index in 0..sums {
        let below = decls.sums.len();
        let variants = rng.range(1, VARIANTS_MAX);
        let variants = (0..variants)
            .map(|variant| {
                let fields = rng.below(FIELDS_PER_VARIANT_MAX + 1);
                let fields = (0..fields)
                    .map(|_| field_type(rng, &decls, below))
                    .collect();
                (
                    fresh(rng, &mut decls.names, format!("s{index}v{variant}")),
                    fields,
                )
            })
            .collect();
        let name = fresh(rng, &mut decls.names, format!("s{index}"));
        decls.sums.push(SumDef { name, variants });
    }
    tools(rng, &mut decls);
    fields(rng, &mut decls);
    decls.dispatch_hooks = chance(rng, DISPATCH_HOOKS_PERCENT);
    decls
}

fn constant(rng: &mut Rng, decls: &mut Decls, name: String, value: u64) -> usize {
    let name = fresh(rng, &mut decls.names, name);
    decls.constants.push((name, value));
    decls.constants.len() - 1
}

fn constants(rng: &mut Rng, decls: &mut Decls) {
    for index in 0..rng.range(1, LOOP_BOUNDS_MAX) {
        let value = rng.below(LOOP_BOUND_VALUE_MAX + 1);
        let id = constant(rng, decls, format!("steps{index}"), value);
        decls.loop_bounds.push(id);
    }
    for index in 0..rng.range(1, CAPACITIES_MAX) {
        let value = CAPACITY_VALUES[rng.pick(CAPACITY_VALUES.len())];
        let id = constant(rng, decls, format!("cap{index}"), value);
        decls.capacities.push(id);
    }
    let value = MODEL_CALLS_VALUES[rng.pick(MODEL_CALLS_VALUES.len())];
    let calls = constant(rng, decls, "calls_max".to_owned(), value);
    decls.calls_bound = Some(calls);
    for index in 0..rng.below(ARITHMETIC_CONSTANTS_MAX + 1) {
        let value = ARITHMETIC_VALUES[rng.pick(ARITHMETIC_VALUES.len())];
        constant(rng, decls, format!("k{index}"), value);
    }
    if chance(rng, WAIT_PERCENT) {
        let value = WAIT_MS_VALUES[rng.pick(WAIT_MS_VALUES.len())];
        decls.wait_ms = Some(constant(rng, decls, "wait_ms".to_owned(), value));
    }
    if chance(rng, CUT_PERCENT) {
        let value = CUT_TOKENS_VALUES[rng.pick(CUT_TOKENS_VALUES.len())];
        decls.cut_tokens = Some(constant(rng, decls, "tokens_max".to_owned(), value));
    }
}

fn builtin_sums(decls: &mut Decls) {
    let call = Ty::Sum(CALL);
    let calls = decls.calls_bound.expect("constants come first");
    let sum = |name: &str, variants: Vec<(&str, Vec<Ty>)>| SumDef {
        name: name.to_owned(),
        variants: variants
            .into_iter()
            .map(|(variant, fields)| (variant.to_owned(), fields))
            .collect(),
    };
    decls.sums.push(sum(
        "call",
        vec![("call", vec![Ty::Text, Ty::Text, Ty::Text])],
    ));
    decls.sums.push(sum(
        "reply",
        vec![
            ("answer", vec![Ty::Text]),
            ("calls", vec![Ty::List(Box::new(call.clone()), calls)]),
        ],
    ));
    decls.sums.push(sum(
        "entry",
        vec![
            ("user", vec![Ty::Text]),
            ("assistant", vec![Ty::Sum(REPLY)]),
            ("result", vec![call.clone(), Ty::Text]),
            ("error", vec![call.clone(), Ty::Text]),
            ("unknown", vec![call]),
        ],
    ));
}

fn tools(rng: &mut Rng, decls: &mut Decls) {
    for index in 0..rng.below(TOOLS_MAX + 1) {
        let params = rng.below(PARAMS_PER_TOOL_MAX + 1);
        let params: Vec<Ty> = (0..params)
            .map(|_| field_type(rng, decls, decls.sums.len()))
            .collect();
        let name = fresh(rng, &mut decls.names, format!("t{index}"));
        let defaults = default_values(rng, decls, &params);
        decls.defaults.push(defaults);
        let tool = ToolDef {
            name,
            params,
            returns: scalar(rng),
            effect: EffectClass::ALL[rng.pick(EffectClass::ALL.len())],
        };
        decls.tools.push(tool);
    }
}

fn default_values(rng: &mut Rng, decls: &Decls, params: &[Ty]) -> Vec<Option<String>> {
    let mut defaults = vec![None; params.len()];
    for index in 0..params.len() {
        if !chance(rng, DEFAULT_PERCENT) {
            continue;
        }
        defaults[index] = match &params[index] {
            Ty::U64 => Some(decls.constants[rng.pick(decls.constants.len())].0.clone()),
            Ty::Bool => Some(["true", "false"][rng.pick(2)].to_owned()),
            Ty::Text => {
                let literal = crate::generate::expr::TEXT_LITERALS
                    [rng.pick(crate::generate::expr::TEXT_LITERALS.len())];
                Some(crate::generate::expr::text_literal(literal))
            }
            _ => None,
        };
    }
    defaults
}

fn fields(rng: &mut Rng, decls: &mut Decls) {
    if decls.has_model() {
        let capacity = decls.capacities[rng.pick(decls.capacities.len())];
        let context = Ty::List(Box::new(Ty::Sum(ENTRY)), capacity);
        let name = fresh(rng, &mut decls.names, "context".to_owned());
        decls.fields.push((name, context));
    }
    for index in 0..rng.range(1, STATE_FIELDS_MAX) {
        let shape = weighted(rng, &STATE_SHAPES);
        let ty = shaped(rng, decls, shape, decls.sums.len());
        let name = fresh(rng, &mut decls.names, format!("f{index}"));
        decls.fields.push((name, ty));
    }
}

fn number(rng: &mut Rng, value: u64) -> String {
    let digits = value.to_string();
    if !chance(rng, UNDERSCORE_NUMBER_PERCENT) || digits.len() < 2 {
        return digits;
    }
    let split = 1 + rng.pick(digits.len() - 1);
    format!("{}_{}", &digits[..split], &digits[split..])
}

pub fn render(rng: &mut Rng, decls: &Decls) -> Vec<String> {
    let mut items: Vec<Vec<String>> = Vec::new();
    for (name, value) in &decls.constants {
        items.push(vec![format!("const {name}: u64 = {}", number(rng, *value))]);
    }
    for sum in &decls.sums[BUILTIN_SUMS..] {
        let variants: Vec<String> = sum
            .variants
            .iter()
            .map(|(name, fields)| render_variant(decls, name, fields))
            .collect();
        items.push(vec![format!(
            "type {} = {}",
            sum.name,
            variants.join(" | ")
        )]);
    }
    for (tool, defaults) in decls.tools.iter().zip(&decls.defaults) {
        let params: Vec<String> = tool
            .params
            .iter()
            .zip(defaults)
            .enumerate()
            .map(|(index, (ty, default))| match default {
                Some(value) => format!("a{index}: {} = {value}", decls.render_type(ty)),
                None => format!("a{index}: {}", decls.render_type(ty)),
            })
            .collect();
        items.push(vec![format!(
            "tool {}({}) -> {} effect {}",
            tool.name,
            params.join(", "),
            decls.render_type(&tool.returns),
            tool.effect.name()
        )]);
    }
    if let Some(model) = &decls.model {
        let calls = decls.calls_bound.expect("constants come first");
        items.push(vec![format!(
            "model {model} calls {}",
            decls.constants[calls].0
        )]);
    }
    let mut state = vec!["state {".to_owned()];
    for (name, ty) in &decls.fields {
        state.push(format!("  {name}: {}", decls.render_type(ty)));
    }
    state.push("}".to_owned());
    items.push(state);
    shuffle(rng, &mut items);
    items.into_iter().flatten().collect()
}

fn render_variant(decls: &Decls, name: &str, fields: &[Ty]) -> String {
    if fields.is_empty() {
        return name.to_owned();
    }
    let fields: Vec<String> = fields.iter().map(|ty| decls.render_type(ty)).collect();
    format!("{name}({})", fields.join(", "))
}

pub fn shuffle<T>(rng: &mut Rng, items: &mut [T]) {
    for index in (1..items.len()).rev() {
        let other = rng.pick(index + 1);
        items.swap(index, other);
    }
}
