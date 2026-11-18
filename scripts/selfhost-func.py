import argparse
import importlib.util
import json
import os
import pathlib
import selectors
import signal
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "output/selfhost-func"
TARGETS = ("native-jsonl", "native-sqlite", "server-postgres", "cloudflare", "vercel")
RUNS_MIN = 3
COMMAND_SECONDS_MAX_DEFAULT = 300
COMMAND_OUTPUT_BYTES_MAX_DEFAULT = 16777216
COMMAND_CLEANUP_SECONDS_MAX = 5
COMMAND_READ_BYTES_MAX = 65536
GRAMMAR = {
    "module": "zero or one memory and zero or one named function, either order, no module wrapper",
    "memory": "unchanged decimal-u32 page count and optional ASCII export, either order",
    "function": "(func $[A-Za-z0-9_]+ header* flat-instruction*)",
    "header": "at most one nonempty unnamed (param (i32|i64)+), one (result (i32|i64)), one (export ASCII-string), any order before body",
    "instructions": "i32.const/i64.const decimal-immediate; local.get unsigned-decimal-param-index; nop; drop; i32/i64 add/sub/mul/div_s/div_u",
    "immediates": "optional minus for const only; i32 range -2147483648..4294967295; i64 range -9223372036854775808..18446744073709551615; unsigned bit patterns use Rust signed LEB wrapping",
    "bounds": "existing source 4194304 bytes, tokens 41943040 bytes, flat/module 4194304 bytes, params <= encoder locals_max 65536; structural depth <= 2 from grammar; bounds never truncate",
    "reachability": "fully validate supported unexported function then omit its type/body to match unchanged Rust reachable(); no calls/self-references in grammar",
    "failures": "malformed/unsupported input returns text and no encoded output; source-capacity exhaustion remains named runtime trap; wasm numeric div traps are emitted, not folded or executed by assembler",
    "limits": "byte assembler, not Wasm type/stack validation; no folded expressions, return/unreachable, branches, locals declarations, named params, calls, imports, globals, data, shake port, full compiler, stage1 or stage2",
}


def bounded_run(arguments, log, accepted, seconds_max, output_bytes_max):
    arguments = [str(value) for value in arguments]
    stderr_path = log.with_suffix(log.suffix + ".err")
    deadline = time.monotonic() + seconds_max
    total = 0
    with log.open("wb") as stdout, stderr_path.open("wb") as stderr, selectors.DefaultSelector() as ready:
        process = subprocess.Popen(arguments, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        try:
            ready.register(process.stdout, selectors.EVENT_READ, stdout)
            ready.register(process.stderr, selectors.EVENT_READ, stderr)
            while ready.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError(f"bound(command_seconds_max={seconds_max}): {arguments}; see {log}")
                events = ready.select(remaining)
                for key, _ in events:
                    chunk = os.read(key.fileobj.fileno(), min(COMMAND_READ_BYTES_MAX, output_bytes_max - total + 1))
                    if not chunk:
                        ready.unregister(key.fileobj)
                        key.fileobj.close()
                    else:
                        if len(chunk) > output_bytes_max - total:
                            raise RuntimeError(f"bound(command_output_bytes_max={output_bytes_max}): {arguments}; see {log}")
                        key.data.write(chunk)
                        total += len(chunk)
            try:
                code = process.wait(timeout=max(0, deadline - time.monotonic()))
            except subprocess.TimeoutExpired as error:
                raise RuntimeError(f"bound(command_seconds_max={seconds_max}): {arguments}; see {log}") from error
            if code not in accepted:
                raise RuntimeError(f"command failed ({code}): {arguments}; see {log}")
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            if process.poll() is None:
                process.wait(timeout=COMMAND_CLEANUP_SECONDS_MAX)
            for pipe in (process.stdout, process.stderr):
                if not pipe.closed:
                    pipe.close()
    return subprocess.CompletedProcess(arguments, code, log.read_bytes(), stderr_path.read_bytes())


def load_base():
    spec = importlib.util.spec_from_file_location("memory_assembly", ROOT / "scripts/selfhost-lex-assemble.py")
    base = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(base)
    base.OUT = OUT
    base.PROGRAM = OUT / "assemble-combined.hl"
    base.ORACLE = OUT / "oracle"
    base.MODULE = OUT / "assemble.wasm"
    return base


def sources(base):
    cases = base.fixtures()
    cases = [(name, path, True if name == "func" else valid) for name, path, valid in cases]
    accepted = {
        "function-void": '(func $f (export "f"))',
        "function-nop": '(func $f (export "f") nop)',
        "unexported-signature": '(func $f (param i32 i64) (result i64) local.get 1)',
        "export-empty-name": '(func $f (export "") (result i32) i32.const 0)',
        "headers-reordered": '(func $f (result i64) (param i32 i64) (export "f") local.get 1)',
        "signature": '(memory 1)(func $f (export "f") (param i32 i64 i32) (result i64) local.get 1)',
        "multiple-export-order": '(func $f (export "f") nop)(memory 1 (export "memory"))',
        "memory-export-first": '(memory (export "memory") 1)(func $f (export "f") nop)',
        "local-leb-edge": '(func $f (export "f") (param ' + ' '.join(['i32'] * 129) + ') (result i32) local.get 128)',
        "mixed-params": '(func $f (param i32 i64) (export "f") local.get 0 drop local.get 1 drop)',
        "decimal-leading-zero": '(func $f (export "f") (result i32) i32.const -00000)',
        "wrapped-i32": '(memory 1)(func $f (export "f") (result i32) i32.const 4294967295)',
        "wrapped-i64": '(memory 1)(func $f (export "f") (result i64) i64.const 18446744073709551615)',
        "runtime-div-zero": '(memory 1)(func $f (export "f") (result i32) i32.const 1 i32.const 0 i32.div_u)',
        "runtime-div-overflow": '(memory 1)(func $f (export "f") (result i64) i64.const -9223372036854775808 i64.const -1 i64.div_s)',
        "runtime-wrapped-add": '(memory 1)(func $f (export "f") (result i32) i32.const 2147483647 i32.const 1 i32.add)',
        "runtime-param-add": '(memory 1)(func $f (export "f") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add)',
        "ascii-export-escape": r'(func $f (export "a\n\t\\\"\00\7f\+f") nop)',
    }
    for ty, values in (("i32", (-2147483648, -65, -64, -1, 0, 63, 64, 127, 128, 2147483647, 2147483648, 4294967295)),
                       ("i64", (-9223372036854775808, -2147483649, -65, -64, -1, 0, 63, 64, 127, 128, 9223372036854775807, 9223372036854775808, 18446744073709551615))):
        for value in values:
            accepted[f"{ty}-constant-{value}"] = f'(func $f (export "f") (result {ty}) {ty}.const {value})'
        for op in ("add", "sub", "mul", "div_s", "div_u"):
            accepted[f"{ty}-{op}"] = f'(func $f (export "f") (result {ty}) {ty}.const 8 {ty}.const 2 {ty}.{op})'
    rejected = {
        "function-unnamed": '(func (export "f"))',
        "function-empty-name": '(func $ (export "f"))',
        "function-nonascii-name": '(func $é (export "f"))',
        "function-name-policy": '(func $f-x (export "f"))',
        "two-functions": '(func $a)(func $b)',
        "duplicate-param-field": '(func $f (param i32) (param i64))',
        "duplicate-result-field": '(func $f (result i32) (result i64))',
        "duplicate-function-export": '(func $f (export "x") (export "y"))',
        "param-empty": '(func $f (param))',
        "param-named": '(func $f (param $x i32))',
        "param-invalid": '(func $f (param f32))',
        "param-nested": '(func $f (param (i32)))',
        "result-empty": '(func $f (result))',
        "result-multiple": '(func $f (result i32 i64))',
        "result-named": '(func $f (result $x i32))',
        "result-unknown": '(func $f (result f64))',
        "function-export-extra": '(func $f (export "x" 0))',
        "function-export-unknown": '(func $f (export x))',
        "function-export-nonascii": '(func $f (export "é"))',
        "function-export-bad-utf8": r'(func $f (export "\ff"))',
        "header-after-body": '(func $f nop (export "f"))',
        "unknown-instruction": '(func $f (export "f") mystery)',
        "unexported-unknown": '(func $f mystery)',
        "unexported-missing-immediate": '(func $f i32.const)',
        "const-missing": '(func $f (export "f") i32.const)',
        "local-missing": '(func $f (export "f") (param i32) local.get)',
        "const-string": '(func $f (export "f") i64.const "1")',
        "const-folded": '(func $f (export "f") (i32.const 1))',
        "const-prefix": '(func $f (export "f") i32.const -)',
        "const-plus": '(func $f (export "f") i32.const +1)',
        "const-hex": '(func $f (export "f") i32.const 0x1)',
        "const-underscore": '(func $f (export "f") i32.const 1_0)',
        "const-unicode": '(func $f (export "f") i32.const ١)',
        "const-symbol": '(func $f (export "f") i32.const $static_end)',
        "const-operand-op": '(func $f (export "f") i32.const nop)',
        "const-extra": '(func $f (export "f") i32.const 1 2)',
        "i32-too-positive": '(func $f (export "f") i32.const 4294967296)',
        "i32-too-negative": '(func $f (export "f") i32.const -2147483649)',
        "i64-too-positive": '(func $f (export "f") i64.const 18446744073709551616)',
        "i64-too-negative": '(func $f (export "f") i64.const -9223372036854775809)',
        "huge-constant": '(func $f (export "f") i64.const ' + '9' * 100 + ')',
        "local-without-params": '(func $f (export "f") local.get 0)',
        "local-past-params": '(func $f (export "f") (param i64) local.get 1)',
        "local-negative": '(func $f (export "f") (param i64) local.get -0)',
        "local-name": '(func $f (export "f") (param i64) local.get $x)',
        "local-set": '(func $f (export "f") (param i64) local.set 0)',
        "locals-declaration": '(func $f (local i32))',
        "call": '(func $f (export "f") call $f)',
        "return": '(func $f (export "f") return)',
        "unreachable": '(func $f (export "f") unreachable)',
        "block": '(func $f (export "f") block end)',
        "unclosed-function": '(func $f (export "f") i32.const 1',
        "partial-header": '(func $f (param i32',
        "params-over-bound": '(func $f (export "f") (param ' + ' '.join(['i32'] * (65536 + 1)) + ') nop)',

    }
    for valid, collection in ((True, accepted), (False, rejected)):
        for name, source in collection.items():
            path = OUT / "fixtures" / f"{name}.wat"
            path.write_text(source)
            cases.append((name, path, valid))
    return cases


def tokens(base):
    cases = base.token_fixtures()
    def record(kind, payload=b"", line=1):
        return bytes([kind]) + line.to_bytes(4, "little") + len(payload).to_bytes(4, "little") + payload
    head = record(1) + record(3, b"func") + record(3, b"$f")
    values = {
        "function-partial": head,
        "function-close-line": head + record(2, line=2),
        "function-dangling-const": head + record(3, b"i64.const") + record(2),
        "function-operand-is-open": head + record(3, b"i32.const") + record(1) + record(2) + record(2),
        "function-export-close-line": head + record(1) + record(3, b"export") + record(4, b"f") + record(2, line=2) + record(2),
        "function-atom-empty": head + record(3) + record(2),
    }
    for name, value in values.items():
        path = OUT / "fixtures" / f"token-{name}.hex"
        path.write_text(value.hex())
        cases.append((name, path))
    return cases


def mutations(base, args, original):
    plants = {
        "type-byte": ('return assembly_i64_type', 'return assembly_i32_type', 'signature', True),
        "constant-opcode": ('return assembly_i32_constant', 'return assembly_i64_constant', 'wrapped-i32', True),
        "signed-edge": ('value = widened(value)', 'value = value % 4294967296', 'wrapped-i32', True),
        "add-opcode": ('if assembly_matches(payload, "i32.add") { return 106 }', 'if assembly_matches(payload, "i32.add") { return 107 }', 'i32-add', True),
        "omit-function": ('if not exported { return assembly_layout', 'if exported { return assembly_layout', 'function-void', True),
        "local-bound": ('limit = params - 1', 'limit = params', 'local-past-params', False),
        "i32-bound": ('if opcode == assembly_i32_constant { limit = assembly_u32_max }', 'if opcode == assembly_i32_constant { limit = assembly_u64_max }', 'i32-too-positive', False),
        "memory-pages": ('pages = assembly_pages(payload)', 'pages = assembly_pages(payload) + 1', 'one', True),
    }
    caught = []
    for name, (before, after, case, valid) in plants.items():
        if original.count(before) != 1:
            raise RuntimeError(f"mutation site not unique: {name}")
        program = OUT / f"mutation-{name}.hl"
        program.write_text(base.combined(original.replace(before, after)))
        directory = OUT / "mutations" / name
        directory.mkdir(parents=True, exist_ok=True)
        fixture = OUT / "fixtures" / f"{case}.wat"
        value = base.interpreter(args.hl, program, "assemble", fixture, directory)
        expected = None
        if valid:
            base.run([base.ORACLE, fixture, directory / "rust.flat", directory / "rust.wasm"], directory / "oracle.log")
            expected = (directory / "rust.flat").read_bytes()
        if valid:
            failed = not value.startswith("ok ") or bytes.fromhex(value[3:]) != expected
        else:
            failed = value.startswith("ok ")
        if not failed:
            raise RuntimeError(f"interpreter mutation survived: {name}")
        module = directory / "mutated.wasm"
        base.run([args.hl, "emit", "--format", "wasm", "--target", "native-jsonl", program], module)
        actual, error = base.wasm(args.host, module, "flatten", fixture, directory / "wasm")
        survived = actual == expected if valid else actual is None
        if survived:
            raise RuntimeError(f"compiled mutation survived: {name}: {error}")
        caught.append(name)
    return caught


def probe(base, args, cases):
    names = {"function-void", "signature", "wrapped-i32", "wrapped-i64", "i64-constant--9223372036854775808", "runtime-div-zero", "runtime-div-overflow", "memory-export-first", "multiple-export-order", "const-missing", "i64-too-positive", "local-past-params", "unexported-unknown"}
    selected = [(name, path, valid) for name, path, valid in cases if name in names]
    source = base.PROGRAM.read_text().split("\non assemble(")[0]
    source = "\n".join(line for line in source.splitlines() if not line.startswith("tool ")) + "\n"
    checks = []
    for index, (name, path, valid) in enumerate(selected):
        directory = OUT / "probe-oracle" / name
        directory.mkdir(parents=True, exist_ok=True)
        literal = json.dumps(path.read_text(), ensure_ascii=True)
        if valid:
            base.run([base.ORACLE, path, directory / "rust.flat", directory / "rust.wasm"], directory / "oracle.log")
            flat = 'ok ' + (directory / "rust.flat").read_bytes().hex()
            wasm = 'ok ' + (directory / "rust.wasm").read_bytes().hex()
            condition = f'assembly_flat({literal}) == "{flat}" and assembly_module({literal}) == "{wasm}"'
        else:
            condition = f'before(assembly_flat({literal}), " ") != "ok" and before(assembly_module({literal}), " ") != "ok"'
        source += f'\nfn function_probe_{index}() -> bool {{\n  return {condition}\n}}\n'
        checks.append(f'  if not function_probe_{index}() {{ return "mismatch-{name}" }}')
    source += '\non probe() -> text {\n' + '\n'.join(checks) + '\n  return "ok"\n}\n'
    program = OUT / "probe.hl"
    program.write_text(source)
    directory = OUT / "probe"
    directory.mkdir(parents=True, exist_ok=True)
    base.run([args.hl, "run", program, "--handler", "probe"], directory / "interpreter.jsonl")
    if base.finish(directory / "interpreter.jsonl") != "ok":
        raise RuntimeError("oracle-backed all-target probe failed under interpreter")
    for target in TARGETS:
        target_out = directory / target
        target_out.mkdir(parents=True, exist_ok=True)
        module = target_out / "probe.wasm"
        base.run([args.hl, "check", "--target", target, base.PROGRAM], target_out / "check.log")
        base.run([args.hl, "prove", "--target", target, base.PROGRAM], target_out / "prove.log")
        base.run([args.hl, "prove", "--target", target, program], target_out / "probe-prove.log")
        base.run([args.hl, "emit", "--target", target, "--format", "wasm", program], module)
        for repeat in range(args.runs):
            run_out = target_out / str(repeat)
            run_out.mkdir(parents=True, exist_ok=True)
            trace = run_out / "trace.jsonl"
            base.run([args.hl, "sim", "--target", target, "--seeds", "0..1", "--trace", trace, program], run_out / "sim.log")
            base.run([args.node, ROOT / "scripts/wasm-sim.mjs", "--target", target, "--module", module, "--trace", trace], run_out / "node.log")
            base.run([args.hl, "wasm-sim", "--target", target, "--module", module, "--trace", trace], run_out / "interpreter.log")
    return {"fixtures": len(selected), "targets": list(TARGETS), "runs": args.runs, "engines": ["hl interpreter", "Node WebAssembly", "hl wasm-sim"], "expected": "unchanged Rust flat+actual module bytes for valid inputs, failclosed for invalid inputs"}


def bounds(base, args):
    directory = OUT / "bounds"
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / "oversized.wat"
    source.write_bytes(b" " * (4194304 + 1))
    base.run([args.hl, "run", base.PROGRAM, "--handler", "assemble", "--arg-file", source], directory / "interpreter.jsonl")
    compiled = directory / "wasm"
    compiled.mkdir(parents=True, exist_ok=True)
    (compiled / "input.txt").write_bytes(source.read_bytes())
    base.run([args.host, "run", "--module", base.MODULE, "--journal", compiled / "journal", "--cwd", compiled,
              "--handler", "compile", "--args", '{"input_path":"input.txt","output_path":"result.out"}'], compiled / "host.log", accepted=(0, 1))
    for journal in (directory / "interpreter.jsonl", compiled / "journal/journal.jsonl"):
        finished = [json.loads(line).get("record", {}).get("status", {}).get("status", {}).get("finished") for line in journal.read_text().splitlines()]
        finished = [value for value in finished if value is not None]
        if len(finished) != 1 or finished[0].get("outcome") != "failed" or finished[0].get("result") != {"trap": "bound(source_bytes_max)"}:
            raise RuntimeError(f"source exhaustion must trap by name: {journal}")
    if (compiled / "result.out").exists():
        raise RuntimeError("source bound failure wrote module output")
    return {"source_bytes_max_plus_one": "bound(source_bytes_max)", "modes": ["interpreter", "compiled native-jsonl"]}


def static(base):
    original = base.static()
    if 'const assembly_function_flat_overhead: u64 = 69' not in original or 17 * 4 + 1 != 69:
        raise RuntimeError("function flat overhead derivation changed")
    (OUT / "grammar.json").write_text(json.dumps(GRAMMAR, indent=2) + "\n")
    return original


def main():
    global OUT
    parser = argparse.ArgumentParser(description="Bounded function+memory assembler port. No full compiler or stage1/stage2 claims. All runtime commands require exclusive root allocation and prebuilt binaries; no implicit build.")
    parser.add_argument("--static", action="store_true")
    parser.add_argument("--allocated", action="store_true")
    parser.add_argument("--out", type=pathlib.Path, default=OUT)
    parser.add_argument("--hl", type=pathlib.Path, default=ROOT / "target/release/hl")
    parser.add_argument("--host", type=pathlib.Path, default=ROOT / "hosts/native/target/release/hl-host")
    parser.add_argument("--node", default="node")
    parser.add_argument("--runs", type=int, default=RUNS_MIN)
    parser.add_argument("--command-seconds-max", type=int, default=COMMAND_SECONDS_MAX_DEFAULT, help="caller policy: wall seconds per child command, including pipe drain")
    parser.add_argument("--command-output-bytes-max", type=int, default=COMMAND_OUTPUT_BYTES_MAX_DEFAULT, help="caller policy: combined stdout/stderr bytes per child command")
    args = parser.parse_args()
    if args.command_seconds_max <= 0 or args.command_output_bytes_max <= 0:
        parser.error("command resource bounds must be positive")
    if args.runs < RUNS_MIN:
        parser.error("at least three repeated corpus runs required")
    if not args.static and not args.allocated:
        parser.error("runtime requires --allocated after root grants exclusive shared-box use")
    OUT = args.out.resolve()
    OUT.mkdir(parents=True, exist_ok=True)
    marker = OUT / "runtime-started.json"
    if marker.exists():
        raise RuntimeError("runtime evidence already exists; preserve it and select a fresh --out directory")
    if not args.static:
        marker.write_text(json.dumps({"runs": args.runs, "status": "incomplete-unless-evidence-passed"}) + "\n")
    evidence_path = OUT / "evidence.json"
    evidence = {"status": "static-only", "full_bootstrap_runs": 0, "stage1_runs": 0, "stage2_runs": 0, "runtime_runs": 0}
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    base = load_base()
    base.run = lambda arguments, log, accepted=(0,): bounded_run(arguments, log, accepted, args.command_seconds_max, args.command_output_bytes_max)
    evidence["runner_policy"] = {"command_seconds_max": args.command_seconds_max, "command_output_bytes_max": args.command_output_bytes_max, "cleanup_seconds_max": COMMAND_CLEANUP_SECONDS_MAX}
    original = static(base)
    cases = sources(base)
    malformed = tokens(base)
    if args.static:
        print(f"static-ready: accepted={sum(valid for _, _, valid in cases)} rejected={sum(not valid for _, _, valid in cases)} malformed-tokens={len(malformed)}; runtime=0; bootstrap=0")
        return
    for binary in (args.hl, args.host):
        if not binary.is_file():
            raise RuntimeError(f"prebuilt binary required: {binary}")
    base.run([sys.executable, ROOT / "scripts/selfhost-func-runner-drill.py", "--allocated", "--out", OUT / "runner-drill"], OUT / "runner-drill.log")
    evidence["runner_drill"] = json.loads((OUT / "runner-drill/evidence.json").read_text())
    base.run(["rustc", "--edition=2024", "-O", ROOT / "scripts/selfhost-lex-assemble-oracle.rs", "-o", base.ORACLE], OUT / "oracle-build.log")
    base.run([args.hl, "check", base.PROGRAM], OUT / "check.log")
    base.run([args.hl, "prove", base.PROGRAM], OUT / "prove.log")
    base.run([args.hl, "emit", "--format", "wasm", "--target", "native-jsonl", base.PROGRAM], base.MODULE)
    records = base.compare(args, cases, malformed)
    caught = mutations(base, args, original)
    base.run([args.node, ROOT / "scripts/selfhost-func-execute.mjs", OUT, str(args.runs)], OUT / "emitted-execution.log")
    target_results = probe(base, args, cases)
    bound_results = bounds(base, args)
    evidence.update(status="passed", runtime_runs=args.runs, records=records, mutants_caught=caught,
                    all_target_probe=target_results, bounds=bound_results, emitted_execution=json.loads((OUT / "emitted-execution.json").read_text()),
                    component_sources={name: base.identity((ROOT / "compiler" / name).read_bytes()) for name in ("assemble-lex.hl", "assemble.hl", "encode.hl")},
                    binaries={"hl": base.identity(args.hl.read_bytes()), "host": base.identity(args.host.read_bytes())}, grammar=GRAMMAR)
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"passed N={args.runs} exact flat+module equality both modes; {len(caught)} mutants; all five targets; no bootstrap claims")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, AssertionError) as error:
        print(f"selfhost-func: {error}", file=sys.stderr)
        sys.exit(1)
