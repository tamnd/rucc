#!/bin/sh
# The SQLite shell for wasm32-wasip1, built by rucc at -O0 and -O2 and run with a fixed workload,
# each output held to the output of the shell that clang from wasi-sdk 34 builds at -O2.
#
# usage: tests/sqlite/wasm.sh <rucc> [<sqlite-autoconf directory>]
#
# Without a directory the SQLite release below is downloaded and checked against its hash. RUNNER
# goes in front of each program, as in tests/rung0/run.sh, and CI runs this once with
# `wasmtime run` and once with `node tests/rung0/wasi.mjs`. The sysroot for wasm32-wasip1 must be
# fetched first with `rucc --fetch wasm32-wasip1`.
#
# wasm-workload.sql makes a table of 200000 rows in one transaction, indexes it, and asks
# questions whose answers are arithmetic: aggregates, a range, a GROUP BY, a join, a window, the
# printf, JSON, date and text functions, the edges of integer and float arithmetic, a delete, an
# update and an integrity check. WASI has no temporary directory, so the workload first sets
# `PRAGMA temp_store=MEMORY`. wasm-workload.expected is the output of the clang build. It was made
# with this command, where S is the wasi-sdk 34 directory and the flags are the ones below:
#
#     $S/bin/clang --target=wasm32-wasip1 -O2 $defs $emulated shell.c sqlite3.c wasi-stubs.c \
#         $libraries -o sqlite3.wasm
#
# The shell calls `system`, `popen` and `pclose`, which wasi-libc does not have, and
# wasi-stubs.c defines them. It also needs the four emulations of wasi-libc for signals, process
# clocks, `getpid` and `mmap`, each a macro and a library.

set -eu

[ $# -eq 1 ] || [ $# -eq 2 ] || { echo "usage: $0 <rucc> [<sqlite-autoconf directory>]" >&2; exit 2; }
rucc=$1
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
version=3530400
sha256=0e9483900e92cd5de8fd48d16bf9200145a61f7fd5be542a5ac81d8a9516eb9c

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM

if [ $# -eq 2 ]; then
	sqlite=$2
else
	curl -fsSL -o "$out/sqlite.tar.gz" "https://sqlite.org/2026/sqlite-autoconf-$version.tar.gz"
	echo "$sha256  $out/sqlite.tar.gz" | sha256sum -c --quiet -
	tar -xzf "$out/sqlite.tar.gz" -C "$out"
	sqlite=$out/sqlite-autoconf-$version
fi

defs="-DSQLITE_THREADSAFE=0 -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_OMIT_WAL"
emulated="-D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_GETPID -D_WASI_EMULATED_MMAN"
libraries="-lwasi-emulated-signal -lwasi-emulated-process-clocks -lwasi-emulated-getpid -lwasi-emulated-mman"

failed=0
for level in 0 2; do
	shell=$out/sqlite3-O$level.wasm
	# shellcheck disable=SC2086
	if ! "$rucc" --target=wasm32-wasip1 -O$level $defs $emulated -I"$sqlite" "$sqlite/shell.c" \
		"$sqlite/sqlite3.c" "$here/wasi-stubs.c" $libraries -o "$shell"; then
		echo "sqlite -O$level did not build"
		failed=$((failed + 1))
		continue
	fi
	# shellcheck disable=SC2086
	# The shell writes a warning to stderr that it cannot find a home directory, and Node writes
	# one that WASI is experimental, so stderr is shown only when the run fails.
	if ! ${RUNNER:-} "$shell" :memory: <"$here/wasm-workload.sql" >"$out/O$level.out" 2>"$out/O$level.err"; then
		echo "sqlite -O$level exited nonzero"
		sed 's/^/    /' "$out/O$level.err"
		failed=$((failed + 1))
	elif ! diff -u "$here/wasm-workload.expected" "$out/O$level.out"; then
		echo "sqlite -O$level gave other answers than the clang build"
		failed=$((failed + 1))
	else
		echo "sqlite -O$level gave the answers of the clang build"
	fi
done
[ "$failed" -eq 0 ]
