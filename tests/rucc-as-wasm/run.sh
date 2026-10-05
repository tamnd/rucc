#!/bin/sh
# Rung 0 compiled twice, once by native rucc and once by rucc built as a WebAssembly module and run
# in Wasmtime, and every object compared byte for byte. This is the check of #2862: rucc as a wasm
# module is the same compiler, so its output is the same bytes.
#
# usage: tests/rucc-as-wasm/run.sh <rucc> <rucc.wasm> <triple> [<c-testsuite checkout>]
#
# The objects are for <triple>, and its sysroot has to be in the cache of the native rucc already,
# which `rucc --fetch <triple>` does. The module gets the cache at /.cache/rucc, which is where it
# looks when no variable names one, so the run sets no environment variable. Nothing is linked or
# run, because a wasm module cannot start a linker.
#
# The programs are compiled in one command for each level and each standard, so that Wasmtime
# compiles the module six times and not four hundred times.

set -eu

[ $# -eq 3 ] || [ $# -eq 4 ] || {
	echo "usage: $0 <rucc> <rucc.wasm> <triple> [<c-testsuite>]" >&2
	exit 2
}
rucc=$1
module=$2
triple=$3
wasmtime=${WASMTIME:-wasmtime}
commit=5c7275656d751de0e68b2d340a95b5681858ed07

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM

if [ $# -eq 4 ]; then
	suite=$(CDPATH='' cd -- "$4" && pwd)
else
	suite=$out/c-testsuite
	git init -q "$suite"
	git -C "$suite" fetch -q --depth 1 https://github.com/c-testsuite/c-testsuite.git "$commit"
	git -C "$suite" checkout -q FETCH_HEAD
fi
cases=$suite/tests/single-exec
[ -d "$cases" ] || { echo "no tests/single-exec under $suite" >&2; exit 2; }

# The cache the native rucc uses, which is section 13.2's order on a Unix host.
cache=${RUCC_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/rucc}
[ -d "$cache/sysroots/$triple" ] || {
	echo "no sysroot for $triple in $cache, run \`$rucc --fetch $triple\` first" >&2
	exit 2
}

# The standard each program asks for in its tags, as tests/rung0/run.sh reads it.
for source in "$cases"/*.c; do
	std=gnu11
	for tag in 89 99; do
		if [ -f "$source.tags" ] && grep -qw "c$tag" "$source.tags"; then
			std=gnu$tag
		fi
	done
	basename "$source" >>"$out/$std.list"
done

failed=0
compared=0
for level in 0 2; do
	for list in "$out"/*.list; do
		std=$(basename "$list" .list)
		mkdir -p "$out/native/$std-O$level" "$out/wasm/$std-O$level"
		# Both compilers get the same command, with the same absolute names, so that every name
		# that goes into an object is the same. A program that one of them refuses is an object that
		# is missing on that side, and the comparison below reports it.
		# The names in c-testsuite have no spaces, so the split is the one wanted.
		# shellcheck disable=SC2046
		set -- $(sed "s|^|$cases/|" "$list")
		(cd "$out/native/$std-O$level" &&
			"$rucc" --target="$triple" -std="$std" -O$level -c "$@" >/dev/null 2>&1) || true
		(cd "$out/wasm/$std-O$level" &&
			"$wasmtime" run --dir=. --dir="$cases" --dir="$cache::/.cache/rucc" "$module" \
				--target="$triple" -std="$std" -O$level -c "$@" >/dev/null 2>&1) || true
		for name in "$@"; do
			name=$(basename "$name" .c)
			object=$name.o
			native=$out/native/$std-O$level/$object
			wasm=$out/wasm/$std-O$level/$object
			if [ ! -f "$native" ] && [ ! -f "$wasm" ]; then
				continue
			fi
			compared=$((compared + 1))
			if [ ! -f "$wasm" ]; then
				printf '%s -O%s  native rucc wrote an object and rucc.wasm did not\n' "$name" "$level"
				failed=$((failed + 1))
			elif [ ! -f "$native" ]; then
				printf '%s -O%s  rucc.wasm wrote an object and native rucc did not\n' "$name" "$level"
				failed=$((failed + 1))
			elif ! cmp -s "$native" "$wasm"; then
				printf '%s -O%s  the objects differ\n' "$name" "$level"
				failed=$((failed + 1))
			fi
		done
	done
done

echo "rucc as wasm on $triple: $compared objects compared, $failed differ"
[ "$compared" -gt 0 ] && [ "$failed" -eq 0 ]
