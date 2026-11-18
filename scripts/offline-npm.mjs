#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, mkdirSync, copyFileSync, existsSync } from "node:fs";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const oracle = join(root, "oracle/cloudflare");
const vendor = join(root, "vendor/npm");
const prefix = resolve(process.argv[2] ?? join(root, "output/cloudflare-npm"));
const cache = resolve(process.argv[3] ?? join(root, "output/npm-cache"));
const lock = JSON.parse(readFileSync(join(oracle, "package-lock.json")));
const manifest = JSON.parse(readFileSync(join(vendor, "manifest.json")));
const packages = Object.entries(lock.packages).filter(([name]) => name !== "");
if (packages.length !== manifest.length) throw new Error("npm vendor manifest count differs from the lock");
for (const [name, entry] of packages) {
  const found = manifest.filter((row) => row.package === name.slice("node_modules/".length));
  if (found.length !== 1) throw new Error(`npm vendor manifest must name ${name} once`);
  const row = found[0];
  if (row.file !== entry.resolved.split("/").at(-1) || row.url !== entry.resolved || row.integrity !== entry.integrity || row.license !== entry.license) {
    throw new Error(`npm vendor manifest differs from lock: ${name}`);
  }
  const bytes = readFileSync(join(vendor, row.file));
  if (bytes.length !== row.bytes || createHash("sha256").update(bytes).digest("hex") !== row.sha256) throw new Error(`npm archive SHA256 mismatch: ${row.file}`);
  const [algorithm, expected] = entry.integrity.split("-");
  if (createHash(algorithm).update(bytes).digest("base64") !== expected) throw new Error(`npm archive integrity mismatch: ${row.file}`);
}
mkdirSync(prefix, { recursive: true });
mkdirSync(cache, { recursive: true });
function npm(args) {
  const run = spawnSync("npm", [...args, "--offline", "--ignore-scripts", "--no-audit", "--no-fund", "--cache", cache], {
    cwd: prefix, stdio: "inherit", timeout: 300000,
    env: { ...process.env, npm_config_registry: "http://127.0.0.1:9", npm_config_update_notifier: "false" },
  });
  if (run.error) throw run.error;
  if (run.status !== 0) throw new Error(`offline npm ${args[0]} exited ${run.status}`);
}
npm(["cache", "add", ...manifest.map((row) => join(vendor, row.file))]);
for (const name of ["package.json", "package-lock.json"]) copyFileSync(join(oracle, name), join(prefix, name));
npm(["ci", "--include=optional"]);
const platform = `${process.platform}-${process.arch}`;
const names = { "linux-x64": "linux-64", "linux-arm64": "linux-arm64", "darwin-x64": "darwin-64", "darwin-arm64": "darwin-arm64", "win32-x64": "windows-64", "win32-arm64": "windows-64" };
if (!(platform in names)) throw new Error(`no pinned workerd package for ${platform}`);
const packageName = `@cloudflare/workerd-${names[platform]}`;
const binary = join(prefix, "node_modules", packageName, "bin", process.platform === "win32" ? "workerd.exe" : "workerd");
if (!existsSync(binary)) throw new Error(`offline npm did not install ${packageName}`);
for (const name of ["workerd", packageName]) {
  const installed = JSON.parse(readFileSync(join(prefix, "node_modules", name, "package.json")));
  if (installed.version !== lock.packages["node_modules/workerd"].version) throw new Error(`installed version differs: ${name}`);
}
const pinned = /^1\.(\d{4})(\d{2})(\d{2})\.\d+$/.exec(lock.packages["node_modules/workerd"].version);
if (!pinned) throw new Error("workerd lock version has an unexpected format");
const expectedVersion = `workerd ${pinned[1]}-${pinned[2]}-${pinned[3]}`;
const version = spawnSync(binary, ["--version"], { encoding: "utf8", timeout: 30000 });
if (version.error) throw version.error;
if (version.status !== 0 || version.stdout.trim() !== expectedVersion) throw new Error(`workerd version mismatch: ${version.stdout} ${version.stderr}`);
console.log(`offline npm: verified ${manifest.length} pinned archives; ${version.stdout.trim()}`);
