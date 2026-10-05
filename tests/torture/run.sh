#!/bin/sh
# The execute tests of GCC's torture suite for one target: each program built and linked by rucc
# at -O0 and at -O2 with the flags that GCC's own driver gives it, and run, each one held to a zero
# exit status. A program that finds a wrong result calls `abort`, so nothing else is compared.
#
# usage: tests/torture/run.sh <rucc> <triple> [<gcc source tree>]
#
# Without a source tree the GCC release below is downloaded and checked against its hash, and only
# the execute directory and the five files from gcc.dg that it includes come out of it. RUNNER
# goes in front of each program, and the wasm job sets it to `wasmtime run --dir=.`. Each program
# runs in a directory of its own, because some of them write a file there. JOBS is how many
# programs are built and run at the same time, and the default is the number of processors.
#
# The effective targets that GCC's directives ask for are written down below for each target this
# runs on. A program that needs one that the target does not have is skipped and counted, as GCC
# does.
#
# tests/torture/exclude-<triple>.txt names the programs left out, one per line, as the name, the
# level or `all`, and the issue that tracks it. A line with no issue number fails the run, and so
# does an excluded program that passes, so the list can only shrink. A program that the platform
# cannot run as it is written has an issue too, which says why.

set -eu

# Some of the sources have bytes that are not UTF-8 in their comments, and sed stops on them in a
# UTF-8 locale.
LC_ALL=C
export LC_ALL

[ $# -eq 2 ] || [ $# -eq 3 ] || { echo "usage: $0 <rucc> <triple> [<gcc source tree>]" >&2; exit 2; }
rucc=$1
triple=$2
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
version=16.2.0
sha256=e6738e29597f733270731aa90600f37ffdc045079dfc27ec7e8192cc81085c3e
exclude=$here/exclude-$triple.txt

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM

if [ $# -eq 3 ]; then
	tree=$3
else
	curl -fsSL -o "$out/gcc.tar.xz" "https://ftp.gnu.org/gnu/gcc/gcc-$version/gcc-$version.tar.xz"
	echo "$sha256  $out/gcc.tar.xz" | sha256sum -c --quiet -
	testsuite=gcc-$version/gcc/testsuite
	tar -xJf "$out/gcc.tar.xz" -C "$out" "$testsuite/gcc.c-torture/execute" \
		"$testsuite/gcc.dg/pr98304-1.c" "$testsuite/gcc.dg/tree-ssa/pr105777.c" \
		"$testsuite/gcc.dg/tree-ssa/pr30314.c" "$testsuite/gcc.dg/tree-ssa/pr109938.c" \
		"$testsuite/gcc.dg/tree-ssa/pr109986.c"
	rm "$out/gcc.tar.xz"
	tree=$out/gcc-$version
fi
suite=$tree/gcc/testsuite/gcc.c-torture/execute
[ -d "$suite" ] || { echo "no gcc/testsuite/gcc.c-torture/execute under $tree" >&2; exit 2; }

# The effective targets that are false, and what each program is linked with. On wasm32 a program
# cannot write code at run time and cannot read the call stack, so trampolines, return_address and
# untyped_assembly are false. nonlocal_goto is false until nested functions (#2943) and
# `__builtin_setjmp` (#2944) are built on wasm. WASI has no signals and no mmap, and a pointer is
# 32 bits. The stack is 8 MiB, which is what a Linux program has, and the long double conversions
# of printf are in a library of their own.
case $triple in
wasm32-wasi*)
	false_targets='trampolines nonlocal_goto return_address untyped_assembly signal mmap lp64 dfp dfprt run_expensive_tests'
	stack=8388608
	link="-lc-printscan-long-double -Wl,-z,stack-size=$stack"
	;;
*)
	echo "the effective targets of $triple are not written down in $0" >&2
	exit 2
	;;
esac

failed=0
passed=0
skipped=0
excluded=0

if [ -f "$exclude" ]; then
	if grep -v '^#' "$exclude" | grep -v '^$' | grep -v '#[0-9]' >"$out/bare"; then
		echo "exclusions in $exclude with no issue number:"
		sed 's/^/    /' "$out/bare"
		failed=$((failed + 1))
	fi
fi

mkdir "$out/run"
(
	cd "$suite"
	for source in *.c ieee/*.c builtins/*.c; do
		case $source in
		*-lib.c) continue ;;
		esac
		for level in 0 2; do
			echo "${source%.c} $level"
		done
	done
) >"$out/cases"

jobs=${JOBS:-$(getconf _NPROCESSORS_ONLN)}
# A program that case.sh could not finish has no line in the results, so the run fails.
if ! TORTURE_RUCC=$rucc TORTURE_TRIPLE=$triple TORTURE_SUITE=$suite TORTURE_OUT=$out/run \
	TORTURE_FALSE=$false_targets TORTURE_STACK=$stack TORTURE_LINK=$link \
	xargs -P "$jobs" -n 2 sh "$here/case.sh" <"$out/cases" >"$out/results"; then
	echo "case.sh stopped on a program, and the message is above"
	failed=$((failed + 1))
fi

listed() {
	[ -f "$exclude" ] && grep -q -E "^$1 ($2|all)( |\$)" "$exclude"
}

sort "$out/results" >"$out/sorted"
while read -r name level result; do
	case $result in
	skipped*)
		skipped=$((skipped + 1))
		continue
		;;
	esac
	if listed "$name" "$level"; then
		if [ "$result" = ok ]; then
			printf '%s %s  passes but is excluded, take it off the list\n' "$name" "$level"
			failed=$((failed + 1))
		else
			excluded=$((excluded + 1))
		fi
	elif [ "$result" = ok ]; then
		passed=$((passed + 1))
	else
		printf '%s %s  %s\n' "$name" "$level" "$result"
		head -5 "$out/run/$(printf '%s\n' "$name" | tr / _)$level.log" | sed 's/^/    /'
		failed=$((failed + 1))
	fi
done <"$out/sorted"

echo "torture on $triple: $passed passed, $failed failed, $excluded excluded, $skipped skipped"
[ "$failed" -eq 0 ]
