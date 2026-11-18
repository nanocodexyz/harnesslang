import argparse
import ast
import hashlib
import importlib.util
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CATALOG_SOURCE = "compiler/assemble-catalog.hl"
CATALOG_SHA256 = "8199ff2498ec4d7e3fda2696811a07d0bc58e845d1fb947ee7ff1191b0e29180"
RUST_HASHES = {
    "assemble": "e9d433c6564d214f603c8b3aec2fc91b6a394506b07e4c800519a03c43f895b1",
    "shake": "16e1ba0e073e7823ce36cb705aec72a64b1497145a6fb8b4d66b046a199965f5",
    "encode": "024a299ce18c6250f304b25b3643ba54cb558e7fbbd958226e26589777b99f27",
    "flat": "c0d0da7c54e42fb9c1dd301574c053e66ee393af2377feab07ab1aa4ec9e8097",
}
ENCODER_SHA256 = "148575c5ff0a8c7132f6642cd3dc1a1be6664ad54e421d2a02928689010e7425"
SOURCE_BYTES_MAX = 4194304
NESTING_MAX = 4096
CONTRACT = {
    "catalog_dependency": "Exact raw source SHA256 is authoritative; the component path and observed Git HEAD are optional provenance. --catalog-root explicitly accepts a same-tree integration or a .git-less source package with identical component bytes.",
    "input": "expr_body(canonical tokens, validated catalog v1 function_record); expr_selected(tokens,valid_catalog,name); all arena inputs/results bytes<tokens_bytes_max> exact nominal capacity",
    "entry": "expr_source/expr_parse handler selects $f; expr_named_source accepts explicit symbol; expr_compile writes binary bytes only on success",
    "output": "ok HEX raw instruction bytes only. NO local declarations vector and NO implicit final end. No type/stack validation or evaluation.",
    "grammar": "Existing16 spellings preserved plus51 integer scalar spellings (67 total): both widths clz/ctz/popcnt, remainders, bitwise, shifts/rotates, comparisons/eqz, wrap/extend. Flat sequences mixed with folded lists; each folded head consumes its sole immediate if any, then zero or more folded children in source order; child arity follows Rust byte assembler, not Wasm validation. Named local.get/local.set, grouped/repeated i32/i64 params/locals via frozen catalog; duplicate named locals last-wins.",
    "constants": "Existing decimal subset only: optional minus, i32 -2147483648..4294967295 and i64 -9223372036854775808..18446744073709551615; unchanged assembly_immediate, unsigned/signed/widened helpers. No hex, plus, underscore, symbols, floats.",
    "unsupported": "Control/branches/calls/globals/memory/floats/local.tee/select and every spelling outside the explicit67 failclosed, only the selected function body is inspected; other catalog bodies remain opaque. Not shake/reachability/full assembler/stage1/stage2/bootstrap.",
    "bounds": {"source_bytes_max": SOURCE_BYTES_MAX, "tokens_bytes_max": 41943040, "nesting_max": NESTING_MAX, "locals_max": 65536, "body_bytes_max": SOURCE_BYTES_MAX, "frame_words": 6, "frame_bytes": 24, "stack_bytes_max": 98304, "single_instruction_bytes_max": 11},
    "frame": "six LEu32 phase(children), opcode, immediate-token-byte-offset, open-line, children-visited, next-token-byte-offset. Head/immediate consumed before frame push; child postorder completion emits opcode+immediate on close. Source-order traversal never reverses operand order.",
    "visits": "each cursor iteration consumes at least one canonical token; bound tokens_bytes_max conservatively exceeds max floor(tokens_bytes_max/header_bytes) visits. Every frame pop removes24 bytes; each open at most nesting4096 (outer function also counted by catalog). Parent child visits <= floor(token bytes/9). Max frame stack4096*24=98304 < nominal tokens41943040. Line/offset values already checked by catalog and fit u32.",
    "failure_kinds": "expr invalid/unsupported for external grammar; expr bound for policy; expr invariant for internal constructed-wire contract violations (language has no assertion/panic statement; no new language features introduced). Source overflow keeps existing named runtime trap.",
    "oracle": "Pinned original Rust tokenize/header/function directly, bypassing shake and reachable for selected raw-body projection; production function emits then wrapper removes precisely one final OP_END. Complete oracle module wraps actual production Function with its params/locals and export f; no HL parser algorithm reimplemented in oracle.",
    "scope": "Only selected raw body projection. Static checks are Python AST/source concatenation/arithmetic, NOT HL check/prove/parity. Runtime only with explicit allocation, N>=3 fresh Rust/interpreter/compiled fixed-host runs; no build implicit in static.",
}
EXPECTED_PROJECTIONS = {
    "empty": "",
    "operand-order": "411341046b",
    "named-locals": "200041046b21032003",
    "mixed": "200042077d42027f",
    "depth-three": "4117410241046c6b",
    "wrapped-i32": "417f",
    "wrapped-i64": "427f",
    "encoder-text-bytes": "200041046a",
    "comparison": "4100410046",
    "encoder-value-text": "2000a7",
    "encoder-text-value": "2000ad",
    "encoder-list-buffer": "2000422088a7",
    "encoder-list-pack": "2000ad4220862001ad84",
    "encoder-w-blank": "2000412046200041094672",
    "encoder-pair-before": "2000200254200020025120012003547172",
}
MUTANTS = {
    "backwards-operands": ("      push body instruction", "      var reversed: bytes<tokens_bytes_max> = []\n      push reversed instruction\n      push reversed body\n      body = reversed", "operand-order"),
    "signed-i32": ("return assembly_immediate(payload, opcode, 0)", "return assembly_immediate(payload, assembly_i64_constant, 0)", "wrapped-i32"),
    "named-index": ("index = catalog_local_index(record, payload)", "index = catalog_local_index(record, payload) + 1", "named-locals"),
    "opcode-mis-map": ('assembly_matches(payload, "i32.rem_s") { return 111 }', 'assembly_matches(payload, "i32.rem_s") { return 112 }', "scalar-i32-rem_s-folded"),
    "operator-width": ('assembly_matches(payload, "i64.eqz") { return 80 }', 'assembly_matches(payload, "i64.eqz") { return 69 }', "scalar-i64-eqz-folded"),
    "depth": ("len(frames) / expr_frame_bytes == nesting_max", "len(frames) / expr_frame_bytes == 1", "depth-three"),
}


SCALAR_CASES = [
    ('i32.eqz', 69, 'i32', 'i32', (-1,), 0),
    ('i32.eq', 70, 'i32', 'i32', (-21, 3), 0),
    ('i32.ne', 71, 'i32', 'i32', (-21, 3), 1),
    ('i32.lt_s', 72, 'i32', 'i32', (-21, 3), 1),
    ('i32.lt_u', 73, 'i32', 'i32', (-21, 3), 0),
    ('i32.gt_s', 74, 'i32', 'i32', (-21, 3), 0),
    ('i32.gt_u', 75, 'i32', 'i32', (-21, 3), 1),
    ('i32.le_s', 76, 'i32', 'i32', (-21, 3), 1),
    ('i32.le_u', 77, 'i32', 'i32', (-21, 3), 0),
    ('i32.ge_s', 78, 'i32', 'i32', (-21, 3), 0),
    ('i32.ge_u', 79, 'i32', 'i32', (-21, 3), 1),
    ('i32.clz', 103, 'i32', 'i32', (-1,), 0),
    ('i32.ctz', 104, 'i32', 'i32', (-1,), 0),
    ('i32.popcnt', 105, 'i32', 'i32', (-1,), 32),
    ('i32.rem_s', 111, 'i32', 'i32', (-21, 3), 0),
    ('i32.rem_u', 112, 'i32', 'i32', (-21, 3), 1),
    ('i32.and', 113, 'i32', 'i32', (-21, 3), 3),
    ('i32.or', 114, 'i32', 'i32', (-21, 3), -21),
    ('i32.xor', 115, 'i32', 'i32', (-21, 3), -24),
    ('i32.shl', 116, 'i32', 'i32', (-21, 3), -168),
    ('i32.shr_s', 117, 'i32', 'i32', (-21, 3), -3),
    ('i32.shr_u', 118, 'i32', 'i32', (-21, 3), 536870909),
    ('i32.rotl', 119, 'i32', 'i32', (-21, 3), -161),
    ('i32.rotr', 120, 'i32', 'i32', (-21, 3), 2147483645),
    ('i64.eqz', 80, 'i64', 'i32', (-1,), 0),
    ('i64.eq', 81, 'i64', 'i32', (-21, 3), 0),
    ('i64.ne', 82, 'i64', 'i32', (-21, 3), 1),
    ('i64.lt_s', 83, 'i64', 'i32', (-21, 3), 1),
    ('i64.lt_u', 84, 'i64', 'i32', (-21, 3), 0),
    ('i64.gt_s', 85, 'i64', 'i32', (-21, 3), 0),
    ('i64.gt_u', 86, 'i64', 'i32', (-21, 3), 1),
    ('i64.le_s', 87, 'i64', 'i32', (-21, 3), 1),
    ('i64.le_u', 88, 'i64', 'i32', (-21, 3), 0),
    ('i64.ge_s', 89, 'i64', 'i32', (-21, 3), 0),
    ('i64.ge_u', 90, 'i64', 'i32', (-21, 3), 1),
    ('i64.clz', 121, 'i64', 'i64', (-1,), 0),
    ('i64.ctz', 122, 'i64', 'i64', (-1,), 0),
    ('i64.popcnt', 123, 'i64', 'i64', (-1,), 64),
    ('i64.rem_s', 129, 'i64', 'i64', (-21, 3), 0),
    ('i64.rem_u', 130, 'i64', 'i64', (-21, 3), 1),
    ('i64.and', 131, 'i64', 'i64', (-21, 3), 3),
    ('i64.or', 132, 'i64', 'i64', (-21, 3), -21),
    ('i64.xor', 133, 'i64', 'i64', (-21, 3), -24),
    ('i64.shl', 134, 'i64', 'i64', (-21, 3), -168),
    ('i64.shr_s', 135, 'i64', 'i64', (-21, 3), -3),
    ('i64.shr_u', 136, 'i64', 'i64', (-21, 3), 2305843009213693949),
    ('i64.rotl', 137, 'i64', 'i64', (-21, 3), -161),
    ('i64.rotr', 138, 'i64', 'i64', (-21, 3), 9223372036854775805),
    ('i32.wrap_i64', 167, 'i64', 'i32', (-4294967297,), -1),
    ('i64.extend_i32_s', 172, 'i32', 'i64', (-21,), -21),
    ('i64.extend_i32_u', 173, 'i32', 'i64', (-21,), 4294967275),
]
SCALAR_EDGES = [
    ("i32-eqz-zero", "i32.eqz", "i32", "i32", (0,), 1, "410045"),
    ("i64-eqz-zero", "i64.eqz", "i64", "i32", (0,), 1, "420050"),
    ("i32-clz-zero", "i32.clz", "i32", "i32", (0,), 32, "410067"),
    ("i64-ctz-zero", "i64.ctz", "i64", "i64", (0,), 64, "42007a"),
    ("wrap-min", "i32.wrap_i64", "i64", "i32", (-9223372036854775808,), 0, "428080808080808080807fa7"),
    ("extend-min-s", "i64.extend_i32_s", "i32", "i64", (-2147483648,), -2147483648, "418080808078ac"),
    ("extend-min-u", "i64.extend_i32_u", "i32", "i64", (-2147483648,), 2147483648, "418080808078ad"),
    ("i32-shift-mask", "i32.shr_u", "i32", "i32", (-1, 32), -1, "417f412076"),
    ("i64-shift-mask", "i64.shr_u", "i64", "i64", (-1, 64), -1, "427f42c00088"),
]
SCALAR_OPERAND_BYTES = {
    ("i32", (-1,)): "417f", ("i64", (-1,)): "427f",
    ("i32", (-21, 3)): "416b4103", ("i64", (-21, 3)): "426b4203",
    ("i64", (-4294967297,)): "42ffffffff6f", ("i32", (-21,)): "416b",
}


def scalar_forms(op, input_type, result_type, values):
    folded = [f"({input_type}.const {value})" for value in values]
    flat = [f"{input_type}.const {value}" for value in values]
    bodies = {"folded": f"({op} {' '.join(folded)})", "flat": f"{' '.join(flat)} {op}", "mixed": f"{flat[0]} {' '.join(folded[1:])} {op}"}
    return {style: f"(func $f (result {result_type}) {body})" for style, body in bodies.items()}


def scalar_fixtures():
    cases = []
    for op, code, input_type, result_type, values, expected in SCALAR_CASES:
        projection = SCALAR_OPERAND_BYTES[input_type, values] + f"{code:02x}"
        for style, source in scalar_forms(op, input_type, result_type, values).items():
            name = f"scalar-{op.replace('.', '-')}-{style}"
            cases.append((name, source, "positive"))
            EXPECTED_PROJECTIONS[name] = projection
    for label, op, input_type, result_type, values, expected, projection in SCALAR_EDGES:
        for style, source in scalar_forms(op, input_type, result_type, values).items():
            name = f"edge-{label}-{style}"
            cases.append((name, source, "positive"))
            EXPECTED_PROJECTIONS[name] = projection
    return cases


def scalar_static(component):
    mapping = {op: int(code) for op, code in re.findall(r'assembly_matches\(payload, "([^"]+)"\) \{ return (\d+) \}', component)}
    expected = {op: code for op, code, *_ in SCALAR_CASES}
    if len(expected) != 51 or any(mapping.get(op) != code for op, code in expected.items()):
        raise RuntimeError("explicit scalar mappings differ from independent fixture bytes")
    if len(scalar_fixtures()) != 180:
        raise RuntimeError("scalar fixture inventory changed")
    return {"new_spellings": len(expected), "total_spellings": 67, "new_fixture_count": 180, "method": "literal byte/result fixtures, not a translated Rust parser or runtime parity", "opcodes": expected}


def identity(data):
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def load_runner():
    spec = importlib.util.spec_from_file_location("expr_runner", ROOT / "scripts/selfhost-func.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def combined(component, catalog):
    pieces = ["harness expr_subset\n"]
    for path, marker in ((ROOT / "compiler/assemble-lex.hl", "\non tokenize("), (ROOT / "compiler/encode.hl", "\non encode("), (ROOT / "compiler/assemble.hl", "\non assemble(")):
        source = path.read_text().split(marker)[0]
        pieces.append("\n".join(line for line in source.splitlines() if not line.startswith("harness ") and (path.name == "assemble-lex.hl" or not line.startswith("tool "))))
    pieces.append("\n".join(line for line in catalog.split("\non catalog(")[0].splitlines() if not line.startswith("harness ")))
    pieces.append("\n".join(line for line in component.splitlines() if not line.startswith("harness ")))
    return "\n\n".join(pieces) + "\n"


def fixtures():
    positive = {
        "empty": '(func $f)',
        "operand-order": '(func $f (export "f") (result i32) (i32.sub (i32.const 19) (i32.const 4)))',
        "named-locals": '(func $f (param $x i32) (result i32) (local i64 i64) (local $y i32) (local.set $y (i32.sub (local.get $x) (i32.const 4))) (local.get $y))',
        "mixed": '(func $f (param $x i64) (result i64) local.get $x (i64.const 7) i64.sub (i64.const 2) i64.div_s)',
        "depth-three": '(func $f (result i32) (i32.sub (i32.const 23) (i32.mul (i32.const 2) (i32.const 4))))',
        "duplicate-name": '(func $f (param $x i32) (result i32) (local $x i32) (local.set $x (i32.const 42)) (local.get $x))',
        "interleaved-headers": '(func $f (local $t i32) (param $x i32) (result i32) (local.set $t (local.get $x)) (local.get $t))',
        "flat-set": '(func $f (result i32) (local i32) i32.const 42 local.set 0 local.get 0)',
        "local-index-leb": '(func $f (param ' + ' '.join(['i32'] * 129) + ') (result i32) (local.get 128))',
        "nop-drop": '(func $f (nop) (drop (i32.const 1)))',
        "wrapped-i32": '(func $f (result i32) (i32.const 4294967295))',
        "wrapped-i64": '(func $f (result i64) (i64.const 18446744073709551615))',
        "rust-raw-not-stack-validator": '(func $f (i32.const 1 (nop)))',
        "catalog-selects-not-full-assembler": '(func $other unknown)(func $f (nop))',
    }
    for ty, values in (("i32", (-2147483648, -65, -64, -1, 0, 63, 64, 127, 128, 2147483647, 2147483648)), ("i64", (-9223372036854775808, -2147483649, -65, -64, -1, 0, 63, 64, 127, 128, 9223372036854775807, 9223372036854775808))):
        for value in values:
            positive[f"{ty}-{value}"] = f'(func $f (result {ty}) ({ty}.const {value}))'
        for op in ("add", "sub", "mul", "div_s", "div_u"):
            positive[f"{ty}-{op}"] = f'(func $f (result {ty}) ({ty}.{op} ({ty}.const 21) ({ty}.const 3)))'
    positive["comparison"] = '(func $f (i32.eq (i32.const 0) (i32.const 0)))'
    positive["depth-at-policy"] = '(func $f ' + '(nop ' * (NESTING_MAX - 1) + ')' * NESTING_MAX
    invalid = {
        "extra-immediate": '(func $f (i32.const 1 2))',
        "bare-child": '(func $f (i32.add i32.const 1))',
        "atom-after-child": '(func $f (i32.add (i32.const 1) 2))',
        "empty-fold": '(func $f ())',
        "missing-immediate": '(func $f (local.get))',
        "flat-missing": '(func $f i64.const)',
        "unknown-name": '(func $f (param i32) (local.get $absent))',
        "negative-local": '(func $f (param i32) (local.get -0))',
        "local-past-end": '(func $f (param i32) (local i64) (local.set 2))',
        "named-group": '(func $f (local $x i32 i64))',
        "immediate-folded": '(func $f (i32.const (i32.const 1)))',
        "const-over32": '(func $f (i32.const 4294967296))',
        "const-under32": '(func $f (i32.const -2147483649))',
        "const-over64": '(func $f (i64.const 18446744073709551616))',
        "const-under64": '(func $f (i64.const -9223372036854775809))',
        "depth-over-policy": '(func $f ' + '(nop ' * NESTING_MAX + ')' * (NESTING_MAX + 1),
    }
    unsupported = {
        "negative-local": invalid.pop("negative-local"),
        "local-past-end": invalid.pop("local-past-end"),
        "call": '(func $f (call $f))',
        "control": '(func $f (block (nop)))',
        "float": '(func $f (f32.const 1))',
        "memory": '(func $f (i32.load (i32.const 0)))',
        "unknown-integer-op": '(func $f (i32.abs (i32.const 0)))',
        "local-tee": '(func $f (local i32) (local.tee 0 (i32.const 0)))',
        "hex-subset": '(func $f (i32.const 0x10))',
        "return": '(func $f return)',
    }
    if "negative-local" in invalid or "local-past-end" in invalid:
        raise RuntimeError("Rust permits -0/u32 local immediates outside declared locals; strict leaf only")
    if unsupported["negative-local"] != '(func $f (param i32) (local.get -0))' or unsupported["local-past-end"] != '(func $f (param i32) (local i64) (local.set 2))':
        raise RuntimeError("strict local-index policy classifications changed")
    return [(name, source, category) for category, group in (("positive", positive), ("invalid", invalid), ("unsupported", unsupported)) for name, source in group.items()] + scalar_fixtures()


def top_spans(source):
    stack = []
    spans = []
    start = 0
    at = 0
    while at < len(source):
        ch = source[at]
        if ch == '"':
            at += 1
            while at < len(source) and source[at] != '"':
                at += 2 if source[at] == '\\' else 1
            if at == len(source):
                raise RuntimeError("unterminated fixture string")
        elif ch == '(':
            if not stack:
                start = at
            stack.append(at)
            if len(stack) > NESTING_MAX:
                raise RuntimeError("fixture nesting bound")
        elif ch == ')':
            if not stack:
                raise RuntimeError("fixture unmatched close")
            stack.pop()
            if not stack:
                spans.append((start, at + 1))
        at += 1
    if stack:
        raise RuntimeError("fixture unclosed form")
    return spans


def encoder_cases(args, out):
    if args.encoder_wat is None:
        return []
    if not args.encoder_wat.is_absolute() or args.encoder_wat.stat().st_size != 683936:
        raise RuntimeError("encoder fixture needs absolute path and frozen683936 byte length")
    raw = args.encoder_wat.read_bytes()
    if identity(raw)["sha256"] != ENCODER_SHA256:
        raise RuntimeError("encoder fixture pinned hash mismatch")
    source = raw.decode()
    spans = top_spans(source)
    forms = [source[start:end] for start, end in spans]
    functions = {re.match(r'\(func\s+(\S+)', form).group(1): form for form in forms if form.startswith('(func ')}
    if len(forms) != 2048 or len(functions) != 826:
        raise RuntimeError("frozen source inventory differs")
    selected = [("encoder-text-bytes", "$text_bytes", "positive"), ("encoder-text-new-unsupported", "$text_new", "unsupported")]
    selected += [("encoder-value-text", "$value_text", "positive"), ("encoder-text-value", "$text_value", "positive"), ("encoder-list-buffer", "$list_buffer", "positive"), ("encoder-list-pack", "$list_pack", "positive"), ("encoder-w-blank", "$w_blank", "positive"), ("encoder-pair-before", "$pair_before", "positive")]
    manifest = {"source": identity(raw), "forms": len(forms), "functions": len(functions), "folded_functions_inventory33": 801, "opcode_spellings_inventory33": 69, "selections": []}
    result = []
    for label, symbol, category in selected:
        form = functions[symbol]
        renamed = form.replace(symbol, "$f", 1)
        manifest["selections"].append({"fixture": label, "symbol": symbol, "original": identity(form.encode()), "renamed": identity(renamed.encode()), "transformation": "only selected function declaration symbol renamed to $f; exact header/body source preserved", "category": category})
        result.append((label, renamed, category))
    (out / "encoder-selection.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return result


def complete_expectations(cases):
    expected = {
        "empty": ([], None, "void"), "operand-order": ([], 15, "i32"),
        "named-locals": ([46], 42, "i32"), "mixed": ([27], 10, "i64"),
        "depth-three": ([], 15, "i32"), "duplicate-name": ([], 42, "i32"),
        "interleaved-headers": ([42], 42, "i32"), "flat-set": ([], 42, "i32"),
        "local-index-leb": ([0] * 128 + [42], 42, "i32"),
        "nop-drop": ([], None, "void"), "wrapped-i32": ([], -1, "i32"),
        "wrapped-i64": ([], -1, "i64"), "depth-at-policy": ([], None, "void"),
        "catalog-selects-not-full-assembler": ([], None, "void"),
        "encoder-text-bytes": ([38], 42, "i32"),
        "encoder-value-text": ([-4294967297], -1, "i32"),
        "encoder-text-value": ([-1], 4294967295, "i64"),
        "encoder-list-buffer": ([180388626435], 42, "i32"),
        "encoder-list-pack": ([42, 3], 180388626435, "i64"),
        "encoder-w-blank": ([9], 1, "i32"),
        "encoder-pair-before": ([7, 2, 7, 3], 1, "i32"),
    }
    for name, source, category in cases:
        if category != "positive" or name in expected or name in {"rust-raw-not-stack-validator", "comparison"}:
            continue
        if name.startswith(("scalar-", "edge-")):
            continue
        match = re.fullmatch(r"(i32|i64)-(-?[0-9]+)", name)
        if match:
            ty, value = match.groups()
            width = 32 if ty == "i32" else 64
            value = (int(value) + (1 << (width - 1))) % (1 << width) - (1 << (width - 1))
        else:
            ty, op = name.split("-", 1)
            value = {"add": 24, "sub": 18, "mul": 63, "div_s": 7, "div_u": 7}[op]
        expected[name] = ([], value, ty)
    for op, code, input_type, result_type, values, value in SCALAR_CASES:
        for style in ("folded", "flat", "mixed"):
            expected[f"scalar-{op.replace('.', '-')}-{style}"] = ([], value, result_type)
    for label, op, input_type, result_type, values, value, projection in SCALAR_EDGES:
        for style in ("folded", "flat", "mixed"):
            expected[f"edge-{label}-{style}"] = ([], value, result_type)
    positives = {name for name, _, category in cases if category == "positive"}
    expected = {name: value for name, value in expected.items() if name in positives}
    if set(expected) != positives - {"rust-raw-not-stack-validator", "comparison"}:
        raise RuntimeError("complete valid-fixture execution coverage gap")
    return {name: {"args": [str(arg) for arg in args], "arg_type": "i64" if name in {"mixed", "encoder-value-text", "encoder-list-buffer", "encoder-pair-before"} else "i32", "value": str(value) if value is not None else None, "result_type": ty} for name, (args, value, ty) in expected.items()}


def encoder_inventory(args, component, out):
    if args.encoder_wat is None:
        return None
    spec = importlib.util.spec_from_file_location("expr_inventory", ROOT / "scripts/selfhost-expr-inventory.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    assembly = (ROOT / "compiler/assemble.hl").read_text()
    old = set(re.findall(r'assembly_matches\(payload, "([^"]+)"\)', assembly.split("fn assembly_opcode(")[1].split("fn assembly_immediate(")[0])) | {"local.set"}
    expanded = old | set(re.findall(r'assembly_matches\(payload, "([^"]+)"\)', component))
    result = module.inventory(args.encoder_wat, (ROOT / "src/emit/wasm/assemble.rs").read_text(), old, expanded)
    (out / "encoder-scalar-inventory.json").write_text(json.dumps(result, indent=2) + "\n")
    return result["summary"]


def program_static(program, component):
    driver_tree = ast.parse(pathlib.Path(__file__).read_text())
    declarations = re.findall(r"^(?:fn|on|tool|const) (\w+)", program, re.M)
    if len(set(declarations)) != len(declarations):
        raise RuntimeError("duplicate declaration in source concatenation")
    handlers = set(re.findall(r"^on (\w+)\(", program, re.M))
    selected_handlers = set()
    for node in ast.walk(driver_tree):
        if isinstance(node, ast.List):
            for index, item in enumerate(node.elts[:-1]):
                if isinstance(item, ast.Constant) and item.value == "--handler":
                    value = node.elts[index + 1]
                    if not isinstance(value, ast.Constant) or not isinstance(value.value, str):
                        raise RuntimeError("runtime handler selection must be statically literal")
                    selected_handlers.add(value.value)
    if selected_handlers != {"expr_parse", "expr_compile"} or not selected_handlers <= handlers:
        raise RuntimeError("runtime handler literals do not match composed declarations")
    functions = list(re.finditer(r"^fn (\w+)\(", program, re.M))
    positions = {match.group(1): match.start() for match in functions}
    component_bindings = set(re.findall(r"\b(?:let|var|loop) (\w+)", component))
    component_bindings.update(re.findall(r"\b(\w+)\s*:", "\n".join(line for line in component.splitlines() if line.startswith("fn "))))
    if component_bindings & set(positions):
        raise RuntimeError(f"expr binding shadows composed fn: {sorted(component_bindings & set(positions))}")
    if re.search(r"\b(?:let|var|loop) code\b|\bcode\s*:", component):
        raise RuntimeError("reserved code binding in expr component")
    lengths = {}
    for index, match in enumerate(functions):
        end = functions[index + 1].start() if index + 1 < len(functions) else len(program)
        body = program[match.end():end].split("\non ")[0]
        calls = set(re.findall(r"\b(\w+)\(", body))
        if any(positions[name] >= match.start() for name in calls if name in positions):
            raise RuntimeError(f"pure function forward/self call: {match.group(1)}")
        if match.group(1).startswith("expr_"):
            lengths[match.group(1)] = len(body.splitlines())
    stripped = re.sub(r'"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\'', '""', program)
    delimiters = []
    pairs = {')': '(', ']': '[', '}': '{'}
    for char in stripped:
        if char in '([{':
            delimiters.append(char)
        elif char in pairs:
            if not delimiters or delimiters.pop() != pairs[char]:
                raise RuntimeError("source concatenation delimiter mismatch (not HL parse)")
    if delimiters:
        raise RuntimeError("source concatenation unclosed delimiter (not HL parse)")
    if any(not re.match(r"[a-z_][a-z_0-9]*$", bound) for bound in re.findall(r"\bloop \w+ below (\S+)", component)):
        raise RuntimeError("component loop bound is not named")
    return lengths


def static(args, out):
    catalog_path = args.catalog_root / "compiler/assemble-catalog.hl"
    catalog_raw = catalog_path.read_bytes()
    if identity(catalog_raw)["sha256"] != CATALOG_SHA256:
        raise RuntimeError("catalog source differs from pinned committed API dependency")
    catalog = catalog_raw.decode("utf-8")
    head = None
    if (args.catalog_root / ".git").exists():
        try:
            observed = subprocess.run(["git", "-C", str(args.catalog_root), "rev-parse", "HEAD"], check=False, capture_output=True, text=True)
            if observed.returncode == 0:
                head = observed.stdout.strip()
        except FileNotFoundError:
            pass
    for name, digest in RUST_HASHES.items():
        if identity((ROOT / f"src/emit/wasm/{name}.rs").read_bytes())["sha256"] != digest:
            raise RuntimeError(f"frozen Rust {name} changed; never re-bless to match port")
    span_probe = '(func $f (export "a)\\\"("))(func $g (nop))'
    if [span_probe[a:b] for a, b in top_spans(span_probe)] != ['(func $f (export "a)\\\"("))', '(func $g (nop))']:
        raise RuntimeError("source-span quoted delimiter/escape probe")
    module_probe = bytes.fromhex("0061736d010000000105016000017f03020100070501016600000a06010400410f0b")
    if patch_module(module_probe, bytes.fromhex("410f")) != module_probe or patch_module(module_probe, bytes.fromhex("4110")) == module_probe:
        raise RuntimeError("source-only module-splice helper probe")
    for value in (0, 127, 128, 65535, 4294967295):
        encoded = unsigned(value)
        if read_unsigned(encoded, 0) != (value, len(encoded)):
            raise RuntimeError("source-only module-u32 helper probe")
    component = (ROOT / "compiler/assemble-expr.hl").read_text()
    if not all(name.startswith("expr_") for name in re.findall(r"^(?:fn|const) (\w+)", component, re.M)):
        raise RuntimeError("component namespace violation")
    if re.search(r'\b(assert|panic)\b|//|/\*|;', component):
        raise RuntimeError("new language feature or comment in component")
    for name, (before, after, _) in MUTANTS.items():
        if component.count(before) != 1 or before == after:
            raise RuntimeError(f"mutant site not unique: {name}")
    if 6 * 4 != 24 or 4096 * 24 != 98304 or 98304 > 41943040 or 1 + 10 != 11:
        raise RuntimeError("frame/instruction capacity derivation")
    scalar_evidence = scalar_static(component)
    program = combined(component, catalog)
    lengths = program_static(program, component)
    for path in (ROOT / "compiler/assemble-expr.hl", ROOT / "scripts/selfhost-expr.py", ROOT / "scripts/selfhost-expr-oracle.rs", ROOT / "scripts/selfhost-expr-execute.mjs", ROOT / "scripts/selfhost-expr-inventory.py"):
        raw = path.read_bytes()
        if not raw.endswith(b"\n") or any(line.rstrip() != line for line in raw.splitlines()):
            raise RuntimeError(f"source newline/trailing whitespace: {path}")
    (out / "expr-combined.hl").write_text(program)
    cases = fixtures() + encoder_cases(args, out)
    execution_expected = complete_expectations(cases)
    (out / "execution-expectations.json").write_text(json.dumps(execution_expected, indent=2) + "\n")
    scalar_evidence["encoder_inventory"] = encoder_inventory(args, component, out)
    counts = {category: sum(c == category for _, _, c in cases) for category in ("positive", "invalid", "unsupported")}
    expected_counts = {"positive": 229 + 7 * (args.encoder_wat is not None), "invalid": 14, "unsupported": 10 + (args.encoder_wat is not None)}
    if counts != expected_counts:
        raise RuntimeError("fixture category count changed")
    files = []
    for name, source, category in cases:
        path = out / "fixtures" / f"{name}.wat"
        path.parent.mkdir(exist_ok=True)
        path.write_text(source)
        files.append((name, path, category))
    for name, (before, after, _) in MUTANTS.items():
        (out / f"mutant-{name}.hl").write_text(combined(component.replace(before, after), catalog))
    evidence = {"status": "source-static-only", "runtime_runs": 0, "hl_check_runs": 0, "hl_proof_runs": 0, "stage1_runs": 0, "stage2_runs": 0, "bootstrap_runs": 0, "catalog_source": CATALOG_SOURCE, "catalog_observed_git_head": head, "catalog_pin_mode": "exact raw component SHA256; Git HEAD optional metadata only", "sources": {str(path.relative_to(ROOT)): identity(path.read_bytes()) for path in (ROOT / "compiler/assemble-expr.hl", ROOT / "scripts/selfhost-expr.py", ROOT / "scripts/selfhost-expr-oracle.rs", ROOT / "scripts/selfhost-expr-execute.mjs", ROOT / "scripts/selfhost-expr-inventory.py")}, "catalog": identity(catalog.encode()), "combined": identity(program.encode()), "contract": CONTRACT, "function_lines": lengths, "counts": {category: sum(c == category for _, _, c in cases) for category in ("positive", "invalid", "unsupported")}, "checks": ["Python AST (not HL AST)", "Python source-span escape, synthetic code-section splice and u32 LEB probes (no Wasm runtime)", "exact four Rust hashes and raw catalog SHA; Git provenance optional", "declaration uniqueness and pure function source call ordering", "Python AST runtime handler literals match composed HL handler declarations", "new declaration namespace and no new syntax/comments", "new bindings do not shadow composed functions or reserved code keyword", "frame and instruction bound arithmetic", "mutant sites unique", "concatenated delimiter balance (not HL parse)", "named HL loop bounds and newline/whitespace", "fixtures emitted, not executed"]}
    evidence["scalar"] = scalar_evidence
    evidence["complete_fixture_expectations"] = len(execution_expected)
    evidence["raw_only_not_type_validated"] = ["rust-raw-not-stack-validator", "comparison"]
    (out / "source-static-evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return files, evidence


def finished(path):
    records = [json.loads(line).get("record", {}).get("status", {}).get("status", {}).get("finished") for line in path.read_text().splitlines()]
    records = [value for value in records if value is not None]
    if len(records) != 1 or records[0].get("outcome") != "completed" or not isinstance(records[0].get("result"), str):
        raise RuntimeError(f"exactly one completed text result required: {path}")
    return records[0]["result"]


def execute(args, run, program, module, path, directory):
    directory.mkdir(parents=True)
    run([args.hl, "run", program, "--handler", "expr_parse", "--arg-file", path], directory / "interpreter.jsonl")
    value = finished(directory / "interpreter.jsonl")
    (directory / "input.wat").write_bytes(path.read_bytes())
    run([args.host, "run", "--module", module, "--journal", directory / "journal", "--cwd", directory, "--handler", "expr_compile", "--args", '{"input_path":"input.wat","output_path":"result.bin"}'], directory / "host.log")
    compiled_status = finished(directory / "journal/journal.jsonl")
    target = directory / "result.bin"
    actual = target.read_bytes() if target.is_file() else None
    if value.startswith("ok "):
        if actual != bytes.fromhex(value[3:]):
            raise RuntimeError(f"interpreter/compiled bytes mismatch: {directory}")
    elif actual is not None or compiled_status.startswith("ok "):
        raise RuntimeError(f"negative wrote output or compiled accepted: {directory}")
    return value, actual


def read_unsigned(raw, at):
    value = 0
    for shift in range(0, 35, 7):
        byte = raw[at]
        at += 1
        value |= (byte & 127) << shift
        if byte < 128:
            return value, at
    raise RuntimeError("module u32 LEB overflow")


def unsigned(value):
    out = bytearray()
    for _ in range(5):
        byte = value & 127
        value >>= 7
        out.append(byte | (128 if value else 0))
        if not value:
            return bytes(out)
    raise RuntimeError("module u32 LEB overflow")


def patch_module(oracle, instructions):
    if oracle[:8] != b'\0asm\x01\0\0\0':
        raise RuntimeError("oracle module header")
    at = 8
    result = bytearray(oracle[:8])
    code_sections = 0
    while at < len(oracle):
        kind = oracle[at]
        size, start = read_unsigned(oracle, at + 1)
        end = start + size
        content = oracle[start:end]
        if kind == 10:
            code_sections += 1
            count, body_at = read_unsigned(content, 0)
            body_size, local_at = read_unsigned(content, body_at)
            groups, next_at = read_unsigned(content, local_at)
            for _ in range(groups):
                _, next_at = read_unsigned(content, next_at)
                next_at += 1
            if count != 1 or local_at + body_size != len(content):
                raise RuntimeError("execution wrapper requires exactly one function")
            body = content[local_at:next_at] + instructions + b'\x0b'
            content = unsigned(1) + unsigned(len(body)) + body
        result.extend(bytes([kind]) + unsigned(len(content)) + content)
        at = end
    if code_sections != 1:
        raise RuntimeError("execution wrapper requires one code section")
    return bytes(result)


def runtime(args, out, cases, evidence):
    runner = load_runner()
    def run(arguments, log, accepted=(0,)):
        return runner.bounded_run(arguments, log, accepted, args.command_seconds_max, args.command_output_bytes_max)
    for path in (args.hl, args.host):
        if not path.is_absolute() or not path.is_file():
            raise RuntimeError("allocated runtime requires absolute prebuilt hl and host")
    oracle = out / "oracle"
    run(["rustc", "--edition=2024", "-O", ROOT / "scripts/selfhost-expr-oracle.rs", "-o", oracle], out / "oracle-build.log")
    program = out / "expr-combined.hl"
    module = out / "expr.wasm"
    run([args.hl, "check", program], out / "check.log")
    run([args.hl, "prove", program], out / "prove.log")
    run([args.hl, "emit", "--target", "native-jsonl", "--format", "wasm", program], module)
    records = []
    for repeat in range(args.runs):
        for name, path, category in cases:
            directory = out / "runs" / str(repeat) / name
            directory.mkdir(parents=True)
            expected = None
            if category != "unsupported":
                run([oracle, path, directory / "rust.bin", directory / "rust.wasm"], directory / "oracle.log", accepted=(0,) if category == "positive" else (1,))
                if category == "positive":
                    expected = (directory / "rust.bin").read_bytes()
                    if name in EXPECTED_PROJECTIONS and expected.hex() != EXPECTED_PROJECTIONS[name]:
                        raise RuntimeError(f"independent exact instruction bytes invariant: {name}")
            value, compiled = execute(args, run, program, module, path, directory / "hl")
            if category == "positive":
                if not value.startswith("ok ") or bytes.fromhex(value[3:]) != expected or compiled != expected:
                    raise RuntimeError(f"Rust body projection mismatch: {directory}")
                (directory / "interpreter.bin").write_bytes(bytes.fromhex(value[3:]))
            elif value.startswith("ok "):
                raise RuntimeError(f"negative admitted: {directory}")
            records.append({"repeat": repeat, "fixture": name, "category": category, "result": identity(value.encode())})
    caught = []
    for name, (_, _, fixture) in MUTANTS.items():
        mutated = out / f"mutant-{name}.hl"
        mutant_module = out / f"mutant-{name}.wasm"
        run([args.hl, "emit", "--target", "native-jsonl", "--format", "wasm", mutated], mutant_module)
        path = next(path for label, path, _ in cases if label == fixture)
        for repeat in range(args.runs):
            directory = out / "mutants" / name / str(repeat)
            directory.mkdir(parents=True)
            run([oracle, path, directory / "rust.bin", directory / "rust.wasm"], directory / "oracle.log")
            expected = (directory / "rust.bin").read_bytes()
            value, compiled = execute(args, run, mutated, mutant_module, path, directory / "hl")
            if value.startswith("ok ") and (bytes.fromhex(value[3:]) == expected or compiled == expected):
                raise RuntimeError(f"mutant survived: {name}")
            caught.append({"mutant": name, "repeat": repeat, "engines": ["interpreter", "compiled"], "fresh_rust": True})
    executions = []
    for repeat in range(args.runs):
        for fixture in complete_expectations(cases):
            if not any(name == fixture for name, _, _ in cases):
                continue
            directory = out / "runs" / str(repeat) / fixture
            oracle_wasm = (directory / "rust.wasm").read_bytes()
            for mode, body_path in (("interpreter", directory / "interpreter.bin"), ("compiled", directory / "hl/result.bin")):
                module_path = directory / f"{mode}.wasm"
                actual = patch_module(oracle_wasm, body_path.read_bytes())
                if actual != oracle_wasm:
                    raise RuntimeError("emitted instruction bytes changed full oracle module")
                module_path.write_bytes(actual)
                executions.append({"fixture": fixture, "mode": mode, "repeat": repeat, "module": str(module_path)})
    (out / "execution-input.json").write_text(json.dumps(executions) + "\n")
    run([args.node, ROOT / "scripts/selfhost-expr-execute.mjs", out], out / "execution.log")
    evidence.update(status="passed-selected-body-only", runtime_runs=args.runs, hl_check_runs=1, hl_proof_runs=1, hl_emit_runs=1 + len(MUTANTS), rust_oracle_build_runs=1, proof_scope="Handler/effect proof only; pure helper internals report zero arithmetic/invariant/push proof steps, not universal parser correctness", records=records, mutants=caught, executions=json.loads((out / "execution-results.json").read_text()), binaries={str(path): identity(path.read_bytes()) for path in (args.hl, args.host, oracle)})
    (out / "runtime-evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description="Selected iterative folded-scalar instruction cursor. --static never invokes Rust/compiler/runtime. No full assembler or stage1/stage2 claims.")
    parser.add_argument("--static", action="store_true")
    parser.add_argument("--allocated", action="store_true")
    parser.add_argument("--catalog-root", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--encoder-wat", type=pathlib.Path)
    parser.add_argument("--hl", type=pathlib.Path)
    parser.add_argument("--host", type=pathlib.Path)
    parser.add_argument("--node", default="node")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--command-seconds-max", type=int, default=300)
    parser.add_argument("--command-output-bytes-max", type=int, default=16777216)
    args = parser.parse_args()
    if args.static == args.allocated:
        parser.error("choose --static or --allocated (runtime needs root allocation)")
    if args.runs < 3 or min(args.command_seconds_max, args.command_output_bytes_max) <= 0:
        parser.error("N>=3 and positive caller command resource bounds required")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    cases, evidence = static(args, out)
    if args.allocated:
        if args.hl is None or args.host is None:
            parser.error("--allocated needs --hl and --host absolute prebuilt paths")
        (out / "runtime-started.json").write_text('{"status":"incomplete-unless-runtime-evidence-passed"}\n')
        runtime(args, out, cases, evidence)
    print(json.dumps({"counts": evidence["counts"], "runtime": args.allocated, "status": evidence["status"]}))


if __name__ == "__main__":
    main()
