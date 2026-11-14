use crate::ir::{BinaryOp, Expr, Program};
use crate::run::journal::{self, Outcome, Record};
use crate::run::json::Json;
use crate::run::value::{self, Value};
use crate::target::Target;

const TRAP_PREFIXES: [&str; 2] = ["u64 ", "bound("];

#[derive(Debug, Default)]
pub struct Soundness {
    pub programs: u64,
    pub proved: u64,
    pub runs: u64,
    pub turns: u64,
    pub states: u64,
    pub traps_unproved: u64,
}

fn trap(result: &Json) -> Option<&str> {
    let Json::Object(map) = result else {
        return None;
    };
    let trap = map.get("trap")?.as_str()?;
    TRAP_PREFIXES
        .iter()
        .any(|prefix| trap.starts_with(prefix))
        .then_some(trap)
}

fn pure(program: &Program, state: &[Value], expr: &Expr) -> Option<Value> {
    Some(match expr {
        Expr::U64(number) => Value::U64(*number),
        Expr::Bool(flag) => Value::Bool(*flag),
        Expr::Text(text) => Value::Text(text.clone()),
        Expr::Constant(constant) => Value::U64(program.constants[*constant].value),
        Expr::Field(field) => state[*field].clone(),
        Expr::Cancelled => return None,
        Expr::Not(operand) => Value::Bool(!pure(program, state, operand)?.as_bool()),
        Expr::Len(operand) => Value::U64(pure(program, state, operand)?.as_list().len() as u64),
        Expr::Binary {
            op, left, right, ..
        } => {
            let left = pure(program, state, left)?;
            let right = pure(program, state, right)?;
            binary(*op, &left, &right)?
        }
        Expr::TextLen(operand) => Value::U64(crate::ir::utf16_len(
            pure(program, state, operand)?.as_text(),
        )),
        Expr::Truncate { text, tokens } => Value::Text(crate::ir::truncate_middle(
            pure(program, state, text)?.as_text(),
            program.constants[*tokens].value,
        )),
        Expr::Local(_) | Expr::Member { .. } | Expr::Construct { .. } | Expr::With { .. } => {
            return None;
        }
    })
}

fn binary(op: BinaryOp, left: &Value, right: &Value) -> Option<Value> {
    let number = |value: Option<u64>| value.map(Value::U64);
    Some(match op {
        BinaryOp::Add => number(left.as_u64().checked_add(right.as_u64()))?,
        BinaryOp::Subtract => number(left.as_u64().checked_sub(right.as_u64()))?,
        BinaryOp::Multiply => number(left.as_u64().checked_mul(right.as_u64()))?,
        BinaryOp::Concat => Value::Text(format!("{}{}", left.as_text(), right.as_text())),
        BinaryOp::Equal => Value::Bool(left == right),
        BinaryOp::NotEqual => Value::Bool(left != right),
        BinaryOp::Less => Value::Bool(left.as_u64() < right.as_u64()),
        BinaryOp::LessEqual => Value::Bool(left.as_u64() <= right.as_u64()),
        BinaryOp::Greater => Value::Bool(left.as_u64() > right.as_u64()),
        BinaryOp::GreaterEqual => Value::Bool(left.as_u64() >= right.as_u64()),
        BinaryOp::And => Value::Bool(left.as_bool() && right.as_bool()),
        BinaryOp::Or => Value::Bool(left.as_bool() || right.as_bool()),
    })
}

fn records(bytes: &[u8]) -> Vec<Record> {
    journal::decode_lines(bytes, true)
        .or_else(|_| journal::decode_lines(bytes, false))
        .expect("a passing seed's journal decodes")
        .records
}

fn check_record(program: &Program, record: &Record, stats: &mut Soundness) -> Result<(), String> {
    match record {
        Record::Status {
            turn,
            outcome: Outcome::Failed,
            result,
            ..
        } => match trap(result) {
            Some(trap) => Err(format!("turn {turn} trapped with {trap}")),
            None => Ok(()),
        },
        Record::Checkpoint { payload, .. } => {
            let json = crate::run::json::decode(payload).expect("a checkpoint payload is json");
            let state = value::state_from_json(program, &json).expect("a checkpoint decodes");
            stats.states += 1;
            for invariant in &program.invariants {
                if pure(program, &state, &invariant.condition).map(|held| held.as_bool())
                    != Some(true)
                {
                    return Err(format!(
                        "invariant {} does not hold in a checkpointed state",
                        invariant.name
                    ));
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn traps(records: &[Record]) -> u64 {
    records
        .iter()
        .filter(|record| matches!(record, Record::Status { outcome: Outcome::Failed, result, .. } if trap(result).is_some()))
        .count() as u64
}

pub fn check_program(
    seed: u64,
    sim_seeds: u64,
    ignored: Option<&str>,
    stats: &mut Soundness,
) -> Result<(), String> {
    let generated = crate::generate::program(seed)?;
    let parsed = crate::syntax::parse(&generated.source).expect("a generated program parses");
    let program = crate::check::check(&parsed).expect("a generated program checks");
    stats.programs += 1;
    let report = crate::prove::prove(&program, &Target::ALL, crate::prove::Mutation::None);
    let proved = report
        .findings
        .iter()
        .all(|finding| Some(finding.property) == ignored);
    stats.proved += u64::from(proved);
    for target in Target::ALL {
        for sim_seed in 0..sim_seeds {
            let run = crate::sim::run_seed(&program, target, crate::run::Mutation::None, sim_seed);
            if let Err(violation) = &run.result {
                return Err(format!(
                    "program seed {seed} on {} sim seed {sim_seed}: invariant {} failed: {}",
                    target.name(),
                    violation.invariant.name(),
                    violation.detail
                ));
            }
            stats.runs += 1;
            stats.turns += run.stats.turns;
            let records = records(&run.journal);
            if !proved {
                stats.traps_unproved += traps(&records);
                continue;
            }
            for record in &records {
                check_record(&program, record, stats).map_err(|error| {
                    format!("program seed {seed} proves on every target, yet on {} sim seed {sim_seed}: {error}", target.name())
                })?;
            }
        }
    }
    Ok(())
}
