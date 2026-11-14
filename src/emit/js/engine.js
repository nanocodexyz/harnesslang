const WAIT_TOOL = "wait";
const WAIT_MS = "ms";
const WAIT_UNTIL = "until";
const SUSPEND_TOOL = "suspend";
const SUSPEND_KEY = "key";

class Suspended {
  constructor(suspension) {
    this.suspension = suspension;
  }
}

function withoutUntil(call) {
  if (!isObject(call.input)) return call;
  return { ...call, input: object(Object.keys(call.input).filter((key) => key !== WAIT_UNTIL).map((key) => [key, call.input[key]])) };
}

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
    this.cells = new Map([...log.cells].map(([seq, cell]) => [seq, cellLogClone(cell)]));
    this.nested = new Map(log.nested);
    this.nextCell = 0;
    this.events = log.events.slice();
    this.taken = 0;
    this.admitted = new Map();
    this.listed = new Map();
    this.notices = new Map();
    this.discovered = new Map();
    this.blobs = new Blobs();
  }

  notice(listing) {
    return this.notices.get(listing) ?? "";
  }

  keepBlobs(field, count) {
    assert(PROGRAM.blobs !== undefined, "the checker keeps blobs only in a program with blobs");
    const items = this.state[field];
    assert(Array.isArray(items), "keep blobs targets a list field");
    this.state[field] = keptBlobs(items, PROGRAM.blobs.attached, count);
  }

  cancelled() {
    return this.events.some((event) => event.delivery.kind === "cancel" && event.at <= this.seq);
  }

  take() {
    const visible = this.events.filter((event) => event.delivery.kind === "steer" && event.at <= this.seq);
    assert(this.taken <= visible.length, "the steers a turn sees only grow");
    const fresh = visible.slice(this.taken).map((event) => event.delivery.message);
    this.taken = visible.length;
    return fresh;
  }

  async drain() {
    if (this.target.inbound !== "after_run" || !eventsTaken() || !this.mode.live) return;
    const world = this.mode.world;
    const arrived = await world.inbox(this.turn);
    if (arrived.length === 0) return;
    const position = this.seq;
    const answers = [];
    let recorded = false;
    for (const delivery of arrived) {
      const admitted = admitEvent(this.turn, this.events, position, delivery);
      if (admitted.record !== null) {
        this.append(admitted.record);
        recorded = true;
      }
      answers.push([delivery.id, admitted.answer]);
    }
    if (recorded) await this.flush();
    await world.answer(this.turn, position, answers);
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
      if (error instanceof Suspended) return { suspended: error.suspension, state: this.state };
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
    if (eventsTaken() && this.mode.live) await closeInbox(this.mode.world, this.turn, this.seq, this.events);
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

  keepAfter(field, count, first) {
    const items = this.state[field];
    assert(Array.isArray(items), "a list field holds a list");
    const definition = PROGRAM.state[field];
    const kept = count < BigInt(items.length) ? Number(count) : items.length;
    if (BigInt(kept) >= definition.ty.capacity) {
      throw new Trap(`bound(${definition.name})`);
    }
    this.state[field] = [first, ...items.slice(items.length - kept)];
    assert(this.state[field].length === kept + 1, "keep after leaves the first entry and at most the last count entries");
  }

  keep(field, count) {
    const items = this.state[field];
    assert(Array.isArray(items), "a list field holds a list");
    if (count >= BigInt(items.length)) {
      return;
    }
    this.state[field] = items.slice(items.length - Number(count));
    assert(BigInt(this.state[field].length) === count, "keep leaves exactly the last count entries");
  }

  resolve(site, args) {
    switch (site.kind) {
      case "tool": {
        const tool = PROGRAM.tools[site.tool];
        const input = object(tool.params.map((param, index) => [param.name, toJson(param.ty, args[index])]));
        return { tool: tool.name, effect: tool.effect, input };
      }
      case "listing":
        return { tool: PROGRAM.listings[site.tool].name, effect: "none", input: null };
      case "dispatch": {
        const fields = args[0].f;
        const tool = fields[CALL_TOOL];
        const raw = fields[CALL_INPUT];
        const typed = toolInput(tool, raw);
        return { tool, effect: dispatchClass(tool), input: typed.kind === "typed" ? typed.input : raw };
      }
      case "model":
        assert(PROGRAM.model !== null, "the checker admits model calls only with a model");
        return { tool: PROGRAM.model.name, effect: "idempotent", input: null };
      case "cell":
        return { tool: PROGRAM.codes[site.code].name, effect: "none", input: args[0] };
      case "wait":
        return { tool: WAIT_TOOL, effect: "none", input: object([[WAIT_MS, args[0]]]) };
      case "suspend":
        return { tool: SUSPEND_TOOL, effect: "none", input: object([[SUSPEND_KEY, args[0]]]) };
      default:
        return { tool: site.kind, effect: "none", input: null };
    }
  }

  async effect(siteId, args) {
    const site = this.handler.sites[siteId];
    if (site.kind === "wait" && args[0] > site.ms) throw new Trap(`bound(${site.cap})`);
    this.seq += 1;
    const seq = this.seq;
    const call = this.resolve(site, args);
    const code = site.kind === "cell" ? site.code : site.kind === "dispatch" ? codeNamed(call.tool) : null;
    if (code !== null) return this.cellSite(code, seq, { ...call, effect: "none" });
    if (site.kind === "suspend") return this.suspendSite(siteId, seq, call);
    if (site.kind === "wait") return this.waitSite(siteId, seq, call, args);
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
        return this.settle(site, call, logged.output);
      case "reply":
        if (site.kind !== "model") throw this.diverged(seq, "a model reply", call.tool);
        if (logged.mode !== site.mode) {
          throw this.diverged(seq, `a model reply in ${modeText(logged.mode)}`, `${call.tool} in ${modeText(site.mode)}`);
        }
        return this.reply(logged.response);
      case "unknown":
        this.same(seq, logged.call, call);
        if (!reportsUnknown(site.result)) return this.reopen(siteId, seq, call, args);
        return variant(VARIANT_UNKNOWN, []);
      case "open":
        this.same(seq, logged.call, call);
        return this.reopen(siteId, seq, call, args);
      default:
        throw new Panic(`unknown site log ${logged.kind}`);
    }
  }

  async suspendSite(siteId, seq, call) {
    const site = this.handler.sites[siteId];
    const logged = this.sites.get(seq);
    if (logged === undefined) {
      this.requireLive(seq);
      this.append({ kind: "call", turn: this.turn, seq, tool: call.tool, effect: call.effect, input: call.input });
      this.sites.set(seq, { kind: "open", call });
      return this.suspend(seq, call);
    }
    switch (logged.kind) {
      case "done":
        this.same(seq, logged.call, call);
        return this.decode(site, call, logged.output);
      case "reply":
        throw this.diverged(seq, "a model reply", call.tool);
      case "open":
      case "unknown": {
        this.same(seq, logged.call, call);
        this.requireLive(seq);
        const suspension = { turn: this.turn, seq, key: logged.call.input[SUSPEND_KEY] };
        const world = this.mode.world;
        const resume = await world.resume(suspension);
        if (resume !== null) {
          if (resume.key === suspension.key) {
            const output = { tag: "ok", value: resume.answer };
            this.append({ kind: "result", turn: this.turn, seq, output });
            this.sites.set(seq, { kind: "done", call: logged.call, output });
            await this.flush();
            await world.resumed(suspension, resume, true);
            return this.decode(site, call, output);
          }
          await world.resumed(suspension, resume, false);
        }
        return this.suspend(seq, logged.call);
      }
      default:
        throw new Panic(`unknown site log ${logged.kind}`);
    }
  }

  async suspend(seq, call) {
    await this.flush();
    assert(this.buffer.length === 0, "a suspend's call record is durable before the turn waits");
    await this.hook(runPoint("before_run", "suspend", "none"));
    const key = isObject(call.input) ? call.input[SUSPEND_KEY] : undefined;
    if (typeof key !== "string") throw new Corrupt(`turn ${this.turn} seq ${seq}: a suspend's input lacks its key`);
    throw new Suspended({ turn: this.turn, seq, key });
  }

  async waitSite(siteId, seq, call, args) {
    const site = this.handler.sites[siteId];
    const logged = this.sites.get(seq);
    if (logged === undefined) {
      this.requireLive(seq);
      const clock = await this.mode.world.clock();
      const ms = call.input[WAIT_MS];
      const until = clock + ms;
      if (until > U64_MAX) throw new Corrupt(`turn ${this.turn} seq ${seq}: the clock ${clock} plus the wait's ${ms} ms passes u64`);
      const timed = { ...call, input: object([[WAIT_MS, ms], [WAIT_UNTIL, until]]) };
      this.append({ kind: "call", turn: this.turn, seq, tool: timed.tool, effect: timed.effect, input: timed.input });
      this.sites.set(seq, { kind: "open", call: timed });
      await this.mode.world.beforeExternal();
      return this.execute(siteId, seq, timed, args);
    }
    switch (logged.kind) {
      case "done":
        this.same(seq, withoutUntil(logged.call), call);
        return this.decode(site, call, logged.output);
      case "reply":
        throw this.diverged(seq, "a model reply", call.tool);
      case "open":
      case "unknown":
        this.same(seq, withoutUntil(logged.call), call);
        return this.reopen(siteId, seq, logged.call, args);
      default:
        throw new Panic(`unknown site log ${logged.kind}`);
    }
  }

  async cellSite(code, seq, call) {
    const logged = this.sites.get(seq);
    if (logged !== undefined) {
      switch (logged.kind) {
        case "done": {
          this.same(seq, logged.call, call);
          const log = this.cells.get(seq);
          if (this.mutation === "cell_rerun_after_finish" && this.mode.live && log !== undefined) {
            await this.mode.world.cellStart(log.cell, log.header.script);
          }
          return this.decodeCell(logged.output);
        }
        case "unknown":
          this.same(seq, logged.call, call);
          return variant(VARIANT_UNKNOWN, []);
        case "reply":
          throw this.diverged(seq, "a model reply", call.tool);
        case "open":
          this.same(seq, logged.call, call);
          this.requireLive(seq);
          break;
        default:
          throw new Panic(`unknown site log ${logged.kind}`);
      }
    } else {
      this.requireLive(seq);
      this.append({ kind: "call", turn: this.turn, seq, tool: call.tool, effect: call.effect, input: call.input });
      this.sites.set(seq, { kind: "open", call });
    }
    const outcome = await this.runCell(code, seq, call);
    const output = cellOutcomeOutput(outcome);
    this.append({ kind: "result", turn: this.turn, seq, output });
    this.sites.set(seq, { kind: "done", call, output });
    return this.decodeCell(output);
  }

  async runCell(code, seq, call) {
    assert(typeof call.input === "string", "a cell's input is its script");
    const existing = this.cells.get(seq);
    if (existing !== undefined && existing.finished) {
      return { kind: "unknown", text: divergenceText(existing.cell, "it finished but its result was lost with the journal's tail") };
    }
    assert(this.nextCell > 0, "a live turn knows its session's next cell id");
    const site = {
      code,
      target: this.target,
      mutation: this.mutation,
      session: PROGRAM.name,
      turn: this.turn,
      seq,
      script: call.input,
      cell: existing === undefined ? { kind: "fresh", id: this.nextCell } : { kind: "existing", log: existing },
      nested: new Map(this.nested),
    };
    if (existing === undefined) this.nextCell += 1;
    return runCell(site, new EngineCellHost(this));
  }

  decodeCell(output) {
    const outcome = decodeOrCorrupt(() => cellOutcomeFrom(output));
    if (outcome.kind === "done") return variant(VARIANT_OK, [outcome.text]);
    if (outcome.kind === "threw") return variant(VARIANT_FAILED, [outcome.text]);
    return variant(VARIANT_UNKNOWN, []);
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
    this.append({ kind: "recovery", turn: this.turn, attempt, unknown: [seq], nested: [] });
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
          values[index] = this.settle(site, call, logged.output);
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
    assert(reportsUnknown(site.result), "a dispatch yields an outcome that reports unknown");
    const returned = group.map((waiting) => dispatchedReturns(waiting.call.tool));
    const gates = group.map((waiting) => this.gate(waiting.call));
    const sent = [];
    for (let position = 0; position < group.length; position++) {
      if (gates[position].refused === undefined) sent.push(position);
    }
    const requests = sent.map((position) => {
      const waiting = group[position];
      return { turn: this.turn, seq: waiting.seq, tool: waiting.call.tool, effect: waiting.call.effect, input: waiting.call.input, returns: returned[position], admitted: gates[position].admitted };
    });
    const batch = requests.length === 0 ? [] : await this.mode.world.tools(requests, CONCURRENT_REQUESTS_MAX);
    assert(batch.length === sent.length, "a batch returns one output per request");
    if (this.sites.get(this.seq)?.kind === "open") await this.drain();
    const completed = batch.map(([position, output]) => [sent[position], output]);
    for (let position = 0; position < group.length; position++) {
      if (gates[position].refused !== undefined) completed.push([position, { tag: "failed", error: gates[position].refused }]);
    }
    const ordered = this.mutation === "results_in_completion_order" ? completed : [...completed].sort((left, right) => left[0] - right[0]);
    for (const [position, raw] of ordered) {
      const waiting = group[position];
      const output = this.checked(waiting.call.tool, returned[position], raw);
      this.append({ kind: "result", turn: this.turn, seq: waiting.seq, output });
      this.sites.set(waiting.seq, { kind: "done", call: waiting.call, output });
      values[waiting.index] = this.settle(site, waiting.call, output);
    }
    if (late || inStep) await this.flush();
  }

  async dispatch(siteId, seq, call, args, step) {
    const site = this.handler.sites[siteId];
    const world = this.mode.world;
    await this.hook(runPoint("before_run", site.kind, call.effect));
    if (site.kind === "model") {
      const callsMax = PROGRAM.model === null ? 0n : PROGRAM.model.calls;
      const response = await world.model({ turn: this.turn, seq, callsMax, mode: site.mode, context: args[0], blobs: this.blobs });
      await this.hook(runPoint("after_run", site.kind, call.effect));
      await this.drain();
      this.append({ kind: "model", turn: this.turn, seq, response, mode: site.mode });
      this.sites.set(seq, { kind: "reply", response, mode: site.mode });
      return this.reply(response);
    }
    let output;
    if (site.kind === "wait") {
      const until = isObject(call.input) ? call.input[WAIT_UNTIL] : undefined;
      if (typeof until !== "bigint") throw new Corrupt(`turn ${this.turn} seq ${seq}: a wait's input lacks its deadline`);
      output = { tag: "ok", value: await world.sleep(this.turn, seq, until) };
    } else if (site.kind === "now") {
      output = { tag: "ok", value: await world.now() };
    } else if (site.kind === "random") {
      output = { tag: "ok", value: await world.random() };
    } else if (site.kind === "listing") {
      const listing = PROGRAM.listings[site.tool];
      output = checkedListing(listing, await world.listing({ turn: this.turn, seq, listing: listing.name }));
    } else {
      const returns = site.result.known ?? site.result.maybe ?? (reportsUnknown(site.result) ? dispatchedReturns(call.tool) : undefined);
      assert(returns !== undefined, "a tool site yields known or maybe");
      const gate = site.kind === "dispatch" ? this.gate(call) : { admitted: null };
      if (gate.refused !== undefined) {
        output = { tag: "failed", error: gate.refused };
      } else {
        const request = { turn: this.turn, seq, tool: call.tool, effect: call.effect, input: call.input, returns, admitted: gate.admitted };
        output = this.checked(call.tool, returns, await world.tool(request));
      }
    }
    await this.hook(runPoint("after_run", site.kind, call.effect));
    await this.drain();
    this.append({ kind: "result", turn: this.turn, seq, output });
    this.sites.set(seq, { kind: "done", call, output });
    if (step) await this.flush();
    return this.settle(site, call, output);
  }

  checked(tool, returns, raw) {
    let output = checkedOutput(tool, returns, raw);
    const listing = searchOf(tool);
    if (listing !== null) output = checkedSearch(tool, listing, this.admitted.get(listing), output);
    if (output.tag !== "ok" || blobReturned(tool) === null) return output;
    const data = hostData(output.value);
    if (data === null) return output;
    return { tag: "ok", value: this.blobs.journaled(data, true) };
  }

  settle(site, call, output) {
    this.admitListing(site, output);
    const listing = searchOf(call.tool);
    if (listing !== null && output.tag === "ok" && typeof output.value === "string") {
      if (!this.discovered.has(listing)) this.discovered.set(listing, new Set());
      const found = this.discovered.get(listing);
      for (const name of foundNames(output.value)) {
        const named = listed(name);
        if (named !== null) found.add(named.name);
      }
    }
    return this.decode(site, call, output);
  }

  gate(call) {
    if (typeof call.input === "string") {
      const typed = toolInput(call.tool, call.input);
      if (typed.kind === "invalid") return { refused: typed.error };
    }
    const found = listed(call.tool);
    if (found === null) return { admitted: null };
    const tools = this.admitted.get(found.listing);
    const fingerprint = tools === undefined ? undefined : tools.get(found.name);
    if (fingerprint === undefined) return { refused: notAdmitted(call.tool, PROGRAM.listings[found.listing].name, found.name) };
    const discovered = this.discovered.get(found.listing)?.has(found.name) ?? false;
    if (deferredListing(found.listing) && !discovered) return { refused: notDiscovered(call.tool, PROGRAM.listings[found.listing].name) };
    return { admitted: fingerprint };
  }

  admitListing(site, output) {
    if (site.kind !== "listing") return;
    if (output.tag !== "ok") {
      this.notices.set(site.tool, "");
      this.admitted.delete(site.tool);
      return;
    }
    let tools;
    try {
      tools = admittedTools(output.value);
    } catch (error) {
      if (!(error instanceof DecodeError)) throw error;
      throw new Corrupt(`turn ${this.turn} result: ${error.message}`);
    }
    const before = this.listed.get(site.tool);
    this.notices.set(site.tool, before === undefined ? "" : listingNotice(PROGRAM.listings[site.tool].name, before, tools));
    this.listed.set(site.tool, tools);
    this.admitted.set(site.tool, tools);
  }

  reply(response) {
    if (PROGRAM.model.window !== null) return attemptOutcome(response);
    if (PROGRAM.model.declared) return modelOutcome(response);
    try {
      return fromJson({ sum: SUM_REPLY }, response);
    } catch (error) {
      if (!(error instanceof DecodeError)) throw error;
      throw new Trap(`model reply: ${error.message}`);
    }
  }

  decode(site, call, output) {
    const corrupt = (message) => new Corrupt(`turn ${this.turn} result: ${message}`);
    if (reportsUnknown(site.result) && site.result.maybe === undefined) return this.dispatched(call, output, corrupt);
    if (site.result === "text" && output.tag === "ok") {
      if (typeof output.value !== "string") throw corrupt(`${encodeJson(output.value)} is not text`);
      return output.value;
    }
    if (site.result === "u64" && output.tag === "ok") {
      if (typeof output.value !== "bigint") throw corrupt(`${encodeJson(output.value)} is not u64`);
      return output.value;
    }
    if (site.kind === "listing" && output.tag === "ok") {
      try {
        return variant(VARIANT_OK, [BigInt(admittedTools(output.value).size)]);
      } catch (error) {
        if (!(error instanceof DecodeError)) throw error;
        throw corrupt(error.message);
      }
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

  dispatched(call, output, corrupt) {
    if (output.tag === "failed") return variant(VARIANT_FAILED, [output.error]);
    if (isObject(output.value) && blobReturned(call.tool) !== null) {
      try {
        return variant(DISPATCHED_BLOB, [digestOf(output.value)]);
      } catch (error) {
        if (!(error instanceof DecodeError)) throw error;
        throw corrupt(error.message);
      }
    }
    if (typeof output.value === "string") return variant(VARIANT_OK, [output.value]);
    throw corrupt(`${encodeJson(output.value)} is not text`);
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

class EngineCellHost {
  constructor(engine) {
    assert(engine.mode.live, "cells run only live");
    this.engine = engine;
    this.world = engine.mode.world;
  }

  append(record) {
    this.engine.append(record);
  }

  flush() {
    return this.engine.flush();
  }

  async flushWithholding(cell) {
    const engine = this.engine;
    const withheld = engine.buffer.filter((record) => record.kind === "cell" && record.cell === cell);
    engine.buffer = engine.buffer.filter((record) => !(record.kind === "cell" && record.cell === cell));
    try {
      await engine.flush();
    } finally {
      engine.buffer.push(...withheld);
    }
  }

  hook(point) {
    return this.world.hook(point);
  }

  start(cell, script, context) {
    return this.world.cellStart(cell, script, context);
  }

  resume(cell, input) {
    return this.world.cellResume(cell, input);
  }

  tool(request) {
    return this.world.tool(request);
  }

  attempt(seq, nested) {
    return this.world.nestedAttempt(this.engine.turn, seq, nested);
  }

  async recovered(seq, nested) {
    const engine = this.engine;
    engine.append({ kind: "recovery", turn: engine.turn, attempt: engine.nextAttempt(), unknown: [], nested: [[seq, nested]] });
    const key = nestedKey(seq, nested);
    const site = engine.nested.get(key);
    if (site !== undefined && site.kind === "open") engine.nested.set(key, { kind: "unknown", call: site.call });
    await engine.flush();
  }

  clock() {
    return this.world.now();
  }

  random() {
    return this.world.random();
  }

  armResume() {
    return this.world.beforeExternal();
  }

  async waitTimer(wait) {
    if (await this.world.waited(wait)) return;
    await this.world.hook(CELL_POINT_WAITING);
    await this.world.wait(wait);
  }

  observe(event) {
    return this.world.observe(event);
  }
}

function codeNamed(name) {
  const index = PROGRAM.codes.findIndex((code) => code.name === name);
  return index < 0 ? null : index;
}

async function closeInbox(world, turn, position, events) {
  const arrived = await world.inbox(turn);
  if (arrived.length === 0) return;
  await world.answer(turn, position, closingAnswers(events, arrived));
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
  checkEvents(turn, log);
  if (log.finished === null) throw new Corrupt(`turn ${turn} is replayed before it finished`);
  const replaySetup = { target: setup.target, mutation: "none" };
  const ran = await new Engine(replaySetup, handler, { live: false }, turn, state, log).run(args);
  if (ran.suspended !== undefined) throw new Corrupt(`replay of turn ${turn} suspends, where the journal says it finished`);
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

function assumeNestedFailed(log, ids) {
  return ids.map(([seq, nested]) => {
    const key = nestedKey(seq, nested);
    const site = log.nested.get(key);
    assert(site.kind === "unknown", "at-load recovery marks open nested calls unknown");
    const output = { tag: "failed", error: `${site.call.tool} was lost in a crash` };
    log.nested.set(key, { kind: "done", call: site.call, output });
    return [seq, nested, output];
  });
}

function recoverAtLoad(log, mutation) {
  const unknown = [];
  for (const [seq, site] of log.sites) {
    if (site.kind === "open" && mayBeUnknown(site.call.effect)) unknown.push(seq);
  }
  for (const seq of unknown) {
    log.sites.set(seq, { kind: "unknown", call: log.sites.get(seq).call });
  }
  const nested = [];
  const skipped = mutation === "cell_recovery_redispatches_external" || mutation === "recovery_skipped";
  for (const [key, site] of skipped ? [] : log.nested) {
    if (site.kind === "open" && mayBeUnknown(site.call.effect)) nested.push(key.split("/").map(Number));
  }
  nested.sort((left, right) => left[0] - right[0] || left[1] - right[1]);
  for (const [seq, id] of nested) {
    const key = nestedKey(seq, id);
    log.nested.set(key, { kind: "unknown", call: log.nested.get(key).call });
  }
  return { unknown, nested };
}

function cloneLog(log) {
  const cells = new Map([...log.cells].map(([seq, cell]) => [seq, cellLogClone(cell)]));
  return { ...log, sites: new Map(log.sites), nested: new Map(log.nested), lastNested: new Map(log.lastNested), cells, events: log.events.slice() };
}

async function runTurn(setup, world, epoch, records, request) {
  const session = fold(records);
  const handler = handlerNamed(request.handler);
  assert(handler !== null, "a turn request names a handler of the program");
  const args = argumentsJson(handler, request.arguments);
  const existing = session.turns.get(request.turn);
  if (existing !== undefined && existing.finished !== null) {
    if (eventsTaken()) await closeInbox(world, request.turn, existing.finished.last, existing.events);
    return { ran: "already_finished", finish: { turn: request.turn, outcome: existing.finished.outcome, result: existing.finished.result } };
  }
  const state = await stateBefore(setup, session, request.turn);
  const blobs = sessionBlobs(session);
  blobs.checkState(state);
  const pending = [];
  let unknown = { unknown: [], nested: [] };
  let log;
  if (existing !== undefined) {
    if (existing.handler !== request.handler || !jsonEqual(existing.arguments, args)) {
      throw new Corrupt(`turn ${request.turn} was admitted with other arguments`);
    }
    checkEvents(request.turn, existing);
    log = cloneLog(existing);
    if (setup.target.recovery === "at_load") unknown = recoverAtLoad(log, setup.mutation);
  } else {
    const next = session.turns.size === 0 ? 1 : session.lastTurn + 1;
    if (request.turn !== next) throw new Corrupt(`turn ${request.turn} is requested when turn ${next} is next`);
    pending.push({ kind: "admitted", turn: request.turn, handler: request.handler, arguments: args });
    log = newTurnLog(request.handler, null);
  }
  const assumed = setup.mutation === "cell_recovery_reports_failed" ? assumeNestedFailed(log, unknown.nested.splice(0)) : [];
  const engine = new Engine(setup, handler, { live: true, world, epoch }, request.turn, state, log);
  engine.nextCell = nextCell(session);
  engine.blobs = blobs;
  for (const record of pending) engine.append(record);
  if (unknown.unknown.length > 0 || unknown.nested.length > 0) {
    engine.append({ kind: "recovery", turn: request.turn, attempt: engine.nextAttempt(), unknown: unknown.unknown, nested: unknown.nested });
  }
  for (const [seq, nested, output] of assumed) {
    engine.append({ kind: "nested_result", turn: request.turn, seq, nested, output });
  }
  const ran = await engine.run(request.arguments);
  if (ran.suspended !== undefined) return { ran: "suspended", suspension: ran.suspended };
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

function suspensionJson(suspension) {
  return object([["key", suspension.key], ["seq", BigInt(suspension.seq)], ["turn", BigInt(suspension.turn)]]);
}

function suspensionFrom(json) {
  if (!isObject(json)) throw new DecodeError("a suspension is an object");
  if (typeof json.key !== "string") throw new DecodeError("a suspension names its key");
  return { turn: smallNumber(json.turn, "a suspension's turn"), seq: smallNumber(json.seq, "a suspension's seq"), key: json.key };
}

function resumeJson(resume) {
  return object([["answer", resume.answer], ["key", resume.key]]);
}

function resumeFrom(json) {
  if (!isObject(json) || typeof json.key !== "string" || typeof json.answer !== "string") throw new DecodeError("a resume carries a text key and a text answer");
  return { key: json.key, answer: json.answer };
}

function finishFromJson(json) {
  return { turn: smallNumber(json.turn, "a finish's turn"), outcome: json.outcome, result: json.result };
}

function toolRequestJson(request) {
  const entries = [
    ["effect", request.effect],
    ["input", request.input],
    ["nested", request.nested === undefined || request.nested === null ? null : BigInt(request.nested.id)],
    ["returns", typeName(request.returns)],
    ["seq", BigInt(request.seq)],
    ["tool", request.tool],
    ["turn", BigInt(request.turn)],
  ];
  if (request.admitted !== null) entries.push(["admitted", request.admitted]);
  return object(entries);
}

function listingRequestJson(request) {
  return object([
    ["listing", request.listing],
    ["seq", BigInt(request.seq)],
    ["turn", BigInt(request.turn)],
  ]);
}

function modelRequestJson(request) {
  const entries = [
    ["calls_max", request.callsMax],
    ["context", toJson(CONTEXT_TYPE, request.context)],
    ["seq", BigInt(request.seq)],
    ["turn", BigInt(request.turn)],
  ];
  if (request.mode !== null) entries.push([MODEL_MODE, request.mode]);
  if (PROGRAM.blobs !== undefined) entries.push([MODEL_BLOBS, request.blobs.json()]);
  return object(entries);
}

function modeText(mode) {
  return mode === null ? "no mode" : `mode ${mode}`;
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
    async listing(request) {
      return outputFrom(await post(config.listingsUrl, listingRequestJson(request)));
    },
    async model(request) {
      return post(config.modelUrl, modelRequestJson(request));
    },
    now() {
      return BigInt(config.now());
    },
    clock() {
      return BigInt(config.now());
    },
    async sleep(turn, seq, untilMs) {
      const left = Number(untilMs) - Number(config.now());
      if (left > 0) await new Promise((resolve) => setTimeout(resolve, left));
      const clock = BigInt(config.now());
      return clock > untilMs ? clock : untilMs;
    },
    async resume() {
      return null;
    },
    async resumed() {},
    random() {
      const value = config.random();
      assert(typeof value === "bigint" && value >= 0n && value <= U64_MAX, "random yields a u64");
      return value;
    },
    async wait(wait) {
      const left = Number(wait.untilMs) - Number(config.now());
      if (left > 0) await new Promise((resolve) => setTimeout(resolve, left));
    },
    async observe() {},
  };
}

function platformRandom() {
  return crypto.getRandomValues(new BigUint64Array(1))[0];
}

const runtime = {
  Panic, Trap, Corrupt, DecodeError, HostError,
  encodeJson, decodeJson, crc32, encodeLine, decodeLine, encodeLines, decodeLines,
  recordJson, recordFrom, fold, toJson, fromJson, finishJson, defaultHost,
  Fraction, object, cellOutcomeOutput, ScriptEngine, scriptEngineModule, PROGRAM, CELL_SCRIPT_PARAMETERS, sameStep, entryIsInput, isObject,
  deliveryJson, deliveryFrom, suspensionJson, suspensionFrom, resumeJson, resumeFrom,
};
