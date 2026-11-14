use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::json::Value;

pub const COMMAND_BYTES_MAX: usize = 1024 * 1024;
pub const DESCRIPTION_BYTES_MAX: usize = 1024;
pub const STREAM_BYTES_MAX: usize = 30_000;
pub const TIMEOUT_MAX_MS: u64 = 600_000;
pub const ERROR_CHARS: usize = 10_000;
pub const ERROR_CHARS_SLACK: usize = 1_024;
pub const SHELL_SIGNAL_STATUS: i32 = 128;
pub const TIMED_OUT_STATUS: i32 = SHELL_SIGNAL_STATUS + 15;
pub const READ_BYTES_MAX: u64 = 262_144;
pub const READ_TOKENS_MAX: usize = 25_000;
pub const BYTES_PER_TOKEN: usize = 4;
pub const SELECTED_BYTES_PER_TOKEN: u64 = 128;
pub const PIPE_CHUNK_BYTES: usize = 8192;
pub const MILLISECONDS_PER_MINUTE: u128 = 60_000;
pub const MILLISECONDS_PER_SECOND: u128 = 1_000;
pub const PAGE_FILL: f64 = 0.85;
pub const PAGE_SHRINK: f64 = 0.7;
pub const PAGE_SHRINKS_MAX: usize = 6;
pub const BYTES_PER_TOKEN_MIN: f64 = 0.5;
pub const TIMEOUT_POLL: Duration = Duration::from_millis(10);
pub const SIMILAR_EXTENSION_CHARS_MAX: usize = 32;

const SHELL: &str = "bash";
const SHELL_FALLBACK: &str = "/bin/sh";
const BASH_WRAPPER: &str = "__hl_state=\"$1\"; shift; trap 'pwd -P > \"$__hl_state-cwd.new\" && mv \"$__hl_state-cwd.new\" \"$__hl_state-cwd\"' EXIT; . \"$__hl_state-command\"";
const TRUNCATED: &str = "[Truncated: PARTIAL view \u{2014} ";
const ALLOW_LARGE_UNAVAILABLE: &str =
    " allow_large is not available where this read runs; read it in parts with offset and limit.";
const FILE_STATE: &str =
    " (file state is current in your context \u{2014} no need to Read it back)";
const UNKNOWN_OUTCOME: &str = "Tool execution interrupted by a host crash; outcome unknown. Do not assume it did not run or automatically repeat it.";
const BINARY_EXTENSIONS: [&str; 87] = [
    ".ico", ".tiff", ".tif", ".mp4", ".mov", ".avi", ".mkv", ".webm", ".wmv", ".flv", ".m4v",
    ".mpeg", ".mpg", ".mp3", ".wav", ".ogg", ".flac", ".aac", ".m4a", ".wma", ".aiff", ".opus",
    ".zip", ".tar", ".gz", ".bz2", ".7z", ".rar", ".xz", ".z", ".tgz", ".iso", ".exe", ".dll",
    ".so", ".dylib", ".bin", ".o", ".a", ".obj", ".lib", ".app", ".msi", ".deb", ".rpm", ".doc",
    ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".odt", ".ods", ".odp", ".ttf", ".otf", ".woff",
    ".woff2", ".eot", ".pyc", ".pyo", ".class", ".jar", ".war", ".ear", ".node", ".wasm", ".rlib",
    ".sqlite", ".sqlite3", ".db", ".mdb", ".idx", ".psd", ".ai", ".eps", ".sketch", ".fig", ".xd",
    ".blend", ".3ds", ".max", ".swf", ".fla", ".lockb", ".dat", ".data",
];
const MEDIA_EXTENSIONS: [&str; 6] = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".pdf"];
const IMAGE_EXTENSIONS: [&str; 5] = [".png", ".jpg", ".jpeg", ".gif", ".webp"];
const IMAGE_TOO_LARGE: &str =
    "image content omitted because it exceeded the supported size limit; use a smaller image";
const IMAGE_UNPROCESSABLE: &str = "image content omitted because it could not be processed";
const PNG_SIGNATURE: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
const JPEG_SIGNATURE: &[u8] = &[0xff, 0xd8, 0xff];
const GIF_SIGNATURES: [&[u8]; 2] = [b"GIF87a", b"GIF89a"];
const RIFF_SIGNATURE: &[u8] = b"RIFF";
const WEBP_SIGNATURE: &[u8] = b"WEBP";
const WEBP_SIGNATURE_AT: usize = 8;

pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(PNG_SIGNATURE) {
        return Some("image/png");
    }
    if bytes.starts_with(JPEG_SIGNATURE) {
        return Some("image/jpeg");
    }
    if GIF_SIGNATURES
        .iter()
        .any(|signature| bytes.starts_with(signature))
    {
        return Some("image/gif");
    }
    let webp = bytes.starts_with(RIFF_SIGNATURE)
        && bytes.get(WEBP_SIGNATURE_AT..WEBP_SIGNATURE_AT + WEBP_SIGNATURE.len())
            == Some(WEBP_SIGNATURE);
    webp.then_some("image/webp")
}

pub fn unknown_outcome() -> &'static str {
    UNKNOWN_OUTCOME
}

pub fn unavailable(name: &str, offered: &[&str]) -> String {
    let hint = match name {
        "Grep" => ". Grep is not available in this session \u{2014} search file contents with `grep` via the Bash tool instead.".to_owned(),
        "Glob" => ". Glob is not available in this session \u{2014} find files with `find` via the Bash tool instead.".to_owned(),
        _ => match offered.iter().filter(|tool| tool.eq_ignore_ascii_case(name)).collect::<Vec<_>>()[..] {
            [tool] => format!(". Tool names are case-sensitive: call {tool} instead."),
            _ => String::new(),
        },
    };
    format!("<tool_use_error>Error: No such tool available: {name}{hint}</tool_use_error>")
}

pub struct Workspace {
    pub root: PathBuf,
    pub state: PathBuf,
    pub image_bytes_max: Option<u64>,
}

pub enum Ran {
    Ok(String),
    Image(Vec<u8>),
    Failed(String),
}

fn text_field<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing or invalid {key}"))
}

fn u64_field(input: &Value, key: &str) -> Result<u64, String> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("invalid {key}"))
}

fn bool_field(input: &Value, key: &str) -> Result<bool, String> {
    match input.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(format!("invalid {key}")),
    }
}

impl Workspace {
    pub fn run(&self, tool: &str, input: &Value) -> Ran {
        let outcome = match tool {
            "Bash" => self.bash(input),
            "Read" => self.read(input),
            "Edit" => self.edit(input).map(Ran::Ok),
            "Write" => self.write(input).map(Ran::Ok),
            other => Ok(Ran::Failed(unavailable(
                other,
                &["Read", "Edit", "Write", "Bash"],
            ))),
        };
        match outcome {
            Ok(ran) => ran,
            Err(error) => Ran::Failed(format!("<tool_use_error>{error}</tool_use_error>")),
        }
    }

    fn resolve(&self, text: &str) -> Result<PathBuf, String> {
        if text.is_empty() {
            return Err("file path is empty".into());
        }
        Ok(self.root.join(text))
    }

    fn missing(&self, path: &Path) -> String {
        let mut text = format!(
            "File does not exist. Note: your current working directory is {}.",
            self.root.display()
        );
        if let Some(similar) = similar(path) {
            text.push_str(&format!(" Did you mean {similar}?"));
        }
        text
    }

    fn read(&self, input: &Value) -> Result<Ran, String> {
        let file_path = text_field(input, "file_path")?;
        if let Some(error) = binary(file_path) {
            return Err(error);
        }
        let path = self.resolve(file_path)?;
        match std::fs::canonicalize(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Ran::Failed(self.missing(&path)));
            }
            Err(error) => return Err(format!("path: {error}")),
            Ok(real) if real.is_dir() => {
                return Ok(Ran::Failed(format!(
                    "EISDIR: illegal operation on a directory, read '{}'",
                    path.display()
                )));
            }
            Ok(real) if !real.is_file() => return Err("path is not a regular file".into()),
            Ok(_) => {}
        }
        let image = IMAGE_EXTENSIONS
            .iter()
            .any(|extension| file_path.to_ascii_lowercase().ends_with(*extension));
        if let (true, Some(bytes_max)) = (image, self.image_bytes_max) {
            return self.read_image(&path, bytes_max);
        }
        if let Some(extension) = MEDIA_EXTENSIONS
            .iter()
            .find(|extension| file_path.to_ascii_lowercase().ends_with(*extension))
        {
            return Err(format!(
                "this harness reads text only and cannot return {extension} files as images or documents"
            ));
        }
        if !text_field(input, "pages")?.is_empty() {
            return Err("pages is only applicable to PDF files".into());
        }
        let bytes = std::fs::read(&path).map_err(|error| format!("read: {error}"))?;
        let text = String::from_utf8(bytes).map_err(|error| format!("read: {error}"))?;
        read_view(file_path, &text, input).map(|view| match view {
            Ok(text) => Ran::Ok(text),
            Err(text) => Ran::Failed(text),
        })
    }

    fn read_image(&self, path: &Path, bytes_max: u64) -> Result<Ran, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("read: {error}"))?;
        if media_type(&bytes).is_none() {
            return Ok(Ran::Failed(IMAGE_UNPROCESSABLE.to_owned()));
        }
        if bytes.len() as u64 > bytes_max {
            return Ok(Ran::Failed(IMAGE_TOO_LARGE.to_owned()));
        }
        Ok(Ran::Image(bytes))
    }

    fn write_target(&self, text: &str) -> Result<PathBuf, String> {
        let path = self.resolve(text)?;
        if path.is_dir() {
            return Err("target is not a regular file".into());
        }
        let parent = path.parent().ok_or("file has no parent")?;
        std::fs::create_dir_all(parent).map_err(|error| format!("create directory: {error}"))?;
        Ok(path)
    }

    fn write(&self, input: &Value) -> Result<String, String> {
        let content = text_field(input, "content")?;
        let path = self.write_target(text_field(input, "file_path")?)?;
        let existed = path.exists();
        atomic_write(&path, content)?;
        let shown = path.display().to_string();
        Ok(if existed {
            format!("The file {shown} has been updated successfully.{FILE_STATE}")
        } else {
            format!("File created successfully at: {shown}{FILE_STATE}")
        })
    }

    fn edit(&self, input: &Value) -> Result<String, String> {
        let file_path = text_field(input, "file_path")?;
        let old = text_field(input, "old_string")?;
        let new = text_field(input, "new_string")?;
        let replace_all = bool_field(input, "replace_all")?;
        let path = self.resolve(file_path)?;
        let content = match std::fs::read_to_string(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !old.is_empty() {
                    return Err(self.missing(&path));
                }
                String::new()
            }
            Err(_) if path.is_dir() => {
                return Err(
                    "Error calling tool (Edit): EISDIR: illegal operation on a directory, read"
                        .into(),
                );
            }
            Err(error) => return Err(format!("read: {error}")),
            Ok(content) => content,
        };
        let updated = apply_edit(&content, old, new, replace_all)?;
        let path = self.write_target(file_path)?;
        atomic_write(&path, &updated)?;
        let shown = path.display();
        Ok(if replace_all {
            format!(
                "The file {shown} has been updated. All occurrences were successfully replaced."
            )
        } else {
            format!("The file {shown} has been updated successfully.")
        })
    }

    fn bash(&self, input: &Value) -> Result<Ran, String> {
        if bool_field(input, "dangerouslyDisableSandbox")? {
            return Err("this host does not authorize disabling the sandbox".into());
        }
        if bool_field(input, "run_in_background")? {
            return Err("this executor does not support background commands".into());
        }
        let command = text_field(input, "command")?;
        if command.trim().is_empty() || command.len() > COMMAND_BYTES_MAX {
            return Err(format!(
                "command must be nonblank and at most {COMMAND_BYTES_MAX} bytes"
            ));
        }
        if text_field(input, "description")?.len() > DESCRIPTION_BYTES_MAX {
            return Err(format!("description exceeds {DESCRIPTION_BYTES_MAX} bytes"));
        }
        let requested = u64_field(input, "timeout")?;
        if requested == 0 {
            return Err("timeout must be at least 1 millisecond".into());
        }
        let timeout = Duration::from_millis(requested.min(TIMEOUT_MAX_MS));
        let result = self
            .execute(command, timeout)
            .map_err(|error| format!("command executor: {error}"))?;
        Ok(rendered(&result))
    }

    fn cwd(&self) -> PathBuf {
        match std::fs::read_to_string(self.state.join("bash-cwd")) {
            Ok(text) if Path::new(text.trim_end()).is_dir() => PathBuf::from(text.trim_end()),
            _ => self.root.clone(),
        }
    }

    fn execute(&self, command: &str, timeout: Duration) -> Result<Shell, String> {
        let script = self.state.join("bash-command");
        std::fs::write(&script, command).map_err(|error| error.to_string())?;
        let spawn = |shell: &str| {
            Command::new(shell)
                .arg("-c")
                .arg(BASH_WRAPPER)
                .arg(shell)
                .arg(self.state.join("bash"))
                .current_dir(self.cwd())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
        };
        let mut child = match spawn(SHELL) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => spawn(SHELL_FALLBACK),
            spawned => spawned,
        }
        .map_err(|error| error.to_string())?;
        let stdout = capture(child.stdout.take().expect("piped stdout"));
        let stderr = capture(child.stderr.take().expect("piped stderr"));
        let started = Instant::now();
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                break status;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                let _ = Command::new("kill")
                    .arg("-KILL")
                    .arg("--")
                    .arg(format!("-{}", child.id()))
                    .status();
                break child.wait().map_err(|error| error.to_string())?;
            }
            std::thread::sleep(TIMEOUT_POLL);
        };
        let (stdout, stdout_cut) = stdout.join().expect("stdout reader");
        let (stderr, stderr_cut) = stderr.join().expect("stderr reader");
        let code = match (timed_out, status.code()) {
            (true, _) => TIMED_OUT_STATUS,
            (false, Some(code)) => code,
            (false, None) => {
                SHELL_SIGNAL_STATUS
                    + std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(0)
            }
        };
        let message =
            timed_out.then(|| format!("Command timed out after {}", timed_out_after(timeout)));
        Ok(Shell {
            stdout,
            stdout_cut,
            stderr,
            stderr_cut,
            code,
            message,
        })
    }
}

use std::os::unix::process::CommandExt as _;

struct Shell {
    stdout: String,
    stdout_cut: bool,
    stderr: String,
    stderr_cut: bool,
    code: i32,
    message: Option<String>,
}

fn capture(
    mut stream: impl std::io::Read + Send + 'static,
) -> std::thread::JoinHandle<(String, bool)> {
    std::thread::spawn(move || {
        let mut kept = Vec::with_capacity(STREAM_BYTES_MAX);
        let mut buffer = [0_u8; PIPE_CHUNK_BYTES];
        let mut cut = false;
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let room = STREAM_BYTES_MAX - kept.len();
                    kept.extend_from_slice(&buffer[..count.min(room)]);
                    cut |= count > room;
                }
            }
        }
        let mut text = String::from_utf8_lossy(&kept).into_owned();
        if cut {
            let end = text.floor_char_boundary(STREAM_BYTES_MAX);
            text.truncate(end);
        }
        (text, cut)
    })
}

fn timed_out_after(timeout: Duration) -> String {
    let milliseconds = timeout.as_millis();
    if milliseconds < MILLISECONDS_PER_MINUTE {
        return format!("{}s", milliseconds / MILLISECONDS_PER_SECOND);
    }
    let minutes = milliseconds / MILLISECONDS_PER_MINUTE;
    let seconds = (milliseconds % MILLISECONDS_PER_MINUTE + MILLISECONDS_PER_SECOND / 2)
        / MILLISECONDS_PER_SECOND;
    if seconds * MILLISECONDS_PER_SECOND == MILLISECONDS_PER_MINUTE {
        format!("{}m 0s", minutes + 1)
    } else {
        format!("{minutes}m {seconds}s")
    }
}

fn with_cut_notice(stream: &str, text: &str, cut: bool) -> String {
    if !cut {
        return text.to_owned();
    }
    let line = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!(
        "{text}{line}[{stream} cut after its first {} bytes: the rest was not kept, so send the command's output to a file and read it in parts]\n",
        text.len()
    )
}

fn without_empty_lines(text: &str) -> String {
    let lines = text.split('\n').collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    lines[start..].join("\n").trim_end().to_owned()
}

fn rendered(shell: &Shell) -> Ran {
    let parts = [
        (shell.code != 0).then(|| format!("Exit code {}", shell.code)),
        Some(without_empty_lines(&with_cut_notice(
            "stdout",
            &shell.stdout,
            shell.stdout_cut,
        ))),
        Some(without_empty_lines(&with_cut_notice(
            "stderr",
            &shell.stderr,
            shell.stderr_cut,
        ))),
        shell.message.clone(),
    ];
    let text = parts
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    match (shell.code, text.is_empty()) {
        (0, true) => Ran::Ok("(Bash completed with no output)".to_owned()),
        (0, false) => Ran::Ok(text),
        _ => Ran::Failed(truncated_error(text)),
    }
}

fn truncated_error(text: String) -> String {
    let units = text.encode_utf16().count();
    if units <= ERROR_CHARS + ERROR_CHARS_SLACK {
        return text;
    }
    let mut head = (0, 0);
    for (at, c) in text.char_indices() {
        if head.1 + c.len_utf16() > ERROR_CHARS / 2 {
            break;
        }
        head = (at + c.len_utf8(), head.1 + c.len_utf16());
    }
    let mut tail = (text.len(), 0);
    for (at, c) in text.char_indices().rev() {
        if tail.1 + c.len_utf16() > ERROR_CHARS - ERROR_CHARS / 2 || at < head.0 {
            break;
        }
        tail = (at, tail.1 + c.len_utf16());
    }
    format!(
        "{}\n\n... [{} characters truncated] ...\n\n{}",
        &text[..head.0],
        units - head.1 - tail.1,
        &text[tail.0..]
    )
}

fn binary(file_path: &str) -> Option<String> {
    let extension = file_path[file_path.rfind('.')?..].to_ascii_lowercase();
    BINARY_EXTENSIONS.contains(&extension.as_str()).then(|| {
        format!("This tool cannot read binary files. The file appears to be a binary {extension} file. Use a skill for this file type if one is available, or a shell command or script that can read the format.")
    })
}

fn similar(path: &Path) -> Option<String> {
    let name = path.file_name()?;
    let stem = path.file_stem()?;
    let parent = path.parent()?;
    let mut entries: Vec<_> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    entries.sort();
    entries
        .into_iter()
        .find(|entry| {
            let entry = Path::new(entry);
            entry.as_os_str() != name
                && entry.file_stem() == Some(stem)
                && entry.extension().is_none_or(|extension| {
                    extension.to_str().is_some_and(|extension| {
                        (1..=SIMILAR_EXTENSION_CHARS_MAX).contains(&extension.chars().count())
                    })
                })
        })
        .map(|entry| parent.join(entry).display().to_string())
}

fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .ok_or("file has no name")?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.hl-agent-write"));
    std::fs::write(&temporary, content).map_err(|error| format!("write: {error}"))?;
    if let Ok(metadata) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&temporary, metadata.permissions());
    }
    std::fs::rename(&temporary, path).map_err(|error| format!("write: {error}"))
}

pub fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, String> {
    if old.is_empty() {
        if !content.trim().is_empty() {
            return Err("Cannot create new file - file already exists.".into());
        }
        return Ok(new.to_owned());
    }
    if old == new {
        return Err("No changes to make: old_string and new_string are exactly the same.".into());
    }
    let count = content.matches(old).count();
    if count == 0 {
        return Err(format!(
            "String to replace not found in file.\nString: {old}"
        ));
    }
    if count != 1 && !replace_all {
        return Err(format!(
            "Found {count} matches of the string to replace, but replace_all is false. To replace all occurrences, set replace_all to true. To replace only one occurrence, please provide more context to uniquely identify the instance.\nString: {old}"
        ));
    }
    Ok(if replace_all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    })
}

fn size_text(bytes: u64) -> String {
    let trimmed = |value: f64| format!("{value:.1}").trim_end_matches(".0").to_owned();
    let kilobytes = bytes as f64 / 1024.0;
    if kilobytes < 1.0 {
        return format!("{bytes} bytes");
    }
    if kilobytes < 1024.0 {
        return format!("{}KB", trimmed(kilobytes));
    }
    format!("{}MB", trimmed(kilobytes / 1024.0))
}

fn read_view(file_path: &str, text: &str, input: &Value) -> Result<Result<String, String>, String> {
    let offset = u64_field(input, "offset")?;
    let limit = Some(u64_field(input, "limit")?).filter(|limit| *limit > 0);
    let hint = if bool_field(input, "allow_large")? {
        ALLOW_LARGE_UNAVAILABLE
    } else {
        ""
    };
    let size = text.len() as u64;
    if limit.is_none() && size > READ_BYTES_MAX {
        return Ok(Err(format!(
            "File content ({}) exceeds maximum allowed size ({}). Use offset and limit parameters to read specific portions of the file, or search for specific content instead of reading the whole file.{hint}",
            size_text(size),
            size_text(READ_BYTES_MAX)
        )));
    }
    let selected_max = READ_TOKENS_MAX as u64 * SELECTED_BYTES_PER_TOKEN;
    let start = offset.saturating_sub(1);
    let end = limit.map(|limit| start.saturating_add(limit));
    let mut all: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        all.pop();
        all.push("");
    }
    if text.is_empty() {
        all.clear();
    }
    let all: Vec<&str> = all
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                line.strip_prefix('\u{feff}').unwrap_or(line)
            } else {
                line
            }
        })
        .collect();
    let total = all.len() as u64;
    let mut lines = Vec::new();
    let mut selected = 0_u64;
    for (index, line) in all.iter().enumerate() {
        let index = index as u64;
        if index >= start && end.is_none_or(|end| index < end) {
            selected += line.len() as u64 + u64::from(!lines.is_empty());
            if limit.is_some() && selected > selected_max {
                return Ok(Err(format!(
                    "The requested line range contains over {} of text, more than a read can return. Use a smaller limit \u{2014} or, if a single line is this large, no limit will fit it: search for specific content instead.{hint}",
                    size_text(selected_max)
                )));
            }
            lines.push(*line);
        }
    }
    let first = offset.max(1);
    let content = lines.join("\n");
    let tokens = content.len().div_ceil(BYTES_PER_TOKEN);
    let mut shown = lines.len();
    let mut page = content.clone();
    let mut banner = None;
    if tokens > READ_TOKENS_MAX {
        if offset > 1 || limit.is_some() {
            return Ok(Err(format!(
                "File content ({tokens} tokens) exceeds maximum allowed tokens ({READ_TOKENS_MAX}). Use offset and limit parameters to read specific portions of the file, or search for specific content instead of reading the whole file.{hint}"
            )));
        }
        let (kept, count) = first_page(&lines, &content, tokens);
        page = kept;
        shown = count;
        banner = Some(format!(
            "{TRUNCATED}{file_path}: showing lines 1-{count} of {total} total ({tokens} tokens, cap {READ_TOKENS_MAX}). Call Read with offset={} limit={count} for the next page, or Grep to find a specific section.{hint} Do NOT answer from this page alone if the answer may be further in the file.]",
            count + 1
        ));
    }
    let mut out = if !page.is_empty() || (shown >= 1 && total > 1) {
        page.split('\n')
            .enumerate()
            .map(|(index, line)| format!("{}\t{line}", first + index as u64))
            .collect::<Vec<_>>()
            .join("\n")
    } else if shown >= 1 || total == 0 {
        "<system-reminder>Warning: the file exists but the contents are empty.</system-reminder>"
            .to_owned()
    } else {
        format!(
            "<system-reminder>Warning: the file exists but is shorter than the provided offset ({first}). The file has {total} lines.</system-reminder>"
        )
    };
    if let Some(banner) = banner {
        out.push_str(&format!("\n<system-reminder>{banner}</system-reminder>"));
    }
    Ok(Ok(out))
}

fn first_page(lines: &[&str], content: &str, tokens: usize) -> (String, usize) {
    let cap = READ_TOKENS_MAX;
    let per_token = (content.len() as f64 / tokens.max(1) as f64).max(BYTES_PER_TOKEN_MIN);
    let estimate = |text: &str| text.len() as f64 / per_token;
    let mut count = ((lines.len() as f64 * cap as f64 / tokens.max(1) as f64 * PAGE_FILL).floor()
        as usize)
        .clamp(1, lines.len().max(1));
    let mut page = lines[..count.min(lines.len())].join("\n");
    for _ in 0..PAGE_SHRINKS_MAX {
        if estimate(&page) <= cap as f64 || count <= 1 {
            break;
        }
        count = ((count as f64 * PAGE_SHRINK).floor() as usize).max(1);
        page = lines[..count].join("\n");
    }
    (page, count)
}
