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

/// The `-S` text of the object for `text`.
fn assembly(text: &str) -> String {
    let mut names = Interner::new();
    let module = rucc_ir::parse(text, &mut names).expect("the IR parses");
    let object = rucc_wasm::translate(&module, &names, Cpu::Lime1.features()).unwrap();
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
        std::fs::write(&listing, assembly(text)).unwrap();
        let args: Vec<&std::ffi::OsStr> = vec![
            "--target=wasm32-wasip1".as_ref(),
            "-mcpu=lime1".as_ref(),
            "-c".as_ref(),
            listing.as_os_str(),
            "-o".as_ref(),
            object_file.as_os_str(),
        ];
        run(&sdk.join("bin/clang"), &args);
    } else {
        std::fs::write(&object_file, object(text).unwrap().bytes).unwrap();
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
    for (name, text) in [("program", PROGRAM), ("twisted", TWISTED), ("threaded", THREADED)] {
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
    ] {
        if let Some(got) = link_and_run_as(&format!("{name}-s"), text, true) {
            assert_eq!(got, status, "{name}");
        }
    }
}
