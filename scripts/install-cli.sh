#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
bin=${HL_CLI_BIN:-$HOME/hl-cli/bin}
target=hosts/native/target

cargo build --release --locked
CARGO_TARGET_DIR="$(pwd)/$target" HL_BIN="$(pwd)/target/release/hl" cargo build --release --locked --manifest-path hosts/native/Cargo.toml
mkdir -p "$bin"
for file in "$target/release/hl-host" scripts/hl; do
  name=$(basename "$file")
  cp "$file" "$bin/.$name.new"
  chmod 755 "$bin/.$name.new"
  mv -f "$bin/.$name.new" "$bin/$name"
done
echo "install-cli: $bin/hl and $bin/hl-host from $(git rev-parse --short HEAD)"
