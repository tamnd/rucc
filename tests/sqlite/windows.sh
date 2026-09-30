#!/bin/sh
# Rung 1 for x86_64-windows-gnu: SQLite's own test suite, built by rucc and by MinGW GCC, run
# under Wine on Linux.
#
#   tests/sqlite/windows.sh RUCC [WORKDIR]
#
# RUCC is the rucc binary. Both builds are cross builds from Linux with the same configure line
# and the same flags, and both testfixtures run test/veryquick.test under Wine against the same
# Tcl. The suite does not pass cleanly on either side, because some of it asks things of the C
# runtime or of the file system that Wine answers differently from Windows (date4 compares
# strftime with SQLite's own date formatting, for one). So the reference build is the oracle: the
# script fails when a test fails in rucc's build and passes in MinGW GCC's, and prints both failure
# lists either way.
#
# Tcl is built from source with MinGW GCC, since no Linux distribution ships a Windows tcl86.dll.
# It is the fixture and not the thing under test, so rucc never builds it. Three fixes are made to
# the installed tree, each for something outside rucc. SQLite's autosetup wants a tclsh beside the
# tclConfig.sh it is given and stops configure with `missing "` when there is none, so the host's
# tclsh8.6 is linked in. TCL_DEFS and TCL_PACKAGE_PATH come out of the MinGW build with
# backslashes in them and autosetup reads that file as Tcl, so both are rewritten. SQLite's
# configure finds zlib through the same prefix, so the zlib Tcl carries is copied in beside it.
#
# The CFLAGS carry workarounds for SQLite's source rather than for either compiler.
# ext/misc/fileio.c uses Windows types and the dirent API on _WIN32 without including the headers
# that declare them, and uses S_ISLNK, which the MinGW headers do not have. Including windows.h
# first has a cost of its own: sqliteInt.h defines _FILE_OFFSET_BITS as 64, and by then the
# headers have already made off_t 32 bits, so newer mingw-w64 headers give struct stat a 32 bit
# st_size and send fstat to fstat64, and test_fs.c reads every file as empty. MinGW GCC does the
# same with those headers. Defining _FILE_OFFSET_BITS on the command line keeps the two agreeing.
#
# Three files are removed from both trees before the run, because under Wine each stops the
# whole suite with a Tcl error rather than failing its own cases. test/symlink2.test probes for
# symlink support and the `del` it cleans up with fails. test/win32lock.test takes a lock with
# LockFileEx and gets "busy" back where Windows waits. test/win32longpath.test cannot delete the
# long path it made.
#
# The Tcl DLL links msvcrt, as everything Ubuntu's MinGW GCC builds does, and rucc's testfixture
# links the UCRT, so the two keep separate copies of the environment. vtabH.test sets
# fstreeDrive from Tcl and test_fs.c reads it with getenv, and in rucc's build it read nothing
# and listed C: instead. Setting it before Wine starts gives both runtimes the value the test
# would have set.
#
# Needs x86_64-w64-mingw32-gcc, wine64, tclsh8.6, curl, unzip and make.

set -eu

rucc=${1:?usage: tests/sqlite/windows.sh RUCC [WORKDIR]}
work=${2:-${TMPDIR:-/tmp}/rucc-sqlite-windows}
jobs=${JOBS:-$(nproc)}

sqlite_url=https://sqlite.org/2026/sqlite-src-3530400.zip
sqlite_sha256=d18fa15aec74d8c17e1463f861095adc01b5ad190256acb4f91d22f0368d232b
tcl_version=8.6.16
tcl_sha256=91cb8fa61771c63c262efb553059b7c7ad6757afa5857af6265e4b0bdc2a14a5

cflags="-O2 -D_FILE_OFFSET_BITS=64 -include windows.h -include dirent.h -DS_ISLNK=0*"

case $rucc in
/*) ;;
*) rucc=$PWD/$rucc ;;
esac

if command -v wine64 > /dev/null; then
    wine=wine64
elif [ -x /usr/lib/wine/wine64 ]; then
    wine=/usr/lib/wine/wine64
else
    wine=wine
fi

mkdir -p "$work"
cd "$work"
work=$PWD
tcl=$work/tcl
export WINEDEBUG=-all
export WINEPREFIX=${WINEPREFIX:-$work/wine}

if [ ! -f "$tcl/lib/tclConfig.sh" ]; then
    echo "building Tcl $tcl_version for Windows with MinGW GCC"
    rm -rf tcl-src "$tcl"
    mkdir tcl-src
    curl -fsSL -o tcl.tar.gz "https://prdownloads.sourceforge.net/tcl/tcl$tcl_version-src.tar.gz"
    echo "$tcl_sha256  tcl.tar.gz" | sha256sum -c -
    tar xzf tcl.tar.gz -C tcl-src --strip-components=1
    (
        cd tcl-src/win
        ./configure --host=x86_64-w64-mingw32 --prefix="$tcl" --enable-64bit --enable-threads > configure.log 2>&1 || { tail -40 configure.log; exit 1; }
        make -j"$jobs" > make.log 2>&1 || { tail -40 make.log; exit 1; }
        make install > install.log 2>&1 || { tail -40 install.log; exit 1; }
    )
    ln -sf "$(command -v tclsh8.6)" "$tcl/bin/tclsh8.6"
    sed -i -e 's|^TCL_DEFS=.*|TCL_DEFS=""|' -e "s|^TCL_PACKAGE_PATH=.*|TCL_PACKAGE_PATH='$tcl/lib'|" "$tcl/lib/tclConfig.sh"
    cp tcl-src/compat/zlib/zlib.h tcl-src/compat/zlib/zconf.h "$tcl/include/"
    cp tcl-src/compat/zlib/win64/libz.dll.a "$tcl/lib/"
fi

if [ ! -f sqlite.zip ]; then
    curl -fsSL -o sqlite.zip "$sqlite_url"
fi
echo "$sqlite_sha256  sqlite.zip" | sha256sum -c -

printf '#!/bin/sh\nexec "%s" --target=x86_64-windows-gnu "$@"\n' "$rucc" > rucc-windows
chmod +x rucc-windows

# The tcl86.dll and zlib1.dll the testfixtures load.
WINEPATH=$(printf 'Z:%s' "$tcl/bin" | tr / '\\')
export WINEPATH

build() {
    name=$1
    cc=$2
    rm -rf "$name"
    mkdir "$name"
    unzip -q sqlite.zip -d "$name"
    mv "$name"/sqlite-src-*/* "$name"/
    rm "$name/test/symlink2.test" "$name/test/win32lock.test" "$name/test/win32longpath.test"
    (
        cd "$name"
        CC=$cc CFLAGS=$cflags CPPFLAGS=-I$tcl/include LDFLAGS=-L$tcl/lib \
            ./configure --host=x86_64-w64-mingw32 --all --disable-readline --with-tcl="$tcl/lib" > configure.log 2>&1 || { tail -40 configure.log; exit 1; }
        make -j"$jobs" testfixture.exe > make.log 2>&1 || { tail -40 make.log; exit 1; }
    )
}

run() {
    name=$1
    (
        cd "$name"
        start=$(date +%s)
        # veryquick.test exits non-zero whenever anything failed, which is expected here.
        fstreeDrive=Z: "$wine" ./testfixture.exe test/veryquick.test --maxerror=1000000 --verbose=file --output=test-out.txt > suite.log 2>&1 || true
        end=$(date +%s)
        summary=$(grep -E '^[0-9]+ errors out of [0-9]+ tests' suite.log | tail -1)
        echo "$name: ${summary:-no summary line} in $((end - start))s"
        [ -n "$summary" ] || { tail -20 suite.log; exit 1; }
        {
            sed -n 's/^!Failures on these tests: //p' suite.log | tr ' ' '\n'
            sed -n 's/^! \([^ ]*\) expected: .*/\1/p' suite.log
        } | sed '/^$/d' | sort -u > failures.txt
    )
}

echo "building SQLite with MinGW GCC"
build gcc x86_64-w64-mingw32-gcc
echo "building SQLite with rucc"
build rucc "$work/rucc-windows"
"$rucc" --version | head -1
x86_64-w64-mingw32-gcc --version | head -1

# The two suites run side by side, since each spends most of its time waiting on Wine.
run gcc > gcc.summary 2>&1 &
gcc_pid=$!
run rucc > rucc.summary 2>&1 &
rucc_pid=$!
status=0
wait $gcc_pid || status=1
wait $rucc_pid || status=1
cat gcc.summary rucc.summary
[ $status -eq 0 ] || exit 1

only_rucc=$(comm -13 gcc/failures.txt rucc/failures.txt)
only_gcc=$(comm -23 gcc/failures.txt rucc/failures.txt)
both=$(comm -12 gcc/failures.txt rucc/failures.txt | wc -l)
echo "failing in both builds: $both tests"
if [ -n "$only_gcc" ]; then
    echo "failing only in MinGW GCC's build:"
    echo "$only_gcc" | sed 's/^/  /'
fi
if [ -n "$only_rucc" ]; then
    echo "failing only in rucc's build:"
    echo "$only_rucc" | sed 's/^/  /'
    exit 1
fi
echo "rucc's build fails nothing that MinGW GCC's build passes"
