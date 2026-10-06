#!/bin/sh
# The round trip of the -S text for one wasm target (tamnd/rucc#3141): each c-testsuite
# single-exec program is compiled by rucc to text with -S and to an object with -c, at -O0 and
# -O2, and the text is then assembled by rucc with -c. The object from the text must be the object
# from the C source, byte for byte.
#
# usage: tests/wasm-asm/run.sh <rucc> <triple> [<c-testsuite checkout>]
#
# Without a checkout the suite is cloned at the commit that tests/rung0/run.sh uses. LLVM_MC, when
# it is set, is the llvm-mc of LLVM 23 or later, and each text must also assemble with it, which
# is the check that the text is in the dialect of LLVM. The object of llvm-mc is not compared,
# because it pads the sizes of its sections and adds a data count section.

set -eu

[ $# -eq 2 ] || [ $# -eq 3 ] || { echo "usage: $0 <rucc> <triple> [<c-testsuite>]" >&2; exit 2; }
rucc=$1
triple=$2
commit=5c7275656d751de0e68b2d340a95b5681858ed07

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM

if [ $# -eq 3 ]; then
	suite=$3
else
	suite=$out/c-testsuite
	git init -q "$suite"
	git -C "$suite" fetch -q --depth 1 https://github.com/c-testsuite/c-testsuite.git "$commit"
	git -C "$suite" checkout -q FETCH_HEAD
fi
cases=$suite/tests/single-exec
[ -d "$cases" ] || { echo "no tests/single-exec under $suite" >&2; exit 2; }

failed=0
passed=0

for source in "$cases"/*.c; do
	name=$(basename "$source" .c)
	# The standard of the tags of each program, as in tests/rung0/run.sh.
	std=gnu11
	for tag in 89 99; do
		if [ -f "$source.tags" ] && grep -qw "c$tag" "$source.tags"; then
			std=gnu$tag
		fi
	done
	for level in 0 2; do
		base=$out/$name-O$level
		flags="--target=$triple -std=$std -O$level"
		result=ok
		if ! $rucc $flags -S -o "$base.s" "$source" >"$out/build.log" 2>&1; then
			result='gave no text'
		elif ! $rucc $flags -c -o "$base.c.o" "$source" >"$out/build.log" 2>&1; then
			result='gave no object'
		elif ! $rucc --target="$triple" -c -o "$base.s.o" "$base.s" >"$out/build.log" 2>&1; then
			result='the text did not assemble'
		elif ! cmp -s "$base.c.o" "$base.s.o"; then
			result='the text assembled to another object'
		elif [ -n "${LLVM_MC:-}" ] && ! $LLVM_MC -triple="$triple" -filetype=obj -o "$base.mc.o" "$base.s" >"$out/build.log" 2>&1; then
			result='llvm-mc did not assemble the text'
		fi
		if [ "$result" = ok ]; then
			passed=$((passed + 1))
		else
			printf '%s -O%s  %s\n' "$name" "$level" "$result"
			head -5 "$out/build.log" | sed 's/^/    /'
			failed=$((failed + 1))
		fi
	done
done

echo "-S round trip on $triple: $passed passed, $failed failed"
[ "$failed" -eq 0 ]
