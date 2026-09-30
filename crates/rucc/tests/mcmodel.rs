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
