#!/usr/bin/env bash
# The glibc header check of milestone #3281. It builds glibc with GCC, then runs the
# check-installed-headers-c test of each glibc subdirectory with CC as TEST_CC.
#
#   tests/tools/glibc-headers.sh CC GLIBC
#
# CC is the path of a rucc binary. GLIBC is the directory of a glibc release. The build goes in
# $WORK, or in a new temporary directory when WORK is not set.
#
# The test compiles each installed header alone, with each -std and each feature test macro that
# glibc supports, and with _FORTIFY_SOURCE at 1, 2 and 3. glibc passes its own CFLAGS to TEST_CC
# too, so CC must also take each flag that glibc builds with.
#
# The check fails when a subdirectory that installs headers does not say PASS.
set -euo pipefail

if [ $# -ne 2 ]; then
    echo "usage: $0 CC GLIBC" >&2
    exit 2
fi
CC=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
GLIBC=$(cd "$2" && pwd)
work=${WORK:-$(mktemp -d)}
mkdir -p "$work"
cd "$work"

if [ ! -f config.make ]; then
    "$GLIBC/configure" --prefix=/usr --disable-werror TEST_CC="$CC" >configure.log 2>&1 || {
        tail -30 configure.log
        exit 1
    }
fi
make -j"$(nproc)" >build.log 2>&1 || {
    tail -40 build.log
    exit 1
}

# A subdirectory that installs no headers has no rule for the test, and is skipped.
failed=0
for dir in $(sed -n 's/^sorted-subdirs := //p' sysd-sorted); do
    test="$dir/check-installed-headers-c"
    log="headers-${dir//\//_}.log"
    rm -f "$test.test-result"
    make test t="$test" TEST_CC="$CC" >"$log" 2>&1 || true
    if grep -q "No rule to make target .*/$test.out" "$log"; then
        continue
    fi
    result=$(head -1 "$test.test-result" 2>/dev/null || echo "NO RESULT: $test")
    echo "$result"
    case $result in
    PASS:*) ;;
    *)
        echo "    see $work/$test.out"
        failed=1
        ;;
    esac
done
exit $failed
