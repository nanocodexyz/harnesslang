pub mod bundle;
pub mod cloudflare;
pub mod http;
pub mod json;
pub mod local;
pub mod multipart;
pub mod sha1;
pub mod vercel;
pub mod websocket;

use std::path::PathBuf;
use std::process::ExitCode;

use crate::ir::Program;
use crate::target::Target;

pub const CLOUDFLARE_TOKEN_VARIABLE: &str = "CLOUDFLARE_API_TOKEN";
pub const CLOUDFLARE_ACCOUNT_VARIABLE: &str = "CLOUDFLARE_ACCOUNT_ID";
pub const CLOUDFLARE_API_VARIABLE: &str = "HL_CLOUDFLARE_API";
pub const VERCEL_TOKEN_VARIABLE: &str = "VERCEL_TOKEN";
pub const VERCEL_TEAM_VARIABLE: &str = "VERCEL_TEAM_ID";
pub const VERCEL_API_VARIABLE: &str = "HL_VERCEL_API";
pub const CERTIFICATE_VARIABLE: &str = "HL_DEPLOY_CA";
pub const CLOUDFLARE_API_DEFAULT: &str = "https://api.cloudflare.com/client/v4";
pub const VERCEL_API_DEFAULT: &str = "https://api.vercel.com";
pub const USER_AGENT: &str = concat!("harnesslang-hl/", env!("CARGO_PKG_VERSION"));
pub const NAME_BYTES_MAX: usize = 63;
pub const VARIABLES_MAX: usize = 64;
pub const SECRET_BYTES_MAX: usize = 5 * 1024;
pub const LOG_EVENTS_DEFAULT: u64 = 50;
pub const LOG_EVENTS_MAX: u64 = 2000;
pub const LOG_MINUTES_DEFAULT: u64 = 60;
const USAGE_EXIT: u8 = 2;
const FAILURE_EXIT: u8 = 1;

pub struct Environment {
    pub cloudflare_token: Option<String>,
    pub cloudflare_account: Option<String>,
    pub cloudflare_api: String,
    pub vercel_token: Option<String>,
    pub vercel_team: Option<String>,
    pub vercel_api: String,
    pub certificate: Option<String>,
    pub scratch: PathBuf,
    pub now_ms: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    pub method: http::Method,
    pub path: &'static str,
    pub source: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plant {
    None,
    MigrationEveryDeploy,
    SecretsDropped,
    DigestWrong,
    RollbackCurrent,
}

impl Plant {
    const ALL: [(Self, &'static str); 5] = [
        (Self::None, "none"),
        (Self::MigrationEveryDeploy, "migration_every_deploy"),
        (Self::SecretsDropped, "secrets_dropped"),
        (Self::DigestWrong, "digest_wrong"),
        (Self::RollbackCurrent, "rollback_current"),
    ];

    fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, text)| *text == name)
            .map(|(plant, _)| *plant)
    }

    fn names() -> String {
        Self::ALL.iter().map(|(_, name)| *name).collect::<Vec<_>>().join("|")
    }
}

pub enum Secret {
    Set(String),
    List,
    Delete(String),
}

pub enum Action {
    Deploy {
        program: Box<Program>,
        variables: Vec<(String, String)>,
    },
    Status,
    Logs {
        follow: bool,
        events: u64,
        minutes: u64,
    },
    Rollback {
        to: Option<String>,
    },
    Secrets(Secret),
    Destroy,
}

pub struct Context<'a> {
    pub environment: &'a Environment,
    pub client: http::Client,
    pub plant: Plant,
    pub name: String,
    pub target: Target,
    pub out: PathBuf,
}

impl Context<'_> {
    pub fn say(&self, line: &str) {
        use std::io::Write;
        match std::io::stdout().lock().write_all(format!("{line}\n").as_bytes()) {
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
            written => written.expect("stdout takes the report"),
        }
    }
}

pub fn usage() -> String {
    format!(
        "hl deploy --target {targets} [--name NAME] [--var KEY=VALUE]... [--out DIR] FILE\n       hl status|logs|rollback|destroy|secrets --target {targets} (--name NAME | FILE) ...\n       hl logs ... [--follow] [--events N] [--minutes M]\n       hl rollback ... [--to VERSION]\n       hl secrets set KEY|list|delete KEY ... (set reads the value from stdin)\n       hl destroy ... --yes\n       hl deploy --endpoints\n       deploy plants: --plant {plants}",
        targets = Target::names(),
        plants = Plant::names()
    )
}

struct Parsed {
    target: Option<Target>,
    name: Option<String>,
    file: Option<String>,
    variables: Vec<(String, String)>,
    out: Option<String>,
    follow: bool,
    events: u64,
    minutes: u64,
    to: Option<String>,
    confirmed: bool,
    plant: Plant,
    words: Vec<String>,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut parsed = Parsed {
        target: None,
        name: None,
        file: None,
        variables: Vec::new(),
        out: None,
        follow: false,
        events: LOG_EVENTS_DEFAULT,
        minutes: LOG_MINUTES_DEFAULT,
        to: None,
        confirmed: false,
        plant: Plant::None,
        words: Vec::new(),
    };
    let mut rest = args.iter();
    let missing = |flag: &str| format!("{flag} needs a value");
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--target" => {
                let name = rest.next().ok_or_else(|| missing(flag))?;
                parsed.target =
                    Some(Target::parse(name).ok_or_else(|| format!("unknown target {name}"))?);
            }
            "--name" => parsed.name = Some(rest.next().ok_or_else(|| missing(flag))?.clone()),
            "--out" => parsed.out = Some(rest.next().ok_or_else(|| missing(flag))?.clone()),
            "--to" => parsed.to = Some(rest.next().ok_or_else(|| missing(flag))?.clone()),
            "--var" => {
                let pair = rest.next().ok_or_else(|| missing(flag))?;
                let (key, value) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("--var {pair} is not KEY=VALUE"))?;
                parsed.variables.push((key.to_owned(), value.to_owned()));
            }
            "--events" => parsed.events = number(rest.next(), flag)?,
            "--minutes" => parsed.minutes = number(rest.next(), flag)?,
            "--follow" => parsed.follow = true,
            "--yes" => parsed.confirmed = true,
            "--plant" => {
                let name = rest.next().ok_or_else(|| missing(flag))?;
                parsed.plant =
                    Plant::parse(name).ok_or_else(|| format!("unknown deploy plant {name}"))?;
            }
            word if word.starts_with("--") => return Err(format!("unknown flag {word}")),
            word if word.ends_with(".hl") && parsed.file.is_none() => {
                parsed.file = Some(word.to_owned());
            }
            word => parsed.words.push(word.to_owned()),
        }
    }
    Ok(parsed)
}

fn number(value: Option<&String>, flag: &str) -> Result<u64, String> {
    value
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| format!("{flag} needs a whole number"))
}

pub fn default_name(program: &str) -> String {
    program.replace('_', "-").to_ascii_lowercase()
}

pub fn valid_name(name: &str) -> Result<(), String> {
    let first_ok = name
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let rest_ok = name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !first_ok || !rest_ok || name.ends_with('-') || name.contains("---") {
        return Err(format!(
            "name '{name}' must be lowercase letters, digits and single or double dashes, starting with a letter or digit and not ending with a dash (Cloudflare script and Vercel project names)"
        ));
    }
    if name.len() > NAME_BYTES_MAX {
        return Err(format!(
            "name '{name}' is longer than name_bytes_max ({NAME_BYTES_MAX}), the DNS label its workers.dev and vercel.app hostname becomes"
        ));
    }
    Ok(())
}

pub fn valid_variable(key: &str) -> Result<(), String> {
    let ok = key
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_uppercase() || byte == b'_')
        && key
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "variable '{key}' must be uppercase letters, digits and underscores, as an environment variable name"
        ))
    }
}

pub fn command(
    verb: &str,
    args: &[String],
    environment: &Environment,
    load: &dyn Fn(&str) -> Result<Program, String>,
) -> ExitCode {
    if verb == "deploy" && args.iter().any(|arg| arg == "--endpoints") {
        for line in endpoint_lines() {
            println!("{line}");
        }
        return ExitCode::SUCCESS;
    }
    let invocation = match invocation(verb, args, load) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("hl {verb}: {error}\nusage: {}", usage());
            return ExitCode::from(USAGE_EXIT);
        }
    };
    let (target, name, out, plant, action) = invocation;
    let context = Context {
        environment,
        client: http::Client::new(environment.certificate.clone(), environment.scratch.clone()),
        plant,
        name,
        target,
        out,
    };
    let result = match target {
        Target::Cloudflare => cloudflare::run(&context, action),
        Target::Vercel => vercel::run(&context, action),
        _ => local::run(&context, action),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hl {verb}: error: {error}");
            ExitCode::from(FAILURE_EXIT)
        }
    }
}

type Invocation = (Target, String, PathBuf, Plant, Action);

fn invocation(
    verb: &str,
    args: &[String],
    load: &dyn Fn(&str) -> Result<Program, String>,
) -> Result<Invocation, String> {
    let parsed = parse(args)?;
    let target = parsed.target.ok_or("--target is required")?;
    let program = match &parsed.file {
        Some(file) => Some(load(file)?),
        None => None,
    };
    let name = match (&parsed.name, &program) {
        (Some(name), _) => name.clone(),
        (None, Some(program)) => default_name(&program.name),
        (None, None) => return Err("give FILE or --name NAME".to_owned()),
    };
    valid_name(&name)?;
    if parsed.variables.len() > VARIABLES_MAX {
        return Err(format!(
            "more than variables_max ({VARIABLES_MAX}) --var flags"
        ));
    }
    for (key, _) in &parsed.variables {
        valid_variable(key)?;
    }
    if parsed.events == 0 || parsed.events > LOG_EVENTS_MAX {
        return Err(format!(
            "--events must be 1 to log_events_max ({LOG_EVENTS_MAX}), the telemetry query's own limit"
        ));
    }
    let out = PathBuf::from(
        parsed
            .out
            .clone()
            .unwrap_or_else(|| format!(".hl/deploy/{}/{name}", target.name())),
    );
    let action = action(verb, parsed.words.as_slice(), program, &parsed)?;
    Ok((target, name, out, parsed.plant, action))
}

fn action(verb: &str, words: &[String], program: Option<Program>, parsed: &Parsed) -> Result<Action, String> {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let plain = |action: Action| {
        if words.is_empty() {
            Ok(action)
        } else {
            Err(format!("unexpected arguments: {}", words.join(" ")))
        }
    };
    match verb {
        "deploy" => plain(Action::Deploy {
            program: Box::new(program.ok_or("hl deploy needs FILE")?),
            variables: parsed.variables.clone(),
        }),
        "status" => plain(Action::Status),
        "logs" => plain(Action::Logs {
            follow: parsed.follow,
            events: parsed.events,
            minutes: parsed.minutes,
        }),
        "rollback" => plain(Action::Rollback {
            to: parsed.to.clone(),
        }),
        "destroy" if parsed.confirmed => plain(Action::Destroy),
        "destroy" => Err("hl destroy deletes the deployment and its state; pass --yes".to_owned()),
        "secrets" => match words.as_slice() {
            ["set", key] => {
                valid_variable(key)?;
                Ok(Action::Secrets(Secret::Set((*key).to_owned())))
            }
            ["list"] => Ok(Action::Secrets(Secret::List)),
            ["delete", key] => Ok(Action::Secrets(Secret::Delete((*key).to_owned()))),
            _ => Err("hl secrets takes set KEY, list or delete KEY".to_owned()),
        },
        other => Err(format!("unknown command {other}")),
    }
}

pub fn read_secret() -> Result<String, String> {
    use std::io::Read;
    let mut value = String::new();
    std::io::stdin()
        .take(u64::try_from(SECRET_BYTES_MAX).expect("fits u64") + 1)
        .read_to_string(&mut value)
        .map_err(|error| format!("cannot read the secret from stdin: {error}"))?;
    if value.len() > SECRET_BYTES_MAX {
        return Err(format!(
            "the secret is longer than secret_bytes_max ({SECRET_BYTES_MAX}), Cloudflare's limit on one environment variable"
        ));
    }
    let value = value.strip_suffix('\n').unwrap_or(&value).to_owned();
    if value.is_empty() {
        return Err("the secret read from stdin is empty".to_owned());
    }
    Ok(value)
}

pub fn endpoint_lines() -> Vec<String> {
    let mut lines = Vec::new();
    for (provider, endpoints) in [
        ("cloudflare", cloudflare::ENDPOINTS.as_slice()),
        ("vercel", vercel::ENDPOINTS.as_slice()),
    ] {
        for endpoint in endpoints {
            lines.push(format!(
                "{provider} {} {} {}",
                endpoint.method.name(),
                endpoint.path,
                endpoint.source
            ));
        }
    }
    lines
}

pub fn fill(path: &str, values: &[(&str, &str)]) -> String {
    let mut out = path.to_owned();
    for (name, value) in values {
        let slot = format!("{{{name}}}");
        assert!(out.contains(&slot), "endpoint {path} has slot {name}");
        out = out.replace(&slot, &http::query_escape(value));
    }
    assert!(!out.contains('{'), "every slot of {path} is filled");
    out
}
