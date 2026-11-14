#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";

const ROUNDS_MAX = 64;
const HL = "target/release/hl";
const OUTPUT_BYTES_MAX = 64 * 1024 * 1024;

const separator = process.argv.indexOf("--");
const { values } = parseArgs({
  args: process.argv.slice(2, separator < 0 ? undefined : separator),
  options: { file: { type: "string" }, out: { type: "string" }, match: { type: "string" } },
});
const command = separator < 0 ? [] : process.argv.slice(separator + 1);
if (!values.file || !values.out || !values.match || command.length === 0) {
  console.error("usage: node scripts/fuzz-shrink.mjs --file IN.hl --out OUT.hl --match REGEX -- COMMAND [ARG]...");
  process.exit(2);
}
const pattern = new RegExp(values.match);
const scratch = values.out + ".candidate.hl";
let tries = 0;

function checks(path) {
  return spawnSync(HL, ["emit", "--target", "native-jsonl", path], { maxBuffer: OUTPUT_BYTES_MAX }).status === 0;
}

function interesting(lines) {
  tries += 1;
  writeFileSync(scratch, lines.join("\n"));
  if (!checks(scratch)) return false;
  const run = spawnSync(command[0], [...command.slice(1), scratch], { encoding: "utf8", maxBuffer: OUTPUT_BYTES_MAX });
  return run.status !== 0 && pattern.test(run.stdout + run.stderr);
}

function blockEnd(lines, start) {
  if (!lines[start].trimEnd().endsWith("{")) return start;
  let depth = 0;
  for (let index = start; index < lines.length; index++) {
    for (const character of lines[index]) {
      if (character === "{") depth += 1;
      if (character === "}") depth -= 1;
    }
    if (depth === 0 && !lines[index].trimEnd().endsWith("{")) return index;
  }
  return lines.length - 1;
}

function removals(lines) {
  const out = [];
  for (let start = 0; start < lines.length; start++) {
    const end = blockEnd(lines, start);
    out.push([...lines.slice(0, start), ...lines.slice(end + 1)]);
    if (end > start + 1) {
      const body = lines.slice(start + 1, end).filter((line) => !/^\s*\} else/.test(line));
      out.push([...lines.slice(0, start), ...body, ...lines.slice(end + 1)]);
    }
  }
  return out;
}

function edits(lines) {
  const out = [];
  lines.forEach((line, index) => {
    const variants = line.match(/^type (\S+) = (.*)$/);
    if (variants) {
      const parts = variants[2].split(" | ");
      parts.forEach((_, drop) => {
        if (parts.length > 1) out.push([...lines.slice(0, index), "type " + variants[1] + " = " + parts.filter((_, at) => at !== drop).join(" | "), ...lines.slice(index + 1)]);
      });
    }
    const literal = line.replace(/"(?:[^"\\]|\\.)*"/, '""');
    if (literal !== line) out.push([...lines.slice(0, index), literal, ...lines.slice(index + 1)]);
  });
  return out;
}

let lines = readFileSync(values.file, "utf8").split("\n");
if (!interesting(lines)) {
  console.error("fuzz-shrink: the input is not interesting: the command does not fail matching " + values.match);
  process.exit(1);
}
for (let round = 0; round < ROUNDS_MAX; round++) {
  let progressed = false;
  for (const generate of [removals, edits]) {
    let index = 0;
    for (;;) {
      const candidates = generate(lines);
      if (index >= candidates.length) break;
      if (interesting(candidates[index])) {
        lines = candidates[index];
        progressed = true;
      } else {
        index += 1;
      }
    }
  }
  if (!progressed) break;
}
writeFileSync(values.out, lines.join("\n") + (lines.at(-1) === "" ? "" : "\n"));
console.log("fuzz-shrink: " + values.out + ": " + lines.filter((line) => line.trim() !== "").length + " lines after " + tries + " tries");
