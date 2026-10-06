//! rucc as a WebAssembly reactor: the driver run more than once in one instance.
//!
//! Design: spec/wasm document 12 section 12.9, and tamnd/rucc#2867. Layer rank 14, beside `rucc`,
//! see `spec/18-package-layout.md`.
//!
//! The `rucc` binary for `wasm32-wasip1` is a command module, which runs `_start` once and is then
//! finished, so a page makes a new instance for each compile. A playground that compiles on each
//! change wants one instance that stays. This crate is that instance:
//!
//! ```text
//! cargo rustc -p rucc-reactor --release --target wasm32-wasip1 --crate-type cdylib
//! ```
//!
//! writes `rucc_reactor.wasm` in the target directory. The crate is an rlib everywhere else, so a
//! native build of the workspace does not link the compiler a second time. The module exports
//! `memory`, `_initialize`, which the host calls once before anything else, and these:
//!
//! | export | meaning |
//! |---|---|
//! | `rucc_alloc(len) -> ptr` | `len` bytes for the host to write arguments into |
//! | `rucc_free(ptr, len)` | gives back what `rucc_alloc` gave |
//! | `rucc_run(argv, argc) -> status` | runs the driver, as `main` does, and keeps what it wrote |
//! | `rucc_output(kind) -> ptr` | where the bytes of one output of the last run start |
//! | `rucc_output_len(kind) -> len` | how many bytes that output has |
//!
//! `argv` points to `argc` pairs of words, the address and the length of each argument in UTF-8,
//! and the first argument is the name of the program, as in `main`. A word is 32 bits on wasm32.
//! The output kinds are [`STDERR`], [`STDOUT`] and [`OUTPUT`]. A C function has one result, so an
//! output is two calls and not one call that gives a pair. The bytes of an output stay where they
//! are until the next `rucc_run`, and the host copies them out before it runs rucc again.
//!
//! The file system and the environment are the ones that the WASI host gave the instance, and they
//! stay between runs, so the sysroot is installed once. A trap, which a panic is on this target,
//! ends the instance, and the host makes a new one.

#![doc(html_root_url = "https://docs.rs/rucc-reactor/0.27.0")]

use std::cell::RefCell;

/// The kind of [`rucc_output`] for what the last run wrote to standard error: its diagnostics.
pub const STDERR: u32 = 0;
/// The kind of [`rucc_output`] for what the last run wrote to standard output.
pub const STDOUT: u32 = 1;
/// The kind of [`rucc_output`] for the bytes of the file that the last run named with `-o`. It is
/// empty when the run named none, or failed, or the file cannot be read.
pub const OUTPUT: u32 = 2;

thread_local! {
    /// The outputs of the last run, in the order of their kinds.
    static LAST: RefCell<[Vec<u8>; 3]> = const { RefCell::new([Vec::new(), Vec::new(), Vec::new()]) };
}

/// Runs the constructors of the module, once, as `_initialize` in `crt1-reactor.o` of wasi-libc
/// does. rustc links a cdylib with no start object, and wasm-ld then wraps every export in a call
/// to the constructors and the destructors, as for a command. A module that exports
/// `_initialize` is a reactor to wasm-ld and to the hosts, and gets no wrappers.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn _initialize() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" {
        fn __wasm_call_ctors();
    }
    if !DONE.swap(true, Ordering::Relaxed) {
        // SAFETY: wasm-ld defines the function, and the guard runs it once.
        unsafe { __wasm_call_ctors() };
    }
}

/// `len` bytes for the host, set to zero. The address is not null when `len` is not zero.
#[unsafe(no_mangle)]
pub extern "C" fn rucc_alloc(len: usize) -> *mut u8 {
    Box::into_raw(vec![0_u8; len].into_boxed_slice()).cast()
}

/// Gives back the `len` bytes at `ptr` that [`rucc_alloc`] gave.
///
/// # Safety
///
/// `ptr` and `len` are a result of `rucc_alloc` and the length given to it, and are given back
/// once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rucc_free(ptr: *mut u8, len: usize) {
    // SAFETY: the caller gives back a box that `rucc_alloc` made with this length.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
}

/// Runs the driver with the `argc` arguments at `argv` and gives its exit status. An argument that
/// is not UTF-8 is status 1 with a message, as on the command line.
///
/// # Safety
///
/// `argv` points to `argc` pairs of an address and a length, and each pair names bytes that the
/// host has written.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rucc_run(argv: *const usize, argc: usize) -> i32 {
    let mut args = Vec::with_capacity(argc);
    for at in 0..argc {
        // SAFETY: the caller gives `argc` pairs at `argv`, and each pair names bytes in memory.
        let bytes = unsafe {
            let ptr = *argv.add(2 * at) as *const u8;
            std::slice::from_raw_parts(ptr, *argv.add(2 * at + 1))
        };
        match std::str::from_utf8(bytes) {
            Ok(arg) => args.push(arg.to_owned()),
            Err(_) => {
                let why = format!("rucc: error: argument {at} is not UTF-8\n");
                LAST.with(|last| *last.borrow_mut() = [why.into_bytes(), Vec::new(), Vec::new()]);
                return 1;
            }
        }
    }
    let (program, rest) = args.split_first().map_or(("rucc", &[][..]), |(p, r)| (p.as_str(), r));
    let (status, captured) = rucc_driver::host::capture(|| rucc_driver::run_as(program, rest));
    let output = match (status, output(rest)) {
        (0, Some(path)) => std::fs::read(path).unwrap_or_default(),
        _ => Vec::new(),
    };
    LAST.with(|last| *last.borrow_mut() = [captured.stderr, captured.stdout, output]);
    status
}

/// Where the bytes of output `kind` of the last run start. A kind that does not exist is empty.
#[unsafe(no_mangle)]
pub extern "C" fn rucc_output(kind: u32) -> *const u8 {
    LAST.with(|last| last.borrow().get(kind as usize).map_or(std::ptr::null(), |out| out.as_ptr()))
}

/// How many bytes output `kind` of the last run has.
#[unsafe(no_mangle)]
pub extern "C" fn rucc_output_len(kind: u32) -> usize {
    LAST.with(|last| last.borrow().get(kind as usize).map_or(0, Vec::len))
}

/// The file that `-o` names in `args`, the last one when there are more, as the driver takes it.
fn output(args: &[String]) -> Option<&str> {
    let mut named = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "-o" {
            named = args.next().map(String::as_str);
        } else if let Some(path) = arg.strip_prefix("-o") {
            named = Some(path);
        }
    }
    named
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    /// Runs the reactor as a host does, with the words of `line` written to memory it gave.
    fn run(line: &str) -> i32 {
        let line = args(line);
        let mut pairs = Vec::new();
        let mut given = Vec::new();
        for arg in &line {
            let ptr = rucc_alloc(arg.len());
            // SAFETY: `rucc_alloc` gave `arg.len()` bytes at `ptr`.
            unsafe { std::ptr::copy_nonoverlapping(arg.as_ptr(), ptr, arg.len()) };
            pairs.extend([ptr as usize, arg.len()]);
            given.push((ptr, arg.len()));
        }
        // SAFETY: the pairs name the bytes written above, which stay until they are given back.
        let status = unsafe { rucc_run(pairs.as_ptr(), line.len()) };
        for (ptr, len) in given {
            // SAFETY: each one is a result of `rucc_alloc` with its length, given back once.
            unsafe { rucc_free(ptr, len) };
        }
        status
    }

    fn last(kind: u32) -> String {
        // SAFETY: the output is this many bytes at this address until the next run.
        let bytes = unsafe { std::slice::from_raw_parts(rucc_output(kind), rucc_output_len(kind)) };
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn the_output_is_the_last_file_that_dash_o_names() {
        assert_eq!(output(&args("-O2 a.c -o a.wasm")), Some("a.wasm"));
        assert_eq!(output(&args("-oa.wasm a.c")), Some("a.wasm"));
        assert_eq!(output(&args("-o a.wasm -o b.wasm a.c")), Some("b.wasm"));
        assert_eq!(output(&args("-fsyntax-only a.c")), None);
        assert_eq!(output(&args("a.c -o")), None);
    }

    #[test]
    fn each_run_keeps_what_it_wrote_until_the_next_run() {
        assert_ne!(run("rucc --no-such-flag"), 0);
        assert!(last(STDERR).contains("--no-such-flag"), "{}", last(STDERR));
        assert_eq!(last(STDOUT), "");
        assert_eq!(rucc_output_len(OUTPUT), 0);
        assert_eq!(rucc_output_len(7), 0);

        assert_eq!(run("rucc --version"), 0);
        assert!(last(STDOUT).contains(rucc_driver::VERSION), "{}", last(STDOUT));
        assert_eq!(last(STDERR), "");
    }
}
