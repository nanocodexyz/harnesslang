use std::collections::{BTreeMap, BTreeSet};

use super::assemble::{Sexp, number};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Arms {
    Taken,
    Flipped,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Value {
    I32(i32),
    I64(i64),
    Address(String, u32),
}

type Set = BTreeSet<Value>;
type Env = BTreeMap<String, Set>;
type Sets = BTreeMap<(String, usize), Option<Set>>;
type Calls = Vec<(String, usize, Option<Set>)>;
type Returns = BTreeMap<String, Option<Set>>;

const WORD_BYTES: u32 = 4;
const HEADER_FORMS: [&str; 4] = ["export", "param", "result", "local"];

struct Known {
    words: BTreeMap<String, Vec<String>>,
    constants: BTreeMap<String, Value>,
    expressions: BTreeMap<String, (Vec<String>, Sexp)>,
    arities: BTreeMap<String, usize>,
    sets: Sets,
    returns: Returns,
    depth_max: usize,
    arms: Arms,
}

fn atom(word: &str) -> Sexp {
    Sexp::Atom(word.to_owned(), 0)
}

fn list(items: Vec<Sexp>) -> Sexp {
    Sexp::List(items, 0)
}

fn literal(ty: &str, word: &str) -> Option<Value> {
    if word.starts_with('$') {
        return (ty == "i32.const").then(|| Value::Address(word.to_owned(), 0));
    }
    let value = number(word)?;
    match ty {
        "i32.const" => i32::try_from(value)
            .ok()
            .or_else(|| u32::try_from(value).ok().map(|unsigned| unsigned as i32))
            .map(Value::I32),
        "i64.const" => i64::try_from(value)
            .ok()
            .or_else(|| u64::try_from(value).ok().map(|unsigned| unsigned as i64))
            .map(Value::I64),
        _ => None,
    }
}

fn written(value: &Value) -> Option<Vec<Sexp>> {
    match value {
        Value::I32(value) => Some(vec![atom("i32.const"), atom(&value.to_string())]),
        Value::I64(value) => Some(vec![atom("i64.const"), atom(&value.to_string())]),
        Value::Address(name, 0) => Some(vec![atom("i32.const"), atom(name)]),
        Value::Address(..) => None,
    }
}

fn only(set: &Set) -> Option<&Value> {
    if set.len() == 1 { set.first() } else { None }
}

fn constant_of(form: &Sexp) -> Option<Value> {
    let Sexp::List(items, _) = form else {
        return None;
    };
    match items.as_slice() {
        [Sexp::Atom(ty, _), Sexp::Atom(word, _)] => literal(ty, word),
        _ => None,
    }
}

fn body_at(items: &[Sexp]) -> usize {
    let mut at = 2;
    while at < items.len()
        && items[at]
            .head()
            .is_some_and(|head| HEADER_FORMS.contains(&head))
    {
        at += 1;
    }
    at
}

fn header_count(items: &[Sexp], kind: &str) -> usize {
    items[2..body_at(items)]
        .iter()
        .filter(|item| item.head() == Some(kind))
        .count()
}

fn param_names(items: &[Sexp]) -> Vec<Option<String>> {
    items[2..body_at(items)]
        .iter()
        .filter(|item| item.head() == Some("param"))
        .map(|item| match item {
            Sexp::List(param, _) => param
                .get(1)
                .and_then(Sexp::atom)
                .filter(|name| name.starts_with('$'))
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

fn single_expression(items: &[Sexp]) -> Option<(String, Vec<String>, Sexp)> {
    let name = items.get(1)?.atom()?;
    let at = body_at(items);
    let plain = header_count(items, "result") == 1
        && header_count(items, "local") == 0
        && header_count(items, "export") == 0;
    if !plain || at + 1 != items.len() || !matches!(items[at], Sexp::List(..)) {
        return None;
    }
    let params: Option<Vec<String>> = param_names(items).into_iter().collect();
    Some((name.to_owned(), params?, items[at].clone()))
}

fn immutable_global(items: &[Sexp]) -> Option<(String, Value)> {
    let name = items.get(1)?.atom()?;
    if !matches!(items.get(2), Some(Sexp::Atom(..))) || items.len() != 4 {
        return None;
    }
    Some((name.to_owned(), constant_of(&items[3])?))
}

fn import_arity(items: &[Sexp]) -> Option<(String, usize)> {
    let Some(Sexp::List(signature, _)) = items.get(3) else {
        return None;
    };
    let name = signature.get(1).and_then(Sexp::atom)?;
    let count = signature
        .iter()
        .filter(|item| item.head() == Some("param"))
        .count();
    Some((name.to_owned(), count))
}

fn known(forms: &[Sexp], arms: Arms) -> Known {
    let mut known = Known {
        words: BTreeMap::new(),
        constants: BTreeMap::new(),
        expressions: BTreeMap::new(),
        arities: BTreeMap::new(),
        sets: BTreeMap::new(),
        returns: BTreeMap::new(),
        depth_max: forms.len(),
        arms,
    };
    for form in forms {
        let Sexp::List(items, _) = form else {
            continue;
        };
        let name = items
            .get(1)
            .and_then(Sexp::atom)
            .unwrap_or_default()
            .to_owned();
        match form.head() {
            Some("words") => {
                let words = items[2..]
                    .iter()
                    .filter_map(Sexp::atom)
                    .map(str::to_owned)
                    .collect();
                known.words.insert(name, words);
            }
            Some("global") => {
                if let Some((name, value)) = immutable_global(items) {
                    known.constants.insert(name, value);
                }
            }
            Some("func") => {
                known.arities.insert(name, param_names(items).len());
            }
            Some("import") => {
                if let Some((name, count)) = import_arity(items) {
                    known.arities.insert(name, count);
                }
            }
            _ => {}
        }
    }
    known
}

fn learn_expressions(forms: &[Sexp], known: &mut Known) -> bool {
    let before = (known.constants.len(), known.expressions.len());
    for form in forms {
        let Sexp::List(items, _) = form else {
            continue;
        };
        if form.head() != Some("func") {
            continue;
        }
        let Some((name, params, body)) = single_expression(items) else {
            continue;
        };
        if params.is_empty()
            && let Some(value) = constant_of(&body)
        {
            known.constants.entry(name.clone()).or_insert(value);
        }
        known.expressions.insert(name, (params, body));
    }
    before != (known.constants.len(), known.expressions.len())
}

pub fn shake(forms: Vec<Sexp>, arms: Arms) -> Vec<Sexp> {
    let mut known = known(&forms, arms);
    let mut forms = forms;
    learn_expressions(&forms, &mut known);
    let rounds_max = forms.len() + 1;
    for round in 0..=rounds_max {
        assert!(
            round < rounds_max,
            "every folding round learns a new function"
        );
        facts(&forms, &mut known);
        forms = forms
            .into_iter()
            .map(|form| fold_function(form, &known).0)
            .collect();
        if !learn_expressions(&forms, &mut known) {
            break;
        }
    }
    forms
}

fn facts(forms: &[Sexp], known: &mut Known) {
    let mut sets: Sets = BTreeMap::new();
    let mut returns: Returns = BTreeMap::new();
    for items in forms.iter().filter_map(function_items) {
        let name = function_name(items);
        let start = (header_count(items, "export") == 0).then(Set::new);
        for index in 0..param_names(items).len() {
            sets.insert((name.clone(), index), start.clone());
        }
        returns.insert(name, Some(Set::new()));
    }
    let rounds_max = (sets.len() + returns.len()) * (forms.len() + 1) + 1;
    for round in 0..=rounds_max {
        assert!(
            round < rounds_max,
            "every facts round widens a parameter's or a result's set"
        );
        known.sets = sets.clone();
        known.returns = returns.clone();
        for form in forms.iter().filter(|form| function_items(form).is_some()) {
            let name = function_items(form).map(function_name).unwrap_or_default();
            let (_, calls, result) = fold_function(form.clone(), known);
            if let Some(slot) = returns.get_mut(&name) {
                *slot = joined(slot.take(), result);
            }
            for (callee, index, set) in calls {
                if let Some(slot) = sets.get_mut(&(callee, index)) {
                    *slot = joined(slot.take(), set);
                }
            }
        }
        if sets == known.sets && returns == known.returns {
            break;
        }
    }
    known.sets = sets;
    known.returns = returns;
}

struct Walk<'k> {
    known: &'k Known,
    base: Env,
    calls: Calls,
    returns: Option<Set>,
}

impl Walk<'_> {
    fn revert(&self, env: &mut Env, name: &str) {
        match self.base.get(name) {
            Some(set) => env.insert(name.to_owned(), set.clone()),
            None => env.remove(name),
        };
    }

    fn forget(&self, env: &mut Env, form: &[Sexp]) {
        let mut counts = BTreeMap::new();
        assigned(form, &mut counts);
        for name in counts.keys() {
            self.revert(env, name);
        }
    }

    fn returned(&mut self, set: Option<Set>) {
        self.returns = match (self.returns.take(), set) {
            (Some(mut all), Some(set)) => {
                all.extend(set);
                Some(all)
            }
            _ => None,
        };
    }
}

fn base_env(items: &[Sexp], body: &[Sexp], known: &Known) -> Env {
    let name = function_name(items);
    let mut counts = BTreeMap::new();
    assigned(body, &mut counts);
    let mut env = Env::new();
    for (index, param) in param_names(items).into_iter().enumerate() {
        if let Some(param) = param
            && !counts.contains_key(&param)
            && let Some(Some(set)) = known.sets.get(&(name.clone(), index))
            && !set.is_empty()
        {
            env.insert(param, set.clone());
        }
    }
    for (local, zero) in local_zeros(items) {
        if counts.get(&local) != Some(&1) {
            continue;
        }
        if let Some(value) = assignment(body, &local)
            && let Some(mut set) = evaluate(value, &env, known, 0)
        {
            set.insert(zero);
            env.insert(local, set);
        }
    }
    env
}

fn fold_function(form: Sexp, known: &Known) -> (Sexp, Calls, Option<Set>) {
    let Sexp::List(mut items, line) = form else {
        return (form, Vec::new(), None);
    };
    if items.first().and_then(Sexp::atom) != Some("func") {
        return (Sexp::List(items, line), Vec::new(), None);
    }
    let body = items.split_off(body_at(&items));
    let valued = header_count(&items, "result") == 1 && !exits_flat(&body);
    let base = base_env(&items, &body, known);
    let mut walk = Walk {
        known,
        base: base.clone(),
        calls: Vec::new(),
        returns: valued.then(Set::new),
    };
    let mut env = base;
    let folded = fold_sequence(body, &mut env, &mut walk);
    let last = folded.last().cloned();
    match last {
        Some(last @ Sexp::List(..)) if diverges(&last) || last.head() == Some("return") => {}
        Some(last @ Sexp::List(..)) => walk.returned(evaluate(&last, &env, known, 0)),
        _ => walk.returns = None,
    }
    items.extend(folded);
    let returns = if valued { walk.returns } else { None };
    (Sexp::List(items, line), walk.calls, returns)
}

fn exits_flat(items: &[Sexp]) -> bool {
    items.iter().any(|item| match item {
        Sexp::Atom(word, _) => word == "return",
        Sexp::List(inner, _) => exits_flat(inner),
        Sexp::Text(..) => false,
    }) || branches_to(items, None)
}

fn fold_sequence(items: Vec<Sexp>, env: &mut Env, walk: &mut Walk) -> Vec<Sexp> {
    let mut whole = BTreeMap::new();
    assigned(&items, &mut whole);
    let mut out = Vec::with_capacity(items.len());
    let mut stack: Vec<Option<Set>> = Vec::new();
    let mut items = items.into_iter().peekable();
    while let Some(item) = items.next() {
        let Sexp::Atom(ref word, _) = item else {
            let produced = fold_instruction(item, env, walk);
            for form in &produced {
                match evaluate(form, env, walk.known, 0) {
                    Some(set) => stack.push(Some(set)),
                    None => stack.clear(),
                }
                effects(form, env, walk);
            }
            out.extend(produced);
            continue;
        };
        if let Some(constant) = items
            .peek()
            .and_then(|next| replaced(word, next, walk.known))
        {
            items.next();
            stack.push(constant_of(&list(constant.clone())).map(|value| Set::from([value])));
            out.extend(constant);
            continue;
        }
        let word = word.clone();
        out.push(item);
        let operand = items
            .peek()
            .and_then(Sexp::atom)
            .unwrap_or_default()
            .to_owned();
        let consumed = flat(&word, &operand, &mut stack, env, walk);
        if consumed && let Some(next) = items.next() {
            out.push(next);
        }
        if matches!(word.as_str(), "block" | "loop" | "if" | "else" | "end") {
            for name in whole.keys() {
                walk.revert(env, name);
            }
        }
    }
    pruned(out)
}

fn flat(
    word: &str,
    operand: &str,
    stack: &mut Vec<Option<Set>>,
    env: &mut Env,
    walk: &mut Walk,
) -> bool {
    let pushed = match word {
        "i32.const" | "i64.const" => literal(word, operand).map(|value| Set::from([value])),
        "local.get" => env.get(operand).cloned(),
        "global.get" => walk
            .known
            .constants
            .get(operand)
            .map(|value| Set::from([value.clone()])),
        "call" => {
            let arity = walk.known.arities.get(operand).copied().unwrap_or_default();
            for index in (0..arity).rev() {
                walk.calls
                    .push((operand.to_owned(), index, stack.pop().flatten()));
            }
            stack.clear();
            return true;
        }
        "local.set" | "local.tee" => {
            walk.revert(env, operand);
            stack.clear();
            return true;
        }
        "return" => {
            walk.returns = None;
            stack.clear();
            return false;
        }
        _ => {
            stack.clear();
            return false;
        }
    };
    stack.push(pushed);
    true
}

fn effects(form: &Sexp, env: &mut Env, walk: &mut Walk) {
    let Sexp::List(items, _) = form else {
        return;
    };
    if let [Sexp::Atom(word, _), Sexp::Atom(local, _), value] = items.as_slice()
        && word == "local.set"
    {
        walk.forget(env, std::slice::from_ref(value));
        match evaluate(value, env, walk.known, 0) {
            Some(values) => env.insert(local.clone(), values),
            None => {
                walk.revert(env, local);
                None
            }
        };
        return;
    }
    walk.forget(env, items);
    let mut counts = BTreeMap::new();
    assigned(items, &mut counts);
    for local in guarded(form) {
        if !counts.contains_key(&local)
            && let Some(set) = env.get_mut(&local)
        {
            set.retain(|value| !matches!(value, Value::I32(0) | Value::I64(0)));
        }
    }
}

fn fold_instruction(form: Sexp, env: &mut Env, walk: &mut Walk) -> Vec<Sexp> {
    let Sexp::List(mut items, line) = form else {
        return vec![form];
    };
    match items.first().and_then(Sexp::atom) {
        Some(word @ ("block" | "loop")) => {
            let looped = word == "loop";
            let body = items.split_off(block_header(&items));
            let mut inner = env.clone();
            if looped {
                walk.forget(&mut inner, &body);
            }
            items.extend(fold_sequence(body, &mut inner, walk));
            vec![Sexp::List(items, line)]
        }
        Some("if") => fold_if(items, line, env, walk),
        _ => fold_operation(items, line, env, walk),
    }
}

fn fold_arm(arm: Sexp, env: &Env, walk: &mut Walk) -> (String, Vec<Sexp>) {
    let Sexp::List(mut arm, _) = arm else {
        return (String::new(), vec![arm]);
    };
    let body = arm.split_off(1);
    let name = arm[0].atom().unwrap_or_default().to_owned();
    let mut inner = env.clone();
    (name, fold_sequence(body, &mut inner, walk))
}

fn fold_if(mut items: Vec<Sexp>, line: usize, env: &mut Env, walk: &mut Walk) -> Vec<Sexp> {
    let rest = items.split_off(block_header(&items));
    let mut conditions = Vec::new();
    let mut arms = Vec::new();
    for item in rest {
        if matches!(item.head(), Some("then" | "else")) {
            arms.push(item);
        } else {
            conditions.extend(fold_instruction(item, env, walk));
        }
    }
    let Some((taken, mut kept)) = decided(&conditions, env, walk.known) else {
        items.extend(conditions);
        for arm in arms {
            let (name, body) = fold_arm(arm, env, walk);
            let mut arm = vec![atom(&name)];
            arm.extend(body);
            items.push(list(arm));
        }
        return vec![Sexp::List(items, line)];
    };
    let wanted = if taken { "then" } else { "else" };
    let chosen = arms
        .into_iter()
        .find(|arm| arm.head() == Some(wanted))
        .map(|arm| fold_arm(arm, env, walk).1);
    let has_result = items.last().and_then(Sexp::head) == Some("result");
    if chosen.is_none() && !has_result {
        return kept;
    }
    let chosen = chosen.unwrap_or_default();
    if items.len() == 1 && !branches_to(&chosen, None) {
        kept.extend(chosen);
        return kept;
    }
    items[0] = atom("block");
    items.extend(chosen);
    kept.push(Sexp::List(items, line));
    kept
}

fn fold_operation(items: Vec<Sexp>, line: usize, env: &mut Env, walk: &mut Walk) -> Vec<Sexp> {
    let head = items
        .first()
        .and_then(Sexp::atom)
        .unwrap_or_default()
        .to_owned();
    if items.len() == 2
        && let Some(constant) = replaced(&head, &items[1], walk.known)
    {
        return vec![list(constant)];
    }
    let mut folded = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Sexp::List(..) => {
                let produced = fold_instruction(item, env, walk);
                for form in &produced {
                    effects(form, env, walk);
                }
                folded.extend(produced);
            }
            other => folded.push(other),
        }
    }
    let items = folded;
    if head == "call"
        && let Some(callee) = items.get(1).and_then(Sexp::atom)
    {
        for (index, argument) in items[2..].iter().enumerate() {
            let set = evaluate(argument, env, walk.known, 0);
            walk.calls.push((callee.to_owned(), index, set));
        }
    }
    if head == "return" {
        let set = items
            .get(1)
            .and_then(|value| evaluate(value, env, walk.known, 0));
        walk.returned(set);
    }
    if head == "br_if" && items.len() == 3 {
        match constant_of(&items[2]) {
            Some(Value::I32(0)) => return Vec::new(),
            Some(Value::I32(_)) => return vec![list(vec![atom("br"), items[1].clone()])],
            _ => {}
        }
    }
    let folded = Sexp::List(items, line);
    if matches!(head.as_str(), "local.get" | "i32.const" | "i64.const") {
        return vec![folded];
    }
    if removable(&folded, walk.known)
        && let Some(set) = evaluate(&folded, env, walk.known, 0)
        && let Some(value) = only(&set)
        && let Some(constant) = written(value)
    {
        return vec![list(constant)];
    }
    vec![folded]
}

fn function_items(form: &Sexp) -> Option<&[Sexp]> {
    match form {
        Sexp::List(items, _) if form.head() == Some("func") => Some(items),
        _ => None,
    }
}

fn function_name(items: &[Sexp]) -> String {
    items
        .get(1)
        .and_then(Sexp::atom)
        .unwrap_or_default()
        .to_owned()
}

fn admissible(value: &Value) -> bool {
    matches!(value, Value::Address(_, 0) | Value::I32(0) | Value::I64(0))
}

fn joined(slot: Option<Set>, set: Option<Set>) -> Option<Set> {
    let (mut slot, set) = (slot?, set?);
    if !set.iter().all(admissible) {
        return None;
    }
    slot.extend(set);
    Some(slot)
}

fn assigned(items: &[Sexp], counts: &mut BTreeMap<String, usize>) {
    let mut previous: Option<&str> = None;
    for item in items {
        match item {
            Sexp::Atom(word, _) => {
                if matches!(previous, Some("local.set" | "local.tee")) {
                    *counts.entry(word.clone()).or_default() += 1;
                }
                previous = Some(word);
            }
            Sexp::List(inner, _) => {
                previous = None;
                assigned(inner, counts);
            }
            Sexp::Text(..) => previous = None,
        }
    }
}

fn assignment<'f>(items: &'f [Sexp], name: &str) -> Option<&'f Sexp> {
    for item in items {
        let Sexp::List(inner, _) = item else {
            continue;
        };
        if let [Sexp::Atom(word, _), Sexp::Atom(local, _), value] = inner.as_slice()
            && matches!(word.as_str(), "local.set" | "local.tee")
            && local == name
        {
            return Some(value);
        }
        if let Some(value) = assignment(inner, name) {
            return Some(value);
        }
    }
    None
}

fn local_zeros(items: &[Sexp]) -> Vec<(String, Value)> {
    items[2..body_at(items)]
        .iter()
        .filter(|item| item.head() == Some("local"))
        .filter_map(|item| match item {
            Sexp::List(local, _) => match local.as_slice() {
                [_, Sexp::Atom(name, _), Sexp::Atom(ty, _)] if name.starts_with('$') => {
                    let zero = if ty == "i64" {
                        Value::I64(0)
                    } else {
                        Value::I32(0)
                    };
                    Some((name.clone(), zero))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn zero_tested(form: &Sexp, out: &mut Vec<String>) {
    let Sexp::List(items, _) = form else {
        return;
    };
    match (items.first().and_then(Sexp::atom), items.get(1)) {
        (Some("i32.eqz" | "i64.eqz"), Some(Sexp::List(inner, _)))
            if inner.len() == 2 && inner[0].atom() == Some("local.get") =>
        {
            out.extend(inner[1].atom().map(str::to_owned));
        }
        (Some("i32.or"), _) => {
            for item in &items[1..] {
                zero_tested(item, out);
            }
        }
        _ => {}
    }
}

fn guarded(form: &Sexp) -> Vec<String> {
    let Sexp::List(items, _) = form else {
        return Vec::new();
    };
    let at = block_header(items);
    let [condition, Sexp::List(then, _)] = &items[at..] else {
        return Vec::new();
    };
    let diverging =
        then.first().and_then(Sexp::atom) == Some("then") && then.last().is_some_and(diverges);
    if form.head() != Some("if") || at != 1 || !diverging {
        return Vec::new();
    }
    let mut names = Vec::new();
    zero_tested(condition, &mut names);
    names
}

fn replaced(word: &str, operand: &Sexp, known: &Known) -> Option<Vec<Sexp>> {
    if !matches!(word, "global.get" | "call") {
        return None;
    }
    written(known.constants.get(operand.atom()?)?)
}

fn pruned(items: Vec<Sexp>) -> Vec<Sexp> {
    let mut out = Vec::with_capacity(items.len());
    let mut items = items.into_iter().peekable();
    while let Some(item) = items.next() {
        let diverged = match &item {
            Sexp::List(..) => diverges(&item),
            Sexp::Atom(word, _) => matches!(word.as_str(), "return" | "unreachable" | "br"),
            Sexp::Text(..) => false,
        };
        let branch = item.atom() == Some("br");
        let block = item.head() == Some("block");
        out.push(item);
        if diverged && block {
            out.push(atom("unreachable"));
        }
        if branch && let Some(label) = items.next() {
            out.push(label);
        }
        if !diverged {
            continue;
        }
        let mut depth = 0_usize;
        while let Some(next) = items.peek() {
            match next.atom() {
                Some("block" | "loop" | "if") => depth += 1,
                Some("end" | "else") if depth == 0 => break,
                Some("end") => depth -= 1,
                _ => {}
            }
            items.next();
        }
    }
    out
}

fn branches_to(items: &[Sexp], label: Option<&str>) -> bool {
    let words = ["br", "br_if", "br_table"];
    let target = |word: &Sexp| match (label, word.atom()) {
        (Some(label), Some(atom)) => atom == label,
        (None, Some(atom)) => number(atom).is_some(),
        _ => false,
    };
    let flat = items
        .windows(2)
        .any(|pair| pair[0].atom().is_some_and(|word| words.contains(&word)) && target(&pair[1]));
    let folded = items
        .first()
        .and_then(Sexp::atom)
        .is_some_and(|word| words.contains(&word))
        && items[1..].iter().any(target);
    flat || folded
        || items.iter().any(|item| match item {
            Sexp::List(inner, _) => branches_to(inner, label),
            _ => false,
        })
}

fn diverges(form: &Sexp) -> bool {
    let Sexp::List(items, _) = form else {
        return false;
    };
    match items.first().and_then(Sexp::atom) {
        Some("return" | "unreachable" | "br" | "br_table") => true,
        Some("block") => {
            let label = items
                .get(1)
                .and_then(Sexp::atom)
                .filter(|word| word.starts_with('$'));
            let body = &items[block_header(items)..];
            let ends = match body.last() {
                Some(last @ Sexp::List(..)) => diverges(last),
                Some(Sexp::Atom(word, _)) => matches!(word.as_str(), "return" | "unreachable"),
                _ => false,
            };
            ends && !branches_to(body, label)
        }
        _ => false,
    }
}

fn block_header(items: &[Sexp]) -> usize {
    let mut at = 1;
    if items
        .get(at)
        .and_then(Sexp::atom)
        .is_some_and(|word| word.starts_with('$'))
    {
        at += 1;
    }
    if items.get(at).and_then(Sexp::head) == Some("result") {
        at += 1;
    }
    at
}

fn decided(conditions: &[Sexp], env: &Env, known: &Known) -> Option<(bool, Vec<Sexp>)> {
    let [condition] = conditions else {
        return None;
    };
    let Value::I32(value) = only(&evaluate(condition, env, known, 0)?)?.clone() else {
        return None;
    };
    let kept = if removable(condition, known) {
        Vec::new()
    } else {
        vec![list(vec![atom("drop"), condition.clone()])]
    };
    Some(((value != 0) == (known.arms == Arms::Taken), kept))
}

fn pure(form: &Sexp) -> bool {
    let Sexp::List(items, _) = form else {
        return false;
    };
    let Some(head) = items.first().and_then(Sexp::atom) else {
        return false;
    };
    let numeric = (head.starts_with("i32.") || head.starts_with("i64.")) && !head.contains("store");
    let allowed = numeric || matches!(head, "local.get" | "global.get");
    allowed
        && items[1..]
            .iter()
            .all(|item| matches!(item, Sexp::Atom(..)) || pure(item))
}

fn evaluate(form: &Sexp, env: &Env, known: &Known, depth: usize) -> Option<Set> {
    let Sexp::List(items, _) = form else {
        return None;
    };
    let head = items.first().and_then(Sexp::atom)?;
    if let Some(value) = constant_of(form) {
        return Some(Set::from([value]));
    }
    match (head, items.get(1).and_then(Sexp::atom), items.len()) {
        ("local.get", Some(name), 2) => {
            return env.get(name).filter(|set| !set.is_empty()).cloned();
        }
        ("global.get", Some(name), 2) => {
            return known
                .constants
                .get(name)
                .map(|value| Set::from([value.clone()]));
        }
        ("call", Some(name), _) => return evaluate_call(name, &items[2..], env, known, depth),
        ("i32.load", _, _) => {
            if let Some(set) = table_load(items, env, known, depth) {
                return Some(set);
            }
        }
        _ => {}
    }
    let mut immediates = Vec::new();
    let mut operands = Vec::new();
    for item in &items[1..] {
        match item {
            Sexp::Atom(word, _) => immediates.push(word.as_str()),
            Sexp::List(..) => operands.push(item),
            Sexp::Text(..) => return None,
        }
    }
    let zero = Value::I32(0);
    if let Some(answer) = unsigned_against_zero(head, &operands, env, known, depth) {
        return Some(Set::from([Value::I32(answer)]));
    }
    if head == "i32.and" && operands.len() == 2 {
        for (one, other) in [(0, 1), (1, 0)] {
            if constant_of(operands[one]).as_ref() == Some(&zero) && pure(operands[other]) {
                return Some(Set::from([zero]));
            }
        }
    }
    if head == "select" && operands.len() == 3 && pure(operands[0]) && pure(operands[1]) {
        let condition = evaluate(operands[2], env, known, depth)?;
        let picked = match only(&condition)? {
            Value::I32(0) => operands[1],
            Value::I32(_) => operands[0],
            _ => return None,
        };
        return evaluate(picked, env, known, depth);
    }
    let sets: Vec<Set> = operands
        .iter()
        .map(|operand| evaluate(operand, env, known, depth))
        .collect::<Option<_>>()?;
    let mut out = Set::new();
    for values in product(&sets) {
        out.insert(apply(head, &immediates, &values, known)?);
    }
    Some(out)
}

fn unsigned_against_zero(
    head: &str,
    operands: &[&Sexp],
    env: &Env,
    known: &Known,
    depth: usize,
) -> Option<i32> {
    let answer = match head {
        "i32.ge_u" | "i64.ge_u" => 1,
        "i32.lt_u" | "i64.lt_u" => 0,
        _ => return None,
    };
    let [left, right] = operands else {
        return None;
    };
    let zero = Set::from([Value::I32(0)]);
    let zero_wide = Set::from([Value::I64(0)]);
    let right = evaluate(right, env, known, depth)?;
    (pure(left) && (right == zero || right == zero_wide)).then_some(answer)
}

fn table_load(items: &[Sexp], env: &Env, known: &Known, depth: usize) -> Option<Set> {
    let Some(Sexp::List(address, _)) = items.last() else {
        return None;
    };
    let [Sexp::Atom(add, _), base, _] = address.as_slice() else {
        return None;
    };
    if add != "i32.add" || evaluate(address.last()?, env, known, depth).is_some() {
        return None;
    }
    let Value::Address(name, 0) = only(&evaluate(base, env, known, depth)?)?.clone() else {
        return None;
    };
    let mut skip = 0;
    for immediate in &items[1..items.len() - 1] {
        let value = immediate.atom()?.strip_prefix("offset=")?;
        skip = usize::try_from(number(value)?).ok()?;
    }
    if skip % WORD_BYTES as usize != 0 {
        return None;
    }
    let words = known.words.get(&name)?;
    let words = words.get(skip / WORD_BYTES as usize..)?;
    words
        .iter()
        .map(|word| literal("i32.const", word))
        .collect()
}

fn removable(form: &Sexp, known: &Known) -> bool {
    let Sexp::List(items, _) = form else {
        return false;
    };
    let Some(head) = items.first().and_then(Sexp::atom) else {
        return false;
    };
    let numeric = (head.starts_with("i32.") || head.starts_with("i64.")) && !head.contains("store");
    let call = head == "call"
        && items.get(1).and_then(Sexp::atom).is_some_and(|name| {
            known.expressions.contains_key(name) || known.constants.contains_key(name)
        });
    let allowed = numeric || call || matches!(head, "local.get" | "global.get");
    allowed
        && items[1..]
            .iter()
            .all(|item| matches!(item, Sexp::Atom(..)) || removable(item, known))
}

fn product(sets: &[Set]) -> Vec<Vec<Value>> {
    let mut rows = vec![Vec::new()];
    for set in sets {
        rows = rows
            .into_iter()
            .flat_map(|row| {
                set.iter().map(move |value| {
                    let mut row = row.clone();
                    row.push(value.clone());
                    row
                })
            })
            .collect();
    }
    rows
}

fn evaluate_call(
    name: &str,
    arguments: &[Sexp],
    env: &Env,
    known: &Known,
    depth: usize,
) -> Option<Set> {
    if arguments.is_empty()
        && let Some(value) = known.constants.get(name)
    {
        return Some(Set::from([value.clone()]));
    }
    if depth >= known.depth_max {
        return None;
    }
    let Some((params, body)) = known.expressions.get(name) else {
        return known
            .returns
            .get(name)
            .cloned()
            .flatten()
            .filter(|set| !set.is_empty());
    };
    if params.len() != arguments.len() {
        return None;
    }
    let mut inner = Env::new();
    for (param, argument) in params.iter().zip(arguments) {
        inner.insert(param.clone(), evaluate(argument, env, known, depth)?);
    }
    evaluate(body, &inner, known, depth + 1)
}

fn apply(head: &str, immediates: &[&str], values: &[Value], known: &Known) -> Option<Value> {
    match (head, immediates, values) {
        ("i32.load", immediates, [Value::Address(name, offset)]) => {
            load(name, *offset, immediates, known)
        }
        ("i32.add", [], [Value::Address(name, offset), Value::I32(add)])
        | ("i32.add", [], [Value::I32(add), Value::Address(name, offset)]) => {
            let offset = u32::try_from(i64::from(*offset) + i64::from(*add)).ok()?;
            Some(Value::Address(name.clone(), offset))
        }
        ("i32.eqz", [], [Value::Address(..)]) => Some(Value::I32(0)),
        ("i32.eq", [], [Value::Address(..), Value::I32(0)])
        | ("i32.eq", [], [Value::I32(0), Value::Address(..)]) => Some(Value::I32(0)),
        ("i32.ne", [], [Value::Address(..), Value::I32(0)])
        | ("i32.ne", [], [Value::I32(0), Value::Address(..)]) => Some(Value::I32(1)),
        (_, [], [Value::I32(value)]) => unary32(head, *value),
        (_, [], [Value::I64(value)]) => unary64(head, *value),
        (_, [], [Value::I32(left), Value::I32(right)]) => binary32(head, *left, *right),
        (_, [], [Value::I64(left), Value::I64(right)]) => binary64(head, *left, *right),
        _ => None,
    }
}

fn load(name: &str, offset: u32, immediates: &[&str], known: &Known) -> Option<Value> {
    let mut extra = 0;
    for immediate in immediates {
        let value = immediate.strip_prefix("offset=")?;
        extra = u32::try_from(number(value)?).ok()?;
    }
    let at = offset.checked_add(extra)?;
    if at % WORD_BYTES != 0 {
        return None;
    }
    let word = known
        .words
        .get(name)?
        .get(usize::try_from(at / WORD_BYTES).ok()?)?;
    literal("i32.const", word)
}

fn unary32(head: &str, value: i32) -> Option<Value> {
    match head {
        "i32.eqz" => Some(Value::I32(i32::from(value == 0))),
        "i64.extend_i32_u" => Some(Value::I64(i64::from(value as u32))),
        "i64.extend_i32_s" => Some(Value::I64(i64::from(value))),
        _ => None,
    }
}

fn unary64(head: &str, value: i64) -> Option<Value> {
    match head {
        "i64.eqz" => Some(Value::I32(i32::from(value == 0))),
        "i32.wrap_i64" => Some(Value::I32(value as i32)),
        _ => None,
    }
}

fn compare<T: Ord>(op: &str, signed: (T, T), unsigned: (u64, u64)) -> Option<Value> {
    let truth = match op {
        "eq" => signed.0 == signed.1,
        "ne" => signed.0 != signed.1,
        "lt_s" => signed.0 < signed.1,
        "gt_s" => signed.0 > signed.1,
        "le_s" => signed.0 <= signed.1,
        "ge_s" => signed.0 >= signed.1,
        "lt_u" => unsigned.0 < unsigned.1,
        "gt_u" => unsigned.0 > unsigned.1,
        "le_u" => unsigned.0 <= unsigned.1,
        "ge_u" => unsigned.0 >= unsigned.1,
        _ => return None,
    };
    Some(Value::I32(i32::from(truth)))
}

fn binary32(head: &str, left: i32, right: i32) -> Option<Value> {
    let op = head.strip_prefix("i32.")?;
    let shift = (right as u32) % i32::BITS;
    let value = match op {
        "add" => left.wrapping_add(right),
        "sub" => left.wrapping_sub(right),
        "mul" => left.wrapping_mul(right),
        "and" => left & right,
        "or" => left | right,
        "xor" => left ^ right,
        "shl" => left.wrapping_shl(shift),
        "shr_u" => ((left as u32) >> shift) as i32,
        "shr_s" => left >> shift,
        _ => {
            let unsigned = (u64::from(left as u32), u64::from(right as u32));
            return compare(op, (left, right), unsigned);
        }
    };
    Some(Value::I32(value))
}

fn binary64(head: &str, left: i64, right: i64) -> Option<Value> {
    let op = head.strip_prefix("i64.")?;
    let shift = u32::try_from((right as u64) % u64::from(i64::BITS)).expect("a shift fits u32");
    let value = match op {
        "add" => left.wrapping_add(right),
        "sub" => left.wrapping_sub(right),
        "mul" => left.wrapping_mul(right),
        "and" => left & right,
        "or" => left | right,
        "xor" => left ^ right,
        "shl" => left.wrapping_shl(shift),
        "shr_u" => ((left as u64) >> shift) as i64,
        "shr_s" => left >> shift,
        _ => return compare(op, (left, right), (left as u64, right as u64)),
    };
    Some(Value::I64(value))
}
