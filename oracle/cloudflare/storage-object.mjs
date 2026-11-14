import { DurableObject } from "cloudflare:workers";

const SCHEMA = "CREATE TABLE IF NOT EXISTS facts (id INTEGER PRIMARY KEY, what TEXT NOT NULL)";
const INSERT = "INSERT INTO facts (what) VALUES (?)";
const SPIN_MARKER = "hl-storage spin ";
const ALARM_MARKER = "hl-storage alarm";
const OBJECT_NAME = "storage-facts";

export class FactObject extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(SCHEMA);
  }

  insert(what) {
    this.ctx.storage.sql.exec(INSERT, what);
  }

  spin(fact) {
    console.log(SPIN_MARKER + fact);
    for (;;) {}
  }

  async batch(url) {
    const rows = Number(url.searchParams.get("rows"));
    const bytes = Number(url.searchParams.get("bytes"));
    const trial = Number(url.searchParams.get("trial"));
    const value = "x".repeat(bytes);
    if (url.searchParams.get("unconfirmed") === "1") {
      for (let row = 0; row < rows; row++) this.ctx.storage.put(`k${trial * rows + row}`, value, { allowUnconfirmed: true });
    } else {
      for (let row = 0; row < rows; row++) this.insert(value);
    }
    const answer = await fetch(`${this.env.HOST_URL}/outbound`, { method: "POST", body: String(rows) });
    return new Response(await answer.text());
  }

  async fetch(request) {
    const url = new URL(request.url);
    const fact = url.searchParams.get("fact");
    if (fact === "two_inserts_then_spin") {
      this.insert("first");
      this.insert("second");
      return this.spin(fact);
    }
    if (fact === "insert_microtask_spin") {
      this.insert("first");
      await Promise.resolve();
      return this.spin(fact);
    }
    if (fact === "insert_sync_spin") {
      this.insert("first");
      await this.ctx.storage.sync();
      return this.spin(fact);
    }
    if (fact === "batch_then_fetch") return this.batch(url);
    if (fact === "arm") {
      this.insert("armed");
      await this.ctx.storage.setAlarm(Date.now() + Number(url.searchParams.get("ms")));
      return new Response("armed");
    }
    if (fact === "slow") {
      this.insert("slow started");
      const answer = await fetch(`${this.env.HOST_URL}/slow`, { method: "POST" });
      this.insert(`slow finished ${await answer.text()}`);
      return new Response("slow");
    }
    if (fact === "quick") {
      this.insert("quick");
      return new Response("quick");
    }
    return new Response(`unknown fact ${fact}`, { status: 400 });
  }

  async alarm() {
    this.insert("alarm");
    console.log(ALARM_MARKER);
  }
}

export default {
  fetch(request, env) {
    return env.FACTS.get(env.FACTS.idFromName(OBJECT_NAME)).fetch(request);
  },
};
