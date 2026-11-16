use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use crate::json::Value;
use crate::tools::{self, Ran};

pub const MIN_YIELD_TIME_MS: u64 = 250;
pub const DEFAULT_YIELD_TIME_MS: u64 = 10_000;
pub const MAX_YIELD_TIME_MS: u64 = 30_000;
pub const MIN_EMPTY_YIELD_TIME_MS: u64 = 5_000;
pub const MAX_BACKGROUND_TERMINAL_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 10_000;
pub const OUTPUT_MAX_BYTES: u64 = 1024 * 1024;
pub const MAX_PROCESSES: usize = 64;
pub const BYTES_PER_TOKEN: u64 = 4;
pub const POLL: Duration = Duration::from_millis(10);
const SHELL: &str = "bash";
const PTY_WRAPPER: &str = "script";
const PTY_FLAGS: [&str; 2] = ["-qfe", "-c"];
const PTY_TRANSCRIPT: &str = "/dev/null";
const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const ADD_FILE: &str = "*** Add File: ";
const DELETE_FILE: &str = "*** Delete File: ";
const UPDATE_FILE: &str = "*** Update File: ";
const MOVE_TO: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";
const CHANGE_CONTEXT: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT: &str = "@@";
const HEREDOC_OPENERS: [&str; 3] = ["<<EOF", "<<'EOF'", "<<\"EOF\""];
const HEREDOC_LINES_MIN: usize = 4;

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    output: PathBuf,
    read: u64,
    pty: bool,
}

pub struct Exec {
    root: PathBuf,
    state: PathBuf,
    sessions: BTreeMap<u64, Session>,
    next: u64,
}

enum Settled {
    Exited(i32),
    Running,
    Stopped,
}

fn text_field<'a>(input: &'a Value, key: &str) -> &'a str {
    input.get(key).and_then(Value::as_str).unwrap_or("")
}

fn u64_field(input: &Value, key: &str, default: u64) -> u64 {
    input.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn bool_field(input: &Value, key: &str, default: bool) -> bool {
    match input.get(key) {
        Some(Value::Bool(value)) => *value,
        _ => default,
    }
}

pub fn truncate_tokens(text: &str, max_tokens: u64) -> (String, Option<u64>) {
    let budget = usize::try_from(max_tokens.saturating_mul(BYTES_PER_TOKEN)).unwrap_or(usize::MAX);
    if text.len() <= budget {
        return (text.to_owned(), None);
    }
    let original = (text.len() as u64).div_ceil(BYTES_PER_TOKEN);
    let mut left = budget / 2;
    while !text.is_char_boundary(left) {
        left -= 1;
    }
    let mut right = text.len() - (budget - left);
    while !text.is_char_boundary(right) {
        right += 1;
    }
    let removed = ((right - left) as u64).div_ceil(BYTES_PER_TOKEN);
    (
        format!(
            "{}…{removed} tokens truncated…{}",
            &text[..left],
            &text[right..]
        ),
        Some(original),
    )
}

fn rendered(
    wall: Duration,
    exit: Option<i32>,
    session: Option<u64>,
    output: &str,
    max_tokens: u64,
) -> String {
    let (shown, original) = truncate_tokens(output, max_tokens);
    let mut sections = vec![format!("Wall time: {:.4} seconds", wall.as_secs_f64())];
    if let Some(code) = exit {
        sections.push(format!("Process exited with code {code}"));
    }
    if let Some(id) = session {
        sections.push(format!("Process running with session ID {id}"));
    }
    if let Some(original) = original {
        sections.push(format!("Original token count: {original}"));
    }
    sections.push("Output:".to_owned());
    sections.push(shown);
    sections.join("\n")
}

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| directory.join(program).is_file())
    })
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| {
        tools::SHELL_SIGNAL_STATUS
            + std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(0)
    })
}

impl Exec {
    pub fn new(root: PathBuf, state: PathBuf) -> Self {
        Self {
            root,
            state,
            sessions: BTreeMap::new(),
            next: 1,
        }
    }

    pub fn leave_running(&mut self) -> Vec<(u64, u32)> {
        let mut left = Vec::new();
        for (id, mut session) in std::mem::take(&mut self.sessions) {
            drop(session.stdin.take());
            match session.child.try_wait() {
                Ok(Some(_)) => {
                    let _ = std::fs::remove_file(&session.output);
                }
                _ => left.push((id, session.child.id())),
            }
        }
        left
    }

    fn workdir(&self, workdir: &str) -> PathBuf {
        if workdir.is_empty() {
            return self.root.clone();
        }
        let path = Path::new(workdir);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }

    fn settle(
        session: &mut Session,
        until: Instant,
        stop: &dyn Fn() -> bool,
    ) -> Result<Settled, String> {
        loop {
            if let Some(status) = session
                .child
                .try_wait()
                .map_err(|error| error.to_string())?
            {
                return Ok(Settled::Exited(exit_code(status)));
            }
            if stop() {
                tools::kill_group(&mut session.child)?;
                let _ = session.child.wait();
                return Ok(Settled::Stopped);
            }
            if Instant::now() >= until {
                return Ok(Settled::Running);
            }
            std::thread::sleep(POLL);
        }
    }

    fn drain(session: &mut Session) -> String {
        use std::io::{Read as _, Seek as _};
        let Ok(mut file) = std::fs::File::open(&session.output) else {
            return String::new();
        };
        let size = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let start = session.read.max(size.saturating_sub(OUTPUT_MAX_BYTES));
        if file.seek(std::io::SeekFrom::Start(start)).is_err() {
            return String::new();
        }
        let mut bytes = Vec::new();
        let _ = file.take(size - start).read_to_end(&mut bytes);
        session.read = size;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if session.pty {
            text.replace("\r\n", "\n")
        } else {
            text
        }
    }

    fn finish(
        &mut self,
        mut session: Session,
        settled: Settled,
        begun: Instant,
        max_tokens: u64,
    ) -> Ran {
        let output = Self::drain(&mut session);
        match settled {
            Settled::Exited(code) => {
                let _ = std::fs::remove_file(&session.output);
                Ran::Ok(rendered(
                    begun.elapsed(),
                    Some(code),
                    None,
                    &output,
                    max_tokens,
                ))
            }
            Settled::Stopped => {
                let _ = std::fs::remove_file(&session.output);
                Ran::Failed(format!(
                    "aborted by user after {:.1}s",
                    begun.elapsed().as_secs_f64()
                ))
            }
            Settled::Running => {
                let id = self.next;
                self.next += 1;
                self.sessions.insert(id, session);
                Ran::Ok(rendered(
                    begun.elapsed(),
                    None,
                    Some(id),
                    &output,
                    max_tokens,
                ))
            }
        }
    }

    pub fn exec_command(&mut self, input: &Value, stop: &dyn Fn() -> bool) -> Ran {
        let cmd = text_field(input, "cmd");
        if cmd.is_empty() {
            return Ran::Failed("missing command line for unified exec request".to_owned());
        }
        if self.sessions.len() >= MAX_PROCESSES {
            if let Some(oldest) = self.sessions.keys().next().copied()
                && let Some(mut session) = self.sessions.remove(&oldest)
            {
                let _ = tools::kill_group(&mut session.child);
                let _ = session.child.wait();
            }
        }
        let shell = match text_field(input, "shell") {
            "" if on_path(SHELL) => SHELL,
            "" => tools::SHELL_FALLBACK,
            other => other,
        };
        let login = bool_field(input, "login", true);
        let tty = bool_field(input, "tty", false);
        let yield_ms = u64_field(input, "yield_time_ms", DEFAULT_YIELD_TIME_MS)
            .clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS);
        let max_tokens = u64_field(input, "max_output_tokens", DEFAULT_MAX_OUTPUT_TOKENS);
        let output = self.state.join(format!("exec-{}.out", self.next));
        let flag = if login { "-lc" } else { "-c" };
        let pty = tty && on_path(PTY_WRAPPER);
        let spawned = (|| -> std::io::Result<Child> {
            let file = std::fs::File::create(&output)?;
            let errors = file.try_clone()?;
            let mut command = if pty {
                let mut command = Command::new(PTY_WRAPPER);
                command
                    .args(PTY_FLAGS)
                    .arg(format!("{shell} {flag} {}", quoted(cmd)))
                    .arg(PTY_TRANSCRIPT);
                command
            } else {
                let mut command = Command::new(shell);
                command.arg(flag).arg(cmd);
                command
            };
            command
                .current_dir(self.workdir(text_field(input, "workdir")))
                .stdin(if tty { Stdio::piped() } else { Stdio::null() })
                .stdout(file)
                .stderr(errors)
                .process_group(0)
                .spawn()
        })();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                return Ran::Failed(format!("Failed to create unified exec process: {error}"));
            }
        };
        let begun = Instant::now();
        let stdin = child.stdin.take();
        let mut session = Session {
            child,
            stdin,
            output,
            read: 0,
            pty,
        };
        let until = begun + Duration::from_millis(yield_ms);
        match Self::settle(&mut session, until, stop) {
            Ok(settled) => self.finish(session, settled, begun, max_tokens),
            Err(error) => Ran::Failed(format!("Unified exec process failed: {error}")),
        }
    }

    pub fn write_stdin(&mut self, input: &Value, stop: &dyn Fn() -> bool) -> Ran {
        let id = u64_field(input, "session_id", 0);
        let chars = text_field(input, "chars");
        let max_tokens = u64_field(input, "max_output_tokens", DEFAULT_MAX_OUTPUT_TOKENS);
        let Some(mut session) = self.sessions.remove(&id) else {
            return Ran::Failed(format!("Unknown process id {id}"));
        };
        if !chars.is_empty() {
            let written = match session.stdin.as_mut() {
                None => {
                    self.sessions.insert(id, session);
                    return Ran::Failed("stdin is closed for this session; rerun exec_command with tty=true to keep stdin open".to_owned());
                }
                Some(stdin) => stdin
                    .write_all(chars.as_bytes())
                    .and_then(|()| stdin.flush()),
            };
            if written.is_err() {
                self.sessions.insert(id, session);
                return Ran::Failed("failed to write to stdin".to_owned());
            }
        }
        let requested = u64_field(input, "yield_time_ms", MIN_YIELD_TIME_MS);
        let yield_ms = if chars.is_empty() {
            requested.clamp(MIN_EMPTY_YIELD_TIME_MS, MAX_BACKGROUND_TERMINAL_TIMEOUT_MS)
        } else {
            requested.clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS)
        };
        let begun = Instant::now();
        let until = begun + Duration::from_millis(yield_ms);
        match Self::settle(&mut session, until, stop) {
            Ok(Settled::Running) => {
                let output = Self::drain(&mut session);
                self.sessions.insert(id, session);
                Ran::Ok(rendered(
                    begun.elapsed(),
                    None,
                    Some(id),
                    &output,
                    max_tokens,
                ))
            }
            Ok(settled) => self.finish(session, settled, begun, max_tokens),
            Err(error) => Ran::Failed(format!("Unified exec process failed: {error}")),
        }
    }
}

pub fn view_image(root: &Path, input: &Value) -> Ran {
    let path = text_field(input, "path");
    if path.is_empty() {
        return Ran::Failed("view_image needs the path of an image file".to_owned());
    }
    let bytes = match std::fs::read(resolved(root, path)) {
        Ok(bytes) => bytes,
        Err(error) => return Ran::Failed(format!("unable to read image at {path}: {error}")),
    };
    if tools::media_type(&bytes).is_none() {
        return Ran::Failed(format!(
            "{path} is not a PNG, JPEG, GIF or WebP image; convert or render it to PNG first"
        ));
    }
    if bytes.len() as u64 > tools::IMAGE_BYTES_MAX {
        return Ran::Failed(format!(
            "{path} is {} bytes, more than the {} an image may have; scale it down or crop it first",
            bytes.len(),
            tools::IMAGE_BYTES_MAX
        ));
    }
    Ran::Image(bytes)
}

pub fn update_plan(input: &Value) -> Ran {
    let plan = text_field(input, "plan");
    match Value::decode(plan) {
        Ok(Value::Array(steps)) => {
            let in_progress = steps
                .iter()
                .filter(|step| step.get("status").and_then(Value::as_str) == Some("in_progress"))
                .count();
            if in_progress > 1 {
                return Ran::Failed("At most one step can be in_progress at a time.".to_owned());
            }
            Ran::Ok("Plan updated".to_owned())
        }
        _ => Ran::Failed(format!(
            "failed to parse function arguments: plan is not a list of steps: {plan}"
        )),
    }
}

#[derive(Debug)]
struct Chunk {
    context: Option<String>,
    old: Vec<String>,
    new: Vec<String>,
    end_of_file: bool,
}

#[derive(Debug)]
enum Hunk {
    Add(String, String),
    Delete(String),
    Update(String, Option<String>, Vec<Chunk>),
}

fn hunk_error(line: usize, message: &str) -> String {
    format!("Invalid patch hunk on line {line}: {message}")
}

fn boundaries(patch: &str) -> Result<Vec<&str>, String> {
    let lines: Vec<&str> = patch.trim().lines().collect();
    let strict = |lines: &[&str]| -> Result<(), String> {
        match (
            lines.first().map(|line| line.trim()),
            lines.last().map(|line| line.trim()),
        ) {
            (Some(BEGIN_PATCH), Some(END_PATCH)) => Ok(()),
            (Some(first), _) if first != BEGIN_PATCH => Err(
                "Invalid patch: The first line of the patch must be '*** Begin Patch'".to_owned(),
            ),
            _ => {
                Err("Invalid patch: The last line of the patch must be '*** End Patch'".to_owned())
            }
        }
    };
    match strict(&lines) {
        Ok(()) => Ok(lines),
        Err(error) => {
            let wrapped = lines.len() >= HEREDOC_LINES_MIN
                && HEREDOC_OPENERS.contains(&lines[0])
                && lines[lines.len() - 1].ends_with("EOF");
            if !wrapped {
                return Err(error);
            }
            let inner = lines[1..lines.len() - 1].to_vec();
            strict(&inner)?;
            Ok(inner)
        }
    }
}

fn parse_patch(patch: &str) -> Result<Vec<Hunk>, String> {
    let lines = boundaries(patch)?;
    let body = &lines[1..lines.len() - 1];
    let mut hunks = Vec::new();
    let mut at = 0;
    while at < body.len() {
        let number = at + 2;
        let line = body[at].trim();
        if let Some(path) = line.strip_prefix(ADD_FILE) {
            let mut contents = String::new();
            at += 1;
            while at < body.len() && body[at].starts_with('+') {
                contents.push_str(&body[at][1..]);
                contents.push('\n');
                at += 1;
            }
            hunks.push(Hunk::Add(path.to_owned(), contents));
        } else if let Some(path) = line.strip_prefix(DELETE_FILE) {
            hunks.push(Hunk::Delete(path.to_owned()));
            at += 1;
        } else if let Some(path) = line.strip_prefix(UPDATE_FILE) {
            at += 1;
            let mut moved = None;
            if at < body.len()
                && let Some(to) = body[at].trim().strip_prefix(MOVE_TO)
            {
                moved = Some(to.to_owned());
                at += 1;
            }
            let mut chunks: Vec<Chunk> = Vec::new();
            while at < body.len() && !body[at].trim().starts_with("*** ")
                || at < body.len() && body[at].trim() == END_OF_FILE
            {
                let text = body[at];
                if text.trim() == END_OF_FILE {
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.end_of_file = true;
                    }
                    at += 1;
                    continue;
                }
                if text == EMPTY_CHANGE_CONTEXT || text.starts_with(CHANGE_CONTEXT) {
                    let context = text.strip_prefix(CHANGE_CONTEXT).map(str::to_owned);
                    chunks.push(Chunk {
                        context,
                        old: Vec::new(),
                        new: Vec::new(),
                        end_of_file: false,
                    });
                    at += 1;
                    continue;
                }
                if chunks.is_empty() {
                    chunks.push(Chunk {
                        context: None,
                        old: Vec::new(),
                        new: Vec::new(),
                        end_of_file: false,
                    });
                }
                let chunk = chunks.last_mut().expect("a chunk was just pushed");
                match text.chars().next() {
                    Some('+') => chunk.new.push(text[1..].to_owned()),
                    Some('-') => chunk.old.push(text[1..].to_owned()),
                    Some(' ') => {
                        chunk.old.push(text[1..].to_owned());
                        chunk.new.push(text[1..].to_owned());
                    }
                    None => {
                        chunk.old.push(String::new());
                        chunk.new.push(String::new());
                    }
                    Some(_) => {
                        return Err(hunk_error(
                            at + 2,
                            &format!(
                                "Unexpected line found in update hunk: '{text}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                            ),
                        ));
                    }
                }
                at += 1;
            }
            if chunks
                .iter()
                .all(|chunk| chunk.old.is_empty() && chunk.new.is_empty())
            {
                return Err(hunk_error(
                    number,
                    &format!("Update file hunk for path '{path}' is empty"),
                ));
            }
            hunks.push(Hunk::Update(path.to_owned(), moved, chunks));
        } else {
            return Err(hunk_error(
                number,
                &format!(
                    "'{line}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
                ),
            ));
        }
    }
    Ok(hunks)
}

fn normalise(text: &str) -> String {
    text.trim()
        .chars()
        .map(|c| match c {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

fn seek(lines: &[String], pattern: &[String], start: usize, end_of_file: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let from = if end_of_file {
        (lines.len() - pattern.len()).max(start)
    } else {
        start
    };
    let last = lines.len() - pattern.len();
    let passes: [&dyn Fn(&str, &str) -> bool; 4] = [
        &|a, b| a == b,
        &|a, b| a.trim_end() == b.trim_end(),
        &|a, b| a.trim() == b.trim(),
        &|a, b| normalise(a) == normalise(b),
    ];
    for same in passes {
        for at in from..=last {
            if pattern
                .iter()
                .enumerate()
                .all(|(offset, line)| same(&lines[at + offset], line))
            {
                return Some(at);
            }
        }
    }
    None
}

fn updated(original: &str, path: &str, chunks: &[Chunk]) -> Result<String, String> {
    let mut lines: Vec<String> = original.split('\n').map(str::to_owned).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut at = 0;
    for chunk in chunks {
        if let Some(context) = &chunk.context {
            match seek(&lines, std::slice::from_ref(context), at, false) {
                Some(found) => at = found + 1,
                None => return Err(format!("Failed to find context '{context}' in {path}")),
            }
        }
        if chunk.old.is_empty() {
            replacements.push((lines.len(), 0, chunk.new.clone()));
            continue;
        }
        let mut pattern: &[String] = &chunk.old;
        let mut new: &[String] = &chunk.new;
        let mut found = seek(&lines, pattern, at, chunk.end_of_file);
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new.last().is_some_and(String::is_empty) {
                new = &new[..new.len() - 1];
            }
            found = seek(&lines, pattern, at, chunk.end_of_file);
        }
        let Some(start) = found else {
            return Err(format!(
                "Failed to find expected lines in {path}:\n{}",
                chunk.old.join("\n")
            ));
        };
        replacements.push((start, pattern.len(), new.to_vec()));
        at = start + pattern.len();
    }
    replacements.sort_by_key(|(start, _, _)| *start);
    for (start, length, new) in replacements.into_iter().rev() {
        lines.splice(start..start + length, new);
    }
    let mut text = lines.join("\n");
    text.push('\n');
    Ok(text)
}

fn resolved(root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create parent directories for {}: {error}",
                path.display()
            )
        })?;
    }
    std::fs::write(path, contents)
        .map_err(|error| format!("Failed to write file {}: {error}", path.display()))
}

pub fn apply_patch(root: &Path, input: &Value) -> Ran {
    let patch = text_field(input, "input");
    let hunks = match parse_patch(patch) {
        Ok(hunks) => hunks,
        Err(error) => return Ran::Failed(format!("apply_patch verification failed: {error}")),
    };
    if hunks.is_empty() {
        return Ran::Failed("No files were modified.".to_owned());
    }
    let mut planned: Vec<(char, String, Option<PathBuf>, Option<String>)> = Vec::new();
    for hunk in &hunks {
        match hunk {
            Hunk::Add(path, contents) => {
                planned.push(('A', path.clone(), None, Some(contents.clone())));
            }
            Hunk::Delete(path) => {
                if !resolved(root, path).is_file() {
                    return Ran::Failed(format!(
                        "apply_patch verification failed: Failed to read {path}: No such file or directory"
                    ));
                }
                planned.push(('D', path.clone(), None, None));
            }
            Hunk::Update(path, moved, chunks) => {
                let original = match std::fs::read_to_string(resolved(root, path)) {
                    Ok(original) => original,
                    Err(error) => {
                        return Ran::Failed(format!(
                            "apply_patch verification failed: Failed to read file to update {path}: {error}"
                        ));
                    }
                };
                match updated(&original, path, chunks) {
                    Ok(contents) => planned.push((
                        'M',
                        moved.clone().unwrap_or_else(|| path.clone()),
                        moved.as_ref().map(|_| resolved(root, path)),
                        Some(contents),
                    )),
                    Err(error) => {
                        return Ran::Failed(format!("apply_patch verification failed: {error}"));
                    }
                }
            }
        }
    }
    let mut summary = String::from("Success. Updated the following files:\n");
    for (kind, path, source, contents) in &planned {
        let target = resolved(root, path);
        let done = match (kind, contents) {
            ('D', _) => std::fs::remove_file(&target)
                .map_err(|error| format!("Failed to delete file {}: {error}", target.display())),
            (_, Some(contents)) => write_file(&target, contents).and_then(|()| match source {
                Some(source) if *source != target => {
                    std::fs::remove_file(source).map_err(|error| {
                        format!("Failed to remove original {}: {error}", source.display())
                    })
                }
                _ => Ok(()),
            }),
            (_, None) => Ok(()),
        };
        if let Err(error) = done {
            return Ran::Failed(error);
        }
        summary.push_str(&format!("{kind} {path}\n"));
    }
    Ran::Ok(format!(
        "Exit code: 0\nWall time: 0 seconds\nOutput:\n{summary}"
    ))
}

pub fn patch_applied(root: &Path, input: &Value) -> Option<Ran> {
    let hunks = parse_patch(text_field(input, "input")).ok()?;
    let mut summary = String::from("Success. Updated the following files:\n");
    for hunk in &hunks {
        match hunk {
            Hunk::Add(path, contents) => {
                if std::fs::read_to_string(resolved(root, path)).ok()? != *contents {
                    return None;
                }
                summary.push_str(&format!("A {path}\n"));
            }
            Hunk::Delete(path) => {
                if resolved(root, path).exists() {
                    return None;
                }
                summary.push_str(&format!("D {path}\n"));
            }
            Hunk::Update(..) => return None,
        }
    }
    (!hunks.is_empty()).then(|| {
        Ran::Ok(format!(
            "Exit code: 0\nWall time: 0 seconds\nOutput:\n{summary}"
        ))
    })
}
