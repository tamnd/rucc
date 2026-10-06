#!/bin/sh
# Rung 0 of spec/14-target-ladder.md for one target: the c-testsuite single-exec programs, built
# and linked by rucc at -O0 and -O2 under the GNU dialect of the standard each one's tags name,
# and run on this machine, each held to the output the suite expects and to a zero exit status.
#
# usage: tests/rung0/run.sh <rucc> <triple> [<c-testsuite checkout>]
#
# Without a checkout the suite is cloned at the commit below, so that a new upstream test cannot
# turn a run red on its own. The programs run directly, so this is for a target this machine can
# execute, i686-linux-gnu on an x86_64 Linux host being the first. RUNNER, when it is set, goes in
# front of each program, which is how i686-windows-gnu runs under Wine: RUNNER=wine.
#
# VALIDATE, when it is set, is a command that checks one output file, and the wasm job sets it to
# `wasm-tools validate`. Each program is then also compiled with -c, and the object and the linked
# program both go to that command. A file that it rejects fails the program.
#
# SAME_LINK, when it is set, is a command that compares two wasm modules, and the wasm job sets it
# to tests/wasm-link/same.sh. Each program is then also linked by the linker inside rucc, with
# -fuse-ld=rucc, and the command gets the module from `wasm-ld` and that module. The second module
# also runs and must print what the suite expects.
#
# tests/rung0/exclude-<triple>.txt names the programs left out, one per line, each with the issue
# that tracks it. A line with no issue number fails the run, and so does an excluded program that
# passes, so the list can only shrink.

set -eu

[ $# -eq 2 ] || [ $# -eq 3 ] || { echo "usage: $0 <rucc> <triple> [<c-testsuite>]" >&2; exit 2; }
rucc=$1
triple=$2
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
commit=5c7275656d751de0e68b2d340a95b5681858ed07
exclude=$here/exclude-$triple.txt

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM

if [ $# -eq 3 ]; then
	suite=$3
else
	suite=$out/c-testsuite
	git init -q "$suite"
	# Git for Windows checks text out with CR LF line ends by default, and the .expected files
	# are compared byte for byte, so the checkout keeps them as the suite wrote them.
	git -C "$suite" config core.autocrlf false
	git -C "$suite" fetch -q --depth 1 https://github.com/c-testsuite/c-testsuite.git "$commit"
	git -C "$suite" checkout -q FETCH_HEAD
fi
cases=$suite/tests/single-exec
[ -d "$cases" ] || { echo "no tests/single-exec under $suite" >&2; exit 2; }

failed=0
passed=0
skipped=0

# Every exclusion has to say which issue it waits on.
if [ -f "$exclude" ]; then
	if grep -v '^#' "$exclude" | grep -v '^$' | grep -v '#[0-9]' >"$out/bare"; then
		echo "exclusions in $exclude with no issue number:"
		sed 's/^/    /' "$out/bare"
		failed=$((failed + 1))
	fi
fi

# A Windows program is named with .exe, so that Windows and Wine both take it for one, and its
# output has CR LF line ends, which come off before the comparison. On windows-gnu the C library
# is UCRT, whose printf reads a long double as the eight bytes of a double where mingw's long
# double is the x87 one, so the %Lf lines come out wrong unless the program uses mingw's own
# printf, which is what __USE_MINGW_ANSI_STDIO asks for. MinGW GCC prints the same wrong lines
# without it. On wasi, printf in wasi-libc formats a long double only when the program is linked
# with -lc-printscan-long-double, and otherwise it stops the program with a message that names the
# flag. clang needs the same flag.
suffix=
defines=
libraries=
case $triple in
*-windows-*) suffix=.exe ;;
esac
case $triple in
*-windows-gnu*) defines=-D__USE_MINGW_ANSI_STDIO=1 ;;
wasm32-wasi*) libraries=-lc-printscan-long-double ;;
esac

mkdir "$out/in"

excluded() {
	[ -f "$exclude" ] && grep -q "^$1 " "$exclude"
}

for source in "$cases"/*.c; do
	name=$(basename "$source" .c)
	# Each program is written to the standard its tags name, and C23 reads some of them
	# differently, `int f()` taking no arguments being the one that shows. The GNU dialect of
	# that standard is used because a few of the c89 programs have // comments in them.
	std=gnu11
	for tag in 89 99; do
		if [ -f "$source.tags" ] && grep -qw "c$tag" "$source.tags"; then
			std=gnu$tag
		fi
	done
	for level in 0 2; do
		binary=$out/$name-O$level$suffix
		# The module from the linker inside rucc has the same file name in another directory,
		# because the name section holds the file name.
		inside=$out/in/$name-O$level$suffix
		result=ok
		if ! $rucc --target="$triple" -std=$std $defines -O$level -o "$binary" "$source" $libraries -lm >"$out/build.log" 2>&1; then
			result='did not build'
		elif [ -n "${VALIDATE:-}" ] && ! { $rucc --target="$triple" -std=$std $defines -O$level -c -o "$out/$name.o" "$source" && $VALIDATE "$out/$name.o" && $VALIDATE "$binary"; } >"$out/build.log" 2>&1; then
			result='did not validate'
		elif ! ${RUNNER:-} "$binary" >"$binary.raw" 2>"$binary.err"; then
			result='exited nonzero'
		elif ! tr -d '\r' <"$binary.raw" | cmp -s "$source.expected" -; then
			result='printed something else'
		elif [ -n "${SAME_LINK:-}" ] && ! { $rucc --target="$triple" -std=$std $defines -O$level -fuse-ld=rucc -o "$inside" "$source" $libraries -lm && $SAME_LINK "$binary" "$inside"; } >"$out/build.log" 2>&1; then
			result='linked to another module inside rucc'
		elif [ -n "${SAME_LINK:-}" ] && ! { ${RUNNER:-} "$inside" >"$inside.raw" 2>/dev/null && tr -d '\r' <"$inside.raw" | cmp -s "$source.expected" -; }; then
			result='printed something else when linked inside rucc'
		fi
		if excluded "$name"; then
			if [ "$result" = ok ]; then
				printf '%s -O%s  passes but is excluded, take it off the list\n' "$name" "$level"
				failed=$((failed + 1))
			else
				skipped=$((skipped + 1))
			fi
		elif [ "$result" = ok ]; then
			passed=$((passed + 1))
		else
			printf '%s -O%s  %s\n' "$name" "$level" "$result"
			head -5 "$out/build.log" | sed 's/^/    /'
			failed=$((failed + 1))
		fi
	done
done

echo "rung 0 on $triple: $passed passed, $failed failed, $skipped skipped"
[ "$failed" -eq 0 ]
