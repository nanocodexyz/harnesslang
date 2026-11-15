#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";

const MUTATIONS = {
  dispatch_before_insert: [['const late = this.object.mutation === "cloudflare_fetch_before_insert" &&', "const late ="]],
  no_alarm: [['if (this.object.mutation !== "cloudflare_no_alarm") this.object.arm();', ""]],
  recovery_redispatches: [
    ['if (site.kind === "open" && mayBeUnknown(site.call.effect)) unknown.push(seq);', ""],
    ['assert(!mayBeUnknown(call.effect), "recovery turns every open external call unknown before the turn runs again");', ""],
  ],
  recovery_unreported: [["  if (unknown.length > 0) {", "  if (false) {"]],
  unserialized: [["await this.serialized(() => this.drive(turn))", "await this.drive(turn)"]],
  stale_alarm_flag: [["      this.armed = false;\n      await Promise.allSettled(", "      await Promise.allSettled("]],
  no_sync: [["    await this.storage.sync();\n", ""]],
  vercel_step_continues: [["    throw new StepEnd(linesOf(text));", "    linesOf(text);"]],
  vercel_ack_unhooked: [["      if (live) await platform.hook(POINT_BEFORE_ACKNOWLEDGE);", ""]],
  vercel_resumed_dropped: [["      await platform.resumed(suspensionFrom(answered.suspension), resumeFrom(answered.resume), true);", ""]],
};

const [name, from, to] = process.argv.slice(2);
if (MUTATIONS[name] === undefined || from === undefined || to === undefined) {
  console.error(`usage: node mutate.mjs ${Object.keys(MUTATIONS).join("|")} IN.mjs OUT.mjs`);
  process.exit(2);
}
let source = readFileSync(from, "utf8");
for (const [pattern, replacement] of MUTATIONS[name]) {
  const found = source.split(pattern).length - 1;
  if (found !== 1) {
    console.error(`mutation ${name}: expected the generated code to hold ${JSON.stringify(pattern)} once, found ${found}`);
    process.exit(1);
  }
  source = source.replace(pattern, () => replacement);
}
writeFileSync(to, source);
