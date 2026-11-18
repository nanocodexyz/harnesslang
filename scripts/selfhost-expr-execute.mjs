import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

const [directory] = process.argv.slice(2);
assert.ok(directory, "usage: node selfhost-expr-execute.mjs OUT");
const inputs = JSON.parse(fs.readFileSync(path.join(directory, "execution-input.json"), "utf8"));
const expectations = JSON.parse(fs.readFileSync(path.join(directory, "execution-expectations.json"), "utf8"));
const decode = (value, type) => type === "void" ? undefined : type === "i64" ? BigInt(value) : Number(value);
const results = [];
for (const input of inputs) {
  const bytes = fs.readFileSync(input.module);
  assert.equal(WebAssembly.validate(bytes), true, input.fixture);
  const module = new WebAssembly.Module(bytes);
  assert.deepEqual(WebAssembly.Module.imports(module), [], "complete fixture must require no host imports");
  const instance = new WebAssembly.Instance(module, {});
  const expected = expectations[input.fixture];
  assert.ok(expected, input.fixture);
  const args = expected.args.map(value => decode(value, expected.arg_type));
  assert.equal(instance.exports.f(...args), decode(expected.value, expected.result_type), input.fixture);
  results.push({ ...input, result: String(expected.value), complete_fixture_valid: true });
}
assert.ok(results.length >= 3 * Object.keys(expectations).length * 2);
assert.deepEqual([...new Set(results.map(row => row.fixture))].sort(), Object.keys(expectations).sort());
fs.writeFileSync(path.join(directory, "execution-results.json"), JSON.stringify(results, null, 2) + "\n");
process.stdout.write(`passed ${results.length} complete-fixture executions with exact instruction-body parity\n`);
