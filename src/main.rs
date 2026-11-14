use std::process::ExitCode;

use harnesslang::ir::{self, Program, Type};
use harnesslang::run::journal::{CRC32_CHECK, CRC32_CHECK_INPUT};
use harnesslang::run::json;
use harnesslang::run::mutation::Mutation;
use harnesslang::run::turn::TurnRequest;
use harnesslang::run::value::{self, Value};
use harnesslang::sim::{self, Options};
use harnesslang::target::{self, Target};
use harnesslang::{check, emit, syntax};

const USAGE_EXIT: u8 = 2;
const FAILURE_EXIT: u8 = 1;

fn usage() -> ExitCode {
    eprintln!(
        "usage: hl check [--target {targets}] FILE\n       hl run FILE --handler NAME [--arg VALUE]... [--seed S]\n       hl sim --target {targets} --seeds A..B [--twice] [--strict [--any-state]] [--mutation {mutations}] [--trace PATH] [--dump-journal DIR] FILE\n       hl sim --exhaustive [--depth N] [--cuts lines|bytes] --target {targets} --seeds A..B [--mutation {mutations}] FILE\n       hl emit --target {targets} FILE
       {prove}",
        targets = Target::names(),
        mutations = Mutation::names(),
        prove = harnesslang::prove::cli::usage()
    );
    ExitCode::from(USAGE_EXIT)
}

fn print_report(report: &str) -> std::io::Result<()> {
    use std::io::Write;
    match std::io::stdout().lock().write_all(
        format!(
            "{report}
"
        )
        .as_bytes(),
    ) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        written => written,
    }
}

fn load(file: &str) -> Result<Program, String> {
    let source = std::fs::read_to_string(file)
        .map_err(|error| format!("{file}: error: cannot read: {error}"))?;
    let parsed = syntax::parse(&source).map_err(|diagnostic| diagnostic.render(file))?;
    check::check(&parsed).map_err(|diagnostic| diagnostic.render(file))
}

fn parse_seeds(value: &str) -> Option<std::ops::Range<u64>> {
    let (low, high) = value.split_once("..")?;
    let range = low.parse().ok()?..high.parse().ok()?;
    (range.start < range.end).then_some(range)
}

fn main() -> ExitCode {
    assert_eq!(
        harnesslang::run::crc::crc32(CRC32_CHECK_INPUT),
        CRC32_CHECK,
        "CRC-32 matches its catalogued check value"
    );
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        return usage();
    };
    match command.as_str() {
        "check" => check_command(rest),
        "run" => run_command(rest),
        "sim" => sim_command(rest),
        "emit" => emit_command(rest),
        "sql" => harnesslang::emit::sql::command::run(rest),
        "prove" => ExitCode::from(harnesslang::prove::cli::run(rest, load)),
        "gen" => harnesslang::generate::cli::gen_command(rest),
        "fuzz-parse" => harnesslang::generate::cli::fuzz_parse_command(rest),
        "fuzz-prove" => harnesslang::generate::cli::fuzz_prove_command(rest),
        _ => usage(),
    }
}

struct Flags {
    file: Option<String>,
    target: Option<Target>,
    seeds: Option<std::ops::Range<u64>>,
    twice: bool,
    strict: bool,
    any_state: bool,
    mutation: Mutation,
    handler: Option<String>,
    arguments: Vec<String>,
    seed: u64,
    trace: Option<String>,
    dump: Option<String>,
    exhaustive: bool,
    depth: u64,
    cuts: sim::exhaustive::Cuts,
}

fn flags(args: &[String]) -> Option<Flags> {
    let mut flags = Flags {
        file: None,
        target: None,
        seeds: None,
        twice: false,
        strict: false,
        any_state: false,
        mutation: Mutation::None,
        handler: None,
        arguments: Vec::new(),
        seed: 0,
        trace: None,
        dump: None,
        exhaustive: false,
        depth: sim::exhaustive::FAULTS_PER_SESSION_DEFAULT,
        cuts: sim::exhaustive::Cuts::Lines,
    };
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--target" => flags.target = Some(Target::parse(rest.next()?)?),
            "--seeds" => flags.seeds = Some(parse_seeds(rest.next()?)?),
            "--twice" => flags.twice = true,
            "--strict" => flags.strict = true,
            "--any-state" => flags.any_state = true,
            "--mutation" => flags.mutation = Mutation::parse(rest.next()?)?,
            "--handler" => flags.handler = Some(rest.next()?.clone()),
            "--arg" => flags.arguments.push(rest.next()?.clone()),
            "--seed" => flags.seed = rest.next()?.parse().ok()?,
            "--trace" => flags.trace = Some(rest.next()?.clone()),
            "--dump-journal" => flags.dump = Some(rest.next()?.clone()),
            "--exhaustive" => flags.exhaustive = true,
            "--depth" => flags.depth = rest.next()?.parse().ok()?,
            "--cuts" => flags.cuts = sim::exhaustive::Cuts::parse(rest.next()?)?,
            file if !file.starts_with("--") && flags.file.is_none() => {
                flags.file = Some(file.to_owned())
            }
            _ => return None,
        }
    }
    Some(flags)
}

fn worst_lines(program: &ir::Program, handler: &ir::Handler) -> String {
    let worst = &handler.worst;
    let measures = [
        ("effects", &worst.effects),
        ("journal records", &worst.records),
        ("outbound requests", &worst.requests),
    ];
    let mut lines = vec![format!(
        "  handler {}: worst case {} effects, {} journal records, {} outbound requests",
        handler.name, worst.effects.total, worst.records.total, worst.requests.total
    )];
    for (name, cost) in measures {
        lines.push(format!("    {name}: {}", cost.describe_loops()));
    }
    lines.push("    plus one recovery record per restart".to_owned());
    let events = &program.events;
    if events.any() {
        let steers = events.steers.as_ref().map_or_else(String::new, |bound| {
            format!(", {} ({}) steer(s)", bound.name, bound.value)
        });
        lines.push(format!(
            "    plus at most {} event record(s) per turn ({} cancel{steers})",
            events.records_max(),
            u64::from(events.cancel)
        ));
    }
    lines.join("\n")
}

fn check_command(args: &[String]) -> ExitCode {
    let Some(flags) = flags(args) else {
        return usage();
    };
    let Some(file) = flags.file else {
        return usage();
    };
    let program = match load(&file) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    };
    println!(
        "{file}: harness {}: {} tool(s), {} state field(s), {} handler(s)",
        program.name,
        program.tools.len(),
        program.state.len(),
        program.handlers.len()
    );
    for handler in &program.handlers {
        println!("{}", worst_lines(&program, handler));
    }
    let proved_targets = flags
        .target
        .map_or_else(|| Target::ALL.to_vec(), |target| vec![target]);
    let proved = harnesslang::prove::cli::report(
        &file,
        &program,
        &proved_targets,
        harnesslang::prove::Mutation::None,
    );
    let Some(target) = flags.target else {
        return if proved {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(FAILURE_EXIT)
        };
    };
    let model = target::model(target);
    println!("{}", target::describe(&model));
    if program.events.any() {
        println!("{}", target::inbound::describe(target));
    }
    if let Some(limit) = model.limits.memory {
        println!(
            "  not checked: memory ({} {}), since text values carry no bound",
            limit.value, limit.unit
        );
    }
    if let Some(limit) = model.limits.duration_per_invocation {
        println!(
            "  not checked: duration ({} {}), since effect durations are not modelled",
            limit.value, limit.unit
        );
    }
    let errors = target::check_limits(&program, &model);
    if errors.is_empty() && proved {
        println!("{file}: ok for {}", target.name());
        return ExitCode::SUCCESS;
    }
    for error in errors {
        eprintln!("{file}: error: {error}");
    }
    ExitCode::from(FAILURE_EXIT)
}

fn argument(program: &Program, ty: &Type, text: &str) -> Result<Value, String> {
    match ty {
        Type::Text => Ok(Value::Text(text.to_owned())),
        Type::U64 => text
            .parse()
            .map(Value::U64)
            .map_err(|_| format!("'{text}' is not a u64")),
        Type::Bool => text
            .parse()
            .map(Value::Bool)
            .map_err(|_| format!("'{text}' is not a bool")),
        other if program.text_enum(other).is_some() => {
            value::from_json(program, other, &json::Json::text(text))
        }
        other => value::from_json(program, other, &json::decode(text)?),
    }
}

fn run_command(args: &[String]) -> ExitCode {
    let Some(flags) = flags(args) else {
        return usage();
    };
    let (Some(file), Some(name)) = (flags.file, flags.handler) else {
        return usage();
    };
    let program = match load(&file) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    };
    let Some(handler) = program.handler(&name) else {
        eprintln!("{file}: error: no handler '{name}'");
        return ExitCode::from(FAILURE_EXIT);
    };
    if flags.arguments.len() != handler.params.len() {
        eprintln!(
            "{file}: error: handler '{name}' takes {} argument(s), found {}",
            handler.params.len(),
            flags.arguments.len()
        );
        return ExitCode::from(FAILURE_EXIT);
    }
    let mut arguments = Vec::new();
    for (local, text) in handler.params.iter().zip(&flags.arguments) {
        match argument(&program, &handler.locals[*local].ty, text) {
            Ok(value) => arguments.push(value),
            Err(error) => {
                eprintln!(
                    "{file}: error: argument '{}': {error}",
                    handler.locals[*local].name
                );
                return ExitCode::from(FAILURE_EXIT);
            }
        }
    }
    let target = flags.target.unwrap_or(Target::NativeJsonl);
    let mut world = sim::world::SimWorld::new(&program, target, Mutation::None, flags.seed, false);
    let request = TurnRequest {
        turn: 1,
        handler: name,
        arguments,
    };
    world.begin_turn(&request, false);
    let (records, epoch) = world.begin_run().expect("a new journal reads");
    let model = world.target;
    let ran = harnesslang::run::turn::run_turn(
        &program,
        &model,
        Mutation::None,
        &mut world,
        epoch,
        &records,
        &request,
    );
    print!("{}", String::from_utf8_lossy(&world.medium.bytes()));
    match ran {
        Ok(_) => ExitCode::SUCCESS,
        Err(exit) => {
            eprintln!("{file}: error: {exit:?}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

fn sim_command(args: &[String]) -> ExitCode {
    let Some(flags) = flags(args) else {
        return usage();
    };
    let (Some(file), Some(target), Some(seeds)) = (flags.file, flags.target, flags.seeds) else {
        return usage();
    };
    let program = match load(&file) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    };
    if !flags.mutation.targets().contains(&target) {
        eprintln!(
            "{file}: error: mutation {} does not apply to {}",
            flags.mutation.name(),
            target.name()
        );
        return ExitCode::from(USAGE_EXIT);
    }
    if flags.exhaustive {
        if flags.twice
            || flags.strict
            || flags.any_state
            || flags.trace.is_some()
            || flags.dump.is_some()
        {
            return usage();
        }
        let options = sim::exhaustive::Options {
            target,
            seeds,
            mutation: flags.mutation,
            faults_per_session: flags.depth,
            cuts: flags.cuts,
        };
        return exhaustive_command(&program, &options, &file);
    }
    if let Some(directory) = flags.dump {
        if flags.twice || flags.strict || flags.any_state || flags.trace.is_some() {
            return usage();
        }
        return dump_command(&program, target, flags.mutation, seeds, &directory, &file);
    }
    let options = Options {
        target,
        seeds,
        twice: flags.twice,
        mutation: flags.mutation,
        strict: match (flags.strict, flags.any_state) {
            (false, false) => sim::strict::Mode::Off,
            (true, false) => sim::strict::Mode::FromZero,
            (true, true) => sim::strict::Mode::FromAny,
            (false, true) => return usage(),
        },
    };
    let summary = match &flags.trace {
        None => sim::sweep(&program, &options),
        Some(path) => match traced_sweep(&program, &options, path) {
            Ok(summary) => summary,
            Err(error) => {
                eprintln!("{path}: error: {error}");
                return ExitCode::from(FAILURE_EXIT);
            }
        },
    };
    if let Err(error) = print_report(&sim::report(&summary, &options, &file)) {
        eprintln!("stdout: error: {error}");
        return ExitCode::from(FAILURE_EXIT);
    }
    if summary.failure.is_some() {
        ExitCode::from(FAILURE_EXIT)
    } else {
        ExitCode::SUCCESS
    }
}

fn exhaustive_command(
    program: &Program,
    options: &sim::exhaustive::Options,
    file: &str,
) -> ExitCode {
    if let Err(error) = sim::exhaustive::validate(options) {
        eprintln!("{file}: error: {error}");
        return ExitCode::from(USAGE_EXIT);
    }
    let summary = sim::exhaustive::explore(program, options);
    if let Err(error) = print_report(&sim::exhaustive::report(&summary, options, file)) {
        eprintln!("stdout: error: {error}");
        return ExitCode::from(FAILURE_EXIT);
    }
    if summary.stopped.is_some() {
        ExitCode::from(FAILURE_EXIT)
    } else {
        ExitCode::SUCCESS
    }
}

fn dump_command(
    program: &Program,
    target: Target,
    mutation: Mutation,
    seeds: std::ops::Range<u64>,
    directory: &str,
    file: &str,
) -> ExitCode {
    let label = std::path::Path::new(file)
        .file_stem()
        .map_or("journal".into(), |stem| stem.to_string_lossy());
    let path = std::path::Path::new(directory);
    match sim::dump::dump(program, target, mutation, seeds, path, &label) {
        Ok(dumped) => {
            for failure in &dumped.failures {
                eprintln!("{file}: {failure}");
            }
            println!(
                "dumped target={} mutation={} journals={} records={} failures={} to {directory}",
                target.name(),
                mutation.name(),
                dumped.journals,
                dumped.records,
                dumped.failures.len()
            );
            if dumped.failures.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(FAILURE_EXIT)
            }
        }
        Err(error) => {
            eprintln!("{file}: error: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

fn traced_sweep(program: &Program, options: &Options, path: &str) -> Result<sim::Summary, String> {
    let file = std::fs::File::create(path).map_err(|error| format!("cannot create: {error}"))?;
    let mut sink = std::io::BufWriter::new(file);
    let summary = sim::sweep_traced(program, options, Some(&mut sink))?;
    std::io::Write::flush(&mut sink).map_err(|error| format!("cannot write: {error}"))?;
    Ok(summary)
}

fn emit_command(args: &[String]) -> ExitCode {
    if args.iter().any(|arg| arg == "--sql") {
        return harnesslang::emit::sql::command::emit_listing(args);
    }
    let Some(flags) = flags(args) else {
        return usage();
    };
    let (Some(file), Some(target)) = (flags.file, flags.target) else {
        return usage();
    };
    let program = match load(&file) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    };
    match emit::emit(&program, target) {
        Ok(code) => {
            print!("{code}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{file}: error: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}
