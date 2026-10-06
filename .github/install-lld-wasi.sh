#!/usr/bin/env bash
# Installs lld 21, or another version, from apt.llvm.org on an Ubuntu 24.04 runner, for the jobs
# that link for WASI.
#
# The wasi-libc of wasi-sdk 34, which `rucc --fetch wasm32-wasip1` installs, refers to
# `__wasm_first_page_end` in `sbrk` and in dlmalloc, and lld defines that symbol from 21 on. Ubuntu
# 24.04 has lld 18, 19 and 20, and each of them stops on the first program that calls malloc. The
# `wasm-ld` in the Linux release of wasi-sdk 34 needs `libedit.so.0`, which Ubuntu does not have,
# so the one from apt.llvm.org is used. It goes in /usr/lib/llvm-<N>/bin, where the driver looks.
#
# The wasm job installs lld 23 with the argument 23. That is the `wasm-ld` of wasi-sdk 34, which the
# linker inside rucc writes the same modules as, and tests/wasm-link/same.sh compares the two byte
# for byte. lld 21 lays out memory in another order, with the data before the stack, so its modules
# are not the same bytes. The driver takes the newest /usr/lib/llvm-<N>/bin.
#
# usage: sudo .github/install-lld-wasi.sh [<major version, 21 when not given>]

set -euo pipefail

version=${1:-21}

curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key | gpg --dearmor -o /usr/share/keyrings/apt.llvm.org.gpg
echo "deb [signed-by=/usr/share/keyrings/apt.llvm.org.gpg] http://apt.llvm.org/noble/ llvm-toolchain-noble-$version main" >"/etc/apt/sources.list.d/llvm-$version.list"
apt-get update || echo "at least one apt source is unhappy, carrying on"
apt-get install -y "lld-$version"
"/usr/lib/llvm-$version/bin/wasm-ld" --version
