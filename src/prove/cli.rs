use crate::ir::Program;
use crate::prove::{self, Mutation, Severity};
use crate::target::Target;

pub const USAGE_EXIT: u8 = 2;
pub const FAILURE_EXIT: u8 = 1;

pub fn usage() -> String {
    format!(
        "hl prove [--target {}] [--mutation {}] FILE",
        Target::names(),
        Mutation::names()
    )
}

pub fn report(file: &str, program: &Program, targets: &[Target], mutation: Mutation) -> bool {
    let report = prove::prove(program, targets, mutation);
    for line in &report.proved {
        println!("  proved {line}");
    }
    for finding in &report.findings {
        match finding.severity {
            Severity::Warning => println!("{file}: warning: {}", finding.render()),
            Severity::Error => eprintln!("{file}: error: {}", finding.render()),
        }
    }
    let names: Vec<&str> = targets.iter().map(|target| target.name()).collect();
    let errors = report.errors();
    if errors == 0 {
        println!("{file}: proved for {}", names.join(", "));
        return true;
    }
    eprintln!(
        "{file}: error: {errors} proof obligation(s) failed for {}",
        names.join(", ")
    );
    false
}

pub fn run(args: &[String], load: fn(&str) -> Result<Program, String>) -> u8 {
    let mut file = None;
    let mut target = None;
    let mut mutation = Mutation::None;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let parsed = match flag.as_str() {
            "--target" => rest.next().and_then(|name| Target::parse(name)).map(|parsed| target = Some(parsed)),
            "--mutation" => rest.next().and_then(|name| Mutation::parse(name)).map(|parsed| mutation = parsed),
            name if !name.starts_with("--") && file.is_none() => {
                file = Some(name.to_owned());
                Some(())
            }
            _ => None,
        };
        if parsed.is_none() {
            eprintln!("usage: {}", usage());
            return USAGE_EXIT;
        }
    }
    let Some(file) = file else {
        eprintln!("usage: {}", usage());
        return USAGE_EXIT;
    };
    let applicable = mutation.targets();
    let targets = match target {
        Some(target) if !applicable.contains(&target) => {
            eprintln!(
                "{file}: error: mutation {} does not apply to {}",
                mutation.name(),
                target.name()
            );
            return USAGE_EXIT;
        }
        Some(target) => vec![target],
        None => applicable,
    };
    let program = match load(&file) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{error}");
            return FAILURE_EXIT;
        }
    };
    if report(&file, &program, &targets, mutation) {
        0
    } else {
        FAILURE_EXIT
    }
}
