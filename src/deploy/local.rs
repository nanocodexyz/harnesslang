use std::path::{Path, PathBuf};

use super::bundle::Bundle;
use super::{Action, Context, Plant, Secret};

pub const VERSIONS_MAX: u64 = 1000;
const CURRENT_FILE: &str = "current";
const SECRETS_FILE: &str = "secrets.env";
const VARIABLES_FILE: &str = "variables.env";
const DIGEST_FILE: &str = "digest";
const HISTORY_FILE: &str = "history.log";
const SECRETS_MODE: u32 = 0o600;

pub fn run(context: &Context, action: Action) -> Result<(), String> {
    match action {
        Action::Deploy { bundle, variables } => deploy(context, &bundle, &variables),
        Action::Status => status(context),
        Action::Logs { events, .. } => logs(context, events),
        Action::Rollback { to } => rollback(context, to.as_deref()),
        Action::Secrets(secret) => secrets(context, secret),
        Action::Destroy => destroy(context),
    }
}

fn versions_directory(context: &Context) -> PathBuf {
    context.out.join("versions")
}

fn versions(context: &Context) -> Result<Vec<u64>, String> {
    let directory = versions_directory(context);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", directory.display())),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        let name = entry.file_name();
        let number = name
            .to_str()
            .and_then(|text| text.parse::<u64>().ok())
            .ok_or_else(|| {
                format!(
                    "{} holds {}, which is not a version number",
                    directory.display(),
                    name.to_string_lossy()
                )
            })?;
        found.push(number);
    }
    found.sort_unstable();
    Ok(found)
}

fn current(context: &Context) -> Result<Option<u64>, String> {
    let path = context.out.join(CURRENT_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| format!("{} does not hold a version number", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn write_atomic(path: &Path, text: &str, mode: Option<u32>) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let staged = path.with_extension("staged");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    if let Some(mode) = mode {
        options.mode(mode);
    }
    let mut file = options
        .open(&staged)
        .map_err(|error| format!("cannot write {}: {error}", staged.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", staged.display()))?;
    std::fs::rename(&staged, path)
        .map_err(|error| format!("cannot replace {}: {error}", path.display()))
}

fn select(context: &Context, version: u64) -> Result<(), String> {
    write_atomic(
        &context.out.join(CURRENT_FILE),
        &format!("{version}\n"),
        None,
    )
}

fn history(context: &Context, line: &str) -> Result<(), String> {
    use std::io::Write;
    let path = context.out.join(HISTORY_FILE);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    writeln!(
        file,
        "{} {line}",
        super::iso_time(context.environment.now_ms)
    )
    .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn deploy(
    context: &Context,
    bundle: &Bundle,
    variables: &[(String, String)],
) -> Result<(), String> {
    context.say(&format!(
        "deploy {} {}: {}",
        context.target.name(),
        context.name,
        bundle.describe()
    ));
    let known = versions(context)?;
    let next = known.last().map_or(1, |last| last + 1);
    if next > VERSIONS_MAX {
        return Err(format!(
            "{} already keeps versions_max ({VERSIONS_MAX}) versions; hl destroy clears them",
            context.out.display()
        ));
    }
    let directory = versions_directory(context).join(next.to_string());
    bundle.write(&directory)?;
    std::fs::write(directory.join(DIGEST_FILE), format!("{}\n", bundle.digest))
        .map_err(|error| format!("cannot write {}: {error}", directory.display()))?;
    let mut lines = String::new();
    for (key, value) in variables {
        lines.push_str(&format!("{key}={value}\n"));
    }
    std::fs::write(directory.join(VARIABLES_FILE), lines)
        .map_err(|error| format!("cannot write {}: {error}", directory.display()))?;
    if context.plant == Plant::SecretsDropped {
        write_atomic(&context.out.join(SECRETS_FILE), "", Some(SECRETS_MODE))?;
    }
    select(context, next)?;
    history(
        context,
        &format!("deploy version {next} digest {}", bundle.digest),
    )?;
    context.say(&format!(
        "  version {next} is current, installed at {}",
        directory.display()
    ));
    context.say(&format!(
        "  secrets in {} (mode 600), read by the host at start",
        context.out.join(SECRETS_FILE).display()
    ));
    Ok(())
}

fn digest(context: &Context, version: u64) -> Result<String, String> {
    let path = versions_directory(context)
        .join(version.to_string())
        .join(DIGEST_FILE);
    std::fs::read_to_string(&path)
        .map(|text| text.trim().to_owned())
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn status(context: &Context) -> Result<(), String> {
    context.say(&format!(
        "status {} {}",
        context.target.name(),
        context.name
    ));
    let current = current(context)?
        .ok_or_else(|| format!("{} holds no installed version", context.out.display()))?;
    let digest = digest(context, current)?;
    context.say(&format!(
        "  version {current} is current, digest {}",
        &digest[..super::bundle::DIGEST_SHOWN.min(digest.len())]
    ));
    context.say(&format!("  versions: {}", versions(context)?.len()));
    let names = secret_names(context)?;
    context.say(&format!(
        "  secrets: {}",
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    ));
    Ok(())
}

fn logs(context: &Context, events: u64) -> Result<(), String> {
    let path = context.out.join(HISTORY_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let keep = usize::try_from(events)
        .expect("events fit usize")
        .min(lines.len());
    context.say(&format!(
        "logs {} {}: the last {keep} of {} install events",
        context.target.name(),
        context.name,
        lines.len()
    ));
    for line in &lines[lines.len() - keep..] {
        context.say(line);
    }
    Ok(())
}

fn rollback(context: &Context, to: Option<&str>) -> Result<(), String> {
    let current = current(context)?
        .ok_or_else(|| format!("{} holds no installed version", context.out.display()))?;
    let known = versions(context)?;
    let target = match to {
        Some(text) => {
            let wanted: u64 = text
                .parse()
                .map_err(|_| format!("--to {text} is not a version number"))?;
            if !known.contains(&wanted) {
                return Err(format!("{} has no version {wanted}", context.out.display()));
            }
            wanted
        }
        None if context.plant == Plant::RollbackCurrent => current,
        None => *known
            .iter()
            .filter(|version| **version < current)
            .max()
            .ok_or_else(|| {
                format!("version {current} is the first; there is nothing to roll back to")
            })?,
    };
    select(context, target)?;
    history(
        context,
        &format!("rollback to version {target} from {current}"),
    )?;
    context.say(&format!(
        "rollback {} {}: version {target} is current, was {current}",
        context.target.name(),
        context.name
    ));
    Ok(())
}

fn read_secrets(context: &Context) -> Result<Vec<(String, String)>, String> {
    let path = context.out.join(SECRETS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    text.lines()
        .map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .ok_or_else(|| format!("{} holds a line that is not KEY=VALUE", path.display()))
        })
        .collect()
}

fn secret_names(context: &Context) -> Result<Vec<String>, String> {
    Ok(read_secrets(context)?
        .into_iter()
        .map(|(key, _)| key)
        .collect())
}

fn write_secrets(context: &Context, secrets: &[(String, String)]) -> Result<(), String> {
    std::fs::create_dir_all(&context.out)
        .map_err(|error| format!("cannot create {}: {error}", context.out.display()))?;
    let mut text = String::new();
    for (key, value) in secrets {
        if value.contains('\n') {
            return Err(format!(
                "secret {key} holds a line break, which a KEY=VALUE file cannot keep"
            ));
        }
        text.push_str(&format!("{key}={value}\n"));
    }
    write_atomic(&context.out.join(SECRETS_FILE), &text, Some(SECRETS_MODE))
}

fn secrets(context: &Context, secret: Secret) -> Result<(), String> {
    let mut held = read_secrets(context)?;
    match secret {
        Secret::Set(key) => {
            let value = super::read_secret()?;
            held.retain(|(name, _)| *name != key);
            held.push((key.clone(), value));
            held.sort();
            write_secrets(context, &held)?;
            history(context, &format!("secret {key} set"))?;
            context.say(&format!(
                "secrets {} {}: set {key}",
                context.target.name(),
                context.name
            ));
        }
        Secret::List => {
            for (key, _) in held {
                context.say(&key);
            }
        }
        Secret::Delete(key) => {
            let before = held.len();
            held.retain(|(name, _)| *name != key);
            if held.len() == before {
                return Err(format!("{} holds no secret {key}", context.out.display()));
            }
            write_secrets(context, &held)?;
            history(context, &format!("secret {key} deleted"))?;
            context.say(&format!(
                "secrets {} {}: deleted {key}",
                context.target.name(),
                context.name
            ));
        }
    }
    Ok(())
}

fn destroy(context: &Context) -> Result<(), String> {
    if current(context)?.is_none() {
        return Err(format!(
            "{} holds no installed version",
            context.out.display()
        ));
    }
    super::forget(context)?;
    context.say(&format!(
        "destroy {} {}: removed {}",
        context.target.name(),
        context.name,
        context.out.display()
    ));
    Ok(())
}
