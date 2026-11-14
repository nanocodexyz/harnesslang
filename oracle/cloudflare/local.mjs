import { existsSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..");
const NPM_PREFIX = join(ROOT, "output", "cloudflare-npm");
const WORKERD = join(NPM_PREFIX, "node_modules", ".bin", "workerd");
const METADATA_FILE = "metadata.sqlite";
const SQLITE_BUSY_WAIT_MS = 5000;
const NO_SUCH_TABLE = "no such table";

function objectFile(folder) {
  if (!existsSync(folder)) return null;
  const files = readdirSync(folder).filter((name) => name.endsWith(".sqlite") && name !== METADATA_FILE);
  if (files.length > 1) throw new Error(`oracle assertion failed: one object per storage folder, found ${files.length} in ${folder}`);
  return files.length === 0 ? null : join(folder, files[0]);
}

function readRows(file, sql) {
  const database = new DatabaseSync(file, { readOnly: true, timeout: SQLITE_BUSY_WAIT_MS });
  try {
    return database.prepare(sql).all().map((row) => ({ ...row }));
  } catch (error) {
    if (String(error.message).includes(NO_SUCH_TABLE)) return [];
    throw error;
  } finally {
    database.close();
  }
}

export { NPM_PREFIX, WORKERD, objectFile, readRows };
