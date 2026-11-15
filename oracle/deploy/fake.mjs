import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { SourceTextModule } from "node:vm";
import { join, dirname, normalize } from "node:path";

const args = Object.fromEntries(process.argv.slice(2).reduce((pairs, word, index, words) => (word.startsWith("--") ? [...pairs, [word.slice(2), words[index + 1]]] : pairs), []));
const CLOUDFLARE_PREFIX = "/cloudflare/client/v4";
const VERCEL_PREFIX = "/vercel";
const TOKEN = args.token;
const EPOCH_MS = Date.parse("2026-11-14T00:00:00Z");
const BODY_BYTES_MAX = 200 * 1024 * 1024;
const LATENCY_MS = Number(args["latency-ms"] ?? 0);
const UNDECLARED_BODY = new Set(["worker-tail-logs-start-tail"]);
const AUTH_SECRET = "HL_AUTH_TOKEN";

const specs = {
  cloudflare: JSON.parse(readFileSync(args["cloudflare-spec"], "utf8")),
  vercel: JSON.parse(readFileSync(args["vercel-spec"], "utf8")),
};

const violations = [];
const exercised = new Set();
let clock = 0;
let counter = 0;
const now = () => EPOCH_MS + 1000 * ++clock;
const iso = (ms) => new Date(ms).toISOString();
const next = () => ++counter;
const uuid = (kind, n) => kind + "0000000-0000-4000-8000-" + String(n).padStart(12, "0");

function violation(where, what) {
  violations.push(where + ": " + what);
}

function resolver(spec) {
  return (schema) => {
    let seen = 0;
    while (schema && schema.$ref) {
      if (++seen > 64) throw new Error("reference cycle at " + schema.$ref);
      let value = spec;
      for (const part of schema.$ref.slice(2).split("/")) value = value[part.replaceAll("~1", "/").replaceAll("~0", "~")];
      schema = value;
    }
    return schema;
  };
}

function declared(spec, schema, depth = 0) {
  const deref = resolver(spec);
  schema = deref(schema);
  if (!schema || depth > 32) return null;
  if (schema.additionalProperties) return null;
  let keys = new Set(Object.keys(schema.properties ?? {}));
  let open = schema.properties === undefined && schema.allOf === undefined;
  for (const part of schema.allOf ?? []) {
    const inner = declared(spec, part, depth + 1);
    if (inner === null) return null;
    for (const key of inner) keys.add(key);
    open = false;
  }
  if (open && !schema.oneOf && !schema.anyOf) return null;
  if (schema.oneOf || schema.anyOf) {
    if (schema.properties === undefined && schema.allOf === undefined) return null;
  }
  return keys;
}

function typeOf(value) {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  return typeof value;
}

function validate(spec, schema, value, path, strict, mode, errors, depth = 0) {
  const deref = resolver(spec);
  schema = deref(schema);
  if (!schema || depth > 64) return errors;
  if (value === null && schema.nullable) return errors;
  if (schema.allOf) {
    for (const part of schema.allOf) validate(spec, part, value, path, false, mode, errors, depth + 1);
  }
  for (const key of ["oneOf", "anyOf"]) {
    if (!schema[key]) continue;
    const branches = schema[key].map((branch) => validate(spec, branch, value, path, strict, mode, [], depth + 1));
    if (!branches.some((branch) => branch.length === 0)) {
      const best = branches.reduce((left, right) => (right.length < left.length ? right : left));
      errors.push(path + ": matches none of " + key + " (closest: " + best.slice(0, 3).join("; ") + ")");
    }
  }
  if (schema.enum && !schema.enum.some((option) => option === value || String(option) === String(value))) {
    errors.push(path + ": " + JSON.stringify(value) + " is not one of " + JSON.stringify(schema.enum));
  }
  const type = schema.type ?? (schema.properties ? "object" : undefined);
  const actual = typeOf(value);
  if (type === "integer" && !(actual === "number" && Number.isInteger(value))) errors.push(path + ": expected integer, got " + actual);
  else if (type === "number" && actual !== "number") errors.push(path + ": expected number, got " + actual);
  else if (type === "string" && actual !== "string") errors.push(path + ": expected string, got " + actual);
  else if (type === "boolean" && actual !== "boolean") errors.push(path + ": expected boolean, got " + actual);
  else if (type === "array" && actual !== "array") errors.push(path + ": expected array, got " + actual);
  else if (type === "object" && actual !== "object") errors.push(path + ": expected object, got " + actual);
  if (actual === "string") {
    if (schema.maxLength !== undefined && value.length > schema.maxLength) errors.push(path + ": longer than " + schema.maxLength);
    if (schema.minLength !== undefined && value.length < schema.minLength) errors.push(path + ": shorter than " + schema.minLength);
    if (schema.pattern && !new RegExp(schema.pattern, "u").test(value)) errors.push(path + ": does not match " + schema.pattern);
  }
  if (actual === "array" && schema.items) {
    value.forEach((item, index) => validate(spec, schema.items, item, path + "[" + index + "]", strict, mode, errors, depth + 1));
  }
  if (actual === "object") {
    const properties = schema.properties ?? {};
    for (const [key, inner] of Object.entries(properties)) {
      if (key in value) validate(spec, inner, value[key], path + "." + key, strict, mode, errors, depth + 1);
    }
    if (typeof schema.additionalProperties === "object") {
      for (const [key, inner] of Object.entries(value)) {
        if (!(key in properties)) validate(spec, schema.additionalProperties, inner, path + "." + key, strict, mode, errors, depth + 1);
      }
    }
    if (mode === "request") {
      for (const key of schema.required ?? []) if (!(key in value) && !deref(properties[key])?.readOnly) errors.push(path + ": lacks required " + key);
    }
    for (const [key, inner] of Object.entries(properties)) {
      if (!(key in value)) continue;
      if (mode === "request" && deref(inner)?.readOnly) errors.push(path + "." + key + " is read-only, a field the server sets");
      if (mode === "response" && deref(inner)?.writeOnly) errors.push(path + "." + key + " is write-only, a field only requests carry");
    }
    if (strict) {
      const keys = declared(spec, schema);
      if (keys !== null) for (const key of Object.keys(value)) if (!keys.has(key)) errors.push(path + ": " + key + " is not declared");
    }
  }
  return errors;
}

function routes(spec) {
  const table = [];
  for (const [template, operations] of Object.entries(spec.paths)) {
    const names = [];
    const pattern = new RegExp("^" + template.replace(/[.*+?^$()|[\]\\]/g, "\\$&").replace(/\{([^}]+)\}/g, (_, name) => (names.push(name), "([^/]+)")) + "$");
    for (const [method, operation] of Object.entries(operations)) {
      if (operation && operation.operationId) table.push({ method: method.toUpperCase(), template, pattern, names, operation });
    }
  }
  return table;
}

const tables = { cloudflare: routes(specs.cloudflare), vercel: routes(specs.vercel) };

function match(provider, method, path) {
  const candidates = tables[provider].filter((route) => route.method === method && route.pattern.test(path));
  candidates.sort((left, right) => right.template.replace(/\{[^}]+\}/g, "").length - left.template.replace(/\{[^}]+\}/g, "").length);
  const route = candidates[0];
  if (!route) return null;
  const found = route.pattern.exec(path);
  const params = Object.fromEntries(route.names.map((name, index) => [name, decodeURIComponent(found[index + 1])]));
  return { ...route, params };
}

function parameterValue(spec, schema, text) {
  const deref = resolver(spec);
  schema = deref(schema) ?? {};
  const options = schema.oneOf ? schema.oneOf.map(deref) : [schema];
  for (const option of options) {
    if (option.type === "boolean" && (text === "true" || text === "false")) return text === "true";
    if ((option.type === "number" || option.type === "integer") && /^-?\d+(\.\d+)?$/.test(text)) return Number(text);
    if (option.type === "array") return text.split(",");
  }
  return text;
}

function checkRequest(provider, route, url, headers, where) {
  const spec = specs[provider];
  const deref = resolver(spec);
  const parameters = (route.operation.parameters ?? []).map(deref);
  for (const parameter of parameters.filter((parameter) => parameter.in === "path")) {
    for (const error of validate(spec, parameter.schema, route.params[parameter.name], "path " + parameter.name, true, "request", [])) violation(where, error);
  }
  const declaredQuery = new Map(parameters.filter((parameter) => parameter.in === "query").map((parameter) => [parameter.name, parameter]));
  for (const [key, text] of url.searchParams) {
    const parameter = declaredQuery.get(key);
    if (!parameter) {
      violation(where, "query " + key + " is not a parameter of " + route.operation.operationId);
      continue;
    }
    for (const error of validate(spec, parameter.schema, parameterValue(spec, parameter.schema, text), "query " + key, true, "request", [])) violation(where, error);
  }
  for (const parameter of parameters.filter((parameter) => parameter.in === "header" && parameter.required)) {
    if (headers[parameter.name.toLowerCase()] === undefined) violation(where, "lacks required header " + parameter.name);
  }
}

function parseMultipart(body, contentType) {
  const boundary = /boundary=([^;]+)/.exec(contentType)?.[1];
  if (!boundary) return null;
  const marker = Buffer.from("--" + boundary);
  const parts = [];
  let offset = body.indexOf(marker);
  while (offset >= 0) {
    offset += marker.length;
    if (body.slice(offset, offset + 2).toString() === "--") break;
    offset += 2;
    const end = body.indexOf(marker, offset);
    if (end < 0) return null;
    const chunk = body.slice(offset, end - 2);
    const split = chunk.indexOf("\r\n\r\n");
    const head = chunk.slice(0, split).toString();
    const disposition = /name="([^"]*)"(?:; filename="([^"]*)")?/.exec(head) ?? [];
    parts.push({ name: disposition[1], fileName: disposition[2], type: /Content-Type: (.*)/i.exec(head)?.[1]?.trim(), bytes: chunk.slice(split + 4) });
    offset = end;
  }
  return parts;
}

function checkBody(provider, route, headers, body, where) {
  const spec = specs[provider];
  const deref = resolver(spec);
  const declaredBody = deref(route.operation.requestBody);
  const type = (headers["content-type"] ?? "").split(";")[0].trim();
  if (!declaredBody) {
    if (body.length > 0 && !UNDECLARED_BODY.has(route.operation.operationId)) violation(where, "sends a body that " + route.operation.operationId + " does not declare");
    return { json: body.length > 0 && type === "application/json" ? JSON.parse(body.toString()) : null };
  }
  if (body.length === 0) {
    if (declaredBody.required) violation(where, "lacks the required body");
    return {};
  }
  const content = declaredBody.content ?? {};
  if (!(type in content)) {
    violation(where, "sends " + type + ", which " + route.operation.operationId + " does not accept (" + Object.keys(content).join(", ") + ")");
    return {};
  }
  if (type === "application/json") {
    let json;
    try {
      json = JSON.parse(body.toString());
    } catch (error) {
      violation(where, "body is not JSON: " + error.message);
      return {};
    }
    for (const error of validate(spec, content[type].schema, json, "body", true, "request", [])) violation(where, error);
    return { json };
  }
  if (type === "multipart/form-data") {
    const parts = parseMultipart(body, headers["content-type"]);
    if (!parts) {
      violation(where, "multipart body does not parse");
      return {};
    }
    const schema = deref(content[type].schema);
    const merged = (schema.allOf ?? [schema]).map(deref);
    const metadataPart = parts.find((part) => part.name === "metadata");
    if (!metadataPart) {
      violation(where, "multipart body has no metadata part");
      return { parts };
    }
    if (metadataPart.type !== "application/json") violation(where, "metadata part is " + metadataPart.type + ", not application/json");
    const metadata = JSON.parse(metadataPart.bytes.toString());
    const metadataSchema = { allOf: merged.map((part) => deref(part).properties?.metadata).filter(Boolean) };
    for (const error of validate(spec, metadataSchema, metadata, "metadata", true, "request", [])) violation(where, error);
    const allowed = (content[type].encoding?.files?.contentType ?? "").split(",").map((text) => text.trim());
    for (const part of parts.filter((part) => part.name !== "metadata")) {
      if (!allowed.includes(part.type)) violation(where, "module " + part.name + " has content type " + part.type + ", which the upload does not accept");
    }
    return { parts, metadata };
  }
  return {};
}

function checkResponse(provider, route, status, value, where) {
  const spec = specs[provider];
  const deref = resolver(spec);
  const responses = route.operation.responses ?? {};
  const response = deref(responses[String(status)] ?? responses[String(status)[0] + "XX"] ?? responses.default);
  if (!response) {
    violation(where, "the fake answered " + status + ", which " + route.operation.operationId + " does not declare");
    return;
  }
  const schema = response.content?.["application/json"]?.schema;
  if (!schema || value === undefined) return;
  for (const error of validate(spec, schema, value, "fake response", true, "response", [])) violation(where + " (fake)", error);
}

const sha1 = (bytes) => createHash("sha1").update(bytes).digest("hex");
const sha256 = (text) => createHash("sha256").update(text).digest("hex");
const digest = (files) =>
  sha256(
    [...files]
      .sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0))
      .map((file) => file.sha + " " + file.size + " " + file.path + "\n")
      .join(""),
  );

function importsOf(text) {
  const found = [];
  const pattern = /(?:import|export)\s[^"'\x60;]*?from\s*["']([^"']+)["']|import\s*\(\s*["']([^"']+)["']\s*\)|import\s*["']([^"']+)["']/g;
  for (const hit of text.matchAll(pattern)) found.push(hit[1] ?? hit[2] ?? hit[3]);
  return found;
}

if (typeof SourceTextModule !== "function") throw new Error("run the fake with node --experimental-vm-modules: it parses every uploaded module in process");

function buildCheck(files) {
  const problems = [];
  const names = new Set(files.map((file) => file.path));
  for (const file of files.filter((file) => /\.m?js$/.test(file.path))) {
    const text = file.bytes.toString();
    for (const specifier of importsOf(text)) {
      if (!specifier.startsWith(".")) continue;
      const resolved = normalize(join(dirname(file.path), specifier));
      if (!names.has(resolved)) problems.push(file.path + " imports " + specifier + ", which the upload lacks");
    }
    try {
      new SourceTextModule(text, { identifier: file.path });
    } catch (error) {
      problems.push(file.path + " does not parse: " + error.name + ": " + error.message);
    }
  }
  return problems;
}

const cloudflare = { scripts: new Map(), subdomain: "hl-fake", tails: new Map() };
const vercel = { projects: new Map(), blobs: new Map(), deployments: new Map() };

function envelope(result) {
  const out = { success: true, errors: [], messages: [] };
  if (result !== undefined) out.result = result;
  return out;
}

function cloudflareError(status, code, message) {
  return { status, value: { success: false, errors: [{ code, message }], messages: [], result: null } };
}

function versionSummary(version) {
  return { id: version.id, number: version.number, metadata: { created_on: version.created_on, modified_on: version.created_on, source: "api", author_email: "hl@example.com" } };
}

function scriptSummary(name, script) {
  const serving = script.versions.find((version) => version.id === currentVersion(script));
  return { id: name, etag: serving.digest, created_on: script.created_on, modified_on: serving.created_on, migration_tag: script.migration_tag ?? undefined, compatibility_date: serving.compatibility_date, handlers: ["fetch"], has_modules: true, usage_model: "standard" };
}

function currentVersion(script) {
  const latest = script.deployments[script.deployments.length - 1];
  return latest.versions.find((version) => version.percentage === 100)?.version_id;
}

function deployVersion(script, version, annotations) {
  const at = now();
  const deployment = { id: uuid("d", next()), created_on: iso(at), source: "api", strategy: "percentage", versions: [{ version_id: version.id, percentage: 100 }], author_email: "hl@example.com" };
  if (annotations) deployment.annotations = annotations;
  script.deployments.push(deployment);
  return deployment;
}

function newVersion(script, fields) {
  const at = now();
  const version = { ...fields, id: uuid("a", next()), number: script.versions.length + 1, created_on: iso(at) };
  script.versions.push(version);
  return version;
}

function uploadScript(name, parts, metadata) {
  const modules = parts.filter((part) => part.name !== "metadata").map((part) => ({ path: part.fileName ?? part.name, bytes: part.bytes, sha: sha1(part.bytes), size: part.bytes.length }));
  if (!modules.some((module) => module.path === metadata.main_module)) return cloudflareError(400, 10021, "main_module " + metadata.main_module + " is not among the uploaded modules");
  const problems = buildCheck(modules);
  if (problems.length > 0) return cloudflareError(400, 10021, "Uncaught error while validating the script: " + problems.join("; "));
  let script = cloudflare.scripts.get(name);
  const classes = new Set(script?.classes ?? []);
  let tag = script?.migration_tag ?? null;
  const migrations = metadata.migrations;
  if (migrations) {
    if ((migrations.old_tag ?? null) !== tag) return cloudflareError(400, 10079, "Migration tag precondition failed; current tag is " + JSON.stringify(tag) + ", old_tag " + JSON.stringify(migrations.old_tag ?? null));
    for (const added of [...(migrations.new_sqlite_classes ?? []), ...(migrations.new_classes ?? [])]) {
      if (classes.has(added)) return cloudflareError(400, 10074, "Cannot apply new-sqlite-class migration to class '" + added + "' that is already depended on by existing Durable Objects");
      classes.add(added);
    }
    tag = migrations.new_tag ?? tag;
  }
  const source = modules.map((module) => module.bytes.toString()).join("\n");
  for (const binding of (metadata.bindings ?? []).filter((binding) => binding.type === "durable_object_namespace" && !binding.script_name)) {
    if (!classes.has(binding.class_name)) return cloudflareError(400, 10061, "Cannot create binding for class '" + binding.class_name + "' because it is not currently configured to implement Durable Objects. Did you forget to apply a migration?");
    if (!new RegExp("class\\s+" + binding.class_name + "\\b").test(source) || !new RegExp("export\\s*\\{[^}]*\\b" + binding.class_name + "\\b|export\\s+class\\s+" + binding.class_name + "\\b").test(source)) {
      return cloudflareError(400, 10061, "Durable Object class '" + binding.class_name + "' is not exported by the main module");
    }
  }
  if (!script) {
    script = { created_on: iso(now()), versions: [], deployments: [], events: [] };
    cloudflare.scripts.set(name, script);
  }
  let bindings = metadata.bindings ?? [];
  const keep = new Set(metadata.keep_bindings ?? []);
  if (script.versions.length > 0) {
    const previous = script.versions.find((version) => version.id === currentVersion(script));
    const kept = previous.bindings.filter((binding) => keep.has(binding.type) && !bindings.some((other) => other.name === binding.name));
    bindings = [...bindings, ...kept];
  }
  script.classes = [...classes];
  script.migration_tag = tag;
  const version = newVersion(script, { bindings, modules, digest: digest(modules), main: metadata.main_module, compatibility_date: metadata.compatibility_date });
  deployVersion(script, version, metadata.annotations);
  return { status: 200, value: envelope(scriptSummary(name, script)) };
}

function serving(script) {
  return script.versions.find((version) => version.id === currentVersion(script));
}

function secretVersion(script, change) {
  const previous = serving(script);
  const version = newVersion(script, { ...previous, bindings: change(previous.bindings) });
  deployVersion(script, version, { "workers/triggered_by": "secret" });
}

function handleCloudflare(route, request, body, url) {
  const id = route.operation.operationId;
  const name = route.params.script_name;
  const script = name ? cloudflare.scripts.get(name) : undefined;
  const missing = () => cloudflareError(404, 10007, "This Worker does not exist on your account.");
  switch (id) {
    case "worker-script-list-workers":
      return { status: 200, value: envelope([...cloudflare.scripts.entries()].map(([key, value]) => scriptSummary(key, value))) };
    case "worker-script-upload-worker-module":
      if (!body.parts) return cloudflareError(400, 10021, "expected a multipart upload");
      return uploadScript(name, body.parts, body.metadata);
    case "worker-subdomain-get-subdomain":
      return { status: 200, value: envelope({ subdomain: cloudflare.subdomain }) };
    case "worker-script-post-subdomain":
      if (!script) return missing();
      script.subdomain = body.json;
      return { status: 200, value: envelope({ enabled: body.json.enabled, previews_enabled: body.json.previews_enabled ?? false }) };
    case "worker-deployments-list-deployments":
      if (!script) return missing();
      return { status: 200, value: envelope({ deployments: [...script.deployments].reverse() }) };
    case "worker-deployments-create-deployment": {
      if (!script) return missing();
      const total = body.json.versions.reduce((sum, version) => sum + version.percentage, 0);
      if (total !== 100) return cloudflareError(400, 10204, "version percentages add up to " + total + ", not 100");
      for (const version of body.json.versions) if (!script.versions.some((known) => known.id === version.version_id)) return cloudflareError(400, 10205, "no version " + version.version_id);
      const deployment = { id: uuid("d", next()), created_on: iso(now()), source: "api", strategy: body.json.strategy, versions: body.json.versions, author_email: "hl@example.com", annotations: body.json.annotations };
      script.deployments.push(deployment);
      return { status: 200, value: envelope(deployment) };
    }
    case "worker-versions-list-versions":
      if (!script) return missing();
      return { status: 200, value: envelope({ items: [...script.versions].reverse().map(versionSummary) }) };
    case "worker-list-script-secrets":
      if (!script) return missing();
      return { status: 200, value: envelope(serving(script).bindings.filter((binding) => binding.type === "secret_text").map((binding) => ({ name: binding.name, type: "secret_text" }))) };
    case "worker-put-script-secret":
      if (!script) return missing();
      secretVersion(script, (bindings) => [...bindings.filter((binding) => binding.name !== body.json.name), { type: "secret_text", name: body.json.name, text: body.json.text }]);
      return { status: 200, value: envelope({ name: body.json.name, type: "secret_text" }) };
    case "worker-delete-script-secret": {
      if (!script) return missing();
      if (!serving(script).bindings.some((binding) => binding.type === "secret_text" && binding.name === route.params.secret_name)) return cloudflareError(404, 10056, "secret not found");
      secretVersion(script, (bindings) => bindings.filter((binding) => binding.name !== route.params.secret_name));
      return { status: 200, value: envelope(null) };
    }
    case "worker-tail-logs-start-tail": {
      if (!script) return missing();
      const tail = String(next()).padStart(32, "0");
      cloudflare.tails.set(tail, name);
      return { status: 200, value: envelope({ id: tail, url: "ws://" + request.headers.host + "/tail/" + tail, expires_at: iso(now() + 6 * 3600 * 1000) }) };
    }
    case "worker-tail-logs-delete-tail":
      if (!cloudflare.tails.delete(route.params.id)) return cloudflareError(404, 10057, "no tail " + route.params.id);
      return { status: 200, value: envelope(undefined) };
    case "telemetry.query": {
      const query = body.json;
      const service = query.parameters?.filters?.find((filter) => filter.key === "$metadata.service")?.value;
      const events = (cloudflare.scripts.get(service)?.events ?? [])
        .filter((event) => event.at >= query.timeframe.from && event.at <= query.timeframe.to)
        .slice(-(query.limit ?? 50))
        .map((event) => ({ dataset: "cloudflare-workers", timestamp: event.at, source: event.message, $metadata: { id: "event-" + event.at, level: event.level, message: event.message, service }, $workers: { eventType: "fetch", scriptName: service, outcome: "ok", requestId: "request-" + event.at } }));
      return { status: 200, value: envelope({ events: { count: events.length, events } }) };
    }
    case "worker-script-delete-worker":
      if (!script) return missing();
      if (script.classes.length > 0 && url.searchParams.get("force") !== "true") return cloudflareError(400, 10064, "the script has Durable Objects; deleting it needs force=true");
      cloudflare.scripts.delete(name);
      return { status: 200, value: envelope(null) };
    default:
      return cloudflareError(501, 10000, "the fake does not implement " + id);
  }
}

function vercelError(status, code, message, extra = {}) {
  return { status, value: { error: { code, message, ...extra } } };
}

function project(key) {
  for (const [name, value] of vercel.projects) if (name === key || value.id === key) return value;
  return undefined;
}

function deploymentSummary(deployment) {
  return { id: deployment.id, url: deployment.url, name: deployment.name, readyState: deployment.readyState, createdAt: deployment.createdAt, alias: deployment.alias, target: deployment.target, meta: deployment.meta, type: "LAMBDAS", plan: "hobby", private: false, createdIn: "sfo1", creator: { uid: "user_hl" }, deploymentHostname: deployment.url };
}

function projectValue(value) {
  const out = { id: value.id, name: value.name, accountId: "team_hl", framework: value.framework ?? null, nodeVersion: "24.x", createdAt: value.createdAt, updatedAt: value.createdAt, targets: {} };
  if (value.production) out.targets.production = deploymentSummary(vercel.deployments.get(value.production));
  return out;
}

const BUILD_STEPS = 2;

function advance(deployment) {
  const steps = ["QUEUED", "BUILDING"];
  const index = steps.indexOf(deployment.readyState);
  if (index < 0) return;
  if (index + 1 < steps.length) {
    deployment.readyState = steps[index + 1];
    return;
  }
  if (deployment.problems.length > 0) {
    deployment.readyState = "ERROR";
    deployment.errorMessage = deployment.problems.join("; ");
    return;
  }
  deployment.readyState = "READY";
  const owner = project(deployment.projectId);
  if (deployment.target === "production") {
    deployment.alias = [owner.name + ".vercel.app"];
    owner.production = deployment.id;
  }
}

function deploymentValue(deployment) {
  const out = { id: deployment.id, url: deployment.url, name: deployment.name, readyState: deployment.readyState, createdAt: deployment.createdAt, projectId: deployment.projectId, target: deployment.target, alias: deployment.alias, aliasAssigned: deployment.readyState === "READY", meta: deployment.meta };
  if (deployment.errorMessage) out.errorMessage = deployment.errorMessage;
  return out;
}

function handleVercel(route, request, body, url, raw) {
  const id = route.operation.operationId;
  switch (id) {
    case "getProject": {
      const found = project(route.params.idOrName);
      if (!found) return vercelError(404, "not_found", "Project not found");
      return { status: 200, value: projectValue(found) };
    }
    case "createProject": {
      if (project(body.json.name)) return vercelError(409, "conflict", "Project already exists");
      const env = (body.json.environmentVariables ?? []).map((variable) => ({ id: "env_" + next(), key: variable.key, value: variable.value, type: variable.type ?? "encrypted", target: [variable.target].flat(), createdAt: now() }));
      const created = { id: "prj_" + next(), name: body.json.name, framework: body.json.framework, createdAt: now(), env, events: [] };
      vercel.projects.set(created.name, created);
      return { status: 200, value: projectValue(created) };
    }
    case "deleteProject": {
      const found = project(route.params.idOrName);
      if (!found) return vercelError(404, "not_found", "Project not found");
      vercel.projects.delete(found.name);
      return { status: 204 };
    }
    case "uploadFile": {
      const claimed = request.headers["x-vercel-digest"];
      if (claimed !== sha1(raw)) return vercelError(400, "invalid_digest", "The digest " + claimed + " does not match the file's SHA-1");
      if (Number(request.headers["content-length"]) !== raw.length) return vercelError(400, "invalid_size", "Content-Length does not match the file");
      vercel.blobs.set(claimed, raw);
      return { status: 200, value: {} };
    }
    case "createDeployment": {
      const owner = project(body.json.project ?? body.json.name);
      if (!owner) return vercelError(404, "not_found", "Project not found");
      const missing = body.json.files.filter((file) => !vercel.blobs.has(file.sha)).map((file) => file.sha);
      if (missing.length > 0) return vercelError(400, "missing_files", "Missing files", { missing });
      const files = body.json.files.map((file) => ({ path: file.file, sha: file.sha, size: file.size, bytes: vercel.blobs.get(file.sha) }));
      const problems = files.filter((file) => file.size !== file.bytes.length).map((file) => file.path + " has size " + file.size + " but its upload has " + file.bytes.length);
      if (owner.framework === "nextjs") {
        const manifest = files.find((file) => file.path === "package.json");
        const dependencies = manifest ? JSON.parse(manifest.bytes.toString()).dependencies ?? {} : {};
        for (const needed of ["next", "workflow"]) if (!dependencies[needed]) problems.push("package.json lacks the " + needed + " dependency a Next.js workflow app builds with");
      }
      problems.push(...buildCheck(files));
      const number = next();
      const deployment = { id: "dpl_" + number, url: owner.name + "-" + number + ".vercel.app", name: owner.name, readyState: "QUEUED", createdAt: now(), projectId: owner.id, target: body.json.target ?? null, alias: [], files, digest: digest(files), meta: body.json.meta ?? {}, problems };
      vercel.deployments.set(deployment.id, deployment);
      return { status: 200, value: deploymentValue(deployment) };
    }
    case "getDeployment": {
      const deployment = vercel.deployments.get(route.params.idOrUrl);
      if (!deployment) return vercelError(404, "not_found", "Deployment not found");
      advance(deployment);
      return { status: 200, value: deploymentValue(deployment) };
    }
    case "getDeployments": {
      const listed = [...vercel.deployments.values()]
        .filter((deployment) => (!url.searchParams.get("projectId") || deployment.projectId === url.searchParams.get("projectId")) && (!url.searchParams.get("target") || deployment.target === url.searchParams.get("target")) && (!url.searchParams.get("state") || url.searchParams.get("state").split(",").includes(deployment.readyState)))
        .sort((left, right) => right.createdAt - left.createdAt)
        .slice(0, Number(url.searchParams.get("limit") ?? 20))
        .map((deployment) => ({ uid: deployment.id, name: deployment.name, url: deployment.url, created: deployment.createdAt, createdAt: deployment.createdAt, state: deployment.readyState, readyState: deployment.readyState, target: deployment.target, projectId: deployment.projectId, creator: { uid: "user_hl" }, inspectorUrl: "https://vercel.com/hl/" + deployment.name + "/" + deployment.id }));
      return { status: 200, value: { deployments: listed, pagination: { count: listed.length, next: null, prev: null } } };
    }
    case "getDeploymentEvents": {
      const deployment = vercel.deployments.get(route.params.idOrUrl);
      if (!deployment) return vercelError(404, "not_found", "Deployment not found");
      const lines = ["Cloning " + deployment.files.length + " files", "Running \"npm run build\"", "Build completed"];
      const events = lines.map((text, index) => ({ type: "stdout", created: deployment.createdAt + index, payload: { id: deployment.id + "-" + index, date: deployment.createdAt + index, created: deployment.createdAt + index, deploymentId: deployment.id, serial: String(index), text, info: { type: "build", name: "bld_" + deployment.id } } }));
      if (url.searchParams.get("follow") === "1") {
        for (let step = 0; step < BUILD_STEPS && ["QUEUED", "BUILDING"].includes(deployment.readyState); step += 1) advance(deployment);
        return { status: 200, stream: events.map((event) => JSON.stringify(event)), type: "application/stream+json" };
      }
      return { status: 200, value: events.slice(-Number(url.searchParams.get("limit") ?? events.length)) };
    }
    case "getRuntimeLogs": {
      const owner = project(route.params.projectId);
      if (!owner) return vercelError(404, "not_found", "Project not found");
      const lines = owner.events.map((event, index) => JSON.stringify({ rowId: "row-" + index, timestampInMs: event.at, level: event.level, message: event.message, messageTruncated: false, requestMethod: "POST", requestPath: "/sessions/s1/start", responseStatusCode: 200, source: "serverless", domain: owner.name + ".vercel.app" }));
      return { status: 200, stream: lines, type: "application/stream+json" };
    }
    case "requestRollback": {
      const owner = project(route.params.projectId);
      const deployment = vercel.deployments.get(route.params.deploymentId);
      if (!owner || !deployment || deployment.projectId !== owner.id) return vercelError(404, "not_found", "Deployment not found");
      if (deployment.readyState !== "READY" || deployment.target !== "production") return vercelError(400, "bad_request", "Only ready production deployments can be rolled back to");
      if (owner.production === deployment.id) return vercelError(409, "conflict", "The deployment is already the production deployment");
      owner.production = deployment.id;
      return { status: 201 };
    }
    case "filterProjectEnvs": {
      const owner = project(route.params.idOrName);
      if (!owner) return vercelError(404, "not_found", "Project not found");
      return { status: 200, value: { envs: owner.env.map((variable) => ({ id: variable.id, key: variable.key, type: variable.type, target: variable.target, value: variable.type === "plain" ? variable.value : "", createdAt: variable.createdAt })), pagination: { count: owner.env.length, next: null, prev: null } } };
    }
    case "createProjectEnv": {
      const owner = project(route.params.idOrName);
      if (!owner) return vercelError(404, "not_found", "Project not found");
      const created = [];
      for (const variable of [body.json].flat()) {
        const existing = owner.env.find((other) => other.key === variable.key);
        if (existing && url.searchParams.get("upsert") !== "true") return vercelError(400, "ENV_ALREADY_EXISTS", "A variable with the name " + variable.key + " already exists");
        owner.env = owner.env.filter((other) => other.key !== variable.key);
        const stored = { id: existing?.id ?? "env_" + next(), key: variable.key, value: variable.value, type: variable.type, target: variable.target, createdAt: now() };
        owner.env.push(stored);
        created.push({ id: stored.id, key: stored.key, type: stored.type, target: stored.target, value: "" });
      }
      return { status: 201, value: { created, failed: [] } };
    }
    case "removeProjectEnv": {
      const owner = project(route.params.idOrName);
      const variable = owner?.env.find((other) => other.id === route.params.id);
      if (!variable) return vercelError(404, "not_found", "Environment variable not found");
      owner.env = owner.env.filter((other) => other.id !== variable.id);
      return { status: 200, value: { id: variable.id, key: variable.key, type: variable.type, target: variable.target, value: "" } };
    }
    default:
      return vercelError(501, "not_implemented", "the fake does not implement " + id);
  }
}

function fakeControl(request, url, raw) {
  const parts = url.pathname.split("/").filter(Boolean);
  if (parts[1] === "traffic") {
    const counted = { requests: traffic.requests, rounds: traffic.rounds };
    traffic.requests = 0;
    traffic.rounds = 0;
    return counted;
  }
  if (parts[1] === "report") return { violations, exercised: [...exercised].sort() };
  if (parts[1] === "cloudflare") {
    const script = cloudflare.scripts.get(parts[2]);
    if (!script) return { exists: false };
    if (parts[3] === "invoke") {
      script.events.push({ at: now(), level: url.searchParams.get("level") ?? "log", message: raw.toString() || "invoked" });
      return { events: script.events.length };
    }
    const version = serving(script);
    const auth = version.bindings.find((binding) => binding.type === "secret_text" && binding.name === AUTH_SECRET);
    return { exists: true, digest: version.digest, version: version.id, versions: script.versions.length, secrets: version.bindings.filter((binding) => binding.type === "secret_text").map((binding) => binding.name).sort(), migration_tag: script.migration_tag, auth_sha256: auth ? sha256(auth.text) : null };
  }
  if (parts[1] === "vercel") {
    const owner = project(parts[2]);
    if (!owner) return { exists: false };
    if (parts[3] === "invoke") {
      owner.events.push({ at: now(), level: url.searchParams.get("level") ?? "info", message: raw.toString() || "invoked" });
      return { events: owner.events.length };
    }
    const deployment = owner.production ? vercel.deployments.get(owner.production) : null;
    const auth = owner.env.find((variable) => variable.key === AUTH_SECRET && variable.type !== "plain");
    return { exists: true, digest: deployment?.digest ?? null, version: owner.production ?? null, versions: [...vercel.deployments.values()].filter((other) => other.projectId === owner.id).length, secrets: owner.env.filter((variable) => variable.type !== "plain").map((variable) => variable.key).sort(), auth_sha256: auth ? sha256(auth.value) : null };
  }
  return { error: "unknown control " + url.pathname };
}

function respond(response, status, value, type = "application/json") {
  const text = value === undefined ? "" : JSON.stringify(value);
  response.writeHead(status, { "content-type": type, "content-length": Buffer.byteLength(text) });
  response.end(text);
}

const traffic = { requests: 0, rounds: 0, inflight: 0 };

const server = createServer((request, response) => {
  if (!request.url.startsWith("/__fake/")) {
    traffic.requests += 1;
    if (traffic.inflight === 0) traffic.rounds += 1;
    traffic.inflight += 1;
    response.on("close", () => {
      traffic.inflight -= 1;
    });
  }
  const chunks = [];
  let size = 0;
  request.on("data", (chunk) => {
    size += chunk.length;
    if (size > BODY_BYTES_MAX) request.destroy();
    else chunks.push(chunk);
  });
  request.on("end", () => setTimeout(() => {
    const raw = Buffer.concat(chunks);
    const url = new URL(request.url, "http://" + request.headers.host);
    if (url.pathname.startsWith("/__fake/")) return respond(response, 200, fakeControl(request, url, raw));
    const provider = url.pathname.startsWith(CLOUDFLARE_PREFIX) ? "cloudflare" : url.pathname.startsWith(VERCEL_PREFIX) ? "vercel" : null;
    if (!provider) return respond(response, 404, { error: "no provider at " + url.pathname });
    const path = url.pathname.slice((provider === "cloudflare" ? CLOUDFLARE_PREFIX : VERCEL_PREFIX).length);
    const where = request.method + " " + provider + " " + path;
    const route = match(provider, request.method, path);
    if (!route) {
      violation(where, "no operation of the pinned spec");
      return respond(response, 404, provider === "cloudflare" ? cloudflareError(404, 7003, "No route for that URI").value : vercelError(404, "not_found", "no route").value);
    }
    exercised.add(provider + " " + route.operation.operationId);
    if (request.headers.authorization !== "Bearer " + TOKEN) {
      return respond(response, 403, provider === "cloudflare" ? cloudflareError(403, 10000, "Authentication error").value : vercelError(403, "forbidden", "Not authorized").value);
    }
    if (request.headers["user-agent"] === undefined) violation(where, "sends no User-Agent");
    checkRequest(provider, route, url, request.headers, where);
    const body = route.operation.operationId === "uploadFile" ? {} : checkBody(provider, route, request.headers, raw, where);
    const answer = provider === "cloudflare" ? handleCloudflare(route, request, body, url) : handleVercel(route, request, body, url, raw);
    if (answer.stream) {
      response.writeHead(answer.status, { "content-type": answer.type });
      for (const line of answer.stream) response.write(line + "\n");
      return response.end();
    }
    checkResponse(provider, route, answer.status, answer.value, where);
    respond(response, answer.status, answer.value);
  }, request.url.startsWith("/__fake/") ? 0 : LATENCY_MS));
});

server.on("upgrade", (request, socket) => {
  const tail = /^\/tail\/([0-9]+)$/.exec(request.url)?.[1];
  const name = tail && cloudflare.tails.get(tail);
  if (!name || request.headers["sec-websocket-protocol"] !== "trace-v1") {
    socket.end("HTTP/1.1 400 Bad Request\r\n\r\n");
    violation("tail " + request.url, "upgrade without a live tail or without protocol trace-v1");
    return;
  }
  const accept = createHash("sha1").update(request.headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest("base64");
  socket.write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + "\r\nSec-WebSocket-Protocol: trace-v1\r\n\r\n");
  let buffer = Buffer.alloc(0);
  let configured = false;
  socket.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 2) {
      const masked = (buffer[1] & 0x80) !== 0;
      let length = buffer[1] & 0x7f;
      let offset = 2;
      if (length === 126) (length = buffer.readUInt16BE(2)), (offset = 4);
      else if (length === 127) (length = Number(buffer.readBigUInt64BE(2))), (offset = 10);
      if (!masked) violation("tail " + tail, "a client frame is not masked");
      const mask = buffer.slice(offset, offset + 4);
      offset += masked ? 4 : 0;
      if (buffer.length < offset + length) return;
      const payload = Buffer.from(buffer.slice(offset, offset + length).map((byte, index) => (masked ? byte ^ mask[index % 4] : byte)));
      const opcode = buffer[0] & 0x0f;
      buffer = buffer.slice(offset + length);
      if (opcode === 0x8) return socket.end();
      if (opcode === 0x1 && !configured) {
        configured = true;
        const message = JSON.parse(payload.toString());
        if (typeof message.debug !== "boolean") violation("tail " + tail, "the first message does not set debug");
        for (const event of cloudflare.scripts.get(name)?.events ?? []) {
          const text = Buffer.from(JSON.stringify({ outcome: "ok", scriptName: name, eventTimestamp: event.at, event: { request: { url: "https://" + name + "." + cloudflare.subdomain + ".workers.dev/sessions/s1/turn", method: "POST" } }, logs: [{ message: [event.message], level: event.level, timestamp: event.at }], exceptions: [] }));
          const head = text.length < 126 ? Buffer.from([0x81, text.length]) : Buffer.from([0x81, 126, text.length >> 8, text.length & 0xff]);
          socket.write(Buffer.concat([head, text]));
        }
      }
    }
  });
  socket.on("error", () => {});
});

server.listen(0, "127.0.0.1", () => {
  writeFileSync(args["port-file"], String(server.address().port));
});

process.on("SIGTERM", () => {
  if (args.report) writeFileSync(args.report, JSON.stringify({ violations, exercised: [...exercised].sort() }));
  process.exit(0);
});
