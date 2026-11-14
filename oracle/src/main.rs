mod cells;
mod compare;
mod djs;
mod golden;
mod journal;
mod summary;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use compare::{Disagreement, Tally};

const DISAGREEMENTS_SHOWN_MAX: usize = 20;
const SUMMARY_SUFFIX: &str = ".summary.jsonl";
const JOURNAL_SUFFIX: &str = ".jsonl";
const USAGE_EXIT: u8 = 2;
const FAILURE_EXIT: u8 = 1;
const GOLDEN_FLAG: &str = "--golden";

fn pairs(directory: &Path) -> Result<Vec<(String, PathBuf, PathBuf)>, String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("{}: cannot read: {error}", directory.display()))?;
    let mut pairs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", directory.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(SUMMARY_SUFFIX) else {
            continue;
        };
        let journal = directory.join(format!("{stem}{JOURNAL_SUFFIX}"));
        pairs.push((stem.to_owned(), entry.path(), journal));
    }
    pairs.sort();
    Ok(pairs)
}

fn check_directory(
    directory: &Path,
    tally: &mut Tally,
    disagreements: &mut Vec<Disagreement>,
) -> Result<(), String> {
    for (stem, summary, journal) in pairs(directory)? {
        let text = std::fs::read_to_string(&summary)
            .map_err(|error| format!("{}: cannot read: {error}", summary.display()))?;
        let bytes = std::fs::read(&journal)
            .map_err(|error| format!("{}: cannot read: {error}", journal.display()))?;
        compare::journal(&stem, &bytes, &text, tally, disagreements);
    }
    Ok(())
}

fn write_golden(directory: &str) -> ExitCode {
    match golden::write(Path::new(directory)) {
        Ok(count) => {
            println!("ok golden cells={count} directory={directory}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("hl-oracle: error: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

fn main() -> ExitCode {
    let directories: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, directory] = directories.as_slice()
        && flag == GOLDEN_FLAG
    {
        return write_golden(directory);
    }
    if directories.is_empty() {
        eprintln!("usage: hl-oracle DIR... | hl-oracle --golden DIR");
        return ExitCode::from(USAGE_EXIT);
    }
    let mut tally = Tally::default();
    let mut disagreements = Vec::new();
    for directory in &directories {
        if let Err(error) = check_directory(Path::new(directory), &mut tally, &mut disagreements) {
            eprintln!("hl-oracle: error: {error}");
            return ExitCode::from(FAILURE_EXIT);
        }
    }
    for disagreement in disagreements.iter().take(DISAGREEMENTS_SHOWN_MAX) {
        println!("DISAGREE {disagreement}");
    }
    if disagreements.len() > DISAGREEMENTS_SHOWN_MAX {
        println!(
            "... {} more disagreements",
            disagreements.len() - DISAGREEMENTS_SHOWN_MAX
        );
    }
    let verdict = if disagreements.is_empty() {
        "ok"
    } else {
        "FAIL"
    };
    println!("{verdict} {tally} disagreements={}", disagreements.len());
    if disagreements.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(FAILURE_EXIT)
    }
}
