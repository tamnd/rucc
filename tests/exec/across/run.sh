#!/usr/bin/env bash
# Builds the program here with one.c and two.c compiled by two different compilers, both ways
# round, at -O0 and -O2, and runs it.
#
#   tests/exec/across/run.sh CC1 CC2 [RUNNER...]
#
# CC1 and CC2 are whole commands, such as "rucc --target=x86_64-windows-gnu" and
# "x86_64-w64-mingw32-gcc", and CC1 is the one that links. RUNNER is what starts the program, which
# is wine on Linux. A run that has not finished after two minutes is a failure, because a place one
# compiler saved and the other cannot come back to properly can send the program round forever.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
read -r -a first <<<"$1"
read -r -a second <<<"$2"
shift 2
runner=("$@")
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

failed=0
for opt in -O0 -O2; do
    for way in first second; do
        if [ "$way" = first ]; then
            a=("${first[@]}")
            b=("${second[@]}")
        else
            a=("${second[@]}")
            b=("${first[@]}")
        fi
        what="one.c by ${a[0]##*/}, two.c by ${b[0]##*/}, $opt"
        if ! "${a[@]}" "$opt" -c "$here/one.c" -o "$out/one.o" ||
            ! "${b[@]}" "$opt" -c "$here/two.c" -o "$out/two.o" ||
            ! "${first[@]}" "$out/one.o" "$out/two.o" -o "$out/across.exe"; then
            echo "FAIL $what: did not build"
            failed=1
            continue
        fi
        got="$(timeout 120 "${runner[@]}" "$out/across.exe" | tr -d '\r')" || true
        if [ "$got" = "499500 499500" ]; then
            echo "ok   $what"
        else
            echo "FAIL $what: printed '${got}'"
            failed=1
        fi
    done
done
exit "$failed"
