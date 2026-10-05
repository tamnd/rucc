// Runs a wasm32-wasip1 command module under the WASI of Node, for tests/rung0/run.sh:
//
//     RUNNER="node tests/rung0/wasi.mjs" tests/rung0/run.sh <rucc> wasm32-wasip1
//
// The working directory is preopened as ".", as `wasmtime run --dir=.` does, because c-testsuite
// 00187 writes a file there and reads it back. The exit status of the module is the exit status of
// this script. Node writes a warning that WASI is experimental to stderr, which run.sh does not
// compare.

import { readFile } from 'node:fs/promises';
import { argv, env, exit } from 'node:process';
import { WASI } from 'node:wasi';

const [, , path, ...rest] = argv;
const wasi = new WASI({
  version: 'preview1',
  args: [path, ...rest],
  env,
  preopens: { '.': '.' },
  returnOnExit: true,
});
const module = await WebAssembly.compile(await readFile(path));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
exit(wasi.start(instance));
