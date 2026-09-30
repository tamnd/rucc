//! Section types, groups and flags an ELF `.section` asks for, end to end.
//!
//! The kernel's `ELFNOTE` wants `@note` to be `SHT_NOTE`, its linker script keeps sections that are
//! declared and empty, `retpoline.S` puts every thunk in a COMDAT group of its own, and
//! `__retain` wants `SHF_GNU_RETAIN`. Each is checked against what llvm-mc and gas write for the
//! same file. See tamnd/rucc#2273.

use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "\
\t.section .note.Linux,\"a\",@note
\t.long 6, 4, 1
\t.section .empty.keep,\"a\"
\t.section .text.__x86_indirect_thunk_rax,\"axG\",@progbits,__x86_indirect_thunk_rax,comdat
\t.globl __x86_indirect_thunk_rax
__x86_indirect_thunk_rax:
\tret
\t.section .data.plain,\"awG\",@progbits,grp
\t.long 1
\t.section .text.kept,\"axR\",@progbits
\tret
\t.section .gnu.linkonce.t.foo,\"ax\"
\t.linkonce discard
\tret
";

/// The object for [`SOURCE`], assembled for x86-64 Linux.
fn object(what: &str) -> Vec<u8> {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rucc-groups-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.s"), SOURCE).expect("the fixture can be written");
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-c", "-o"])
        .arg(dir.join("one.o"))
        .arg(dir.join("one.s"))
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

/// One section header, as much of it as this looks at.
#[derive(Debug)]
struct Header {
    name: String,
    kind: usize,
    flags: usize,
    offset: usize,
    size: usize,
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
            let name = strings + word(object, header, 4);
            let end = object[name..].iter().position(|&byte| byte == 0).expect("a name ends");
            Header {
                name: String::from_utf8_lossy(&object[name..name + end]).into_owned(),
                kind: word(object, header + 4, 4),
                flags: word(object, header + 8, 8),
                offset: word(object, header + 24, 8),
                size: word(object, header + 32, 8),
            }
        })
        .collect()
}

const SHT_NOTE: usize = 7;
const SHT_PROGBITS: usize = 1;
const SHT_GROUP: usize = 17;
const SHF_GROUP: usize = 0x200;
const SHF_GNU_RETAIN: usize = 0x20_0000;
const GRP_COMDAT: usize = 1;

#[test]
fn a_note_is_a_note_and_an_empty_declared_section_is_kept() {
    let bytes = object("note");
    let headers = headers(&bytes);
    let find = |name: &str| {
        headers.iter().find(|h| h.name == name).unwrap_or_else(|| panic!("{name} in {headers:#?}"))
    };
    assert_eq!(find(".note.Linux").kind, SHT_NOTE);
    let empty = find(".empty.keep");
    assert_eq!((empty.kind, empty.size), (SHT_PROGBITS, 0));
    let kept = find(".text.kept");
    assert_ne!(kept.flags & SHF_GNU_RETAIN, 0, "{kept:?}");
}

#[test]
fn each_group_holds_its_sections_and_only_comdat_ones_say_so() {
    let bytes = object("groups");
    let headers = headers(&bytes);
    let index = |name: &str| headers.iter().position(|h| h.name == name).expect(name);
    let groups: Vec<Vec<usize>> = headers
        .iter()
        .filter(|h| h.kind == SHT_GROUP)
        .map(|h| (0..h.size / 4).map(|n| word(&bytes, h.offset + n * 4, 4)).collect())
        .collect();
    assert_eq!(groups.len(), 3, "{headers:#?}");
    for (member, comdat) in [
        (".text.__x86_indirect_thunk_rax", true),
        (".data.plain", false),
        (".gnu.linkonce.t.foo", true),
    ] {
        let at = index(member);
        assert_ne!(headers[at].flags & SHF_GROUP, 0, "{member}");
        let group = groups.iter().find(|g| g[1..].contains(&at)).expect(member);
        assert_eq!(group[0] == GRP_COMDAT, comdat, "{member}: {group:?}");
    }
}
