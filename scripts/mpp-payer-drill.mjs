import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";

const seeds = (process.env.MPP_PAYER_SEEDS ?? "1..8").split("..").map(Number);
const resources = Number(process.env.MPP_PAYER_RESOURCES ?? "6");
const target = "agent/target/release";
const out = "output/mpp-payer-drill";
const program = readFileSync("examples/mpp_agent.hl", "utf8");
const constant = (name) => BigInt(program.match(new RegExp("^const " + name + ": u64 = (\\d+)$", "m"))[1]);
const spendCap = constant("spend_cap_units");
const chargeMax = constant("charge_max_units");
const paymentsMax = Number(constant("payments_per_call_max"));
const wallet = program.match(/^wallet (\w+) /m)[1];
const prefix = "payment required: ";
const portWaitMs = 10000;
const portPollMs = 50;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function port(file) {
  for (let waited = 0; waited < portWaitMs; waited += portPollMs) {
    if (existsSync(file)) return readFileSync(file, "utf8").trim();
    await sleep(portPollMs);
  }
  throw new Error("no port in " + file);
}

async function post(base, path, body) {
  const response = await fetch(base + path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  if (!response.ok) throw new Error(path + " answered " + response.status + ": " + (await response.text()));
  return response.json();
}

function request(tool, input, turn, seq, effect, returns) {
  return { effect, input, nested: null, returns, seq, tool, turn, job: false };
}

async function seedRun(seed) {
  const run = out + "/" + seed;
  mkdirSync(run, { recursive: true });
  const server = spawn(target + "/mpp-server", ["serve", run + "/server.port", run + "/server.jsonl", String(seed), "none"], { stdio: "ignore" });
  const payer = spawn(target + "/mpp-payer", [run + "/payer.port", run + "/state", wallet], { stdio: "ignore", env: { ...process.env, HL_AGENT_WALLET_BALANCE: "6000" } });
  try {
    const base = "http://127.0.0.1:" + (await port(run + "/payer.port"));
    const site = "http://127.0.0.1:" + (await port(run + "/server.port"));
    let seq = 0;
    let spent = 0n;
    let recovered = 0;
    for (let index = 1; index <= resources; index += 1) {
      const url = site + "/r" + index;
      seq += 1;
      let output = await post(base, "/tool", request("fetch", { payment: "", url }, 1, seq, "idempotent", "text"));
      for (let round = 0; round < paymentsMax && typeof output.failed === "string" && output.failed.startsWith(prefix); round += 1) {
        const room = spendCap - spent < chargeMax ? spendCap - spent : chargeMax;
        seq += 1;
        const paidSeq = seq;
        const paid = await post(base, "/tool", request(wallet, { max: Number(room), required: output.failed }, 1, paidSeq, "external", "u64"));
        if (paid.ok === undefined) break;
        if (BigInt(paid.ok) > room) throw new Error("the payer paid " + paid.ok + " over the room " + room);
        spent += BigInt(paid.ok);
        const again = await post(base, "/recover", request(wallet, {}, 1, paidSeq, "external", "u64"));
        if (again.reply?.ok !== paid.ok) throw new Error("recovering " + paidSeq + " answered " + JSON.stringify(again) + " where the payer paid " + paid.ok);
        recovered += 1;
        seq += 1;
        output = await post(base, "/tool", request("fetch", { payment: output.failed, url }, 1, seq, "idempotent", "text"));
      }
    }
    const lost = await post(base, "/recover", request(wallet, {}, 1, seq + 1, "external", "u64"));
    if (lost !== "not_dispatched") throw new Error("an undispatched wallet call recovered as " + JSON.stringify(lost));
    const verdict = execFileSync(target + "/mpp-server", ["judge", run + "/server.jsonl", run + "/state/payments.jsonl", String(spendCap)], { encoding: "utf8" }).trim();
    return verdict + " recovered=" + recovered;
  } finally {
    server.kill();
    payer.kill();
  }
}

rmSync(out, { recursive: true, force: true });
let passed = 0;
let failed = 0;
for (let seed = seeds[0]; seed <= seeds[1]; seed += 1) {
  try {
    console.log("seed=" + seed + " " + (await seedRun(seed)));
    passed += 1;
  } catch (error) {
    console.log("FAIL seed=" + seed + ": " + (error.stdout?.trim() || error.message));
    failed += 1;
  }
}
console.log("RESULT seeds=" + seeds.join("..") + " passed=" + passed + " failed=" + failed);
process.exit(failed === 0 ? 0 : 1);
