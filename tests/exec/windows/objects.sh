#!/usr/bin/env bash
# Compiles every program here to an object at -O0 and -O2 and prints a hash of each, one line per
# object, so that two machines can be checked for writing the same bytes from the same source.
#
#   tests/exec/windows/objects.sh CC > hashes.txt
#
# CI runs this with rucc on Windows and on Linux and compares the two lists. A cross compiler that
# writes a different object depending on the machine it runs on is a bug even when both run.
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 CC" >&2
    exit 2
fi
read -r -a cc <<<"$1"

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# From this directory and with relative names, so no path of either machine can end up in an object.
cd "$here"
for src in *.c; do
    name=${src%.c}
    flags=$(sed -n '1,3s|^/\* flags: \(.*\) \*/$|\1|p' "$src")
    read -r -a flags <<<"$flags"
    for level in -O0 -O2; do
        "${cc[@]}" "$level" ${flags[@]+"${flags[@]}"} -c "$src" -o "$work/$name.obj"
        hash=$(sha256sum "$work/$name.obj" | cut -d' ' -f1)
        echo "$hash $name $level"
    done
done
