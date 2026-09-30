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
        // g, z, y and the static inside `reads`, in that order, with the zeros written out.
        let contents = &bytes[mine.offset..mine.offset + mine.size];
        assert_eq!(contents, [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0], "{level}");
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
