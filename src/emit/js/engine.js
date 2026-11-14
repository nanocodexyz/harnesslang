const WAIT_TOOL = "wait";
const HOLD_TOOL = "hold";
const WAIT_MS = "ms";
const WAIT_UNTIL = "until";
const SUSPEND_TOOL = "suspend";
const SUSPEND_KEY = "key";
const DENY_TOOL = "deny";
const SPAWN_KEY_SEPARATOR = ".";
const SPAWN_SCOPE_SEPARATOR = "/";
const ROOT_SCOPE = "";
const SIDE_TURN = 0;
const SIDE_REFUSED = "the checker refuses this effect in a side handler";

function spawnOutput(tool, scope, turn, seq, output) {
  const declared = PROGRAM.tools.find((candidate) => candidate.name === tool);
  if (output.tag !== "ok" || declared === undefined || !declared.spawn) return output;
  const key = String(turn) + SPAWN_KEY_SEPARATOR + String(seq);
  return { tag: "ok", value: scope === ROOT_SCOPE ? key : scope + SPAWN_SCOPE_SEPARATOR + key };
}

class StepRan {
  constructor(ran) {
    this.ran = ran;
  }
}

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
    this.takenJobs = 0;
    this.jobs = new Map();
    this.lost = [];
    this.attested = new Set();
    this.undispatchedBefore = log.undispatched;
    this.admitted = new Map();
    this.listed = new Map();
    this.notices = new Map();
    this.discovered = new Map();
    this.blobs = new Blobs();
    this.snapshot = null;
  }

  async sideQuestion(seq) {
    if (PROGRAM.sides.length === 0) return;
    const world = this.mode.world;
    if (world.side === undefined) return;
    const request = await world.side(this.turn, seq);
    if (request === null) return;
    assert(this.snapshot !== null, "a live run of a program with side handlers keeps its snapshot");
    let answer;
    try {
      answer = await runSide({ target: this.target, mutation: "none" }, world, this.snapshot, this.blobs, request);
    } catch (error) {
      if (!(error instanceof Corrupt)) throw error;
      answer = { error: error.message };
    }
    await world.sided(request, answer);
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

  take(kind) {
    assert(kind === "steer" || kind === "job", "take reads steers or job notices");
    const counter = kind === "job" ? "takenJobs" : "taken";
    const visible = this.events.filter((event) => event.delivery.kind === kind && event.at <= this.seq);
    assert(this[counter] <= visible.length, "the events a turn sees only grow");
    const fresh = visible.slice(this[counter]).map((event) => (kind === "job" ? event.delivery.notice : event.delivery.message));
    this[counter] = visible.length;
    return fresh;
  }

  async drain() {
    if (this.target.inbound !== "after_run") return;
    await this.drainNow();
  }

  async drainDirect(ran) {
    if (!eventsTaken() || !this.mode.live) return;
    if (this.target.inbound === "after_run") {
      await this.drainNow();
      return;
    }
    assert(this.buffer.length === 0, "an effect that drains right after it runs flushed before it ran");
    throw new StepRan(ran);
  }

  async drainNow() {
    if (!eventsTaken() || !this.mode.live) return;
    const world = this.mode.world;
    const queued = this.lost.splice(0);
    const arrived = [...queued, ...(await world.inbox(this.turn))];
    if (arrived.length === 0) return;
    const position = this.seq;
    const answers = [];
    let recorded = false;
    const jobs = { open: this.jobs, recorded: (key) => jobRecorded(this.sites, this.turn, key) };
    for (let index = 0; index < arrived.length; index++) {
      const delivery = arrived[index];
      const admitted = admitEvent(this.turn, this.events, position, delivery, jobs);
      if (admitted.record !== null) {
        this.append(admitted.record);
        recorded = true;
      }
      if (index < queued.length) {
        if (admitted.answer === "full") this.lost.push(delivery);
        continue;
      }
      answers.push([delivery.id, admitted.answer]);
    }
    if (recorded) await this.flush();
    await world.answer(this.turn, position, answers);
  }

  preempted(by) {
    if (!anyInterrupt(by)) return false;
    const cancelled = by.cancel && this.events.some((event) => event.delivery.kind === "cancel");
    const steered = by.steer && steerCount(this.events) > this.taken;
    return cancelled || steered;
  }

  flushesToInterrupt(by) {
    return anyInterrupt(by) && this.target.inbound !== "after_run";
  }

  jobRoom(tool) {
    const bound = jobsOf(tool);
    if (bound === null) return false;
    let open = 0;
    for (const owner of this.jobs.values()) if (owner === tool) open += 1;
    return BigInt(open) < bound;
  }

  toolOutput(tool, returns, seq, room, raw) {
    if (jobStarted(raw) === null) return this.checked(tool, returns, raw);
    if (!room) return { tag: "failed", error: `tool ${tool} started a job, which it may not: it is not a jobs tool or keeps its bound of jobs running` };
    this.jobs.set(spawnKey(this.turn, seq), tool);
    return raw;
  }

  startedJobs(entries) {
    const started = [];
    for (const [seq, tool, output] of entries) {
      const key = spawnKey(this.turn, seq);
      if (output.tag === "ok" && jobStarted(output) !== null && this.jobs.has(key)) started.push([key, tool]);
    }
    return started;
  }

  async ran(seq) {
    const world = this.mode.world;
    if (world.ran === undefined) return null;
    const ran = await world.ran(this.turn, seq);
    if (ran === null) return null;
    for (const [key, tool] of ran.started) this.jobs.set(key, tool);
    return ran;
  }

  async freshAfterRecovery(seq) {
    const world = this.mode.world;
    if (world.undispatched === undefined) return false;
    return world.undispatched(this.turn, seq);
  }

  async hold(seq, call) {
    if (this.jobs.size === 0 && this.lost.length === 0) return this.mode.world.clock();
    const until = isObject(call.input) ? call.input[WAIT_UNTIL] : undefined;
    if (typeof until !== "bigint") throw new Corrupt(`turn ${this.turn} seq ${seq}: a hold's input lacks its deadline`);
    return this.mode.world.hold(this.turn, seq, until);
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
    await world.flush(this.mode.epoch, this.buffer.slice(), this.seq);
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
      case "hold":
        return { tool: HOLD_TOOL, effect: "none", input: object([[WAIT_MS, args[0]]]) };
      case "suspend":
        return { tool: SUSPEND_TOOL, effect: "none", input: object([[SUSPEND_KEY, args[0]]]) };
      case "deny": {
        const fields = args[0].f;
        const input = object([["id", fields[CALL_ID]], ["input", fields[CALL_INPUT]], ["reason", args[1]], ["tool", fields[CALL_TOOL]]]);
        return { tool: DENY_TOOL, effect: "none", input };
      }
      default:
        return { tool: site.kind, effect: "none", input: null };
    }
  }

  async effect(siteId, args) {
    const site = this.handler.sites[siteId];
    const sleeps = site.kind === "wait" || site.kind === "hold";
    if (sleeps && args[0] > site.ms) throw new Trap(`bound(${site.cap})`);
    this.seq += 1;
    const seq = this.seq;
    const call = this.resolve(site, args);
    const code = site.kind === "cell" ? site.code : site.kind === "dispatch" ? codeNamed(call.tool) : null;
    if (code !== null) return this.cellSite(code, seq, { ...call, effect: "none" });
    if (site.kind === "suspend") return this.suspendSite(siteId, seq, call);
    if (sleeps) return this.waitSite(siteId, seq, call, args);
    if (site.class !== "dispatched") {
      assert(site.class === call.effect, "a fixed site runs with its declared class");
    }
    const logged = this.sites.get(seq);
    if (logged !== undefined) return this.recorded(siteId, seq, call, args, logged);
    this.requireLive(seq);
    await this.sideQuestion(seq);
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
        if (!jsonEqual(logged.settings, settingsOf(args))) {
          throw this.diverged(seq, `a model reply with ${settingsText(logged.settings)}`, `${call.tool} with ${settingsText(settingsOf(args))}`);
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
      const world = this.mode.world;
      const ran = world.ran === undefined ? null : await world.ran(this.turn, seq);
      if (ran !== null || (await this.freshAfterRecovery(seq))) return this.dispatch(siteId, seq, call, args, true);
      const attempt = await world.attempt(this.turn, seq, this.undispatchedBefore.has(seq));
      if (attempt > 1 && this.mutation !== "vercel_retry_redispatches") {
        const retried = await this.retried(seq, call);
        if (retried.value !== undefined) return retried.value;
        if (retried.output !== undefined) return this.settle(this.handler.sites[siteId], call, retried.output);
      }
      return this.dispatch(siteId, seq, call, args, true);
    }
    const attested = this.attested.delete(seq);
    assert(!mayBeUnknown(call.effect) || attested, "recovery turns every open external call unknown before the turn runs again, unless its host attested it was not dispatched");
    return this.execute(siteId, seq, call, args);
  }

  async retried(seq, call) {
    const recovered = await this.askRecover(seq, call);
    const kind = recovered.kind;
    const attempt = this.nextAttempt();
    this.append({ kind: "recovery", turn: this.turn, attempt, unknown: kind === "unknown" ? [seq] : [], nested: [], replied: kind === "reply" ? [seq] : [], undispatched: kind === "not_dispatched" ? [seq] : [] });
    if (kind === "unknown") {
      this.sites.set(seq, { kind: "unknown", call });
      await this.flush();
      return { value: variant(VARIANT_UNKNOWN, []) };
    }
    if (kind === "not_dispatched") {
      await this.flush();
      return {};
    }
    assert(kind === "reply", "a host answers a recovery with a reply, not dispatched or unknown");
    this.append({ kind: "result", turn: this.turn, seq, output: recovered.output });
    this.sites.set(seq, { kind: "done", call, output: recovered.output });
    await this.flush();
    return { output: recovered.output };
  }

  async askRecover(seq, call) {
    if (!recoverable(call.tool)) return { kind: "unknown" };
    const returns = dispatchedReturns(call.tool);
    const room = this.jobRoom(call.tool);
    const recovered = await this.mode.world.recover({ turn: this.turn, seq, tool: call.tool, effect: call.effect, input: call.input, returns, admitted: null, job: room, interrupts: interruptsOf("dispatch", call.tool) });
    if (recovered.kind !== "reply") return recovered;
    return { kind: "reply", output: this.toolOutput(call.tool, returns, seq, room, recovered.output) };
  }

  async unknown(seq, call) {
    const attempt = this.nextAttempt();
    this.append({ kind: "recovery", turn: this.turn, attempt, unknown: [seq], nested: [], replied: [], undispatched: [] });
    this.sites.set(seq, { kind: "unknown", call });
    await this.flush();
    return variant(VARIANT_UNKNOWN, []);
  }

  async execute(siteId, seq, call, args) {
    const site = this.handler.sites[siteId];
    const gate = this.target.gateBeforeOutbound && outbound(site.kind);
    const required = flushes(site.durability, call.effect) || this.flushesToInterrupt(interruptsOf(site.kind, call.tool));
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
          const recovered = !mayBeUnknown(call.effect) || this.target.recovery === "in_step" || this.attested.delete(seq);
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
    const by = group.map((waiting) => interruptsOf("dispatch", waiting.call.tool));
    const interrupting = { cancel: by.some((one) => one.cancel), steer: by.some((one) => one.steer) };
    const gate = this.target.gateBeforeOutbound;
    const inStep = external && this.target.recovery === "in_step";
    const late = external && !gate && this.mutation === "batch_flush_after_dispatch";
    if (external) await this.mode.world.beforeExternal();
    if ((external || gate || this.flushesToInterrupt(interrupting)) && !late) {
      await this.flush();
      assert(this.buffer.length === 0, "every call record of a parallel for is durable before an external call of it runs");
    }
    const first = group[0];
    let ran = await this.ran(first.seq);
    if (inStep && ran === null && !(await this.freshAfterRecovery(first.seq))) {
      const attempt = await this.mode.world.attempt(this.turn, first.seq, this.undispatchedBefore.has(first.seq));
      if (first.reopened && attempt > 1 && this.mutation !== "vercel_retry_redispatches") {
        const retried = await this.retried(first.seq, first.call);
        if (retried.value !== undefined) {
          values[first.index] = retried.value;
          return;
        }
        if (retried.output !== undefined) {
          values[first.index] = this.settle(site, first.call, retried.output);
          return;
        }
      } else {
        assert(first.reopened || attempt === 1, "a fresh external step starts at its first attempt");
      }
    }
    assert(reportsUnknown(site.result), "a dispatch yields an outcome that reports unknown");
    if (ran === null) {
      ran = await this.batchRun(group, by);
      if (this.sites.get(this.seq)?.kind === "open") {
        if (this.flushesToInterrupt(interrupting)) await this.drainDirect(ran);
        else await this.drain();
      }
    }
    const arrived = this.events.slice(ran.events);
    for (const [position, output] of ran.outputs) {
      const waiting = group[position];
      const interrupted = !ran.preempted[position] && arrived.some((event) => interruptedBy(event.delivery, by[position]));
      if (interrupted) this.jobs.delete(spawnKey(this.turn, waiting.seq));
      if (interrupted && mayBeUnknown(waiting.call.effect)) {
        values[waiting.index] = await this.unknown(waiting.seq, waiting.call);
        continue;
      }
      const recorded = interrupted ? { tag: "failed", error: INTERRUPTED_REASON } : output;
      this.append({ kind: "result", turn: this.turn, seq: waiting.seq, output: recorded });
      this.sites.set(waiting.seq, { kind: "done", call: waiting.call, output: recorded });
      values[waiting.index] = this.settle(site, waiting.call, recorded);
    }
    if (late || inStep) await this.flush();
  }

  async batchRun(group, by) {
    const preempted = group.map((waiting, position) => this.preempted(by[position]));
    const returned = group.map((waiting) => dispatchedReturns(waiting.call.tool));
    const rooms = group.map((waiting) => this.jobRoom(waiting.call.tool));
    const gates = group.map((waiting, position) => (preempted[position] ? { refused: INTERRUPTED_REASON } : this.gate(waiting.call)));
    const sent = [];
    for (let position = 0; position < group.length; position++) {
      if (gates[position].refused === undefined) sent.push(position);
    }
    const requests = sent.map((position) => {
      const waiting = group[position];
      return { turn: this.turn, seq: waiting.seq, tool: waiting.call.tool, effect: waiting.call.effect, input: waiting.call.input, returns: returned[position], admitted: gates[position].admitted, job: rooms[position], interrupts: by[position] };
    });
    const batch = requests.length === 0 ? [] : await this.mode.world.tools(requests, CONCURRENT_REQUESTS_MAX);
    assert(batch.length === sent.length, "a batch returns one output per request");
    const completed = batch.map(([position, output]) => [sent[position], output]);
    for (let position = 0; position < group.length; position++) {
      if (gates[position].refused !== undefined) completed.push([position, { tag: "failed", error: gates[position].refused }]);
    }
    const ordered = this.mutation === "results_in_completion_order" ? completed : [...completed].sort((left, right) => left[0] - right[0]);
    const outputs = ordered.map(([position, raw]) => {
      const waiting = group[position];
      return [position, spawnOutput(waiting.call.tool, this.mode.scope, this.turn, waiting.seq, this.toolOutput(waiting.call.tool, returned[position], waiting.seq, rooms[position], raw))];
    });
    const started = this.startedJobs(outputs.map(([position, output]) => [group[position].seq, group[position].call.tool, output]));
    return { seq: group[0].seq, position: this.seq, events: this.events.length, model: false, preempted, outputs, started };
  }

  async dispatch(siteId, seq, call, args, step) {
    const site = this.handler.sites[siteId];
    const by = interruptsOf(site.kind, call.tool);
    let ran = await this.ran(seq);
    if (ran === null) {
      await this.hook(runPoint("before_run", site.kind, call.effect));
      const preempted = this.preempted(by);
      const output = await this.runEffect(site, seq, call, args, preempted, by);
      await this.hook(runPoint("after_run", site.kind, call.effect));
      const model = site.kind === "model";
      const started = model ? [] : this.startedJobs([[seq, call.tool, output]]);
      ran = { seq, position: this.seq, events: this.events.length, model, preempted: [preempted], outputs: [[0, output]], started };
      if (anyInterrupt(by) || site.kind === "hold") await this.drainDirect(ran);
      else await this.drain();
    }
    const output = ran.outputs[0][1];
    const interrupted = !ran.preempted[0] && this.events.slice(ran.events).some((event) => interruptedBy(event.delivery, by));
    if (site.kind === "model") {
      const response = interrupted ? interruptedResponse() : output;
      const settings = settingsOf(args);
      this.append({ kind: "model", turn: this.turn, seq, response, mode: site.mode, settings });
      this.sites.set(seq, { kind: "reply", response, mode: site.mode, settings });
      return this.reply(response);
    }
    if (interrupted) {
      this.jobs.delete(spawnKey(this.turn, seq));
      if (mayBeUnknown(call.effect)) return this.unknown(seq, call);
    }
    const recorded = interrupted ? { tag: "failed", error: INTERRUPTED_REASON } : output;
    this.append({ kind: "result", turn: this.turn, seq, output: recorded });
    this.sites.set(seq, { kind: "done", call, output: recorded });
    if (step) await this.flush();
    return this.settle(site, call, recorded);
  }

  async runEffect(site, seq, call, args, preempted, by) {
    const world = this.mode.world;
    if (site.kind === "model") {
      if (preempted) return interruptedResponse();
      const callsMax = PROGRAM.model === null ? 0n : PROGRAM.model.calls;
      return world.model({ turn: this.turn, seq, callsMax, mode: site.mode, settings: settingsOf(args), context: args[0], blobs: this.blobs, interrupts: by });
    }
    let output;
    if (site.kind === "wait") {
      const until = isObject(call.input) ? call.input[WAIT_UNTIL] : undefined;
      if (typeof until !== "bigint") throw new Corrupt(`turn ${this.turn} seq ${seq}: a wait's input lacks its deadline`);
      output = { tag: "ok", value: await world.sleep(this.turn, seq, until) };
    } else if (site.kind === "hold") {
      output = { tag: "ok", value: await this.hold(seq, call) };
    } else if (site.kind === "now") {
      output = { tag: "ok", value: await world.now() };
    } else if (site.kind === "random") {
      output = { tag: "ok", value: await world.random() };
    } else if (site.kind === "deny") {
      output = { tag: "failed", error: args[1] };
    } else if (site.kind === "listing") {
      const listing = PROGRAM.listings[site.tool];
      output = checkedListing(listing, await world.listing({ turn: this.turn, seq, listing: listing.name }));
    } else if (preempted) {
      output = { tag: "failed", error: INTERRUPTED_REASON };
    } else {
      const returns = site.result.known ?? site.result.maybe ?? (reportsUnknown(site.result) ? dispatchedReturns(call.tool) : undefined);
      assert(returns !== undefined, "a tool site yields known or maybe");
      const gate = site.kind === "dispatch" ? this.gate(call) : { admitted: null };
      if (gate.refused !== undefined) {
        output = { tag: "failed", error: gate.refused };
      } else {
        const room = this.jobRoom(call.tool);
        const request = { turn: this.turn, seq, tool: call.tool, effect: call.effect, input: call.input, returns, admitted: gate.admitted, job: room, interrupts: by };
        output = spawnOutput(call.tool, this.mode.scope, this.turn, seq, this.toolOutput(call.tool, returns, seq, room, await world.tool(request)));
      }
    }
    return output;
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
    const job = jobStarted(output);
    if (job !== null) {
      if (jobsOf(call.tool) === null) throw corrupt(`${call.tool} started a job, which only a jobs tool does`);
      return variant(VARIANT_OK, [job]);
    }
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
    engine.append({ kind: "recovery", turn: engine.turn, attempt: engine.nextAttempt(), unknown: [], nested: [[seq, nested]], replied: [], undispatched: [] });
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
  const arrived = (await world.inbox(turn)).filter((delivery) => delivery.kind !== "job");
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

async function recoverLoaded(world, turn, seq, call) {
  if (!recoverable(call.tool)) return { kind: "unknown" };
  const recovered = await world.recover({ turn, seq, tool: call.tool, effect: call.effect, input: call.input, returns: "text", admitted: null, job: false, interrupts: interruptsOf("dispatch", call.tool) });
  if (recovered.kind !== "reply") return recovered;
  if (jobStarted(recovered.output) !== null && jobsOf(call.tool) !== null) return recovered;
  return { kind: "reply", output: checkedOutput(call.tool, "text", recovered.output) };
}

async function recoverAtLoad(world, turn, log, mutation) {
  const unknown = [];
  const replied = [];
  const undispatched = [];
  for (const [seq, site] of [...log.sites]) {
    if (site.kind !== "open" || !mayBeUnknown(site.call.effect)) continue;
    const recovered = await recoverLoaded(world, turn, seq, site.call);
    if (recovered.kind === "unknown") {
      log.sites.set(seq, { kind: "unknown", call: site.call });
      unknown.push(seq);
    } else if (recovered.kind === "not_dispatched") {
      undispatched.push(seq);
    } else {
      assert(recovered.kind === "reply", "a host answers a recovery with a reply, not dispatched or unknown");
      log.sites.set(seq, { kind: "done", call: site.call, output: recovered.output });
      replied.push([seq, recovered.output]);
    }
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
  return { unknown, nested, replied, undispatched };
}

async function lostJobs(world, turn, session) {
  if (PROGRAM.events.jobs === null || session.jobs.size === 0) return [];
  const open = [...session.jobs.keys()].sort();
  const lost = await world.jobs(turn, open);
  return lost.filter((key) => session.jobs.has(key)).map((key) => lostDelivery(key));
}

function cloneLog(log) {
  const cells = new Map([...log.cells].map(([seq, cell]) => [seq, cellLogClone(cell)]));
  return { ...log, sites: new Map(log.sites), nested: new Map(log.nested), lastNested: new Map(log.lastNested), cells, events: log.events.slice() };
}

const IDLE_AT = "at";
const IDLE_DUE = "due";

function idleArmed(state) {
  const idle = PROGRAM.idle;
  if (idle === null) return null;
  const due = state[idle.due];
  return due !== 0n && state[idle.runs] < idle.bound ? due : null;
}

async function idleDue(setup, records) {
  if (PROGRAM.idle === null) return null;
  const session = fold(records);
  const last = session.turns.get(session.lastTurn);
  if (last !== undefined && last.finished === null) return null;
  const state = await stateBefore(setup, session, session.lastTurn + 1);
  return idleArmed(state);
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
  const idles = PROGRAM.idle !== null && PROGRAM.idle.handler === request.handler;
  const lastLog = session.turns.get(session.lastTurn);
  if (idles && existing === undefined && lastLog !== undefined && lastLog.finished === null) return { ran: "not_due", notDue: { kind: "busy" } };
  const state = await stateBefore(setup, session, request.turn);
  const blobs = sessionBlobs(session);
  blobs.checkState(state);
  const pending = [];
  let unknown = { unknown: [], nested: [], replied: [], undispatched: [] };
  let log;
  if (existing !== undefined) {
    if (existing.handler !== request.handler || (!idles && !jsonEqual(existing.arguments, args))) {
      throw new Corrupt(`turn ${request.turn} was admitted with other arguments`);
    }
    checkEvents(request.turn, existing);
    log = cloneLog(existing);
    if (setup.target.recovery === "at_load") unknown = await recoverAtLoad(world, request.turn, log, setup.mutation);
  } else {
    const next = session.turns.size === 0 ? 1 : session.lastTurn + 1;
    if (request.turn !== next) throw new Corrupt(`turn ${request.turn} is requested when turn ${next} is next`);
    let admitted = args;
    if (idles) {
      const due = idleArmed(state);
      if (due === null) return { ran: "not_due", notDue: { kind: "unarmed" } };
      const clock = await world.clock();
      if (clock < due) return { ran: "not_due", notDue: { kind: "early", due, clock } };
      admitted = object([[IDLE_AT, clock], [IDLE_DUE, due]]);
    }
    pending.push({ kind: "admitted", turn: request.turn, handler: request.handler, arguments: admitted });
    log = newTurnLog(request.handler, null);
  }
  const assumed = setup.mutation === "cell_recovery_reports_failed" ? assumeNestedFailed(log, unknown.nested.splice(0)) : [];
  const scope = request.scope === undefined ? ROOT_SCOPE : request.scope;
  assert(typeof scope === "string", "a turn request's scope is text");
  const engine = new Engine(setup, handler, { live: true, world, epoch, scope }, request.turn, state, log);
  if (PROGRAM.sides.length > 0) engine.snapshot = stateFromJson(stateJson(state));
  engine.nextCell = nextCell(session);
  engine.blobs = blobs;
  engine.jobs = new Map(session.jobs);
  for (const [seq, output] of unknown.replied) {
    if (jobStarted(output) !== null) engine.jobs.set(spawnKey(request.turn, seq), log.sites.get(seq).call.tool);
  }
  engine.lost = await lostJobs(world, request.turn, session);
  engine.attested = new Set(unknown.undispatched);
  for (const record of pending) engine.append(record);
  if (unknown.unknown.length > 0 || unknown.nested.length > 0 || unknown.replied.length > 0 || unknown.undispatched.length > 0) {
    engine.append({ kind: "recovery", turn: request.turn, attempt: engine.nextAttempt(), unknown: unknown.unknown, nested: unknown.nested, replied: unknown.replied.map(([seq]) => seq), undispatched: unknown.undispatched });
  }
  for (const [seq, output] of unknown.replied) {
    engine.append({ kind: "result", turn: request.turn, seq, output });
  }
  for (const [seq, nested, output] of assumed) {
    engine.append({ kind: "nested_result", turn: request.turn, seq, nested, output });
  }
  const ran = await engine.run(request.arguments);
  if (ran.suspended !== undefined) return { ran: "suspended", suspension: ran.suspended };
  return { ran: "finished", finish: ran.finish };
}

class SideWorld {
  constructor(world) {
    this.world = world;
    this.answered = null;
  }

  append() {}

  async hook() {}

  async beforeExternal() {
    throw new Panic(SIDE_REFUSED + ": a side question runs no external call");
  }

  async flush() {}

  async attempt() {
    throw new Panic(SIDE_REFUSED + ": a side question runs no external call");
  }

  async nestedAttempt() {
    throw new Panic(SIDE_REFUSED + ": a code cell");
  }

  cellStart() {
    throw new Panic(SIDE_REFUSED + ": a code cell");
  }

  cellResume() {
    throw new Panic(SIDE_REFUSED + ": a code cell");
  }

  async waited() {
    throw new Panic(SIDE_REFUSED + ": a code cell's timer");
  }

  async wait() {
    throw new Panic(SIDE_REFUSED + ": a code cell's timer");
  }

  async observe() {
    throw new Panic(SIDE_REFUSED + ": a code cell");
  }

  tool(request) {
    assert(request.turn === SIDE_TURN, "a side question's tool calls carry the side turn");
    return this.world.tool(request);
  }

  listing() {
    throw new Panic(SIDE_REFUSED + ": a listing read");
  }

  async tools() {
    throw new Panic(SIDE_REFUSED + ": a parallel for");
  }

  model(request) {
    assert(request.turn === SIDE_TURN, "a side question's model calls carry the side turn");
    return this.world.model(request);
  }

  now() {
    throw new Panic(SIDE_REFUSED + ": now");
  }

  random() {
    throw new Panic(SIDE_REFUSED + ": random");
  }

  async acknowledge(finish) {
    assert(this.answered === null, "a side question is answered once");
    this.answered = finish;
  }

  async inbox(turn) {
    assert(turn === SIDE_TURN, "a side question reads no turn's inbox");
    return [];
  }

  async answer() {
    throw new Panic("a side question takes no events, so it answers none");
  }

  clock() {
    throw new Panic(SIDE_REFUSED + ": wait");
  }

  sleep() {
    throw new Panic(SIDE_REFUSED + ": wait");
  }

  async resume() {
    throw new Panic(SIDE_REFUSED + ": suspend");
  }

  async resumed() {
    throw new Panic(SIDE_REFUSED + ": suspend");
  }

  async recover() {
    throw new Panic(SIDE_REFUSED + ": a side question runs no external call");
  }

  async jobs() {
    throw new Panic(SIDE_REFUSED + ": a side question starts no turn");
  }

  async hold() {
    throw new Panic(SIDE_REFUSED + ": hold");
  }
}

function sideNamed(name) {
  return PROGRAM.sides.find((side) => side.name === name) ?? null;
}

async function runSide(setup, world, state, blobs, request) {
  const side = sideNamed(request.handler);
  assert(side !== null, "a side request names a side handler of the program");
  const scratch = new SideWorld(world);
  const engine = new Engine(setup, side, { live: true, world: scratch, epoch: 0 }, SIDE_TURN, stateFromJson(stateJson(state)), newTurnLog(side.name, null));
  engine.blobs = blobs;
  const ran = await engine.run(request.arguments);
  assert(ran.suspended === undefined, SIDE_REFUSED + ": a side handler never suspends");
  assert(scratch.answered === ran.finish, "a side question's finish is its answer");
  return { outcome: ran.finish.outcome, result: ran.finish.result };
}

function lastFinished(session) {
  let last = 0;
  for (const [turn, log] of session.turns) {
    if (log.finished !== null && turn > last) last = turn;
  }
  return last;
}

async function answerSide(setup, world, records, request) {
  const session = fold(records);
  const state = await stateBefore(setup, session, lastFinished(session) + 1);
  const blobs = sessionBlobs(session);
  blobs.checkState(state);
  return runSide(setup, world, state, blobs, request);
}

function sideRequestFromJson(json) {
  if (!isObject(json) || typeof json.handler !== "string") throw new DecodeError("a side question names its side handler");
  const side = sideNamed(json.handler);
  if (side === null) throw new DecodeError(`no side handler '${json.handler}'`);
  return { handler: json.handler, arguments: argumentsFromJson(side, json.arguments) };
}

function sideAnswerJson(answer) {
  return object([["outcome", answer.outcome], ["result", answer.result]]);
}

function turnRequestFromJson(json) {
  if (!isObject(json)) throw new DecodeError("a turn request is an object");
  const turn = smallNumber(json.turn, "a turn request's turn");
  if (typeof json.handler !== "string") throw new DecodeError("a turn request names its handler");
  const handler = handlerNamed(json.handler);
  if (handler === null) throw new DecodeError(`no handler '${json.handler}'`);
  if (json.scope !== undefined && typeof json.scope !== "string") throw new DecodeError("a turn request's scope is text, the key its session was spawned under");
  const scope = json.scope === undefined ? ROOT_SCOPE : json.scope;
  return { turn, handler: json.handler, arguments: argumentsFromJson(handler, json.arguments), scope };
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
  entries.push(["job", request.job === true]);
  const by = request.interrupts ?? NO_INTERRUPTS;
  if (anyInterrupt(by)) entries.push([INTERRUPTED_BY, interruptNames(by)]);
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
  if (request.settings !== null) entries.push([MODEL_SETTINGS, request.settings]);
  const by = request.interrupts ?? NO_INTERRUPTS;
  if (anyInterrupt(by)) entries.push([INTERRUPTED_BY, interruptNames(by)]);
  if (PROGRAM.blobs !== undefined) entries.push([MODEL_BLOBS, request.blobs.json()]);
  return object(entries);
}

function settingsOf(args) {
  if (PROGRAM.model === null || PROGRAM.model.settings.length === 0) return null;
  assert(args.length === 1 + PROGRAM.model.settings.length, "a model site passes the context and every setting");
  const entries = PROGRAM.model.settings.map((setting, index) => [setting.name, toJson(setting.type, args[index + 1])]);
  entries.sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
  return object(entries);
}

function settingsText(settings) {
  return settings === null ? "no settings" : `settings ${encodeJson(settings)}`;
}

function modeText(mode) {
  return mode === null ? "no mode" : `mode ${mode}`;
}

class HostError extends Error {}

function defaultHost(config) {
  const post = async (url, json, signal) => {
    const response = await config.fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: encodeJson(json), signal });
    const body = await response.text();
    if (!response.ok) throw new HostError(`${url} answered ${response.status}: ${body}`);
    return decodeJson(body);
  };
  const until = (untilMs, signal) => new Promise((resolve) => {
    const left = Number(untilMs) - Number(config.now());
    if (left <= 0 || signal?.aborted) {
      resolve();
      return;
    }
    const timer = setTimeout(resolve, left);
    signal?.addEventListener("abort", () => {
      clearTimeout(timer);
      resolve();
    }, { once: true });
  });
  return {
    async tool(request) {
      try {
        return outputFrom(await post(config.toolsUrl, toolRequestJson(request), request.signal));
      } catch (error) {
        if (request.signal?.aborted) return { tag: "failed", error: INTERRUPTED_REASON };
        throw error;
      }
    },
    async listing(request) {
      return outputFrom(await post(config.listingsUrl, listingRequestJson(request)));
    },
    async model(request) {
      try {
        return await post(config.modelUrl, modelRequestJson(request), request.signal);
      } catch (error) {
        if (request.signal?.aborted) return interruptedResponse();
        throw error;
      }
    },
    async recover(request) {
      if (config.recoverUrl === undefined) return { kind: "unknown" };
      return recoveredFrom(await post(config.recoverUrl, toolRequestJson(request)));
    },
    async jobs(turn, open) {
      if (config.jobsUrl === undefined) return [];
      const lost = await post(config.jobsUrl, object([["open", open], ["turn", BigInt(turn)]]));
      if (!Array.isArray(lost) || lost.some((key) => typeof key !== "string")) throw new DecodeError("a jobs answer lists the lost job keys as text");
      return lost;
    },
    async hold(turn, seq, untilMs, signal) {
      await until(untilMs, signal);
      return BigInt(config.now());
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
  deliveryJson, deliveryFrom, outputFrom, outputJson, recoveredFrom, suspensionJson, suspensionFrom, resumeJson, resumeFrom,
  SIDE_TURN, answerSide, sideRequestFromJson, sideAnswerJson,
  idleDue,
};
