#!/bin/sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
binary=$1
root=$2
tools=${EVAL_OUT:-$here/../output/eval}/tools
patchelf_url=https://github.com/NixOS/patchelf/releases/download/0.18.0/patchelf-0.18.0-x86_64.tar.gz
patchelf_sha256=ce84f2447fb7a8679e58bc54a20dc2b01b37b5802e12c57eece772a6f14bf3f0
mount=/opt/nanoeval-agent/lib
mkdir -p "$tools"
if [ ! -x "$tools/bin/patchelf" ]; then
  curl -fsSL -o "$tools/patchelf.tgz" "$patchelf_url"
  echo "$patchelf_sha256  $tools/patchelf.tgz" | sha256sum -c - >/dev/null
  tar -xzf "$tools/patchelf.tgz" -C "$tools"
fi
rm -rf "$root"
mkdir -p "$root/lib" "$root/bin"
loader=$(readelf -l /bin/sh | sed -n 's/.*interpreter: \(.*\)]/\1/p')
cp -L "$loader" "$root/lib/ld-linux-x86-64.so.2"
cp "$binary" "$root/bin/hl-agent"
"$tools/bin/patchelf" --set-interpreter "$mount/ld-linux-x86-64.so.2" --set-rpath "$mount" "$root/bin/hl-agent"
for needed in $("$tools/bin/patchelf" --print-needed "$root/bin/hl-agent"); do
  [ "$needed" = ld-linux-x86-64.so.2 ] && continue
  library=$(ldconfig -p | awk -v name="$needed" '$1 == name && /x86-64/ { print $NF; exit }')
  [ -n "$library" ] || { echo "agent-root.sh: hl-agent needs $needed, which this host does not have" >&2; exit 1; }
  cp -L "$library" "$root/lib/$needed"
done
sha256sum "$binary" | cut -d' ' -f1 > "$root/hl-agent.sha256"
