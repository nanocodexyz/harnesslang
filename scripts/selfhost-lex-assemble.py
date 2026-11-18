import argparse
import hashlib
import importlib.util
import json
import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "output/selfhost-lex-assemble"
PROGRAM = OUT / "assemble-combined.hl"
ORACLE = OUT / "oracle"
MODULE = OUT / "assemble.wasm"
REPEATS_MIN = 3
ORACLE_HASHES = {
    "assemble": "e9d433c6564d214f603c8b3aec2fc91b6a394506b07e4c800519a03c43f895b1",
    "shake": "16e1ba0e073e7823ce36cb705aec72a64b1497145a6fb8b4d66b046a199965f5",
    "encode": "024a299ce18c6250f304b25b3643ba54cb558e7fbbd958226e26589777b99f27",
    "flat": "c0d0da7c54e42fb9c1dd301574c053e66ee393af2377feab07ab1aa4ec9e8097",
}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def run(arguments, log, accepted=(0,)):
    result = subprocess.run([str(value) for value in arguments], cwd=ROOT, capture_output=True)
    log.write_bytes(result.stdout)
    log.with_suffix(log.suffix + ".err").write_bytes(result.stderr)
    if result.returncode not in accepted:
        raise RuntimeError(f"command failed ({result.returncode}): {arguments}; see {log}")
    return result


def finish(path):
    values = []
    for line in path.read_text().splitlines():
        status = json.loads(line).get("record", {}).get("status", {}).get("status", {})
        if "finished" in status:
            values.append(status["finished"])
    if len(values) != 1 or values[0].get("outcome") != "completed" or not isinstance(values[0].get("result"), str):
        raise RuntimeError(f"expected exactly one completed text result: {path}")
    return values[0]["result"]


def combined(assembler):
    lexer = (ROOT / "compiler/assemble-lex.hl").read_text().split("\non tokenize(")[0]
    encoder = (ROOT / "compiler/encode.hl").read_text().split("\non encode(")[0]
    chunks = ["harness assembly_subset\n"]
    for source in (lexer, encoder, assembler):
        lines = [line for line in source.splitlines() if not line.startswith("harness ")]
        if source == encoder:
            lines = [line for line in lines if not line.startswith("tool ")]
        chunks.append("\n".join(lines))
    return "\n\n".join(chunks) + "\n"


def static():
    for name, expected in ORACLE_HASHES.items():
        if sha((ROOT / f"src/emit/wasm/{name}.rs").read_bytes()) != expected:
            raise RuntimeError(f"Rust {name} oracle changed; do not re-bless it to match the port")
    spec = importlib.util.spec_from_file_location("lex_oracle", ROOT / "scripts/selfhost-lex.py")
    lexer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(lexer)
    lexer.OUT.mkdir(parents=True, exist_ok=True)
    lexer.frozen()
    assembler = (ROOT / "compiler/assemble.hl").read_text()
    if "const assembly_export_flat_overhead: u64 = 49" not in assembler:
        raise RuntimeError("flat export capacity derivation changed")
    if 10 * 4 + 4 + 1 + 4 != 49:
        raise RuntimeError("flat subset layout overhead does not match its derivation")
    generated = combined(assembler)
    declarations = re.findall(r"^(fn|on|tool|const) ([a-z_][a-z_0-9]*)", generated, re.MULTILINE)
    if len(set(declarations)) != len(declarations):
        raise RuntimeError("combined source has duplicate declarations")
    for kind, name in (("fn", "tokenize"), ("fn", "encode_hex"), ("on", "compile"), ("tool", "read_file")):
        if declarations.count((kind, name)) != 1:
            raise RuntimeError(f"combined source must contain exactly one {kind} {name}")
    if "pub const STATIC_BASE_BYTES: u32 = 1024;" not in (ROOT / "src/emit/wasm/assemble.rs").read_text():
        raise RuntimeError("Rust static layout base changed")
    PROGRAM.write_text(generated)
    return assembler


def fixtures():
    accepted = {
        "empty": "",
        "func": "(func $f)",
        "whitespace": " \t\r\n",
        "zero": "(memory 0)",
        "one": "(memory 1)",
        "leading-zero": "(memory 00002)",
        "leb-edge": "(memory 127)",
        "leb-next": "(memory 128)",
        "wasm32-pages": "(memory 65536)",
        "u32-max": "(memory 4294967295)",
        "export-first": '(memory (export "memory") 2)',
        "export-last": '(memory 2 (export "memory"))',
        "export-empty": '(memory (export "") 0)',
        "multiline": '\n(memory\n (export\n "memory")\n 3\n)\n',
        "export-escapes": r'''(memory (export "a\n\t\\\"\'\00\7f\+f") 1)''',
        "export-controls": '(memory (export "a\x00\r\n") 1)',
    }
    rejected = {
        "module-wrapper": "(module (memory 1))",
        "func-shaped-memory": "(func 1)",
        "global": "(global $g i32 (i32.const 0))",
        "data": '(data (i32.const 0) "x")',
        "two-memories": "(memory 1)(memory 2)",
        "atom": "memory",
        "empty-list": "()",
        "missing-pages": "(memory)",
        "missing-pages-export": '(memory (export "x"))',
        "duplicate-pages": "(memory 1 2)",
        "negative": "(memory -1)",
        "minus-zero": "(memory -0)",
        "plus": "(memory +1)",
        "hex": "(memory 0x1)",
        "underscore": "(memory 1_0)",
        "float": "(memory 1.0)",
        "u32-overflow": "(memory 4294967296)",
        "u64-overflow": "(memory 18446744073709551616)",
        "unicode-digit": "(memory ١)",
        "named": "(memory $m 1)",
        "text-pages": '(memory "1")',
        "trailing-field": "(memory 1 unknown)",
        "unknown-list": "(memory 1 (shared))",
        "unknown-export": '(memory (foo "x") 1)',
        "duplicate-export": '(memory (export "x") (export "y") 1)',
        "export-extra": '(memory (export "x" 0) 1)',
        "export-nontext": "(memory (export x) 1)",
        "non-ascii-name": '(memory (export "é") 1)',
        "invalid-utf8-name": r'''(memory (export "\ff") 1)''',
        "unclosed": "(memory 1",
        "unexpected-close": "(memory 1))",
        "bad-escape": r'''(memory (export "\xz") 1)''',
        "comment": ";; comment\n(memory 1)",
    }
    paths = []
    for valid, cases in ((True, accepted), (False, rejected)):
        for name, source in cases.items():
            path = OUT / "fixtures" / f"{name}.wat"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(source.encode())
            paths.append((name, path, valid))
    return paths


def token_fixtures():
    def header(kind, line=1, payload=b"", size=None):
        return bytes([kind]) + line.to_bytes(4, "little") + (len(payload) if size is None else size).to_bytes(4, "little") + payload
    opened = header(1)
    memory = header(3, payload=b"memory")
    pages = header(3, payload=b"1")
    closed = header(2)
    cases = {
        "truncated-header": b"\x01",
        "unknown-kind": header(5),
        "zero-line": header(1, line=0),
        "delimiter-payload": header(1, payload=b"x"),
        "truncated-payload": header(3, payload=b"x", size=2),
        "oversized-payload": header(3, size=4294967295),
        "unclosed-records": opened + memory + pages,
        "mismatched-close-line": opened + memory + pages + header(2, line=2),
        "extra-close": opened + memory + pages + closed + closed,
        "empty-atom": opened + memory + header(3) + closed,
    }
    paths = []
    for name, data in cases.items():
        path = OUT / "fixtures" / f"token-{name}.hex"
        path.write_text(data.hex())
        paths.append((name, path))
    return paths


def interpreter(hl, program, handler, path, directory):
    log = directory / f"interpreter-{handler}.jsonl"
    run([hl, "run", program, "--handler", handler, "--arg-file", path], log)
    return finish(log)


def wasm(host, module, handler, path, directory):
    directory.mkdir(parents=True, exist_ok=True)
    input_path = directory / "input.txt"
    output_path = directory / "result.out"
    shutil.copyfile(path, input_path)
    run([host, "run", "--module", module, "--journal", directory / "journal", "--cwd", directory,
         "--handler", handler, "--args", '{"input_path":"input.txt","output_path":"result.out"}'], directory / "host.out")
    result = finish(directory / "journal/journal.jsonl")
    return (output_path.read_bytes() if output_path.is_file() else None), result


def identity(data):
    return {"bytes": len(data), "sha256": sha(data)}


def compare(args, cases, tokens):
    records = []
    for repeat in range(args.runs):
        for name, path, valid in cases:
            directory = OUT / "runs" / str(repeat) / name
            directory.mkdir(parents=True, exist_ok=True)
            value = interpreter(args.hl, PROGRAM, "assemble", path, directory)
            flat, error = wasm(args.host, MODULE, "flatten", path, directory / "flat")
            record = {"repeat": repeat + 1, "case": name, "accepted": valid, "source": identity(path.read_bytes())}
            if valid:
                if not value.startswith("ok "):
                    raise RuntimeError(f"accepted case rejected by interpreter: {name}: {value}")
                expected_flat = directory / "rust.flat"
                expected_wasm = directory / "rust.wasm"
                run([ORACLE, path, expected_flat, expected_wasm], directory / "oracle.log")
                interpreted_flat = bytes.fromhex(value[3:])
                rust_flat = expected_flat.read_bytes()
                if interpreted_flat != rust_flat or flat != rust_flat:
                    raise RuntimeError(f"flat byte mismatch: {name}; see {directory}")
                (directory / "interpreter.flat").write_bytes(interpreted_flat)
                encoded = interpreter(args.hl, PROGRAM, "encode_assembly", path, directory)
                if not encoded.startswith("ok "):
                    raise RuntimeError(f"accepted case failed encoding: {name}: {encoded}")
                interpreted_wasm = bytes.fromhex(encoded[3:])
                compiled_wasm, _ = wasm(args.host, MODULE, "compile", path, directory / "wasm")
                rust_wasm = expected_wasm.read_bytes()
                if interpreted_wasm != rust_wasm or compiled_wasm != rust_wasm:
                    raise RuntimeError(f"wasm byte mismatch: {name}; see {directory}")
                (directory / "interpreter.wasm").write_bytes(interpreted_wasm)
                record["flat"] = {mode: identity(data) for mode, data in (("rust", rust_flat), ("interpreter", interpreted_flat), ("wasm", flat))}
                record["module"] = {mode: identity(data) for mode, data in (("rust", rust_wasm), ("interpreter", interpreted_wasm), ("wasm", compiled_wasm))}
                record["direct_byte_comparison"] = True
            else:
                if value.startswith("ok ") or flat is not None or error.startswith("ok "):
                    raise RuntimeError(f"unsupported source accepted: {name}; see {directory}")
                if value != error:
                    raise RuntimeError(f"negative result differs between modes: {name}")
                encoded = interpreter(args.hl, PROGRAM, "encode_assembly", path, directory)
                encoded_output, encoded_error = wasm(args.host, MODULE, "compile", path, directory / "wasm")
                if encoded != value or encoded_output is not None or encoded_error != value:
                    raise RuntimeError(f"unsupported source reached the encoder: {name}")
                record["rejection"] = value
            records.append(record)
        for name, path in tokens:
            directory = OUT / "runs" / str(repeat) / f"token-{name}"
            directory.mkdir(parents=True, exist_ok=True)
            value = interpreter(args.hl, PROGRAM, "assemble_tokens", path, directory)
            compiled, _ = wasm(args.host, MODULE, "compile_tokens", path, directory / "wasm")
            if compiled is None or value.startswith("ok ") or compiled.decode() != value:
                raise RuntimeError(f"malformed token stream not rejected identically: {name}")
            records.append({"repeat": repeat + 1, "case": f"token-{name}", "accepted": False, "rejection": value})
        (OUT / f"run-{repeat + 1}.json").write_text(json.dumps([record for record in records if record["repeat"] == repeat + 1], indent=2) + "\n")
        print(f"ok assembly-subset repeat={repeat + 1} source_cases={len(cases)} malformed_tokens={len(tokens)} modes=interpreter,wasm", flush=True)
    for name, _, valid in cases:
        values = [record for record in records if record["case"] == name]
        for field in (("flat", "module") if valid else ("rejection",)):
            if len({json.dumps(record[field], sort_keys=True) for record in values}) != 1:
                raise RuntimeError(f"non-deterministic repeated result: {name}: {field}")
    return records


def mutations(args, original):
    plants = {
        "page-count": ("pages = assembly_pages(payload)", "pages = assembly_pages(payload) + 1", "one", True),
        "omit-export": ("return assembly_layout(phase == assembly_done, pages, exported, export_name)", "return assembly_layout(phase == assembly_done, pages, false, export_name)", "export-first", True),
        "duplicate-memory": ('if form_memory and memory_seen { return "assembler subset: duplicate memory" }', 'if false { return "assembler subset: duplicate memory" }', "two-memories", False),
    }
    caught = []
    for name, (before, after, case, valid) in plants.items():
        if original.count(before) != 1:
            raise RuntimeError(f"mutation site is not unique: {name}")
        program = OUT / f"mutation-{name}.hl"
        program.write_text(combined(original.replace(before, after)))
        directory = OUT / "mutations" / name
        directory.mkdir(parents=True, exist_ok=True)
        fixture = OUT / "fixtures" / f"{case}.wat"
        value = interpreter(args.hl, program, "assemble", fixture, directory)
        if valid:
            run([ORACLE, fixture, directory / "rust.flat", directory / "rust.wasm"], directory / "oracle.log")
            failed = not value.startswith("ok ") or bytes.fromhex(value[3:]) != (directory / "rust.flat").read_bytes()
        else:
            failed = value.startswith("ok ")
        if not failed:
            raise RuntimeError(f"mutation survived or did not activate: {name}")
        module = directory / "mutated.wasm"
        run([args.hl, "emit", "--format", "wasm", "--target", "native-jsonl", program], module)
        output, error = wasm(args.host, module, "flatten", fixture, directory / "wasm")
        if valid:
            wasm_failed = output != (directory / "rust.flat").read_bytes()
        else:
            wasm_failed = output is not None
        if not wasm_failed:
            raise RuntimeError(f"compiled wasm mutation survived or did not activate: {name}: {error}")
        caught.append(name)
    return caught


def main():
    parser = argparse.ArgumentParser(description="Memory-only WAT -> lexer records -> flat -> unchanged encode.hl. Not a full assembler, compiler, or stage2. No automatic heavy build; requires allocated local runtime and prebuilt hl/hl-host. ASCII export bytes only; decimal-u32 pages (not Wasm validity checking).")
    parser.add_argument("--static", action="store_true")
    parser.add_argument("--runs", type=int, default=REPEATS_MIN)
    parser.add_argument("--hl", type=pathlib.Path, default=ROOT / "target/release/hl")
    parser.add_argument("--host", type=pathlib.Path, default=ROOT / "hosts/native/target/release/hl-host")
    parser.add_argument("--mutations", action="store_true")
    args = parser.parse_args()
    if args.runs < REPEATS_MIN:
        parser.error("at least three repeated identical corpus runs are required")
    OUT.mkdir(parents=True, exist_ok=True)
    evidence = {"status": "unmeasured", "full_bootstrap_runs": 0, "full_compiler_cases": 0, "assembly_subset_cases_passed": 0, "repeats": 0}
    evidence_path = OUT / "evidence.json"
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    original = static()
    cases = fixtures()
    tokens = token_fixtures()
    if args.static:
        print(f"static only: frozen Rust sources verified; combined source generated; {len(cases)} source fixtures and {len(tokens)} malformed token fixtures; runtime cases=0 bootstrap runs=0")
        return
    for binary in (args.hl, args.host):
        if not binary.is_file():
            raise RuntimeError(f"prebuilt binary required: {binary}")
    runs = OUT / "runs"
    if runs.exists():
        shutil.rmtree(runs)
    run(["rustc", "--edition=2024", "-O", ROOT / "scripts/selfhost-lex-assemble-oracle.rs", "-o", ORACLE], OUT / "oracle-build.log")
    run([args.hl, "check", PROGRAM], OUT / "check.log")
    run([args.hl, "prove", PROGRAM], OUT / "prove.log")
    run([args.hl, "emit", "--format", "wasm", "--target", "native-jsonl", PROGRAM], MODULE)
    records = compare(args, cases, tokens)
    caught = mutations(args, original) if args.mutations else []
    evidence.update(status="passed", assembly_subset_cases_passed=sum(valid for _, _, valid in cases), repeats=args.runs,
                    rejected_source_cases=sum(not valid for _, _, valid in cases), rejected_token_cases=len(tokens),
                    mutation_plants_caught=caught, records=records, source_sha256=sha(PROGRAM.read_bytes()),
                    binaries={"hl": identity(args.hl.read_bytes()), "host": identity(args.host.read_bytes())},
                    component_sources={name: identity((ROOT / "compiler" / name).read_bytes()) for name in ("assemble-lex.hl", "assemble.hl", "encode.hl")},
                    limitations="One memory only, ASCII export bytes, decimal u32. This retained memory-focused corpus also accepts one supported unexported function; the separate function runner covers the broader subset. No module wrapper, globals, data, general name resolution, shake port, full source compiler, stage1 or stage2. Rust assembler accepts u32 pages above Wasm32 validation limits; these cases measure byte parity only, not executable module validity.")
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"passed: {evidence['assembly_subset_cases_passed']} subset cases, {args.runs} repeats per mode; {len(caught)} mutation plants; full bootstrap runs=0")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, AssertionError, ValueError) as error:
        print(f"selfhost-lex-assemble: {error}", file=sys.stderr)
        sys.exit(1)
