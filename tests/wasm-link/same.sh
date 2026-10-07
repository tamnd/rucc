#!/bin/sh
# Fails when the module that the linker inside rucc wrote is not byte for byte the module that
# `wasm-ld` wrote for the same line (#2867), DWARF sections and all (#3149).
#
# usage: tests/wasm-link/same.sh <module from wasm-ld> <module from rucc -fuse-ld=rucc>
#
# The two modules must have the same file name in two directories, because each linker writes the
# file name in the name section.

set -eu

[ $# -eq 2 ] || { echo "usage: $0 <module from wasm-ld> <module from rucc -fuse-ld=rucc>" >&2; exit 2; }

if ! cmp "$1" "$2"; then
	echo "the linker inside rucc wrote $2, and it is not the module that wasm-ld wrote, $1"
	exit 1
fi
