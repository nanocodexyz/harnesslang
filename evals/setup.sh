#!/bin/sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${EVAL_OUT:-$here/../output/eval}
nanoeval_commit=556e56b7980375173705d54776a3c5da4637e8c3
nanocode_commit=c7679e3e8
patchelf_url=https://github.com/NixOS/patchelf/releases/download/0.18.0/patchelf-0.18.0-x86_64.tar.gz
patchelf_sha256=ce84f2447fb7a8679e58bc54a20dc2b01b37b5802e12c57eece772a6f14bf3f0
codex_version=0.169.0
codex_url=https://github.com/openai/codex/releases/download/rust-v$codex_version/codex-x86_64-unknown-linux-musl.tar.gz
codex_sha256=9af4d751fce183faedea629437d5252dbf458eeb04130cf4f97be844621ea985
codex_host_url=https://github.com/openai/codex/releases/download/rust-v$codex_version/codex-code-mode-host-x86_64-unknown-linux-musl.tar.gz
codex_host_sha256=cbed6a8d25e8268792b757767fbc76d4c62e84bcf8ee4a63f97547f0de3aaceb
mkdir -p "$out/src" "$out/tasks" "$out/tools"

if [ ! -d "$out/src/nanoeval" ]; then git clone -q https://github.com/nanocodexyz/nanoeval "$out/src/nanoeval"; fi
git -C "$out/src/nanoeval" checkout -q "$nanoeval_commit"
rm -rf "$out/tasks/nanoeval"
cp -R "$out/src/nanoeval/tasks" "$out/tasks/nanoeval"
python3 "$here/fetch.py" terminal-bench-2.1 "$out/tasks/terminal-bench-2.1" $(awk '$1 == "terminal-bench-2.1" { print $2 }' "$here/suite.txt" "$here/suite2.txt")

if [ ! -x "$out/tools/bin/patchelf" ]; then
  curl -fsSL -o "$out/tools/patchelf.tgz" "$patchelf_url"
  echo "$patchelf_sha256  $out/tools/patchelf.tgz" | sha256sum -c - >/dev/null
  tar -xzf "$out/tools/patchelf.tgz" -C "$out/tools"
fi

nanocode_dir=${NANOCODE_DIR:-$out/src/nanocode}
if [ ! -d "$nanocode_dir" ]; then git clone -q https://github.com/nanocodexyz/nanocode "$nanocode_dir"; fi
git -C "$nanocode_dir" checkout -q "$nanocode_commit"
(cd "$nanocode_dir" && nice -n 15 cargo build --release -j2 --locked -p nanocode-cli --bin nanocode)

root=$out/agent-root
rm -rf "$root"
mkdir -p "$root/lib" "$root/bin"
loader=$(readelf -l /bin/sh | sed -n 's/.*interpreter: \(.*\)]/\1/p')
cp -L "$loader" "$root/lib/ld-linux-x86-64.so.2"
for library in libc.so.6 libm.so.6 libgcc_s.so.1; do
  cp -L "$(ldconfig -p | awk -v name="$library" '$1 == name && /x86-64/ { print $NF; exit }')" "$root/lib/$library"
done
install_agent() {
  cp "$1" "$root/bin/$2"
  "$out/tools/bin/patchelf" --set-interpreter /opt/nanoeval-agent/lib/ld-linux-x86-64.so.2 --set-rpath /opt/nanoeval-agent/lib "$root/bin/$2"
  for needed in $("$out/tools/bin/patchelf" --print-needed "$root/bin/$2"); do
    [ -f "$root/lib/$needed" ] || { echo "setup.sh: $2 needs $needed, which agent-root does not carry" >&2; exit 1; }
  done
}
install_agent "$nanocode_dir/target/release/nanocode" nanocode
if [ -n "${HL_AGENT:-}" ]; then install_agent "$HL_AGENT" hl-agent; fi
if [ -n "${CLAUDE_CODE_VERSION:-}" ]; then
  for build in linux-x64 linux-x64-musl; do
    mkdir -p "$out/tools/claude-code-$build"
    (cd "$out/tools/claude-code-$build" && npm pack -q "@anthropic-ai/claude-code-$build@$CLAUDE_CODE_VERSION" > /dev/null && tar -xzf anthropic-ai-claude-code-$build-$CLAUDE_CODE_VERSION.tgz)
  done
  cp "$out/tools/claude-code-linux-x64/package/claude" "$root/bin/claude-code"
  cp "$out/tools/claude-code-linux-x64-musl/package/claude" "$root/bin/claude-code-musl"
fi
if [ "${CODEX:-0}" = 1 ]; then
  codex_dir=$out/tools/codex-$codex_version
  if [ ! -x "$codex_dir/codex-x86_64-unknown-linux-musl" ]; then
    mkdir -p "$codex_dir"
    curl -fsSL -o "$codex_dir/codex.tgz" "$codex_url"
    echo "$codex_sha256  $codex_dir/codex.tgz" | sha256sum -c - >/dev/null
    tar -xzf "$codex_dir/codex.tgz" -C "$codex_dir"
  fi
  if [ ! -x "$codex_dir/codex-code-mode-host-x86_64-unknown-linux-musl" ]; then
    curl -fsSL -o "$codex_dir/host.tgz" "$codex_host_url"
    echo "$codex_host_sha256  $codex_dir/host.tgz" | sha256sum -c - >/dev/null
    tar -xzf "$codex_dir/host.tgz" -C "$codex_dir"
  fi
  cp "$codex_dir/codex-x86_64-unknown-linux-musl" "$root/bin/codex"
  cp "$codex_dir/codex-code-mode-host-x86_64-unknown-linux-musl" "$root/bin/codex-code-mode-host"
fi
ls -l "$root/bin"
