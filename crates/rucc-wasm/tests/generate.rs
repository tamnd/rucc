//! The wasm back end, from IR text to an object.
//!
//! Each test reads a module in the IR's text form, which is what `rucc --emit=ir` prints, and
//! writes the object. The test that runs the object needs `WASI_SDK_PATH`, as the tests of the
//! object writer do, and it says that it did nothing when the variable is not set. When
//! `wasm-tools` is on `PATH`, it also validates the object and the module.

use std::path::{Path, PathBuf};
use std::process::Command;

use rucc_base::Interner;
use rucc_target::wasm::Cpu;

/// A loop with block parameters, a switch with three cases and a default, and a `main` with a
/// local array, which is the C below at `-O1`. With one argument, `main` exits with 17.
///
/// ```c
/// int sum(const int *p, int n) { int s = 0; for (int i = 0; i < n; i++) s += p[i]; return s; }
/// int pick(int k) { switch (k) { case 1: return 10; case 2: return 20; case 5: return 7;
///                                default: return -1; } }
/// int main(int argc, char **argv) { int a[4] = {1, 2, 3, argc}; return sum(a, 4) + pick(argc); }
/// ```
const PROGRAM: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @sum(ptr, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = iconst.i32 0
    %3 = icmp slt %2, %1
    br_if %3, block3, block2(%2)

block1(%4: i32, %5: i32):
    %6 = iconst.i32 2
    %7 = shl.nsw %4, %6
    %8 = ptr_add %0, %7
    %9 = load.i32 %8, align 4
    %10 = add.nsw %5, %9
    %11 = iconst.i32 1
    %12 = add.nsw %4, %11
    %13 = icmp slt %12, %1
    br_if %13, block4, block5

block2(%14: i32):
    return %14

block3:
    jump block1(%2, %2)

block4:
    jump block1(%12, %10)

block5:
    jump block2(%10)
}

func @pick(i32) -> i32, linkage(external) {
block0(%0: i32):
    switch %0, block1, [1 => block2, 2 => block3, 5 => block4]

block1:
    %1 = iconst.i32 -1
    return %1

block2:
    %2 = iconst.i32 10
    return %2

block3:
    %3 = iconst.i32 20
    return %3

block4:
    %4 = iconst.i32 7
    return %4
}

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = alloca, size 16, align 4
    %3 = iconst.i32 1
    store %3 -> %2, align 4
    %4 = iconst.i32 4
    %5 = ptr_add %2, %4
    %6 = iconst.i32 2
    store %6 -> %5, align 4
    %7 = iconst.i32 8
    %8 = ptr_add %2, %7
    %9 = iconst.i32 3
    store %9 -> %8, align 4
    %10 = iconst.i32 12
    %11 = ptr_add %2, %10
    store %0 -> %11, align 4
    %12 = call.nofree @sum(%2, %4) : (ptr, i32) -> i32
    %13 = call.nofree @pick(%0) : (i32) -> i32
    %14 = add.nsw %12, %13
    return %14
}
"#;

fn object(text: &str) -> Result<rucc_object::wasm::Written, rucc_wasm::Refusal> {
    let mut names = Interner::new();
    let module = rucc_ir::parse(text, &mut names).expect("the IR parses");
    rucc_wasm::generate(&module, &names, Cpu::Lime1.features())
}

#[test]
fn the_object_defines_each_function_and_renames_main() {
    let written = object(PROGRAM).unwrap();
    assert_eq!(&written.bytes[..8], b"\0asm\x01\0\0\0");
    assert_eq!(written.defines, ["sum", "pick", "__main_argc_argv"]);
    for section in [&b"linking"[..], b"reloc.CODE", b"target_features", b"producers"] {
        assert!(written.bytes.windows(section.len()).any(|w| w == section));
    }
}

/// A loop that the entry jumps into at two places, which is not reducible, as a `goto` into a
/// loop makes it. The loop gets a dispatch node, and with one argument `main` goes in at
/// `block1` and exits with 90.
///
/// ```c
/// int main(int argc, char **argv) {
///   int i = 0, s = 0;
///   if (argc) goto a; else goto b;
/// a: s += 3;
/// b: i++; s *= 2; if (i < 4) goto a;
///   return s;
/// }
/// ```
const TWISTED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = iconst.i32 0
    br_if %0, block1(%2, %2), block2(%2, %2)

block1(%3: i32, %4: i32):
    %5 = iconst.i32 3
    %6 = add %4, %5
    jump block2(%3, %6)

block2(%7: i32, %8: i32):
    %9 = iconst.i32 1
    %10 = add %7, %9
    %11 = add %8, %8
    %12 = iconst.i32 4
    %13 = icmp slt %10, %12
    br_if %13, block1(%10, %11), block3(%11)

block3(%14: i32):
    return %14
}
"#;

#[test]
fn a_graph_that_is_not_reducible_gets_a_dispatch_node() {
    let written = object(TWISTED).unwrap();
    assert_eq!(written.defines, ["__main_argc_argv"]);
    // The dispatch is a `br_table` on the label local, and nothing else in the function has one.
    assert!(written.bytes.contains(&0x0e), "no br_table");
}

/// Where wasi-sdk is, or nothing when this machine has none.
fn sdk() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("WASI_SDK_PATH")?);
    path.join("bin/wasm-ld").exists().then_some(path)
}

/// A program on `PATH`, or nothing.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| p.exists())
}

fn run(program: &Path, args: &[&std::ffi::OsStr]) -> std::process::Output {
    let out = Command::new(program).args(args).output().expect("the tool starts");
    assert!(
        out.status.success() || program.ends_with("wasmtime"),
        "{}: {}",
        program.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Link the object for `text` with wasm-ld against wasi-libc, validate the object and the module
/// when `wasm-tools` is on `PATH`, and run the module under Wasmtime. The exit status, or nothing
/// when this machine has no wasi-sdk or no Wasmtime.
fn link_and_run(name: &str, text: &str) -> Option<i32> {
    let Some(sdk) = sdk() else {
        eprintln!("WASI_SDK_PATH is not set, so the object was not linked");
        return None;
    };
    let dir =
        std::env::temp_dir().join(format!("rucc-wasm-generate-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let object_file = dir.join("a.o");
    std::fs::write(&object_file, object(text).unwrap().bytes).unwrap();

    let lib = sdk.join("share/wasi-sysroot/lib/wasm32-wasip1");
    let module = dir.join("a.wasm");
    let crt = lib.join("crt1-command.o");
    let search = format!("-L{}", lib.display());
    let args: Vec<&std::ffi::OsStr> = vec![
        "-m".as_ref(),
        "wasm32".as_ref(),
        search.as_ref(),
        crt.as_os_str(),
        object_file.as_os_str(),
        "-lc".as_ref(),
        "-o".as_ref(),
        module.as_os_str(),
    ];
    run(&sdk.join("bin/wasm-ld"), &args);

    if let Some(tools) = on_path("wasm-tools") {
        run(&tools, &["validate".as_ref(), object_file.as_os_str()]);
        run(&tools, &["validate".as_ref(), module.as_os_str()]);
    }
    let Some(wasmtime) = on_path("wasmtime") else {
        eprintln!("there is no wasmtime on PATH, so the module was not run");
        return None;
    };
    let out = run(&wasmtime, &[module.as_os_str()]);
    std::fs::remove_dir_all(&dir).unwrap();
    out.status.code()
}

#[test]
fn the_object_links_with_wasm_ld_and_runs() {
    if let Some(status) = link_and_run("program", PROGRAM) {
        assert_eq!(status, 17);
    }
}

#[test]
fn the_dispatch_node_goes_to_the_entry_that_the_edge_named() {
    if let Some(status) = link_and_run("twisted", TWISTED) {
        assert_eq!(status, 90);
    }
}
