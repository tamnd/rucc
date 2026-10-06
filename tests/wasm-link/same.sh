#!/bin/sh
# Fails when the module that the linker inside rucc wrote is not byte for byte the module that
# `wasm-ld` wrote for the same line (#2867).
#
# usage: tests/wasm-link/same.sh <module from wasm-ld> <module from rucc -fuse-ld=rucc>
#
# The linker inside rucc does not copy the DWARF sections of its inputs yet, so they come off the
# `wasm-ld` module before the comparison, with `wasm-tools strip`. WASM_TOOLS, when it is set, is
# the wasm-tools program to use. The two modules must have the same file name in two directories,
# because each linker writes the file name in the name section.

set -eu

[ $# -eq 2 ] || { echo "usage: $0 <module from wasm-ld> <module from rucc -fuse-ld=rucc>" >&2; exit 2; }
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM

${WASM_TOOLS:-wasm-tools} strip -d '\.debug.*' "$1" -o "$out/stripped.wasm"
if ! cmp "$out/stripped.wasm" "$2"; then
	echo "the linker inside rucc wrote $2, and it is not the module that wasm-ld wrote, $1"
	exit 1
fi
