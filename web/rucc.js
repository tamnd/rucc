// rucc in a browser: the compiler as a wasm32-wasip1 command module, run with browser_wasi_shim
// over a file system in memory (spec/wasm, document 12 section 12.8, and tamnd/rucc#2867).
//
// The same file runs in a page and in Node. The page maps the bare name of the shim to its copy of
// the shim with an import map, and Node finds the shim in web/node_modules, which `npm ci` in web/
// writes. tests/web/run.mjs is the test that CI runs with it.
//
// A command module runs `_start` once, so each run of rucc is a new instance of the one compiled
// module. What stays between two runs is the file system, which is one `Map` that every instance
// gets as its preopened `/`. [`Rucc.load`] puts the sysroot archive in the download cache of rucc
// and runs `rucc --fetch wasm32-wasip1`, which finds the archive there with the right hash and
// unpacks it with the readers inside rucc, so the page has no tar reader of its own. The builtins
// archive goes in /rucc and every compile names it with `-B/rucc/`, because a wasm module cannot
// say where its own file is.
//
// [`Reactor`] is the same with rucc_reactor.wasm, the reactor build of section 12.9. It makes one
// instance and runs the driver in it again and again through `rucc_run`, so a run starts no new
// instance. The file system is the same `Map`, and the API is the API of [`Rucc`].

import { Directory, File, OpenFile, PreopenDirectory, WASI } from '@bjorn3/browser_wasi_shim';

/** The name that rucc gives the sysroot archive in its download cache. */
const SYSROOT = 'rucc-sysroot-wasm32-wasip1.tar.gz';

/** The environment of every run of rucc. WASI has no temporary directory, so rucc is told one. */
const ENV = ['RUCC_CACHE_DIR=/cache', 'TMPDIR=/tmp'];

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** The sha256 of some bytes, in hex. */
async function sha256(bytes) {
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
  return Array.from(digest, (b) => b.toString(16).padStart(2, '0')).join('');
}

/**
 * The contents of the directory at `path` under `root`, made with its parents if it is not there.
 * A path is relative to `root` and has `/` between its parts.
 */
export function directory(root, path) {
  let at = root;
  for (const part of path.split('/').filter(Boolean)) {
    let next = at.get(part);
    if (next === undefined) {
      next = new Directory(new Map());
      at.set(part, next);
    }
    if (!(next instanceof Directory)) {
      throw new Error(`/${path} has a file where a directory must be`);
    }
    at = next.contents;
  }
  return at;
}

/**
 * Runs a command module once, with `root` as its `/`, and gives its exit status and what it wrote.
 * A trap, such as a failed `memory.grow` or an unreachable, is status 134 with the trap at the end
 * of stderr, so that a caller sees one kind of failure.
 *
 * Standard output and standard error are regular files in memory, as with `>out 2>err` on the
 * command line. The `ConsoleStdout` of the shim says that it is a terminal, and a program that
 * asks writes another thing to a terminal: the SQLite shell draws boxes, and a compiler adds
 * colours.
 */
export async function run(module, root, args, { env = [], stdin = new Uint8Array() } = {}) {
  const stdout = new File(new Uint8Array());
  const stderr = new File(new Uint8Array());
  const fds = [
    new OpenFile(new File(stdin)),
    new OpenFile(stdout),
    new OpenFile(stderr),
    new PreopenDirectory('/', root),
  ];
  const wasi = new WASI(args, env, fds, { debug: false });
  const imports = { wasi_snapshot_preview1: wasi.wasiImport };
  const instance = await WebAssembly.instantiate(module, imports);
  let status;
  try {
    status = wasi.start(instance);
  } catch (why) {
    const trap = encoder.encode(`${args[0]}: the module stopped: ${why}\n`);
    const data = new Uint8Array(stderr.data.length + trap.length);
    data.set(stderr.data);
    data.set(trap, stderr.data.length);
    stderr.data = data;
    status = 134;
  }
  return { status, stdout: stdout.data, stderr: stderr.data };
}

/** rucc, compiled once, with the file system that its runs share. */
export class Rucc {
  constructor(module, root) {
    this.module = module;
    this.root = root;
  }

  /**
   * Compiles rucc and installs its sysroot. `compiler` is rucc.wasm as bytes or as a compiled
   * module, `builtins` is librucc_builtins.a for wasm32-wasip1, and `sysroot` is the archive of
   * the sysroot that this release of rucc pins. rucc checks the hash of the archive against its
   * own record, so another archive is refused with the message of rucc.
   */
  static async load({ compiler, builtins, sysroot }) {
    const module =
      compiler instanceof WebAssembly.Module ? compiler : await WebAssembly.compile(compiler);
    const root = new Map();
    const hash = await sha256(sysroot);
    directory(root, `cache/downloads/${hash.slice(0, 12)}`).set(SYSROOT, new File(sysroot));
    directory(root, 'rucc').set('librucc_builtins.a', new File(builtins));
    directory(root, 'tmp');
    directory(root, 'work');
    const rucc = await this.open(module, root);
    const fetched = await rucc.run(['--fetch', 'wasm32-wasip1']);
    if (fetched.status !== 0) {
      throw new Error(decoder.decode(fetched.stderr));
    }
    // The archive is not needed again once the tree is there, and it is memory.
    directory(root, 'cache').delete('downloads');
    return rucc;
  }

  /** A `Rucc` for `module`, with `root` as its `/`. */
  static async open(module, root) {
    return new Rucc(module, root);
  }

  /** Runs rucc with these arguments, after the name of the program. */
  run(args, options = {}) {
    return run(this.module, this.root, ['rucc', ...args], { env: ENV, ...options });
  }

  /**
   * Compiles and links C source for wasm32-wasip1. `files` maps a name in /work to its text or
   * bytes, and `args` is the rest of the command line, which names the files as /work/<name>.
   * The result has the exit status, the diagnostics, and the module, which is null when rucc
   * failed.
   */
  async build(files, args) {
    const work = directory(this.root, 'work');
    work.clear();
    for (const [name, text] of Object.entries(files)) {
      work.set(name, new File(typeof text === 'string' ? encoder.encode(text) : text));
    }
    const line = ['--target=wasm32-wasip1', '-B/rucc/', ...args, '-o', '/work/a.out.wasm'];
    const result = await this.run(line);
    const out = work.get('a.out.wasm');
    return {
      status: result.status,
      diagnostics: decoder.decode(result.stderr) + decoder.decode(result.stdout),
      module: result.status === 0 && out instanceof File ? out.data : null,
    };
  }

  /** Compiles and links one C file, `main.c`, with `-O2` or with the flags given. */
  compile(source, flags = ['-O2']) {
    return this.build({ 'main.c': source }, [...flags, '/work/main.c']);
  }
}

/**
 * rucc as a reactor, which is one instance that [`Rucc.load`] makes and every run uses. Give
 * `load` rucc_reactor.wasm, from `cargo rustc -p rucc-reactor --release --target wasm32-wasip1
 * --crate-type cdylib`, as `compiler`.
 *
 * The driver writes into buffers inside the instance and not to the file descriptors, and
 * `run` copies them out. Standard input is empty, because rucc reads none for a compile. A trap
 * ends the instance, so the run that trapped is status 134, as with [`Rucc`], and the next run
 * makes a new instance over the same file system.
 */
export class Reactor extends Rucc {
  /** How many instances this reactor has made. One, unless a run trapped. */
  instances = 0;

  static async open(module, root) {
    const reactor = new Reactor(module, root);
    await reactor.start();
    return reactor;
  }

  /** Makes the instance and runs its constructors. */
  async start() {
    const fds = [
      new OpenFile(new File(new Uint8Array())),
      new OpenFile(new File(new Uint8Array())),
      new OpenFile(new File(new Uint8Array())),
      new PreopenDirectory('/', this.root),
    ];
    const wasi = new WASI(['rucc'], ENV, fds, { debug: false });
    const imports = { wasi_snapshot_preview1: wasi.wasiImport };
    this.instance = await WebAssembly.instantiate(this.module, imports);
    wasi.initialize(this.instance);
    this.instances += 1;
  }

  async run(args) {
    if (this.instance === null) {
      await this.start();
    }
    const exports = this.instance.exports;
    const words = ['rucc', ...args].map((arg) => encoder.encode(arg));
    // Each pair is the address and the length of one argument, in two 32 bit words.
    const pairs = exports.rucc_alloc(8 * words.length);
    const given = [[pairs, 8 * words.length]];
    for (const [at, word] of words.entries()) {
      const ptr = exports.rucc_alloc(word.length);
      given.push([ptr, word.length]);
      // An allocation can grow the memory, which makes a new buffer, so a view is made after it.
      new Uint8Array(exports.memory.buffer, ptr, word.length).set(word);
      const view = new DataView(exports.memory.buffer);
      view.setUint32(pairs + 8 * at, ptr, true);
      view.setUint32(pairs + 8 * at + 4, word.length, true);
    }
    let status;
    try {
      status = exports.rucc_run(pairs, words.length);
    } catch (why) {
      this.instance = null;
      return {
        status: 134,
        stdout: new Uint8Array(),
        stderr: encoder.encode(`rucc: the module stopped: ${why}\n`),
      };
    }
    // A copy of each output, because the next run reuses the memory.
    const output = (kind) => {
      const ptr = exports.rucc_output(kind);
      return new Uint8Array(exports.memory.buffer, ptr, exports.rucc_output_len(kind)).slice();
    };
    const result = { status, stderr: output(0), stdout: output(1) };
    for (const [ptr, len] of given) {
      exports.rucc_free(ptr, len);
    }
    return result;
  }
}

/**
 * Runs a module that rucc linked, with a file system of its own that is empty, and gives its exit
 * status and what it wrote. `stdin` is text or bytes.
 */
export async function runProgram(bytes, { args = [], stdin = '' } = {}) {
  const module = await WebAssembly.compile(bytes);
  const input = typeof stdin === 'string' ? encoder.encode(stdin) : stdin;
  return run(module, new Map(), ['main.wasm', ...args], { stdin: input });
}
