#!/usr/bin/env bash
# Builds every program here at -O0 and -O2 with the compiler it is given, runs each one, and
# compares what it printed with the .out file beside it.
#
#   tests/exec/windows/run.sh CC [RUNNER...]
#   tests/exec/windows/run.sh --bless CC [RUNNER...]
#
# CC is a whole command, such as "rucc --target=x86_64-windows-gnu" or "x86_64-w64-mingw32-gcc".
# RUNNER is what starts a Windows program, which is nothing on Windows and in WSL, and wine on
# Linux. --bless writes the .out files instead of comparing, and is meant to be run with MinGW GCC
# as CC, since the expected output is what GCC's build of the same program prints.
#
# A program whose first lines have "args:" in them is run with those arguments, read with the shell's
# quoting and no wildcard expansion, from this directory so that a wildcard has something to match.
# One whose first lines have "flags:" in them is compiled with those flags as well, such as
# -municode for a program that starts at wmain. "gcc flags:" go after the source and only when CC is gcc,
# for a program that asks rucc for something gcc does not do, such as a library named in a pragma.
# "gcc skip:" gives the reason gcc's build of that program is not run, and the program is skipped
# when CC is gcc.
#
# A directory here is one program in several files. Every .c in it other than main.c is built with
# -shared into a DLL of the same name, with the .def file of that name if there is one, and writes
# an import library that main.c is then linked against. The program runs beside its DLLs and its
# output is compared with main.out in the directory.
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

# Runs one program and compares what it printed with the expected output, or writes the expected
# output when blessing. Takes the name and level for the messages, the program, the file to compare
# with or bless into, and the arguments to run it with.
judge() {
    local name=$1 opt=$2 exe=$3 want=$4
    shift 4
    local got="$exe.txt" status=0
    # A compiler run through WSL's interop writes the file without the execute bit.
    chmod +x "$exe" 2>/dev/null || true
    (cd "$here" && "${runner[@]}" "$exe" "$@") </dev/null >"$got" 2>"$exe.err" || status=$?
    # A Windows program writes CRLF to a text stream, and Wine and a Windows host agree on that, so
    # the carriage returns go before comparing rather than into the .out files.
    tr -d '\r' <"$got" >"$got.lf"
    if [ $bless = 1 ] && [ $status -ne 0 ]; then
        echo "FAIL $name: the reference build exited with $status"
        failed=$((failed + 1))
    elif [ $bless = 1 ]; then
        cp "$got.lf" "$want"
        echo "blessed $name"
    elif [ $status -ne 0 ]; then
        echo "FAIL $name $opt: exited with $status"
        sed 's/^/    /' "$got.lf" "$exe.err" | head -20
        failed=$((failed + 1))
    # Compared in the shell rather than with diff, which the Git for Windows bash on a runner does
    # not have.
    elif [ "$(cat "$want")" != "$(cat "$got.lf")" ]; then
        echo "FAIL $name $opt: output differs, expected then got"
        sed 's/^/    /' "$want"
        echo "    ..."
        sed 's/^/    /' "$got.lf"
        failed=$((failed + 1))
    else
        echo "ok   $name $opt"
    fi
}

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
    theirs=()
    case "${cc[0]##*/}" in
        *gcc*)
            why=$(sed -n '1,3s|^/\* gcc skip: \(.*\) \*/$|\1|p' "$src")
            if [ -n "$why" ]; then
                echo "skip $name: $why"
                continue
            fi
            theirs=$(sed -n '1,3s|^/\* gcc flags: \(.*\) \*/$|\1|p' "$src")
            read -r -a theirs <<<"$theirs"
            ;;
    esac
    opts=(-O0 -O2)
    if [ $bless = 1 ]; then
        opts=(-O2)
    fi
    for opt in "${opts[@]}"; do
        exe="$work/$name$opt.exe"
        ran=$((ran + 1))
        if ! (cd "$here" && "${cc[@]}" "$opt" ${flags[@]+"${flags[@]}"} "$name.c" ${theirs[@]+"${theirs[@]}"} -o "$exe") >"$work/$name$opt.log" 2>&1; then
            echo "FAIL $name $opt: did not build"
            sed 's/^/    /' "$work/$name$opt.log"
            failed=$((failed + 1))
            continue
        fi
        judge "$name" "$opt" "$exe" "$here/$name.out" "${args[@]}"
    done
done

for dir in "$here"/*/; do
    dir=${dir%/}
    [ -f "$dir/main.c" ] || continue
    name=$(basename "$dir")
    opts=(-O0 -O2)
    if [ $bless = 1 ]; then
        opts=(-O2)
    fi
    for opt in "${opts[@]}"; do
        out="$work/$name$opt"
        mkdir -p "$out"
        ran=$((ran + 1))
        libs=()
        built=1
        for src in "$dir"/*.c; do
            lib=$(basename "$src" .c)
            [ "$lib" = main ] && continue
            def=()
            [ -f "$dir/$lib.def" ] && def=("$dir/$lib.def")
            # Built from inside the output directory so the import library is a bare name. A path
            # inside a -Wl, option is not one Git Bash rewrites for a Windows program.
            if ! (cd "$out" && "${cc[@]}" "$opt" -shared "$src" ${def[@]+"${def[@]}"} -o "$lib.dll" "-Wl,--out-implib,lib$lib.dll.a") >"$out/$lib.log" 2>&1; then
                echo "FAIL $name $opt: $lib.dll did not build"
                sed 's/^/    /' "$out/$lib.log"
                built=0
                break
            fi
            libs+=("-l$lib")
        done
        if [ $built = 1 ] && ! (cd "$out" && "${cc[@]}" "$opt" "$dir/main.c" -o main.exe -L. ${libs[@]+"${libs[@]}"}) >"$out/main.log" 2>&1; then
            echo "FAIL $name $opt: main.exe did not build"
            sed 's/^/    /' "$out/main.log"
            built=0
        fi
        if [ $built = 0 ]; then
            failed=$((failed + 1))
            continue
        fi
        judge "$name/" "$opt" "$out/main.exe" "$dir/main.out"
    done
done

echo "$((ran - failed)) of $ran passed"
[ $failed -eq 0 ]
