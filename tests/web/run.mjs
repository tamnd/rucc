// rucc.wasm as the browser page runs it: web/rucc.js over browser_wasi_shim, in Node, which runs
// the same V8 as Chrome. This is the test of the page that CI runs (tamnd/rucc#2867):
//
//     (cd web && npm ci)
//     node tests/web/run.mjs <rucc.wasm> <librucc_builtins.a> <sysroot.tar.gz> [<sqlite dir>]
//
// where <sqlite dir> is an unpacked sqlite-autoconf release.
//
// The three files are the ones that the page downloads: rucc built for wasm32-wasip1, the builtins
// archive that `cargo xtask builtins --target wasm32-wasip1` writes, and the sysroot archive that
// this release pins, which `rucc --fetch wasm32-wasip1` leaves in the download cache. The test
// installs the sysroot through rucc.wasm, compiles small programs at -O0 and -O2, runs them in a
// second instance with standard input, and checks that an error in the source comes back as a
// diagnostic. With a SQLite directory it also builds the shell at -O0 and -O2, all of it inside
// the shim, as tests/sqlite/wasm.sh does on the command line, and holds the answers of the shell
// to wasm-workload.expected, which is the output of the clang build.

import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { argv, exit } from 'node:process';
import { fileURLToPath } from 'node:url';

import { Rucc, runProgram } from '../../web/rucc.js';

const [, , compilerPath, builtinsPath, sysrootPath, sqlite] = argv;
if (sysrootPath === undefined) {
  console.error(
    'usage: node tests/web/run.mjs <rucc.wasm> <librucc_builtins.a> <sysroot.tar.gz> [<sqlite dir>]',
  );
  exit(2);
}
const here = fileURLToPath(new URL('.', import.meta.url));
const decoder = new TextDecoder();

let failed = 0;
function check(ok, what, detail = '') {
  console.log(`${ok ? 'ok' : 'FAILED'}: ${what}`);
  if (!ok) {
    failed += 1;
    if (detail) {
      console.log(detail.replace(/^/gm, '    '));
    }
  }
}

const started = performance.now();
const rucc = await Rucc.load({
  compiler: await readFile(compilerPath),
  builtins: await readFile(builtinsPath),
  sysroot: await readFile(sysrootPath),
});
const loaded = ((performance.now() - started) / 1000).toFixed(1);
check(true, `rucc.wasm compiled and installed its sysroot in ${loaded}s`);

const SUM = `#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
  char line[64];
  long total = 0;
  int lines = 0;
  while (fgets(line, sizeof line, stdin)) {
    total += strtol(line, NULL, 10);
    lines++;
  }
  printf("%d lines, total %ld\\n", lines, total);
  return lines == 3 ? 0 : 1;
}
`;

for (const level of ['-O0', '-O2']) {
  const built = await rucc.compile(SUM, [level]);
  check(built.module !== null, `a program that reads stdin builds at ${level}`, built.diagnostics);
  if (built.module === null) {
    continue;
  }
  const ran = await runProgram(built.module, { stdin: '40\n2\n-7\n' });
  const out = decoder.decode(ran.stdout);
  check(
    ran.status === 0 && out === '3 lines, total 35\n',
    `it runs and prints its total at ${level}`,
    `status ${ran.status}: ${out}`,
  );
}

const broken = await rucc.compile('int main(void) { return x; }\n');
check(
  broken.status !== 0 && broken.module === null && broken.diagnostics.includes("'x' undeclared"),
  'an error in the source comes back as a diagnostic',
  broken.diagnostics,
);

if (sqlite !== undefined) {
  const files = {
    'shell.c': await readFile(join(sqlite, 'shell.c')),
    'sqlite3.c': await readFile(join(sqlite, 'sqlite3.c')),
    'sqlite3.h': await readFile(join(sqlite, 'sqlite3.h')),
    'sqlite3ext.h': await readFile(join(sqlite, 'sqlite3ext.h')),
    'wasi-stubs.c': await readFile(join(here, '../sqlite/wasi-stubs.c')),
  };
  // The flags of tests/sqlite/wasm.sh.
  const flags = [
    '-DSQLITE_THREADSAFE=0', '-DSQLITE_OMIT_LOAD_EXTENSION', '-DSQLITE_OMIT_WAL',
    '-D_WASI_EMULATED_SIGNAL', '-D_WASI_EMULATED_PROCESS_CLOCKS',
    '-D_WASI_EMULATED_GETPID', '-D_WASI_EMULATED_MMAN',
    '-I/work', '/work/shell.c', '/work/sqlite3.c', '/work/wasi-stubs.c',
    '-lwasi-emulated-signal', '-lwasi-emulated-process-clocks',
    '-lwasi-emulated-getpid', '-lwasi-emulated-mman',
  ];
  const workload = await readFile(join(here, '../sqlite/wasm-workload.sql'));
  const expected = await readFile(join(here, '../sqlite/wasm-workload.expected'), 'utf8');
  for (const level of ['-O0', '-O2']) {
    const start = performance.now();
    const built = await rucc.build(files, [level, ...flags]);
    const seconds = ((performance.now() - start) / 1000).toFixed(1);
    const what = `the SQLite shell builds at ${level} in ${seconds}s`;
    check(built.module !== null, what, built.diagnostics);
    if (built.module === null) {
      continue;
    }
    const ran = await runProgram(built.module, { args: [':memory:'], stdin: workload });
    const out = decoder.decode(ran.stdout);
    check(
      ran.status === 0 && out === expected,
      `the SQLite shell from ${level} gives the answers of the clang build`,
      `status ${ran.status}\n${decoder.decode(ran.stderr)}`,
    );
  }
}

exit(failed === 0 ? 0 : 1);
