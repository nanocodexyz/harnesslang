import { DurableObject, RpcTarget, WorkerEntrypoint } from "cloudflare:workers";

const CELL_COMPATIBILITY_DATE = "2026-11-01";
const SCHEMA = "CREATE TABLE IF NOT EXISTS cell_journal (seq INTEGER PRIMARY KEY, entry TEXT NOT NULL)";
const INSERT = "INSERT INTO cell_journal (seq, entry) VALUES (?, ?)";
const OUTBOUND_INHERIT = "inherit";
const OBJECT_NAME = "cell-facts";

function cellModule(code) {
  return [
    'import { WorkerEntrypoint } from "cloudflare:workers";',
    "export default class extends WorkerEntrypoint {",
    "  async run() {",
    "    const tools = this.env.TOOLS ?? this.env.CELL.get(this.env.CELL.idFromName(this.env.OBJECT));",
    "    const host = this.env.HOST_URL;",
    "    try {",
    `      return { ok: await (async () => { ${code} })() };`,
    "    } catch (error) {",
    "      return { error: String(error?.message ?? error) };",
    "    }",
    "  }",
    "}",
  ].join("\n");
}

class LocalTools extends RpcTarget {
  constructor(object) {
    super();
    this.object = object;
  }

  count() {
    return this.object.count();
  }
}

export class CellTools extends WorkerEntrypoint {
  object() {
    return this.env.CELL.get(this.env.CELL.idFromName(this.ctx.props.object));
  }

  charge(input) {
    return this.object().charge(input);
  }

  count() {
    return this.object().count();
  }
}

export class CellObject extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(SCHEMA);
    this.seq = ctx.storage.sql.exec("SELECT count(*) AS n FROM cell_journal").one().n;
    this.counted = 0;
  }

  journal(entry) {
    this.seq += 1;
    this.ctx.storage.sql.exec(INSERT, this.seq, JSON.stringify(entry));
    return this.seq;
  }

  async charge(input) {
    const seq = this.journal({ call: "charge", input });
    const response = await fetch(`${this.env.HOST_URL}/charge`, { method: "POST", body: JSON.stringify({ seq, input }) });
    const output = await response.text();
    this.journal({ result: seq, output });
    return output;
  }

  count() {
    this.counted += 1;
    return this.counted;
  }

  cellEnv(via) {
    const base = { HOST_URL: this.env.HOST_URL, OBJECT: OBJECT_NAME };
    if (via === "namespace") return { ...base, CELL: this.env.CELL };
    if (via === "target") return { ...base, TOOLS: new LocalTools(this) };
    if (via === "stub") return { ...base, TOOLS: this.env.CELL.get(this.ctx.id) };
    return { ...base, TOOLS: this.ctx.exports.CellTools({ props: { object: OBJECT_NAME } }) };
  }

  async fetch(request) {
    const url = new URL(request.url);
    const code = await request.text();
    const outbound = url.searchParams.get("outbound");
    if (url.searchParams.get("pending") === "1") this.journal({ pending: true });
    const worker = this.env.LOADER.get(`cell-${url.searchParams.get("id")}`, () => {
      const source = {
        compatibilityDate: CELL_COMPATIBILITY_DATE,
        mainModule: "cell.js",
        modules: { "cell.js": cellModule(code) },
        env: this.cellEnv(url.searchParams.get("via")),
      };
      if (outbound !== OUTBOUND_INHERIT) source.globalOutbound = null;
      return source;
    });
    const started = this.counted;
    const result = await worker.getEntrypoint().run();
    return Response.json({ ...result, rpc_calls: this.counted - started, isolate_global: globalThis.cellMarker ?? null });
  }
}

export default {
  fetch(request, env) {
    const id = env.CELL.idFromName(OBJECT_NAME);
    return env.CELL.get(id).fetch(request);
  },
};
