#!/usr/bin/env bash
# Builds each program here at -O0 and -O2, runs it, and compares what it printed with the .out file
# beside it.
#
#   tests/exec/atomic/run.sh CC [RUNNER...]
#
# CC is a whole command, such as "rucc" or "rucc --target=aarch64-linux-gnu". It builds and links
# NAME.c, because the routines a program gets are decided by its link. GCC builds NAME_gcc.c as a
# shared library with -latomic, and the program gets its path as its argument. RUNNER is what
# starts a program, such as qemu-aarch64 on an x86-64 host.
#
# The environment can set GCC, which defaults to gcc, and FLAGS, which go to both halves.
set -euo pipefail

if [ $# -lt 1 ]; then
    echo "usage: $0 CC [RUNNER...]" >&2
    exit 2
fi
read -r -a cc <<<"$1"
shift
runner=("$@")
read -r -a gcc <<<"${GCC:-gcc}"
read -r -a both <<<"${FLAGS:-}"

here=$(cd "$(dirname "$0")" && pwd)
work=${WORK:-$(mktemp -d)}
mkdir -p "$work"
failed=0
ran=0

for src in "$here"/*.c; do
    name=$(basename "$src" .c)
    case $name in *_gcc) continue ;; esac
    for opt in -O0 -O2; do
        exe="$work/$name$opt"
        log="$work/$name$opt.log"
        ran=$((ran + 1))
        if ! { "${gcc[@]}" -O2 ${both[@]+"${both[@]}"} -fPIC -shared "$here/${name}_gcc.c" -o "$exe.so" -latomic &&
            "${cc[@]}" "$opt" ${both[@]+"${both[@]}"} -pthread "$src" -o "$exe" -ldl; } >"$log" 2>&1; then
            echo "FAIL $name $opt: did not build"
            sed 's/^/    /' "$log"
            failed=$((failed + 1))
            continue
        fi
        got="$work/$name$opt.txt"
        status=0
        timeout 300 ${runner[@]+"${runner[@]}"} "$exe" "$exe.so" </dev/null >"$got" 2>"$work/$name$opt.err" || status=$?
        if [ $status -ne 0 ]; then
            echo "FAIL $name $opt: exited with $status"
            sed 's/^/    /' "$got" "$work/$name$opt.err" | head -20
            failed=$((failed + 1))
        elif ! diff -u "$here/$name.out" "$got" >"$work/$name$opt.diff"; then
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
