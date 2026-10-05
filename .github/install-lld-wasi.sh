#!/usr/bin/env bash
# Installs lld 21 from apt.llvm.org on an Ubuntu 24.04 runner, for the jobs that link for WASI.
#
# The wasi-libc of wasi-sdk 34, which `rucc --fetch wasm32-wasip1` installs, refers to
# `__wasm_first_page_end` in `sbrk` and in dlmalloc, and lld defines that symbol from 21 on. Ubuntu
# 24.04 has lld 18, 19 and 20, and each of them stops on the first program that calls malloc. The
# `wasm-ld` in the Linux release of wasi-sdk 34 needs `libedit.so.0`, which Ubuntu does not have,
# so the one from apt.llvm.org is used. It goes in /usr/lib/llvm-21/bin, where the driver looks.
#
# usage: sudo .github/install-lld-wasi.sh

set -euo pipefail

curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key | gpg --dearmor -o /usr/share/keyrings/apt.llvm.org.gpg
echo "deb [signed-by=/usr/share/keyrings/apt.llvm.org.gpg] http://apt.llvm.org/noble/ llvm-toolchain-noble-21 main" >/etc/apt/sources.list.d/llvm-21.list
apt-get update || echo "at least one apt source is unhappy, carrying on"
apt-get install -y lld-21
/usr/lib/llvm-21/bin/wasm-ld --version
