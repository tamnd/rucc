#!/bin/sh
# The wasm SIMD intrinsics of rucc against those of clang (tamnd/rucc#3190). tests/wasm-simd/gen.py
# writes a program that calls every function and macro of the wasm_simd128.h that rucc ships, over
# vectors with the edge values of each lane type, and prints a hash of what each one gave back. rucc
# compiles the program at -O0 and -O2 with its own header, the program runs under RUNNER, and its
# output must be tests/wasm-simd/expected.txt, line for line.
#
# usage: tests/wasm-simd/run.sh <rucc> <triple>
#
# expected.txt is the output of the same program compiled by clang 23 of wasi-sdk 34 with its own
# header, -msimd128 and -mrelaxed-simd, and run under Wasmtime 49. With CLANG set to that clang,
# the script also compiles the program with it, and its output must be expected.txt too. RUNNER is
# `wasmtime run` when it is not set.

set -eu

[ $# -eq 2 ] || { echo "usage: $0 <rucc> <triple>" >&2; exit 2; }
rucc=$1
triple=$2
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
runner=${RUNNER:-wasmtime run}

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM

python3 "$here/gen.py" "$root/crates/rucc-session/runtime/include/wasm_simd128.h" > "$out/simd.c"

failed=0
check() {
	# $1 names the build, $2 is the module.
	if ! $runner "$2" > "$out/$1.txt"; then
		echo "$1: the program did not run to its end"
		failed=1
		return
	fi
	if ! diff "$here/expected.txt" "$out/$1.txt" > "$out/$1.diff"; then
		echo "$1: these functions differ from clang (expected, then $1):"
		sed -n 's/^[<>] //p' "$out/$1.diff" | head -40
		failed=1
		return
	fi
	echo "$1: $(wc -l < "$out/$1.txt" | tr -d ' ') functions give the answers of clang"
}

for level in 0 2; do
	"$rucc" "--target=$triple" "-O$level" -Wno-deprecated-declarations "$out/simd.c" \
		-o "$out/rucc-O$level.wasm"
	check "rucc -O$level" "$out/rucc-O$level.wasm"
done

if [ -n "${CLANG:-}" ]; then
	"$CLANG" "--target=$triple" -O2 -msimd128 -mrelaxed-simd -Wno-deprecated-declarations \
		"$out/simd.c" -o "$out/clang.wasm"
	check clang "$out/clang.wasm"
fi

exit $failed
