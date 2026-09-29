#!/usr/bin/env bash
# Compiles every program here to assembly at -O0 and -O2 for Windows on AArch64 and fails if any
# instruction writes x18 or w18.
#
#   tests/exec/windows/x18.sh CC
#
# CC is a whole command, such as "rucc --target=aarch64-windows-gnu". On Windows x18 holds the
# address of the thread's TEB for the whole life of the thread, and the kernel may put it back at
# any moment, so code that uses it as a scratch register breaks at random. A compiled function may
# read it, which is how NtCurrentTeb and thread locals find the TEB, and must never write it.
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 CC" >&2
    exit 2
fi
read -r -a cc <<<"$1"

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0
ran=0

cd "$here"
for src in *.c */*.c; do
    flags=$(sed -n '1,3s|^/\* flags: \(.*\) \*/$|\1|p' "$src")
    read -r -a flags <<<"$flags"
    for opt in -O0 -O2; do
        ran=$((ran + 1))
        out="$work/$(echo "$src" | tr / _)$opt.s"
        if ! "${cc[@]}" "$opt" ${flags[@]+"${flags[@]}"} -S "$src" -o "$out" >"$out.log" 2>&1; then
            echo "FAIL $src $opt: did not compile"
            sed 's/^/    /' "$out.log" | head -20
            failed=$((failed + 1))
            continue
        fi
        # Instructions only, since a directive or a label can name anything, and of those the
        # ones that write x18 or w18: the first operand of anything but a store, a compare, a
        # branch or a call, and the second of a load pair.
        bad=$(grep -v '^[[:space:]]*\.' "$out" | grep -v '^[^[:space:]].*:' | awk '
            {
                op = $1
                rest = $0
                sub(/^[[:space:]]*[^[:space:]]+[[:space:]]*/, "", rest)
                n = split(rest, arg, /[[:space:]]*,[[:space:]]*/)
                if (op !~ /^(st.*|cmp|cmn|tst|cbn?z|tbn?z|b|b\..*|bl|br|blr|ret)$/ && arg[1] ~ /^[xw]18$/) print
                else if (op ~ /^ld[a-z]*p$/ && arg[2] ~ /^[xw]18$/) print
            }' || true)
        if [ -n "$bad" ]; then
            echo "FAIL $src $opt: uses x18"
            echo "$bad" | sed 's/^/    /' | head -20
            failed=$((failed + 1))
        else
            echo "ok   $src $opt"
        fi
    done
done

echo "$((ran - failed)) of $ran kept off x18"
[ $failed -eq 0 ]
