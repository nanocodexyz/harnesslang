use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::sim::rng::Rng;
use crate::target::Target;

const MUTATIONS_PER_CASE_MAX: u64 = 4;
const GENERATED_BASE_PERCENT: u64 = 30;
const REPEATS_MAX: u64 = 4096;
const SPAN_BYTES_MAX: u64 = 64;
const SIMULATED_SEEDS: u64 = 1;
const FRAGMENTS: [&str; 34] = [
    "{",
    "}",
    "(",
    ")",
    "<",
    ">",
    ",",
    ":",
    "=",
    "==",
    "=>",
    "->",
    "|",
    ".",
    "+",
    "++",
    "-",
    "*",
    "\"",
    "\\",
    "\n",
    " ",
    "0",
    "18446744073709551615",
    "18446744073709551616",
    "_",
    "not ",
    "list<",
    "maybe<",
    "known<",
    "match x {",
    "loop i below k {",
    "ok(",
    ".id",
];
const KEYWORDS_AND_NAMES: [&str; 12] = [
    "harness",
    "const",
    "type",
    "tool",
    "model",
    "state",
    "on",
    "invariant",
    "requires",
    "unknown",
    "dispatch",
    "len",
];

#[derive(Clone, Copy)]
enum Mutation {
    FlipByte,
    InsertFragment,
    DeleteSpan,
    DuplicateSpan,
    RepeatFragment,
    Truncate,
    ReplaceWord,
}

const MUTATIONS: [(Mutation, u64); 7] = [
    (Mutation::FlipByte, 3),
    (Mutation::InsertFragment, 4),
    (Mutation::DeleteSpan, 3),
    (Mutation::DuplicateSpan, 2),
    (Mutation::RepeatFragment, 1),
    (Mutation::Truncate, 2),
    (Mutation::ReplaceWord, 2),
];

#[derive(Debug, Default)]
pub struct Stats {
    pub cases: u64,
    pub not_utf8: u64,
    pub parse_errors: u64,
    pub check_errors: u64,
    pub checked: u64,
    pub proved: u64,
    pub emitted: u64,
    pub simulated: u64,
    pub panics: u64,
}

fn span(rng: &mut Rng, length: usize) -> (usize, usize) {
    let start = rng.pick(length.max(1)).min(length);
    let size = rng.range(1, SPAN_BYTES_MAX) as usize;
    (start, (start + size).min(length))
}

fn apply(rng: &mut Rng, bytes: &mut Vec<u8>, mutation: Mutation) {
    let length = bytes.len();
    match mutation {
        Mutation::FlipByte if length > 0 => {
            let at = rng.pick(length);
            bytes[at] ^= 1 << rng.below(8);
        }
        Mutation::InsertFragment => {
            let at = rng.pick(length + 1);
            let fragment = FRAGMENTS[rng.pick(FRAGMENTS.len())].as_bytes();
            bytes.splice(at..at, fragment.iter().copied());
        }
        Mutation::DeleteSpan => {
            let (start, end) = span(rng, length);
            bytes.drain(start..end);
        }
        Mutation::DuplicateSpan => {
            let (start, end) = span(rng, length);
            let copy: Vec<u8> = bytes[start..end].to_vec();
            bytes.splice(end..end, copy);
        }
        Mutation::RepeatFragment => {
            let at = rng.pick(length + 1);
            let fragment = FRAGMENTS[rng.pick(FRAGMENTS.len())].as_bytes();
            let repeats = rng.range(1, REPEATS_MAX) as usize;
            let repeated = fragment.repeat(repeats);
            bytes.splice(at..at, repeated);
        }
        Mutation::Truncate => bytes.truncate(rng.pick(length + 1)),
        Mutation::ReplaceWord => {
            let (start, end) = span(rng, length);
            let word = KEYWORDS_AND_NAMES[rng.pick(KEYWORDS_AND_NAMES.len())].as_bytes();
            bytes.splice(start..end, word.iter().copied());
        }
        Mutation::FlipByte => {}
    }
}

pub fn mutated(seed: u64, case: u64, bases: &[Vec<u8>]) -> Vec<u8> {
    let mut rng = Rng::derive_keyed(seed, "mutate", &[case]);
    let mut bytes = if bases.is_empty() || crate::generate::chance(&mut rng, GENERATED_BASE_PERCENT)
    {
        let program_seed = rng.draw();
        match crate::generate::program(program_seed) {
            Ok(generated) => generated.source.into_bytes(),
            Err(_) => Vec::new(),
        }
    } else {
        bases[rng.pick(bases.len())].clone()
    };
    for _ in 0..rng.range(1, MUTATIONS_PER_CASE_MAX) {
        let mutation = crate::generate::weighted(&mut rng, &MUTATIONS);
        apply(&mut rng, &mut bytes, mutation);
    }
    bytes
}

pub fn pipeline(bytes: &[u8], case: u64, stats: &mut Stats) {
    let Ok(source) = std::str::from_utf8(bytes) else {
        stats.not_utf8 += 1;
        return;
    };
    let Ok(parsed) = crate::syntax::parse(source) else {
        stats.parse_errors += 1;
        return;
    };
    let Ok(program) = crate::check::check(&parsed) else {
        stats.check_errors += 1;
        return;
    };
    stats.checked += 1;
    let report = crate::prove::prove(&program, &Target::ALL, crate::prove::Mutation::None);
    if report.errors() == 0 {
        stats.proved += 1;
    }
    for target in Target::ALL {
        crate::target::check_limits(&program, &crate::target::model(target));
        if crate::emit::emit(&program, target).is_ok() {
            stats.emitted += 1;
        }
    }
    simulate(&program, case, stats);
}

fn simulate(program: &crate::ir::Program, case: u64, stats: &mut Stats) {
    let bounded = program
        .handlers
        .iter()
        .all(|handler| handler.worst.effects.total <= crate::generate::EFFECTS_PER_HANDLER_MAX);
    if !bounded {
        return;
    }
    let target = Target::ALL[(case % Target::ALL.len() as u64) as usize];
    for seed in case..case + SIMULATED_SEEDS {
        let run = crate::sim::run_seed(program, target, crate::run::Mutation::None, seed);
        if let Err(violation) = run.result {
            panic!(
                "simulating on {} seed {seed}: invariant {} failed: {}",
                target.name(),
                violation.invariant.name(),
                violation.detail
            );
        }
        stats.simulated += 1;
    }
}

pub fn guarded(bytes: &[u8], case: u64, stats: &mut Stats) -> bool {
    catch_unwind(AssertUnwindSafe(|| pipeline(bytes, case, stats))).is_ok()
}
