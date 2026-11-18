import collections
import hashlib
import json
import re

ENCODER_BYTES = 683936
ENCODER_SHA256 = "148575c5ff0a8c7132f6642cd3dc1a1be6664ad54e421d2a02928689010e7425"
HEADERS = {"export", "param", "result", "local"}
MARKERS = {"then", "else", "result"}


def parse(source):
    stack = [[]]
    tokens = re.finditer(r'"(?:[^"\\]|\\.)*"|[()]|[^\s()"]+', source)
    for token in tokens:
        word = token.group()
        if word == "(":
            if len(stack) > 4096:
                raise RuntimeError("inventory nesting bound")
            node = []
            stack[-1].append(node)
            stack.append(node)
        elif word == ")":
            if len(stack) == 1:
                raise RuntimeError("inventory unmatched close")
            stack.pop()
        else:
            stack[-1].append(word)
    if len(stack) != 1:
        raise RuntimeError("inventory unclosed form")
    return stack[0]


def family(op):
    if op in {"call"}:
        return "call"
    if op.startswith("global."):
        return "global"
    if op.startswith("local."):
        return "local"
    if ".load" in op or ".store" in op:
        return "load-store"
    if op.startswith("memory."):
        return "memory"
    if op.startswith(("i32.", "i64.")):
        return "integer-scalar"
    if op.startswith(("f32.", "f64.")):
        return "float"
    if op in {"drop", "select", "nop"}:
        return "stack"
    return "control"


def rust_opcodes(source):
    comparisons = re.search(r'const COMPARISONS:.*?= \[(.*?)\];', source, re.S).group(1)
    arithmetic = re.search(r'const ARITHMETIC:.*?= \[(.*?)\];', source, re.S).group(1)
    numeric = {f"{ty}.{op}" for ty in ("i32", "i64") for op in re.findall(r'"([a-z_]+)"', comparisons + arithmetic)}
    simple = set(re.findall(r'"([\w.]+)" => (?:plain|memory)\(', source))
    others = set("block loop if else end local.get local.set local.tee global.get global.set call br br_if br_table i32.const i64.const memory.size memory.grow memory.copy memory.fill".split())
    return numeric | simple | others


def record(form, known):
    at = 2
    headers = []
    while at < len(form) and isinstance(form[at], list) and form[at] and form[at][0] in HEADERS:
        headers.append(form[at])
        at += 1
    operations = collections.Counter()
    todo = list(reversed(form[at:]))
    constants = []
    folded = 0
    flat_constants = 0
    while todo:
        node = todo.pop()
        if isinstance(node, list):
            if not node or not isinstance(node[0], str):
                raise RuntimeError("inventory invalid body head")
            head = node[0]
            if head in known and head not in MARKERS:
                operations[head] += 1
                folded += 1
            elif head not in MARKERS:
                raise RuntimeError(f"inventory unknown body head {head}")
            if head in {"i32.const", "i64.const"}:
                constants.append((head, node[1] if len(node) > 1 else None))
            todo.extend(reversed(node[1:]))
        elif node in known:
            operations[node] += 1
            if node in {"i32.const", "i64.const"}:
                flat_constants += 1
                constants.append((node, todo[-1] if todo else None))
    bad_constants = [(op, word) for op, word in constants if not isinstance(word, str) or re.fullmatch(r'-?[0-9]+', word) is None]
    return {"symbol": form[1], "opcodes": dict(sorted(operations.items())), "folded_instructions": folded, "flat_constants": flat_constants, "constant_syntax_excluded": bad_constants, "body_nodes": len(form) - at}


def inventory(path, rust, old, expanded):
    raw = path.read_bytes()
    if len(raw) != ENCODER_BYTES or hashlib.sha256(raw).hexdigest() != ENCODER_SHA256:
        raise RuntimeError("inventory frozen source identity mismatch")
    forms = parse(raw.decode())
    functions = [form for form in forms if form and form[0] == "func"]
    if len(forms) != 2048 or len(functions) != 826:
        raise RuntimeError("inventory frozen AST counts mismatch")
    known = rust_opcodes(rust)
    records = [record(form, known) for form in functions]
    for row in records:
        ops = set(row["opcodes"])
        row["old_opcode_subset"] = ops <= old
        row["expanded_opcode_subset"] = ops <= expanded
        row["potential_full_scalar_body"] = row["expanded_opcode_subset"] and not row["constant_syntax_excluded"]
        row["new_potential_full_scalar_body"] = row["potential_full_scalar_body"] and not row["old_opcode_subset"]
        row["missing_opcodes"] = sorted(ops - expanded)
    missing = collections.Counter()
    families = collections.Counter()
    for row in records:
        for op in row["missing_opcodes"]:
            missing[op] += row["opcodes"][op]
            families[family(op)] += row["opcodes"][op]
    summary = {"functions": len(records), "forms": len(forms), "old_opcode_subset_bodies": sum(row["old_opcode_subset"] for row in records), "expanded_opcode_subset_bodies": sum(row["expanded_opcode_subset"] for row in records), "potential_full_scalar_bodies": sum(row["potential_full_scalar_body"] for row in records), "new_potential_full_scalar_bodies": sum(row["new_potential_full_scalar_body"] for row in records), "new_potential_symbols": [row["symbol"] for row in records if row["new_potential_full_scalar_body"]], "missing_opcode_occurrences": dict(sorted(missing.items())), "missing_opclasses": dict(sorted(families.items()))}
    return {"status": "source-static-AST-inventory-only", "runtime_runs": 0, "source": {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}, "method": "Bounded Python source S-expression AST; no Rust lexer executed; function headers excluded, folded heads and known flat opcode atoms counted. Potential means all instruction spellings in supported set and constant syntax decimal; not HL admission, type validity, parser parity, binary parity, or execution. Folded and flat constants both inspected; symbolic/hex/underscore/plus immediates exclude potential full scalar bodies. Catalog/local index validity and policy bounds are not reimplemented here.", "summary": summary, "functions": records}
