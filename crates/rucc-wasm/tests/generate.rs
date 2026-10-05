//! The wasm back end, from IR text to an object.
//!
//! Each test reads a module in the IR's text form, which is what `rucc --emit=ir` prints, and
//! writes the object. The test that runs the object needs `WASI_SDK_PATH`, as the tests of the
//! object writer do, and it says that it did nothing when the variable is not set. When
//! `wasm-tools` is on `PATH`, it also validates the object and the module.

use std::path::{Path, PathBuf};
use std::process::Command;

use rucc_base::Interner;
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
