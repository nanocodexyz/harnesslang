import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

const [directory, repeatsText] = process.argv.slice(2);
const repeats = Number(repeatsText);
assert.ok(directory && Number.isInteger(repeats) && repeats >= 3, "usage: node selfhost-func-execute.mjs OUT REPEATS>=3");
const fixtures = [
  { name: "wrapped-i32", args: [], expected: -1 },
  { name: "wrapped-i64", args: [], expected: -1n },
  { name: "runtime-wrapped-add", args: [], expected: -2147483648 },
  { name: "runtime-param-add", args: [19, 23], expected: 42 },
  { name: "runtime-div-zero", args: [], trap: true },
  { name: "runtime-div-overflow", args: [], trap: true },
];
const results = [];
for (let repeat = 0; repeat < repeats; repeat += 1) {
  for (const fixture of fixtures) {
    const base = path.join(directory, "runs", String(repeat), fixture.name);
    const oracle = fs.readFileSync(path.join(base, "rust.wasm"));
    for (const mode of ["interpreter", "compiled"]) {
      const filename = mode === "interpreter" ? path.join(base, "interpreter.wasm") : path.join(base, "wasm", "result.out");
      const bytes = fs.readFileSync(filename);
      assert.deepEqual(bytes, oracle, `${fixture.name}: executed bytes differ from unchanged Rust oracle`);
      assert.equal(WebAssembly.validate(bytes), true, `${fixture.name}: emitted module invalid`);
      const module = new WebAssembly.Module(bytes);
      assert.deepEqual(WebAssembly.Module.imports(module), [], "fixture must have no host imports");
      const instance = new WebAssembly.Instance(module, {});
      assert.equal(typeof instance.exports.f, "function");
      if (fixture.trap) {
        assert.throws(() => instance.exports.f(...fixture.args), WebAssembly.RuntimeError, fixture.name);
      } else {
        assert.equal(instance.exports.f(...fixture.args), fixture.expected, fixture.name);
      }
      results.push({ repeat: repeat + 1, fixture: fixture.name, mode, outcome: fixture.trap ? "WebAssembly.RuntimeError" : String(fixture.expected), direct_oracle_byte_equality: true });
    }
  }
}
fs.writeFileSync(path.join(directory, "emitted-execution.json"), JSON.stringify(results, null, 2) + "\n");
process.stdout.write(`passed ${results.length} emitted module executions, signed wrapping, params and both division traps\n`);
