//! What `__attribute__((section(...)))` reaches the listing and the object file as, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! The unit tests underneath cover one step each: `rucc-sema` reads the attribute, `rucc-asm`
//! decides what the section holds and writes its directive, and `rucc-object` writes the section
//! header. What is left is the trip from the source to the file, which is only visible from the
//! outside, so this runs the compiler over C and reads what it writes. The case is the one in
//! tamnd/rucc#909, checked against what gcc writes for it on x86-64 Linux: a variable, a function
//! and a table entry that nothing in the file refers to, each in a section of its own.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-section-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    dir
}

/// What the compiler wrote for that source with those flags, taking it as text.
fn run(dir: &Path, target: &str, flags: &[&str], out: &str) -> Vec<u8> {
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(flags)
        .arg("-o")
        .arg(dir.join(out))
        .arg(dir.join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    std::fs::read(dir.join(out)).expect("the output was written")
}

/// The listing for that source on that target.
fn listing(what: &str, target: &str, source: &str) -> String {
    let dir = fixture(what, source);
    let text = run(&dir, target, &["-S"], "one.s");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(text).expect("a listing is text")
}

/// The object for that source at that level of optimization, for x86-64 Linux.
fn object(what: &str, level: &str, source: &str) -> Vec<u8> {
    let dir = fixture(what, source);
    let bytes = run(&dir, LINUX, &["-c", level], "one.o");
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

const LINUX: &str = "x86_64-unknown-linux-gnu";

/// The case from the issue with the three details it asks about beside it: zeros that have to be
/// bytes, a constant with nothing in it the loader writes, and a page of zeros in a section whose
/// name says it carries none.
const PLACED: &str = "\
__attribute__((section(\".mine\"))) int g = 7;
__attribute__((section(\".init.text\"))) void f(void) {}
__attribute__((section(\".initcall\"), used)) static void (*const p)(void) = f;
__attribute__((section(\".mine\"))) int z;
__attribute__((section(\".mine\"))) int y = 0;
__attribute__((section(\".roz\"))) const int k = 3;
__attribute__((section(\".bss..page_aligned\"), aligned(4096))) char page[4096];
void after(void) {}
int reads(void) { static int s __attribute__((section(\".mine\"))) = 5; return s + g + k; }
";

/// Two bytes at that offset, as a number.
fn two(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes(bytes[at..at + 2].try_into().expect("two bytes")))
}

/// Four bytes at that offset, as a number.
fn four(bytes: &[u8], at: usize) -> usize {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes")) as usize
}

/// Eight bytes at that offset, as a number.
fn eight(bytes: &[u8], at: usize) -> usize {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes")) as usize
}

/// One section header of an ELF file, as much of it as this looks at.
#[derive(Debug)]
struct Header {
    name: String,
    kind: usize,
    flags: usize,
    offset: usize,
    size: usize,
}

/// Every section header of an ELF file, in the order they are in it.
///
/// Written out rather than pulled in, for the reason `debug_locals.rs` gives: this crate depends on
/// the driver and on nothing else, and a test is a poor reason to change that.
fn headers(object: &[u8]) -> Vec<Header> {
    let start = eight(object, 0x28);
    let (size, count, names) = (two(object, 0x3a), two(object, 0x3c), two(object, 0x3e));
    let at = |which: usize| start + which * size;
    let strings = eight(object, at(names) + 24);
    (0..count)
        .map(|which| {
            let header = at(which);
            let name = strings + four(object, header);
            let end = object[name..].iter().position(|&byte| byte == 0).expect("a name ends");
            Header {
                name: String::from_utf8_lossy(&object[name..name + end]).into_owned(),
                kind: four(object, header + 4),
                flags: eight(object, header + 8),
                offset: eight(object, header + 24),
                size: eight(object, header + 32),
            }
        })
        .collect()
}

const SHT_PROGBITS: usize = 1;
const SHT_RELA: usize = 4;
const SHT_NOBITS: usize = 8;
const SHF_WRITE: usize = 1;
const SHF_ALLOC: usize = 2;
const SHF_EXECINSTR: usize = 4;

#[test]
fn an_elf_listing_puts_each_definition_in_the_section_it_named_with_gccs_flags() {
    let text = listing("elf-s", LINUX, PLACED);
    for line in [
        "\t.section\t.init.text,\"ax\",@progbits\n",
        "\t.section\t.mine,\"aw\",@progbits\n",
        "\t.section\t.initcall,\"aw\",@progbits\n",
        "\t.section\t.roz,\"a\",@progbits\n",
        "\t.section\t.bss..page_aligned,\"aw\",@nobits\n",
    ] {
        assert!(text.contains(line), "{line:?} in\n{text}");
    }
    // The function after the one in a section of its own goes back to where code goes.
    let placed = text.find("\nf:").expect("f is defined");
    let after = text.find("\nafter:").expect("after is defined");
    assert!(text[placed..after].contains("\t.text\n"), "{text}");
    // The zeros in `.mine` are bytes, and the variables in it are in the order they were written.
    let order: Vec<usize> = ["\ng:", "\nz:", "\ny:", "\ns.0:"]
        .iter()
        .map(|label| text.find(label).unwrap_or_else(|| panic!("{label} in\n{text}")))
        .collect();
    assert!(order.is_sorted(), "{text}");
    assert!(!text.contains(".comm"), "{text}");
    assert!(!text.contains("\t.bss\n"), "{text}");
}

#[test]
fn an_elf_object_has_the_sections_gcc_writes_for_the_same_source() {
    for level in ["-O0", "-O2"] {
        let bytes = object(&format!("elf-c{level}"), level, PLACED);
        let headers = headers(&bytes);
        let find = |name: &str| {
            headers
                .iter()
                .find(|header| header.name == name)
                .unwrap_or_else(|| panic!("{name} at {level}: {headers:?}"))
        };
        let code = find(".init.text");
        assert_eq!((code.kind, code.flags), (SHT_PROGBITS, SHF_ALLOC | SHF_EXECINSTR), "{level}");
        let mine = find(".mine");
        assert_eq!((mine.kind, mine.flags), (SHT_PROGBITS, SHF_ALLOC | SHF_WRITE), "{level}");
        // g, z, y and the static inside `reads`, with the zeros written out. In that order at
        // `-O0` and the other way round when optimizing, which is how gcc lays them out.
        let contents = &bytes[mine.offset..mine.offset + mine.size];
        let expected = match level {
            "-O0" => [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0],
            _ => [5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0],
        };
        assert_eq!(contents, expected, "{level}");
        let table = find(".initcall");
        assert_eq!((table.kind, table.flags), (SHT_PROGBITS, SHF_ALLOC | SHF_WRITE), "{level}");
        assert_eq!(table.size, 8, "{level}");
        assert_eq!(find(".rela.initcall").kind, SHT_RELA, "{level}");
        let fixed = find(".roz");
        assert_eq!((fixed.kind, fixed.flags), (SHT_PROGBITS, SHF_ALLOC), "{level}");
        let page = find(".bss..page_aligned");
        assert_eq!((page.kind, page.flags), (SHT_NOBITS, SHF_ALLOC | SHF_WRITE), "{level}");
        assert_eq!(page.size, 4096, "{level}");
    }
}

/// A Mach-O section is a segment and a section, so a name written for ELF is given the segment its
/// contents belong in and the underscores every Mach-O section name starts with.
#[test]
fn a_darwin_listing_maps_an_elf_name_to_a_segment_and_a_section() {
    let text = listing("darwin", "arm64-apple-darwin", PLACED);
    for line in [
        "\t.section\t__TEXT,__init_text,regular,pure_instructions\n",
        "\t.section\t__DATA,__mine\n",
        "\t.section\t__DATA,__initcall\n",
    ] {
        assert!(text.contains(line), "{line:?} in\n{text}");
    }
    let spoken = listing(
        "darwin-own",
        "arm64-apple-darwin",
        "__attribute__((section(\"__DATA,__own\"))) int x = 1;\n",
    );
    assert!(spoken.contains("\t.section\t__DATA,__own\n"), "{spoken}");
}

#[test]
fn a_windows_listing_names_the_section_with_coff_flags() {
    let text = listing("coff", "x86_64-pc-windows-gnu", PLACED);
    for line in [
        "\t.section\t.init.text,\"xr\"\n",
        "\t.section\t.mine,\"dw\"\n",
        "\t.section\t.roz,\"dr\"\n",
    ] {
        assert!(text.contains(line), "{line:?} in\n{text}");
    }
}

#[test]
fn a_section_written_beside_the_star_is_the_declarations() {
    // The kernel's `__ADDRESSABLE`, with the attributes between the star and the name. gcc puts
    // the object in the section, and modpost refuses a vmlinux where it lands in `.data`.
    let source = "void f(void);\n\
                  static void * __attribute__((__used__)) \
                  __attribute__((__section__(\".discard.addressable\"))) \
                  keep_f = (void *)(unsigned long)&f;\n";
    for level in ["-O0", "-O2"] {
        let dir = fixture(&format!("starred{level}"), source);
        let text = run(&dir, LINUX, &["-S", level], "one.s");
        let _ = std::fs::remove_dir_all(&dir);
        let text = String::from_utf8(text).expect("a listing is text");
        assert!(
            text.contains("\t.section\t.discard.addressable,\"aw\",@progbits\n"),
            "{level}:\n{text}"
        );
        assert!(text.contains("keep_f:\n"), "`used` keeps it at {level}:\n{text}");
    }
}

#[test]
fn a_function_returning_a_pointer_may_have_its_section_beside_the_star() {
    // `void * __init memblock_alloc_try_nid(...)`, on a definition and on a declaration that a
    // definition without the attribute follows.
    let source = "int g;\n\
                  void * __attribute__((__section__(\".init.text\"))) f(void) { return &g; }\n\
                  void * __attribute__((__section__(\".init.text\"))) h(void);\n\
                  void *h(void) { return 0; }\n";
    let text = listing("starred-function", LINUX, source);
    for name in ["f", "h"] {
        let placed = format!(
            "\t.section\t.init.text,\"ax\",@progbits\n\t.p2align\t4, 0x90\n\t.globl\t{name}\n"
        );
        assert!(text.contains(&placed), "{name} in .init.text:\n{text}");
    }
}

#[test]
fn a_weak_written_beside_the_star_makes_the_function_weak() {
    // `struct execmem_info * __weak execmem_arch_setup(void)` in the kernel's `mm/execmem.c`,
    // and `char * __weak __init pcibios_setup(char *str)`, on a definition and on a declaration.
    let source = "int g;\n\
                  int * __attribute__((__weak__)) f(void) { return &g; }\n\
                  char * __attribute__((__weak__)) __attribute__((__section__(\".init.text\"))) \
                  h(char *s) { return s; }\n\
                  int * __attribute__((__weak__)) k(void);\n\
                  int *use(void) { return k(); }\n";
    let text = listing("starred-weak", LINUX, source);
    for name in ["f", "h", "k"] {
        assert!(text.contains(&format!("\t.weak\t{name}\n")), "{name} is weak:\n{text}");
        assert!(!text.contains(&format!("\t.globl\t{name}\n")), "{name} is not global:\n{text}");
    }
}

#[test]
fn an_attribute_after_a_tag_with_no_body_is_the_declarations() {
    // `static enum memblock_flags __init_memblock choose_memblock_flags(void)` and
    // `struct mem_section __ref *sparse_index_alloc(int nid)`. With no brace after the tag the
    // attribute is a specifier of the declaration, and gcc places each of these by it.
    let source = "enum e { A };\nstruct s { int x; };\n\
                  enum e __attribute__((__section__(\".init.text\"))) f(void) { return A; }\n\
                  struct s __attribute__((__section__(\".ref.text\"))) *g(int n) { return 0; }\n\
                  struct s __attribute__((__section__(\".mine\"))) v;\n";
    let text = listing("after-tag", LINUX, source);
    for (section, name) in
        [(".init.text,\"ax\"", "f"), (".ref.text,\"ax\"", "g"), (".mine,\"aw\"", "v")]
    {
        let placed = format!("\t.section\t{section},@progbits\n");
        let at = text.find(&placed).unwrap_or_else(|| panic!("{section} in\n{text}"));
        let label = text.find(&format!("\n{name}:\n")).expect("the label is written");
        assert!(at < label, "{name} after {section}:\n{text}");
    }
}

/// What `__attribute__((retain))` asks for, which is a section of its own with `SHF_GNU_RETAIN` on
/// it so that `--gc-sections` keeps it. Checked against what gcc 16 writes for the same source with
/// `-fPIC`: the section a definition would have gone in under `-ffunction-sections` or
/// `-fdata-sections`, with an `R` in the flags, or the program's own section with one.
const RETAINED: &str = "\
__attribute__((retain)) int a = 1;
__attribute__((retain)) int e;
__attribute__((retain)) const int d = 4;
__attribute__((retain)) static const char *const p = \"x\";
__attribute__((retain)) __thread int t1 = 1;
__attribute__((retain)) __thread int t2;
__attribute__((retain, section(\".mine\"))) int m = 3;
__attribute__((retain)) static int f(void) { return 1; }
__attribute__((retain, section(\".kept.text\"))) int g(void) { return 2; }
int plain = 5;
int h(void) { return plain; }
";

/// `SHF_GNU_RETAIN`, which is in the range ELF leaves to the operating system.
const SHF_GNU_RETAIN: usize = 0x20_0000;
const SHF_TLS: usize = 0x400;

#[test]
fn a_retained_definition_is_in_a_section_of_its_own_marked_r() {
    let dir = fixture("retain-s", RETAINED);
    let text = run(&dir, LINUX, &["-S", "-fPIC"], "one.s");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8(text).expect("a listing is text");
    for line in [
        "\t.section\t.data.a,\"awR\"\n",
        "\t.section\t.bss.e,\"awR\",@nobits\n",
        "\t.section\t.rodata.d,\"aR\"\n",
        "\t.section\t.data.rel.ro.local.p,\"awR\"\n",
        "\t.section\t.tdata.t1,\"awTR\",@progbits\n",
        "\t.section\t.tbss.t2,\"awTR\",@nobits\n",
        "\t.section\t.mine,\"awR\",@progbits\n",
        "\t.section\t.text.f,\"axR\",@progbits\n",
        "\t.section\t.kept.text,\"axR\",@progbits\n",
    ] {
        assert!(text.contains(line), "{line:?} in\n{text}");
    }
    // What was not asked to be kept is where it always goes.
    assert!(!text.contains(".data.plain"), "{text}");
    assert!(!text.contains(".text.h"), "{text}");
    assert!(!text.contains(".comm"), "{text}");
}

#[test]
fn a_retained_section_has_the_flag_in_the_object() {
    for level in ["-O0", "-O2"] {
        let bytes = object(&format!("retain-c{level}"), level, RETAINED);
        let headers = headers(&bytes);
        let find = |name: &str| {
            headers
                .iter()
                .find(|header| header.name == name)
                .unwrap_or_else(|| panic!("{name} at {level}: {headers:?}"))
        };
        let kept = SHF_GNU_RETAIN | SHF_ALLOC;
        for (name, kind, flags) in [
            (".data.a", SHT_PROGBITS, kept | SHF_WRITE),
            (".bss.e", SHT_NOBITS, kept | SHF_WRITE),
            (".rodata.d", SHT_PROGBITS, kept),
            (".tdata.t1", SHT_PROGBITS, kept | SHF_WRITE | SHF_TLS),
            (".tbss.t2", SHT_NOBITS, kept | SHF_WRITE | SHF_TLS),
            (".mine", SHT_PROGBITS, kept | SHF_WRITE),
            (".text.f", SHT_PROGBITS, kept | SHF_EXECINSTR),
            (".kept.text", SHT_PROGBITS, kept | SHF_EXECINSTR),
        ] {
            let header = find(name);
            assert_eq!((header.kind, header.flags), (kind, flags), "{name} at {level}");
        }
        assert_eq!(find(".data").flags & SHF_GNU_RETAIN, 0, "{level}");
        assert_eq!(find(".text").flags & SHF_GNU_RETAIN, 0, "{level}");
    }
}

/// The text from the last section directive in front of a label up to the label, which is where
/// the listing says what section the symbol is in and what is done to it there.
fn leading<'a>(text: &'a str, label: &str) -> &'a str {
    let at = text.find(&format!("\n{label}:\n")).unwrap_or_else(|| panic!("{label} in\n{text}"));
    let start = text[..at].rfind("\t.section").unwrap_or_else(|| panic!("{label} in\n{text}"));
    &text[start..at]
}

/// `copy(name)` takes what gcc 16 takes from the declaration it names, which is the section, the
/// alignment and `used`, and leaves what gcc leaves, which is whatever is about the name rather
/// than the thing: `weak` and `visibility` here. Checked against the listing gcc 16 writes for the
/// same source. The last two lines are the shape the kernel's `module_init` makes.
#[test]
fn a_copy_takes_the_section_alignment_and_used_and_not_the_linkage() {
    let text = listing(
        "copy",
        LINUX,
        "\
__attribute__((section(\".s1\"), aligned(32), cold, weak, visibility(\"hidden\"))) void a(void) {}
__attribute__((copy(a))) void b(void) {}
__attribute__((section(\".s2\"), aligned(64), used)) int x = 1;
__attribute__((copy(x))) int y = 2;
__attribute__((copy(x))) static int z = 3;
__attribute__((section(\".init.text\"))) static int initfn(void) { return 0; }
int init_module(void) __attribute__((copy(initfn), alias(\"initfn\")));
",
    );
    let b = leading(&text, "b");
    assert!(b.starts_with("\t.section\t.s1,"), "{b}");
    assert!(b.contains("\t.p2align\t5"), "{b}");
    let y = leading(&text, "y");
    assert!(y.starts_with("\t.section\t.s2,"), "{y}");
    assert!(y.contains("\t.p2align\t6"), "{y}");
    // A static nothing refers to is dropped unless something keeps it, and the `used` it took
    // from `x` is what does.
    let z = leading(&text, "z");
    assert!(z.starts_with("\t.section\t.s2,"), "{z}");
    assert!(!text.contains(".weak\tb"), "{text}");
    assert!(!text.contains(".hidden\tb"), "{text}");
    assert!(text.contains("init_module"), "{text}");
}

/// The section directive the definition of that label sits under in a listing.
fn under<'a>(text: &'a str, label: &str) -> &'a str {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|line| *line == format!("{label}:"))
        .unwrap_or_else(|| panic!("{label} is defined in\n{text}"));
    lines[..at]
        .iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| line.starts_with(".section") || *line == ".data" || *line == ".bss")
        .unwrap_or_else(|| panic!("{label} is under a section in\n{text}"))
}

/// Where gcc 14 puts each of these at `-O2 -fno-pie` on x86-64 Linux. A constant of nothing but
/// zeros is read only data and not zeroed space, and so is a `static` that nothing writes and
/// whose address only reaches loads. One that is written, or whose address goes to a call, stays
/// in `.data`, and at `-O0` nothing is promoted.
#[test]
fn a_constant_of_zeros_and_a_static_nothing_writes_are_read_only_data() {
    let source = "\
struct ops { int (*f)(void); int g; };
const struct ops empty_ops;
static const char *names[] = { \"a\", \"b\" };
static int counter = 5;
static int escaped[2] = { 1, 2 };
void take(int *);
const char *name(int i) { return names[i & 1]; }
int bump(void) { take(escaped); return counter++; }
";
    let dir = fixture("readonly", source);
    let optimized = String::from_utf8(run(&dir, LINUX, &["-S", "-O2", "-fno-pie"], "one.s"))
        .expect("a listing is text");
    let plain = String::from_utf8(run(&dir, LINUX, &["-S", "-O0", "-fno-pie"], "zero.s"))
        .expect("a listing is text");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(under(&optimized, "empty_ops"), ".section\t.rodata");
    assert_eq!(under(&optimized, "names"), ".section\t.rodata");
    assert_eq!(under(&optimized, "counter"), ".data");
    assert_eq!(under(&optimized, "escaped"), ".data");
    assert_eq!(under(&plain, "empty_ops"), ".section\t.rodata");
    assert_eq!(under(&plain, "names"), ".data");
}

/// A constant holding an address in a section the program named, which is how the kernel writes
/// `module_param` and its `__initconst` tables. Under `-fno-pic` the static linker is the only one
/// that writes the address, so gcc and clang leave the section read only, and only position
/// independent code needs it writable.
#[test]
fn a_named_constant_holding_an_address_is_read_only_without_pic() {
    let source = "\
struct kp { const char *name; int (*fn)(void); int perm; };
static int f(void) { return 1; }
static const struct kp p1 __attribute__((used, section(\"__param\"))) = { \"x\", f, 0644 };
";
    let dir = fixture("param", source);
    for target in [LINUX, "i686-unknown-linux-gnu"] {
        let absolute = String::from_utf8(run(&dir, target, &["-S", "-O2", "-fno-pic"], "one.s"))
            .expect("a listing is text");
        let moved = String::from_utf8(run(&dir, target, &["-S", "-O2", "-fpic"], "pic.s"))
            .expect("a listing is text");
        assert!(absolute.contains(".section\t__param,\"a\",@progbits"), "{target}: {absolute}");
        assert!(moved.contains(".section\t__param,\"aw\",@progbits"), "{target}: {moved}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
