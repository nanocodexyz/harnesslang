use crate::ir::{self, Program};
use crate::run::mutation::Mutation;
use crate::{check, syntax};

pub fn load(file: &str) -> Result<Program, String> {
    load_within(file, &mut Vec::new(), Mutation::None)
}

pub fn load_within(
    file: &str,
    spawning: &mut Vec<(std::path::PathBuf, String)>,
    mutation: Mutation,
) -> Result<Program, String> {
    let source = std::fs::read_to_string(file)
        .map_err(|error| format!("{file}: error: cannot read: {error}"))?;
    let identity = std::fs::canonicalize(file)
        .map_err(|error| format!("{file}: error: cannot resolve: {error}"))?;
    if let Some(start) = spawning.iter().position(|(path, _)| *path == identity) {
        let chain: Vec<&str> = spawning[start..]
            .iter()
            .map(|(_, shown)| shown.as_str())
            .chain([file])
            .collect();
        return Err(format!(
            "{file}: error: the spawn tree returns to a harness it is inside, so its sessions have no bound: {}",
            chain.join(" spawns ")
        ));
    }
    let depth = u64::try_from(spawning.len()).expect("a spawn chain's length fits u64");
    if depth > ir::SPAWN_DEPTH_MAX {
        return Err(format!(
            "{file}: error: the spawn tree is deeper than spawn_depth_max ({}) levels",
            ir::SPAWN_DEPTH_MAX
        ));
    }
    let parsed = syntax::parse(&source).map_err(|diagnostic| diagnostic.render(file))?;
    let directory = std::path::Path::new(file)
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    spawning.push((identity, file.to_owned()));
    let checked = check::check_mutated(
        &parsed,
        &mut |child: &str| {
            let path = directory.join(child);
            load_within(&path.to_string_lossy(), spawning, Mutation::None)
        },
        mutation,
    )
    .map_err(|diagnostic| diagnostic.render(file));
    spawning.pop();
    checked
}
