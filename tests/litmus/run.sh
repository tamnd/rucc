#!/bin/sh
# Litmus tests for the barriers and atomics PostgreSQL is built on, run on the machine itself.
#
# usage: tests/litmus/run.sh <rucc> [cc]
#
# Every program is built by rucc at -O0 and -O2 and run, and exits nonzero when any run saw an
# outcome the memory model forbids or a count came out wrong. The same programs are built by the
# system compiler as a control, so a failure that shows up there too is the test's own and not
# rucc's. On AArch64 Linux gcc is built twice, once as it comes, which calls the outline atomics in
# libgcc, and once with -mno-outline-atomics, which inlines the same exclusive loops rucc does.
#
# How long each test runs is bounded by LITMUS_ITERATIONS and LITMUS_SECONDS, see litmus.h, and
# each program is killed after LITMUS_TIMEOUT seconds, 120 unless set, in case a lock never comes
# free.

set -eu

[ $# -ge 1 ] && [ $# -le 2 ] || { echo "usage: $0 <rucc> [cc]" >&2; exit 2; }
rucc=$1
cc=${2:-cc}
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
limit=${LITMUS_TIMEOUT:-120}

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT INT TERM
failed=0
passed=0

# The builds, one per line: a name, then the compiler and its flags.
builds="rucc-O0 $rucc -O0
rucc-O2 $rucc -O2
cc-O2 $cc -O2"
if [ "$(uname -s)" = Linux ] && [ "$(uname -m)" = aarch64 ] && "$cc" --version 2>/dev/null | grep -q 'Free Software'; then
	builds="$builds
cc-O2-inline $cc -O2 -mno-outline-atomics"
fi

timed() {
	if command -v timeout >/dev/null; then
		timeout "$limit" "$@"
	else
		"$@"
	fi
}

for source in "$here"/*.c; do
	name=$(basename "$source" .c)
	echo "$builds" | while read -r build compiler flags; do
		binary=$out/$name-$build
		# shellcheck disable=SC2086
		if ! $compiler $flags -pthread -o "$binary" "$source" >"$out/build.log" 2>&1; then
			printf '%-10s %-13s did not build\n' "$name" "$build"
			sed 's/^/    /' "$out/build.log"
			echo fail >>"$out/results"
			continue
		fi
		printf '%-10s %-13s\n' "$name" "$build"
		if timed "$binary" >"$binary.txt" 2>&1; then
			sed 's/^/    /' "$binary.txt"
			echo pass >>"$out/results"
		else
			status=$?
			sed 's/^/    /' "$binary.txt"
			printf '    exited with %s\n' "$status"
			echo fail >>"$out/results"
		fi
	done
done

passed=$(grep -c pass "$out/results" || true)
failed=$(grep -c fail "$out/results" || true)
printf '\n%s passed, %s failed\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
