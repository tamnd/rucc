# rucc in the browser

This directory is a static web page that compiles and runs C with no server code. rucc, built for wasm32-wasip1, runs in the page with [browser_wasi_shim](https://github.com/bjorn3/browser_wasi_shim) 0.4.2 over a file system in memory. It compiles and links a program for wasm32-wasip1 with the linker inside rucc, and the page runs the program in a second instance with its own file system and the standard input from the page. The design is in document 12 of the wasm spec, sections 12.8 and 12.9, and the work is tamnd/rucc#2867.

## Files

- `rucc.js` is the module that does the work. It runs in a page and in Node. `Rucc.load` compiles rucc.wasm and installs the sysroot, `Rucc.compile` and `Rucc.build` compile and link, and `runProgram` runs the module that comes out.
- `index.html` is the page: a source box, a line of flags, a box for standard input, and the output of rucc and of the program.
- `assemble.sh` puts the page, the shim and the three files that the page downloads in one directory.
- `package.json` and `package-lock.json` pin the shim. `npm ci` in this directory installs it in `node_modules`.

## The three files

The page downloads three files by these names:

| file | what it is | where it comes from |
|---|---|---|
| `rucc.wasm` | rucc as a command module | `cargo build --release --bin rucc --target wasm32-wasip1` |
| `librucc_builtins.a` | the runtime of rucc for wasm32-wasip1 | `cargo xtask builtins --target wasm32-wasip1` |
| `rucc-sysroot-wasm32-wasip1.tar.gz` | the sysroot that this release of rucc pins | `rucc --fetch wasm32-wasip1`, which leaves it in the download cache |

The page does not unpack the sysroot itself. It puts the archive in the download cache of rucc and runs `rucc --fetch wasm32-wasip1`, which finds the archive there with the right hash and unpacks it with the readers inside rucc. An archive with another hash is refused with the message of rucc. A wasm module cannot say where its own file is, so every compile names the builtins with `-B/rucc/`.

## Build and look at the page

```sh
cargo build --release --bin rucc
cargo build --release --bin rucc --target wasm32-wasip1
cargo xtask builtins --target wasm32-wasip1
./target/release/rucc --fetch wasm32-wasip1
(cd web && npm ci)
release=target/wasm32-wasip1/release
web/assemble.sh "$release/rucc.wasm" "$release/librucc_builtins.a" \
    ~/.cache/rucc/downloads/*/rucc-sysroot-wasm32-wasip1.tar.gz site
python3 -m http.server -d site 8000
```

The download cache is `~/.cache/rucc` unless `RUCC_CACHE_DIR` or `XDG_CACHE_HOME` says another place.

## The test

`tests/web/run.mjs` runs `rucc.js` in Node with the same shim and the same three files. It compiles small programs at `-O0` and `-O2`, runs them with standard input, and checks that an error in the source comes back as a diagnostic. Given a SQLite directory, it builds the SQLite shell at `-O0` and `-O2` inside the shim and holds its answers to the output of the clang build, as `tests/sqlite/wasm.sh` does on the command line. CI runs it in the `wasm` job.

## What the page does not do

- It cannot download a sysroot. No downloader runs in a wasm host, so the archive must come from the same place as the page.
- It runs rucc on the main thread, so the page does not answer while a compile runs. A worker is the fix, and a playground needs one.
- It makes a new instance of rucc for each compile. The reactor of section 12.9, which keeps one instance, is a separate item of #2867.
