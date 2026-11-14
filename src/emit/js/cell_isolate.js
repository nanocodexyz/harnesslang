import { WorkerEntrypoint } from "cloudflare:workers";
import cell from "./script.js";

export default class CellIsolate extends WorkerEntrypoint {
  async run(contextText, step) {
    const context = cellContextFrom(decodeJson(contextText));
    const engine = new ScriptEngine(cell, {
      limits: context.limits,
      replay: context.replay,
      platform: platformCellValues(),
      macrotask: () => new Promise((resolve) => setTimeout(resolve, 0)),
    });
    let next = await engine.start();
    for (let index = 0; index < context.stepsMax; index++) {
      const answer = await step(encodeJson(engineStepJson(next)));
      if (next.kind === "done" || next.kind === "threw") return;
      next = await engine.resume(engineInputFrom(decodeJson(answer)));
    }
    await step(encodeJson(engineStepJson({ kind: "threw", error: `the cell took more than its ${context.stepsMax} engine steps` })));
  }
}
