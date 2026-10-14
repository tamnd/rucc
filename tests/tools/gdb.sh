#!/usr/bin/env bash
# The GDB check of milestone #3280. It builds Lua with -g, with GCC at -O2 and with CC at -O0 and
# -O2, and runs gdb.lua under GDB in each build.
#
#   tests/tools/gdb.sh CC LUA
#
# CC is a whole command, such as "rucc". LUA is the directory of a Lua 5.4 release. GCC is gcc
# unless the environment sets it.
#
# GDB stops once in each function of FUNCS, and at each stop it prints the arguments and the
# backtrace. This is done two times: once with "tbreak FUNC", which stops after the prologue, and
# once with "tbreak *FUNC", which stops on the first instruction. The rate is the share of the
# arguments that GDB can show, that is, the ones that are not <optimized out>.
#
# The check fails when:
#
# - a build does not stop in each function,
# - the rate of CC at -O2 is more than 10 points under the rate of GCC at -O2,
# - the rate of CC at -O0 is not 100%,
# - a backtrace of CC has a frame with no name, or does not reach main.
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

FUNCS=(luaH_getshortstr luaS_newlstr luaV_concat luaD_precall luaC_step lua_pushinteger)
BUILDS=(gcc-O2 cc-O2 cc-O0)

build_lua "$work/gcc-O2" "${GCC:-gcc}" -g -O2
build_lua "$work/cc-O2" "$1" -g -O2
build_lua "$work/cc-O0" "$1" -g -O0

# Writes the GDB commands for one way to stop. The program output goes to /dev/null so that it
# does not mix with what GDB prints.
commands() {
    local at=$1 func
    echo "set pagination off"
    echo "set confirm off"
    for func in "${FUNCS[@]}"; do
        echo "tbreak $at$func"
    done
    echo "run $here/gdb.lua > /dev/null"
    for func in "${FUNCS[@]}"; do
        echo "echo @args\\n"
        echo "info args"
        echo "echo @bt\\n"
        echo "bt"
        echo "echo @end\\n"
        echo "continue"
    done
}

# Reads a GDB log and prints: stops, values, optimized out, frames, frames with no name, and the
# stops whose backtrace does not reach main.
count() {
    awk '
        /^@args$/ { part = "args"; stops++; main = 0; next }
        /^@bt$/ { part = "bt"; next }
        /^@end$/ { if (!main) lost++; part = ""; next }
        part == "args" && /^[A-Za-z_][A-Za-z0-9_]* = / {
            values++
            if (/= <optimized out>$/) out++
        }
        part == "bt" && /^#[0-9]/ {
            frames++
            if (/ \?\? /) unknown++
            if (/ main \(/) main = 1
        }
        END { printf "%d %d %d %d %d %d\n", stops, values, out, frames, unknown, lost }
    ' "$1"
}

failed=0
printf '%-8s %-6s %6s %7s %14s %6s %7s %8s %13s\n' build stop stops values "optimized out" shown frames unknown "not to main"
for at in "" "*"; do
    cmds="$work/cmds${at:+-entry}"
    commands "$at" >"$cmds"
    for build in "${BUILDS[@]}"; do
        log="$work/$build${at:+-entry}.log"
        gdb -batch -nx -x "$cmds" "$work/$build/lua" >"$log" 2>&1 || true
        read -r stops values out frames unknown lost < <(count "$log")
        rate=0
        if [ "$values" -gt 0 ]; then
            rate=$(((values - out) * 100 / values))
        fi
        printf '%-8s %-6s %6d %7d %14d %5d%% %7d %8d %13d\n' "$build" "${at:-line}" "$stops" "$values" "$out" "$rate" "$frames" "$unknown" "$lost"
        eval "rate_${build//-/_}=$rate"
        if [ "$stops" -ne ${#FUNCS[@]} ]; then
            echo "FAIL $build: stopped $stops times, not ${#FUNCS[@]}, see $log"
            failed=1
        fi
        case $build in
        cc-*)
            if [ "$unknown" -ne 0 ] || [ "$lost" -ne 0 ]; then
                echo "FAIL $build: a backtrace has a frame with no name or does not reach main, see $log"
                failed=1
            fi
            ;;
        esac
    done
    if [ "$rate_cc_O2" -lt $((rate_gcc_O2 - 10)) ]; then
        echo "FAIL cc-O2 at ${at:-line}: $rate_cc_O2% is more than 10 points under the $rate_gcc_O2% of gcc"
        failed=1
    fi
    if [ "$rate_cc_O0" -ne 100 ]; then
        echo "FAIL cc-O0 at ${at:-line}: $rate_cc_O0%, not 100%"
        failed=1
    fi
done
exit $failed
