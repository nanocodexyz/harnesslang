use std::path::{Path, PathBuf};

use harnesslang::wasm::js::Json;

use crate::{now_ms, random};

const CONTROL: &str = "control";
const CANCEL_PREFIX: &str = "cancel-";
const STEER_PREFIX: &str = "steer-";
const PENDING_SUFFIX: &str = ".new";

pub fn directory(session: &Path) -> PathBuf {
    session.join(CONTROL)
}

pub fn is_delivery(id: &str) -> bool {
    id.starts_with(CANCEL_PREFIX) || id.starts_with(STEER_PREFIX)
}

pub fn send(session: &Path, steer: Option<&str>) -> Result<String, String> {
    let control = directory(session);
    std::fs::create_dir_all(&control).map_err(|error| format!("{}: {error}", control.display()))?;
    let prefix = if steer.is_some() {
        STEER_PREFIX
    } else {
        CANCEL_PREFIX
    };
    let name = format!("{prefix}{:020}-{:016x}", now_ms(), random());
    let pending = control.join(format!("{name}{PENDING_SUFFIX}"));
    std::fs::write(&pending, steer.unwrap_or_default())
        .map_err(|error| format!("{}: {error}", pending.display()))?;
    std::fs::rename(&pending, control.join(&name))
        .map_err(|error| format!("{}: {error}", control.display()))?;
    Ok(name)
}

fn names(control: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(control) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| is_delivery(name) && !name.ends_with(PENDING_SUFFIX))
        .collect();
    names.sort();
    names
}

pub fn cancel_pending(control: &Path) -> bool {
    names(control)
        .iter()
        .any(|name| name.starts_with(CANCEL_PREFIX))
}

pub fn deliveries(control: &Path) -> Vec<Json> {
    names(control)
        .into_iter()
        .filter_map(|name| {
            if name.starts_with(CANCEL_PREFIX) {
                return Some(Json::Object(vec![(
                    "cancel".to_owned(),
                    Json::Object(vec![("id".to_owned(), Json::Text(name))]),
                )]));
            }
            let message = std::fs::read_to_string(control.join(&name)).ok()?;
            Some(Json::Object(vec![(
                "steer".to_owned(),
                Json::Object(vec![
                    ("id".to_owned(), Json::Text(name)),
                    ("message".to_owned(), Json::Text(message)),
                ]),
            )]))
        })
        .collect()
}

pub fn consume(control: &Path, id: &str) {
    let _ = std::fs::remove_file(control.join(id));
}
