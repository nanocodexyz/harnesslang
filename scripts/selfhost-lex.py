import argparse
import concurrent.futures
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "output/selfhost-lex"
HL = ROOT / "target/release/hl"
HOST = ROOT / "hosts/native/target/release/hl-host"
PROGRAM = ROOT / "compiler/assemble-lex.hl"
ORACLE = OUT / "oracle"
MODULE = OUT / "assemble-lex.wasm"
FROZEN_SHA256 = "f3e7011924b28138c322d04019ccac93d80d9caaf470347181154becba53a29a"
SOURCE_BYTES_MAX = 4194304
NESTING_MAX = 4096
FIXTURE_SEEDS_MAX = 64
FIXTURE_FRAGMENTS_MAX = 32
BYTE_VALUES = 256
GENERATOR_MULTIPLIER = 1664525
GENERATOR_INCREMENT = 1013904223
GENERATOR_MODULUS = 4294967296
TARGETS = ("native-jsonl", "native-sqlite", "server-postgres", "cloudflare", "vercel")


def run(arguments, output=None, accepted=(0,)):
    result = subprocess.run([str(value) for value in arguments], cwd=ROOT, capture_output=True)
    if output is not None:
        output.write_bytes(result.stdout)
        output.with_suffix(output.suffix + ".err").write_bytes(result.stderr)
    if result.returncode not in accepted:
        raise RuntimeError(f"command failed ({result.returncode}): {arguments}\n{result.stderr.decode(errors='replace')}")
    return result.stdout


def frozen():
    source = (ROOT / "src/emit/wasm/assemble.rs").read_text()
    start = source.index("#[derive(Clone, Debug)]")
    end = source.index("pub fn number")
    copied = source[start:end]
    if hashlib.sha256(copied.encode()).hexdigest() != FROZEN_SHA256:
        raise RuntimeError("the frozen Rust tokenizer changed; do not re-bless the oracle to match a port")
    bounds = dict(re.findall(r"const (\w+): u64 = (\d+)", PROGRAM.read_text()))
    assert int(bounds["source_bytes_max"]) == SOURCE_BYTES_MAX
    assert int(bounds["nesting_max"]) == NESTING_MAX
    assert int(bounds["tokens_bytes_max"]) == SOURCE_BYTES_MAX * 10
    assert int(bounds["result_bytes_max"]) == SOURCE_BYTES_MAX * 20 + len("ok ")
    assert int(bounds["string_unterminated"]) == SOURCE_BYTES_MAX + 1
    assert int(bounds["string_bad_escape"]) == SOURCE_BYTES_MAX + 2
    assert f"const NESTING_MAX: usize = {NESTING_MAX};" in source
    assert "const HEX_RADIX: u32 = 16;" in source
    (OUT / "frozen.rs").write_text(copied)


def fixtures():
    cases = {
        "empty": "",
        "space": " \t\r\n\n",
        "siblings": '(a)(b "" x)atom"tail"',
        "lines": '(module\n(func\r\n  (export "x")\n))\n',
        "bytes": '"' + "".join(f"\\{byte:02x}" for byte in range(BYTE_VALUES)) + '"',
        "escapes": r'''"\n\t\\\"\'\00\aF\FF\+f"''',
        "unicode": '(αβ "🙂\né")\nλ',
        "raw-controls": '"\x00\t\r\n"\x00\v\f;',
        "string-line": '("a\nb"\n"c\nd")\n)',
        "no-comments": '(;; x\n (;y;) )',
        "open": "(\n(",
        "close": "\n)",
        "unfinished": '("no end\n',
        "backslash": '"abc' + "\\",
        "short-hex": '"' + "\\a",
        "bad-hex": r'''"\xz"''',
        "bad-escape": r'''"\r"''',
        "escape-line": '\n(\n"\\xz")',
        "comment-atoms": '(; not-a-comment\n)',
        "comment-close": ';; not-a-comment\n)',
        "bad-sign": r'''"\-1"''',
        "depth-limit": "(" * NESTING_MAX + ")" * NESTING_MAX,
        "depth-overflow": "(" * (NESTING_MAX + 1),
        "stack-reuse": "(\n(\n)\n)\n((()))",
    }
    fragments = ("a", " ", "\n", "(", ")", '"', "\\", "0", "n", "+", "é")
    for seed in range(FIXTURE_SEEDS_MAX):
        state = seed + 1
        parts = []
        for _ in range(FIXTURE_FRAGMENTS_MAX):
            state = (state * GENERATOR_MULTIPLIER + GENERATOR_INCREMENT) % GENERATOR_MODULUS
            parts.append(fragments[state % len(fragments)])
        cases[f"seed-{seed}"] = "".join(parts)
    paths = []
    for name, text in cases.items():
        path = OUT / "fixtures" / f"{name}.wat"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(text.encode())
        paths.append(path)
    return paths


def prepare():
    for binary in (HL, HOST):
        if not binary.is_file():
            raise RuntimeError(f"build {binary.relative_to(ROOT)} on a sandbox before running this script")
    run(["rustc", "--edition=2024", "-O", ROOT / "scripts/selfhost-lex-oracle.rs", "-o", ORACLE])
    run([HL, "check", PROGRAM], OUT / "check.log")
    run([HL, "prove", PROGRAM], OUT / "prove.log")
    run([HL, "emit", "--format", "wasm", "--target", "native-jsonl", PROGRAM], MODULE)


def finished_record(path):
    values = []
    for line in path.read_text().splitlines():
        record = json.loads(line).get("record", {})
        status = record.get("status", {}).get("status", {})
        if "finished" in status:
            values.append(status["finished"])
    if len(values) != 1:
        raise RuntimeError(f"expected exactly one finished record: {path}")
    return values[0]


def finished(path):
    finish = finished_record(path)
    if finish.get("outcome") != "completed" or not isinstance(finish.get("result"), str):
        raise RuntimeError(f"expected completed text result: {path}")
    return finish["result"].encode()


def compare(path, index):
    directory = OUT / "cases" / str(index)
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    source = directory / "source.wat"
    shutil.copyfile(path, source)
    expected = run([ORACLE, source], directory / "rust.out")
    run([HL, "run", PROGRAM, "--handler", "tokenize", "--arg-file", source], directory / "run.jsonl")
    interpreted = finished(directory / "run.jsonl")
    (directory / "interpreter.out").write_bytes(interpreted)
    run([HOST, "run", "--module", MODULE, "--journal", directory / "journal", "--cwd", directory,
         "--handler", "compile", "--args", '{"input_path":"source.wat","output_path":"compiled.out"}'], directory / "host.out")
    compiled = (directory / "compiled.out").read_bytes()
    if interpreted != expected or compiled != expected:
        raise RuntimeError(f"tokenizer differs from frozen Rust: {path} (see {directory})")
    return f"ok selfhost-lex input={path.relative_to(ROOT)} source_bytes={source.stat().st_size}"


def bounds():
    directory = OUT / "bounds"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    source = directory / "source.wat"
    source.write_bytes(b" " * (SOURCE_BYTES_MAX + 1))
    run([ORACLE, source], directory / "rust.out", accepted=(1,))
    trap = "bound(source_bytes_max)"
    if trap not in (directory / "rust.out.err").read_text():
        raise RuntimeError("Rust oracle failed for a reason other than the source policy")
    run([HL, "run", PROGRAM, "--handler", "tokenize", "--arg-file", source], directory / "run.jsonl")
    run([HOST, "run", "--module", MODULE, "--journal", directory / "journal", "--cwd", directory,
         "--handler", "compile", "--args", '{"input_path":"source.wat","output_path":"compiled.out"}'],
        directory / "host.out", accepted=(0, 1))
    journals = (directory / "run.jsonl", directory / "journal/journal.jsonl")
    for journal in journals:
        finish = finished_record(journal)
        if finish.get("outcome") != "failed" or finish.get("result") != {"trap": trap}:
            raise RuntimeError(f"expected exact source-bound trap: {journal}")
    if (directory / "compiled.out").exists():
        raise RuntimeError("source exhaustion wrote an output file")
    print("ok selfhost-lex source_bound=source_bytes_max interpreter=failed wasm=failed", flush=True)


def emitted(files, targets):
    paths = []
    for file in files:
        for target in targets:
            directory = OUT / "emitted" / target
            directory.mkdir(parents=True, exist_ok=True)
            name = file.relative_to(ROOT).as_posix().replace("/", "-")
            path = directory / f"{name}.wat"
            run([HL, "emit", "--format", "wat", "--target", target, file], path)
            paths.append(path)
    return paths


def mutations(paths):
    original = PROGRAM.read_text()
    plants = {
        "line": ("line = line + 1", "line = line + 2"),
        "escape": ("decoded = '\\n'", "decoded = tab_byte"),
        "nesting": ("if depth == nesting_max {", "if depth + 1 == nesting_max {"),
    }
    for name, (before, after) in plants.items():
        assert original.count(before) == 1
        path = OUT / f"mutation-{name}.hl"
        path.write_text(original.replace(before, after))
        caught = False
        for fixture in paths:
            expected = run([ORACLE, fixture])
            output = OUT / f"mutation-{name}.jsonl"
            run([HL, "run", path, "--handler", "tokenize", "--arg-file", fixture], output)
            if finished(output) != expected:
                caught = True
                break
        if not caught:
            raise RuntimeError(f"mutation survived: {name}")
        print(f"ok selfhost-lex mutation={name}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=(
        "Differential-check the bounded assemble lexer, not the assembler or a self-hosted compiler. "
        "The stream is 'ok ' plus hex records: kind u8 (open=1, close=2, atom=3, text=4), "
        "opening line u32-le, payload length u32-le, payload bytes. Closing records reuse the opening line. "
        "Failures are the original Rust error text. The 4 MiB caller source policy matches encode.hl; "
        "one record per source byte costs at most 9 header bytes plus one payload byte, so output <=10N. "
        "The result needs 20N+3 UTF-8 bytes, nesting is Rust's 4096, decimal u64 needs at most 20 digits. "
        "Run on a sandbox with existing release hl and hl-host binaries; no automatic heavy build."))
    parser.add_argument("files", nargs="*", type=pathlib.Path)
    parser.add_argument("--static", action="store_true", help="verify frozen source and bound derivations; generate fixtures without builds")
    parser.add_argument("--fixtures-only", action="store_true")
    parser.add_argument("--mutations", action="store_true")
    parser.add_argument("--bounds", action="store_true")
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--targets", nargs="+", choices=TARGETS, default=TARGETS)
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be at least one")
    OUT.mkdir(parents=True, exist_ok=True)
    frozen()
    paths = fixtures()
    if args.static:
        print(f"selfhost-lex static: frozen Rust hash and capacity derivations verified; {len(paths)} fixtures generated")
        return
    prepare()
    if not args.fixtures_only:
        files = [path.resolve() for path in args.files] or sorted((ROOT / "examples").glob("*.hl")) + sorted((ROOT / "docs/snippets").glob("*.hl"))
        paths += emitted(files, args.targets)
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as workers:
        results = workers.map(compare, paths, range(len(paths)))
        for result in results:
            print(result, flush=True)
    if args.mutations:
        mutations(paths)
    if args.bounds:
        bounds()
    print(f"selfhost-lex: {len(paths)} inputs identical to frozen Rust in the interpreter and compiled wasm")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, AssertionError, ValueError) as error:
        print(f"selfhost-lex: {error}", file=sys.stderr)
        sys.exit(1)
