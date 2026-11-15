use super::bundle::{self, Bundle, Origin};
use super::http::{Method, Request};
use super::json::Value;
use super::multipart::{self, Part};
use super::websocket::{self, Message};
use super::{Action, Context, Endpoint, Plant, Secret};

const SPEC: &str = "cloudflare/api-schemas 8b1a451";

macro_rules! endpoint {
    ($method:ident, $path:expr, $operation:expr) => {
        Endpoint {
            method: Method::$method,
            path: $path,
            source: $operation,
        }
    };
}

const SCRIPT: &str = "/accounts/{account_id}/workers/scripts/{script_name}";
pub const SCRIPTS: Endpoint = endpoint!(
    Get,
    "/accounts/{account_id}/workers/scripts",
    "worker-script-list-workers"
);
pub const UPLOAD: Endpoint = endpoint!(Put, SCRIPT, "worker-script-upload-worker-module");
pub const DELETE: Endpoint = endpoint!(Delete, SCRIPT, "worker-script-delete-worker");
pub const ACCOUNT_SUBDOMAIN: Endpoint = endpoint!(
    Get,
    "/accounts/{account_id}/workers/subdomain",
    "worker-subdomain-get-subdomain"
);
pub const SCRIPT_SUBDOMAIN: Endpoint = endpoint!(
    Post,
    "/accounts/{account_id}/workers/scripts/{script_name}/subdomain",
    "worker-script-post-subdomain"
);
pub const DEPLOYMENTS: Endpoint = endpoint!(
    Get,
    "/accounts/{account_id}/workers/scripts/{script_name}/deployments",
    "worker-deployments-list-deployments"
);
pub const CREATE_DEPLOYMENT: Endpoint = endpoint!(
    Post,
    "/accounts/{account_id}/workers/scripts/{script_name}/deployments",
    "worker-deployments-create-deployment"
);
pub const VERSIONS: Endpoint = endpoint!(
    Get,
    "/accounts/{account_id}/workers/scripts/{script_name}/versions",
    "worker-versions-list-versions"
);
pub const SECRETS: Endpoint = endpoint!(
    Get,
    "/accounts/{account_id}/workers/scripts/{script_name}/secrets",
    "worker-list-script-secrets"
);
pub const PUT_SECRET: Endpoint = endpoint!(
    Put,
    "/accounts/{account_id}/workers/scripts/{script_name}/secrets",
    "worker-put-script-secret"
);
pub const DELETE_SECRET: Endpoint = endpoint!(
    Delete,
    "/accounts/{account_id}/workers/scripts/{script_name}/secrets/{secret_name}",
    "worker-delete-script-secret"
);
pub const START_TAIL: Endpoint = endpoint!(
    Post,
    "/accounts/{account_id}/workers/scripts/{script_name}/tails",
    "worker-tail-logs-start-tail"
);
pub const DELETE_TAIL: Endpoint = endpoint!(
    Delete,
    "/accounts/{account_id}/workers/scripts/{script_name}/tails/{id}",
    "worker-tail-logs-delete-tail"
);
pub const TELEMETRY: Endpoint = endpoint!(
    Post,
    "/accounts/{account_id}/workers/observability/telemetry/query",
    "telemetry.query"
);

pub const ENDPOINTS: [Endpoint; 14] = [
    SCRIPTS,
    UPLOAD,
    DELETE,
    ACCOUNT_SUBDOMAIN,
    SCRIPT_SUBDOMAIN,
    DEPLOYMENTS,
    CREATE_DEPLOYMENT,
    VERSIONS,
    SECRETS,
    PUT_SECRET,
    DELETE_SECRET,
    START_TAIL,
    DELETE_TAIL,
    TELEMETRY,
];

pub const DURABLE_CLASS: &str = "HarnessObject";
pub const HARNESS_BINDING: &str = "HARNESS";
pub const LOADER_BINDING: &str = "LOADER";
pub const MIGRATION_TAG: &str = "hl-v1";
const DEPLOY_MESSAGE: &str = "hl deploy";
const NOT_FOUND: u16 = 404;
pub const KEPT_BINDINGS: [&str; 2] = ["secret_text", "secret_key"];
pub const COMPATIBILITY_DATE: &str = "2026-11-01";
pub const TAIL_PROTOCOL: &str = "trace-v1";
pub const TAIL_MESSAGE_BYTES_MAX: u64 = 1024 * 1024;
pub const TELEMETRY_QUERY: &str = "hl-logs";
const FULL_PERCENT: u64 = 100;
const MILLISECONDS_PER_MINUTE: u64 = 60_000;

struct Api<'a> {
    context: &'a Context<'a>,
    account: &'a str,
    token: &'a str,
}

enum Body {
    Empty,
    Json(Value),
    Form(multipart::Form),
}

impl<'a> Api<'a> {
    fn new(context: &'a Context<'a>) -> Result<Self, String> {
        let environment = context.environment;
        let token = environment.cloudflare_token.as_deref().ok_or_else(|| {
            format!(
                "set {} to a Cloudflare API token with Workers Scripts edit and Workers Tail read",
                super::CLOUDFLARE_TOKEN_VARIABLE
            )
        })?;
        let account = environment.cloudflare_account.as_deref().ok_or_else(|| {
            format!(
                "set {} to the Cloudflare account ID",
                super::CLOUDFLARE_ACCOUNT_VARIABLE
            )
        })?;
        Ok(Self {
            context,
            account,
            token,
        })
    }

    fn url(&self, endpoint: Endpoint, slots: &[(&str, &str)], query: &str) -> String {
        let mut values = vec![("account_id", self.account)];
        if endpoint.path.contains("{script_name}") {
            values.push(("script_name", self.context.name.as_str()));
        }
        values.extend_from_slice(slots);
        format!(
            "{}{}{query}",
            self.context.environment.cloudflare_api,
            super::fill(endpoint.path, &values)
        )
    }

    fn send(
        &self,
        endpoint: Endpoint,
        slots: &[(&str, &str)],
        query: &str,
        body: Body,
    ) -> Result<(u16, Value), String> {
        let request = self.prepare(endpoint, slots, query, body);
        read(endpoint, self.context.client.send(&request))
    }

    fn send_all(&self, calls: Vec<(Endpoint, Request)>) -> Vec<Result<(u16, Value), String>> {
        let (endpoints, requests): (Vec<Endpoint>, Vec<Request>) = calls.into_iter().unzip();
        self.context
            .client
            .send_all(&requests)
            .into_iter()
            .zip(endpoints)
            .map(|(response, endpoint)| read(endpoint, response))
            .collect()
    }

    fn prepare(
        &self,
        endpoint: Endpoint,
        slots: &[(&str, &str)],
        query: &str,
        body: Body,
    ) -> Request {
        let request = Request::new(endpoint.method, self.url(endpoint, slots, query))
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("User-Agent", super::USER_AGENT);
        match body {
            Body::Empty => request,
            Body::Json(value) => request.json(&value),
            Body::Form(form) => request.bytes(
                &format!("multipart/form-data; boundary={}", form.boundary),
                form.body,
            ),
        }
    }

    fn call(
        &self,
        endpoint: Endpoint,
        slots: &[(&str, &str)],
        query: &str,
        body: Body,
    ) -> Result<Value, String> {
        let (status, value) = self.send(endpoint, slots, query, body)?;
        envelope(endpoint, status, &value)
    }
}

fn read(
    endpoint: Endpoint,
    response: Result<super::http::Response, String>,
) -> Result<(u16, Value), String> {
    let response = response?;
    let value = response
        .json()
        .map_err(|error| format!("{} {}: {error}", endpoint.method.name(), endpoint.source))?;
    Ok((response.status, value))
}

fn answered(endpoint: Endpoint, answer: Result<(u16, Value), String>) -> Result<Value, String> {
    let (status, value) = answer?;
    envelope(endpoint, status, &value)
}

fn envelope(endpoint: Endpoint, status: u16, value: &Value) -> Result<Value, String> {
    if (200..300).contains(&status) && value.get("success").and_then(Value::as_bool) == Some(true) {
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
    let errors: Vec<String> = value
        .get("errors")
        .map(Value::items)
        .unwrap_or_default()
        .iter()
        .map(|error| {
            format!(
                "{} {}",
                error.get("code").map(Value::shown).unwrap_or_default(),
                error.str_at(&["message"]).unwrap_or("")
            )
        })
        .collect();
    Err(format!(
        "{} {} answered HTTP {status}: {}",
        endpoint.method.name(),
        endpoint.source,
        if errors.is_empty() {
            "no error detail".to_owned()
        } else {
            errors.join("; ")
        }
    ))
}

pub fn run(context: &Context, action: Action) -> Result<(), String> {
    let api = Api::new(context)?;
    match action {
        Action::Deploy { bundle, variables } => deploy(&api, &bundle, &variables),
        Action::Status => status(&api),
        Action::Logs {
            follow: true,
            events,
            ..
        } => tail(&api, events),
        Action::Logs {
            follow: false,
            events,
            minutes,
        } => logs(&api, events, minutes),
        Action::Rollback { to } => rollback(&api, to.as_deref()),
        Action::Secrets(secret) => secrets(&api, secret),
        Action::Destroy => destroy(&api),
        Action::Monitor { events, minutes } => monitor(&api, events, minutes),
    }
}

fn migration_tag(api: &Api, scripts: &Value) -> Option<String> {
    let found = scripts
        .items()
        .iter()
        .find(|script| script.str_at(&["id"]) == Some(api.context.name.as_str()));
    found
        .and_then(|script| script.str_at(&["migration_tag"]))
        .map(str::to_owned)
}

fn metadata(
    api: &Api,
    bundle: &Bundle,
    variables: &[(String, String)],
    tag: Option<&str>,
) -> Result<(Value, String), String> {
    let mut bindings = vec![
        Value::object([
            ("type", Value::text("durable_object_namespace")),
            ("name", Value::text(HARNESS_BINDING)),
            ("class_name", Value::text(DURABLE_CLASS)),
        ]),
        Value::object([
            ("type", Value::text("worker_loader")),
            ("name", Value::text(LOADER_BINDING)),
        ]),
    ];
    for (key, value) in variables {
        bindings.push(Value::object([
            ("type", Value::text("plain_text")),
            ("name", Value::text(key)),
            ("text", Value::text(value)),
        ]));
    }
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("main_module".to_owned(), Value::text(&bundle.main));
    fields.insert(
        "compatibility_date".to_owned(),
        Value::text(COMPATIBILITY_DATE),
    );
    fields.insert("bindings".to_owned(), Value::Array(bindings));
    fields.insert(
        "observability".to_owned(),
        Value::object([("enabled", Value::Bool(true))]),
    );
    if api.context.plant != Plant::SecretsDropped {
        fields.insert("keep_bindings".to_owned(), Value::texts(&KEPT_BINDINGS));
    }
    let migration = match tag {
        Some(MIGRATION_TAG) if api.context.plant != Plant::MigrationEveryDeploy => {
            format!("migration: none, the script is at {MIGRATION_TAG}")
        }
        None | Some(MIGRATION_TAG) => {
            fields.insert(
                "migrations".to_owned(),
                Value::object([
                    ("new_tag", Value::text(MIGRATION_TAG)),
                    ("new_sqlite_classes", Value::texts(&[DURABLE_CLASS])),
                ]),
            );
            format!("migration {MIGRATION_TAG}: new SQLite-backed class {DURABLE_CLASS}")
        }
        Some(other) => {
            return Err(format!(
                "script {} is at migration tag {other}, which hl did not write; hl deploys only scripts it migrates itself ({MIGRATION_TAG})",
                api.context.name
            ));
        }
    };
    Ok((Value::Object(fields), migration))
}

struct Serving {
    deployment: String,
    version: String,
    message: String,
}

fn serving(api: &Api) -> Result<Option<Serving>, String> {
    serving_from(api.send(DEPLOYMENTS, &[], "", Body::Empty))
}

fn serving_from(answer: Result<(u16, Value), String>) -> Result<Option<Serving>, String> {
    let (status, value) = answer?;
    if status == NOT_FOUND {
        return Ok(None);
    }
    let listed = envelope(DEPLOYMENTS, status, &value)?;
    let deployments = listed
        .get("deployments")
        .map(Value::items)
        .unwrap_or_default();
    let Some(latest) = deployments.iter().max_by(|left, right| {
        left.str_at(&["created_on"])
            .cmp(&right.str_at(&["created_on"]))
    }) else {
        return Ok(None);
    };
    let deployment = latest
        .str_at(&["id"])
        .ok_or("a deployment has no id")?
        .to_owned();
    let version = latest
        .get("versions")
        .map(Value::items)
        .unwrap_or_default()
        .iter()
        .find(|version| version.get("percentage").and_then(Value::as_u64) == Some(FULL_PERCENT))
        .and_then(|version| version.str_at(&["version_id"]))
        .ok_or_else(|| {
            format!("deployment {deployment} splits traffic between versions; hl rollback --to VERSION picks one")
        })?
        .to_owned();
    let message = latest
        .str_at(&["annotations", "workers/message"])
        .unwrap_or("")
        .to_owned();
    Ok(Some(Serving {
        deployment,
        version,
        message,
    }))
}

fn fingerprint(plant: Plant, bundle: &Bundle, metadata: &Value) -> String {
    if plant == Plant::FingerprintPartial {
        return bundle.digest.clone();
    }
    let bindings = metadata
        .get("bindings")
        .map(Value::encode)
        .unwrap_or_default();
    crate::run::blob::sha256_hex(
        format!(
            "{}\n{bindings}\n{COMPATIBILITY_DATE}\n{}\n",
            bundle.digest, bundle.main
        )
        .as_bytes(),
    )
}

fn deploy(api: &Api, bundle: &Bundle, variables: &[(String, String)]) -> Result<(), String> {
    let context = api.context;
    context.say(&format!(
        "deploy cloudflare {}: {}",
        context.name,
        bundle.describe()
    ));
    if super::first_from(context) && deploy_new(api, bundle, variables)? {
        return Ok(());
    }
    deploy_known(api, bundle, variables)
}

fn annotate(metadata: &mut Value, message: &str) {
    if let Value::Object(fields) = metadata {
        fields.insert(
            "annotations".to_owned(),
            Value::object([("workers/message", Value::text(message))]),
        );
    }
}

fn with_auth(bundle: &Bundle, metadata: &mut Value) -> Result<Option<String>, String> {
    if !matches!(bundle.origin, Origin::Emitted) {
        return Ok(None);
    }
    let token = super::auth_token()?;
    let Value::Object(fields) = metadata else {
        unreachable!("the upload metadata is an object");
    };
    let Some(Value::Array(bindings)) = fields.get_mut("bindings") else {
        unreachable!("the upload metadata lists its bindings");
    };
    bindings.push(Value::object([
        ("type", Value::text("secret_text")),
        ("name", Value::text(super::AUTH_VARIABLE)),
        ("text", Value::text(&token)),
    ]));
    Ok(Some(token))
}

fn form(bundle: &Bundle, metadata: &Value) -> multipart::Form {
    let mut parts = vec![Part {
        name: "metadata".to_owned(),
        file_name: None,
        content_type: "application/json".to_owned(),
        bytes: metadata.encode().into_bytes(),
    }];
    for file in &bundle.files {
        parts.push(Part {
            name: file.path.clone(),
            file_name: Some(file.path.clone()),
            content_type: bundle::cloudflare_type(&file.path).to_owned(),
            bytes: file.bytes.clone(),
        });
    }
    multipart::encode(&parts)
}

fn deploy_new(api: &Api, bundle: &Bundle, variables: &[(String, String)]) -> Result<bool, String> {
    let context = api.context;
    let (mut metadata, migration) = metadata(api, bundle, variables, None)?;
    let fingerprint = fingerprint(context.plant, bundle, &metadata);
    annotate(&mut metadata, &format!("{DEPLOY_MESSAGE} {fingerprint}"));
    let token = with_auth(bundle, &mut metadata)?;
    let mut answers = api
        .send_all(vec![
            (
                UPLOAD,
                api.prepare(UPLOAD, &[], "", Body::Form(form(bundle, &metadata))),
            ),
            (
                ACCOUNT_SUBDOMAIN,
                api.prepare(ACCOUNT_SUBDOMAIN, &[], "", Body::Empty),
            ),
        ])
        .into_iter();
    let (status, value) = answers.next().expect("the upload's answer")?;
    let subdomain = answered(
        ACCOUNT_SUBDOMAIN,
        answers.next().expect("the subdomain's answer"),
    );
    let uploaded = match envelope(UPLOAD, status, &value) {
        Ok(uploaded) => uploaded,
        Err(error) => {
            context.say(&format!(
                "  the upload as a new script was refused ({error}); reading the script first"
            ));
            return Ok(false);
        }
    };
    if let Some(token) = &token {
        super::save_auth_token(context, token)?;
    }
    let url = url_of(api, &subdomain?)?;
    finish(api, bundle, &uploaded, &migration, true, &url)?;
    Ok(true)
}

fn deploy_known(api: &Api, bundle: &Bundle, variables: &[(String, String)]) -> Result<(), String> {
    let context = api.context;
    let mut answers = api
        .send_all(vec![
            (SCRIPTS, api.prepare(SCRIPTS, &[], "", Body::Empty)),
            (DEPLOYMENTS, api.prepare(DEPLOYMENTS, &[], "", Body::Empty)),
            (
                ACCOUNT_SUBDOMAIN,
                api.prepare(ACCOUNT_SUBDOMAIN, &[], "", Body::Empty),
            ),
        ])
        .into_iter();
    let scripts = answered(SCRIPTS, answers.next().expect("the script list"));
    let serving_now = serving_from(answers.next().expect("the deployments"));
    let subdomain = answered(
        ACCOUNT_SUBDOMAIN,
        answers.next().expect("the subdomain's answer"),
    );
    let tag = migration_tag(api, &scripts?);
    let url = url_of(api, &subdomain?)?;
    let (mut metadata, migration) = metadata(api, bundle, variables, tag.as_deref())?;
    let fingerprint = fingerprint(context.plant, bundle, &metadata);
    let message = format!("{DEPLOY_MESSAGE} {fingerprint}");
    if let Some(serving) = serving_now? {
        if serving.message == message && tag.as_deref() == Some(MIGRATION_TAG) {
            context.say(&format!(
                "  unchanged: version {} already serves fingerprint {} (deployment {})",
                serving.version,
                &fingerprint[..bundle::DIGEST_SHOWN],
                serving.deployment
            ));
            context.say(&format!("  url {url}"));
            return super::record(context, bundle, &serving.version, &url);
        }
    }
    annotate(&mut metadata, &message);
    let first = tag.is_none();
    let token = if first {
        with_auth(bundle, &mut metadata)?
    } else {
        None
    };
    let uploaded = api.call(UPLOAD, &[], "", Body::Form(form(bundle, &metadata)))?;
    if let Some(token) = &token {
        super::save_auth_token(context, token)?;
    }
    finish(api, bundle, &uploaded, &migration, first, &url)
}

fn finish(
    api: &Api,
    bundle: &Bundle,
    uploaded: &Value,
    migration: &str,
    first: bool,
    url: &str,
) -> Result<(), String> {
    let context = api.context;
    context.say(&format!("  {migration}"));
    if let Some(answered) = uploaded.str_at(&["migration_tag"]) {
        if answered != MIGRATION_TAG {
            return Err(format!(
                "the upload answered migration tag {answered}, not {MIGRATION_TAG}"
            ));
        }
    }
    let mut calls = vec![(DEPLOYMENTS, api.prepare(DEPLOYMENTS, &[], "", Body::Empty))];
    if first {
        calls.push((
            SCRIPT_SUBDOMAIN,
            api.prepare(
                SCRIPT_SUBDOMAIN,
                &[],
                "",
                Body::Json(Value::object([
                    ("enabled", Value::Bool(true)),
                    ("previews_enabled", Value::Bool(false)),
                ])),
            ),
        ));
    }
    let mut answers = api.send_all(calls).into_iter();
    let now = serving_from(answers.next().expect("the deployments"));
    if let Some(enabled) = answers.next() {
        answered(SCRIPT_SUBDOMAIN, enabled)?;
    }
    let now =
        now?.ok_or_else(|| format!("script {} has no deployment after its upload", context.name))?;
    context.say(&format!(
        "  version {} serves 100% (deployment {})",
        now.version, now.deployment
    ));
    context.say(&format!("  url {url}"));
    super::record(context, bundle, &now.version, url)
}

fn url(api: &Api) -> Result<String, String> {
    let subdomain = api.call(ACCOUNT_SUBDOMAIN, &[], "", Body::Empty)?;
    url_of(api, &subdomain)
}

fn url_of(api: &Api, subdomain: &Value) -> Result<String, String> {
    let subdomain = subdomain
        .str_at(&["subdomain"])
        .ok_or("the account has no workers.dev subdomain; pick one in the dashboard once")?;
    Ok(format!(
        "https://{}.{subdomain}.workers.dev",
        api.context.name
    ))
}

fn current(api: &Api) -> Result<(String, String), String> {
    let serving =
        serving(api)?.ok_or_else(|| format!("script {} has no deployment", api.context.name))?;
    Ok((serving.deployment, serving.version))
}

fn versions(api: &Api) -> Result<Vec<(u64, String, String)>, String> {
    let listed = api.call(VERSIONS, &[], "", Body::Empty)?;
    let mut versions = Vec::new();
    for item in listed.get("items").map(Value::items).unwrap_or_default() {
        let number = item
            .get("number")
            .and_then(Value::as_u64)
            .ok_or("a version has no number")?;
        let id = item.str_at(&["id"]).ok_or("a version has no id")?;
        let created = item.str_at(&["metadata", "created_on"]).unwrap_or("");
        versions.push((number, id.to_owned(), created.to_owned()));
    }
    versions.sort();
    Ok(versions)
}

fn status(api: &Api) -> Result<(), String> {
    let context = api.context;
    context.say(&format!("status cloudflare {}", context.name));
    context.say(&format!("  url {}", url(api)?));
    let (deployment, version) = current(api)?;
    let versions = versions(api)?;
    let number = versions
        .iter()
        .find(|(_, id, _)| *id == version)
        .map(|(number, _, created)| format!("#{number}, uploaded {created}"))
        .unwrap_or_else(|| "not among the listed versions".to_owned());
    context.say(&format!(
        "  version {version} ({number}) serves 100% (deployment {deployment})"
    ));
    context.say(&format!("  versions: {}", versions.len()));
    let names = secret_names(api)?;
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

fn rollback(api: &Api, to: Option<&str>) -> Result<(), String> {
    let context = api.context;
    let (_, serving) = current(api)?;
    let versions = versions(api)?;
    let position = versions
        .iter()
        .position(|(_, id, _)| *id == serving)
        .ok_or_else(|| format!("the serving version {serving} is not among the listed versions"))?;
    let target = match to {
        Some(id) => versions
            .iter()
            .find(|(_, version, _)| version == id)
            .ok_or_else(|| format!("script {} has no version {id}", context.name))?,
        None if context.plant == Plant::RollbackCurrent => &versions[position],
        None if position == 0 => {
            return Err(format!(
                "version {serving} is the script's first; there is nothing to roll back to"
            ));
        }
        None => &versions[position - 1],
    };
    let body = Value::object([
        ("strategy", Value::text("percentage")),
        (
            "versions",
            Value::Array(vec![Value::object([
                ("version_id", Value::text(&target.1)),
                ("percentage", Value::number(FULL_PERCENT)),
            ])]),
        ),
        (
            "annotations",
            Value::object([(
                "workers/message",
                Value::text(&format!("hl rollback from {serving}")),
            )]),
        ),
    ]);
    let created = api.call(CREATE_DEPLOYMENT, &[], "", Body::Json(body))?;
    context.say(&format!(
        "rollback cloudflare {}: version {} (#{}) serves 100% (deployment {}), was {serving}",
        context.name,
        target.1,
        target.0,
        created.str_at(&["id"]).unwrap_or("?")
    ));
    context.say("  the Durable Object's storage is not rolled back: the older code reads the journal as it is now");
    context.say("  secrets belong to versions: the serving version has the secrets it was uploaded or last changed with");
    Ok(())
}

fn secret_names(api: &Api) -> Result<Vec<String>, String> {
    let listed = api.call(SECRETS, &[], "", Body::Empty)?;
    let mut names: Vec<String> = listed
        .items()
        .iter()
        .filter_map(|secret| secret.str_at(&["name"]).map(str::to_owned))
        .collect();
    names.sort();
    Ok(names)
}

fn secrets(api: &Api, secret: Secret) -> Result<(), String> {
    let context = api.context;
    match secret {
        Secret::Set(key) => {
            let value = super::read_secret()?;
            let body = Value::object([
                ("name", Value::text(&key)),
                ("text", Value::text(&value)),
                ("type", Value::text("secret_text")),
            ]);
            api.call(PUT_SECRET, &[], "", Body::Json(body))?;
            context.say(&format!(
                "secrets cloudflare {}: set {key} (a new version serves it)",
                context.name
            ));
        }
        Secret::List => {
            for name in secret_names(api)? {
                context.say(&name);
            }
        }
        Secret::Delete(key) => {
            api.call(
                DELETE_SECRET,
                &[("secret_name", key.as_str())],
                "",
                Body::Empty,
            )?;
            context.say(&format!(
                "secrets cloudflare {}: deleted {key}",
                context.name
            ));
        }
    }
    Ok(())
}

fn destroy(api: &Api) -> Result<(), String> {
    let context = api.context;
    api.call(DELETE, &[], "?force=true", Body::Empty)?;
    super::forget(context)?;
    context.say(&format!(
        "destroy cloudflare {}: deleted the script, its versions and its Durable Objects' storage",
        context.name
    ));
    Ok(())
}

fn telemetry(api: &Api, events: u64, minutes: u64) -> Result<Vec<super::Seen>, String> {
    telemetry_events(api.call(
        TELEMETRY,
        &[],
        "",
        Body::Json(telemetry_body(api, events, minutes)),
    )?)
}

fn telemetry_body(api: &Api, events: u64, minutes: u64) -> Value {
    let context = api.context;
    let to = context.environment.now_ms;
    let from = to.saturating_sub(minutes * MILLISECONDS_PER_MINUTE);
    let body = Value::object([
        ("queryId", Value::text(TELEMETRY_QUERY)),
        ("view", Value::text("events")),
        ("limit", Value::number(events)),
        (
            "timeframe",
            Value::object([("from", Value::number(from)), ("to", Value::number(to))]),
        ),
        (
            "parameters",
            Value::object([(
                "filters",
                Value::Array(vec![Value::object([
                    ("key", Value::text("$metadata.service")),
                    ("operation", Value::text("eq")),
                    ("type", Value::text("string")),
                    ("value", Value::text(&context.name)),
                ])]),
            )]),
        ),
    ]);
    body
}

fn telemetry_events(result: Value) -> Result<Vec<super::Seen>, String> {
    let mut seen: Vec<super::Seen> = result
        .path(&["events", "events"])
        .map(Value::items)
        .unwrap_or_default()
        .iter()
        .map(|event| {
            let at = event.get("timestamp").and_then(Value::as_u64).unwrap_or(0);
            let level = event.str_at(&["$metadata", "level"]).unwrap_or("log");
            let message = event
                .str_at(&["$metadata", "message"])
                .map(str::to_owned)
                .or_else(|| event.get("source").map(Value::shown))
                .unwrap_or_default();
            let outcome = event.str_at(&["$workers", "outcome"]).unwrap_or("ok");
            super::Seen {
                at,
                line: format!("{} {level} {message}", super::iso_time(at)),
                failed: level == "error" || outcome != "ok",
            }
        })
        .collect();
    seen.sort_by(|left, right| left.at.cmp(&right.at).then(left.line.cmp(&right.line)));
    Ok(seen)
}

fn logs(api: &Api, events: u64, minutes: u64) -> Result<(), String> {
    let context = api.context;
    let seen = telemetry(api, events, minutes)?;
    context.say(&format!(
        "logs cloudflare {}: {} events in the last {minutes} minutes",
        context.name,
        seen.len()
    ));
    for event in seen {
        context.say(&event.line);
    }
    Ok(())
}

fn monitor(api: &Api, events: u64, minutes: u64) -> Result<(), String> {
    let context = api.context;
    let mut answers = api
        .send_all(vec![
            (
                ACCOUNT_SUBDOMAIN,
                api.prepare(ACCOUNT_SUBDOMAIN, &[], "", Body::Empty),
            ),
            (DEPLOYMENTS, api.prepare(DEPLOYMENTS, &[], "", Body::Empty)),
            (
                TELEMETRY,
                api.prepare(
                    TELEMETRY,
                    &[],
                    "",
                    Body::Json(telemetry_body(api, events, minutes)),
                ),
            ),
        ])
        .into_iter();
    let subdomain = answered(ACCOUNT_SUBDOMAIN, answers.next().expect("the subdomain"));
    let now = serving_from(answers.next().expect("the deployments"));
    let seen =
        answered(TELEMETRY, answers.next().expect("the telemetry")).and_then(telemetry_events);
    context.say(&format!("monitor cloudflare {}", context.name));
    context.say(&format!("  url {}", url_of(api, &subdomain?)?));
    let now = now?.ok_or_else(|| format!("script {} has no deployment", context.name))?;
    context.say(&format!(
        "  version {} serves 100% (deployment {})",
        now.version, now.deployment
    ));
    super::traffic(context, minutes, &seen?);
    Ok(())
}

fn tail(api: &Api, events: u64) -> Result<(), String> {
    let context = api.context;
    let started = api.call(
        START_TAIL,
        &[],
        "",
        Body::Json(Value::object([("filters", Value::Array(Vec::new()))])),
    )?;
    let id = started
        .get("id")
        .map(Value::shown)
        .ok_or("the tail has no id")?;
    let address = websocket::parse(started.str_at(&["url"]).ok_or("the tail has no url")?)?;
    let followed = (|| {
        let mut socket = websocket::connect(
            &address,
            TAIL_PROTOCOL,
            context.environment.certificate.as_deref(),
            super::USER_AGENT,
        )?;
        socket.send_text(&Value::object([("debug", Value::Bool(false))]).encode())?;
        context.say(&format!(
            "tail cloudflare {}: following (tail {id})",
            context.name
        ));
        let mut seen = 0;
        while seen < events {
            let text = match socket.next_message(TAIL_MESSAGE_BYTES_MAX, TAIL_MESSAGE_BYTES_MAX)? {
                Message::Closed => break,
                Message::Text(text) => text,
                Message::Binary(bytes) => String::from_utf8(bytes)
                    .map_err(|_| "the tail sent a binary message that is not UTF-8".to_owned())?,
            };
            let event = super::json::decode(&text)
                .map_err(|error| format!("the tail sent an event that is not JSON: {error}"))?;
            for line in tail_lines(&event) {
                context.say(&line);
            }
            seen += 1;
        }
        socket.close()
    })();
    let deleted = api.call(DELETE_TAIL, &[("id", id.as_str())], "", Body::Empty);
    followed?;
    deleted.map(drop)
}

fn tail_lines(event: &Value) -> Vec<String> {
    let at = event
        .get("eventTimestamp")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let outcome = event.str_at(&["outcome"]).unwrap_or("unknown");
    let request = match (
        event.str_at(&["event", "request", "method"]),
        event.str_at(&["event", "request", "url"]),
    ) {
        (Some(method), Some(url)) => format!(" {method} {url}"),
        _ => String::new(),
    };
    let mut lines = vec![format!("{} {outcome}{request}", super::iso_time(at))];
    for log in event.get("logs").map(Value::items).unwrap_or_default() {
        let parts: Vec<String> = log
            .get("message")
            .map(Value::items)
            .unwrap_or_default()
            .iter()
            .map(Value::shown)
            .collect();
        lines.push(format!(
            "  {} {}",
            log.str_at(&["level"]).unwrap_or("log"),
            parts.join(" ")
        ));
    }
    for exception in event
        .get("exceptions")
        .map(Value::items)
        .unwrap_or_default()
    {
        lines.push(format!(
            "  exception {}: {}",
            exception.str_at(&["name"]).unwrap_or("Error"),
            exception.str_at(&["message"]).unwrap_or("")
        ));
    }
    lines
}

pub fn spec() -> &'static str {
    SPEC
}
