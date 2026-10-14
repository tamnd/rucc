#!/usr/bin/env bash
# Builds each program here in two halves at -O0 and -O2, runs it, and compares what it printed with
# the .out file beside it.
#
#   tests/exec/ctors/run.sh CC [RUNNER...]
#   tests/exec/ctors/run.sh --bless gcc [RUNNER...]
#
# CC is a whole command, such as "rucc" or "rucc --target=aarch64-linux-gnu", and it builds NAME.c.
# GCC builds NAME_gcc.c and does each link. When the first line of NAME.c is "/* shared */", the
# object of CC is linked as a shared object, and the program gets its path as its argument.
# Otherwise the two objects are linked into one program. RUNNER is what starts a program, such as
# qemu-aarch64 on an x86-64 host.
#
# The environment can set GCC, which defaults to gcc, and FLAGS, which go to both halves. --bless
# writes the .out files from the build with CC, which is meant to be GCC.
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
    other="$here/${name}_gcc.c"
    shared=0
    if [ "$(head -n 1 "$src")" = "/* shared */" ]; then
        shared=1
    fi
    opts=(-O0 -O2)
    if [ $bless = 1 ]; then
        opts=(-O2)
    fi
    for opt in "${opts[@]}"; do
        exe="$work/$name$opt"
        log="$work/$name$opt.log"
        args=()
        ran=$((ran + 1))
        if [ $shared = 1 ]; then
            built() {
                "${cc[@]}" "$opt" ${both[@]+"${both[@]}"} -fPIC -c "$src" -o "$exe.a.o" &&
                    "${gcc[@]}" ${both[@]+"${both[@]}"} -shared "$exe.a.o" -o "$exe.so" &&
                    "${gcc[@]}" "$opt" ${both[@]+"${both[@]}"} "$other" -o "$exe" -ldl
            }
            args=("$exe.so")
        else
            built() {
                "${cc[@]}" "$opt" ${both[@]+"${both[@]}"} -c "$src" -o "$exe.a.o" &&
                    "${gcc[@]}" "$opt" ${both[@]+"${both[@]}"} -c "$other" -o "$exe.b.o" &&
                    "${gcc[@]}" ${both[@]+"${both[@]}"} "$exe.a.o" "$exe.b.o" -o "$exe"
            }
        fi
        if ! built >"$log" 2>&1; then
            echo "FAIL $name $opt: did not build"
            sed 's/^/    /' "$log"
            failed=$((failed + 1))
            continue
        fi
        got="$work/$name$opt.txt"
        status=0
        timeout 120 ${runner[@]+"${runner[@]}"} "$exe" ${args[@]+"${args[@]}"} </dev/null >"$got" 2>"$work/$name$opt.err" || status=$?
        if [ $bless = 1 ] && [ $status -ne 0 ]; then
            echo "FAIL $name: the reference build exited with $status"
            failed=$((failed + 1))
        elif [ $bless = 1 ]; then
            cp "$got" "$here/$name.out"
            echo "blessed $name"
        elif [ $status -ne 0 ]; then
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
