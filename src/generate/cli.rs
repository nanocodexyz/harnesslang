use std::process::ExitCode;

use crate::generate::mutate::{self, Stats};

const FAILURE_EXIT: u8 = 1;
const USAGE_EXIT: u8 = 2;

fn usage() -> ExitCode {
    eprintln!(
        "usage: hl gen --seed S\n       hl gen --seeds A..B --out DIR\n       hl fuzz-parse --seed S --cases N [--save DIR] [FILE]...\n       hl fuzz-parse --replay FILE\n       hl fuzz-prove --seeds A..B --sim-seeds N [--ignore-property P]"
    );
    ExitCode::from(USAGE_EXIT)
}

fn parse_range(value: &str) -> Option<std::ops::Range<u64>> {
    let (low, high) = value.split_once("..")?;
    let range = low.parse().ok()?..high.parse().ok()?;
    (range.start < range.end).then_some(range)
}

pub fn gen_command(args: &[String]) -> ExitCode {
    match args {
        [flag, seed] if flag == "--seed" => {
            let Ok(seed) = seed.parse() else {
                return usage();
            };
            match crate::generate::program(seed) {
                Ok(generated) => {
                    print!("{}", generated.source);
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("gen: error: {error}");
                    ExitCode::from(FAILURE_EXIT)
                }
            }
        }
        [seeds_flag, seeds, out_flag, out] if seeds_flag == "--seeds" && out_flag == "--out" => {
            let Some(seeds) = parse_range(seeds) else {
                return usage();
            };
            gen_many(seeds, out)
        }
        _ => usage(),
    }
}

fn gen_many(seeds: std::ops::Range<u64>, out: &str) -> ExitCode {
    if let Err(error) = std::fs::create_dir_all(out) {
        eprintln!("gen: error: cannot create {out}: {error}");
        return ExitCode::from(FAILURE_EXIT);
    }
    for seed in seeds {
        let generated = match crate::generate::program(seed) {
            Ok(generated) => generated,
            Err(error) => {
                eprintln!("gen: error: {error}");
                return ExitCode::from(FAILURE_EXIT);
            }
        };
        let path = format!("{out}/fuzz{seed}.hl");
        if let Err(error) = std::fs::write(&path, &generated.source) {
            eprintln!("gen: error: cannot write {path}: {error}");
            return ExitCode::from(FAILURE_EXIT);
        }
        println!(
            "{path} attempt={} effects_max={} lines={}",
            generated.attempt,
            generated.effects_max,
            generated.source.lines().count()
        );
    }
    ExitCode::SUCCESS
}

pub fn fuzz_prove_command(args: &[String]) -> ExitCode {
    let (args, ignored) = match args {
        [rest @ .., flag, property] if flag == "--ignore-property" => {
            (rest, Some(property.as_str()))
        }
        _ => (args, None),
    };
    let [seeds_flag, seeds, sim_flag, sim_seeds] = args else {
        return usage();
    };
    let (Some(seeds), Ok(sim_seeds)) = (parse_range(seeds), sim_seeds.parse::<u64>()) else {
        return usage();
    };
    if seeds_flag != "--seeds" || sim_flag != "--sim-seeds" {
        return usage();
    }
    let mut stats = crate::generate::sound::Soundness::default();
    for seed in seeds {
        if let Err(error) =
            crate::generate::sound::check_program(seed, sim_seeds, ignored, &mut stats)
        {
            println!("FAIL fuzz-prove: {error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    }
    println!(
        "fuzz-prove: programs={} proved={} runs={} turns={} checkpointed_states_checked={} traps_in_unproved={}",
        stats.programs, stats.proved, stats.runs, stats.turns, stats.states, stats.traps_unproved
    );
    ExitCode::SUCCESS
}

struct FuzzFlags {
    seed: u64,
    cases: u64,
    save: Option<String>,
    files: Vec<String>,
}

fn fuzz_flags(args: &[String]) -> Option<FuzzFlags> {
    let mut flags = FuzzFlags {
        seed: 0,
        cases: 0,
        save: None,
        files: Vec::new(),
    };
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--seed" => flags.seed = rest.next()?.parse().ok()?,
            "--cases" => flags.cases = rest.next()?.parse().ok()?,
            "--save" => flags.save = Some(rest.next()?.clone()),
            file if !file.starts_with("--") => flags.files.push(file.to_owned()),
            _ => return None,
        }
    }
    Some(flags)
}

fn describe(stats: &Stats) -> String {
    format!(
        "cases={} not_utf8={} parse_errors={} check_errors={} checked={} proved={} emitted={} simulated={} panics={}",
        stats.cases,
        stats.not_utf8,
        stats.parse_errors,
        stats.check_errors,
        stats.checked,
        stats.proved,
        stats.emitted,
        stats.simulated,
        stats.panics
    )
}

pub fn fuzz_parse_command(args: &[String]) -> ExitCode {
    if let [flag, file] = args
        && flag == "--replay"
    {
        let Ok(bytes) = std::fs::read(file) else {
            eprintln!("fuzz-parse: error: cannot read {file}");
            return ExitCode::from(FAILURE_EXIT);
        };
        let mut stats = Stats::default();
        mutate::pipeline(&bytes, 0, &mut stats);
        println!("{}", describe(&stats));
        return ExitCode::SUCCESS;
    }
    let Some(flags) = fuzz_flags(args) else {
        return usage();
    };
    let mut bases = Vec::new();
    for file in &flags.files {
        match std::fs::read(file) {
            Ok(bytes) => bases.push(bytes),
            Err(error) => {
                eprintln!("fuzz-parse: error: cannot read {file}: {error}");
                return ExitCode::from(FAILURE_EXIT);
            }
        }
    }
    fuzz(&flags, &bases)
}

fn save(directory: &Option<String>, name: &str, bytes: &[u8]) -> Result<(), String> {
    let Some(directory) = directory else {
        return Ok(());
    };
    let path = format!("{directory}/{name}");
    std::fs::write(&path, bytes).map_err(|error| format!("cannot write {path}: {error}"))
}

fn fuzz(flags: &FuzzFlags, bases: &[Vec<u8>]) -> ExitCode {
    if let Some(directory) = &flags.save
        && let Err(error) = std::fs::create_dir_all(directory)
    {
        eprintln!("fuzz-parse: error: cannot create {directory}: {error}");
        return ExitCode::from(FAILURE_EXIT);
    }
    let mut stats = Stats::default();
    for case in 0..flags.cases {
        let bytes = mutate::mutated(flags.seed, case, bases);
        let saved = save(&flags.save, "last.hl", &bytes).and_then(|()| {
            stats.cases += 1;
            if mutate::guarded(&bytes, case, &mut stats) {
                return Ok(());
            }
            stats.panics += 1;
            println!("fuzz-parse: panic at seed {} case {case}", flags.seed);
            save(
                &flags.save,
                &format!("panic-{}-{case}.hl", flags.seed),
                &bytes,
            )
        });
        if let Err(error) = saved {
            eprintln!("fuzz-parse: error: {error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    }
    println!("fuzz-parse seed={} {}", flags.seed, describe(&stats));
    if stats.panics > 0 {
        ExitCode::from(FAILURE_EXIT)
    } else {
        ExitCode::SUCCESS
    }
}
