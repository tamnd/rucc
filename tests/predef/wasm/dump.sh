#!/bin/sh
# Writes the predefined macros of clang 23 from wasi-sdk 34 for each wasm row, one file for each
# row, at the default CPU of rucc, which is lime1. The test in crates/rucc/tests/wasm_macros.rs
# compares rucc with these files and with approved.txt.
#
# usage: tests/predef/wasm/dump.sh <wasi-sdk directory>

set -eu

[ $# -eq 1 ] || {
	echo "usage: $0 <wasi-sdk directory>" >&2
	exit 2
}
clang=$1/bin/clang
here=$(dirname -- "$0")

for row in wasm32-wasip1 wasm32-wasip2 wasm32-wasip3 wasm32-unknown-unknown; do
	"$clang" --target="$row" -mcpu=lime1 -E -dM -x c /dev/null | LC_ALL=C sort >"$here/$row.txt"
done
