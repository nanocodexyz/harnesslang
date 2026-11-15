use std::path::{Path, PathBuf};

use super::sha1::sha1_hex;
use crate::ir::Program;
use crate::run::blob::sha256_hex;
use crate::target::Target;

use super::{AUTH_VARIABLE, Plant};

pub const BUNDLE_FILES_MAX: usize = 15_000;
pub const BUNDLE_BYTES_MAX: u64 = 100 * 1024 * 1024;
pub const MANIFEST_FILE: &str = "hl-bundle.json";
pub const CLOUDFLARE_MAIN: &str = "worker.mjs";
pub const CLOUDFLARE_AUTH_MAIN: &str = "hl-auth.mjs";
pub const VERCEL_HARNESS: &str = "workflows/harness.mjs";
pub const VERCEL_WORKFLOW: &str = "workflows/session.mjs";
pub const VERCEL_ROUTE: &str = "app/sessions/[session]/[kind]/route.mjs";
pub const NEXT_VERSION: &str = "16.4.6";
pub const REACT_VERSION: &str = "19.3.1";
pub const WORKFLOW_VERSION: &str = "5.5.0";

pub struct File {
    pub path: String,
    pub bytes: Vec<u8>,
    pub sha1: String,
}

pub enum Origin {
    Emitted,
    Directory(PathBuf),
}

pub struct Bundle {
    pub main: String,
    pub files: Vec<File>,
    pub digest: String,
    pub origin: Origin,
}

impl Bundle {
    fn new(main: &str, mut files: Vec<File>, origin: Origin) -> Result<Self, String> {
        files.sort_by(|left, right| left.path.cmp(&right.path));
        for pair in files.windows(2) {
            assert_ne!(pair[0].path, pair[1].path, "a bundle names each file once");
        }
        if !files.iter().any(|file| file.path == main) {
            return Err(format!("the bundle has no main module {main}"));
        }
        let mut listing = String::new();
        for file in &files {
            listing.push_str(&format!(
                "{} {} {}\n",
                file.sha1,
                file.bytes.len(),
                file.path
            ));
        }
        Ok(Self {
            main: main.to_owned(),
            digest: sha256_hex(listing.as_bytes()),
            files,
            origin,
        })
    }

    pub fn bytes(&self) -> u64 {
        self.files
            .iter()
            .map(|file| u64::try_from(file.bytes.len()).expect("fits u64"))
            .sum()
    }

    pub fn describe(&self) -> String {
        let origin = match &self.origin {
            Origin::Emitted => "hl emit".to_owned(),
            Origin::Directory(path) => path.display().to_string(),
        };
        let files = match self.files.len() {
            1 => "1 file".to_owned(),
            count => format!("{count} files"),
        };
        format!(
            "{files}, {} bytes, digest {} (from {origin})",
            self.bytes(),
            &self.digest[..DIGEST_SHOWN]
        )
    }

    pub fn write(&self, directory: &Path) -> Result<(), String> {
        for file in &self.files {
            let path = directory.join(&file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
            }
            std::fs::write(&path, &file.bytes)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        }
        Ok(())
    }
}

pub const DIGEST_SHOWN: usize = 12;

pub fn file(path: &str, bytes: Vec<u8>) -> File {
    File {
        path: path.to_owned(),
        sha1: sha1_hex(&bytes),
        bytes,
    }
}

pub fn emitted(program: &Program, target: Target, plant: Plant) -> Result<Bundle, String> {
    let code = crate::emit::emit(program, target)?;
    match target {
        Target::Cloudflare => Bundle::new(
            CLOUDFLARE_AUTH_MAIN,
            vec![
                file(CLOUDFLARE_MAIN, code.into_bytes()),
                file(CLOUDFLARE_AUTH_MAIN, cloudflare_auth(plant).into_bytes()),
            ],
            Origin::Emitted,
        ),
        Target::Vercel => Bundle::new(
            VERCEL_ROUTE,
            vercel_files(program, code, plant),
            Origin::Emitted,
        ),
        _ => Bundle::new(
            "harness.rs",
            vec![file("harness.rs", code.into_bytes())],
            Origin::Emitted,
        ),
    }
}

fn auth_check(plant: Plant) -> String {
    let refused = if plant == Plant::AuthUnchecked {
        "function refused(token, request) {\n  return null;\n}\n".to_owned()
    } else {
        format!(
            "function refused(token, request) {{\n  if (typeof token !== \"string\" || token.length === 0) {{\n    return new Response(\"{AUTH_VARIABLE} is not set: hl secrets set {AUTH_VARIABLE}\", {{ status: 503 }});\n  }}\n  if (!sameText(request.headers.get(\"authorization\") ?? \"\", \"Bearer \" + token)) {{\n    return new Response(\"unauthorized\", {{ status: 401, headers: {{ \"www-authenticate\": \"Bearer\" }} }});\n  }}\n  return null;\n}}\n"
        )
    };
    format!(
        "const authEncoder = new TextEncoder();\n\nfunction sameText(given, expected) {{\n  const left = authEncoder.encode(given);\n  const right = authEncoder.encode(expected);\n  let differ = left.length ^ right.length;\n  for (let index = 0; index < right.length; index += 1) differ |= (left[index] ?? 0) ^ right[index];\n  return differ === 0;\n}}\n\n{refused}"
    )
}

fn cloudflare_auth(plant: Plant) -> String {
    format!(
        "import worker from \"./{CLOUDFLARE_MAIN}\";\nexport * from \"./{CLOUDFLARE_MAIN}\";\n\n{}\nexport default {{\n  ...worker,\n  async fetch(request, env, context) {{\n    return refused(env.{AUTH_VARIABLE}, request) ?? worker.fetch(request, env, context);\n  }},\n}};\n",
        auth_check(plant)
    )
}

fn vercel_files(program: &Program, harness: String, plant: Plant) -> Vec<File> {
    let package = format!(
        "{{\n  \"name\": \"{}\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"scripts\": {{ \"build\": \"next build\" }},\n  \"dependencies\": {{\n    \"next\": \"{NEXT_VERSION}\",\n    \"react\": \"{REACT_VERSION}\",\n    \"react-dom\": \"{REACT_VERSION}\",\n    \"workflow\": \"{WORKFLOW_VERSION}\"\n  }}\n}}\n",
        super::default_name(&program.name)
    );
    let config =
        "import { withWorkflow } from \"workflow/next\";\n\nexport default withWorkflow({});\n";
    let route = format!(
        "import {{ start, resumeHook }} from \"workflow/api\";\nimport {{ harnessSession }} from \"../../../../workflows/session.mjs\";\n\n{}\nexport async function POST(request, {{ params }}) {{\n  const denied = refused(process.env.{AUTH_VARIABLE}, request);\n  if (denied) return denied;\n  const {{ session, kind }} = await params;\n  const body = await request.json();\n  if (kind === \"start\") {{\n    const run = await start(harnessSession, [session, body.turnsMax, body.entry ?? null]);\n    return Response.json({{ runId: run.runId }});\n  }}\n  if (kind === \"hook\") {{\n    await resumeHook(body.token, body.payload);\n    return Response.json({{ resumed: body.token }});\n  }}\n  return new Response(\"not found\", {{ status: 404 }});\n}}\n",
        auth_check(plant)
    );
    vec![
        file("package.json", package.into_bytes()),
        file("next.config.mjs", config.as_bytes().to_vec()),
        file(VERCEL_HARNESS, harness.into_bytes()),
        file(
            VERCEL_WORKFLOW,
            crate::emit::js::workflow_binding().into_bytes(),
        ),
        file(VERCEL_ROUTE, route.into_bytes()),
    ]
}

pub fn directory(root: &Path, target: Target) -> Result<Bundle, String> {
    let mut files = Vec::new();
    let mut pending = vec![PathBuf::new()];
    let mut bytes_total: u64 = 0;
    while let Some(relative) = pending.pop() {
        let here = root.join(&relative);
        let mut entries: Vec<_> = std::fs::read_dir(&here)
            .map_err(|error| format!("cannot read {}: {error}", here.display()))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<_, _>>()
            .map_err(|error| format!("cannot read {}: {error}", here.display()))?;
        entries.sort();
        for name in entries {
            let path = relative.join(&name);
            let full = root.join(&path);
            let kind = std::fs::symlink_metadata(&full)
                .map_err(|error| format!("cannot stat {}: {error}", full.display()))?;
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if !kind.is_file() {
                return Err(format!(
                    "{} is neither a file nor a directory",
                    full.display()
                ));
            }
            let shown = path
                .to_str()
                .ok_or_else(|| format!("{} is not a UTF-8 path", full.display()))?
                .to_owned();
            if shown == MANIFEST_FILE {
                continue;
            }
            if files.len() == BUNDLE_FILES_MAX {
                return Err(format!(
                    "{} holds more than bundle_files_max ({BUNDLE_FILES_MAX}) files, Vercel's limit on one deployment",
                    root.display()
                ));
            }
            bytes_total += kind.len();
            if bytes_total > BUNDLE_BYTES_MAX {
                return Err(format!(
                    "{} holds more than bundle_bytes_max ({BUNDLE_BYTES_MAX}) bytes, Vercel's limit on a Hobby deployment's source upload",
                    root.display()
                ));
            }
            let bytes = std::fs::read(&full)
                .map_err(|error| format!("cannot read {}: {error}", full.display()))?;
            files.push(file(&shown, bytes));
        }
    }
    let main = manifest_main(root, target)?;
    Bundle::new(&main, files, Origin::Directory(root.to_path_buf()))
}

fn manifest_main(root: &Path, target: Target) -> Result<String, String> {
    let manifest = root.join(MANIFEST_FILE);
    let fallback = match target {
        Target::Vercel => VERCEL_ROUTE,
        Target::Cloudflare => CLOUDFLARE_MAIN,
        _ => "harness.rs",
    };
    let text = match std::fs::read_to_string(&manifest) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(fallback.to_owned());
        }
        Err(error) => return Err(format!("cannot read {}: {error}", manifest.display())),
    };
    let value =
        super::json::decode(&text).map_err(|error| format!("{}: {error}", manifest.display()))?;
    match value.get("main") {
        None => Ok(fallback.to_owned()),
        Some(main) => main
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("{}: main is not a string", manifest.display())),
    }
}

pub fn cloudflare_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("mjs" | "js") => "application/javascript+module",
        Some("wasm") => "application/wasm",
        Some("txt" | "json" | "md" | "html" | "css") => "text/plain",
        _ => "application/octet-stream",
    }
}
