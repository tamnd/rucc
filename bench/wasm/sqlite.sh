#!/bin/sh
# The measurement protocol of milestone WA4 (#2866): the SQLite shell for wasm32-wasip1, built by
# rucc at -O2 and by clang from wasi-sdk 34 at -O2, timed on the same workload under each engine.
#
# usage: bench/wasm/sqlite.sh <rucc> <sqlite-autoconf directory> [<runs>]
#
# The script follows the protocol in this order:
#
# 1. It builds the two modules with the flags of tests/sqlite/wasm.sh and prints their sizes. The
#    clang module is built only when WASI_SDK_PATH names a wasi-sdk 34 directory. RUCC_FLAGS goes
#    to rucc as it is, and CLANG_FLAGS to clang. On Ubuntu the wasm-ld of wasi-sdk does not start,
#    because it needs libedit.so.0, and rucc also finds the linker through WASI_SDK_PATH. There
#    both get `-fuse-ld=/usr/lib/llvm-21/bin/wasm-ld`.
# 2. It runs each module once under each engine and compares the output with sqlite.expected,
#    which is the output of the clang module. A module that gives other answers is not timed, and
#    the script stops with status 1, because its times would mean nothing.
# 3. It records the load average of the machine.
# 4. It runs <runs> rounds, 5 when no number is given. Each round runs each module once under each
#    engine, so a machine that gets slower during the measurement slows both modules and not one
#    of them. The number for a module is its lowest CPU user time over the rounds.
# 5. It records the load average again, and prints the times and the ratio of rucc to clang.
#
# The engines are the ones of `wasmtime` and `node` that are on PATH, or the ones that ENGINES
# names. Wasmtime runs a module that `wasmtime compile` made before the rounds, so its time does
# not include the compilation. Node compiles the module in each run, through
# tests/rung0/wasi.mjs, so its time does.
#
# sqlite.sql makes a table of 2000000 rows in one transaction, indexes it, and asks five
# questions of it. Most of the time is in the B-tree code and in the virtual machine of SQLite.
# The exit criterion of WA4 is a ratio of at most 1.10 for the time under each engine and for
# the size.

set -eu

[ $# -eq 2 ] || [ $# -eq 3 ] || { echo "usage: $0 <rucc> <sqlite-autoconf directory> [<runs>]" >&2; exit 2; }
rucc=$1
sqlite=$2
runs=${3:-5}
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH='' cd -- "$here/../.." && pwd)

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM

defs="-DSQLITE_THREADSAFE=0 -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_OMIT_WAL"
emulated="-D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_GETPID -D_WASI_EMULATED_MMAN"
libraries="-lwasi-emulated-signal -lwasi-emulated-process-clocks -lwasi-emulated-getpid -lwasi-emulated-mman"
sources="$sqlite/shell.c $sqlite/sqlite3.c $root/tests/sqlite/wasi-stubs.c"

# The load average over 1, 5 and 15 minutes.
load() {
	if [ -r /proc/loadavg ]; then
		cut -d ' ' -f 1-3 /proc/loadavg
	else
		sysctl -n vm.loadavg | tr -d '{}' | sed 's/^ *//; s/ *$//'
	fi
}

# The command that runs module $2 under engine $1, without its arguments.
command_for() {
	case $1 in
	wasmtime) echo "wasmtime run --allow-precompiled $out/$2.cwasm" ;;
	node) echo "node $root/tests/rung0/wasi.mjs $out/$2.wasm" ;;
	*) echo "unknown engine $1" >&2; exit 2 ;;
	esac
}

# The CPU user time in seconds of one run of module $2 under engine $1. `times` in the subshell
# gives the time of the children of the subshell, which is the run and nothing else.
user_time() {
	# shellcheck disable=SC2046
	(
		$(command_for "$1" "$2") :memory: <"$here/sqlite.sql" >/dev/null 2>&1
		times
	) | tail -n 1 | awk '{ split($1, t, "m"); sub("s", "", t[2]); printf "%.2f\n", t[1] * 60 + t[2] }'
}

size() {
	wc -c <"$1" | tr -d ' '
}

echo "rucc: $("$rucc" --version | head -n 1)"
echo "machine: $(uname -sm)"

# shellcheck disable=SC2086
"$rucc" --target=wasm32-wasip1 ${RUCC_FLAGS:-} -O2 $defs $emulated -I"$sqlite" $sources $libraries -o "$out/rucc.wasm"
modules=rucc
if [ -n "${WASI_SDK_PATH:-}" ]; then
	# shellcheck disable=SC2086
	"$WASI_SDK_PATH/bin/clang" --target=wasm32-wasip1 --sysroot="$WASI_SDK_PATH/share/wasi-sysroot" \
		${CLANG_FLAGS:-} -O2 $defs $emulated -I"$sqlite" $sources $libraries -o "$out/clang.wasm"
	modules="rucc clang"
	echo "size: rucc $(size "$out/rucc.wasm") bytes, clang $(size "$out/clang.wasm") bytes," \
		"ratio $(echo "$(size "$out/rucc.wasm") $(size "$out/clang.wasm")" | awk '{ printf "%.3f", $1 / $2 }')"
else
	echo "size: rucc $(size "$out/rucc.wasm") bytes, no clang module because WASI_SDK_PATH is not set"
fi

if [ -z "${ENGINES:-}" ]; then
	ENGINES=
	for engine in wasmtime node; do
		if command -v "$engine" >/dev/null 2>&1; then
			ENGINES="$ENGINES $engine"
		fi
	done
fi
[ -n "$ENGINES" ] || { echo "no engine: put wasmtime or node on PATH, or set ENGINES" >&2; exit 2; }

for module in $modules; do
	case " $ENGINES " in
	*" wasmtime "*) wasmtime compile "$out/$module.wasm" -o "$out/$module.cwasm" ;;
	esac
done

failed=0
for engine in $ENGINES; do
	for module in $modules; do
		# shellcheck disable=SC2046
		$(command_for "$engine" "$module") :memory: <"$here/sqlite.sql" >"$out/$engine-$module.out" 2>/dev/null || true
		if ! cmp -s "$here/sqlite.expected" "$out/$engine-$module.out"; then
			echo "answers: $module under $engine gave other answers than sqlite.expected"
			diff -u "$here/sqlite.expected" "$out/$engine-$module.out" || true
			failed=1
		fi
	done
done
[ "$failed" -eq 0 ] || exit 1
echo "answers: every module gives the answers of sqlite.expected under every engine"

echo "load before: $(load)"
round=1
while [ "$round" -le "$runs" ]; do
	for engine in $ENGINES; do
		for module in $modules; do
			took=$(user_time "$engine" "$module")
			best=$(cat "$out/$engine-$module.best" 2>/dev/null || echo "$took")
			echo "$took $best" | awk '{ print ($1 < $2) ? $1 : $2 }' >"$out/$engine-$module.best"
		done
	done
	round=$((round + 1))
done
echo "load after: $(load)"

echo "best CPU user time of $runs runs, in seconds:"
for engine in $ENGINES; do
	line="  $engine: rucc $(cat "$out/$engine-rucc.best")"
	case " $modules " in
	*" clang "*)
		rucc_time=$(cat "$out/$engine-rucc.best")
		clang_time=$(cat "$out/$engine-clang.best")
		line="$line, clang $clang_time, ratio $(echo "$rucc_time $clang_time" | awk '{ printf "%.3f", $1 / $2 }')"
		;;
	esac
	echo "$line"
done
