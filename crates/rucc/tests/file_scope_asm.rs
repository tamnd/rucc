//! What an `asm` at file scope made of directives puts in the object, end to end.
//!
//! On ELF every such block goes to the assembler with the rest of the listing. The compiler used to
//! read some blocks into globals itself, and three blocks every BSD source file writes came out
//! wrong from the reading, with no error. NetBSD's `__RCSID` lost the flags of `.ident`, the note
//! its start files brand a program with lost `SHT_NOTE`, an `.ident` lost its string, and FreeBSD's
//! `__weak_reference` made a local symbol of a weak one. Each is checked here against what gcc and
//! GNU as write for the same file on x86-64 Linux. See tamnd/rucc#4090.
//!
//! `-frucc-asm-reader=off` is still taken and changes nothing. A file gives the same object with it
//! and without it.

use std::path::PathBuf;
use std::process::Command;

/// The object for that source, compiled for x86-64 Linux with the extra flags.
fn object(what: &str, source: &str, flags: &[&str]) -> Vec<u8> {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rucc-file-asm-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-c"])
        .args(flags)
        .arg("-o")
        .arg(dir.join("one.o"))
        .arg(dir.join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    let bytes = std::fs::read(dir.join("one.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

fn word(bytes: &[u8], at: usize, width: usize) -> usize {
    bytes[at..at + width].iter().rev().fold(0, |sum, &byte| sum << 8 | usize::from(byte))
}

/// The string at that offset of a string table.
fn string(bytes: &[u8], at: usize) -> String {
    let end = bytes[at..].iter().position(|&byte| byte == 0).expect("a name ends");
    String::from_utf8_lossy(&bytes[at..at + end]).into_owned()
}

/// One section header, as much of it as this looks at.
#[derive(Debug)]
struct Header {
    name: String,
    kind: usize,
    flags: usize,
    offset: usize,
    size: usize,
    link: usize,
    entsize: usize,
}

/// Every section header, in the order they are in the file.
fn headers(object: &[u8]) -> Vec<Header> {
    let start = word(object, 0x28, 8);
    let (size, count, names) =
        (word(object, 0x3a, 2), word(object, 0x3c, 2), word(object, 0x3e, 2));
    let at = |which: usize| start + which * size;
    let strings = word(object, at(names) + 24, 8);
    (0..count)
        .map(|which| {
            let header = at(which);
            Header {
                name: string(object, strings + word(object, header, 4)),
                kind: word(object, header + 4, 4),
                flags: word(object, header + 8, 8),
                offset: word(object, header + 24, 8),
                size: word(object, header + 32, 8),
                link: word(object, header + 40, 4),
                entsize: word(object, header + 56, 8),
            }
        })
        .collect()
}

/// One symbol, as much of it as this looks at.
#[derive(Debug, PartialEq, Eq)]
struct Symbol {
    name: String,
    bind: usize,
    kind: usize,
    visibility: usize,
    size: usize,
}

/// Every symbol of the object, with a name.
fn symbols(object: &[u8]) -> Vec<Symbol> {
    let headers = headers(object);
    let table = headers.iter().find(|h| h.kind == SHT_SYMTAB).expect("a symbol table");
    let strings = headers[table.link].offset;
    (0..table.size / 24)
        .map(|which| table.offset + which * 24)
        .map(|at| Symbol {
            name: string(object, strings + word(object, at, 4)),
            bind: usize::from(object[at + 4] >> 4),
            kind: usize::from(object[at + 4] & 0xf),
            visibility: usize::from(object[at + 5] & 3),
            size: word(object, at + 16, 8),
        })
        .filter(|symbol| !symbol.name.is_empty())
        .collect()
}

fn section<'a>(headers: &'a [Header], name: &str) -> &'a Header {
    headers.iter().find(|h| h.name == name).unwrap_or_else(|| panic!("{name} in {headers:#?}"))
}

fn symbol<'a>(symbols: &'a [Symbol], name: &str) -> &'a Symbol {
    symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("{name} in {symbols:#?}"))
}

const SHT_PROGBITS: usize = 1;
const SHT_SYMTAB: usize = 2;
const SHT_NOTE: usize = 7;
const SHF_WRITE: usize = 1;
const SHF_ALLOC: usize = 2;
const SHF_MERGE: usize = 0x10;
const SHF_STRINGS: usize = 0x20;
const STB_LOCAL: usize = 0;
const STB_GLOBAL: usize = 1;
const STB_WEAK: usize = 2;
const STT_NOTYPE: usize = 0;
const STT_FUNC: usize = 2;
const STV_HIDDEN: usize = 2;

/// NetBSD's `__RCSID` through `__SECTIONSTRING`, and the note its start files brand a program
/// with. gcc and GNU as give `.ident` as `PROGBITS` with `MS` and an entry size of one, and the
/// note as `NOTE` with `A`. The reading gave both as writable `PROGBITS`.
const NETBSD: &str = "\
__asm(\".pushsection .ident,\\\"MS\\\",@progbits,1\\n.asciz \\\"$NetBSD: x.c,v 1.1 $\\\"\\n.popsection\");
__asm(\".pushsection .note.netbsd.ident,\\\"a\\\",@note\\n.long 7,4,1\\n.ascii \\\"NetBSD\\\\0\\\\0\\\"\\n.long 1000000000\\n.popsection\");
int x;
";

#[test]
fn a_pushed_section_keeps_its_flags_and_its_type() {
    let bytes = object("netbsd", NETBSD, &[]);
    let headers = headers(&bytes);
    let ident = section(&headers, ".ident");
    assert_eq!(ident.kind, SHT_PROGBITS, "{ident:?}");
    assert_eq!(ident.flags, SHF_MERGE | SHF_STRINGS, "{ident:?}");
    assert_eq!(ident.entsize, 1, "{ident:?}");
    let note = section(&headers, ".note.netbsd.ident");
    assert_eq!((note.kind, note.flags), (SHT_NOTE, SHF_ALLOC), "{note:?}");
    assert_eq!(note.size, 24, "{note:?}");
    // No symbol is made for the bytes, which no label named.
    let symbols = symbols(&bytes);
    assert!(symbols.iter().all(|s| s.name == "x" || s.kind == 3 || s.kind == 4), "{symbols:#?}");
}

#[test]
fn an_ident_goes_into_the_comment_section_ahead_of_the_compiler() {
    let bytes = object("ident", "__asm__(\".ident \\\"$FreeBSD: x $\\\"\");\nint x;\n", &[]);
    let headers = headers(&bytes);
    let comment = section(&headers, ".comment");
    let text = &bytes[comment.offset..comment.offset + comment.size];
    assert!(text.starts_with(b"\0$FreeBSD: x $\0"), "{:?}", String::from_utf8_lossy(text));
    assert_eq!(comment.flags & SHF_WRITE, 0, "{comment:?}");
}

/// FreeBSD's `__weak_reference(real, alias)`, which is `.weak` in one block and `.equ` in the
/// next, and a `.weak` and a `.globl` of names the C code defines. gas puts what a directive says
/// on the symbol whoever defines it.
const WEAK: &str = "\
__asm__(\".weak alias\");
int real(void) { return 1; }
__asm__(\".equ alias, real\");
__asm__(\".weak early\");
int early(void) { return 2; }
static int quiet(void) { return 3; }
__asm__(\".globl quiet\");
int (*keep)(void) = quiet;
int h;
__asm__(\".hidden h\");
";

#[test]
fn a_binding_in_one_block_reaches_a_name_defined_anywhere_in_the_file() {
    let bytes = object("weak", WEAK, &[]);
    let symbols = symbols(&bytes);
    let alias = symbol(&symbols, "alias");
    assert_eq!((alias.bind, alias.kind), (STB_WEAK, STT_FUNC), "{alias:?}");
    assert_eq!(symbol(&symbols, "real").bind, STB_GLOBAL);
    assert_eq!(symbol(&symbols, "early").bind, STB_WEAK);
    assert_eq!(symbol(&symbols, "quiet").bind, STB_GLOBAL);
    let h = symbol(&symbols, "h");
    assert_eq!((h.bind, h.visibility), (STB_GLOBAL, STV_HIDDEN), "{h:?}");
    assert!(symbols.iter().all(|s| s.name != "alias" || s.bind != STB_LOCAL));
}

/// A data block with one label, which the reading used to count as exact.
const DATA: &str = "__asm__(\".data\\n.globl table\\ntable:\\n.long 1, 2\\n\");\nint x;\n";

#[test]
fn the_object_is_the_same_with_the_reader_off() {
    for (what, source) in [("netbsd", NETBSD), ("weak", WEAK), ("data", DATA)] {
        let read = object(&format!("{what}-on"), source, &[]);
        let off = object(&format!("{what}-off"), source, &["-frucc-asm-reader=off"]);
        assert!(read == off, "{what}");
    }
}

/// A label in a block goes to the assembler, which gives it no type and no size, the way GNU as
/// does. The reading the compiler used to make of a block like this one gave `table` the type
/// `STT_OBJECT` and the size 8, which gcc does not.
#[test]
fn a_label_in_a_block_gets_no_type_and_no_size() {
    for flags in [&[][..], &["-frucc-asm-reader=off"][..]] {
        let bytes = object(&format!("data-{}", flags.len()), DATA, flags);
        let table = symbols(&bytes).into_iter().find(|s| s.name == "table").expect("table");
        assert_eq!((table.bind, table.kind, table.size), (STB_GLOBAL, STT_NOTYPE, 0), "{table:?}");
    }
}

#[test]
fn the_reader_switch_takes_on_or_off_and_nothing_else() {
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-frucc-asm-reader=maybe", "-c", "-"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(!done.status.success());
    let said = String::from_utf8_lossy(&done.stderr);
    assert!(said.contains("-frucc-asm-reader="), "{said}");
}

/// FreeBSD's `__sym_default` with `__weak_reference`, as `setcontext.c` writes them. The `@@@`
/// renames `s` to `s@@V2`, so the debug information cannot find the function by its own name,
/// and `-g` stopped with "the listing has no label". gcc writes `s@@V2` and the weak `__i`.
const RENAMED: &str = "\
__asm__(\".weak __i\");
__asm__(\".equ __i, s\");
__asm__(\".symver s, s@@@V2\");
int g(int);
int s(int x) { return g(x); }
";

#[test]
fn a_function_renamed_by_symver_still_gets_its_debug_information() {
    let bytes = object("renamed", RENAMED, &["-g"]);
    let symbols = symbols(&bytes);
    let s = symbol(&symbols, "s@@V2");
    assert_eq!((s.bind, s.kind), (STB_GLOBAL, STT_FUNC), "{s:?}");
    assert_eq!(symbol(&symbols, "__i").bind, STB_WEAK);
    assert!(symbols.iter().all(|s| s.name != "s"), "{symbols:#?}");
    let headers = headers(&bytes);
    assert!(section(&headers, ".debug_info").size > 0);
    assert!(section(&headers, ".debug_line").size > 0);
}
