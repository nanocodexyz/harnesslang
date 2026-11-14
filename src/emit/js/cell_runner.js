const RANDOM_BYTES_PER_DRAW = 8;
const RANDOM_FRACTION_SHIFT = 32n;
const CELL_POINT_BEFORE_HEADER = { point: "cell_before_header" };
const CELL_POINT_AFTER_HEADER = { point: "cell_after_header" };
const CELL_POINT_ENTRY = { point: "cell_entry" };
const CELL_POINT_WAITING = { point: "cell_waiting" };
const CELL_POINT_BEFORE_FINISHED = { point: "cell_before_finished" };
const CELL_POINT_AFTER_FINISHED = { point: "cell_after_finished" };
const BYTE_MASK = 0xffn;
const BITS_PER_BYTE_BIG = 8n;

class CellInterrupt {
  constructor(kind, text) {
    this.kind = kind;
    this.text = text;
  }
}

function nestedPoint(name, effect) {
  return { point: name, effect };
}

function cellStepsMax(code) {
  return 2 * (Number(code.journalBytes.value / ENTRY_BYTES) + 1) + 2;
}

function encodeNestedOutput(output) {
  if (output.tag === "ok") return [encodeJson(output.value), true];
  return [encodeJson(object([["error", output.error]])), false];
}

function cellRefusal(error) {
  return [encodeJson(object([["code", CALL_LIMIT], ["error", error], ["isError", true], ["retryable", false]])), false];
}

function cellOutcomeOutput(outcome) {
  if (outcome.kind === "done") return { tag: "ok", value: outcome.text };
  if (outcome.kind === "threw") return { tag: "failed", error: outcome.text };
  return { tag: "ok", value: object([[UNKNOWN_KEY, outcome.text]]) };
}

function cellOutcomeFrom(output) {
  if (output.tag === "failed") return { kind: "threw", text: output.error };
  if (typeof output.value === "string") return { kind: "done", text: output.value };
  if (isObject(output.value) && typeof output.value[UNKNOWN_KEY] === "string") return { kind: "unknown", text: output.value[UNKNOWN_KEY] };
  throw new DecodeError(`cell output ${encodeJson(output.value)} is neither text nor unknown`);
}

function headerTools(code) {
  return code.tools.map((index) => {
    const tool = PROGRAM.tools[index];
    return object([["input", object(tool.params.map((param) => [param.name, typeName(param.ty)]))], ["name", tool.name]]);
  });
}

async function runCell(site, host) {
  if (site.cell.kind === "existing") assert(!site.cell.log.finished, "the engine never starts on a finished cell");
  const runner = new CellRunner(site, host);
  let outcome;
  try {
    outcome = await runner.drive();
  } catch (error) {
    if (!(error instanceof CellInterrupt)) throw error;
    outcome = await runner.interrupted(error);
  }
  return runner.finish(outcome);
}

class CellRunner {
  constructor(site, host) {
    this.site = site;
    this.host = host;
    this.code = PROGRAM.codes[site.code];
    this.origin = callId(site.turn, site.seq);
    const ignores = site.mutation === "cell_replay_ignores_journal";
    if (site.cell.kind === "fresh") {
      this.cell = site.cell.id;
      this.replay = [];
      this.entries = 0;
      this.nextSeq = 0;
      this.header = { originCallId: this.origin, script: site.script, tools: headerTools(this.code), yieldTimeMs: this.code.yieldMs.value };
      this.bytes = 0n;
      this.script = site.script;
    } else {
      const log = site.cell.log;
      this.cell = log.cell;
      this.replay = ignores ? [] : log.entries.slice();
      this.entries = log.entries.length;
      this.nextSeq = log.nextSeq;
      this.header = null;
      this.bytes = cellLogBytes(log);
      this.script = log.header.script;
    }
    this.consult = site.cell.kind === "existing" && !ignores;
    this.live = this.replay.length === 0;
    this.cursor = 0;
    this.freshEntries = [];
    this.changedCalls = new Map();
    this.headerUnsaved = false;
    this.calls = new Map();
    this.queue = [];
    this.issued = 0n;
    this.timers = new Map();
    this.armed = 0n;
    this.idShift = 0;
  }

  unchecked() {
    return this.site.mutation === "cell_limit_unchecked";
  }

  diverged(reason) {
    return new CellInterrupt("diverged", reason);
  }

  async interrupted(error) {
    if (error.kind === "threw") return { kind: "threw", text: error.text };
    if (error.kind === "diverged") {
      await this.host.observe({ kind: "diverged", cell: this.cell, reason: error.text });
      return { kind: "unknown", text: divergenceText(this.cell, error.text) };
    }
    assert(error.kind === "overflow", "a cell is interrupted by a throw, a divergence or its journal bound");
    const bound = this.code.journalBytes;
    return { kind: "unknown", text: divergenceText(this.cell, `its journal passed journal_bytes (${bound.name} = ${bound.value})`) };
  }

  async drive() {
    if (this.header !== null) {
      await this.host.hook(CELL_POINT_BEFORE_HEADER);
      this.emit(false);
      this.headerUnsaved = true;
      await this.host.hook(CELL_POINT_AFTER_HEADER);
    }
    if (this.site.script !== this.script) throw this.diverged("its header holds another script than its call");
    let step = await this.host.start(this.cell, this.script);
    const stepsMax = cellStepsMax(this.code);
    for (let index = 0; index < stepsMax; index++) {
      let input;
      switch (step.kind) {
        case "done":
          return this.ended({ kind: "done", text: step.output });
        case "threw":
          return this.ended({ kind: "threw", text: step.error });
        case "call":
          await this.call(step.id, step.name, step.input);
          input = { kind: "issued" };
          break;
        case "timeout":
          input = await this.timeout(step.id, step.delayMs);
          break;
        case "clock":
        case "monotonic":
        case "random": {
          const entry = await this.record({ kind: step.kind, value: step.value ?? null });
          assert(entry.kind === step.kind, "a value step records its own kind of entry");
          input = { kind: step.kind, value: entry.value };
          break;
        }
        case "bytes":
          input = await this.randomBytes(step.length, step.data ?? null);
          break;
        case "idle":
          input = await this.input();
          break;
        default:
          throw new Panic(`unknown engine step ${step.kind}`);
      }
      step = await this.host.resume(this.cell, input);
    }
    throw new CellInterrupt("overflow", null);
  }

  ended(outcome) {
    const entry = this.replay[this.cursor];
    if (entry !== undefined) throw this.diverged(`the script ended where its journal holds ${describeEntry(entry)}`);
    return outcome;
  }

  async randomBytes(length, provided) {
    if (length > this.code.journalBytes.value) throw new CellInterrupt("overflow", null);
    const entry = await this.record({ kind: "bytes", data: provided ?? new Array(Number(length)).fill(0), provided: provided !== null });
    assert(entry.kind === "bytes", "a bytes step records a bytes entry");
    return { kind: "bytes", data: entry.data };
  }

  async fill(step) {
    switch (step.kind) {
      case "timeout":
        return { kind: "timeout", id: step.id, delayMs: step.delayMs, atMs: await this.host.clock() };
      case "clock":
      case "monotonic":
        return { kind: step.kind, value: step.value ?? (await this.host.clock()) };
      case "random": {
        if (step.value !== null) return { kind: "random", value: step.value };
        return { kind: "random", value: (await this.host.random()) >> RANDOM_FRACTION_SHIFT };
      }
      case "bytes": {
        if (step.provided) return { kind: "bytes", data: step.data };
        const filled = [];
        while (filled.length < step.data.length) {
          let word = await this.host.random();
          for (let index = 0; index < RANDOM_BYTES_PER_DRAW; index++) {
            filled.push(Number(word & BYTE_MASK));
            word >>= BITS_PER_BYTE_BIG;
          }
        }
        assert(filled.length <= step.data.length + RANDOM_BYTES_PER_DRAW, "random bytes are drawn a word at a time");
        return { kind: "bytes", data: filled.slice(0, step.data.length) };
      }
      default:
        return step;
    }
  }

  async record(step) {
    const recorded = this.replay[this.cursor];
    if (recorded !== undefined) {
      if (entryIsInput(recorded) || !sameStep(recorded, step)) {
        throw this.diverged(`the script took ${describeEntry(step)} where its journal holds ${describeEntry(recorded)}`);
      }
      this.cursor += 1;
      await this.liveAtEnd();
      return recorded;
    }
    await this.goLive();
    const entry = await this.fill(step);
    await this.appendEntry(entry);
    return entry;
  }

  async liveAtEnd() {
    if (this.cursor === this.replay.length) await this.goLive();
  }

  async appendEntry(entry) {
    assert(this.live, "only a live cell appends entries");
    this.bytes += entryBytes(entry);
    if (this.bytes > this.code.journalBytes.value && !this.unchecked()) throw new CellInterrupt("overflow", null);
    this.freshEntries.push(entry);
    this.entries += 1;
    await this.host.hook(CELL_POINT_ENTRY);
  }

  emit(finished) {
    const header = this.header;
    this.header = null;
    if (this.freshEntries.length === 0 && this.changedCalls.size === 0 && header === null && !finished) return;
    const entries = this.freshEntries;
    this.freshEntries = [];
    const calls = [...this.changedCalls.entries()].sort((left, right) => left[0] - right[0]).map((pair) => pair[1]);
    this.changedCalls = new Map();
    const fresh = header !== null;
    assert(fresh === (this.nextSeq === 0), "the header rides the first update only");
    const update = {
      sessionId: this.site.session,
      cellId: this.cell,
      seq: this.nextSeq,
      header,
      entriesFrom: this.entries - entries.length,
      entries,
      calls,
      finished,
    };
    this.nextSeq += 1;
    this.host.append({ kind: "cell", cell: this.cell, fresh, session: this.site.session, update });
  }

  journaled(id) {
    const call = this.calls.get(id);
    let state;
    if (call.phase === "completed") {
      assert(call.output !== null, "a completed call has its output");
      state = { kind: "completed", encoded: call.output[0], success: call.output[1] };
    } else {
      state = { kind: call.phase };
    }
    return { id, callId: nestedId(this.site.turn, this.site.seq, id), name: call.name, input: call.input, state };
  }

  changed(id) {
    if (this.live) this.changedCalls.set(id, this.journaled(id));
  }

  running() {
    let count = 0n;
    for (const call of this.calls.values()) if (call.phase === "running") count += 1n;
    return count;
  }

  async call(id, name, input) {
    if (this.calls.has(id)) throw this.diverged(`the engine issued nested call ${id} twice`);
    this.issued += 1n;
    const index = this.code.tools.find((candidate) => PROGRAM.tools[candidate].name === name);
    const tool = index === undefined ? null : { index, effect: PROGRAM.tools[index].effect };
    let refused = null;
    if (tool === null) refused = `'${name}' is not a tool of code '${this.code.name}'`;
    else if (this.issued > this.code.calls.value && !this.unchecked()) {
      refused = `the cell made its calls bound (${this.code.calls.name} = ${this.code.calls.value}) of nested calls`;
    }
    this.calls.set(id, { name, tool, input, phase: "queued", output: null, refused });
    if (this.running() < this.code.parallel.value || this.unchecked()) {
      await this.startCall(id);
      return;
    }
    this.queue.push(id);
    this.changed(id);
  }

  async startCall(id) {
    const call = this.calls.get(id);
    try {
      await this.record({ kind: "call", id, name: call.name, input: call.input });
    } catch (error) {
      if (error instanceof CellInterrupt && error.kind === "diverged" && this.site.mutation === "divergence_dispatches" && call.refused === null) {
        call.phase = "running";
        await this.runTool(id);
      }
      throw error;
    }
    call.phase = "running";
    this.changed(id);
    if (this.live) await this.settle(id);
  }

  recordId(id) {
    return id - Math.min(this.idShift, id - 1);
  }

  async goLive() {
    if (this.live) return;
    assert(this.cursor === this.replay.length, "a cell goes live at its journal's end");
    this.live = true;
    if (this.site.mutation === "nested_ids_restart") {
      let last = 0;
      for (const id of this.calls.keys()) last = Math.max(last, id);
      this.idShift = last;
    }
    const open = [...this.calls.entries()].filter(([, call]) => call.phase === "running" && call.output === null).map(([id]) => id).sort((left, right) => left - right);
    for (const id of open) await this.settle(id);
  }

  setOutput(id, output) {
    const call = this.calls.get(id);
    assert(call.output === null, "a nested call settles once");
    call.output = output;
  }

  unknown(id) {
    this.setOutput(id, [encodeJson(unknownOutcome()), false]);
  }

  async settle(id) {
    const call = this.calls.get(id);
    if (call.refused !== null) {
      this.setOutput(id, cellRefusal(call.refused));
      return;
    }
    assert(call.tool !== null, "a call that is not refused names a nested tool");
    const logged = this.consult ? this.site.nested.get(nestedKey(this.site.seq, this.recordId(id))) : undefined;
    if (logged === undefined) {
      if (call.tool.effect === "external") await this.dispatch(id, true);
      return;
    }
    switch (logged.kind) {
      case "done":
        this.setOutput(id, encodeNestedOutput(logged.output));
        return;
      case "unknown":
        this.unknown(id);
        return;
      case "open":
        await this.reopen(id);
        return;
      default:
        throw new Panic("a nested call never holds a model reply");
    }
  }

  inStep() {
    return this.site.target.recovery === "in_step";
  }

  async reopen(id) {
    const rid = this.recordId(id);
    if (this.inStep()) {
      const attempt = await this.host.attempt(this.site.seq, rid);
      if (attempt > 1 && this.site.mutation !== "vercel_nested_retry_redispatches") {
        await this.host.recovered(this.site.seq, rid);
        this.headerUnsaved = false;
        this.unknown(id);
        return;
      }
      await this.runDispatch(id, "external", true, false);
      return;
    }
    assert(this.site.mutation === "cell_recovery_redispatches_external" || this.site.mutation === "recovery_skipped", "recovery turns every open nested external call unknown before the cell runs again");
    await this.runDispatch(id, "external", false, false);
  }

  async dispatch(id, fresh) {
    const call = this.calls.get(id);
    const effect = call.tool.effect;
    if (fresh && effect === "external") {
      this.host.append({ kind: "nested_call", turn: this.site.turn, seq: this.site.seq, nested: this.recordId(id), tool: call.name, effect, input: call.input });
    }
    const step = this.inStep() && effect === "external";
    await this.runDispatch(id, effect, step, step && fresh);
  }

  async runDispatch(id, effect, step, first) {
    this.emit(false);
    const gate = this.site.target.gateBeforeOutbound;
    const required = effect === "external";
    let late = false;
    if (this.site.mutation === "nested_flush_after_dispatch") late = required && !gate;
    if (this.site.mutation === "cell_gate_releases_early") late = gate;
    if (required) await this.host.armResume();
    if ((required || gate) && !late) {
      if (this.site.mutation === "cell_header_rides_past_external" && required) {
        await this.host.flushWithholding(this.cell);
      } else {
        await this.host.flush();
        this.headerUnsaved = false;
      }
    }
    if (first) {
      const attempt = await this.host.attempt(this.site.seq, this.recordId(id));
      assert(attempt === 1 || this.site.mutation === "cell_replay_ignores_journal", "a fresh nested external step starts at its first attempt");
    }
    const output = await this.runTool(id);
    if (required) this.host.append({ kind: "nested_result", turn: this.site.turn, seq: this.site.seq, nested: this.recordId(id), output });
    if (step || late) {
      await this.host.flush();
      this.headerUnsaved = false;
    }
    this.setOutput(id, encodeNestedOutput(output));
  }

  async runTool(id) {
    const call = this.calls.get(id);
    assert(call.tool !== null, "a dispatched call names a nested tool");
    const tool = PROGRAM.tools[call.tool.index];
    const effect = call.tool.effect;
    await this.host.hook(nestedPoint("cell_before_nested", effect));
    const output = await this.host.tool({
      turn: this.site.turn,
      seq: this.site.seq,
      tool: call.name,
      effect,
      input: call.input,
      returns: tool.returns,
      nested: { id, cell: this.cell, code: this.site.code },
    });
    await this.host.hook(nestedPoint("cell_after_nested", effect));
    return output;
  }

  async timeout(id, delayMs) {
    if (!this.unchecked()) {
      if (this.armed >= this.code.timers.value) {
        return { kind: "refused", error: `setTimeout: the cell armed its timers bound (${this.code.timers.name} = ${this.code.timers.value})` };
      }
      if (delayMs > this.code.timerMs.value) {
        return { kind: "refused", error: `setTimeout: ${delayMs} ms is past timer_ms (${this.code.timerMs.name} = ${this.code.timerMs.value})` };
      }
    }
    if (this.timers.has(id)) throw this.diverged(`the engine armed timer ${id} twice`);
    const entry = await this.record({ kind: "timeout", id, delayMs, atMs: 0n });
    assert(entry.kind === "timeout", "a timeout step records a timeout entry");
    this.timers.set(id, [delayMs, entry.atMs]);
    this.armed += 1n;
    this.emit(false);
    return { kind: "issued" };
  }

  async input() {
    const entry = this.replay[this.cursor];
    if (entry !== undefined) return this.replayedInput(entry);
    await this.goLive();
    const ready = [...this.calls.entries()].filter(([, call]) => call.phase === "running").map(([id]) => id).sort((left, right) => left - right)[0];
    if (ready !== undefined) return this.deliver(ready);
    let earliest = null;
    for (const [id, [delay, at]] of this.timers) {
      const until = at + delay > U64_MAX ? U64_MAX : at + delay;
      if (earliest === null || until < earliest[0] || (until === earliest[0] && id < earliest[1])) earliest = [until, id];
    }
    if (earliest === null) throw new CellInterrupt("threw", "the cell waits with no nested call running and no timer armed");
    return this.fire(earliest[1], earliest[0]);
  }

  async replayedInput(entry) {
    if (entry.kind === "result") {
      const call = this.calls.get(entry.id);
      if (call === undefined || call.phase !== "running") {
        throw this.diverged(`its journal delivers nested call ${entry.id}, which the script has not started`);
      }
      this.cursor += 1;
      await this.liveAtEnd();
      call.output = [entry.encoded, entry.success];
      await this.complete(entry.id);
      return { kind: "result", id: entry.id, encoded: entry.encoded, success: entry.success };
    }
    if (entry.kind === "timer") {
      if (!this.timers.has(entry.id)) throw this.diverged(`its journal fires timer ${entry.id}, which the script has not armed`);
      this.timers.delete(entry.id);
      this.cursor += 1;
      await this.liveAtEnd();
      return { kind: "timer", id: entry.id };
    }
    throw this.diverged(`the script waits where its journal holds ${describeEntry(entry)}`);
  }

  async complete(id) {
    this.calls.get(id).phase = "completed";
    this.changed(id);
    await this.host.observe({ kind: "delivered", cell: this.cell, id });
    while (this.running() < this.code.parallel.value && this.queue.length > 0) {
      await this.startCall(this.queue.shift());
    }
  }

  async deliver(id) {
    if (this.calls.get(id).output === null) await this.dispatch(id, false);
    const output = this.calls.get(id).output;
    assert(output !== null, "a delivered call has its output");
    const [encoded, success] = output;
    await this.appendEntry({ kind: "result", id, encoded, success });
    await this.complete(id);
    this.emit(false);
    return { kind: "result", id, encoded, success };
  }

  async fire(id, untilMs) {
    const [delayMs] = this.timers.get(id);
    const c2 = this.headerUnsaved && this.site.mutation !== "timeout_rides";
    await this.host.armResume();
    if (c2 || this.site.target.waitFlushes) {
      this.emit(false);
      await this.host.flush();
      this.headerUnsaved = false;
    }
    await this.host.waitTimer({ cell: this.cell, code: this.site.code, timer: id, delayMs, untilMs });
    this.timers.delete(id);
    await this.appendEntry({ kind: "timer", id });
    this.emit(false);
    return { kind: "timer", id };
  }

  async finish(outcome) {
    await this.host.hook(CELL_POINT_BEFORE_FINISHED);
    this.emit(true);
    await this.host.hook(CELL_POINT_AFTER_FINISHED);
    await this.host.observe({ kind: "finished", cell: this.cell, outcome });
    return outcome;
  }
}

