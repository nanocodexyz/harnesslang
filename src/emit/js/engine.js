class Engine {
  constructor(setup, handler, mode, turn, state, log) {
    assert(log.handler === handler.name, "a turn runs the handler it was admitted for");
    this.target = setup.target;
    this.mutation = setup.mutation;
    this.handler = handler;
    this.mode = mode;
    this.turn = turn;
    this.state = state;
    this.sites = new Map(log.sites);
    this.seq = 0;
    this.attempts = log.attempts;
    this.buffer = [];
    this.acknowledged = false;
  }

  append(record) {
    if (this.mode.live) {
      this.buffer.push(record);
      this.mode.world.append(record);
    }
  }

  nextAttempt() {
    this.attempts += 1;
    return this.attempts;
  }

  async run(args) {
    assert(args.length === this.handler.params.length, "a turn has one argument per parameter");
    let value;
    let trap = null;
    try {
      value = await this.handler.run(this, this.state, args);
    } catch (error) {
      if (!(error instanceof Trap)) throw error;
      trap = error.message;
    }
    let finish;
    if (trap === null) {
      assert(value !== undefined, "the checker proves every handler returns");
      finish = await this.finish("completed", toJson(this.handler.returns, value));
    } else {
      finish = await this.finish("failed", object([["trap", trap]]));
    }
    return { finish, state: this.state };
  }

  async finish(outcome, result) {
    const finish = { turn: this.turn, outcome, result };
    this.append({ kind: "status", turn: this.turn, last: this.seq, outcome, result });
    this.append({ kind: "checkpoint", revision: this.turn, state: PROGRAM.name, payload: encodeJson(stateJson(this.state)) });
    await this.flush();
    assert(this.buffer.length === 0, "a turn's status is durable before it is acknowledged");
    await this.hook(POINT_BEFORE_ACKNOWLEDGE);
    await this.acknowledge(finish);
    return finish;
  }

  async acknowledge(finish) {
    if (this.acknowledged) return;
    this.acknowledged = true;
    if (this.mode.live) await this.mode.world.acknowledge(finish);
  }

  async hook(point) {
    if (this.mode.live) await this.mode.world.hook(point);
  }

  async flush() {
    if (!this.mode.live || this.buffer.length === 0) return;
    const world = this.mode.world;
    await world.hook(POINT_BEFORE_FLUSH);
    await world.flush(this.mode.epoch, this.buffer.slice());
    this.buffer = [];
    await world.hook(POINT_AFTER_FLUSH);
  }

  push(field, value) {
    const definition = PROGRAM.state[field];
    const items = this.state[field];
    assert(Array.isArray(items), "a list field holds a list");
    if (BigInt(items.length) >= definition.ty.capacity) {
      throw new Trap(`bound(${definition.name})`);
    }
    this.state[field] = [...items, value];
  }

  resolve(site, args) {
    switch (site.kind) {
      case "tool": {
        const tool = PROGRAM.tools[site.tool];
        const input = object(tool.params.map((param, index) => [param.name, toJson(param.ty, args[index])]));
        return { tool: tool.name, effect: tool.effect, input };
      }
      case "dispatch": {
        const fields = args[0].f;
        const tool = fields[CALL_TOOL];
        return { tool, effect: dispatchClass(tool), input: fields[CALL_INPUT] };
      }
      case "model":
        assert(PROGRAM.model !== null, "the checker admits model calls only with a model");
        return { tool: PROGRAM.model.name, effect: "idempotent", input: null };
      default:
        return { tool: site.kind, effect: "none", input: null };
    }
  }

  async effect(siteId, args) {
    const site = this.handler.sites[siteId];
    this.seq += 1;
    const seq = this.seq;
    const call = this.resolve(site, args);
    if (site.class !== "dispatched") {
      assert(site.class === call.effect, "a fixed site runs with its declared class");
    }
    const logged = this.sites.get(seq);
    if (logged !== undefined) return this.recorded(siteId, seq, call, args, logged);
    this.requireLive(seq);
    if (site.kind !== "model") {
      this.append({ kind: "call", turn: this.turn, seq, tool: call.tool, effect: call.effect, input: call.input });
      this.sites.set(seq, { kind: "open", call });
    }
    return this.execute(siteId, seq, call, args);
  }

  async recorded(siteId, seq, call, args, logged) {
    const site = this.handler.sites[siteId];
    switch (logged.kind) {
      case "done":
        this.same(seq, logged.call, call);
        return this.decode(site, logged.output);
      case "reply":
        if (site.kind !== "model") throw this.diverged(seq, "a model reply", call.tool);
        return this.reply(logged.response);
      case "unknown":
        this.same(seq, logged.call, call);
        if (site.result.maybe === undefined) return this.reopen(siteId, seq, call, args);
        return variant(VARIANT_UNKNOWN, []);
      case "open":
        this.same(seq, logged.call, call);
        return this.reopen(siteId, seq, call, args);
      default:
        throw new Panic(`unknown site log ${logged.kind}`);
    }
  }

  requireLive(seq) {
    if (!this.mode.live) {
      throw new Corrupt(`replay of turn ${this.turn} needs seq ${seq}, which the journal does not record`);
    }
  }

  async reopen(siteId, seq, call, args) {
    this.requireLive(seq);
    if (mayBeUnknown(call.effect) && this.target.recovery === "in_step") {
      const attempt = await this.mode.world.attempt(this.turn, seq);
      if (attempt > 1 && this.mutation !== "vercel_retry_redispatches") return this.unknown(seq, call);
      return this.dispatch(siteId, seq, call, args, true);
    }
    assert(!mayBeUnknown(call.effect), "recovery turns every open external call unknown before the turn runs again");
    return this.execute(siteId, seq, call, args);
  }

  async unknown(seq, call) {
    const attempt = this.nextAttempt();
    this.append({ kind: "recovery", turn: this.turn, attempt, unknown: [seq] });
    this.sites.set(seq, { kind: "unknown", call });
    await this.flush();
    return variant(VARIANT_UNKNOWN, []);
  }

  async execute(siteId, seq, call, args) {
    const site = this.handler.sites[siteId];
    const gate = this.target.gateBeforeOutbound && outbound(site.kind);
    const required = flushes(site.durability, call.effect);
    if (mayBeUnknown(call.effect)) await this.mode.world.beforeExternal();
    if (required || gate) {
      await this.flush();
      assert(this.buffer.length === 0, "the call record is durable before its effect runs");
    }
    const inStep = this.target.recovery === "in_step" && mayBeUnknown(call.effect);
    if (inStep) {
      const attempt = await this.mode.world.attempt(this.turn, seq);
      assert(attempt === 1, "a fresh external step starts at its first attempt");
    }
    return this.dispatch(siteId, seq, call, args, inStep);
  }

  async parallel(siteId, items) {
    const site = this.handler.sites[siteId];
    assert(site.kind === "dispatch", "a parallel for dispatches");
    const values = new Array(items.length);
    const pending = [];
    for (let index = 0; index < items.length; index++) {
      this.seq += 1;
      const seq = this.seq;
      const call = this.resolve(site, [items[index]]);
      const logged = this.sites.get(seq);
      if (logged === undefined) {
        this.requireLive(seq);
        this.append({ kind: "call", turn: this.turn, seq, tool: call.tool, effect: call.effect, input: call.input });
        this.sites.set(seq, { kind: "open", call });
        pending.push({ index, seq, call, reopened: false });
        continue;
      }
      switch (logged.kind) {
        case "done":
          this.same(seq, logged.call, call);
          values[index] = this.decode(site, logged.output);
          break;
        case "reply":
          throw this.diverged(seq, "a model reply", call.tool);
        case "unknown":
          this.same(seq, logged.call, call);
          values[index] = variant(VARIANT_UNKNOWN, []);
          break;
        case "open": {
          this.same(seq, logged.call, call);
          this.requireLive(seq);
          const recovered = !mayBeUnknown(call.effect) || this.target.recovery === "in_step";
          const mutated = this.mutation === "recovery_redispatches_external" || this.mutation === "recovery_reports_failed";
          assert(recovered || mutated, "recovery turns every open external call unknown before the turn runs again");
          pending.push({ index, seq, call, reopened: true });
          break;
        }
        default:
          throw new Panic(`unknown site log ${logged.kind}`);
      }
    }
    let start = 0;
    while (start < pending.length) {
      const alone = mayBeUnknown(pending[start].call.effect);
      let end = start + 1;
      while (!alone && end < pending.length && !mayBeUnknown(pending[end].call.effect)) end += 1;
      await this.batchGroup(site, pending.slice(start, end), values);
      start = end;
    }
    return values;
  }

  async batchGroup(site, group, values) {
    const external = mayBeUnknown(group[0].call.effect);
    assert(!external || group.length === 1, "an external call of a parallel for runs alone");
    const gate = this.target.gateBeforeOutbound;
    const inStep = external && this.target.recovery === "in_step";
    const late = external && !gate && this.mutation === "batch_flush_after_dispatch";
    if (external) await this.mode.world.beforeExternal();
    if ((external || gate) && !late) {
      await this.flush();
      assert(this.buffer.length === 0, "every call record of a parallel for is durable before an external call of it runs");
    }
    if (inStep) {
      const first = group[0];
      const attempt = await this.mode.world.attempt(this.turn, first.seq);
      if (first.reopened && attempt > 1 && this.mutation !== "vercel_retry_redispatches") {
        values[first.index] = await this.unknown(first.seq, first.call);
        return;
      }
      assert(first.reopened || attempt === 1, "a fresh external step starts at its first attempt");
    }
    const returns = site.result.maybe;
    assert(returns !== undefined, "a dispatch yields maybe");
    const requests = group.map((waiting) => ({ turn: this.turn, seq: waiting.seq, tool: waiting.call.tool, effect: waiting.call.effect, input: waiting.call.input, returns }));
    const completed = await this.mode.world.tools(requests, CONCURRENT_REQUESTS_MAX);
    assert(completed.length === group.length, "a batch returns one output per request");
    const ordered = this.mutation === "results_in_completion_order" ? completed : [...completed].sort((left, right) => left[0] - right[0]);
    for (const [position, raw] of ordered) {
      const waiting = group[position];
      const output = checkedOutput(waiting.call.tool, returns, raw);
      this.append({ kind: "result", turn: this.turn, seq: waiting.seq, output });
      this.sites.set(waiting.seq, { kind: "done", call: waiting.call, output });
      values[waiting.index] = this.decode(site, output);
    }
    if (late || inStep) await this.flush();
  }

  async dispatch(siteId, seq, call, args, step) {
    const site = this.handler.sites[siteId];
    const world = this.mode.world;
    await this.hook(runPoint("before_run", site.kind, call.effect));
    if (site.kind === "model") {
      const callsMax = PROGRAM.model === null ? 0n : PROGRAM.model.calls;
      const response = await world.model({ turn: this.turn, seq, callsMax, context: args[0] });
      await this.hook(runPoint("after_run", site.kind, call.effect));
      this.append({ kind: "model", turn: this.turn, seq, response });
      this.sites.set(seq, { kind: "reply", response });
      return this.reply(response);
    }
    let output;
    if (site.kind === "now") {
      output = { tag: "ok", value: await world.now() };
    } else if (site.kind === "random") {
      output = { tag: "ok", value: await world.random() };
    } else {
      const returns = site.result.known ?? site.result.maybe;
      assert(returns !== undefined, "a tool site yields known or maybe");
      output = checkedOutput(call.tool, returns, await world.tool({ turn: this.turn, seq, tool: call.tool, effect: call.effect, input: call.input, returns }));
    }
    await this.hook(runPoint("after_run", site.kind, call.effect));
    this.append({ kind: "result", turn: this.turn, seq, output });
    this.sites.set(seq, { kind: "done", call, output });
    if (step) await this.flush();
    return this.decode(site, output);
  }

  reply(response) {
    try {
      return fromJson({ sum: SUM_REPLY }, response);
    } catch (error) {
      if (!(error instanceof DecodeError)) throw error;
      throw new Trap(`model reply: ${error.message}`);
    }
  }

  decode(site, output) {
    const corrupt = (message) => new Corrupt(`turn ${this.turn} result: ${message}`);
    if (site.result === "u64" && output.tag === "ok") {
      if (typeof output.value !== "bigint") throw corrupt(`${encodeJson(output.value)} is not u64`);
      return output.value;
    }
    const inner = site.result.known ?? site.result.maybe;
    if (inner !== undefined && output.tag === "ok") {
      try {
        return variant(VARIANT_OK, [fromJson(inner, output.value)]);
      } catch (error) {
        if (!(error instanceof DecodeError)) throw error;
        throw corrupt(error.message);
      }
    }
    if (inner !== undefined && output.tag === "failed") return variant(VARIANT_FAILED, [output.error]);
    throw corrupt(`${encodeJson(outputJson(output))} does not fit ${typeName(site.result)}`);
  }

  same(seq, recorded, call) {
    if (recorded.tool !== call.tool) throw this.diverged(seq, recorded.tool, call.tool);
    if (recorded.effect !== call.effect || !jsonEqual(recorded.input, call.input)) {
      throw new Corrupt(`replay of turn ${this.turn} diverged at seq ${seq}: tool '${call.tool}' was recorded with another class or input`);
    }
  }

  diverged(seq, recorded, running) {
    return new Corrupt(`replay of turn ${this.turn} diverged at seq ${seq}: the journal records ${recorded}, the program runs '${running}' (${callId(this.turn, seq)})`);
  }
}

function decodeOrCorrupt(decode) {
  try {
    return decode();
  } catch (error) {
    if (error instanceof DecodeError) throw new Corrupt(error.message);
    throw error;
  }
}

async function replayTurn(setup, turn, log, state) {
  const handler = handlerNamed(log.handler);
  if (handler === null) throw new Corrupt(`turn ${turn} ran handler '${log.handler}', which the program lacks`);
  const args = decodeOrCorrupt(() => argumentsFromJson(handler, log.arguments));
  if (log.finished === null) throw new Corrupt(`turn ${turn} is replayed before it finished`);
  const replaySetup = { target: setup.target, mutation: "none" };
  const ran = await new Engine(replaySetup, handler, { live: false }, turn, state, log).run(args);
  if (ran.finish.outcome !== log.finished.outcome || !jsonEqual(ran.finish.result, log.finished.result)) {
    throw new Corrupt(`replay of turn ${turn} finished ${encodeJson(ran.finish.result)} where the journal says ${encodeJson(log.finished.result)}`);
  }
  return ran.state;
}

async function stateBefore(setup, session, turn) {
  let start = 1;
  let state = initialState();
  let revision = null;
  for (const candidate of session.checkpoints.keys()) {
    if (candidate < turn && (revision === null || candidate > revision)) revision = candidate;
  }
  if (revision !== null) {
    const payload = session.checkpoints.get(revision);
    state = decodeOrCorrupt(() => stateFromJson(decodeJson(payload)));
    start = revision + 1;
  }
  for (let earlier = start; earlier < turn; earlier++) {
    const log = session.turns.get(earlier);
    if (log === undefined) throw new Corrupt(`turn ${earlier} is missing before turn ${turn}`);
    state = await replayTurn(setup, earlier, log, state);
  }
  return state;
}

function recoverAtLoad(log) {
  const unknown = [];
  for (const [seq, site] of log.sites) {
    if (site.kind === "open" && mayBeUnknown(site.call.effect)) unknown.push(seq);
  }
  for (const seq of unknown) {
    log.sites.set(seq, { kind: "unknown", call: log.sites.get(seq).call });
  }
  return unknown;
}

function cloneLog(log) {
  return { ...log, sites: new Map(log.sites) };
}

async function runTurn(setup, world, epoch, records, request) {
  const session = fold(records);
  const handler = handlerNamed(request.handler);
  assert(handler !== null, "a turn request names a handler of the program");
  const args = argumentsJson(handler, request.arguments);
  const existing = session.turns.get(request.turn);
  if (existing !== undefined && existing.finished !== null) {
    return { ran: "already_finished", finish: { turn: request.turn, outcome: existing.finished.outcome, result: existing.finished.result } };
  }
  const state = await stateBefore(setup, session, request.turn);
  const pending = [];
  let unknown = [];
  let log;
  if (existing !== undefined) {
    if (existing.handler !== request.handler || !jsonEqual(existing.arguments, args)) {
      throw new Corrupt(`turn ${request.turn} was admitted with other arguments`);
    }
    log = cloneLog(existing);
    if (setup.target.recovery === "at_load") unknown = recoverAtLoad(log);
  } else {
    const next = session.turns.size === 0 ? 1 : session.lastTurn + 1;
    if (request.turn !== next) throw new Corrupt(`turn ${request.turn} is requested when turn ${next} is next`);
    pending.push({ kind: "admitted", turn: request.turn, handler: request.handler, arguments: args });
    log = newTurnLog(request.handler, null);
  }
  const engine = new Engine(setup, handler, { live: true, world, epoch }, request.turn, state, log);
  for (const record of pending) engine.append(record);
  if (unknown.length > 0) {
    engine.append({ kind: "recovery", turn: request.turn, attempt: engine.nextAttempt(), unknown });
  }
  const ran = await engine.run(request.arguments);
  return { ran: "finished", finish: ran.finish };
}

function turnRequestFromJson(json) {
  if (!isObject(json)) throw new DecodeError("a turn request is an object");
  const turn = smallNumber(json.turn, "a turn request's turn");
  if (typeof json.handler !== "string") throw new DecodeError("a turn request names its handler");
  const handler = handlerNamed(json.handler);
  if (handler === null) throw new DecodeError(`no handler '${json.handler}'`);
  return { turn, handler: json.handler, arguments: argumentsFromJson(handler, json.arguments) };
}

function finishJson(finish) {
  return object([["outcome", finish.outcome], ["result", finish.result], ["turn", BigInt(finish.turn)]]);
}

function finishFromJson(json) {
  return { turn: smallNumber(json.turn, "a finish's turn"), outcome: json.outcome, result: json.result };
}

function toolRequestJson(request) {
  return object([
    ["effect", request.effect],
    ["input", request.input],
    ["returns", typeName(request.returns)],
    ["seq", BigInt(request.seq)],
    ["tool", request.tool],
    ["turn", BigInt(request.turn)],
  ]);
}

function modelRequestJson(request) {
  return object([
    ["calls_max", request.callsMax],
    ["context", toJson(CONTEXT_TYPE, request.context)],
    ["seq", BigInt(request.seq)],
    ["turn", BigInt(request.turn)],
  ]);
}

class HostError extends Error {}

function defaultHost(config) {
  const post = async (url, json) => {
    const response = await config.fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: encodeJson(json) });
    const body = await response.text();
    if (!response.ok) throw new HostError(`${url} answered ${response.status}: ${body}`);
    return decodeJson(body);
  };
  return {
    async tool(request) {
      return outputFrom(await post(config.toolsUrl, toolRequestJson(request)));
    },
    async model(request) {
      return post(config.modelUrl, modelRequestJson(request));
    },
    now() {
      return BigInt(config.now());
    },
    random() {
      const value = config.random();
      assert(typeof value === "bigint" && value >= 0n && value <= U64_MAX, "random yields a u64");
      return value;
    },
  };
}

function platformRandom() {
  return crypto.getRandomValues(new BigUint64Array(1))[0];
}

const runtime = {
  Panic, Trap, Corrupt, DecodeError, HostError,
  encodeJson, decodeJson, crc32, encodeLine, decodeLine, encodeLines, decodeLines,
  recordJson, recordFrom, fold, toJson, fromJson, finishJson, defaultHost,
};
