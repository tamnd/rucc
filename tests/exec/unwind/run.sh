#!/usr/bin/env bash
# Builds each program here in two halves at -O0 and -O2, runs it, and compares what it printed with
# the .out file beside it.
#
#   tests/exec/unwind/run.sh CC [RUNNER...]
#   tests/exec/unwind/run.sh --bless gcc [RUNNER...]
#
# CC is a whole command, such as "rucc" or "rucc --target=aarch64-linux-gnu", and it builds NAME.c.
# GCC builds NAME_gcc.c, or G++ builds NAME_gcc.cc, and that one links. The second half throws,
# cancels, exits a thread or walks the stack, and the unwinder goes up through the frames of the
# first half. RUNNER is what starts a program, such as qemu-aarch64 on an x86-64 host.
#
# The environment can set GCC and GXX, which default to gcc and g++, and FLAGS, which go to both
# halves, such as -mbranch-protection=standard on AArch64. A program with a C++ half is skipped
# when there is no G++. A program whose first lines have "flags:" in them builds NAME.c with those
# flags as well. --bless writes the .out files from the build with CC, which is meant to be GCC.
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
read -r -a gxx <<<"${GXX:-g++}"
read -r -a both <<<"${FLAGS:-}"

here=$(cd "$(dirname "$0")" && pwd)
work=${WORK:-$(mktemp -d)}
mkdir -p "$work"
failed=0
ran=0

for src in "$here"/*.c; do
    name=$(basename "$src" .c)
    case $name in *_gcc) continue ;; esac
    if [ -f "$here/${name}_gcc.cc" ]; then
        other="$here/${name}_gcc.cc"
        link=("${gxx[@]}")
        if ! command -v "${gxx[0]}" >/dev/null; then
            echo "skip $name: no ${gxx[0]}"
            continue
        fi
    else
        other="$here/${name}_gcc.c"
        link=("${gcc[@]}")
    fi
    flags=$(sed -n '1,3s|^/\* flags: \(.*\) \*/$|\1|p' "$src")
    read -r -a flags <<<"$flags"
    opts=(-O0 -O2)
    if [ $bless = 1 ]; then
        opts=(-O2)
    fi
    for opt in "${opts[@]}"; do
        exe="$work/$name$opt"
        log="$work/$name$opt.log"
        ran=$((ran + 1))
        if ! { "${cc[@]}" "$opt" ${both[@]+"${both[@]}"} ${flags[@]+"${flags[@]}"} -c "$src" -o "$exe.a.o" &&
            "${link[@]}" "$opt" ${both[@]+"${both[@]}"} -pthread -c "$other" -o "$exe.b.o" &&
            "${link[@]}" ${both[@]+"${both[@]}"} -pthread -rdynamic "$exe.a.o" "$exe.b.o" -o "$exe"; } >"$log" 2>&1; then
            echo "FAIL $name $opt: did not build"
            sed 's/^/    /' "$log"
            failed=$((failed + 1))
            continue
        fi
        got="$work/$name$opt.txt"
        status=0
        timeout 120 ${runner[@]+"${runner[@]}"} "$exe" </dev/null >"$got" 2>"$work/$name$opt.err" || status=$?
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
