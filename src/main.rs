use std::process::ExitCode;

use harnesslang::emit;
use harnesslang::ir::{self, Program, Type};
use harnesslang::run::journal::{CRC32_CHECK, CRC32_CHECK_INPUT};
use harnesslang::run::json;
use harnesslang::run::mutation::Mutation;
use harnesslang::run::turn::TurnRequest;
use harnesslang::run::value::{self, Value};
use harnesslang::sim::{self, Options};
use harnesslang::target::{self, Target};

const USAGE_EXIT: u8 = 2;
const FAILURE_EXIT: u8 = 1;
const WIRE_DIFF_SEEDS: u64 = 2000;
const SUMMARIZED_RESULT: &str = "summarized";

use harnesslang::load::{load, load_within};

fn usage() -> ExitCode {
    eprintln!(
        "usage: hl check [--target {targets}] FILE\n       hl run FILE --handler NAME [--arg VALUE | --arg-file PATH]... [--seed S]\n       hl sim --target {targets} --seeds A..B [--twice] [--strict [--any-state]] [--mutation {mutations}] [--trace PATH] [--scope KEY] [--dump-journal DIR] FILE\n       hl sim --exhaustive [--depth N] [--cuts lines|bytes] --target {targets} --seeds A..B [--mutation {mutations}] FILE\n       hl sim --target {targets} --summarize JOURNAL [--seed S] FILE\n       hl emit --target {targets} [--format wasm|mjs|wat|flat] [--plant NAME] FILE
       hl wasm-sim --target {targets} --module FILE.wasm --trace TRACE.jsonl [--records-max HANDLER=N]... [--requests-max HANDLER=N]...
       hl bundle --target {targets} --out DIR [--plant NAME] FILE
       {deploy}
       {prove}",
        targets = Target::names(),
        mutations = Mutation::names(),
        prove = harnesslang::prove::cli::usage(),
        deploy = harnesslang::deploy::usage()
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
    assert_eq!(
        harnesslang::run::blob::sha256_hex(harnesslang::run::blob::SHA256_CHECK_INPUT),
        harnesslang::run::blob::SHA256_CHECK,
        "SHA-256 matches its catalogued check value"
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
        "assemble" => assemble_command(rest),
        "bundle" => bundle_command(rest),
        "wasm-sim" => wasm_sim_command(rest),
        "wire-diff" => wire_diff_command(rest),
        "sql" => harnesslang::emit::sql::command::run(rest),
        "prove" => ExitCode::from(harnesslang::prove::cli::run(rest, load)),
        "deploy" | "status" | "monitor" | "logs" | "rollback" | "secrets" | "destroy" => {
            deploy_command(command, rest)
        }
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
    scope: String,
    dump: Option<String>,
    summarize: Option<String>,
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
        scope: String::new(),
        dump: None,
        summarize: None,
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
            "--arg-file" => {
                let text = std::fs::read_to_string(rest.next()?).ok()?;
                flags.arguments.push(text);
            }
            "--seed" => flags.seed = rest.next()?.parse().ok()?,
            "--trace" => flags.trace = Some(rest.next()?.clone()),
            "--scope" => flags.scope = rest.next()?.clone(),
            "--dump-journal" => flags.dump = Some(rest.next()?.clone()),
            "--summarize" => flags.summarize = Some(rest.next()?.clone()),
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
    if worst.wait_ms.total > 0 {
        lines.push(format!(
            "    milliseconds of waits: {}, {}",
            worst.wait_ms.total,
            worst.wait_ms.describe_loops()
        ));
    }
    if worst.blob_bytes.total > 0 {
        lines.push(format!(
            "    blob bytes journaled: {}, {}",
            worst.blob_bytes.total,
            worst.blob_bytes.describe_loops()
        ));
    }
    if let Some(family) = &worst.family {
        lines.push(format!(
            "    child sessions: {}, {}",
            worst.spawns.total,
            worst.spawns.describe_loops()
        ));
        for (tool, spawn) in program.spawns() {
            let tree = spawn.tree();
            let descendants = match &spawn.sessions {
                Some(limit) if tree.sessions > 1 => format!(
                    ", {} sessions with its descendants (sessions at most {} ({}))",
                    tree.sessions, limit.name, limit.value
                ),
                _ => String::new(),
            };
            lines.push(format!(
                "      {} runs {}.{}: worst case {} effects, {} journal records, {} outbound requests each{descendants}",
                tool.name,
                spawn.harness(),
                spawn.handler,
                tree.measures[0],
                tree.measures[1],
                tree.measures[2]
            ));
        }
        lines.push(format!(
            "    with its child sessions: {} effects, {} journal records, {} outbound requests",
            family.total[0], family.total[1], family.total[2]
        ));
        if family.depth > 1 {
            lines.push(format!(
                "    sessions in its tree: {}, {} levels below it",
                family.sessions, family.depth
            ));
        }
    }
    lines.push("    plus one recovery record per restart".to_owned());
    if let Some(idle) = program
        .idle
        .as_ref()
        .filter(|idle| idle.handler == handler.name)
    {
        let times = |measure: u64| {
            measure
                .checked_mul(idle.bound.value)
                .map_or_else(|| "more than u64".to_owned(), |total| total.to_string())
        };
        lines.push(format!(
            "    runs when the session idles until {}, at most {} ({}) time(s) after each other turn: {} effects, {} journal records, {} outbound requests in all",
            program.state[idle.due].name,
            idle.bound.name,
            idle.bound.value,
            times(worst.effects.total),
            times(worst.records.total),
            times(worst.requests.total)
        ));
    }
    let events = &program.events;
    if events.any() {
        let steers = events.steers.as_ref().map_or_else(String::new, |bound| {
            format!(", {} ({}) steer(s)", bound.name, bound.value)
        });
        let jobs = events.jobs.as_ref().map_or_else(String::new, |bound| {
            format!(", {} ({}) job notice(s)", bound.name, bound.value)
        });
        lines.push(format!(
            "    plus at most {} event record(s) per turn ({} cancel{steers}{jobs})",
            events.records_max(),
            u64::from(events.cancel)
        ));
    }
    lines.join("\n")
}

fn side_lines(side: &ir::Handler) -> String {
    let worst = &side.worst;
    [
        format!(
            "  side handler {}: worst case {} effects, 0 journal records, {} outbound requests",
            side.name, worst.effects.total, worst.requests.total
        ),
        format!("    effects: {}", worst.effects.describe_loops()),
        format!("    outbound requests: {}", worst.requests.describe_loops()),
        "    journaled nowhere: it runs over the state at the last finished turn".to_owned(),
    ]
    .join(
        "
",
    )
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
    let sides = if program.sides.is_empty() {
        String::new()
    } else {
        format!(", {} side handler(s)", program.sides.len())
    };
    println!(
        "{file}: harness {}: {} tool(s), {} state field(s), {} handler(s){sides}",
        program.name,
        program.tools.len(),
        program.state.len(),
        program.handlers.len()
    );
    for handler in &program.handlers {
        println!("{}", worst_lines(&program, handler));
    }
    for side in &program.sides {
        println!("{}", side_lines(side));
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
    if !program.codes.is_empty() {
        println!("{}", target::describe_cells(&model));
    }
    if program.events.any() {
        println!("{}", target::inbound::describe(target));
    }
    if let Some(limit) = model
        .limits
        .record_bytes
        .filter(|_| program.blobs.is_some())
    {
        println!(
            "  limit record bytes: {} {} ({})",
            limit.value, limit.unit, limit.source
        );
    }
    if let Some(limit) = model.limits.memory {
        println!(
            "  not checked: memory ({} {}), since text values carry no bound",
            limit.value, limit.unit
        );
    }
    if let Some(limit) = model.limits.duration_per_invocation {
        println!(
            "  not checked: duration ({} {}) beyond waits, since other effects' durations are not modelled",
            limit.value, limit.unit
        );
    }
    let mut errors = target::check_cells(&program, &model);
    errors.extend(target::check_limits(&program, &model));
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

fn summarize_command(file: &str, target: Target, journal: &str, seed: u64) -> ExitCode {
    let program = match load(file) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    };
    let bytes = match std::fs::read(journal) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("{journal}: error: cannot read: {error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    };
    let checksum = sim::dump::checksummed(target, Mutation::None);
    let (text, _) =
        sim::dump::summarize(&program, target, checksum, seed, SUMMARIZED_RESULT, &bytes);
    print!("{text}");
    ExitCode::SUCCESS
}

fn sim_command(args: &[String]) -> ExitCode {
    let Some(flags) = flags(args) else {
        return usage();
    };
    if let (Some(journal), Some(file), Some(target), None, None) = (
        &flags.summarize,
        &flags.file,
        flags.target,
        &flags.seeds,
        &flags.dump,
    ) {
        return summarize_command(file, target, journal, flags.seed);
    }
    let (Some(file), Some(target), Some(seeds)) = (flags.file, flags.target, flags.seeds) else {
        return usage();
    };
    let program = match load_within(&file, &mut Vec::new(), flags.mutation) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    };
    let refused = target::check_cells(&program, &target::model(target));
    if !refused.is_empty() {
        for error in refused {
            eprintln!("{file}: error: {error}");
        }
        return ExitCode::from(FAILURE_EXIT);
    }
    if let Err(error) = sim::supported(&program) {
        eprintln!("{file}: error: {error}");
        return ExitCode::from(FAILURE_EXIT);
    }
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
        scope: flags.scope.clone(),
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

fn assemble_command(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        return usage();
    }
    let mut text = String::new();
    if let Err(error) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text) {
        eprintln!("assemble: error: cannot read standard input: {error}");
        return ExitCode::from(FAILURE_EXIT);
    }
    match harnesslang::emit::wasm::assemble::assemble(&text) {
        Ok(module) => write_bytes(&module.encode()),
        Err(error) => {
            eprintln!("assemble: error: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

fn wire_diff_command(args: &[String]) -> ExitCode {
    let seeds = match args {
        [flag, value] if flag == "--seeds" => parse_seeds(value),
        [] => Some(0..WIRE_DIFF_SEEDS),
        _ => None,
    };
    let Some(seeds) = seeds else {
        return usage();
    };
    match harnesslang::wire::diff::run(seeds) {
        Ok(summary) => {
            print!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

fn wasm_sim_command(args: &[String]) -> ExitCode {
    let (mut target, mut module, mut trace) = (None, None, None);
    let mut bounds = harnesslang::wasm::sim::Bounds::default();
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        let bound = match arg.as_str() {
            "--records-max" => Some(&mut bounds.records_per_run),
            "--requests-max" => Some(&mut bounds.requests_per_run),
            _ => None,
        };
        if let Some(bound) = bound {
            let Some((handler, value)) = words.next().and_then(|word| word.split_once('=')) else {
                return usage();
            };
            let Ok(value) = value.parse::<u64>() else {
                return usage();
            };
            bound.insert(handler.to_owned(), value);
            continue;
        }
        let slot = match arg.as_str() {
            "--target" => &mut target,
            "--module" => &mut module,
            "--trace" => &mut trace,
            _ => return usage(),
        };
        match words.next() {
            Some(value) => *slot = Some(value.clone()),
            None => return usage(),
        }
    }
    let (Some(target), Some(module), Some(trace)) = (target, module, trace) else {
        return usage();
    };
    let bytes = match std::fs::read(&module) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("wasm-sim: error: cannot read {module}: {error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    };
    let text = match std::fs::read_to_string(&trace) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("wasm-sim: error: cannot read {trace}: {error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    };
    let origin = std::time::Instant::now();
    let clock = || origin.elapsed().as_secs_f64() * 1000.0;
    match harnesslang::wasm::sim::replay(&target, &bytes, &text, &clock, &bounds) {
        harnesslang::wasm::sim::Outcome::Passed(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        harnesslang::wasm::sim::Outcome::Failed(line) => {
            println!("{line}");
            ExitCode::from(FAILURE_EXIT)
        }
        harnesslang::wasm::sim::Outcome::Errored(message) => {
            eprintln!("{message}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

fn write_bytes(bytes: &[u8]) -> ExitCode {
    use std::io::Write;
    match std::io::stdout().lock().write_all(bytes) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: cannot write standard output: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

fn emit_command(args: &[String]) -> ExitCode {
    if args.iter().any(|arg| arg == "--sql") {
        return harnesslang::emit::sql::command::emit_listing(args);
    }
    let mut format = "wasm";
    let mut plant = "none";
    let mut rest = Vec::new();
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        match arg.as_str() {
            "--format" => match words.next() {
                Some(value) => format = value.as_str(),
                None => return usage(),
            },
            "--plant" => match words.next() {
                Some(value) => plant = value.as_str(),
                None => return usage(),
            },
            _ => rest.push(arg.clone()),
        }
    }
    let Some(flags) = flags(&rest) else {
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
    let emitted = match format {
        "wasm" => {
            emit::wasm::emit_planted(&program, target, plant).map(|bytes| write_bytes(&bytes))
        }
        "mjs" => emit::wasm::emit_planted(&program, target, plant).map(|bytes| {
            print!("{}", emit::wasm::module_js(&bytes));
            ExitCode::SUCCESS
        }),
        "wat" => emit::wasm::emit_text_planted(&program, target, plant).map(|text| {
            print!("{text}");
            ExitCode::SUCCESS
        }),
        "flat" => emit::wasm::emit_flat(&program, target).map(|text| {
            print!("{text}");
            ExitCode::SUCCESS
        }),
        _ => return usage(),
    };
    match emitted {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{file}: error: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

fn bundle_command(args: &[String]) -> ExitCode {
    let mut out = None;
    let mut plant = "none";
    let mut rest = Vec::new();
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        match arg.as_str() {
            "--out" => match words.next() {
                Some(value) => out = Some(std::path::PathBuf::from(value)),
                None => return usage(),
            },
            "--plant" => match words.next() {
                Some(value) => plant = value.as_str(),
                None => return usage(),
            },
            _ => rest.push(arg.clone()),
        }
    }
    let Some(flags) = flags(&rest) else {
        return usage();
    };
    let (Some(file), Some(target), Some(out)) = (flags.file, flags.target, out) else {
        return usage();
    };
    let written = load(&file).and_then(|program| {
        let bundle = harnesslang::deploy::bundle::emitted_planted(
            &program,
            target,
            plant,
            harnesslang::deploy::Plant::None,
        )?;
        bundle.write_with_manifest(&out)?;
        Ok(bundle.describe())
    });
    match written {
        Ok(described) => {
            println!("{}: {described}", out.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{file}: error: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

fn deploy_command(verb: &str, args: &[String]) -> ExitCode {
    use harnesslang::deploy;
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let poll_ms = match variable(deploy::POLL_VARIABLE).map(|text| text.parse::<u64>()) {
        None => deploy::POLL_MS_DEFAULT,
        Some(Ok(value)) if value > 0 => value,
        Some(_) => {
            eprintln!(
                "hl {verb}: {} must be a positive whole number of milliseconds",
                deploy::POLL_VARIABLE
            );
            return ExitCode::from(USAGE_EXIT);
        }
    };
    let now_ms = match variable(deploy::NOW_VARIABLE).map(|text| text.parse::<u64>()) {
        Some(Ok(value)) => value,
        Some(Err(_)) => {
            eprintln!(
                "hl {verb}: {} must be a whole number of milliseconds since 1970",
                deploy::NOW_VARIABLE
            );
            return ExitCode::from(USAGE_EXIT);
        }
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| {
                u64::try_from(elapsed.as_millis()).expect("milliseconds since 1970 fit u64")
            })
            .unwrap_or(0),
    };
    let environment = deploy::Environment {
        cloudflare_token: variable(deploy::CLOUDFLARE_TOKEN_VARIABLE),
        cloudflare_account: variable(deploy::CLOUDFLARE_ACCOUNT_VARIABLE),
        cloudflare_api: variable(deploy::CLOUDFLARE_API_VARIABLE)
            .unwrap_or_else(|| deploy::CLOUDFLARE_API_DEFAULT.to_owned()),
        vercel_token: variable(deploy::VERCEL_TOKEN_VARIABLE),
        vercel_team: variable(deploy::VERCEL_TEAM_VARIABLE),
        vercel_api: variable(deploy::VERCEL_API_VARIABLE)
            .unwrap_or_else(|| deploy::VERCEL_API_DEFAULT.to_owned()),
        certificate: variable(deploy::CERTIFICATE_VARIABLE),
        scratch: std::env::temp_dir().join(format!("hl-deploy-{}", std::process::id())),
        now_ms,
        poll_ms,
    };
    let code = deploy::command(verb, args, &environment, &load);
    match std::fs::remove_dir_all(&environment.scratch) {
        Ok(()) => code,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => code,
        Err(error) => {
            eprintln!(
                "hl {verb}: cannot remove {}: {error}",
                environment.scratch.display()
            );
            ExitCode::from(FAILURE_EXIT)
        }
    }
}
