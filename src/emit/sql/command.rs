use std::process::ExitCode;

use crate::emit::sql::{self, Literal, ParamType, drill};
use crate::ir::Program;
use crate::target::Target;
use crate::{check, syntax};

const USAGE_EXIT: u8 = 2;
const FAILURE_EXIT: u8 = 1;

fn usage() -> ExitCode {
    eprintln!(
        "usage: hl sql render --target T [--mutation M] [--bare] GROUP [NAME=VALUE]...\n       hl sql batch --head H --turn T --turns K\n       hl sql replay --target T --seeds A..B [--mutation M] FILE\n       hl sql verify --target T --seeds A..B FILE DUMP\n  GROUP: connect|schema|acquire|load|commit; M: none|no_fence_predicate|seq_not_unique; --bare leaves out commit's BEGIN and COMMIT; VALUE @PATH reads the value from a file"
    );
    ExitCode::from(USAGE_EXIT)
}

#[derive(Default)]
struct Flags {
    target: Option<Target>,
    mutation: Option<sql::Mutation>,
    seeds: Option<std::ops::Range<u64>>,
    bare: bool,
    head: Option<u64>,
    turn: Option<u64>,
    turns: Option<u64>,
    positional: Vec<String>,
}

fn parse_seeds(value: &str) -> Option<std::ops::Range<u64>> {
    let (low, high) = value.split_once("..")?;
    let range = low.parse().ok()?..high.parse().ok()?;
    (range.start < range.end).then_some(range)
}

fn flags(args: &[String]) -> Option<Flags> {
    let mut flags = Flags::default();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--target" => flags.target = Some(Target::parse(rest.next()?)?),
            "--mutation" => flags.mutation = Some(sql::Mutation::parse(rest.next()?)?),
            "--seeds" => flags.seeds = Some(parse_seeds(rest.next()?)?),
            "--bare" => flags.bare = true,
            "--head" => flags.head = Some(rest.next()?.parse().ok()?),
            "--turn" => flags.turn = Some(rest.next()?.parse().ok()?),
            "--turns" => flags.turns = Some(rest.next()?.parse().ok()?),
            other if other.starts_with("--") => return None,
            other => flags.positional.push(other.to_owned()),
        }
    }
    Some(flags)
}

fn load(file: &str) -> Result<Program, String> {
    let source = std::fs::read_to_string(file)
        .map_err(|error| format!("{file}: error: cannot read: {error}"))?;
    let parsed = syntax::parse(&source).map_err(|diagnostic| diagnostic.render(file))?;
    check::check(&parsed).map_err(|diagnostic| diagnostic.render(file))
}

fn finish(result: Result<String, String>) -> ExitCode {
    match result {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("hl sql: error: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let Some((command, rest)) = args.split_first() else {
        return usage();
    };
    let Some(flags) = flags(rest) else {
        return usage();
    };
    let mutation = flags.mutation.unwrap_or(sql::Mutation::None);
    match (command.as_str(), flags.target, flags.seeds.clone()) {
        ("render", Some(target), None) => finish(render(target, mutation, &flags)),
        ("batch", None, None) => match (flags.head, flags.turn, flags.turns) {
            (Some(head), Some(turn), Some(turns)) => finish(batch(head, turn, turns)),
            _ => usage(),
        },
        ("replay", Some(target), Some(seeds)) if flags.positional.len() == 1 => {
            finish(replay(target, mutation, seeds, &flags.positional[0]))
        }
        ("decode", None, None) if flags.positional.len() == 1 => {
            finish(decode(&flags.positional[0]))
        }
        ("verify", Some(target), Some(seeds)) if flags.positional.len() == 2 => finish(verify(
            target,
            seeds,
            &flags.positional[0],
            &flags.positional[1],
        )),
        _ => usage(),
    }
}

fn render(target: Target, mutation: sql::Mutation, flags: &Flags) -> Result<String, String> {
    let catalogue = sql::catalogue(target, mutation)?;
    let (group, assignments) = flags
        .positional
        .split_first()
        .ok_or("render needs a statement group")?;
    let statements = match group.as_str() {
        "connect" => catalogue.connect.clone(),
        "schema" => catalogue.schema.clone(),
        "acquire" => vec![catalogue.acquire.clone()],
        "load" => vec![catalogue.load.clone()],
        "commit" if flags.bare => catalogue
            .commit
            .iter()
            .filter(|statement| !matches!(statement.name, "begin" | "end"))
            .cloned()
            .collect(),
        "commit" => catalogue.commit.clone(),
        other => return Err(format!("no statement group '{other}'")),
    };
    let mut named = Vec::new();
    for assignment in assignments {
        let (name, value) = assignment
            .split_once('=')
            .ok_or_else(|| format!("'{assignment}' is not NAME=VALUE"))?;
        let value = match value.strip_prefix('@') {
            Some(path) => std::fs::read_to_string(path)
                .map_err(|error| format!("parameter {name}: cannot read {path}: {error}"))?
                .trim_end_matches('\n')
                .to_owned(),
            None => value.to_owned(),
        };
        let Some(param) = statements
            .iter()
            .flat_map(|statement| statement.params.iter())
            .find(|param| param.name == name)
        else {
            if unmutated_has(target, group, name)? {
                continue;
            }
            return Err(format!("group {group} has no parameter '{name}'"));
        };
        let literal = match param.ty {
            ParamType::Integer => Literal::Integer(
                value
                    .parse()
                    .map_err(|_| format!("parameter {name}: '{value}' is not an integer"))?,
            ),
            ParamType::Text | ParamType::Json => Literal::Text(value),
        };
        named.push((param.name, literal));
    }
    sql::render_all(&statements, &named)
}

fn unmutated_has(target: Target, group: &str, name: &str) -> Result<bool, String> {
    let catalogue = sql::catalogue(target, sql::Mutation::None)?;
    let statements: Vec<&sql::Statement> = match group {
        "acquire" => vec![&catalogue.acquire],
        "load" => vec![&catalogue.load],
        "commit" => catalogue.commit.iter().collect(),
        _ => Vec::new(),
    };
    Ok(statements
        .iter()
        .any(|statement| statement.params.iter().any(|param| param.name == name)))
}

fn batch(head: u64, turn: u64, turns: u64) -> Result<String, String> {
    if turns == 0 {
        return Err("a batch holds at least one turn".to_owned());
    }
    let mut records = Vec::new();
    for offset in 0..turns {
        let finished = turn
            .checked_add(offset)
            .ok_or("turn numbers overflow u64")?;
        records.extend(sql::finished_turn_records(finished, "drill"));
    }
    Ok(format!("{}\n", sql::batch_json(&records, head)))
}

fn replay(
    target: Target,
    mutation: sql::Mutation,
    seeds: std::ops::Range<u64>,
    file: &str,
) -> Result<String, String> {
    let program = load(file)?;
    let catalogue = sql::catalogue(target, mutation)?;
    let mut out = drill::preamble(&catalogue)?;
    let mut totals = drill::Totals::default();
    for seed in seeds.clone() {
        let plan = drill::plan(&program, target, seed)?;
        out.push_str(&drill::replay(&catalogue, &plan, &mut totals)?);
    }
    eprintln!("{}", report(target, &seeds, &totals));
    Ok(out)
}

fn per_turn(total: u64, turns: u64) -> String {
    let hundredths = total * 100 / turns.max(1);
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}

fn report(target: Target, seeds: &std::ops::Range<u64>, totals: &drill::Totals) -> String {
    format!(
        "replay target={} seeds={}..{}: seeds={} turns={} acquires={} transactions={} fenced={} rows={} record_bytes={} batch_bytes={}; per turn: transactions={} rows={} record_bytes={} batch_bytes={}",
        target.name(),
        seeds.start,
        seeds.end,
        totals.seeds,
        totals.turns,
        totals.acquires,
        totals.transactions,
        totals.fenced,
        totals.rows,
        totals.record_bytes,
        totals.batch_bytes,
        per_turn(totals.transactions, totals.turns),
        per_turn(totals.rows, totals.turns),
        per_turn(totals.record_bytes, totals.turns),
        per_turn(totals.batch_bytes, totals.turns),
    )
}

fn verify(
    target: Target,
    seeds: std::ops::Range<u64>,
    file: &str,
    dump: &str,
) -> Result<String, String> {
    let program = load(file)?;
    let text =
        std::fs::read_to_string(dump).map_err(|error| format!("{dump}: cannot read: {error}"))?;
    let mut actual: Vec<String> = text.lines().map(str::to_owned).collect();
    for (index, line) in actual.iter().enumerate() {
        drill::check_dump_line(line).map_err(|error| format!("{dump}:{}: {error}", index + 1))?;
    }
    let mut expected = Vec::new();
    let mut rows = 0;
    for seed in seeds.clone() {
        let plan = drill::plan(&program, target, seed)?;
        rows += plan.rows.len();
        expected.extend(drill::expected_dump(&plan));
    }
    expected.sort();
    actual.sort();
    if let Some(index) = (0..expected.len().max(actual.len()))
        .find(|index| expected.get(*index) != actual.get(*index))
    {
        let shorten = |line: Option<&String>| {
            line.map_or("<none>".to_owned(), |line| line.chars().take(200).collect())
        };
        return Err(format!(
            "{dump}: the database holds {} lines, the interpreter {}; first difference at sorted line {}:\n  database:    {}\n  interpreter: {}",
            actual.len(),
            expected.len(),
            index + 1,
            shorten(actual.get(index)),
            shorten(expected.get(index))
        ));
    }
    Ok(format!(
        "verify target={} seeds={}..{}: {} rows and {} sessions read back from the database decode to the interpreter's records\n",
        target.name(),
        seeds.start,
        seeds.end,
        rows,
        seeds.end - seeds.start
    ))
}

pub fn emit_listing(args: &[String]) -> ExitCode {
    let kept: Vec<String> = args.iter().filter(|arg| *arg != "--sql").cloned().collect();
    let Some(flags) = flags(&kept) else {
        return usage();
    };
    let (Some(target), [file]) = (flags.target, flags.positional.as_slice()) else {
        return usage();
    };
    let mutation = flags.mutation.unwrap_or(sql::Mutation::None);
    finish(
        load(file).and_then(|program| crate::emit::sql::listing::emit(&program, target, mutation)),
    )
}

fn decode(dump: &str) -> Result<String, String> {
    let text =
        std::fs::read_to_string(dump).map_err(|error| format!("{dump}: cannot read: {error}"))?;
    let (rows, sessions) = drill::check_dump(&text).map_err(|error| format!("{dump}: {error}"))?;
    Ok(format!(
        "decode: {rows} rows in {sessions} sessions decode, with contiguous seqs and matching session rows\n"
    ))
}
