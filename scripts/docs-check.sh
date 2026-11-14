#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

docs="${DOCS_CHECK_FILES:-docs/LANGUAGE.md README.md}"
language="${DOCS_CHECK_LANGUAGE:-docs/LANGUAGE.md}"
snippets=docs/snippets
scratch="${DOCS_CHECK_SCRATCH:-output/docs-check}"
bin=target/release
mkdir -p "$scratch"

[ -x "$bin/hl" ] || cargo build --release --locked

fail() {
  echo "docs-check: $*" >&2
  exit 1
}

source_list() {
  local file=$1 name=$2
  awk -v start="const $name: [" 'index($0, start) { on = 1 } on { print } on && index($0, "];") { exit }' "$file" |
    grep -oE '"[^"]*"' | tr -d '"' | sort -u
}

doc_list() {
  local label=$1 line
  line=$(grep -m1 "^$label: " "$language") || fail "$language has no '$label:' line"
  grep -oE '`[^`]+`' <<< "${line#*: }" | tr -d '`' | sort -u
}

symbols() {
  sed -n '/const fn text/,/^    }/p' src/syntax/lexer.rs | grep -oE '=> "[^"]+"' | sed 's/^=> "//; s/"$//' | sort -u
}

effect_names() {
  sed -n '/const fn name/,/^    }/p' src/effect.rs | grep -oE '=> "[^"]+"' | sed 's/^=> "//; s/"$//' | sort -u
}

contextual_words() {
  local keywords=$1
  grep -ohE '(word|is_word)\("[a-z_]+"\)' src/syntax/parser.rs | sed -E 's/.*\("(.*)"\)/\1/' | sort -u |
    comm -23 - "$keywords"
}

grammar_terminals() {
  awk '/^```ebnf$/ { on = 1; next } on && /^```$/ { on = 0 } on' "$language" |
    grep -oE '"[^"]+"' | sed 's/^"//; s/"$//' | sort -u
}

compare_list() {
  local what=$1 source=$2 doc=$3 origin=$4
  diff -u "$source" "$doc" > "$doc.diff" ||
    fail "the $what of $language differs from $origin: $(tail -n +3 "$doc.diff" | tr '\n' ' ')"
}

check_lists() {
  local work=$scratch/lists
  mkdir -p "$work"
  source_list src/syntax/parser.rs KEYWORDS > "$work/keywords.source"
  doc_list Keywords > "$work/keywords.doc"
  compare_list "Keywords line" "$work/keywords.source" "$work/keywords.doc" "KEYWORDS in src/syntax/parser.rs"
  symbols > "$work/symbols.source"
  doc_list Symbols > "$work/symbols.doc"
  compare_list "Symbols line" "$work/symbols.source" "$work/symbols.doc" "Symbol::text in src/syntax/lexer.rs"
  source_list src/check/mod.rs BUILTIN_FUNCTIONS > "$work/builtins.source"
  doc_list Builtins > "$work/builtins.doc"
  compare_list "Builtins line" "$work/builtins.source" "$work/builtins.doc" "BUILTIN_FUNCTIONS in src/check/mod.rs"
  contextual_words "$work/keywords.source" > "$work/contextual.source"
  doc_list "Contextual words" > "$work/contextual.doc"
  compare_list "Contextual words line" "$work/contextual.source" "$work/contextual.doc" "the words src/syntax/parser.rs expects"
  effect_names > "$work/effects.source"
  sort -u "$work/keywords.source" "$work/symbols.source" "$work/contextual.source" "$work/effects.source" > "$work/terminals.source"
  grammar_terminals > "$work/terminals.doc"
  [ -s "$work/terminals.doc" ] || fail "$language has no ebnf grammar block"
  compare_list "grammar's terminals" "$work/terminals.source" "$work/terminals.doc" "the keywords, symbols, contextual words and effect classes of src"
  echo "docs-check: keywords, symbols, builtins, contextual words and grammar terminals match src"
}

constant_value() {
  local name=$1 expression
  expression=$(grep -rhE "const $name: [A-Za-z0-9_]+ = [^;]+;" src | head -n 1 | sed -E 's/.*= ([^;]+);.*/\1/' | tr -d '_')
  [ -n "$expression" ] || return 1
  [[ "$expression" =~ ^[0-9\ \*\+]+$ ]] || return 1
  echo $((expression))
}

check_constants() {
  local pairs count=0 name quoted actual
  pairs=$(grep -oE '`[A-Z][A-Z0-9_]+` = [0-9][0-9,]*' "$language" | sort -u || true)
  [ -n "$pairs" ] || fail "$language quotes no named constant"
  while IFS= read -r pair; do
    name=$(sed -E 's/^`([A-Z0-9_]+)`.*/\1/' <<< "$pair")
    quoted=$(sed -E 's/.*= //' <<< "$pair" | tr -d ',')
    actual=$(constant_value "$name") || fail "$language quotes $name, which src does not define as a number"
    [ "$actual" = "$quoted" ] || fail "$language says $name = $quoted, src says $actual"
    count=$((count + 1))
  done <<< "$pairs"
  echo "docs-check: $count named constant(s) match src"
}

extract() {
  local doc=$1 out=$2
  rm -rf "$out"
  mkdir -p "$out"
  awk -v out="$out" -v doc="$doc" '
    function bad(message) { print doc ":" NR ": " message > "/dev/stderr"; failed = 1; exit 1 }
    !inside && /^```/ {
      info = substr($0, 4)
      inside = 1
      kind = ""
      if (info ~ /^hl( |$)/) {
        split(info, words, " ")
        if (words[2] == "") bad("an hl block names no file: write ```hl docs/snippets/NAME.hl")
        snippet += 1
        kind = "hl"
        body = out "/hl-" snippet ".body"
        printf "" > body
        print words[2] > (out "/hl-" snippet ".path")
        close(out "/hl-" snippet ".path")
      } else if (info == "console") {
        kind = "console"
        command_open = 0
      }
      next
    }
    inside && /^```$/ {
      inside = 0
      if (kind == "hl") close(body)
      if (kind == "console" && command_open) close(expected)
      next
    }
    inside && kind == "hl" { print > body; next }
    inside && kind == "console" && /^\$ / {
      if (command_open) close(expected)
      command += 1
      command_open = 1
      print substr($0, 3) > (out "/console-" command ".command")
      close(out "/console-" command ".command")
      print NR > (out "/console-" command ".line")
      close(out "/console-" command ".line")
      expected = out "/console-" command ".expected"
      printf "" > expected
      next
    }
    inside && kind == "console" {
      if (!command_open) bad("a console block must start with a $ command")
      print > expected
      next
    }
    !inside && index($0, ": error: ") { bad("an error is quoted outside a console block, where it is not checked") }
    END { if (inside && !failed) bad("a code block is not closed") }
  ' "$doc"
}

matches() {
  awk '
    FNR == NR { want[++wanted] = $0; next }
    { have[++had] = $0 }
    END {
      w = 1; h = 1
      while (w <= wanted) {
        if (want[w] == "...") {
          w++
          if (w > wanted) exit 0
          while (h <= had && have[h] != want[w]) h++
          if (h > had) { print "expected line not found: " want[w]; exit 1 }
          continue
        }
        if (h > had) { print "missing line: " want[w]; exit 1 }
        if (have[h] != want[w]) { print "line " h ": expected: " want[w] " | found: " have[h]; exit 1 }
        w++; h++
      }
      if (h <= had) { print "unexpected line " h ": " have[h]; exit 1 }
    }
  ' "$1" "$2"
}

check_doc() {
  local doc=$1 out blocks=0 commands=0 path index
  out=$scratch/blocks/$(basename "$doc" .md)
  extract "$doc" "$out" || fail "$doc has a malformed code block"
  for body in "$out"/hl-*.body; do
    [ -e "$body" ] || break
    path=$(cat "${body%.body}.path")
    [ -f "$path" ] || fail "$doc shows $path, which does not exist"
    cmp -s "$body" "$path" || fail "$doc shows $path, but the block differs from the file: $(diff "$body" "$path" | head -n 6 | tr '\n' ' ')"
    echo "$path" >> "$scratch/shown"
    blocks=$((blocks + 1))
  done
  for command_file in "$out"/console-*.command; do
    [ -e "$command_file" ] || break
    index=${command_file%.command}
    cat "$command_file" >> "$scratch/commands"
    PATH="$PWD/$bin:$PATH" bash -c "$(cat "$command_file")" > "$index.actual" 2>&1 || true
    if ! matches "$index.expected" "$index.actual" > "$index.mismatch"; then
      fail "$doc:$(cat "$index.line"): \$ $(cat "$command_file"): output differs: $(cat "$index.mismatch")"
    fi
    commands=$((commands + 1))
  done
  echo "docs-check: $doc: $blocks snippet(s) match their files, $commands command(s) print what the doc shows"
}

check_snippets() {
  local path count=0 failing=0
  while IFS= read -r path; do
    grep -qxF "$path" "$scratch/shown" || fail "$path is not shown in any doc"
    if [[ "$path" == */errors/* ]]; then
      if "$bin/hl" check "$path" > /dev/null 2>&1; then
        fail "$path is an error snippet, but hl check passes"
      fi
      grep -qF "$path" "$scratch/commands" || fail "$path is an error snippet, but no doc runs it to show its error"
      failing=$((failing + 1))
    else
      "$bin/hl" check "$path" > "$scratch/snippet.log" 2>&1 || fail "$path does not pass hl check: $(cat "$scratch/snippet.log")"
    fi
    count=$((count + 1))
  done < <(find "$snippets" -name '*.hl' | sort)
  echo "docs-check: $count snippet(s): $((count - failing)) pass hl check on every target, $failing fail with the error a doc shows"
}

run_checks() {
  rm -f "$scratch/shown" "$scratch/commands"
  touch "$scratch/shown" "$scratch/commands"
  check_lists
  check_constants
  for doc in $docs; do
    check_doc "$doc"
  done
  if [ "${DOCS_CHECK_SELF:-1}" = 1 ]; then
    check_snippets
  fi
}

mutant() {
  local name=$1 expected=$2 edit=$3 work=$scratch/mutants/$1
  mkdir -p "$work"
  cp "$language" "$work/LANGUAGE.md"
  perl -0pi -e "$edit" "$work/LANGUAGE.md"
  cmp -s "$language" "$work/LANGUAGE.md" && fail "mutant $name did not change $language"
  if DOCS_CHECK_SELF=0 DOCS_CHECK_FILES="$work/LANGUAGE.md" DOCS_CHECK_LANGUAGE="$work/LANGUAGE.md" \
    DOCS_CHECK_SCRATCH="$work/scratch" "$0" > "$work/log" 2>&1; then
    fail "mutant $name passed the check"
  fi
  grep -qF -- "$expected" "$work/log" || fail "mutant $name failed for another reason: $(cat "$work/log")"
  echo "docs-check: caught mutant $name: $expected"
}

run_checks
if [ "${DOCS_CHECK_SELF:-1}" = 1 ]; then
  rm -rf "$scratch/mutants"
  mutant error_text "output differs" 's/(\n\$ hl check [^\n]*\n[^\n]*?): error: /$1: error:  /'
  mutant snippet_body "but the block differs from the file" 's/(```hl docs\/snippets\/[^\n]+\n[^\n]*\n)/$1  \n/'
  mutant unnamed_block "an hl block names no file" 's/```hl docs\/snippets\//```hl\n```\n```hl docs\/snippets\//'
  mutant keyword_dropped "differs from KEYWORDS" 's/(\nKeywords: [^\n]*?) `requires`/$1/'
  mutant terminal_added "the grammar's terminals" 's/(```ebnf\n)/$1while = "while" ;\n/'
  mutant constant_value "src says" 's/(`NESTING_DEPTH_MAX` = )64/${1}65/'
  mutant prose_error "quoted outside a console block" 's/(\n## )/\nfile.hl:1:1: error: made up\n$1/'
fi
echo "docs-check: ok"
