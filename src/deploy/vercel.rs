use super::bundle::{Bundle, File, Origin};
use super::http::{Flow, Method, Request};
use super::json::Value;
use super::{Action, Context, Endpoint, Plant, Secret};

const SPEC: &str = "vercel/sdk fe01b47 vercel-spec.json";

macro_rules! endpoint {
    ($method:ident, $path:expr, $operation:expr) => {
        Endpoint {
            method: Method::$method,
            path: $path,
            source: $operation,
        }
    };
}

pub const GET_PROJECT: Endpoint = endpoint!(Get, "/v9/projects/{idOrName}", "getProject");
pub const CREATE_PROJECT: Endpoint = endpoint!(Post, "/v11/projects", "createProject");
pub const DELETE_PROJECT: Endpoint = endpoint!(Delete, "/v9/projects/{idOrName}", "deleteProject");
pub const UPLOAD_FILE: Endpoint = endpoint!(Post, "/v2/files", "uploadFile");
pub const CREATE_DEPLOYMENT: Endpoint = endpoint!(Post, "/v13/deployments", "createDeployment");
pub const GET_DEPLOYMENT: Endpoint = endpoint!(Get, "/v13/deployments/{idOrUrl}", "getDeployment");
pub const LIST_DEPLOYMENTS: Endpoint = endpoint!(Get, "/v7/deployments", "getDeployments");
pub const DEPLOYMENT_EVENTS: Endpoint = endpoint!(
    Get,
    "/v3/deployments/{idOrUrl}/events",
    "getDeploymentEvents"
);
pub const RUNTIME_LOGS: Endpoint = endpoint!(
    Get,
    "/v1/projects/{projectId}/deployments/{deploymentId}/runtime-logs",
    "getRuntimeLogs"
);
pub const ROLLBACK: Endpoint = endpoint!(
    Post,
    "/v1/projects/{projectId}/rollback/{deploymentId}",
    "requestRollback"
);
pub const LIST_ENV: Endpoint = endpoint!(Get, "/v10/projects/{idOrName}/env", "filterProjectEnvs");
pub const CREATE_ENV: Endpoint =
    endpoint!(Post, "/v10/projects/{idOrName}/env", "createProjectEnv");
pub const DELETE_ENV: Endpoint = endpoint!(
    Delete,
    "/v9/projects/{idOrName}/env/{id}",
    "removeProjectEnv"
);

pub const ENDPOINTS: [Endpoint; 13] = [
    GET_PROJECT,
    CREATE_PROJECT,
    DELETE_PROJECT,
    UPLOAD_FILE,
    CREATE_DEPLOYMENT,
    GET_DEPLOYMENT,
    LIST_DEPLOYMENTS,
    DEPLOYMENT_EVENTS,
    RUNTIME_LOGS,
    ROLLBACK,
    LIST_ENV,
    CREATE_ENV,
    DELETE_ENV,
];

pub const FRAMEWORK: &str = "nextjs";
pub const FINGERPRINT_META: &str = "hlFingerprint";
const MISSING_FILES: &str = "missing_files";
pub const BUILD_SECONDS_MAX: u64 = 45 * 60;
pub const DEPLOYMENTS_LISTED_MAX: u64 = 100;
pub const BUILD_EVENTS_FOLLOWED_MAX: u64 = super::LOG_EVENTS_MAX;
const CONFLICT: u16 = 409;
const SERVER_ERROR_STATUS: u64 = 500;
const NOT_FOUND: u16 = 404;
const MILLISECONDS_PER_SECOND: u64 = 1000;
const MILLISECONDS_PER_MINUTE: u64 = 60_000;

struct Api<'a> {
    context: &'a Context<'a>,
    token: &'a str,
}

enum Body {
    Empty,
    Json(Value),
    File { bytes: Vec<u8>, digest: String },
}

struct Answer {
    status: u16,
    value: Value,
}

impl<'a> Api<'a> {
    fn new(context: &'a Context<'a>) -> Result<Self, String> {
        let token = context.environment.vercel_token.as_deref().ok_or_else(|| {
            format!(
                "set {} to a Vercel access token",
                super::VERCEL_TOKEN_VARIABLE
            )
        })?;
        Ok(Self { context, token })
    }

    fn url(&self, endpoint: Endpoint, slots: &[(&str, &str)], query: &[(&str, &str)]) -> String {
        let mut values = Vec::new();
        if endpoint.path.contains("{idOrName}") {
            values.push(("idOrName", self.context.name.as_str()));
        }
        values.extend_from_slice(slots);
        let mut pairs: Vec<(&str, &str)> = query.to_vec();
        if let Some(team) = &self.context.environment.vercel_team {
            pairs.push(("teamId", team.as_str()));
        }
        let query: Vec<String> = pairs
            .iter()
            .map(|(key, value)| format!("{key}={}", super::http::query_escape(value)))
            .collect();
        let query = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        format!(
            "{}{}{query}",
            self.context.environment.vercel_api,
            super::fill(endpoint.path, &values)
        )
    }

    fn request(
        &self,
        endpoint: Endpoint,
        slots: &[(&str, &str)],
        query: &[(&str, &str)],
    ) -> Request {
        Request::new(endpoint.method, self.url(endpoint, slots, query))
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("User-Agent", super::USER_AGENT)
    }

    fn answer(
        &self,
        endpoint: Endpoint,
        slots: &[(&str, &str)],
        query: &[(&str, &str)],
        body: Body,
    ) -> Result<Answer, String> {
        let request = self.prepare(endpoint, slots, query, body);
        read(endpoint, self.context.client.send(&request))
    }

    fn answer_all(&self, calls: Vec<(Endpoint, Request)>) -> Vec<Result<Answer, String>> {
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
        query: &[(&str, &str)],
        body: Body,
    ) -> Request {
        let request = self.request(endpoint, slots, query);
        match body {
            Body::Empty => request,
            Body::Json(value) => request.json(&value),
            Body::File { bytes, digest } => request
                .header("x-vercel-digest", &digest)
                .bytes("application/octet-stream", bytes),
        }
    }

    fn call(
        &self,
        endpoint: Endpoint,
        slots: &[(&str, &str)],
        query: &[(&str, &str)],
        body: Body,
    ) -> Result<Value, String> {
        let answer = self.answer(endpoint, slots, query, body)?;
        if !(200..300).contains(&answer.status) {
            return Err(failure(
                &format!("{} {}", endpoint.method.name(), endpoint.source),
                answer.status,
                &answer.value,
            ));
        }
        Ok(answer.value)
    }
}

fn read(
    endpoint: Endpoint,
    response: Result<super::http::Response, String>,
) -> Result<Answer, String> {
    let response = response?;
    let shown = format!("{} {}", endpoint.method.name(), endpoint.source);
    let value = if response.body.is_empty() {
        Value::Null
    } else {
        response
            .json()
            .map_err(|error| format!("{shown}: {error}"))?
    };
    Ok(Answer {
        status: response.status,
        value,
    })
}

fn failure(shown: &str, status: u16, value: &Value) -> String {
    let code = value.str_at(&["error", "code"]).unwrap_or("");
    let message = value
        .str_at(&["error", "message"])
        .unwrap_or("no error detail");
    format!("{shown} answered HTTP {status}: {code} {message}")
}

pub fn run(context: &Context, action: Action) -> Result<(), String> {
    let api = Api::new(context)?;
    match action {
        Action::Deploy { bundle, variables } => deploy(&api, &bundle, &variables),
        Action::Status => status(&api),
        Action::Logs { follow, events, .. } => logs(&api, follow, events),
        Action::Rollback { to } => rollback(&api, to.as_deref()),
        Action::Secrets(secret) => secrets(&api, secret),
        Action::Destroy => destroy(&api),
        Action::Monitor { events, minutes } => monitor(&api, events, minutes),
    }
}

struct Project {
    id: String,
    created: bool,
    serving: Option<Value>,
}

fn create_body(
    api: &Api,
    bundle: &Bundle,
    variables: &[(String, String)],
    token: Option<&str>,
) -> Value {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("name".to_owned(), Value::text(&api.context.name));
    if matches!(bundle.origin, Origin::Emitted) {
        fields.insert("framework".to_owned(), Value::text(FRAMEWORK));
    }
    let mut environment: Vec<Value> = variables
        .iter()
        .map(|(key, value)| environment_variable(key, value, "plain"))
        .collect();
    if let Some(token) = token {
        environment.push(environment_variable(
            super::AUTH_VARIABLE,
            token,
            "encrypted",
        ));
    }
    if !environment.is_empty() {
        fields.insert("environmentVariables".to_owned(), Value::Array(environment));
    }
    Value::Object(fields)
}

fn auth_for(bundle: &Bundle) -> Result<Option<String>, String> {
    match bundle.origin {
        Origin::Emitted => super::auth_token().map(Some),
        Origin::Directory(_) => Ok(None),
    }
}

fn created_project(api: &Api, created: &Value, token: Option<&str>) -> Result<Project, String> {
    api.context
        .say(&format!("  created project {}", api.context.name));
    if let Some(token) = token {
        super::save_auth_token(api.context, token)?;
    }
    let id = created.str_at(&["id"]).ok_or("the project has no id")?;
    Ok(Project {
        id: id.to_owned(),
        created: true,
        serving: None,
    })
}

fn first_project(
    api: &Api,
    bundle: &Bundle,
    variables: &[(String, String)],
) -> Result<Option<Project>, String> {
    let token = auth_for(bundle)?;
    let body = create_body(api, bundle, variables, token.as_deref());
    let (first, rest) = bundle
        .files
        .split_at(bundle.files.len().min(super::REQUESTS_IN_FLIGHT_MAX - 1));
    let mut calls = vec![(
        CREATE_PROJECT,
        api.prepare(CREATE_PROJECT, &[], &[], Body::Json(body)),
    )];
    calls.extend(first.iter().map(|file| upload_call(api, file)));
    let mut answers = api.answer_all(calls).into_iter();
    let created = answers.next().expect("the project's answer");
    for answer in answers {
        uploaded(answer)?;
    }
    let rest: Vec<&File> = rest.iter().collect();
    upload(api, &rest)?;
    api.context
        .say(&format!("  uploaded {} files by SHA-1", bundle.files.len()));
    let created = created?;
    if created.status == CONFLICT {
        api.context.say("  the project exists already: reading it");
        return Ok(None);
    }
    if !(200..300).contains(&created.status) {
        return Err(failure(
            "POST createProject",
            created.status,
            &created.value,
        ));
    }
    created_project(api, &created.value, token.as_deref()).map(Some)
}

fn project(api: &Api, bundle: &Bundle, variables: &[(String, String)]) -> Result<Project, String> {
    let found = api.answer(GET_PROJECT, &[], &[], Body::Empty)?;
    if found.status != NOT_FOUND {
        if !(200..300).contains(&found.status) {
            return Err(failure("GET getProject", found.status, &found.value));
        }
        let id = found.value.str_at(&["id"]).ok_or("the project has no id")?;
        let serving = found
            .value
            .path(&["targets", "production"])
            .filter(|value| **value != Value::Null)
            .cloned();
        return Ok(Project {
            id: id.to_owned(),
            created: false,
            serving,
        });
    }
    let token = auth_for(bundle)?;
    let body = create_body(api, bundle, variables, token.as_deref());
    let created = api.call(CREATE_PROJECT, &[], &[], Body::Json(body))?;
    created_project(api, &created, token.as_deref())
}

fn environment_variable(key: &str, value: &str, kind: &str) -> Value {
    Value::object([
        ("key", Value::text(key)),
        ("value", Value::text(value)),
        ("type", Value::text(kind)),
        ("target", Value::texts(&["production"])),
    ])
}

fn fingerprint(plant: Plant, bundle: &Bundle, variables: &[(String, String)]) -> String {
    if plant == Plant::FingerprintPartial {
        return bundle.digest.clone();
    }
    let mut text = format!("{}\n", bundle.digest);
    let mut sorted = variables.to_vec();
    sorted.sort();
    for (key, value) in sorted {
        text.push_str(&format!("{key}={value}\n"));
    }
    crate::run::blob::sha256_hex(text.as_bytes())
}

fn upload_call(api: &Api, file: &File) -> (Endpoint, Request) {
    let digest = if api.context.plant == Plant::DigestWrong {
        super::sha1::sha1_hex(file.path.as_bytes())
    } else {
        file.sha1.clone()
    };
    (
        UPLOAD_FILE,
        api.prepare(
            UPLOAD_FILE,
            &[],
            &[],
            Body::File {
                bytes: file.bytes.clone(),
                digest,
            },
        ),
    )
}

fn uploaded(answer: Result<Answer, String>) -> Result<(), String> {
    let answer = answer?;
    if !(200..300).contains(&answer.status) {
        return Err(failure("POST uploadFile", answer.status, &answer.value));
    }
    Ok(())
}

fn upload(api: &Api, files: &[&File]) -> Result<(), String> {
    for batch in files.chunks(super::REQUESTS_IN_FLIGHT_MAX) {
        let calls = batch.iter().map(|file| upload_call(api, file)).collect();
        for answer in api.answer_all(calls) {
            uploaded(answer)?;
        }
    }
    Ok(())
}

fn deployment_body(api: &Api, bundle: &Bundle, project_id: &str, fingerprint: &str) -> Value {
    let files = bundle
        .files
        .iter()
        .map(|file| {
            Value::object([
                ("file", Value::text(&file.path)),
                ("sha", Value::text(&file.sha1)),
                (
                    "size",
                    Value::number(u64::try_from(file.bytes.len()).expect("fits u64")),
                ),
            ])
        })
        .collect();
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("name".to_owned(), Value::text(&api.context.name));
    fields.insert("project".to_owned(), Value::text(project_id));
    fields.insert("target".to_owned(), Value::text("production"));
    fields.insert("files".to_owned(), Value::Array(files));
    fields.insert(
        "meta".to_owned(),
        Value::object([
            ("hlDigest", Value::text(&bundle.digest)),
            (FINGERPRINT_META, Value::text(fingerprint)),
        ]),
    );
    if matches!(bundle.origin, Origin::Emitted) {
        fields.insert(
            "projectSettings".to_owned(),
            Value::object([("framework", Value::text(FRAMEWORK))]),
        );
    }
    Value::Object(fields)
}

fn create_deployment(
    api: &Api,
    bundle: &Bundle,
    body: &Value,
    uploaded: bool,
) -> Result<Value, String> {
    let query = [("skipAutoDetectionConfirmation", "1")];
    if !uploaded {
        let answer = api.answer(CREATE_DEPLOYMENT, &[], &query, Body::Json(body.clone()))?;
        if (200..300).contains(&answer.status) {
            api.context.say("  every file was already uploaded");
            return Ok(answer.value);
        }
        if answer.value.str_at(&["error", "code"]) != Some(MISSING_FILES) {
            return Err(failure(
                "POST createDeployment",
                answer.status,
                &answer.value,
            ));
        }
        let missing: Vec<&str> = answer
            .value
            .path(&["error", "missing"])
            .map(Value::items)
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let files: Vec<&File> = bundle
            .files
            .iter()
            .filter(|file| missing.contains(&file.sha1.as_str()))
            .collect();
        if files.len() != missing.len() {
            return Err(format!(
                "Vercel asked for {} files the bundle does not hold",
                missing.len() - files.len().min(missing.len())
            ));
        }
        upload(api, &files)?;
        api.context.say(&format!(
            "  uploaded {} of {} files by SHA-1, the rest were already uploaded",
            files.len(),
            bundle.files.len()
        ));
    }
    api.call(CREATE_DEPLOYMENT, &[], &query, Body::Json(body.clone()))
}

fn deploy(api: &Api, bundle: &Bundle, variables: &[(String, String)]) -> Result<(), String> {
    let context = api.context;
    context.say(&format!(
        "deploy vercel {}: {}",
        context.name,
        bundle.describe()
    ));
    let fingerprint = fingerprint(context.plant, bundle, variables);
    let first = if super::first_from(context) {
        first_project(api, bundle, variables)?
    } else {
        None
    };
    let uploaded = super::first_from(context);
    let project = match first {
        Some(project) => project,
        None => project(api, bundle, variables)?,
    };
    if let Some(serving) = &project.serving {
        if serving.str_at(&["meta", FINGERPRINT_META]) == Some(fingerprint.as_str()) {
            let id = serving.str_at(&["id"]).unwrap_or("?").to_owned();
            let url = format!("https://{}", serving_url(serving));
            context.say(&format!(
                "  unchanged: deployment {id} already serves fingerprint {}",
                &fingerprint[..super::bundle::DIGEST_SHOWN]
            ));
            context.say(&format!("  url {url}"));
            return super::record(context, bundle, &id, &url);
        }
    }
    if !variables.is_empty() && !project.created {
        let body = Value::Array(
            variables
                .iter()
                .map(|(key, value)| environment_variable(key, value, "plain"))
                .collect(),
        );
        api.call(CREATE_ENV, &[], &[("upsert", "true")], Body::Json(body))?;
        context.say(&format!("  set {} plain variables", variables.len()));
    }
    if project.created && !uploaded {
        let files: Vec<&File> = bundle.files.iter().collect();
        upload(api, &files)?;
        context.say(&format!("  uploaded {} files by SHA-1", bundle.files.len()));
    }
    let body = deployment_body(api, bundle, &project.id, &fingerprint);
    let created = create_deployment(api, bundle, &body, project.created || uploaded)?;
    let id = created
        .str_at(&["id"])
        .ok_or("the deployment has no id")?
        .to_owned();
    context.say(&format!("  deployment {id} created"));
    let ready = wait_ready(api, &id)?;
    let url = format!("https://{}", serving_url(&ready));
    context.say(&format!("  deployment {id} READY, serves production"));
    context.say(&format!("  url {url}"));
    super::record(context, bundle, &id, &url)
}

fn serving_url(deployment: &Value) -> String {
    deployment
        .get("alias")
        .map(Value::items)
        .unwrap_or_default()
        .first()
        .and_then(Value::as_str)
        .or_else(|| deployment.str_at(&["url"]))
        .unwrap_or("?")
        .to_owned()
}

fn follow_build(api: &Api, id: &str) {
    let request = api.request(
        DEPLOYMENT_EVENTS,
        &[("idOrUrl", id)],
        &[("follow", "1"), ("builds", "1")],
    );
    let followed = api.context.client.stream(
        &request,
        BUILD_EVENTS_FOLLOWED_MAX,
        Some(BUILD_SECONDS_MAX),
        &mut |_| Flow::More,
    );
    match followed {
        Ok(lines) if lines == BUILD_EVENTS_FOLLOWED_MAX => api.context.say(&format!(
            "  followed build_events_followed_max ({BUILD_EVENTS_FOLLOWED_MAX}) build events; polling"
        )),
        Ok(_) => {}
        Err(error) => api
            .context
            .say(&format!("  cannot follow the build ({error}); polling")),
    }
}

fn wait_ready(api: &Api, id: &str) -> Result<Value, String> {
    follow_build(api, id);
    let poll_ms = api.context.environment.poll_ms;
    assert!(poll_ms > 0, "the poll interval is positive");
    let polls_max = BUILD_SECONDS_MAX * MILLISECONDS_PER_SECOND / poll_ms;
    let mut last = String::new();
    for _ in 0..polls_max {
        let deployment = api.call(GET_DEPLOYMENT, &[("idOrUrl", id)], &[], Body::Empty)?;
        let state = deployment.str_at(&["readyState"]).unwrap_or("").to_owned();
        match state.as_str() {
            "READY" => return Ok(deployment),
            "ERROR" | "CANCELED" => {
                return Err(format!(
                    "deployment {id} ended {state}: {}",
                    deployment
                        .str_at(&["errorMessage"])
                        .unwrap_or("no error message")
                ));
            }
            _ => {}
        }
        if state != last {
            api.context.say(&format!("  deployment {id} {state}"));
            last = state;
        }
        std::thread::sleep(std::time::Duration::from_millis(poll_ms));
    }
    Err(format!(
        "deployment {id} was not READY within build_seconds_max ({BUILD_SECONDS_MAX}), Vercel's build time limit"
    ))
}

fn production(api: &Api) -> Result<(String, Value), String> {
    let project = api.call(GET_PROJECT, &[], &[], Body::Empty)?;
    let id = project
        .str_at(&["id"])
        .ok_or("the project has no id")?
        .to_owned();
    let serving = project
        .path(&["targets", "production"])
        .filter(|value| **value != Value::Null)
        .cloned()
        .ok_or_else(|| format!("project {} has no production deployment", api.context.name))?;
    Ok((id, serving))
}

fn deployments(api: &Api, project_id: &str) -> Result<Vec<(u64, String, String)>, String> {
    let limit = DEPLOYMENTS_LISTED_MAX.to_string();
    let listed = api.call(
        LIST_DEPLOYMENTS,
        &[],
        &[
            ("projectId", project_id),
            ("target", "production"),
            ("state", "READY"),
            ("limit", &limit),
        ],
        Body::Empty,
    )?;
    let mut found = Vec::new();
    for deployment in listed
        .get("deployments")
        .map(Value::items)
        .unwrap_or_default()
    {
        let created = deployment
            .get("createdAt")
            .and_then(Value::as_u64)
            .ok_or("a deployment has no createdAt")?;
        let id = deployment
            .str_at(&["uid"])
            .ok_or("a deployment has no uid")?;
        let url = deployment.str_at(&["url"]).unwrap_or("");
        found.push((created, id.to_owned(), url.to_owned()));
    }
    found.sort();
    Ok(found)
}

fn status(api: &Api) -> Result<(), String> {
    let context = api.context;
    context.say(&format!("status vercel {}", context.name));
    let (project_id, serving) = production(api)?;
    let id = serving.str_at(&["id"]).unwrap_or("?");
    let url = serving
        .get("alias")
        .map(Value::items)
        .unwrap_or_default()
        .first()
        .and_then(Value::as_str)
        .or_else(|| serving.str_at(&["url"]))
        .unwrap_or("?");
    context.say(&format!("  url https://{url}"));
    let created = serving
        .get("createdAt")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    context.say(&format!(
        "  deployment {id} {} serves production, created {}",
        serving.str_at(&["readyState"]).unwrap_or("?"),
        super::iso_time(created)
    ));
    let listed = deployments(api, &project_id)?;
    context.say(&format!("  ready production deployments: {}", listed.len()));
    let names = environment_names(api)?;
    context.say(&format!(
        "  environment: {}",
        if names.is_empty() {
            "none".to_owned()
        } else {
            names
                .iter()
                .map(|(key, kind, _)| format!("{key} ({kind})"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    Ok(())
}

fn rollback(api: &Api, to: Option<&str>) -> Result<(), String> {
    let context = api.context;
    let (project_id, serving) = production(api)?;
    let serving = serving
        .str_at(&["id"])
        .ok_or("the production deployment has no id")?
        .to_owned();
    let listed = deployments(api, &project_id)?;
    let position = listed.iter().position(|(_, id, _)| *id == serving);
    let target = match (to, position) {
        (Some(id), _) => listed
            .iter()
            .find(|(_, deployment, _)| deployment == id)
            .ok_or_else(|| {
                format!(
                    "project {} has no ready production deployment {id}",
                    context.name
                )
            })?,
        (None, Some(index)) if context.plant == Plant::RollbackCurrent => &listed[index],
        (None, Some(0)) => {
            return Err(format!(
                "deployment {serving} is the project's first; there is nothing to roll back to"
            ));
        }
        (None, Some(index)) => &listed[index - 1],
        (None, None) => {
            return Err(format!(
                "the production deployment {serving} is not among the ready ones listed"
            ));
        }
    };
    let description = format!("hl rollback from {serving}");
    api.call(
        ROLLBACK,
        &[("projectId", &project_id), ("deploymentId", &target.1)],
        &[("description", &description)],
        Body::Empty,
    )?;
    context.say(&format!(
        "rollback vercel {}: deployment {} serves production, was {serving}",
        context.name, target.1
    ));
    context.say("  the workflow runs' event logs are not rolled back: the older code replays them as they are now");
    Ok(())
}

fn environment_names(api: &Api) -> Result<Vec<(String, String, String)>, String> {
    let listed = api.call(LIST_ENV, &[], &[], Body::Empty)?;
    let mut names = Vec::new();
    for variable in listed.get("envs").map(Value::items).unwrap_or_default() {
        let key = variable.str_at(&["key"]).ok_or("a variable has no key")?;
        let kind = variable.str_at(&["type"]).unwrap_or("?");
        let id = variable.str_at(&["id"]).unwrap_or("");
        names.push((key.to_owned(), kind.to_owned(), id.to_owned()));
    }
    names.sort();
    Ok(names)
}

fn secrets(api: &Api, secret: Secret) -> Result<(), String> {
    let context = api.context;
    match secret {
        Secret::Set(key) => {
            let value = super::read_secret()?;
            api.call(
                CREATE_ENV,
                &[],
                &[("upsert", "true")],
                Body::Json(environment_variable(
                    &key,
                    &value,
                    if context.plant == Plant::EnvironmentTypeUnknown {
                        "hidden"
                    } else {
                        "encrypted"
                    },
                )),
            )?;
            context.say(&format!(
                "secrets vercel {}: set {key} (deployments after this one read it)",
                context.name
            ));
        }
        Secret::List => {
            for (key, kind, _) in environment_names(api)? {
                if kind != "plain" {
                    context.say(&key);
                }
            }
        }
        Secret::Delete(key) => {
            let names = environment_names(api)?;
            let (_, _, id) = names
                .iter()
                .find(|(name, _, _)| *name == key)
                .ok_or_else(|| format!("project {} has no variable {key}", context.name))?;
            api.call(DELETE_ENV, &[("id", id.as_str())], &[], Body::Empty)?;
            context.say(&format!("secrets vercel {}: deleted {key}", context.name));
        }
    }
    Ok(())
}

fn destroy(api: &Api) -> Result<(), String> {
    let context = api.context;
    api.call(DELETE_PROJECT, &[], &[], Body::Empty)?;
    super::forget(context)?;
    context.say(&format!(
        "destroy vercel {}: deleted the project and its deployments",
        context.name
    ));
    Ok(())
}

fn logs(api: &Api, follow: bool, events: u64) -> Result<(), String> {
    let context = api.context;
    let (project_id, serving) = production(api)?;
    let deployment = serving
        .str_at(&["id"])
        .ok_or("the production deployment has no id")?
        .to_owned();
    if follow {
        context.say(&format!(
            "logs vercel {}: following deployment {deployment}",
            context.name
        ));
        let request = api.request(
            RUNTIME_LOGS,
            &[("projectId", &project_id), ("deploymentId", &deployment)],
            &[],
        );
        let mut failed = None;
        api.context.client.stream(
            &request,
            events,
            None,
            &mut |line| match super::json::decode(line) {
                Ok(entry) => {
                    context.say(&runtime_line(&entry));
                    Flow::More
                }
                Err(error) => {
                    failed = Some(format!("a runtime log line is not JSON: {error}"));
                    Flow::Stop
                }
            },
        )?;
        return failed.map_or(Ok(()), Err);
    }
    let limit = events.to_string();
    let listed = api.call(
        DEPLOYMENT_EVENTS,
        &[("idOrUrl", &deployment)],
        &[
            ("limit", &limit),
            ("direction", "backward"),
            ("builds", "1"),
        ],
        Body::Empty,
    )?;
    let mut lines: Vec<(u64, String)> = listed
        .items()
        .iter()
        .map(|event| {
            let at = event.get("created").and_then(Value::as_u64).unwrap_or(0);
            let kind = event.str_at(&["type"]).unwrap_or("event");
            let text = event
                .str_at(&["text"])
                .or_else(|| event.str_at(&["payload", "text"]))
                .unwrap_or("");
            (at, format!("{} {kind} {text}", super::iso_time(at)))
        })
        .collect();
    lines.sort();
    context.say(&format!(
        "logs vercel {}: {} events of deployment {deployment}",
        context.name,
        lines.len()
    ));
    for (_, line) in lines {
        context.say(&line);
    }
    Ok(())
}

fn monitor(api: &Api, events: u64, minutes: u64) -> Result<(), String> {
    let context = api.context;
    let (project_id, serving) = production(api)?;
    let deployment = serving
        .str_at(&["id"])
        .ok_or("the production deployment has no id")?
        .to_owned();
    context.say(&format!("monitor vercel {}", context.name));
    context.say(&format!("  url https://{}", serving_url(&serving)));
    context.say(&format!(
        "  deployment {deployment} {} serves production",
        serving.str_at(&["readyState"]).unwrap_or("?")
    ));
    let since = context
        .environment
        .now_ms
        .saturating_sub(minutes * MILLISECONDS_PER_MINUTE);
    let request = api.request(
        RUNTIME_LOGS,
        &[("projectId", &project_id), ("deploymentId", &deployment)],
        &[],
    );
    let mut seen = Vec::new();
    let mut failed = None;
    context.client.stream(
        &request,
        events,
        Some(super::MONITOR_FOLLOW_SECONDS),
        &mut |line| match super::json::decode(line) {
            Ok(entry) => {
                let at = entry
                    .get("timestampInMs")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if at >= since {
                    let status = entry
                        .get("responseStatusCode")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    seen.push(super::Seen {
                        at,
                        line: runtime_line(&entry),
                        failed: entry.str_at(&["level"]) == Some("error")
                            || status >= SERVER_ERROR_STATUS,
                    });
                }
                Flow::More
            }
            Err(error) => {
                failed = Some(format!("a runtime log line is not JSON: {error}"));
                Flow::Stop
            }
        },
    )?;
    if let Some(error) = failed {
        return Err(error);
    }
    seen.sort_by(|left, right| left.at.cmp(&right.at).then(left.line.cmp(&right.line)));
    super::traffic(context, minutes, &seen);
    Ok(())
}

fn runtime_line(entry: &Value) -> String {
    let at = entry
        .get("timestampInMs")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    format!(
        "{} {} {} {} {} {}",
        super::iso_time(at),
        entry.str_at(&["level"]).unwrap_or("info"),
        entry.str_at(&["requestMethod"]).unwrap_or(""),
        entry.str_at(&["requestPath"]).unwrap_or(""),
        entry
            .get("responseStatusCode")
            .map(Value::shown)
            .unwrap_or_default(),
        entry.str_at(&["message"]).unwrap_or("")
    )
}

pub fn spec() -> &'static str {
    SPEC
}
