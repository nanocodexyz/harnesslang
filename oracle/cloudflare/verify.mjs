import { Failure } from "./platform.mjs";

const BUILTIN_TOOLS = ["now", "random", "deny", "suspend"];
const INPUT_REFUSED_PREFIX = "input of ";

function callKey(turn, seq) {
  return `${turn}/${seq}`;
}

function decodeRows(rt, rows) {
  return rows.map((row, index) => {
    if (row.position !== index + 1) throw new Failure("seq_unique", `journal row ${index + 1} has position ${row.position}`);
    try {
      return rt.decodeLine(row.record, false);
    } catch (error) {
      throw new Failure("liveness", `journal row ${row.position} does not decode: ${error.message}`);
    }
  });
}

function foldOrFail(rt, records) {
  try {
    return rt.fold(records);
  } catch (error) {
    throw new Failure("liveness", `the journal does not fold: ${error.message}`);
  }
}

function journalFacts(records) {
  const facts = { calls: new Map(), results: new Map(), recovered: new Map(), statuses: new Map(), seqs: new Map(), rowsPerTurn: new Map(), shapes: new Map(), recoveries: 0 };
  for (const record of records) {
    const turn = record.kind === "checkpoint" ? record.revision : record.turn;
    if (record.kind === "recovery") facts.recoveries += 1;
    else facts.rowsPerTurn.set(turn, (facts.rowsPerTurn.get(turn) ?? 0) + 1);
    const shape = facts.shapes.get(turn) ?? { models: 0, calls: 0 };
    if (record.kind === "model") shape.models += 1;
    if (record.kind === "call") shape.calls += 1;
    facts.shapes.set(turn, shape);
    if (record.kind === "call" || record.kind === "model") {
      const seqs = facts.seqs.get(record.turn) ?? [];
      if (seqs.length > 0 && record.seq <= seqs[seqs.length - 1]) {
        throw new Failure("seq_unique", `turn ${record.turn} records seq ${record.seq} after seq ${seqs[seqs.length - 1]}`);
      }
      seqs.push(record.seq);
      facts.seqs.set(record.turn, seqs);
    }
    if (record.kind === "call") facts.calls.set(callKey(record.turn, record.seq), record);
    if (record.kind === "result") facts.results.set(callKey(record.turn, record.seq), record.output);
    if (record.kind === "recovery") {
      for (const seq of record.unknown) facts.recovered.set(callKey(record.turn, seq), record.attempt);
    }
    if (record.kind === "status") facts.statuses.set(record.turn, [...(facts.statuses.get(record.turn) ?? []), record]);
  }
  return facts;
}

function checkCalls(facts, ledger) {
  const counts = { external: 0, unknown: 0, other: 0 };
  for (const [id, call] of facts.calls) {
    const done = facts.results.has(id);
    const recovered = facts.recovered.has(id);
    if (call.effect === "external") {
      counts.external += 1;
      if (!done && !recovered) throw new Failure("unknown_reported", `external call ${id} has no result and no recovery reports it unknown`);
      if (done && recovered) throw new Failure("unknown_reported", `external call ${id} has a result and was also reported unknown`);
      if (recovered) counts.unknown += 1;
      continue;
    }
    counts.other += 1;
    if (!done) throw new Failure("idempotent_known", `${call.effect} call ${id} (${call.tool}) has no result`);
    if (recovered) throw new Failure("unknown_reported", `${call.effect} call ${id} was reported unknown`);
    const output = facts.results.get(id);
    const refused = output.tag === "failed" && output.error.startsWith(INPUT_REFUSED_PREFIX + call.tool + ": ");
    if (!BUILTIN_TOOLS.includes(call.tool) && !refused && !(ledger.get(id) ?? []).some((dispatch) => dispatch.tool === call.tool)) {
      throw new Failure("idempotent_known", `${call.effect} call ${id} (${call.tool}) has a result no dispatch produced`);
    }
  }
  for (const id of facts.recovered.keys()) {
    if (!facts.calls.has(id)) throw new Failure("unknown_reported", `a recovery reports ${id}, which has no call row`);
  }
  for (const [id, dispatches] of ledger) {
    const external = dispatches.filter((dispatch) => dispatch.effect === "external");
    if (external.length > 1) throw new Failure("external_once", `external call ${id} was dispatched ${external.length} times`);
    if (external.length === 1 && facts.calls.get(id)?.tool !== external[0].tool) {
      throw new Failure("external_once", `external call ${id} (${external[0].tool}) ran without a durable call row for it`);
    }
  }
  return counts;
}

function checkFinishes(rt, facts, acks, turns) {
  for (let turn = 1; turn <= turns; turn++) {
    const statuses = facts.statuses.get(turn) ?? [];
    if (statuses.length !== 1) throw new Failure("finish_once", `turn ${turn} has ${statuses.length} status rows`);
  }
  for (const ack of acks) {
    const status = (facts.statuses.get(ack.turn) ?? [])[0];
    if (status === undefined || status.outcome !== ack.outcome || rt.encodeJson(status.result) !== rt.encodeJson(ack.result)) {
      throw new Failure("finish_once", `turn ${ack.turn} was acknowledged as ${ack.outcome} ${rt.encodeJson(ack.result)}, which its status row does not say`);
    }
  }
}

function verifyJournal(rt, rows, ledger, acks, turns) {
  const records = decodeRows(rt, rows);
  const session = foldOrFail(rt, records);
  for (let turn = 1; turn <= turns; turn++) {
    const log = session.turns.get(turn);
    if (log === undefined || log.finished === null) throw new Failure("liveness", `turn ${turn} is not finished in the object's SQL`);
  }
  const facts = journalFacts(records);
  const counts = checkCalls(facts, ledger);
  checkFinishes(rt, facts, acks, turns);
  return { rows: rows.length, recoveries: facts.recoveries, rowsPerTurn: facts.rowsPerTurn, shapes: facts.shapes, ...counts };
}

function statusOf(rt, rows, turn) {
  let found = null;
  for (const row of rows) {
    const record = rt.decodeLine(row.record, false);
    if (record.kind === "status" && record.turn === turn) found = record;
  }
  return found;
}

export { callKey, statusOf, verifyJournal };
