import argparse
import ast
import collections
import hashlib
import importlib.util
import json
import pathlib
import re
import shutil
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
ORACLE_HASHES = {
    "assemble": "e9d433c6564d214f603c8b3aec2fc91b6a394506b07e4c800519a03c43f895b1",
    "shake": "16e1ba0e073e7823ce36cb705aec72a64b1497145a6fb8b4d66b046a199965f5",
    "encode": "024a299ce18c6250f304b25b3643ba54cb558e7fbbd958226e26589777b99f27",
    "flat": "c0d0da7c54e42fb9c1dd301574c053e66ee393af2377feab07ab1aa4ec9e8097",
}
RUNS_MIN = 3
ENCODER_WAT_SHA256 = "148575c5ff0a8c7132f6642cd3dc1a1be6664ad54e421d2a02928689010e7425"
ENCODER_WAT_BYTES = 683936
GRAMMAR = {
    "scope": "component projection only: caller supplies retained text/words/global forms, no module wrapper; no reachability or shake; every other top-level form rejects",
    "text": "(text $name string*), decoded lexer bytes, u32 length prefix, align4",
    "words": "(words $name (number|placed-name)*), LEu32, forward references, align4",
    "global": "(global atom (i32|i64|(mut i32)|(mut i64)) ((i32.const|i64.const) (number|placed-name))); no extra fields",
    "strict_global_subset": "Extra mut fields, trailing outer global fields and non-const initializer opcodes are unsupported by this leaf although pinned Rust global() accepts them; do not classify them as Rust-invalid.",
    "alignment_probe_wire": "Three LEu32 fields address/size/alignment. Covers checked-add and alignment overflow within that wire domain, not rejection of address or size inputs wider than u32. Manual expected constants, not Rust-derived oracle values.",
    "numbers": "Rust number: strip initial minus then all underscores, optional lowercase0x, radix parser optional sign; words -2147483648..4294967295; globals -9223372036854775808..18446744073709551615, including i32 globals because unchanged Rust stores i64 init then encoder narrows",
    "symbols": "source order; duplicate placed names fail; reserved $static_end appended last overriding a user $static_end exactly as Rust; final align8; base1024; globals have independent duplicate-name namespace",
    "flat": "empty types/imports/functions, absent memory, globals in source order, no exports, one active static segment offset1024 (even if empty); component bytes need not form an executable Wasm module without combiner memory",
    "wire": "symbol result ok+hex: LEu32 static_end, LEu32 count, repeated LEu32 name_bytes/raw name/LEu32 address; final entry $static_end; form spans ok+hex repeated LEu32 start/end_exclusive token byte offsets",
    "bounds": "existing source4194304, tokens41943040, nesting4096, flat4194304, items65536; reserved symbol consumes one item; all arithmetic checked before alignment/append; source exhaustion remains named runtime trap",
    "runtime_policy": "N>=3 independent clean directories, both interpreter and separately compiled native-jsonl fixed host exact flat+encoded projection bytes vs unchanged Rust layout/global functions; retain failures; deterministic correctness only, no stage1/stage2/full assembler claim",
}


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def identity(data):
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def combined(layout):
    lexer = (ROOT / "compiler/assemble-lex.hl").read_text().split("\non tokenize(")[0]
    encoder = (ROOT / "compiler/encode.hl").read_text().split("\non encode(")[0]
    chunks = ["harness static_layout_projection"]
    for index, source in enumerate((lexer, encoder, layout)):
        chunks.append("\n".join(line for line in source.splitlines() if not line.startswith("harness ") and not (index == 1 and line.startswith("tool "))))
    return "\n\n".join(chunks) + "\n"


def fixtures(out):
    positive = {"empty": "", "space": " \t\r\n", "empty-words": "(words $w)", "empty-text": '(text $t "")',
                "empty-text-fields": "(text $t)", "forward": '(words $w $t $later $static_end -1)(text $t "abc")(words $later $w)',
                "empty-alias": '(words $a)(words $b)(text $c "")', "duplicate-reserved": '(text $static_end "x")(words $w $static_end)',
                "global-heap": '(global $heap (mut i32) (i32.const $static_end))(text $s "abc")',
                "namespaces": '(text $x "")(global $x i64 (i64.const $x))', "text-concat": '(text $t "ab" "" "c")',
                "all-bytes": '(text $t "' + ''.join(f'\\{i:02x}' for i in range(256)) + '")',
                "escapes": r'''(text $t "\n\t\\\"\'\00\7f\ff\+f")''',
                "raw-unicode": '(text $t "éλ🙂")', "raw-controls": '(text $t "\x00\t\r\n")',
                "global-opcode-width": '(global $x i32 (i64.const 18446744073709551615))',
                "reserved-empty": '(words $static_end)(words $w $static_end)'}
    for size in (0, 1, 2, 3, 4, 5, 7, 8, 9):
        positive[f"alignment-{size}"] = f'(text $a "{"x" * size}")(words $w $a $static_end)'
    for value in ('0', '-0', '+1', '--1', '-_1', '_+1', '0x+f', '0x-1', '-0x-1', '0_x_f_f', '0xFFFF_FFFF', '-0x80000000', '-2147483648', '4294967295', '00001'):
        positive[f"word-number-{len(positive)}"] = f'(words $w {value})'
    for ty in ('i32', 'i64'):
        for value in ('-9223372036854775808', '18446744073709551615', '0xffffffffffffffff', '-0x8000000000000000', '4294967296'):
            positive[f"global-{ty}-{len(positive)}"] = f'(global $g {ty} (i64.const {value}))'
    invalid = {"duplicate": '(words $w)(text $w "x")', "unknown": '(words $w $no)', "word-over": '(words $w 4294967296)',
               "word-under": '(words $w -2147483649)', "global-over": '(global $g i64 (i64.const 18446744073709551616))',
               "global-under": '(global $g i64 (i64.const -9223372036854775809))', "empty": '()', "atom": 'x',
               "name": '(text x "x")', "bad-word": '(words $w "x")', "bad-text": '(text $t 1)',
               "duplicate-global": '(global $g i32 (i32.const 0))(global $g i64 (i64.const 0))', "no-init": '(global $g i32)',
               "missing-value": '(global $g i32 (i32.const))', "global-ref": '(global $a i32 (i32.const $b))(global $b i32 (i32.const 0))',
               "hex-prefix-plus": '(words $w +0x1)', "empty-digits": '(words $w _)', "bad-hex": '(words $w 0xg)',
               "bad-escape": r'(text $t "\xz")', "unclosed": '(text $t "x"', "extra-close": '(words $w))'}
    unsupported = {"memory": '(memory 1)', "func": '(func $f)', "import": '(import "m" "x" (func $f))',
                   "data": '(data (i32.const 0) "")', "module": '(module)', "type": '(type $t (func))',
                   "bad-mut": '(global $g (mut i32 i64) (i32.const 0))', "extra": '(global $g i32 (i32.const 0) 1)',
                   "bad-type": '(global $g f32 (f32.const 0))', "init-opcode": '(global $g i32 (anything 0))'}
    expected_strict_globals = {
        "bad-mut": '(global $g (mut i32 i64) (i32.const 0))',
        "extra": '(global $g i32 (i32.const 0) 1)',
    }
    for name, source in expected_strict_globals.items():
        if name in invalid or name in positive or unsupported.get(name) != source:
            raise RuntimeError(f"strict global subset must remain unsupported, not Rust-invalid: {name}")
    if (len(positive), len(invalid), len(unsupported)) != (51, 21, 10):
        raise RuntimeError("layout fixture category count drift; review classification before changing expectations")
    cases = []
    for category, sources in (("positive", positive), ("invalid", invalid), ("unsupported", unsupported)):
        for name, source in sources.items():
            path = out / "fixtures" / f"{category}-{name}.wat"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(source.encode())
            cases.append((path.stem, path, category))
    boundary = out / "fixtures" / "exhausted-static-flat.wat"
    boundary.write_text('(text $t "' + 'x' * (4194304 - 40 + 1) + '")')
    cases.append((boundary.stem, boundary, "exhausted"))
    return cases


def malformed(out):
    def header(kind, line=1, payload=b"", size=None):
        return bytes([kind]) + line.to_bytes(4, "little") + (len(payload) if size is None else size).to_bytes(4, "little") + payload
    opened, closed = header(1), header(2)
    words = header(3, payload=b"words")
    name = header(3, payload=b"$w")
    cases = {
        "short-header": b"\x01", "kind": header(5), "zero-line": header(1, line=0),
        "delimiter-payload": header(1, payload=b"x"), "length": header(3, payload=b"x", size=2),
        "oversize": header(3, size=4294967295), "unclosed": opened + words + name,
        "line-mismatch": opened + words + name + header(2, line=2),
        "empty-atom": opened + words + header(3) + closed, "extra-close": opened + words + name + closed + closed,
    }
    paths = []
    for name, raw in cases.items():
        path = out / "fixtures" / f"malformed-{name}.hex"
        path.write_text(raw.hex())
        paths.append((name, path))
    return paths


def source_string_end(source, start):
    at = start + 1
    simple = b"nt\\\"'"
    hex_digits = b"0123456789abcdefABCDEF"
    for _ in range(len(source)):
        if at >= len(source):
            break
        if source[at] == ord('"'):
            return at + 1
        if source[at] == ord('\\'):
            if at + 1 < len(source) and source[at + 1] in simple:
                at += 2
            elif at + 2 < len(source) and source[at + 1] in hex_digits + b"+" and source[at + 2] in hex_digits:
                at += 3
            else:
                raise RuntimeError("encoder source span lexer: bad escape")
        else:
            at += 1
    raise RuntimeError("encoder source span lexer: unterminated string")


def source_tokens(source):
    at = 0
    whitespace = b" \t\r\n"
    delimiters = whitespace + b'()"'
    for _ in range(len(source)):
        if at == len(source):
            return
        start = at
        byte = source[at]
        if byte in whitespace:
            at += 1
        elif byte in b"()":
            at += 1
            yield ("open" if byte == ord('(') else "close"), start, at
        elif byte == ord('"'):
            at = source_string_end(source, at)
            yield "text", start, at
        else:
            for _ in range(len(source) - at):
                if at == len(source) or source[at] in delimiters:
                    break
                at += 1
            yield "atom", start, at
    if at != len(source):
        raise RuntimeError("encoder source span lexer exhausted input-shaped bound")


def source_form_spans(source, nesting_max):
    stack = []
    forms = []
    head = None
    start = 0
    for kind, token_start, token_end in source_tokens(source):
        if not stack:
            if kind != "open":
                raise RuntimeError("encoder projection: top-level list required")
            start, head = token_start, None
        elif len(stack) == 1 and head is None and kind != "atom":
            raise RuntimeError("encoder projection: atom head required")
        if kind == "open":
            if len(stack) == nesting_max:
                raise RuntimeError("encoder projection: nesting_max")
            stack.append(token_start)
        elif kind == "close":
            if not stack:
                raise RuntimeError("encoder projection: unexpected close")
            stack.pop()
            if not stack:
                if head is None:
                    raise RuntimeError("encoder projection: empty top-level form")
                forms.append({"head": head, "start": start, "end_exclusive": token_end})
        elif len(stack) == 1 and head is None:
            if kind != "atom":
                raise RuntimeError("encoder projection: atom head required")
            head = source[token_start:token_end].decode("ascii")
    if stack:
        raise RuntimeError("encoder projection: incomplete form")
    return forms


def encoder_projection(args, out):
    if args.encoder_wat is None:
        return None
    if not args.encoder_wat.is_absolute():
        raise RuntimeError("--encoder-wat must be an absolute path")
    if args.encoder_wat.stat().st_size != ENCODER_WAT_BYTES:
        raise RuntimeError("encoder WAT must match the frozen original byte length before reading")
    source = args.encoder_wat.read_bytes()
    actual = identity(source)
    if actual != {"bytes": ENCODER_WAT_BYTES, "sha256": args.encoder_sha256} or args.encoder_sha256 != ENCODER_WAT_SHA256:
        raise RuntimeError("encoder WAT must match the explicit frozen original hash and size")
    bounds = dict(re.findall(r"const (\w+): u64 = (\d+)", (ROOT / "compiler/assemble-lex.hl").read_text()))
    if len(source) > int(bounds["source_bytes_max"]):
        raise RuntimeError("encoder projection: source_bytes_max")
    forms = source_form_spans(source, int(bounds["nesting_max"]))
    counts = dict(collections.Counter(form["head"] for form in forms))
    if counts != {"func": 826, "global": 376, "text": 766, "words": 68, "import": 11, "memory": 1}:
        raise RuntimeError(f"encoder source form inventory drift: {counts}")
    retained = [form for form in forms if form["head"] in ("text", "words", "global")]
    projection = b"\n".join(source[form["start"]:form["end_exclusive"]] for form in retained) + b"\n"
    path = out / "fixtures" / "positive-encoder-retained-static.wat"
    path.write_bytes(projection)
    manifest = {"method": "bounded source token spans, escaped strings consumed atomically; exact source slices, no grep, no shake/reachability", "source_path": str(args.encoder_wat), "source": actual,
                "source_form_counts": counts, "retained_form_counts": dict(collections.Counter(form["head"] for form in retained)), "retained_total": len(retained),
                "projection": identity(projection), "retained_spans": retained, "coverage_claim": "source extraction only until corresponding runtime matrix records pass", "scope": "raw pre-shake text/words/global component projection only, not full module/stdlib/compiler/stage2"}
    (out / "encoder-projection.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return path, {key: value for key, value in manifest.items() if key != "retained_spans"}


def static(out):
    for name, expected in ORACLE_HASHES.items():
        if identity((ROOT / f"src/emit/wasm/{name}.rs").read_bytes())["sha256"] != expected:
            raise RuntimeError(f"frozen Rust {name} changed; never re-bless oracle")
    layout = (ROOT / "compiler/assemble-layout.hl").read_text()
    declarations = re.findall(r'^(?:const|fn|on) (\w+)', layout, re.M)
    if not all(name.startswith("static_layout") for name in declarations):
        raise RuntimeError("layout declaration namespace")
    generated = combined(layout)
    named = re.findall(r'^(fn|on|const|tool) (\w+)', generated, re.M)
    if len(named) != len(set(named)):
        raise RuntimeError("duplicate combined declaration")
    for expected in ('const static_layout_flat_overhead: u64 = 40', 'const static_layout_long_bytes: u64 = 8',
                     'const static_layout_global_bytes: u64 = 10', 'const static_layout_end_align_bytes: u64 = 8'):
        if expected not in layout:
            raise RuntimeError(f"wire derivation changed: {expected}")
    assert 8 * 4 + 2 * 4 == 40
    assert 2 * 4 == 8
    assert 1 + 1 + 8 == 10
    if ';' in layout or re.search(r'^\s*(//|/\*)', layout, re.M):
        raise RuntimeError("HL comments or semicolon")
    (out / "component.hl").write_text(generated)
    (out / "grammar.json").write_text(json.dumps(GRAMMAR, indent=2) + "\n")
    ast.parse(pathlib.Path(__file__).read_text())
    return layout


def interpreter(base, args, program, handler, path, directory):
    directory.mkdir(parents=True, exist_ok=True)
    log = directory / "interpreter.jsonl"
    base.run([args.hl, "run", program, "--handler", handler, "--arg-file", path], log)
    return base.finish(log)


def compiled(base, args, module, handler, path, directory):
    directory.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(path, directory / "input.txt")
    output = directory / "result.out"
    base.run([args.host, "run", "--module", module, "--journal", directory / "journal", "--cwd", directory,
              "--handler", handler, "--args", '{"input_path":"input.txt","output_path":"result.out"}'], directory / "host.log")
    result = base.finish(directory / "journal/journal.jsonl")
    return output.read_bytes() if output.exists() else None, result


def prepare(base, args, program, module, directory):
    base.run([args.hl, "check", program], directory / "check.log")
    base.run([args.hl, "prove", program], directory / "prove.log")
    base.run([args.hl, "emit", "--format", "wasm", "--target", "native-jsonl", program], module)


def matrix(base, args, out, cases, oracle, program, module):
    records = []
    for repeat in range(args.runs):
        for name, path, category in cases:
            directory = out / "runs" / str(repeat) / name
            value = interpreter(base, args, program, "static_layout", path, directory)
            encoded_value = interpreter(base, args, program, "static_layout_encode", path, directory / "encoded-interpreter")
            actual, error = compiled(base, args, module, "static_layout_flatten", path, directory / "flat")
            encoded, encode_error = compiled(base, args, module, "static_layout_compile", path, directory / "encode")
            if category == "positive":
                flat_path, wasm_path = directory / "rust.flat", directory / "rust.wasm"
                base.run([oracle, path, flat_path, wasm_path], directory / "oracle.log")
                expected = flat_path.read_bytes()
                if not value.startswith("ok ") or bytes.fromhex(value[3:]) != expected or actual != expected or encoded != wasm_path.read_bytes() or not encoded_value.startswith("ok ") or bytes.fromhex(encoded_value[3:]) != wasm_path.read_bytes():
                    raise RuntimeError(f"exact component parity failed: {directory}")
                (directory / "interpreter.flat").write_bytes(bytes.fromhex(value[3:]))
                (directory / "interpreter.wasm").write_bytes(bytes.fromhex(encoded_value[3:]))
                record = {"flat": identity(expected), "wasm": identity(encoded)}
            else:
                if category == "invalid":
                    invalid_oracle = base.run([oracle, path, directory / "invalid.flat", directory / "invalid.wasm"], directory / "invalid-oracle.log", accepted=(0, 1))
                    if invalid_oracle.returncode == 0:
                        raise RuntimeError(f"invalid fixture accepted by frozen Rust: {directory}")
                if value.startswith("ok ") or encoded_value != value or actual is not None or encoded is not None or error != value or encode_error != value:
                    raise RuntimeError(f"failclosed component failed: {directory}")
                record = {"rejection": value}
            records.append(dict(record, repeat=repeat + 1, case=name, category=category, source=identity(path.read_bytes())))
    return records


def mutations(base, args, out, layout, cases):
    old = 'const static_layout_end_align_bytes: u64 = 8'
    if layout.count(old) != 1:
        raise RuntimeError("mutation anchor drift")
    directory = out / "mutation-align4"
    directory.mkdir()
    program, module = directory / "mutant.hl", directory / "mutant.wasm"
    program.write_text(combined(layout.replace(old, 'const static_layout_end_align_bytes: u64 = 4')))
    prepare(base, args, program, module, directory)
    path = next(path for name, path, _ in cases if name == "positive-empty-text")
    expected_flat, expected_wasm = directory / "rust.flat", directory / "rust.wasm"
    base.run([out / "oracle", path, expected_flat, expected_wasm], directory / "oracle.log")
    caught = []
    for repeat in range(args.runs):
        run_dir = directory / str(repeat)
        value = interpreter(base, args, program, "static_layout", path, run_dir)
        flat, result = compiled(base, args, module, "static_layout_flatten", path, run_dir / "flat")
        if not value.startswith("ok ") or flat is None or flat != bytes.fromhex(value[3:]) or flat == expected_flat.read_bytes():
            raise RuntimeError(f"align4 mutant not caught in both modes: {run_dir}")
        caught.append({"repeat": repeat + 1, "mutant": identity(flat), "oracle": identity(expected_flat.read_bytes())})
    return caught


def token_matrix(base, args, out, program, module):
    records = []
    for repeat in range(args.runs):
        for name, path in malformed(out):
            directory = out / "tokens" / str(repeat) / name
            value = interpreter(base, args, program, "static_layout_tokens", path, directory)
            actual, status = compiled(base, args, module, "static_layout_token_compile", path, directory / "host")
            if value.startswith("ok ") or actual is not None or status != value:
                raise RuntimeError(f"malformed token accepted: {directory}")
            records.append({"repeat": repeat + 1, "case": name, "rejection": value})
    return records


def alignment_matrix(base, args, out, layout):
    directory = out / "alignment"
    directory.mkdir()
    program, module = directory / "probe.hl", directory / "probe.wasm"
    probe = """
fn static_layout_probe(input: text) -> text {
  let fields: bytes<tokens_bytes_max> = bytes_of_hex(input)
  if len(fields) != word_bytes * 3 { return "invalid" }
  let value = static_layout_align(static_layout_word(fields, 0), static_layout_word(fields, word_bytes), static_layout_word(fields, word_bytes * 2))
  if value == static_layout_invalid { return "invalid" }
  return "ok " ++ decimal(value)
}

on static_layout_probe(input: text) -> text {
  return static_layout_probe(input)
}

on static_layout_probe_compile(input_path: text, output_path: text) -> text {
  match read_file(input_path) {
    ok(source) => {
      match write_bytes(output_path, hex_result(static_layout_probe(source))) {
        ok(written) => { return written }
        failed(error) => { return error }
      }
    }
    failed(error) => { return error }
  }
}
"""
    program.write_text(combined(layout) + probe)
    prepare(base, args, program, module, directory)
    cases = ((1024, 0, 8, "ok 1024"), (1024, 4, 8, "ok 1032"), (1024, 5, 4, "ok 1032"),
             (4294967295, 1, 4, "invalid"), (4294967295, 0, 4, "invalid"),
             (4294967292, 0, 8, "invalid"), (4294967292, 0, 4, "ok 4294967292"), (1024, 0, 3, "invalid"))
    records = []
    for repeat in range(args.runs):
        for index, (address, size, alignment, expected) in enumerate(cases):
            path = directory / f"case-{index}.hex"
            path.write_text(b"".join(value.to_bytes(4, "little") for value in (address, size, alignment)).hex())
            run_dir = directory / str(repeat) / str(index)
            value = interpreter(base, args, program, "static_layout_probe", path, run_dir)
            actual, status = compiled(base, args, module, "static_layout_probe_compile", path, run_dir / "host")
            if value != expected or actual != expected.encode():
                raise RuntimeError(f"alignment/overflow primitive mismatch: {run_dir}")
            records.append({"repeat": repeat + 1, "address": address, "size": size, "alignment": alignment, "expected": expected})
    return records


def main():
    parser = argparse.ArgumentParser(description="Static layout component projection, NOT full assembler/selfhosting. Runtime forbidden without explicit root allocation.")
    parser.add_argument("--static", action="store_true")
    parser.add_argument("--allocated", action="store_true")
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--hl", type=pathlib.Path)
    parser.add_argument("--host", type=pathlib.Path)
    parser.add_argument("--encoder-wat", type=pathlib.Path, help="optional absolute path to the frozen original encoder WAT; no default artifact dependency")
    parser.add_argument("--encoder-sha256", help="explicit recorded hash required with --encoder-wat")
    parser.add_argument("--runs", type=int, default=RUNS_MIN)
    parser.add_argument("--command-seconds-max", type=int, default=300)
    parser.add_argument("--command-output-bytes-max", type=int, default=16777216)
    args = parser.parse_args()
    if not args.static and not args.allocated:
        parser.error("runtime needs --allocated granted by root")
    if args.runs < RUNS_MIN or args.command_seconds_max <= 0 or args.command_output_bytes_max <= 0:
        parser.error("runs>=3 and positive caller command bounds required")
    if (args.encoder_wat is None) != (args.encoder_sha256 is None):
        parser.error("--encoder-wat and --encoder-sha256 must be supplied together")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    evidence = {"status": "static-only", "runtime_runs": 0, "stage1_runs": 0, "stage2_runs": 0, "bootstrap_runs": 0, "scope": GRAMMAR}
    evidence_path = out / "evidence.json"
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    layout = static(out)
    cases = fixtures(out)
    optional = encoder_projection(args, out)
    if optional is not None:
        path, manifest = optional
        cases.append((path.stem, path, "positive"))
        evidence["encoder_projection"] = manifest
    evidence["fixture_counts"] = {category: sum(c == category for _, _, c in cases) for category in ("positive", "invalid", "unsupported", "exhausted")}
    evidence["classification_checks"] = "static assertions only: bad-mut and extra pinned as strict-leaf unsupported, not Rust-invalid; frozen Rust oracle NOT executed by --static"
    evidence["oracle_hashes"] = ORACLE_HASHES
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    if args.static:
        print(json.dumps({"status": "static-ready", "fixtures": {category: sum(c == category for _, _, c in cases) for category in ("positive", "invalid", "unsupported", "exhausted")}, "malformed": len(malformed(out)), "runtime_runs": 0}))
        return
    if args.hl is None or args.host is None or not all(p.is_file() for p in (args.hl, args.host)):
        parser.error("runtime requires explicit prebuilt --hl and --host")
    evidence["status"] = "runtime-incomplete"
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    base = load("layout_base", ROOT / "scripts/selfhost-lex-assemble.py")
    runner = load("layout_bounded_runner", ROOT / "scripts/selfhost-func.py")
    base.run = lambda arguments, log, accepted=(0,): runner.bounded_run(arguments, log, accepted, args.command_seconds_max, args.command_output_bytes_max)
    oracle, program, module = out / "oracle", out / "component.hl", out / "component.wasm"
    base.run(["rustc", "--edition=2024", "-O", ROOT / "scripts/selfhost-layout-oracle.rs", "-o", oracle], out / "oracle-build.log")
    prepare(base, args, program, module, out)
    records = matrix(base, args, out, cases, oracle, program, module)
    token_records = token_matrix(base, args, out, program, module)
    alignment_records = alignment_matrix(base, args, out, layout)
    caught = mutations(base, args, out, layout, cases)
    evidence.update(status="passed-component-only", runtime_runs=args.runs, records=records, malformed_tokens=token_records, alignment_overflow=alignment_records, mutants_caught=caught,
                    oracle_hashes=ORACLE_HASHES, sources={p: identity((ROOT / p).read_bytes()) for p in ("compiler/assemble-layout.hl", "compiler/assemble-lex.hl", "compiler/encode.hl", "scripts/selfhost-layout-oracle.rs")},
                    binaries={"hl": identity(args.hl.read_bytes()), "host": identity(args.host.read_bytes())})
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps({"status": evidence["status"], "records": len(records), "runs": args.runs, "mutants_caught": len(caught)}))


if __name__ == "__main__":
    main()
