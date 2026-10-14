#!/usr/bin/env bash
# The perf check of milestone #3280. It builds Lua at -O2 with frame pointers, with GCC and with
# CC, and records perf.lua with each build, once with --call-graph=fp and once with
# --call-graph=dwarf.
#
#   tests/tools/perf.sh CC LUA
#
# CC is a whole command, such as "rucc". LUA is the directory of a Lua 5.4 release. GCC is gcc
# unless the environment sets it. perf must be able to record user space, so
# kernel.perf_event_paranoid must be 2 or less.
#
# For each recording, the counts are: the samples, the samples whose stack reaches main, the
# samples with an [unknown] frame, and the mean number of frames in a sample. The check fails when,
# with the same call graph, the CC build has:
#
# - a share of samples that reach main more than 2 points under the GCC share,
# - a share of samples with an [unknown] frame more than 1 point over the GCC share,
# - a mean number of frames more than 1 frame under the GCC mean, with dwarf only. GCC writes an
#   entry for each inlined call, and perf shows it as a frame, so CC must do the same.
set -euo pipefail

if [ $# -ne 2 ]; then
    echo "usage: $0 CC LUA" >&2
    exit 2
fi
here=$(cd "$(dirname "$0")" && pwd)
LUA=$(cd "$2" && pwd)
work=${WORK:-$(mktemp -d)}
mkdir -p "$work"
. "$here/lua.sh"

flags=(-g -O2 -fno-omit-frame-pointer -mno-omit-leaf-frame-pointer)
build_lua "$work/gcc" "${GCC:-gcc}" "${flags[@]}"
build_lua "$work/cc" "$1" "${flags[@]}"

# Reads the output of perf script and prints, in hundredths: the samples, the share that reaches
# main, the share with an [unknown] frame and the mean frames. A sample is a line that does not
# start with a space, then one line for each frame, then an empty line.
count() {
    awk '
        function done() {
            if (!open) return
            samples++
            frames += n
            if (main) reached++
            if (unknown) lost++
            open = 0
        }
        /^$/ { done(); next }
        /^[^ \t]/ { done(); open = 1; n = 0; main = 0; unknown = 0; next }
        {
            n++
            if ($2 ~ /^main(\+0x[0-9a-f]+)?$/) main = 1
            if (/\[unknown\]/) unknown = 1
        }
        END {
            done()
            if (samples == 0) { print "0 0 0 0"; exit }
            printf "%d %d %d %d\n", samples, reached * 10000 / samples, lost * 10000 / samples, frames * 100 / samples
        }
    ' "$1"
}

hundredths() {
    printf '%d.%02d' $(($1 / 100)) $(($1 % 100))
}

failed=0
printf '%-5s %-6s %8s %11s %10s %12s\n' build graph samples "reach main" "[unknown]" "mean frames"
for graph in fp dwarf; do
    for build in gcc cc; do
        data="$work/$build-$graph.data"
        perf record -q -e cpu-clock:u --call-graph="$graph" -o "$data" -- "$work/$build/lua" "$here/perf.lua" >/dev/null
        perf script -i "$data" -F comm,tid,time,ip,sym >"$work/$build-$graph.txt" 2>/dev/null
        read -r samples reached unknown mean < <(count "$work/$build-$graph.txt")
        printf '%-5s %-6s %8d %10s%% %9s%% %12s\n' "$build" "$graph" "$samples" "$(hundredths "$reached")" "$(hundredths "$unknown")" "$(hundredths "$mean")"
        if [ "$samples" -lt 100 ]; then
            echo "FAIL $build $graph: $samples samples, too few to count"
            failed=1
        fi
        eval "${build}_reached=$reached ${build}_unknown=$unknown ${build}_mean=$mean"
    done
    if [ "$cc_reached" -lt $((gcc_reached - 200)) ]; then
        echo "FAIL $graph: the cc stacks reach main more than 2 points less often than the gcc stacks"
        failed=1
    fi
    if [ "$cc_unknown" -gt $((gcc_unknown + 100)) ]; then
        echo "FAIL $graph: the cc stacks have an [unknown] frame more than 1 point more often than the gcc stacks"
        failed=1
    fi
    if [ "$graph" = dwarf ] && [ "$cc_mean" -lt $((gcc_mean - 100)) ]; then
        echo "FAIL dwarf: the cc stacks have more than 1 frame fewer than the gcc stacks"
        failed=1
    fi
done
exit $failed
