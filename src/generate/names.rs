use std::collections::BTreeSet;

use crate::generate::chance;
use crate::sim::rng::Rng;

const TRICKY_PERCENT: u64 = 25;
const TRICKY: [&str; 64] = [
    "self",
    "Self",
    "self_",
    "crate",
    "super",
    "fn",
    "fn_",
    "mut",
    "impl",
    "struct",
    "enum",
    "async",
    "await",
    "dyn",
    "move",
    "ref",
    "static",
    "trait",
    "unsafe",
    "use",
    "where",
    "while",
    "yield",
    "gen",
    "try",
    "box",
    "String",
    "Vec",
    "Option",
    "Some",
    "None",
    "Ok",
    "Err",
    "Json",
    "State",
    "Engine",
    "Halt",
    "engine",
    "arguments",
    "json",
    "value",
    "seq",
    "turn",
    "class",
    "function",
    "var",
    "new",
    "this",
    "delete",
    "default",
    "switch",
    "case",
    "null",
    "undefined",
    "NaN",
    "constructor",
    "prototype",
    "toString",
    "valueOf",
    "hasOwnProperty",
    "Object",
    "rt",
    "below",
    "none",
];
const PAIRED: [&str; 8] = ["a_b", "aB", "A_b", "ab", "Ab", "a__b", "w_1", "w1"];

pub fn fresh(rng: &mut Rng, used: &mut BTreeSet<String>, systematic: String) -> String {
    if chance(rng, TRICKY_PERCENT) {
        let pool: Vec<&str> = TRICKY
            .iter()
            .chain(PAIRED.iter())
            .copied()
            .filter(|name| !used.contains(*name))
            .collect();
        if !pool.is_empty() {
            let name = pool[rng.pick(pool.len())].to_owned();
            used.insert(name.clone());
            return name;
        }
    }
    assert!(
        used.insert(systematic.clone()),
        "systematic name {systematic} is used once"
    );
    systematic
}
