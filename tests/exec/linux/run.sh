#!/usr/bin/env bash
# Builds every program here at -O0 and -O2 with the compiler it is given, runs each one, and
# compares what it printed with the .out file beside it.
#
#   tests/exec/linux/run.sh CC [RUNNER...]
#   tests/exec/linux/run.sh --bless CC [RUNNER...]
#
# CC is a whole command, such as "rucc --target=x86_64-linux-gnu" or "gcc". RUNNER is what starts
# a program, which is nothing on an x86-64 Linux host and qemu-x86_64 on anything else. --bless
# writes the .out files instead of comparing, and is meant to be run with GCC as CC, since the
# expected output is what GCC's build of the same program prints.
#
# These are the programs whose answer depends on the target being x86-64 Linux, which is what
# sets them apart from the ones in tests/exec/windows. A program whose first lines have "flags:"
# in them is compiled with those flags as well.
set -euo pipefail

bless=0
if [ "${1:-}" = "--bless" ]; then
    bless=1
    shift
fi
if [ $# -lt 1 ]; then
    echo "usage: $0 [--bless] CC [RUNNER...]" >&2
    exit 2
fi
read -r -a cc <<<"$1"
shift
runner=("$@")

here=$(cd "$(dirname "$0")" && pwd)
work=${WORK:-$(mktemp -d)}
mkdir -p "$work"
failed=0
ran=0

for src in "$here"/*.c; do
    name=$(basename "$src" .c)
    args=()
    line=$(sed -n '1,3s|^/\* args: \(.*\)$|\1|p' "$src")
    if [ -n "$line" ]; then
        set -f
        eval "args=($line)"
        set +f
    fi
    flags=$(sed -n '1,3s|^/\* flags: \(.*\) \*/$|\1|p' "$src")
    read -r -a flags <<<"$flags"
    opts=(-O0 -O2)
    if [ $bless = 1 ]; then
        opts=(-O2)
    fi
    for opt in "${opts[@]}"; do
        exe="$work/$name$opt"
        got="$work/$name$opt.txt"
        ran=$((ran + 1))
        if ! (cd "$here" && "${cc[@]}" "$opt" ${flags[@]+"${flags[@]}"} "$name.c" -o "$exe") >"$work/$name$opt.log" 2>&1; then
            echo "FAIL $name $opt: did not build"
            sed 's/^/    /' "$work/$name$opt.log"
            failed=$((failed + 1))
            continue
        fi
        status=0
        (cd "$here" && "${runner[@]}" "$exe" "${args[@]}") </dev/null >"$got" 2>"$work/$name$opt.err" || status=$?
        cp "$got" "$got.lf"
        if [ $bless = 1 ] && [ $status -ne 0 ]; then
            echo "FAIL $name: the reference build exited with $status"
            failed=$((failed + 1))
        elif [ $bless = 1 ]; then
            cp "$got.lf" "$here/$name.out"
            echo "blessed $name"
        elif [ $status -ne 0 ]; then
            echo "FAIL $name $opt: exited with $status"
            sed 's/^/    /' "$got.lf" "$work/$name$opt.err" | head -20
            failed=$((failed + 1))
        elif ! diff -u "$here/$name.out" "$got.lf" >"$work/$name$opt.diff"; then
            echo "FAIL $name $opt: output differs"
            sed 's/^/    /' "$work/$name$opt.diff"
            failed=$((failed + 1))
        else
            echo "ok   $name $opt"
        fi
    done
done

echo "$((ran - failed)) of $ran passed"
[ $failed -eq 0 ]
