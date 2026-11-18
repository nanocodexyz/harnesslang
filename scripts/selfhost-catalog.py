import argparse
import ast
import hashlib
import importlib.util
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
RUNS_MIN = 3
WORD_BYTES = 4
SOURCE_BYTES_MAX = 4194304
ENCODER_WAT_BYTES = 683936
ENCODER_WAT_SHA256 = "148575c5ff0a8c7132f6642cd3dc1a1be6664ad54e421d2a02928689010e7425"
EXPECTED_COUNTS = {"same-signature": [1, 1, 2, 3], "import-first-order": [2, 1, 2, 3], "result-order": [2, 0, 2, 2], "encoder-original-catalog": [75, 11, 826, 837]}
ORACLE_HASHES = {
    "assemble": "e9d433c6564d214f603c8b3aec2fc91b6a394506b07e4c800519a03c43f895b1",
    "shake": "16e1ba0e073e7823ce36cb705aec72a64b1497145a6fb8b4d66b046a199965f5",
    "encode": "024a299ce18c6250f304b25b3643ba54cb558e7fbbd958226e26589777b99f27",
    "flat": "c0d0da7c54e42fb9c1dd301574c053e66ee393af2377feab07ab1aa4ec9e8097",
}
CONTRACT = {
    "version": 1,
    "endian": "u32 little endian; blob is u32 byte length then bytes; codes is blob of Wasm value type bytes",
    "sections": ["types: count then (codes(params),codes(results))*", "imports: count then (blob(module),blob(field),u32(typeID))*", "functions: count then function_record*", "names: count then (blob(symbol),u32(index))*"],
    "envelope": "u32(version), blob(types), blob(imports), blob(functions), blob(names)",
    "function_record": "blob(symbol),u32(function_index),u32(typeID),u32(body_start),u32(body_end),codes(params),codes(locals),u32(named_count),(blob(name),u32(local_index))*,u32(export_present),blob(export_name)",
    "spans": "byte offsets in canonical existing lexer tokens; body_start includes first body token, body_end excludes outer close token; empty body has equal offsets",
    "order": "imports first in source order then definitions source order; signature IDs first-seen on that order, exact param/result code vectors; locals not part of signature",
    "names": "function symbol map includes imports then definitions; duplicate import symbols and duplicate local names use last match, like Rust; duplicate defined function symbols rejected; params before locals even if declarations interleaved",
    "exports": "inline function export flag+name, Rust header last export wins; only ASCII import module/field and final effective export names supported, including control bytes; symbols remain UTF8 bytes",
    "helpers": "all arena parameters/results use existing bytes<tokens_bytes_max>, with explicit catalog_bytes_max=flat_bytes_max policy checks before growth; small u32 writer returns bytes<catalog_word_bytes>. Helpers consume only canonical/validated constructed buffers. catalog_tokens(tokens)->text ok HEX or typed-prefix text; catalog_source(text)->text; catalog_index(valid_wire,name)->u64 or 4294967296; catalog_local_index(valid_function_record,name)->same; catalog_map(raw_entries,name)->same, last-match",
    "bounds": {"source_bytes_max": SOURCE_BYTES_MAX, "tokens_bytes_max": 10 * SOURCE_BYTES_MAX, "catalog_bytes_max": SOURCE_BYTES_MAX, "items_max": 65536, "locals_max": 65536, "nesting_max": 4096, "header_fixed_bytes": 40, "envelope_fixed_bytes": 36, "signature_fixed_bytes": 8},
    "scope": "PRESELECTED raw forms only. Body tokens opaque, not validated or encoded. Recognized static forms delegated unchanged to static layout. No shake, reachability, full flat/module combiner, stack validation, stage1, or stage2. Do not compare this projection with shaken whole-module bytes.",
}
MUTANTS = {
    "wrong-type": ('if catalog_is(value, "i64") { return catalog_i64 }', 'if catalog_is(value, "i64") { return catalog_i32 }', "same-signature"),
    "dedup": ('if catalog_equal(slice(signature_entries, at, next), signature) { found = index }', 'if false { found = index }', "same-signature"),
    "type-order": ('type_id = type_count\n', 'type_id = type_count + 1\n', "import-first-order"),
    "import-offset": ('let index = import_count + function_count', 'let index = function_count', "same-signature"),
}


def identity(path):
    raw = path.read_bytes()
    return {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}


def fixtures():
    accepted = {
        "empty": "",
        "same-signature": '(import "env" "sink" (func $sink (param i64) (result i64)))\n(func $a (export "a") (param $x i64) (result i64) (local $tmp i32) local.get $x call $b)\n(func $b (export "b") (param $y i64) (result i64) (local $tmp i64) local.get $y call $sink)',
        "import-first-order": '(func $a (export "a") (param i32) (result i64))\n(import "env" "last" (func $z (param i64) (result i32)))\n(func $b (export "b") (param i64) (result i32))',
        "result-order": '(func $f (export "f") (result i64 i32) (param i32 i64))\n(func $g (export "g") (param i32 i64) (result i32 i64))',
        "signature-names-ignored": '(func $a (export "a") (param $x i64))\n(func $b (export "b") (param $y i64) (local $z i32))',
        "interleaved-headers": '(func $f (local $last i64) (param $first i32) (result $ignored i64) (param i32) (local i32 i64) (export "first") (export "last") local.get $last)',
        "duplicate-local-last-wins": '(func $f (param $x i32) (local $x i64) (export "f") local.get $x)',
        "duplicate-import-last-wins": '(import "a" "b" (func $x (param i32)))(import "c" "d" (func $x (param i64)))(func $f (export "f") call $x)',
        "overwritten-nonascii-export": '(func $f (export "λ") (export "ok"))',
        "overwritten-nonascii-import-export": '(import "m" "n" (func $x (export "λ") (export "ok")))(func $f (export "f") call $x)',
        "empty-fields": '(func $f (param) (result) (local) (export ""))',
        "opaque-folded-body": '(func $f (param $x i64) (export "f") (block $b (result i64) (local.get $x)))',
        "static-forms": '(memory 2)(global $g i64 (i64.const 0))(text $s "s")(words $w 1)(func $f (export "f"))',
        "escaped-import-export": r'(import "a\00" "b\7f" (func $x))(func $f (export "a\n\t\00") call $x)',
    }
    rejected = {
        "duplicate-function": '(func $f (export "a"))(func $f (export "b"))',
        "function-import-collision": '(func $x)(import "m" "n" (func $x))',
        "unknown-type": '(func $f (param f32))',
        "unknown-local-type": '(func $f (local f64))',
        "named-two-types": '(func $f (param $x i32 i64))',
        "named-missing-type": '(func $f (local $x))',
        "nested-type": '(func $f (param (i32)))',
        "missing-name": '(func (param i32))',
        "missing-import": '(import "m")',
        "bad-import-signature": '(import "m" "n" (memory 1))',
        "export-atom": '(func $f (export x))',
        "unicode-export": '(func $f (export "λ"))',
        "final-nonascii-export": '(func $f (export "ok") (export "λ"))',
        "final-nonascii-import-export": '(import "m" "n" (func $x (export "ok") (export "λ")))',
        "unicode-import": '(import "λ" "n" (func $f))',
        "unclosed": '(func $f',
        "empty-form": '()',
        "unknown-form": '(module)',
        "atom": 'func',
        "locals-plus-params-bound": '(func $f (param i32) (local ' + ' '.join(['i64'] * 65536) + '))',
    }
    return [(name, source, True) for name, source in accepted.items()] + [(name, source, False) for name, source in rejected.items()]


def token_fixtures():
    def record(kind, payload=b"", line=1):
        return bytes([kind]) + line.to_bytes(WORD_BYTES, "little") + len(payload).to_bytes(WORD_BYTES, "little") + payload
    values = {
        "truncated": b"\x01",
        "kind": record(5),
        "zero-line": record(1, line=0),
        "payload-overrun": record(3, b"func")[:-1],
        "delimiter-payload": record(1, b"x"),
        "empty-atom": record(1) + record(3) + record(2),
        "wrong-close-line": record(1) + record(3, b"func") + record(2, line=2),
        "unclosed": record(1) + record(3, b"func"),
    }
    return [("token-" + name, raw.hex(), False) for name, raw in values.items()]


def combined(component):
    lexer = (ROOT / "compiler/assemble-lex.hl").read_text().split("\non tokenize(")[0]
    encoder = (ROOT / "compiler/encode.hl").read_text().split("\non encode(")[0]
    parts = ["harness catalog_projection"]
    for source in (lexer, encoder, component):
        parts.append("\n".join(line for line in source.splitlines() if not line.startswith("harness ") and not (source == encoder and line.startswith("tool "))))
    return "\n\n".join(parts) + "\n"


def encoder_fixture(out, source):
    if not source.is_absolute():
        raise RuntimeError("--encoder-wat requires an absolute caller-supplied path")
    with source.open("rb") as stream:
        raw = stream.read(ENCODER_WAT_BYTES + 1)
    if len(raw) != ENCODER_WAT_BYTES or hashlib.sha256(raw).hexdigest() != ENCODER_WAT_SHA256:
        raise RuntimeError("--encoder-wat does not match pinned recovered original WAT; do not re-bless")
    raw.decode("utf8")
    destination = out / "fixtures/encoder-original-catalog.wat"
    destination.write_bytes(raw)
    return "encoder-original-catalog", destination, True


def static(out, encoder_wat=None):
    for name, expected in ORACLE_HASHES.items():
        if identity(ROOT / f"src/emit/wasm/{name}.rs")["sha256"] != expected:
            raise RuntimeError(f"pinned Rust oracle changed: {name}; never re-bless to match port")
    for path in (ROOT / "scripts").glob("selfhost-catalog*.py"):
        ast.parse(path.read_text(), filename=str(path))
    encoder = (ROOT / "compiler/encode.hl").read_text()
    lexer = (ROOT / "compiler/assemble-lex.hl").read_text()
    numeric = dict((name, int(value)) for name, value in re.findall(r"const (\w+): u64 = (\d+)", encoder + lexer + (ROOT / "compiler/assemble-catalog.hl").read_text()))
    if numeric["catalog_bytes_max"] != numeric["flat_bytes_max"] or numeric["catalog_bytes_max"] != SOURCE_BYTES_MAX:
        raise RuntimeError("catalog output policy must derive from existing flat/source capacity")
    if numeric["tokens_bytes_max"] != 10 * SOURCE_BYTES_MAX or numeric["catalog_word_bytes"] != WORD_BYTES:
        raise RuntimeError("canonical token arena or u32 wire changed")
    if numeric["nesting_max"] * WORD_BYTES > numeric["catalog_bytes_max"]:
        raise RuntimeError("line stack capacity derivation failed")
    if 7 * numeric["tokens_bytes_max"] + 10 * WORD_BYTES >= 18446744073709551616:
        raise RuntimeError("maximum header arithmetic may overflow u64")
    if numeric["tokens_bytes_max"] >= 4294967296 or numeric["items_max"] != numeric["locals_max"]:
        raise RuntimeError("token spans/counts must fit u32; existing encoder limits must agree")
    component = (ROOT / "compiler/assemble-catalog.hl").read_text()
    header = component[component.index("fn catalog_header("):component.index("fn catalog_type_index(")]
    if header.count("catalog_ascii(export_name)") != 1 or header.index("catalog_ascii(export_name)") < header.index("if not body { at = catalog_next(tokens, field_end) }"):
        raise RuntimeError("final effective export policy must follow last-export-wins header loop")
    classes = {name: valid for name, _, valid in fixtures()}
    for name in ("overwritten-nonascii-export", "overwritten-nonascii-import-export"):
        if classes.get(name) is not True:
            raise RuntimeError("overwritten nonascii export regression fixture must be positive")
    for name in ("final-nonascii-export", "final-nonascii-import-export"):
        if classes.get(name) is not False:
            raise RuntimeError("final nonascii export regression fixture must reject")
    declarations = re.findall(r"^(?:fn|on|const) (\w+)", component, re.MULTILINE)
    if any(not name.startswith("catalog_") and name != "catalog" for name in declarations):
        raise RuntimeError("component declaration namespace escaped")
    if any(not re.fullmatch(r"[a-z_][a-z_0-9]*", bound) for bound in re.findall(r"\bloop \w+ below (\S+)", component)):
        raise RuntimeError("every catalog loop bound must be a named constant")
    program = combined(component)
    extended = program.split("\non catalog(")[0] + lookup_probe()
    functions = list(re.finditer(r"^fn (\w+)\(", extended, re.MULTILINE))
    positions = {match.group(1): match.start() for match in functions}
    component_bindings = set(re.findall(r"\b(?:let|var|loop) (\w+)", component))
    component_bindings.update(re.findall(r"\b(\w+)\s*:", "\n".join(line for line in component.splitlines() if line.startswith("fn "))))
    if component_bindings & set(positions):
        raise RuntimeError(f"catalog binding shadows composed fn: {sorted(component_bindings & set(positions))}")
    for index, match in enumerate(functions):
        end = functions[index + 1].start() if index + 1 < len(functions) else len(extended)
        body = extended[match.end():end].split("\non ")[0]
        calls = set(re.findall(r"\b(\w+)\(", body))
        if any(positions[name] >= match.start() for name in calls if name in positions):
            raise RuntimeError(f"pure fn has forward/self call: {match.group(1)}")
    names = re.findall(r"^(fn|on|const|tool) (\w+)", program, re.MULTILINE)
    if len(set(names)) != len(names):
        raise RuntimeError("generated program duplicate declarations")
    wrapper = (ROOT / "scripts/selfhost-catalog-oracle.rs").read_text()
    included = (ROOT / "src/emit/wasm/assemble.rs").read_text()
    reference_names = set(re.findall(r"^(?:pub )?(?:fn|const) (\w+)", included, re.MULTILINE))
    wrapper_names = set(re.findall(r"^    (?:pub )?(?:fn|const) (\w+)", wrapper, re.MULTILINE))
    if reference_names & wrapper_names:
        raise RuntimeError("oracle wrapper collides with included Rust declarations")
    if "mod assemble {" not in wrapper or "assemble::catalog_projection" not in wrapper:
        raise RuntimeError("oracle dependency module wiring changed")
    if "//" in component or "/*" in component or ";" in component or re.search(r"\b(?:assert|code)\b", component):
        raise RuntimeError("component must contain no comments, semicolons or reserved assert/code syntax")
    for name, (before, after, _) in MUTANTS.items():
        if component.count(before) != 1 or component.replace(before, after) == component:
            raise RuntimeError(f"mutant site invalid: {name}")
    if 10 * WORD_BYTES != 40 or (1 + 2 * 4) * WORD_BYTES != 36:
        raise RuntimeError("wire overhead derivation changed")
    (out / "catalog-combined.hl").write_text(program)
    (out / "catalog-lookup-probe-static.hl").write_text(program + lookup_probe())
    (out / "contract.json").write_text(json.dumps(CONTRACT, indent=2) + "\n")
    paths = []
    for name, source, valid in fixtures() + token_fixtures():
        suffix = ".hex" if name.startswith("token-") else ".wat"
        path = out / "fixtures" / f"{name}{suffix}"
        path.parent.mkdir(exist_ok=True)
        path.write_text(source)
        paths.append((name, path, valid))
    for name, (before, after, _) in MUTANTS.items():
        (out / f"mutant-{name}.hl").write_text(combined(component.replace(before, after)))
    if encoder_wat is not None:
        paths.append(encoder_fixture(out, encoder_wat))
    return paths


def lookup_probe():
    pure = """
fn catalog_probe_first_record(value: bytes<tokens_bytes_max>) -> bytes<tokens_bytes_max> {
  let imports_at = catalog_skip(value, word_bytes)
  let functions_at = catalog_skip(value, imports_at)
  return slice(value, functions_at + 2 * word_bytes, catalog_skip(value, functions_at))
}

fn catalog_lookup_result(input: text) -> text {
  let result = catalog_source(input)
  if before(result, " ") != "ok" { return result }
  let value: bytes<tokens_bytes_max> = bytes_of_hex(after(result, " "))
  if catalog_index(value, bytes_of("$sink")) != 0 { return "lookup import index" }
  if catalog_index(value, bytes_of("$a")) != 1 { return "lookup first definition" }
  if catalog_index(value, bytes_of("$b")) != 2 { return "lookup forward definition" }
  if catalog_index(value, bytes_of("$missing")) != catalog_missing { return "lookup missing" }
  let record = catalog_probe_first_record(value)
  if catalog_local_index(record, bytes_of("$x")) != 0 { return "lookup named param" }
  if catalog_local_index(record, bytes_of("$tmp")) != 1 { return "lookup local after param" }
  if catalog_local_index(record, bytes_of("$missing")) != catalog_missing { return "lookup missing local" }
  return "ok"
}

fn catalog_duplicate_import_result(input: text) -> text {
  let result = catalog_source(input)
  if before(result, " ") != "ok" { return result }
  let value: bytes<tokens_bytes_max> = bytes_of_hex(after(result, " "))
  if catalog_index(value, bytes_of("$x")) != 1 { return "lookup duplicate import last wins" }
  if catalog_index(value, bytes_of("$f")) != 2 { return "lookup definition after duplicate imports" }
  return "ok"
}

fn catalog_duplicate_local_result(input: text) -> text {
  let result = catalog_source(input)
  if before(result, " ") != "ok" { return result }
  let value: bytes<tokens_bytes_max> = bytes_of_hex(after(result, " "))
  let record = catalog_probe_first_record(value)
  if catalog_local_index(record, bytes_of("$x")) != 1 { return "lookup duplicate local last wins" }
  return "ok"
}
"""
    handlers = []
    for name in ("lookup", "duplicate_import", "duplicate_local"):
        handlers.append(f"""
on catalog_{name}_probe(input: text) -> text {{
  return catalog_{name}_result(input)
}}

on catalog_{name}_compile(input_path: text, output_path: text) -> text {{
  match read_file(input_path) {{
    ok(source) => {{
      let result = catalog_{name}_result(source)
      match write_bytes(output_path, hex_result(result)) {{
        ok(written) => {{ return written }}
        failed(error) => {{ return error }}
      }}
    }}
    failed(error) => {{ return error }}
  }}
}}
""")
    return pure + "\n".join(handlers)


def load_runner():
    spec = importlib.util.spec_from_file_location("catalog_runner", ROOT / "scripts/selfhost-func.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def finished(path):
    values = [json.loads(line).get("record", {}).get("status", {}).get("status", {}).get("finished") for line in path.read_text().splitlines()]
    values = [value for value in values if value is not None]
    if len(values) != 1 or values[0].get("outcome") != "completed" or not isinstance(values[0].get("result"), str):
        raise RuntimeError(f"not exactly one completed text result: {path}")
    return values[0]["result"]


def execute(args, run, program, module, case, directory, lookup=None):
    directory.mkdir(parents=True)
    interpreted = directory / "interpreter.jsonl"
    token_input = case.suffix == ".hex"
    handler = "catalog_token_input" if token_input else "catalog"
    compiled_handler = "catalog_token_compile" if token_input else "catalog_compile"
    if lookup:
        handler = f"catalog_{lookup}_probe"
        compiled_handler = f"catalog_{lookup}_compile"
    run([args.hl, "run", program, "--handler", handler, "--arg-file", case], interpreted)
    value = finished(interpreted)
    (directory / "input.wat").write_bytes(case.read_bytes())
    run([args.host, "run", "--module", module, "--journal", directory / "journal", "--cwd", directory,
         "--handler", compiled_handler, "--args", '{"input_path":"input.wat","output_path":"result.txt"}'], directory / "host.log")
    finished(directory / "journal/journal.jsonl")
    compiled = (directory / "result.txt").read_text()
    if compiled != value:
        raise RuntimeError(f"interpreter/compiled disagreement: {directory}")
    return value


def projection_checks(raw):
    def word(at):
        if at + WORD_BYTES > len(raw):
            raise RuntimeError("truncated oracle projection")
        return int.from_bytes(raw[at:at + WORD_BYTES], "little")
    def blob(at):
        end = at + WORD_BYTES + word(at)
        if end > len(raw):
            raise RuntimeError("truncated projection blob")
        return raw[at + WORD_BYTES:end], end
    if word(0) != 1:
        raise RuntimeError("wire version changed")
    at = WORD_BYTES
    sections = []
    for _ in CONTRACT["sections"]:
        section, at = blob(at)
        sections.append(section)
    if at != len(raw):
        raise RuntimeError("projection trailing bytes")
    return {"section_bytes": [len(section) for section in sections], "counts": [int.from_bytes(section[:WORD_BYTES], "little") for section in sections]}


def oracle_cases(args, run, out, cases):
    references = []
    records = []
    for repeat in range(args.runs):
        expected = {}
        for name, path, valid in cases:
            if path.suffix == ".hex":
                continue
            directory = out / "oracle" / str(repeat) / name
            directory.mkdir(parents=True)
            result = directory / "projection.bin"
            run([args.oracle, path, result], directory / "oracle.log", accepted=(0,) if valid else (1,))
            record = {"repeat": repeat, "case": name, "valid": valid}
            if valid:
                expected[name] = result.read_bytes()
                projection = projection_checks(expected[name])
                if name in EXPECTED_COUNTS and projection["counts"] != EXPECTED_COUNTS[name]:
                    raise RuntimeError(f"independent fixture count invariant failed: {name}")
                if repeat > 0 and expected[name] != references[0][name]:
                    raise RuntimeError(f"Rust projection differs across fresh processes: {name}")
                record["projection"] = identity(result)
            elif result.exists():
                raise RuntimeError(f"invalid Rust fixture wrote projection: {name}")
            records.append(record)
        references.append(expected)
    return references, records


def mutation_cases(args, run, out, cases, references):
    caught = []
    fixture_paths = {name: path for name, path, _ in cases}
    for name, (_, _, fixture) in MUTANTS.items():
        program = out / f"mutant-{name}.hl"
        module = out / f"mutant-{name}.wasm"
        run([args.hl, "check", program], out / f"mutant-{name}-check.log")
        run([args.hl, "emit", "--format", "wasm", "--target", "native-jsonl", program], module)
        for repeat in range(args.runs):
            value = execute(args, run, program, module, fixture_paths[fixture], out / "mutations" / name / str(repeat))
            if not value.startswith("ok "):
                raise RuntimeError(f"mutant rejected instead of semantic projection difference: {name}: {value}")
            mutant = bytes.fromhex(value[3:])
            projection_checks(mutant)
            if mutant == references[repeat][fixture]:
                raise RuntimeError(f"mutant survived: {name}")
            caught.append({"mutant": name, "repeat": repeat, "fixture": fixture, "both_modes": True, "accepted_projection_differs": True})
    return caught


def runtime(args, out, cases, evidence):
    runner = load_runner()
    def run(arguments, log, accepted=(0,)):
        return runner.bounded_run(arguments, log, accepted, args.command_seconds_max, args.command_output_bytes_max)
    program = out / "catalog-combined.hl"
    module = out / "catalog.wasm"
    run([args.hl, "check", program], out / "check.log")
    run([args.hl, "prove", program], out / "prove.log")
    run([args.hl, "emit", "--format", "wasm", "--target", "native-jsonl", program], module)
    references, oracle_records = oracle_cases(args, run, out, cases)
    records = []
    for repeat in range(args.runs):
        for name, path, valid in cases:
            directory = out / "runs" / str(repeat) / name
            value = execute(args, run, program, module, path, directory)
            if valid:
                if not value.startswith("ok ") or bytes.fromhex(value[3:]) != references[repeat][name]:
                    raise RuntimeError(f"exact projection mismatch: {directory}")
            elif value.startswith("ok "):
                raise RuntimeError(f"negative fixture admitted: {directory}")
            records.append({"repeat": repeat, "case": name, "valid": valid, "result_sha256": hashlib.sha256(value.encode()).hexdigest()})
    probe_program = out / "catalog-lookup-probe.hl"
    probe_program.write_text(program.read_text() + lookup_probe())
    probe_module = out / "catalog-lookup-probe.wasm"
    run([args.hl, "check", probe_program], out / "lookup-check.log")
    run([args.hl, "prove", probe_program], out / "lookup-prove.log")
    run([args.hl, "emit", "--format", "wasm", "--target", "native-jsonl", probe_program], probe_module)
    fixture_paths = {name: path for name, path, _ in cases}
    lookup_records = []
    probes = (("lookup", "same-signature"), ("duplicate_import", "duplicate-import-last-wins"), ("duplicate_local", "duplicate-local-last-wins"))
    for repeat in range(args.runs):
        for probe, fixture in probes:
            value = execute(args, run, probe_program, probe_module, fixture_paths[fixture], out / "lookup" / str(repeat) / probe, lookup=probe)
            if value != "ok":
                raise RuntimeError(f"catalog lookup probe failed: {probe}: {value}")
            lookup_records.append({"repeat": repeat, "probe": probe, "fixture": fixture, "both_modes": True})
    caught = mutation_cases(args, run, out, cases, references)
    evidence.update(status="passed-component-projection", runtime_runs=args.runs, records=records, mutants_caught=caught,
                    oracle_records=oracle_records, lookup_records=lookup_records,
                    repetition_scope="N fresh Rust processes per source fixture (not token negatives); N fresh interpreter/compiled-host executions per case and lookup probe; N semantic executions per mutant per engine, compared to that repetition's Rust fixture reference. Compile/check/emit once per program, not N independent builds.",
                    binaries={name: identity(getattr(args, name)) for name in ("hl", "host", "oracle")})


def main():
    parser = argparse.ArgumentParser(description="Catalog component projection; static is compiler-free. Runtime requires root allocation and prebuilt binaries. No full-module/bootstrap claim.")
    parser.add_argument("--static", action="store_true")
    parser.add_argument("--allocated", action="store_true")
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--hl", type=pathlib.Path)
    parser.add_argument("--host", type=pathlib.Path)
    parser.add_argument("--oracle", type=pathlib.Path)
    parser.add_argument("--encoder-wat", type=pathlib.Path, help="optional absolute path to pinned recovered original encoder WAT; source catalog projection only, never shaken full-module parity")
    parser.add_argument("--runs", type=int, default=RUNS_MIN)
    parser.add_argument("--command-seconds-max", type=int, default=300)
    parser.add_argument("--command-output-bytes-max", type=int, default=16777216)
    args = parser.parse_args()
    if args.runs < RUNS_MIN or args.command_seconds_max <= 0 or args.command_output_bytes_max <= 0:
        parser.error("positive resource bounds and N>=3 required")
    if not args.static and not args.allocated:
        parser.error("runtime requires explicit exclusive root allocation")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    cases = static(out, args.encoder_wat)
    evidence = {"status": "static-only", "runtime_runs": 0, "stage1_runs": 0, "stage2_runs": 0, "full_bootstrap_runs": 0,
                "component": identity(ROOT / "compiler/assemble-catalog.hl"), "harness_sources": {name: identity(ROOT / "scripts" / name) for name in ("selfhost-catalog.py", "selfhost-catalog-oracle.rs")}, "oracle_sources": ORACLE_HASHES,
                "accepted": sum(valid for _, _, valid in cases), "rejected": sum(not valid for _, _, valid in cases), "contract": CONTRACT}
    if args.encoder_wat is not None:
        evidence["optional_encoder_wat"] = {"caller_path": str(args.encoder_wat), "fixture": identity(out / "fixtures/encoder-original-catalog.wat"), "expected_projection_counts": EXPECTED_COUNTS["encoder-original-catalog"], "scope": "Unpruned SOURCE header catalog: 826 functions,11 imports,75 signatures. Raw reachable773 and post-shake587 differ; neither selection implemented here. Expected counts are not runtime observations."}
    evidence_path = out / "evidence.json"
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    if not args.static:
        for name in ("hl", "host", "oracle"):
            path = getattr(args, name)
            if path is None or not path.is_file():
                parser.error(f"prebuilt --{name} required")
            setattr(args, name, path.resolve())
        evidence["status"] = "runtime-incomplete"
        evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
        runtime(args, out, cases, evidence)
        evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    print(f'{evidence["status"]}: accepted={evidence["accepted"]} rejected={evidence["rejected"]}; runtime={evidence["runtime_runs"]}; stage1=0 stage2=0')


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError) as error:
        print(f"selfhost-catalog: {error}", file=sys.stderr)
        sys.exit(1)
