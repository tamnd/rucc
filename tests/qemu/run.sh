#!/bin/sh
# Programs held against gcc for another architecture, built by rucc and run under qemu user mode.
#
# usage: tests/qemu/run.sh <rucc> <triple>
#
# The fixtures are the ones `cargo xtask wide`, `divide` and `tail` hold against the system compiler
# on this machine. Each prints what it computed rather than anything about the machine it computed
# it on, so gcc for the target is the reference, run under the same emulator, and every level rucc
# builds at has to print the same thing. A failure seen only here leaves qemu a suspect as well as
# rucc, and the reference running under it too is what tells the two apart.
#
# rucc links as well as compiles, against the Debian cross packages under /usr/<triple>, because a
# cross compile that only works when somebody else links it is not the one a user runs.

set -eu

[ $# -eq 2 ] || { echo "usage: $0 <rucc> <triple>" >&2; exit 2; }
rucc=$1
triple=$2
here=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
prefix=/usr/$triple
qemu="qemu-${triple%%-*}"
command -v "$qemu" >/dev/null || { echo "no $qemu on this machine" >&2; exit 2; }
[ -d "$prefix/lib" ] || { echo "no cross libc under $prefix" >&2; exit 2; }
export QEMU_LD_PREFIX="$prefix"

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM
failed=0
passed=0

for fixture in wide/arithmetic.c divide/constants.c tail/calls.c; do
	name=${fixture%%/*}
	source=$here/tests/$fixture
	"$triple-gcc" -O2 -o "$out/$name-reference" "$source"
	"$qemu" "$out/$name-reference" >"$out/$name-reference.txt"
	for level in 0 1 2; do
		binary=$out/$name-O$level
		if ! $rucc --target="$triple" -O$level -o "$binary" "$source" 2>"$out/build.log"; then
			printf '%-8s -O%s  did not build\n' "$name" "$level"
			sed 's/^/    /' "$out/build.log"
			failed=$((failed + 1))
			continue
		fi
		if ! "$qemu" "$binary" >"$binary.txt" 2>&1; then
			printf '%-8s -O%s  exited nonzero\n' "$name" "$level"
			tail -5 "$binary.txt" | sed 's/^/    /'
			failed=$((failed + 1))
			continue
		fi
		if diff -u "$out/$name-reference.txt" "$binary.txt" >"$out/diff"; then
			printf '%-8s -O%s  ok, %s lines\n' "$name" "$level" "$(wc -l <"$binary.txt")"
			passed=$((passed + 1))
		else
			printf '%-8s -O%s  printed something else than gcc\n' "$name" "$level"
			head -20 "$out/diff" | sed 's/^/    /'
			failed=$((failed + 1))
		fi
	done
done

printf '%s: %d ran, %d failed\n' "$triple" "$passed" "$failed"
[ "$failed" -eq 0 ]
