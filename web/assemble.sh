#!/bin/sh
# Puts the browser page of rucc together in one directory, which any static web server can serve.
#
# usage: web/assemble.sh <rucc.wasm> <librucc_builtins.a> <sysroot.tar.gz> <outdir>
#
# The three files are the ones that tests/web/run.mjs takes: rucc built for wasm32-wasip1, the
# builtins archive for wasm32-wasip1 that `cargo xtask builtins --target wasm32-wasip1` writes, and
# the sysroot archive that this release pins, which `rucc --fetch wasm32-wasip1` leaves in the
# download cache. The page loads them by these names. The shim comes from web/node_modules, so run
# `npm ci` in web/ first. Its two licence files go with it.
#
# To look at the page on this machine:
#
#     web/assemble.sh ... site && python3 -m http.server -d site 8000

set -eu

[ $# -eq 4 ] || { echo "usage: $0 <rucc.wasm> <librucc_builtins.a> <sysroot.tar.gz> <outdir>" >&2; exit 2; }
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
shim=$here/node_modules/@bjorn3/browser_wasi_shim
if [ ! -d "$shim/dist" ]; then
	echo "$0: $shim is not there; run npm ci in $here" >&2
	exit 1
fi

out=$4
mkdir -p "$out/shim"
cp "$here/index.html" "$here/rucc.js" "$out/"
cp "$shim"/dist/*.js "$out/shim/"
cp "$shim/LICENSE-MIT" "$shim/LICENSE-APACHE" "$out/shim/"
cp "$1" "$out/rucc.wasm"
cp "$2" "$out/librucc_builtins.a"
cp "$3" "$out/rucc-sysroot-wasm32-wasip1.tar.gz"
echo "the page is in $out"
