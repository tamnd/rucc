//! The wasm back end, from IR text to an object.
//!
//! Each test reads a module in the IR's text form, which is what `rucc --emit=ir` prints, and
//! writes the object. The test that runs the object needs `WASI_SDK_PATH`, as the tests of the
//! object writer do, and it says that it did nothing when the variable is not set. When
//! `wasm-tools` is on `PATH`, it also validates the object and the module.

use std::path::{Path, PathBuf};
use std::process::Command;

use rucc_base::Interner;
use rucc_object::wasm::{HIDDEN, SymbolKind, WEAK};
use rucc_target::wasm::{Cpu, Feature, Features};

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
    object_for(text, Cpu::Lime1.features())
}

/// [`object`] for a target with these features.
fn object_for(
    text: &str,
    features: Features,
) -> Result<rucc_object::wasm::Written, rucc_wasm::Refusal> {
    let mut names = Interner::new();
    let mut module = rucc_ir::parse(text, &mut names).expect("the IR parses");
    rucc_wasm::prepare(&mut module, &mut names)?;
    rucc_wasm::generate(&module, &names, features)
}

/// The `-S` text of the object for `text`.
fn assembly(text: &str) -> String {
    assembly_for(text, Cpu::Lime1.features())
}

/// [`assembly`] for a target with these features.
fn assembly_for(text: &str, features: Features) -> String {
    let mut names = Interner::new();
    let mut module = rucc_ir::parse(text, &mut names).expect("the IR parses");
    rucc_wasm::prepare(&mut module, &mut names).unwrap();
    let object = rucc_wasm::translate(&module, &names, features).unwrap();
    rucc_wasm::assembly(&object).unwrap()
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

/// A computed `goto` with a table of label addresses and a table of distances between labels.
/// The two tables are in data, so the label numbers are written in the segments, and the code
/// takes the address of `add` with `block_addr`. With one argument, `main` exits with 22, which
/// is what clang gives for the C below.
///
/// ```c
/// int main(int argc, char **argv) {
///     static void *const table[] = { &&add, &&twice, &&stop };
///     static const int from_add[] = { 0, &&twice - &&add, &&stop - &&add };
///     static const unsigned char program[] = { 0, 1, 0, 1, 2 };
///     int s = argc, pc = 0;
///     goto *table[program[pc]];
/// add:
///     s += 3;
///     goto *(&&add + from_add[program[++pc]]);
/// twice:
///     s *= 2;
///     goto *table[program[++pc]];
/// stop:
///     return s;
/// }
/// ```
const THREADED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

global @program.2 : bytes 5 = { i8 0, i8 1, i8 0, i8 1, i8 2 }, align 1, linkage(internal), constant, droppable
global @from_add.1 : bytes 12 = { i32 0, apart.4 @.Llbl.1 from @.Llbl.0, apart.4 @.Llbl.2 from @.Llbl.0 }, align 4, linkage(internal), constant, droppable
global @table.0 : bytes 12 = { addr.4 @.Llbl.0, addr.4 @.Llbl.1, addr.4 @.Llbl.2 }, align 4, linkage(internal), constant, droppable

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = iconst.i32 0
    %3 = global_addr @table.0
    %4 = load %3, align 4
    indirect_br %4, block1(%0, %2), block2(%0, %2), block3(%0)

block1(%5: i32, %6: i32):
    %7 = iconst.i32 3
    %8 = add.nsw %5, %7
    %9 = block_addr block1
    %10 = global_addr @from_add.1
    %11 = global_addr @program.2
    %12 = iconst.i32 1
    %13 = add.nsw %6, %12
    %14 = ptr_add %11, %13
    %15 = load.i8 %14, align 1
    %16 = zext.i32 %15
    %17 = iconst.i32 2
    %18 = shl.nsw %16, %17
    %19 = ptr_add %10, %18
    %20 = load.i32 %19, align 4
    %21 = ptr_add %9, %20
    indirect_br %21, block1(%8, %13), block2(%8, %13), block3(%8)

block2(%22: i32, %23: i32):
    %24 = iconst.i32 2
    %25 = add.nsw %22, %22
    %26 = global_addr @table.0
    %27 = global_addr @program.2
    %28 = iconst.i32 1
    %29 = add.nsw %23, %28
    %30 = ptr_add %27, %29
    %31 = load.i8 %30, align 1
    %32 = zext.i32 %31
    %33 = shl.nsw %32, %24
    %34 = ptr_add %26, %33
    %35 = load %34, align 4
    indirect_br %35, block1(%25, %29), block2(%25, %29), block3(%25)

block3(%36: i32):
    return %36

labels:
    block1 = @.Llbl.0
    block2 = @.Llbl.1
    block3 = @.Llbl.2
}
"#;

#[test]
fn a_computed_goto_is_a_br_table_on_the_number_of_the_label() {
    let written = object(THREADED).unwrap();
    assert_eq!(written.defines, ["__main_argc_argv"]);
    assert!(written.bytes.contains(&0x0e), "no br_table");
}

/// An `__int128` in two `i64` values: a call with two of them and one returned through memory,
/// shifts by a constant across the halves, a sign extension, a compare of the high halves and a
/// truncation. None of it calls the runtime. With one argument, `main` exits with 123, which is
/// what clang gives for the C below at `-O1`.
///
/// ```c
/// typedef __int128 i128;
/// i128 wide(i128 a, i128 b) { return a + b - (b >> 70); }
/// int main(int argc, char **argv) {
///   i128 x = (i128)argc << 64;
///   x = wide(x + 0x7fffffffffffffffLL, (i128)-argc);
///   unsigned __int128 y = (unsigned __int128)x ^ ((unsigned __int128)argc << 100);
///   return (int)(x >> 60) + (y > ((unsigned __int128)1 << 64)) * 100 + (x < 0);
/// }
/// ```
const WIDE: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @wide(i128, i128) -> i128, linkage(external) {
block0(%0: i128, %1: i128):
    %2 = iconst.i128 70
    %3 = ashr %1, %2
    %4 = add %0, %1
    %5 = sub %4, %3
    return %5
}

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = sext.i128 %0
    %3 = iconst.i128 64
    %4 = shl %2, %3
    %5 = iconst.i128 9223372036854775807
    %6 = add.nsw %4, %5
    %7 = iconst.i32 0
    %8 = sub.nsw %7, %0
    %9 = sext.i128 %8
    %10 = call.nofree @wide(%6, %9) : (i128, i128) -> i128
    %11 = iconst.i32 100
    %12 = iconst.i128 100
    %13 = shl %2, %12
    %14 = xor %10, %13
    %15 = iconst.i128 18446744073709551616
    %16 = icmp ugt %14, %15
    %17 = zext.i32 %16
    %18 = iconst.i128 60
    %19 = ashr %10, %18
    %20 = trunc.i32 %19
    %21 = mul.nsw %17, %11
    %22 = add.nsw %20, %21
    %23 = iconst.i128 0
    %24 = icmp slt %10, %23
    %25 = zext.i32 %24
    %26 = add.nsw %22, %25
    return %26
}
"#;

/// Shifts of `i8` values by a count of 8, which the IR takes modulo 8, so that each shift leaves
/// its value as it is. With one argument, `main` exits with 3 + 64 + 1 = 68. A count that is masked
/// to the 8 bits of its type and not to the 3 bits of the width gives 0.
const NARROW: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = trunc.i8 %0
    %3 = iconst.i8 7
    %4 = add %2, %3
    %5 = iconst.i8 3
    %6 = shl %5, %4
    %7 = iconst.i8 64
    %8 = lshr %7, %4
    %9 = iconst.i8 -128
    %10 = ashr %9, %4
    %11 = zext.i32 %6
    %12 = zext.i32 %8
    %13 = icmp eq %10, %9
    %14 = zext.i32 %13
    %15 = add %11, %12
    %16 = add %15, %14
    return %16
}
"#;

#[test]
fn a_narrow_shift_takes_its_count_modulo_its_width() {
    if let Some(status) = link_and_run("narrow", NARROW) {
        assert_eq!(status, 68);
    }
}

/// Compiler barriers between two stores, as `__atomic_signal_fence` and the `asm` of a lock write
/// them. They are no code, so the two stores stay and `main` exits with 0.
///
/// ```c
/// int g;
/// int main(void) { g = 1; __asm__ volatile(" " ::: "memory", "cc"); g = 2; __asm__(""); return 0; }
/// ```
const BARRIER: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

global @g : bytes 4 = { zero 4 }, align 4, linkage(external)

func @main() -> i32, linkage(external) {
block0:
    %0 = global_addr @g
    %1 = iconst.i32 1
    store %1 -> %0, align 4
    inline_asm.volatile " ", "", "memory,cc"()
    %2 = iconst.i32 2
    store %2 -> %0, align 4
    %3 = iconst.i32 0
    inline_asm.volatile.nomem "", "", ""()
    return %3
}
"#;

#[test]
fn a_compiler_barrier_is_no_code_and_other_assembly_is_refused() {
    let text = assembly(BARRIER);
    assert_eq!(text.matches("i32.store").count(), 2, "{text}");
    if let Some(status) = link_and_run("barrier", BARRIER) {
        assert_eq!(status, 0);
    }
    for asm in [
        r#"%4 = inline_asm.i32.nomem "local.get 0", "=r", ""()"#,
        r#"inline_asm.volatile "nop", "", ""()"#,
        r#"inline_asm.volatile "", "", "memory,r0"()"#,
    ] {
        let text = BARRIER.replace(r#"inline_asm.volatile.nomem "", "", ""()"#, asm);
        let Err(refusal) = object(&text) else { panic!("the assembly is not refused") };
        assert!(refusal.why.contains("inline assembly"), "{refusal}");
    }
}

#[test]
fn an_int128_is_two_i64_values_and_is_returned_through_memory() {
    let written = object(WIDE).unwrap();
    assert_eq!(written.defines, ["wide", "__main_argc_argv"]);
}

/// Where wasi-sdk is, or nothing when this machine has none.
/// The tree form names each local by the IR value that it holds and each label local by its
/// dispatch node, marks the code of each IR block where it starts, and indents each construct one
/// step more than the construct around it.
#[test]
fn the_tree_form_names_the_values_and_marks_the_blocks() {
    let mut names = Interner::new();
    let module = rucc_ir::parse(TWISTED, &mut names).expect("the IR parses");
    let text = rucc_wasm::tree(&module, &names, Cpu::Lime1.features()).unwrap();
    assert!(text.starts_with("function __main_argc_argv (i32, i32) -> (i32)\n"), "{text}");
    for line in ["  param 0 i32 %0\n", "local.set label0\n", "local.get label0\n"] {
        assert!(text.contains(line), "{line:?} is not in\n{text}");
    }
    for note in ["; dispatch0 follows", "; loop of dispatch0", "; back to dispatch0"] {
        assert!(text.contains(note), "{note:?} is not in\n{text}");
    }
    for block in 0..4 {
        let mark = format!("; block{block}\n");
        assert!(text.contains(&mark), "{mark:?} is not in\n{text}");
    }
    let mut open = Vec::new();
    for line in text.lines().skip_while(|l| !l.starts_with("  block")).filter(|l| !l.is_empty()) {
        let indent = line.len() - line.trim_start().len();
        let op = line.trim_start().split([' ', '\t']).next().unwrap_or_default();
        match op {
            "block" | "loop" | "if" => open.push(indent),
            "else" => assert_eq!(Some(&indent), open.last(), "{line}"),
            "end_block" | "end_loop" | "end_if" => assert_eq!(open.pop(), Some(indent), "{line}"),
            "end_function" => assert!(open.is_empty() && indent == 0, "{line}"),
            _ => assert_eq!(indent, 2 * open.len() + 2, "{line}"),
        }
    }
}

/// The address of a function, in the code and in the data, and no call through a pointer.
///
/// ```c
/// int f(void) { return 7; }
/// int (*p)(void) = f;
/// void *g(void) { return (void *)f; }
/// ```
const ADDRESSED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

global @p : bytes 4 = { addr.4 @f }, align 4, linkage(external)

func @f() -> i32, linkage(external) {
block0:
    %0 = iconst.i32 7
    return %0
}

func @g() -> ptr, linkage(external) {
block0:
    %0 = global_addr @f
    return %0
}
"#;

/// clang imports the table and keeps it when a function has an address, on a target with the long
/// form of `call_indirect`, and not on `mvp`. rucc does the same, so that the two objects have
/// the same symbols.
#[test]
fn the_address_of_a_function_imports_the_table_as_clang_does() {
    let mut names = Interner::new();
    let module = rucc_ir::parse(ADDRESSED, &mut names).expect("the IR parses");
    for (cpu, wanted) in [(Cpu::Lime1, true), (Cpu::Mvp, false)] {
        let object = rucc_wasm::translate(&module, &names, cpu.features()).unwrap();
        let table = object.symbols.iter().find(|s| s.name == "__indirect_function_table");
        assert_eq!(table.is_some(), wanted, "{}", cpu.name());
        if let Some(table) = table {
            assert_ne!(table.flags & rucc_object::wasm::NO_STRIP, 0);
        }
    }
}

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
    link_and_run_as(name, text, false)
}

/// [`link_and_run`], with the object that clang assembles from the `-S` text in place of the one
/// that rucc writes when `assembled` is set.
fn link_and_run_as(name: &str, text: &str, assembled: bool) -> Option<i32> {
    link_and_run_for(name, text, assembled, Cpu::Lime1.features())
}

/// [`link_and_run_as`] for a target with these features.
fn link_and_run_for(name: &str, text: &str, assembled: bool, features: Features) -> Option<i32> {
    let Some(sdk) = sdk() else {
        eprintln!("WASI_SDK_PATH is not set, so the object was not linked");
        return None;
    };
    let dir =
        std::env::temp_dir().join(format!("rucc-wasm-generate-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let object_file = dir.join("a.o");
    if assembled {
        let listing = dir.join("a.s");
        std::fs::write(&listing, assembly_for(text, features)).unwrap();
        let args: Vec<&std::ffi::OsStr> = vec![
            "--target=wasm32-wasip1".as_ref(),
            "-mcpu=lime1".as_ref(),
            "-mexception-handling".as_ref(),
            "-mtail-call".as_ref(),
            "-c".as_ref(),
            listing.as_os_str(),
            "-o".as_ref(),
            object_file.as_os_str(),
        ];
        run(&sdk.join("bin/clang"), &args);
    } else {
        std::fs::write(&object_file, object_for(text, features).unwrap().bytes).unwrap();
    }

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
        "-lsetjmp".as_ref(),
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

#[test]
fn the_halves_of_an_int128_carry_into_each_other() {
    if let Some(status) = link_and_run("wide", WIDE) {
        assert_eq!(status, 123);
    }
}

#[test]
fn a_computed_goto_arrives_at_the_label_that_the_table_names() {
    if let Some(status) = link_and_run("threaded", THREADED) {
        assert_eq!(status, 22);
    }
}

/// The text closes each construct with the `end` that names it, gives each `select` its type and
/// declares each function before a `call` names it, which are the three things that clang's
/// assembler asks of the text and that the binary does not say.
#[test]
fn the_text_closes_each_construct_with_the_end_that_names_it() {
    let texts =
        [("program", PROGRAM), ("twisted", TWISTED), ("threaded", THREADED), ("sjlj", SJLJ)];
    for (name, text) in texts {
        let listing = assembly(text);
        let count = |word: &str| listing.lines().filter(|l| l.trim_start() == word).count();
        let opened = |word: &str| {
            let prefix = format!("{word}\t");
            listing
                .lines()
                .filter(|l| l.trim_start() == word || l.trim_start().starts_with(&prefix))
                .count()
        };
        assert_eq!(opened("block"), count("end_block"), "{name}: {listing}");
        assert_eq!(opened("loop"), count("end_loop"), "{name}: {listing}");
        assert_eq!(opened("if"), count("end_if"), "{name}: {listing}");
        assert_eq!(opened("try_table"), count("end_try_table"), "{name}: {listing}");
        assert_eq!(count("end_function"), listing.matches(",@function").count(), "{name}");
        assert_eq!(count("end"), 0, "{name}: {listing}");
        assert_eq!(count("select"), 0, "{name}: {listing}");
        for line in listing.lines() {
            if let Some(callee) = line.strip_prefix("\tcall\t") {
                let declared = format!("\t.functype\t{callee} (");
                let at = listing.find(&declared).unwrap_or(usize::MAX);
                assert!(at < listing.find(line).unwrap(), "{name}: `{callee}` is not declared");
            }
        }
    }
    assert!(assembly(THREADED).contains("\tbr_table\t{"));
}

/// clang assembles the text into an object that links and does what the object of rucc does.
#[test]
fn the_text_assembles_to_a_module_that_does_the_same() {
    for (name, text, status) in [
        ("program", PROGRAM, 17),
        ("twisted", TWISTED, 90),
        ("threaded", THREADED, 22),
        ("wide", WIDE, 123),
        ("sjlj", SJLJ, 42),
    ] {
        if let Some(got) = link_and_run_as(&format!("{name}-s"), text, true) {
            assert_eq!(got, status, "{name}");
        }
    }
}

/// A `longjmp` back to a `setjmp` in the same function, and one that goes through a function with
/// a `setjmp` of its own to a function further out, which is the C below at `-O1`. `main` exits
/// with 42.
///
/// ```c
/// static jmp_buf b, d;
/// __attribute__((noinline)) void jump(jmp_buf e, int v) { longjmp(e, v); }
/// __attribute__((noinline)) int inner(void) {
///     jmp_buf c; if (setjmp(c)) return 100; jump(d, 30); return 0; }
/// int main(void) { int r = setjmp(b); if (r == 0) jump(b, 12);
///                  int s = setjmp(d); if (s == 0) inner(); return r + s; }
/// ```
const SJLJ: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

global @d : bytes 164 = { zero 164 }, align 4, linkage(internal), droppable
global @b : bytes 164 = { zero 164 }, align 4, linkage(internal), droppable

func @setjmp(ptr) -> i32, linkage(external), attrs(returns_twice);

func @longjmp(ptr, i32), linkage(external), attrs(noreturn);

func @jump(ptr, i32), linkage(external), attrs(noreturn, noinline) {
block0(%0: ptr, %1: i32):
    call @longjmp(%0, %1) : (ptr, i32)
    unreachable
}

func @inner() -> i32, linkage(external), attrs(noinline) {
block0:
    %0 = alloca, size 164, align 4
    %1 = call @setjmp(%0) : (ptr) -> i32
    %2 = iconst.i32 0
    %3 = icmp ne %1, %2
    br_if %3, block1, block2

block1:
    %4 = iconst.i32 100
    return %4

block2:
    %5 = global_addr @d
    %6 = iconst.i32 30
    call @jump(%5, %6) : (ptr, i32)
    unreachable
}

func @main() -> i32, linkage(external) {
block0:
    %0 = global_addr @b
    %1 = call @setjmp(%0) : (ptr) -> i32
    %2 = iconst.i32 0
    %3 = icmp eq %1, %2
    br_if %3, block1, block2

block1:
    %4 = global_addr @b
    %5 = iconst.i32 12
    call @jump(%4, %5) : (ptr, i32)
    unreachable

block2:
    %6 = global_addr @d
    %7 = call @setjmp(%6) : (ptr) -> i32
    %8 = iconst.i32 0
    %9 = icmp eq %7, %8
    br_if %9, block3, block4

block3:
    %10 = call @inner() : () -> i32
    jump block4

block4:
    %11 = add.nsw %1, %7
    return %11
}
"#;

/// `setjmp` is `__wasm_setjmp` of libsetjmp, `longjmp` is `__wasm_longjmp`, and each call after a
/// `setjmp` is in a `try_table` that catches the tag `__c_longjmp`. The object says that it uses
/// exception handling. A function that returns twice and is not `setjmp` is refused.
#[test]
fn setjmp_and_longjmp_are_the_calls_and_the_catches_of_libsetjmp() {
    let listing = assembly(SJLJ);
    for want in [
        "\tcall\t__wasm_setjmp\n",
        "\tcall\t__wasm_setjmp_test\n",
        "\tcall\t__wasm_longjmp\n",
        "\ttry_table\t(catch __c_longjmp 0)\n",
        "\t.tagtype\t__c_longjmp i32\n",
    ] {
        assert!(listing.contains(want), "{want:?} in {listing}");
    }
    assert!(!listing.contains("\tcall\tsetjmp\n") && !listing.contains("\tcall\tlongjmp\n"));
    let written = object(SJLJ).unwrap();
    assert!(written.bytes.windows(18).any(|w| w == b"exception-handling"));
    assert!(!object(PROGRAM).unwrap().bytes.windows(18).any(|w| w == b"exception-handling"));
    if let Some(status) = link_and_run("sjlj", SJLJ) {
        assert_eq!(status, 42);
    }

    let vfork = SJLJ
        .replace("@setjmp(ptr) -> i32", "@vfork() -> i32")
        .replace("call @setjmp(%0) : (ptr)", "call @vfork() : ()");
    let refusal = object(&vfork).unwrap_err();
    assert!(refusal.why.contains("`vfork` returns twice"), "{refusal}");
    assert_eq!(refusal.function.as_deref(), Some("inner"));
}

/// `__builtin_setjmp` in `main` and `__builtin_longjmp` in a function that `main` calls, which is
/// the C below at `-O1`. `main` exits with 42.
///
/// ```c
/// static void *buf[5];
/// __attribute__((noinline)) void jump(void) { __builtin_longjmp(buf, 1); }
/// int main(void) { volatile int n = 0; if (__builtin_setjmp(buf)) return n + 40; n = 2; jump();
///                  return 1; }
/// ```
const BUILTIN_SJLJ: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

global @buf : bytes 20 = { zero 20 }, align 4, linkage(internal), droppable

func @jump(), linkage(external), attrs(noinline) {
block0:
    %0 = global_addr @buf
    longjmp_marker %0
    return
}

func @main() -> i32, linkage(external) {
block0:
    %0 = alloca, size 4, align 4
    %1 = iconst.i32 0
    store.volatile %1 -> %0, align 4
    %2 = global_addr @buf
    %3 = setjmp_marker.i32 %2
    %4 = icmp ne %3, %1
    br_if %4, block1, block2

block1:
    %5 = load.i32.volatile %0, align 4
    %6 = iconst.i32 40
    %7 = add.nsw %5, %6
    return %7

block2:
    %8 = iconst.i32 2
    store.volatile %8 -> %0, align 4
    call.nofree @jump() : ()
    %9 = iconst.i32 1
    return %9
}
"#;

/// `__builtin_setjmp` is a call of `__wasm_setjmp` and `__builtin_longjmp` is a call of
/// `__wasm_longjmp` with the value 1, as for `setjmp` and `longjmp`.
#[test]
fn builtin_setjmp_and_longjmp_take_the_path_of_setjmp_and_longjmp() {
    let listing = assembly(BUILTIN_SJLJ);
    for want in [
        "\tcall\t__wasm_setjmp\n",
        "\tcall\t__wasm_longjmp\n",
        "\ttry_table\t(catch __c_longjmp 0)\n",
    ] {
        assert!(listing.contains(want), "{want:?} in {listing}");
    }
    for assembled in [false, true] {
        if let Some(status) = link_and_run_as("builtin-sjlj", BUILTIN_SJLJ, assembled) {
            assert_eq!(status, 42, "assembled: {assembled}");
        }
    }
}

/// Calls in tail position, which `tail::mark` writes as the C below at `-O2`. `even` and `odd` call
/// each other a million times, which is deeper than the stack of Wasmtime when each call keeps
/// its frame. `pass` calls through a pointer, and `count` drops the answer of `twice`. `main`
/// exits with 31.
///
/// ```c
/// int odd(unsigned n);
/// int even(unsigned n) { if (n == 0) return 1; return odd(n - 1); }
/// int odd(unsigned n) { if (n == 0) return 0; return even(n - 1); }
/// int (*volatile hop)(unsigned) = even;
/// int pass(unsigned n) { return hop(n); }
/// static int sink;
/// int twice(int n) { sink += n; return 2 * n; }
/// void count(int n) { twice(n); }
/// int main(void) { count(10); count(20); return pass(1000000) + sink; }
/// ```
const TAIL: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

global @hop : bytes 4 = { addr.4 @even }, align 4, linkage(external)
global @sink : bytes 4 = { zero 4 }, align 4, linkage(internal)

func @even(i32) -> i32, linkage(external), attrs(noinline) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp eq %0, %1
    br_if %2, block1, block2

block1:
    %3 = iconst.i32 1
    return %3

block2:
    %4 = iconst.i32 1
    %5 = sub %0, %4
    tail_call @odd(%5) : (i32) -> i32
}

func @odd(i32) -> i32, linkage(external), attrs(noinline) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp eq %0, %1
    br_if %2, block1, block2

block1:
    %3 = iconst.i32 0
    return %3

block2:
    %4 = iconst.i32 1
    %5 = sub %0, %4
    tail_call @even(%5) : (i32) -> i32
}

func @pass(i32) -> i32, linkage(external), attrs(noinline) {
block0(%0: i32):
    %1 = global_addr @hop
    %2 = load.volatile.ptr %1, align 4
    tail_call %2(%0) : (i32) -> i32
}

func @twice(i32) -> i32, linkage(external), attrs(noinline) {
block0(%0: i32):
    %1 = global_addr @sink
    %2 = load.i32 %1, align 4
    %3 = add %2, %0
    store %3 -> %1, align 4
    %4 = add %0, %0
    return %4
}

func @count(i32), linkage(external), attrs(noinline) {
block0(%0: i32):
    tail_call @twice(%0) : (i32) -> i32
}

func @main() -> i32, linkage(external) {
block0:
    %0 = iconst.i32 10
    call @count(%0) : (i32)
    %1 = iconst.i32 20
    call @count(%1) : (i32)
    %2 = iconst.i32 1000000
    %3 = call @pass(%2) : (i32) -> i32
    %4 = global_addr @sink
    %5 = load.i32 %4, align 4
    %6 = add %3, %5
    return %6
}
"#;

/// With the tail-call feature, a tail call is `return_call` or `return_call_indirect`, and the
/// object says that it uses the feature. A tail call whose answer the caller drops stays a call
/// and a `return`, because `return_call` must give back what the caller gives back. Without the
/// feature, each tail call is a call and a `return`.
#[test]
fn a_tail_call_is_return_call_when_the_target_has_the_feature() {
    let tail = Cpu::Lime1.features().with(Feature::TailCall);
    let listing = assembly_for(TAIL, tail);
    for want in [
        "\treturn_call\todd\n",
        "\treturn_call\teven\n",
        "\treturn_call_indirect\t__indirect_function_table, (i32) -> (i32)\n",
        "\tcall\ttwice\n",
    ] {
        assert!(listing.contains(want), "{want:?} in {listing}");
    }
    assert!(!listing.contains("return_call\ttwice"), "{listing}");
    assert!(object_for(TAIL, tail).unwrap().bytes.windows(9).any(|w| w == b"tail-call"));
    let plain = assembly(TAIL);
    assert!(!plain.contains("return_call"), "{plain}");
    assert!(!object(TAIL).unwrap().bytes.windows(9).any(|w| w == b"tail-call"));
    for assembled in [false, true] {
        if let Some(status) = link_and_run_for("tail", TAIL, assembled, tail) {
            assert_eq!(status, 31, "assembled: {assembled}");
        }
    }
}

/// A byte swap of 32 bits. With one argument, `main` exits with 0x33, the second byte from the
/// top of 0x44332211. A swap of the bytes in each half gives 0x22114433 and an exit with 0x11.
///
/// ```c
/// int main(int argc, char **argv) {
///   unsigned y = __builtin_bswap32(0x11223344u * argc);
///   return (y >> 16) & 0xff;
/// }
/// ```
const SWAPPED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = iconst.i32 287454020
    %3 = mul %0, %2
    %4 = bswap %3
    %5 = iconst.i32 16
    %6 = lshr %4, %5
    %7 = iconst.i32 255
    %8 = and %6, %7
    return %8
}
"#;

#[test]
fn a_byte_swap_of_32_bits_reverses_the_four_bytes() {
    for assembled in [false, true] {
        if let Some(status) = link_and_run_as("swapped", SWAPPED, assembled) {
            assert_eq!(status, 0x33, "assembled: {assembled}");
        }
    }
}

/// Two functions that each keep the address of a label, as gcc's torture test `990208-1.c` does
/// with two copies of an inline function. On the native rows the two addresses are different, so
/// `main` exits with 1.
///
/// ```c
/// static void *p, *q;
/// __attribute__((noinline)) void f(void) { here: p = &&here; }
/// __attribute__((noinline)) void g(void) { there: q = &&there; }
/// int main(void) { f(); g(); return p != q; }
/// ```
const TWO_LABELS: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

global @q : bytes 4 = { zero 4 }, align 4, linkage(internal), droppable
global @p : bytes 4 = { zero 4 }, align 4, linkage(internal), droppable

func @f(), linkage(external), attrs(noinline) {
block0:
    jump block1

block1:
    %0 = global_addr @p
    %1 = block_addr block1
    store %1 -> %0, align 4
    return
}

func @g(), linkage(external), attrs(noinline) {
block0:
    jump block1

block1:
    %0 = global_addr @q
    %1 = block_addr block1
    store %1 -> %0, align 4
    return
}

func @main() -> i32, linkage(external) {
block0:
    call @f() : ()
    call @g() : ()
    %0 = global_addr @p
    %1 = load %0, align 4
    %2 = global_addr @q
    %3 = load %2, align 4
    %4 = icmp ne %1, %3
    %5 = zext.i32 %4
    return %5
}
"#;

#[test]
fn the_labels_of_two_functions_have_two_addresses() {
    let text = assembly(TWO_LABELS);
    assert!(text.contains("i32.const\t1\n"), "{text}");
    assert!(text.contains("i32.const\t2\n"), "{text}");
    if let Some(status) = link_and_run("labels", TWO_LABELS) {
        assert_eq!(status, 1);
    }
}

/// Locals of a struct with no members, which GNU C gives the size 0. The frame is empty, so the
/// address of the local is the stack pointer, and `main` exits with 0.
///
/// ```c
/// struct empty {};
/// struct empty *where(void) { struct empty e; return &e; }
/// int main(void) { struct empty a; where(); return 0; }
/// ```
const EMPTY: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @where() -> ptr, linkage(external) {
block0:
    %0 = alloca, align 1
    return %0
}

func @main() -> i32, linkage(external) {
block0:
    %0 = alloca, align 1
    %1 = call @where() : () -> ptr
    %2 = iconst.i32 0
    return %2
}
"#;

#[test]
fn a_local_of_size_zero_needs_no_frame() {
    let text = assembly(EMPTY);
    assert!(text.contains("global.get\t__stack_pointer"), "{text}");
    if let Some(status) = link_and_run("empty", EMPTY) {
        assert_eq!(status, 0);
    }
}

/// Each form of an `asm` with a blank template that rucc takes on wasm, which is the C below at
/// `-O0`. An output tied to an input gets the input, an output that nothing is tied to gets zero,
/// an output in memory keeps its value, and an `asm goto` goes to its first target. `main` exits
/// with 7 + 30 + 0 + 4 + 2, which is 43.
///
/// ```c
/// int tied(int x) { int t; __asm__("" : "=r"(t) : "0"(x)); return t; }
/// long long both(long long x) { __asm__ volatile("" : "+r"(x)); return x; }
/// int none(void) { int t; __asm__("" : "=r"(t)); return t; }
/// int mem(int x) { __asm__ volatile("" : "+m"(x) : : "memory"); return x; }
/// int jump(int x) { __asm__ goto("" : : : : out); return x + 1; out: return 0; }
/// int main(void) { return tied(7) + (int)both(30) + none() + mem(4) + jump(1); }
/// ```
const ASM: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @tied(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = inline_asm.nomem "", "=r,0", ""(%0)
    return %1
}

func @both(i64) -> i64, linkage(external) {
block0(%0: i64):
    %1 = inline_asm.volatile.nomem "", "+r", ""(%0)
    return %1
}

func @none() -> i32, linkage(external) {
block0:
    %0 = inline_asm.i32.nomem "", "=r", ""()
    return %0
}

func @mem(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4, tbaa !1
    inline_asm.volatile "", "+m", "memory"(%1)
    %2 = load.i32 %1, align 4, tbaa !1
    return %2
}

func @jump(i32) -> i32, linkage(external) {
block0(%0: i32):
    inline_asm.volatile.nomem "", "", ""(), labels [block1, block2]

block1:
    %1 = iconst.i32 1
    %2 = add.nsw %0, %1
    return %2

block2:
    %3 = iconst.i32 0
    return %3
}

func @main() -> i32, linkage(external) {
block0:
    %0 = iconst.i32 7
    %1 = call @tied(%0) : (i32) -> i32
    %2 = iconst.i32 30
    %3 = sext.i64 %2
    %4 = call @both(%3) : (i64) -> i64
    %5 = trunc.i32 %4
    %6 = add.nsw %1, %5
    %7 = call @none() : () -> i32
    %8 = add.nsw %6, %7
    %9 = iconst.i32 4
    %10 = call @mem(%9) : (i32) -> i32
    %11 = add.nsw %8, %10
    %12 = iconst.i32 1
    %13 = call @jump(%12) : (i32) -> i32
    %14 = add.nsw %11, %13
    return %14
}

!0 = tbaa "char", offset 0
!1 = tbaa "int", parent !0, offset 0
"#;

#[test]
fn an_asm_with_a_blank_template_is_no_code() {
    // The `asm goto` is a terminator, and it is a branch to the block of its first target.
    let text = assembly(ASM);
    assert!(text.contains("i32.const\t1\n"), "{text}");
    if let Some(status) = link_and_run("asm", ASM) {
        assert_eq!(status, 43);
    }
}

/// A switch on a 128-bit value, which is the C below at `-O0`. Each case is a test of both halves,
/// so the case 2^64 is not the case 0. `main` exits with 1 + 2 * 4 + 3 * 16 + 0 * 64, which is 57.
///
/// ```c
/// unsigned char pick(__int128 v) {
///     switch (v) { case 0: return 1; case 1: return 2; case (__int128)1 << 64: return 3; }
///     return 0;
/// }
/// int main(void) {
///     return pick(0) + pick(1) * 4 + pick((__int128)~0ull + 1) * 16 + pick(2) * 64;
/// }
/// ```
const SWITCH128: &str = r#"; ModuleID = '/tmp/wa0/asm/s.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @pick(i128) -> i8 zext, linkage(external) {
block0(%0: i128):
    switch %0, block1, [0 => block2, 1 => block3, 18446744073709551616 => block4]

block1:
    %1 = iconst.i32 0
    %2 = trunc.i8 %1
    return %2

block2:
    %3 = iconst.i32 1
    %4 = trunc.i8 %3
    return %4

block3:
    %5 = iconst.i32 2
    %6 = trunc.i8 %5
    return %6

block4:
    %7 = iconst.i32 3
    %8 = trunc.i8 %7
    return %8
}

func @main() -> i32, linkage(external) {
block0:
    %0 = iconst.i32 0
    %1 = sext.i128 %0
    %2 = call @pick(%1) : (i128) -> i8 zext
    %3 = zext.i32 %2
    %4 = iconst.i32 1
    %5 = sext.i128 %4
    %6 = call @pick(%5) : (i128) -> i8 zext
    %7 = zext.i32 %6
    %8 = iconst.i32 4
    %9 = mul.nsw %7, %8
    %10 = add.nsw %3, %9
    %11 = iconst.i64 0
    %12 = iconst.i64 -1
    %13 = xor %11, %12
    %14 = zext.i128 %13
    %15 = iconst.i32 1
    %16 = sext.i128 %15
    %17 = add.nsw %14, %16
    %18 = call @pick(%17) : (i128) -> i8 zext
    %19 = zext.i32 %18
    %20 = iconst.i32 16
    %21 = mul.nsw %19, %20
    %22 = add.nsw %10, %21
    %23 = iconst.i32 2
    %24 = sext.i128 %23
    %25 = call @pick(%24) : (i128) -> i8 zext
    %26 = zext.i32 %25
    %27 = iconst.i32 64
    %28 = mul.nsw %26, %27
    %29 = add.nsw %22, %28
    return %29
}
"#;

#[test]
fn a_switch_on_128_bits_tests_both_halves() {
    let text = assembly(SWITCH128);
    assert_eq!(text.matches("i64.eq").count(), 6, "{text}");
    if let Some(status) = link_and_run("switch128", SWITCH128) {
        assert_eq!(status, 57);
    }
}

/// A second name of a variable and two of a static function, one of them weak, which is the C
/// below at `-O1`. With one argument, `main` exits with 24.
///
/// ```c
/// int a[4] = {1, 2, 3, 4};
/// extern int b[4] __attribute__((alias("a")));
/// static int f(int x) { return x + 1; }
/// int g(int) __attribute__((alias("f")));
/// int w(int) __attribute__((weak, alias("f")));
/// int (*p)(int) = &w;
/// int main(int argc, char **argv) { b[1] = 20; return g(a[1]) + p(argc) + (&g == &f); }
/// ```
const ALIASES: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

global @p : bytes 4 = { addr.4 @w }, align 4, linkage(external)
global @a : bytes 16 = { i32 1, i32 2, i32 3, i32 4 }, align 4, linkage(external)

alias @b = @a, linkage(external)
alias @g = @f, linkage(external)
alias @w = @f, linkage(weak)

func @f(i32) -> i32, linkage(internal) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = add.nsw %0, %1
    return %2
}

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = global_addr @b
    %3 = iconst.i32 4
    %4 = ptr_add %2, %3
    %5 = iconst.i32 20
    store %5 -> %4, align 4
    %6 = global_addr @a
    %7 = ptr_add %6, %3
    %8 = load.i32 %7, align 4
    %9 = call @g(%8) : (i32) -> i32
    %10 = global_addr @p
    %11 = load %10, align 4
    %12 = call_indirect %11(%0) : (i32) -> i32
    %13 = add.nsw %9, %12
    %14 = global_addr @g
    %15 = global_addr @f
    %16 = icmp eq %14, %15
    %17 = zext.i32 %16
    %18 = add.nsw %13, %17
    return %18
}
"#;

/// A second name of a function is a symbol with the index of the function and the flags of its
/// own linkage, and a second name of a variable is a data symbol at the place of the variable,
/// as clang writes them. The text sets each second name of a function after the code.
#[test]
fn an_alias_is_a_second_symbol_for_the_same_function_or_place() {
    let mut names = Interner::new();
    let module = rucc_ir::parse(ALIASES, &mut names).expect("the IR parses");
    let object = rucc_wasm::translate(&module, &names, Cpu::Lime1.features()).unwrap();
    let symbol = |name: &str| {
        let index = object.symbols.iter().position(|s| s.name == name).unwrap();
        (u32::try_from(index).unwrap(), &object.symbols[index])
    };
    let (f, _) = symbol("f");
    for (name, flags) in [("g", HIDDEN), ("w", WEAK | HIDDEN)] {
        let (index, alias) = symbol(name);
        assert!(object.aliases.contains(&(index, f)), "{name}");
        assert_eq!(alias.flags, flags, "{name}");
    }
    let (_, a) = symbol("a");
    let (_, b) = symbol("b");
    assert!(matches!(a.kind, SymbolKind::Data { place: Some(_) }));
    assert_eq!(a.kind, b.kind);

    let written = rucc_object::wasm::write(&object).unwrap();
    assert_eq!(written.defines, ["__main_argc_argv", "p", "a", "b", "g", "w"]);
    let listing = rucc_wasm::assembly(&object).unwrap();
    assert!(listing.contains("\n\t.weak\tw\n\t.type\tw,@function\nw = f\n"), "{listing}");

    for assembled in [false, true] {
        if let Some(status) = link_and_run_as("aliases", ALIASES, assembled) {
            assert_eq!(status, 24, "assembled: {assembled}");
        }
    }
}

/// `int main(int argc) { return argc + 40; }` at `-O1`. With no arguments, it exits with 41.
const MAIN_ONE: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @main(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 40
    %2 = add.nsw %0, %1
    return %2
}
"#;

/// `int main(int argc, char **argv, char **envp)` that gives `argc + 2 * (argv[0] != 0) +
/// 4 * (envp != 0)`, at `-O1`. With no arguments, it exits with 7.
const MAIN_THREE: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @main(i32, ptr, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr, %2: ptr):
    %3 = iconst.i32 2
    %4 = iconst.i32 0
    %5 = load %1, align 4
    %6 = inttoptr.ptr %4
    %7 = icmp ne %5, %6
    %8 = zext.i32 %7
    %9 = add.nsw %8, %8
    %10 = add.nsw %0, %9
    %11 = icmp ne %2, %6
    %12 = zext.i32 %11
    %13 = shl.nsw %12, %3
    %14 = add.nsw %10, %13
    return %14
}
"#;

/// A `main` with one parameter or with three keeps its name, and the object defines the
/// `__main_argc_argv` that the start code of wasi-libc calls with two arguments. It calls `main`
/// with the count, the vector and the environment, as many as `main` takes.
#[test]
fn a_main_with_one_or_three_parameters_gets_a_caller_with_two() {
    for (name, text, status) in [("main-one", MAIN_ONE, 41), ("main-three", MAIN_THREE, 7)] {
        let written = object(text).unwrap();
        assert_eq!(written.defines, ["main", "__main_argc_argv"], "{name}");
        for assembled in [false, true] {
            if let Some(got) = link_and_run_as(name, text, assembled) {
                assert_eq!(got, status, "{name}, assembled: {assembled}");
            }
        }
    }
}

/// A load before a store, and a call before a load. The C of `f` is below at `-O1`, and `h` is
/// the same with the call first.
///
/// ```c
/// int g(int);
/// int f(int *p, int *q, int n) { int x = *p; *q = n; return x * n + g(n); }
/// ```
const STACKED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @g(i32) -> i32, linkage(external);

func @f(ptr, ptr, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: ptr, %2: i32):
    %3 = load.i32 %0, align 4
    store %2 -> %1, align 4
    %4 = mul.nsw %3, %2
    %5 = call @g(%2) : (i32) -> i32
    %6 = add.nsw %4, %5
    return %6
}

func @h(ptr, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = call @g(%1) : (i32) -> i32
    %3 = load.i32 %0, align 4
    %4 = add.nsw %2, %3
    return %4
}
"#;

/// The body of `name` in the `-S` text of `text`, at `-O2` when `optimize` is set and at `-O0`
/// when it is not.
fn body_of(text: &str, name: &str, optimize: bool) -> String {
    let mut names = Interner::new();
    let module = rucc_ir::parse(text, &mut names).expect("the IR parses");
    let options = rucc_wasm::Options { features: Cpu::Lime1.features(), optimize };
    let object = rucc_wasm::translate(&module, &names, options).unwrap();
    let listing = rucc_wasm::assembly(&object).unwrap();
    let start = listing.find(&format!("\n{name}:\n")).expect("the function is in the listing");
    let end = start + listing[start..].find("end_function").expect("the function ends");
    listing[start..end].to_owned()
}

/// With optimization, a value with one use in its block stays on the operand stack where its
/// instruction can move to the use. The load does not move past the store, and the call does not
/// move past the load. The multiplication moves past the call, and the call moves to the add,
/// with nothing between. A value written to a local takes the local of a parameter that is dead.
/// Without optimization, each value has a local.
#[test]
fn a_value_with_one_use_stays_on_the_stack_when_its_instruction_can_move() {
    let f = body_of(STACKED, "f", true);
    let code: Vec<&str> = f.lines().skip(3).map(str::trim).filter(|l| !l.is_empty()).collect();
    assert_eq!(
        code,
        [
            "local.get\t0",
            "i32.load\t0",
            "local.set\t0",
            "local.get\t1",
            "local.get\t2",
            "i32.store\t0",
            "local.get\t0",
            "local.get\t2",
            "i32.mul",
            "local.get\t2",
            "call\tg",
            "i32.add",
            "return",
        ],
        "{f}"
    );
    let h = body_of(STACKED, "h", true);
    assert!(h.contains("call\tg\n\tlocal.set\t1\n"), "{h}");
    assert!(h.contains("i32.load\t0\n\ti32.add\n\treturn\n"), "{h}");
    let f = body_of(STACKED, "f", false);
    assert_eq!(f.matches("local.set").count(), 4, "{f}");
}

/// A value with two uses, the first one a store.
///
/// ```c
/// int f(int *p, int n) { int x = n * n; *p = x; return x + n; }
/// ```
const TEED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @f(ptr, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = mul.nsw %1, %1
    store %2 -> %0, align 4
    %3 = add.nsw %2, %1
    return %3
}
"#;

/// With optimization, a value with more than one use is written at its first use, with a
/// `local.tee` that keeps a copy for the other uses.
#[test]
fn a_value_with_more_uses_is_written_at_the_first_one_with_a_tee() {
    let f = body_of(TEED, "f", true);
    let tee =
        "local.get\t0\n\tlocal.get\t1\n\tlocal.get\t1\n\ti32.mul\n\tlocal.tee\t2\n\ti32.store\t0\n";
    assert!(f.contains(tee), "{f}");
    assert!(f.contains("i32.store\t0\n\tlocal.get\t2\n\tlocal.get\t1\n\ti32.add\n"), "{f}");
    assert_eq!(f.matches("local.set").count(), 0, "{f}");
    let f = body_of(TEED, "f", false);
    assert_eq!(f.matches("local.tee").count(), 0, "{f}");
}

/// A compare with two uses, the first one in a value that is an argument of one edge of a
/// `br_if`. sqlite3_uri_boolean in SQLite has this shape after more inlining.
///
/// ```c
/// int f(int a, int b) { int x = a != 0; return b ? x + b : x; }
/// ```
const EDGE_OPERAND: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 0
    %3 = icmp ne %0, %2
    %4 = zext.i32 %3
    %5 = icmp ne %1, %2
    br_if %5, block1, block2(%4)

block1:
    %6 = zext.i32 %3
    %7 = add %6, %1
    jump block2(%7)

block2(%8: i32):
    return %8
}
"#;

/// With optimization, the operands of a value that moves to one edge of a `br_if` are pushed on
/// that edge only, so an operand with another use is not written there with a `local.tee`. It is
/// written to its local before the branch, and both paths read the local. The branch on `%5` reads
/// `%1` as it is, so the only `i32.ne` is the one of `%3`.
#[test]
fn an_operand_of_an_edge_argument_is_not_teed_on_the_edge() {
    let f = body_of(EDGE_OPERAND, "f", true);
    assert!(f.contains("local.get\t0\n\ti32.const\t0\n\ti32.ne\n\tlocal.set\t0\n"), "{f}");
    assert_eq!(f.matches("local.tee").count(), 0, "{f}");
    assert_eq!(f.matches("i32.ne").count(), 1, "{f}");
    assert!(f.contains("local.get\t1\n\tif\n"), "{f}");
}

/// A call in the value that a function with a frame returns.
///
/// ```c
/// int g(int *);
/// int f(int a) { return g(&a) + a; }
/// ```
const FRAMED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @g(ptr) -> i32, linkage(external);

func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    %2 = call @g(%1) : (ptr) -> i32
    %3 = add.nsw %2, %0
    return %3
}
"#;

/// With optimization, a function with a frame gives the frame back before its `return` pushes
/// the value. A call in an operand of that value is made before the frame is given back, because
/// the callee would put its own frame on top of the slot that its argument points to.
#[test]
fn a_call_under_the_return_of_a_function_with_a_frame_stays_before_the_epilogue() {
    let f = body_of(FRAMED, "f", true);
    let call = f.find("call\tg").expect("the call is written");
    let epilogue = f.rfind("global.set").expect("the frame is given back");
    assert!(call < epilogue, "{f}");
}

/// Three reads of a structure, which is the C below, and the same reads with a `ptr_add` that
/// has no `nuw` and with one that goes back.
///
/// ```c
/// struct s { int a, b, c; };
/// int f(struct s *p) { p->c = p->a; return p->b; }
/// ```
const FOLDED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @f(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = load.i32 %0, align 4
    %2 = iconst.i32 8
    %3 = ptr_add.nuw %0, %2
    store %1 -> %3, align 4
    %4 = iconst.i32 4
    %5 = ptr_add.nuw %0, %4
    %6 = load.i32 %5, align 4
    return %6
}

func @g(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = iconst.i32 4
    %2 = ptr_add %0, %1
    %3 = load.i32 %2, align 4
    %4 = iconst.i32 -4
    %5 = ptr_add.nuw %0, %4
    %6 = load.i32 %5, align 4
    %7 = add %3, %6
    return %7
}
"#;

/// With optimization, a load or a store whose address is a `ptr_add` with `nuw` of a constant
/// that is not negative puts the constant in its offset field, and the `ptr_add` is not written.
/// A `ptr_add` with no `nuw`, or with a negative constant, stays an `i32.add`.
#[test]
fn a_constant_offset_with_nuw_goes_in_the_offset_field() {
    let f = body_of(FOLDED, "f", true);
    assert!(f.contains("local.get\t0\n\tlocal.get\t0\n\ti32.load\t0\n\ti32.store\t8\n"), "{f}");
    assert!(f.contains("local.get\t0\n\ti32.load\t4\n"), "{f}");
    assert!(!f.contains("i32.add"), "{f}");
    let g = body_of(FOLDED, "g", true);
    assert_eq!(g.matches("i32.add").count(), 3, "{g}");
    assert!(!g.contains("i32.load\t4") && !g.contains("i32.load\t4294967292"), "{g}");
    let f = body_of(FOLDED, "f", false);
    assert!(f.contains("i32.store\t0\n") && f.contains("i32.load\t0\n"), "{f}");
    assert!(!f.contains("i32.load\t4") && !f.contains("i32.store\t8"), "{f}");
}

/// Calls to `memcpy`, `memset` and `memmove`. The answer of the copy is the destination of the
/// fill, and the answer of the fill is the answer of `f`. The copy in `g` has a small constant
/// length, and `h` is marked `no_builtin`.
const BULK: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @memcpy(ptr, ptr, i32) -> ptr, linkage(external);

func @memset(ptr, i32, i32) -> ptr, linkage(external);

func @memmove(ptr, ptr, i32) -> ptr, linkage(external);

func @f(ptr, ptr, i32) -> ptr, linkage(external) {
block0(%0: ptr, %1: ptr, %2: i32):
    %3 = call @memcpy(%0, %1, %2) : (ptr, ptr, i32) -> ptr
    %4 = iconst.i32 0
    %5 = call @memset(%3, %4, %2) : (ptr, i32, i32) -> ptr
    return %5
}

func @g(ptr, ptr, i32), linkage(external) {
block0(%0: ptr, %1: ptr, %2: i32):
    %3 = iconst.i32 8
    %4 = call @memcpy(%0, %1, %3) : (ptr, ptr, i32) -> ptr
    %5 = call @memmove(%0, %1, %2) : (ptr, ptr, i32) -> ptr
    return
}

func @h(ptr, ptr, i32), linkage(external), attrs(no_builtin) {
block0(%0: ptr, %1: ptr, %2: i32):
    %3 = call @memcpy(%0, %1, %2) : (ptr, ptr, i32) -> ptr
    return
}
"#;

/// The body of `name` in the `-S` text of `text` at `-O2`, after [`rucc_wasm::bulk`] for a
/// target with these features.
fn bulk_body_of(text: &str, name: &str, features: Features) -> String {
    let mut names = Interner::new();
    let mut module = rucc_ir::parse(text, &mut names).expect("the IR parses");
    rucc_wasm::bulk(&mut module, &names, features);
    let options = rucc_wasm::Options { features, optimize: true };
    let object = rucc_wasm::translate(&module, &names, options).unwrap();
    let listing = rucc_wasm::assembly(&object).unwrap();
    let start = listing.find(&format!("\n{name}:\n")).expect("the function is in the listing");
    let end = start + listing[start..].find("end_function").expect("the function ends");
    listing[start..end].to_owned()
}

/// With the bulk memory feature, a call to `memcpy` or `memmove` is `memory.copy` and a call to
/// `memset` is `memory.fill`, and the answer is the destination. A copy of a small constant length
/// is loads and stores, each as wide as the bytes left allow. A function marked `no_builtin`, and
/// a target with no bulk memory, keep the calls.
#[test]
fn a_call_to_memcpy_or_memset_is_a_bulk_memory_instruction() {
    let lime = Cpu::Lime1.features();
    let f = bulk_body_of(BULK, "f", lime);
    let wanted = "local.get\t0\n\tlocal.get\t1\n\tlocal.get\t2\n\tmemory.copy\t0, 0\n\t\
                  local.get\t0\n\ti32.const\t0\n\tlocal.get\t2\n\tmemory.fill\t0\n\t\
                  local.get\t0\n";
    assert!(f.contains(wanted), "{f}");
    assert!(!f.contains("call"), "{f}");
    let g = bulk_body_of(BULK, "g", lime);
    assert_eq!(g.matches("memory.copy").count(), 1, "{g}");
    // The copy of 8 bytes has an alignment of 1, and wasm loads and stores at any address, so
    // it is one piece with the hint of a byte.
    assert!(g.contains("i64.load\t0:p2align=0\n\ti64.store\t0:p2align=0\n"), "{g}");
    assert!(!g.contains("i32.store8"), "{g}");
    assert!(!g.contains("call"), "{g}");
    let h = bulk_body_of(BULK, "h", lime);
    assert!(h.contains("call\tmemcpy") && !h.contains("memory.copy"), "{h}");
    let f = bulk_body_of(BULK, "f", Cpu::Mvp.features());
    assert!(f.contains("call\tmemcpy") && f.contains("call\tmemset"), "{f}");
}

/// Short copies of the IR at three alignments. See
/// [`a_short_copy_is_as_wide_as_its_length_allows_and_its_alignment_is_a_hint`].
const SHORT: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @bytes(ptr, ptr), linkage(external) {
block0(%0: ptr, %1: ptr):
    memcpy %0, %1, size 20, align 1
    return
}

func @shorts(ptr, ptr), linkage(external) {
block0(%0: ptr, %1: ptr):
    memcpy %0, %1, size 7, align 8
    return
}

func @moved(ptr, ptr), linkage(external) {
block0(%0: ptr, %1: ptr):
    memmove %0, %1, size 4, align 1
    return
}
"#;

/// A copy of a small constant length is pieces of 8, 4, 2 and 1 bytes whatever its alignment,
/// because wasm loads and stores at any address. The alignment of each piece goes in its hint,
/// and a piece at an offset has the alignment that the offset leaves. A `memmove` of 4 bytes with
/// an alignment of 1 is one load and one store, where it was `memory.copy`.
#[test]
fn a_short_copy_is_as_wide_as_its_length_allows_and_its_alignment_is_a_hint() {
    let lime = Cpu::Lime1.features();
    let bytes = bulk_body_of(SHORT, "bytes", lime);
    for piece in ["i64.store\t0:p2align=0", "i64.store\t8:p2align=0", "i32.store\t16:p2align=0"] {
        assert!(bytes.contains(piece), "{bytes}");
    }
    assert!(!bytes.contains("store8"), "{bytes}");
    let shorts = bulk_body_of(SHORT, "shorts", lime);
    for piece in ["i32.store\t0\n", "i32.store16\t4\n", "i32.store8\t6\n"] {
        assert!(shorts.contains(piece), "{shorts}");
    }
    let moved = bulk_body_of(SHORT, "moved", lime);
    assert!(moved.contains("i32.load\t0:p2align=0\n\ti32.store\t0:p2align=0\n"), "{moved}");
    assert!(!moved.contains("memory.copy"), "{moved}");
}

/// A loop with two ways out, which is the C below at `-O1`.
///
/// ```c
/// int find(const int *p, int n, int k) {
///   for (int i = 0; i < n; i++)
///     if (p[i] == k) return i;
///   return -1;
/// }
/// ```
const FIND: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @find(ptr, i32, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: i32, %2: i32):
    %3 = iconst.i32 0
    %4 = icmp slt %3, %1
    br_if %4, block5, block2

block1(%5: i32):
    %6 = iconst.i32 2
    %7 = shl.nsw %5, %6
    %8 = ptr_add %0, %7
    %9 = load.i32 %8, align 4
    %10 = icmp eq %9, %2
    br_if %10, block3, block4

block2:
    %11 = iconst.i32 -1
    return %11

block3:
    return %5

block4:
    %12 = iconst.i32 1
    %13 = add.nsw %5, %12
    %14 = icmp slt %13, %1
    br_if %14, block6, block2

block5:
    jump block1(%3)

block6:
    jump block1(%13)
}
"#;

/// An edge that is only a `br` is a `br_if`, on the negation of the condition when it is the
/// second edge, and a `br_if` whose edges both do more is an `if` with no `else`.
#[test]
fn an_edge_that_is_only_a_branch_is_a_br_if() {
    let find = body_of(FIND, "find", false);
    assert_eq!(find.matches("i32.eqz\n\tbr_if\t").count(), 2, "{find}");
    assert_eq!(find.matches("\treturn\n\tend_if\n").count(), 1, "{find}");
    assert!(!find.contains("else"), "{find}");
}

/// With optimization, the argument and the parameter of an edge share a local when they are never
/// live at the same time, and the edge copies nothing. In `sum`, the counter and the next counter
/// share one local, and the sum and the next sum share another, so the edge back to the loop
/// copies nothing. Without optimization, each value has a local and the edge copies two of them.
#[test]
fn an_edge_argument_and_its_parameter_share_a_local() {
    let sum = body_of(PROGRAM, "sum", true);
    assert!(sum.contains("\t.local\ti32, i32\n"), "{sum}");
    assert!(sum.contains("\ti32.add\n\tlocal.tee\t3\n"), "{sum}");
    assert!(!sum.contains("\tlocal.get\t3\n\tlocal.set\t3\n"), "{sum}");
    assert_eq!(sum.matches("local.set").count(), 4, "{sum}");
    let sum = body_of(PROGRAM, "sum", false);
    assert!(sum.matches("local.set").count() > 4, "{sum}");
}

/// An edge to a block that is only a `jump` and that copies nothing goes where the `jump` goes,
/// so the exit test of the loop in `sum` is a `br_if` back to the loop. A `br` just before the
/// `end` of the block that it goes to goes, because the code falls through to the same place.
#[test]
fn a_branch_goes_past_a_block_that_only_jumps() {
    let sum = body_of(PROGRAM, "sum", true);
    assert!(sum.contains("\ti32.lt_s\n\tbr_if\t0\n\tbr\t2\n\tend_loop\n"), "{sum}");
    assert!(sum.contains("\tlocal.set\t2\n\tend_block\n"), "{sum}");
    assert!(!sum.contains("\tbr\t0\n\tend_block\n"), "{sum}");
    let sum = body_of(PROGRAM, "sum", false);
    assert!(sum.contains("\tbr\t1\n\tend_if\n"), "{sum}");
}

/// A `br_if` that branches when its condition is false gets a compare with the inverse predicate
/// when the compare stays on the stack, and an `i32.eqz` after the condition when it does not.
/// In `find`, the test before the loop is `0 < n`, and the branch past the loop is `0 >= n`.
#[test]
fn a_branch_on_a_false_compare_inverts_the_compare() {
    let find = body_of(FIND, "find", true);
    assert!(find.contains("\ti32.const\t0\n\tlocal.get\t1\n\ti32.ge_s\n\tbr_if\t0\n"), "{find}");
    assert!(!find.contains("i32.eqz"), "{find}");
    let find = body_of(FIND, "find", false);
    assert!(find.contains("\ti32.lt_s\n\tlocal.set\t3\n\tlocal.get\t3\n\ti32.eqz\n"), "{find}");
}

const SIGNED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @widen(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = load.i8 %0, align 1
    %2 = sext.i32 %1
    return %2
}

func @less(ptr, ptr) -> i32, linkage(external) {
block0(%0: ptr, %1: ptr):
    %2 = load.i16 %0, align 2
    %3 = load.i16 %1, align 2
    %4 = icmp slt %2, %3
    %5 = zext.i32 %4
    return %5
}

func @unsigned(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = load.i8 %0, align 1
    %2 = zext.i32 %1
    return %2
}

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = alloca, size 1, align 1
    %3 = iconst.i8 -2
    store %3 -> %2, align 1
    %4 = load.i8 %2, align 1
    %5 = sext.i32 %4
    %6 = iconst.i8 0
    %7 = icmp slt %4, %6
    %8 = zext.i32 %7
    %9 = icmp eq %4, %3
    %10 = zext.i32 %9
    %11 = add %5, %8
    %12 = add %11, %10
    %13 = iconst.i32 10
    %14 = add %12, %13
    return %14
}
"#;

/// A narrow load whose uses read it sign extended more often than zero extended is
/// `i32.load8_s` or `i32.load16_s`, with no `i32.extend8_s` or `i32.extend16_s` after it, as clang
/// writes it. A load that is only zero extended stays unsigned. In `main` the byte -2 is read
/// signed twice and compared for equality once, so the load is signed and the equality masks it
/// to 8 bits before it compares: -2 plus 1 plus 1 plus 10 is 10.
#[test]
fn a_narrow_load_that_is_read_signed_is_a_signed_load() {
    let widen = body_of(SIGNED, "widen", true);
    assert!(widen.contains("i32.load8_s\t0\n"), "{widen}");
    assert!(!widen.contains("extend8_s"), "{widen}");
    let less = body_of(SIGNED, "less", true);
    assert_eq!(less.matches("i32.load16_s\t0\n").count(), 2, "{less}");
    assert!(!less.contains("extend16_s"), "{less}");
    let unsigned = body_of(SIGNED, "unsigned", true);
    assert!(unsigned.contains("i32.load8_u\t0\n"), "{unsigned}");
    let main = body_of(SIGNED, "__main_argc_argv", true);
    assert!(main.contains("i32.load8_s"), "{main}");
    assert!(main.contains("i32.const\t255\n\ti32.and\n"), "{main}");
    let main = body_of(SIGNED, "__main_argc_argv", false);
    assert!(main.contains("i32.load8_u"), "{main}");
    if let Some(status) = link_and_run("signed", SIGNED) {
        assert_eq!(status, 10);
    }
}

const EXTENDED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @below(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = load.i8 %0, align 1
    %2 = iconst.i8 -3
    %3 = icmp slt %1, %2
    %4 = zext.i32 %3
    return %4
}

func @widen() -> i32, linkage(external) {
block0:
    %0 = iconst.i16 -5
    %1 = sext.i32 %0
    return %1
}
"#;

/// A sign extension of a narrow constant is the constant extended when the code is written, so
/// `i32.const 253` and `i32.extend8_s` are `i32.const -3`. This is so in a rule, as in the compare
/// of `below`, and in the code of the other instructions, as in the `sext` of `widen`.
#[test]
fn a_sign_extension_of_a_constant_is_the_extended_constant() {
    for optimize in [false, true] {
        let below = body_of(EXTENDED, "below", optimize);
        assert!(below.contains("i32.const\t-3\n"), "{below}");
        assert!(!below.contains("i32.extend8_s\n\ti32.lt_s"), "{below}");
        let widen = body_of(EXTENDED, "widen", optimize);
        assert!(widen.contains("i32.const\t-5\n"), "{widen}");
        assert!(!widen.contains("extend16_s"), "{widen}");
    }
}

const ZERO: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @first(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = iconst.i32 0
    %2 = inttoptr.ptr %1
    %3 = icmp eq %0, %2
    br_if %3, block1, block2

block1:
    return %1

block2:
    %4 = load %0, align 4
    return %4
}

func @none(i64) -> i32, linkage(external) {
block0(%0: i64):
    %1 = iconst.i64 0
    %2 = icmp eq %0, %1
    %3 = zext.i32 %2
    return %3
}

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = iconst.i32 255
    %3 = add %0, %2
    %4 = trunc.i8 %3
    %5 = iconst.i8 0
    %6 = icmp eq %4, %5
    br_if %6, block1, block2

block1:
    %7 = iconst.i32 20
    return %7

block2:
    %8 = iconst.i32 30
    return %8
}
"#;

/// With optimization, a compare of a value against zero or the null pointer is the value as it
/// is in a branch, and `i32.eqz` or `i64.eqz` when it is a value. The compare of an 8-bit value
/// masks the value to 8 bits first: 255 plus `argc`, which is 1, is 0 in 8 bits.
#[test]
fn a_compare_against_zero_is_the_value_or_an_eqz() {
    let first = body_of(ZERO, "first", true);
    assert!(!first.contains("i32.const\t0\n\ti32.eq"), "{first}");
    assert!(
        first.contains("local.get\t0\n\ti32.eqz\n") || first.contains("local.get\t0\n\tif"),
        "{first}"
    );
    let none = body_of(ZERO, "none", true);
    assert!(none.contains("i64.eqz\n"), "{none}");
    assert!(!none.contains("i64.eq\n"), "{none}");
    if let Some(status) = link_and_run("zero", ZERO) {
        assert_eq!(status, 20);
    }
}

const SPIN: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @spin(i32) -> i32, linkage(external) {
block0(%0: i32):
    br_if %0, block1, block2

block1:
    return %0

block2:
    jump block2
}
"#;

/// A function that gives a value and ends with a `return` has no `unreachable` after it, because
/// the stack after a `return` can be of any type. A function whose code can fall through to its
/// end, as after the loop of `spin`, keeps the `unreachable`.
#[test]
fn only_code_that_falls_through_to_the_end_needs_an_unreachable() {
    for optimize in [false, true] {
        let spin = body_of(SPIN, "spin", optimize);
        assert!(!spin.contains("return\n\tunreachable"), "{spin}");
        assert!(
            spin.contains("end_loop\n\tunreachable\n")
                || spin.contains("end_block\n\tunreachable\n"),
            "{spin}"
        );
    }
}

const FIELDS: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

func @field(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = load.i8 %0, align 1
    %2 = iconst.i8 2
    %3 = lshr %1, %2
    %4 = iconst.i8 3
    %5 = and %3, %4
    %6 = icmp eq %5, %2
    %7 = zext.i32 %6
    %8 = zext.i32 %5
    %9 = add %7, %8
    return %9
}

func @main(i32, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = alloca, size 1, align 1
    %3 = iconst.i8 -10
    store %3 -> %2, align 1
    %4 = load.i8 %2, align 1
    %5 = sext.i32 %4
    %6 = iconst.i8 2
    %7 = lshr %4, %6
    %8 = iconst.i8 3
    %9 = and %7, %8
    %10 = zext.i32 %9
    %11 = iconst.i8 15
    %12 = and %4, %11
    %13 = zext.i32 %12
    %14 = add %5, %10
    %15 = add %14, %13
    %16 = icmp slt %4, %3
    %17 = zext.i32 %16
    %18 = add %15, %17
    %19 = iconst.i32 20
    %20 = add %18, %19
    return %20
}
"#;

/// A narrow `and` with a clean operand, here a constant, is clean, and so is a narrow `lshr`,
/// which zero extends its operand first. So the bit field `(x >> 2) & 3` of `field` is compared
/// and widened with no `i32.const 255` and `i32.and`. In `main` the byte -10 is loaded signed,
/// because two of its uses read it sign extended and one reads it zero extended. Then `-10 & 15`
/// is still 6, and `-10 >> 2` masks the byte before it shifts, so it is 61 and its field is 1:
/// -10 plus 1 plus 6 plus 0 plus 20 is 17.
#[test]
fn a_masked_bit_field_needs_no_other_mask() {
    let field = body_of(FIELDS, "field", true);
    assert!(!field.contains("i32.const\t255\n"), "{field}");
    let main = body_of(FIELDS, "__main_argc_argv", true);
    assert!(main.contains("i32.load8_s"), "{main}");
    if let Some(status) = link_and_run("fields", FIELDS) {
        assert_eq!(status, 17);
    }
}

/// Four constants: two string literals, a literal with a zero inside it, a literal aligned to
/// four bytes, and a constant that is not a literal.
const STRINGS: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"

global @.str : bytes 3 = { bytes "hi\00" }, align 1, linkage(internal), constant, literal
global @.str.1 : bytes 3 = { bytes "hi\00" }, align 1, linkage(internal), constant, literal
global @.str.2 : bytes 4 = { bytes "a\00b\00" }, align 1, linkage(internal), constant, literal
global @.str.3 : bytes 3 = { bytes "hi\00" }, align 4, linkage(internal), constant, literal
global @name : bytes 3 = { bytes "hi\00" }, align 1, linkage(external), constant

func @pick(i32) -> ptr, linkage(external) {
block0(%0: i32):
    %1 = global_addr @.str
    %2 = global_addr @.str.1
    %3 = global_addr @.str.2
    %4 = global_addr @.str.3
    %5 = global_addr @name
    %6 = select %0, %1, %2
    %7 = select %0, %3, %4
    %8 = select %0, %6, %7
    %9 = select %0, %8, %5
    return %9
}
"#;

/// A string literal that is one string and is aligned to one byte has the `STRINGS` flag, so
/// wasm-ld keeps one copy of each string. A literal with a zero inside it, a literal aligned to
/// more than one byte, and a constant that is not a literal do not have the flag.
#[test]
fn a_string_literal_is_a_segment_that_wasm_ld_merges() {
    let text = assembly(STRINGS);
    for (name, flags) in
        [(".str", "S"), (".str.1", "S"), (".str.2", ""), (".str.3", ""), ("name", "")]
    {
        let line = format!(".section\t.rodata.{name},\"{flags}\",@\n");
        assert!(text.contains(&line), "{line}{text}");
    }
}
