//! What `-mcmodel=kernel` reaches the object file as, end to end.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3 and `spec/04-driver-and-cli.md` section 4.3.
//!
//! The kernel model says the code and static data are in the top 2 GiB of the address space, so a
//! name's address is a 32 bit number the machine sign extends. gcc writes it that way for the kernel,
//! `movq $sym, %rax` and `sym(,%rdi,8)` with `R_X86_64_32S`, and the kernel's link script asserts
//! that there is no `.got` and no `.plt`. This is the check tamnd/rucc#2275 asks for, and the one the
//! corpus facet `mcmodel-kernel` makes against a real kernel: nothing in a kernel model object reads
//! the global offset table, goes through a PLT stub or asks for thread-local storage.
//!
//! The object is read here rather than the listing, because the relocation is the whole of what is
//! being asked about and the listing only implies it. The top crate has no ELF reader to depend on,
//! and the part of the format read here is small enough to read by hand.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because the kernel model is x86-64
/// ELF's and the answer anywhere else is a refusal.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Every way kernel code reaches a name: a variable defined here and one defined elsewhere, a weak
/// one nothing may define, a static array indexed, a function called and one whose address is
/// taken, a weak function tested and called, and a table of addresses in data.
const KERNEL: &str = "\
int here;
extern int away;
extern int maybe __attribute__((weak));
static long tab[16];
extern void act(void);
extern void perhaps(void) __attribute__((weak));
int read_both(void) { return here + away; }
int *address_of_away(void) { return &away; }
int read_maybe(void) { return maybe; }
long index_tab(long i) { return tab[i]; }
void (*address_of_act(void))(void) { return act; }
void call_perhaps(void) { if (perhaps) perhaps(); act(); }
void *table[] = { &here, act };
";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-mcmodel-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler writes for that source under those flags, and whether it took them.
fn run(what: &str, flags: &[&str], source: &str) -> (bool, Vec<u8>, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The listing, for a compile that has to succeed.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let mut line = vec!["-S"];
    line.extend_from_slice(flags);
    let (ok, out, err) = run(what, &line, source);
    assert!(ok, "the compiler refused the fixture:\n{err}");
    String::from_utf8(out).expect("a listing is text")
}

/// The type of every relocation in an ELF64 little endian object, from each `SHT_RELA` section.
fn relocation_types(object: &[u8]) -> Vec<u32> {
    let u16_at = |at: usize| u16::from_le_bytes([object[at], object[at + 1]]) as usize;
    let u32_at = |at: usize| u32::from_le_bytes(object[at..at + 4].try_into().expect("four bytes"));
    let u64_at = |at: usize| {
        usize::try_from(u64::from_le_bytes(object[at..at + 8].try_into().expect("eight bytes")))
            .expect("an offset that fits")
    };
    assert_eq!(&object[..4], b"\x7fELF", "not an ELF object");
    assert_eq!(object[4], 2, "not ELF64");
    let (table, size, count) = (u64_at(0x28), u16_at(0x3A), u16_at(0x3C));
    let mut types = Vec::new();
    for section in 0..count {
        let header = table + section * size;
        // `SHT_RELA`, and each entry is an offset, an info word whose low half is the type, and
        // an addend.
        if u32_at(header + 4) != 4 {
            continue;
        }
        let (offset, bytes, entry) =
            (u64_at(header + 0x18), u64_at(header + 0x20), u64_at(header + 0x38));
        for at in (offset..offset + bytes).step_by(entry) {
            types.push(u32_at(at + 8));
        }
    }
    types
}

/// `R_X86_64_32S`.
const SIGNED: u32 = 11;

/// Every relocation that reads the global offset table, asks for a PLT stub or entry by offset, or
/// is about thread-local storage, by number: `GOT32`, `GOTPCREL`, `DTPMOD64` to `TPOFF32`,
/// `GOTOFF64`, `GOTPC32`, `GOT64`, `GOTPCREL64`, `GOTPC64`, `GOTPLT64`, `PLTOFF64`, the two
/// descriptor forms, `GOTPCRELX` and `REX_GOTPCRELX`. `PLT32` is not one of them: a call asks for it
/// and the linker makes it a direct call when there is nothing to go through, which gcc's kernel
/// objects have as well.
const REFUSED: [u32; 20] =
    [3, 9, 16, 17, 18, 19, 20, 21, 22, 23, 25, 26, 27, 28, 29, 30, 31, 34, 41, 42];

/// `-Wa,-mrelax-relocations=no`, which the kernel's decompressor is built with, turns every read of
/// the global offset table into `R_X86_64_GOTPCREL`, as gas does, where the default is the
/// `REX_GOTPCRELX` the linker may rewrite.
#[test]
fn a_read_of_the_table_is_kept_when_relaxing_is_off() {
    let source = "extern int away; int *take(void) { return &away; }\n";
    for level in ["-O0", "-O2"] {
        let (ok, object, err) = run("relax", &["-c", "-fPIC", level], source);
        assert!(ok, "{level}: {err}");
        assert!(relocation_types(&object).contains(&42), "{level}: no REX_GOTPCRELX");
        let line = ["-c", "-fPIC", level, "-Wa,-mrelax-relocations=no"];
        let (ok, object, err) = run("kept", &line, source);
        assert!(ok, "{level}: {err}");
        let types = relocation_types(&object);
        assert!(types.contains(&9), "{level}: no GOTPCREL in {types:?}");
        assert!(!types.contains(&41) && !types.contains(&42), "{level}: {types:?}");
    }
}

/// A kernel model object reaches every name directly, and the addresses it wants as values are
/// four bytes the machine sign extends.
#[test]
fn a_kernel_model_object_has_nothing_that_needs_a_table_a_stub_or_a_thread() {
    for level in ["-O0", "-O2"] {
        let (ok, object, err) =
            run("object", &["-c", "-mcmodel=kernel", "-fno-PIE", level], KERNEL);
        assert!(ok, "{level}: {err}");
        let types = relocation_types(&object);
        for kind in &types {
            assert!(!REFUSED.contains(kind), "{level}: relocation type {kind} in {types:?}");
        }
        assert!(types.contains(&SIGNED), "{level}: no R_X86_64_32S in {types:?}");
    }
}

/// The listing says the same in the words gcc uses for the kernel.
#[test]
fn a_kernel_model_listing_writes_addresses_the_way_gcc_writes_them_for_a_kernel() {
    let text = asm("listing", &["-mcmodel=kernel", "-fno-pic", "-O2"], KERNEL);
    for word in ["GOTPCREL", "@PLT", "GOTTPOFF", "TPOFF", "TLSGD"] {
        assert!(!text.contains(word), "{word}: {text}");
    }
    // The address as a value is the immediate of a `movq`, a static array indexed is one load, a
    // variable read is still read from the instruction pointer, and a call is by name.
    assert!(text.contains("movq\t$away, %rax"), "{text}");
    assert!(text.contains("movq\t$act, %rax"), "{text}");
    assert!(text.contains("tab(,%rdi,8)"), "{text}");
    assert!(text.contains("maybe(%rip)"), "{text}");
    assert!(text.contains("call\tperhaps"), "{text}");
}

/// The kernel model and a position independent build cannot both be true, and gcc says so in these
/// words, the default included, since the default here is a position independent executable.
#[test]
fn the_kernel_model_is_refused_beside_position_independent_code() {
    for flags in
        [&["-mcmodel=kernel"][..], &["-mcmodel=kernel", "-fPIE"], &["-mcmodel=kernel", "-fPIC"]]
    {
        let mut line = vec!["-S"];
        line.extend_from_slice(flags);
        let (ok, _, err) = run("refused", &line, "int x;\n");
        assert!(!ok, "{flags:?} was taken");
        assert!(err.contains("code model kernel does not support PIC mode"), "{flags:?}: {err}");
    }
}

/// `__code_model_kernel__` rather than `__code_model_small__`, which is what gcc defines and what a
/// header reads to find out which it is under.
#[test]
fn the_macro_says_which_model() {
    let source = "\
#if defined __code_model_kernel__ && !defined __code_model_small__
int kernel(void) { return 1; }
#endif
";
    let text = asm("macro", &["-mcmodel=kernel", "-fno-pie"], source);
    assert!(text.contains("kernel:"), "{text}");
}

/// A `switch` dense enough for a jump table, with twelve arms.
const SWITCH: &str = "\
int a(int); int b(int);
int sw(int x) {
    switch (x) {
    case 0: return a(1); case 1: return b(2); case 2: return a(3); case 3: return b(4);
    case 4: return a(5); case 5: return b(6); case 6: return a(7); case 7: return b(8);
    case 8: return a(9); case 9: return b(10); case 10: return a(11); case 11: return 7;
    }
    return 0;
}
";

/// A jump table under the kernel model is the one objtool reads: loaded by its own address with the
/// index scaled by eight, and each cell the address of an arm in eight bytes. The distances from
/// the table that position independent code uses are a table objtool says it cannot find.
#[test]
fn a_kernel_model_jump_table_holds_addresses_where_objtool_looks() {
    let text = asm("table", &["-mcmodel=kernel", "-fno-pic", "-O2"], SWITCH);
    assert!(text.contains("movq\t.Lsw_j0(,%rax,8), %rax"), "{text}");
    let cells = text.lines().skip_while(|line| *line != ".Lsw_j0:").skip(1);
    let cells: Vec<&str> = cells.take_while(|line| line.starts_with("\t.quad")).collect();
    assert_eq!(cells.len(), 12, "{text}");
    assert!(cells.iter().all(|cell| cell.starts_with("\t.quad\t.Lsw_")), "{cells:#?}");
    assert!(!text.contains("\t.long\t.Lsw_"), "{text}");

    // Without the unwind table, as the kernel builds, whose one record is a distance of its own.
    let line = ["-c", "-mcmodel=kernel", "-fno-pic", "-O2", "-fno-asynchronous-unwind-tables"];
    let (ok, object, err) = run("table-object", &line, SWITCH);
    assert!(ok, "{err}");
    let types = relocation_types(&object);
    // `R_X86_64_64` for each cell, `R_X86_64_32S` for the load, and no distance at all.
    assert_eq!(types.iter().filter(|&&kind| kind == 1).count(), 12, "{types:?}");
    assert!(types.contains(&SIGNED) && !types.contains(&2), "{types:?}");
}

/// The small model keeps the table of distances, which needs nothing from a linker at load time.
#[test]
fn a_small_model_jump_table_is_still_distances() {
    let text = asm("small", &["-fno-pic", "-O2"], SWITCH);
    assert!(text.contains("\t.long\t.Lsw_"), "{text}");
    assert!(!text.contains(".quad"), "{text}");
}

/// The tiny model on AArch64 is taken, as the arm64 kernel's vDSO asks for it, and the code is the
/// small model's, which reaches everything the tiny model does. Only the macro is different, and
/// anywhere else it is refused, as gcc refuses it.
#[test]
fn the_tiny_model_is_aarch64s_and_writes_the_small_models_code() {
    let source = "\
int here;
#if defined __AARCH64_CMODEL_TINY__ && !defined __AARCH64_CMODEL_SMALL__
int tiny(void) { return here; }
#else
int small(void) { return here; }
#endif
";
    let arm = "--target=aarch64-unknown-linux-gnu";
    let tiny = asm("tiny", &[arm, "-mcmodel=tiny", "-O2"], source);
    let small = asm("tiny-small", &[arm, "-O2"], source);
    assert!(tiny.contains("tiny:"), "{tiny}");
    assert!(small.contains("small:"), "{small}");
    assert_eq!(tiny.replace("tiny", "small"), small);
    let (ok, _, err) = run("tiny-x86", &["-mcmodel=tiny", "-S"], "int x;\n");
    assert!(!ok, "-mcmodel=tiny was taken on x86-64");
    assert!(err.contains("has no tiny code model"), "{err}");
}
