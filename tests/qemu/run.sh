#!/bin/sh
# Programs held against gcc for another architecture, built by rucc and run under qemu user mode.
#
# usage: tests/qemu/run.sh <rucc> <triple>
#
# The first fixtures are the ones `cargo xtask wide`, `divide` and `tail` hold against the system compiler
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

# The registers a call keeps only half of. AAPCS64 preserves the low sixty four bits of v8 to v15,
# and clobber.c, built by gcc, writes the rest and restores only what it owes. values.c keeps
# sixteen byte values live across calls to it, built by gcc as the reference and by rucc at every
# level, and each is linked against gcc's clobber.o so the callee is always one that takes the ABI
# at its word.
preserved=$here/tests/qemu/preserved
"$triple-gcc" -O2 -c -o "$out/clobber.o" "$preserved/clobber.c"
"$triple-gcc" -O2 -o "$out/preserved-reference" "$preserved/values.c" "$out/clobber.o"
"$qemu" "$out/preserved-reference" >"$out/preserved-reference.txt"
for level in 0 1 2; do
	binary=$out/preserved-O$level
	if ! $rucc --target="$triple" -O$level -c -o "$binary.o" "$preserved/values.c" \
		2>"$out/build.log" || ! "$triple-gcc" -o "$binary" "$binary.o" "$out/clobber.o" \
		2>>"$out/build.log"; then
		printf 'preserved -O%s  did not build\n' "$level"
		sed 's/^/    /' "$out/build.log"
		failed=$((failed + 1))
	elif ! "$qemu" "$binary" >"$binary.txt" 2>&1; then
		printf 'preserved -O%s  exited nonzero\n' "$level"
		tail -5 "$binary.txt" | sed 's/^/    /'
		failed=$((failed + 1))
	elif diff -u "$out/preserved-reference.txt" "$binary.txt" >"$out/diff"; then
		printf 'preserved -O%s  ok, %s lines\n' "$level" "$(wc -l <"$binary.txt")"
		passed=$((passed + 1))
	else
		printf 'preserved -O%s  printed something else than gcc\n' "$level"
		head -20 "$out/diff" | sed 's/^/    /'
		failed=$((failed + 1))
	fi
done

# The signature corpus from `cargo xtask abi-signatures`, which is where the calling convention is
# checked rather than the arithmetic. The caller and the callee are each built by rucc and by gcc,
# and every pairing is linked and run, so a pairing that fails is the two compilers disagreeing
# about where an argument or a return value goes: AAPCS64's registers, its homogeneous float
# aggregates, the hidden result pointer in x8, a composite copied to memory when it is larger than
# sixteen bytes, and the variadic list. report.c holds the counter and the one call to printf, and
# gcc builds it for every pairing so a failure is never about it.
abi=$here/tests/abi-signatures
"$triple-gcc" -O2 -c -o "$out/report.o" "$abi/report.c"
for side in caller callee; do
	"$triple-gcc" -O2 -c -o "$out/$side-gcc.o" "$abi/$side.c"
	for level in 0 2; do
		if ! $rucc --target="$triple" -O$level -c -o "$out/$side-rucc-O$level.o" "$abi/$side.c" \
			2>"$out/build.log"; then
			printf 'abi %-6s -O%s  did not build\n' "$side" "$level"
			sed 's/^/    /' "$out/build.log"
			failed=$((failed + 1))
		fi
	done
done
for caller in gcc rucc-O0 rucc-O2; do
	for callee in gcc rucc-O0 rucc-O2; do
		[ "$caller" = gcc ] && [ "$callee" = gcc ] && continue
		pairing="caller $caller, callee $callee"
		binary=$out/abi-$caller-$callee
		[ -f "$out/caller-$caller.o" ] && [ -f "$out/callee-$callee.o" ] || continue
		if ! "$triple-gcc" -o "$binary" "$out/caller-$caller.o" "$out/callee-$callee.o" \
			"$out/report.o" 2>"$out/build.log"; then
			printf 'abi %s  did not link\n' "$pairing"
			sed 's/^/    /' "$out/build.log"
			failed=$((failed + 1))
		elif "$qemu" "$binary" >"$binary.txt" 2>&1; then
			printf 'abi %s  ok\n' "$pairing"
			passed=$((passed + 1))
		else
			printf 'abi %s  disagreed\n' "$pairing"
			head -20 "$binary.txt" | sed 's/^/    /'
			failed=$((failed + 1))
		fi
	done
done

printf '%s: %d ran, %d failed\n' "$triple" "$((passed + failed))" "$failed"
[ "$failed" -eq 0 ]
