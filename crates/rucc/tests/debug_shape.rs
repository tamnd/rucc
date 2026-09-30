//! What the two flags about the shape of the debug output do.
//!
//! Design: `spec/04-driver-and-cli.md` sections 4.8 and 4.12.
//!
//! `-gz` says how the debug sections are compressed and `-gsplit-dwarf` says they go in a file of
//! their own. zlib is written in both of its layouts, and `-gz=zstd` is refused until there is a
//! zstd writer, since the kernel's `DEBUG_INFO_COMPRESSED_ZSTD` probes it and a compiler that took
//! it would have the kernel configured for sections that are not there. Nothing writes a second
//! file, so `-gsplit-dwarf` is refused and `-gno-split-dwarf` is taken.
//!
//! What is asserted is the object's section table, read by hand below, because the claim being
//! made to a build that passes the flag is about the object and not about a field somewhere.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A program with enough in it that an object built from it is not empty.
const SOURCE: &str = "\
int twice(int n) { return n + n; }
int total(const int *of, int many) {
    int sum = 0;
    for (int i = 0; i < many; i++) sum += twice(of[i]);
    return sum;
}
";

/// The target the object is built for, written down rather than taken from the host, because an
/// object is only produced for the one this compiler has a back end for.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// A directory of this test's own, so that two of these running at once do not write the same
/// file, with the source already in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-gz-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    dir
}

/// What the compiler did with those flags: whether it succeeded, and what it said.
fn run(dir: &Path, flags: &[&str], object: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-c"])
        .args(flags)
        .arg("-o")
        .arg(dir.join(object))
        .arg(dir.join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// One section of a 64 bit little endian ELF file: its name, its flags and its bytes.
struct Section {
    name: String,
    flags: u64,
    bytes: Vec<u8>,
}

/// Every section in the file, read from the header table the way a linker reads it.
fn sections(file: &[u8]) -> Vec<Section> {
    let u16_at = |at: usize| usize::from(u16::from_le_bytes([file[at], file[at + 1]]));
    let u32_at = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().expect("four bytes"));
    let u64_at = |at: usize| u64::from_le_bytes(file[at..at + 8].try_into().expect("eight bytes"));
    let table = u64_at(0x28) as usize;
    let (size, count, names) = (u16_at(0x3a), u16_at(0x3c), u16_at(0x3e));
    let header = |index: usize| table + index * size;
    let strings = u64_at(header(names) + 24) as usize;
    (0..count)
        .map(|index| {
            let at = header(index);
            let name = strings + u32_at(at) as usize;
            let end = file[name..].iter().position(|&byte| byte == 0).expect("a name ends");
            let (offset, len) = (u64_at(at + 24) as usize, u64_at(at + 32) as usize);
            Section {
                name: String::from_utf8_lossy(&file[name..name + end]).into_owned(),
                flags: u64_at(at + 8),
                bytes: file[offset..offset + len].to_vec(),
            }
        })
        .collect()
}

fn named<'a>(all: &'a [Section], name: &str) -> &'a Section {
    all.iter().find(|section| section.name == name).unwrap_or_else(|| panic!("no {name}"))
}

/// `SHF_COMPRESSED`.
const COMPRESSED: u64 = 0x800;

#[test]
fn asking_for_no_compression_changes_nothing() {
    let dir = fixture("same");
    let (ok, said) = run(&dir, &["-g"], "plain.o");
    assert!(ok, "{said}");
    let plain = std::fs::read(dir.join("plain.o")).expect("the object was written");
    let (ok, said) = run(&dir, &["-g", "-gz=none"], "asked.o");
    let asked = std::fs::read(dir.join("asked.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok && said.is_empty(), "-gz=none: {said}");
    assert_eq!(asked, plain, "-gz=none changed the object");
}

/// `-gz` and `-gz=zlib` keep each section's name, mark it compressed, and start it with an
/// `Elf64_Chdr` that says zlib and gives the size the section was before.
#[test]
fn zlib_marks_the_section_compressed_and_says_how_large_it_was() {
    let dir = fixture("zlib");
    let (ok, said) = run(&dir, &["-g"], "plain.o");
    assert!(ok, "{said}");
    let plain = std::fs::read(dir.join("plain.o")).expect("the object was written");
    let plain = sections(&plain);
    for spelling in ["-gz", "-gz=zlib"] {
        let (ok, said) = run(&dir, &["-g", spelling], "packed.o");
        assert!(ok && said.is_empty(), "{spelling}: {said}");
        let packed = std::fs::read(dir.join("packed.o")).expect("the object was written");
        let packed = sections(&packed);
        let info = named(&packed, ".debug_info");
        assert_eq!(info.flags & COMPRESSED, COMPRESSED, "{spelling}: not marked compressed");
        let word = |at: usize| u64::from_le_bytes(info.bytes[at..at + 8].try_into().expect("8"));
        assert_eq!(info.bytes[..4], [1, 0, 0, 0], "{spelling}: not ELFCOMPRESS_ZLIB");
        assert_eq!(word(8), named(&plain, ".debug_info").bytes.len() as u64, "{spelling}");
        assert_eq!(word(16), 1, "{spelling}: the alignment it had");
        // A zlib stream with a 32 KiB window, right after the header.
        assert_eq!(info.bytes[24], 0x78, "{spelling}");
        assert!(info.bytes.len() < named(&plain, ".debug_info").bytes.len());
        // The relocations stay as they were, counted into the section as it was before.
        let relocs = named(&packed, ".rela.debug_info");
        assert_eq!(relocs.bytes, named(&plain, ".rela.debug_info").bytes, "{spelling}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `-gz=zlib-gnu` is the older layout: the section is renamed to `.zdebug_*` and starts with
/// `ZLIB` and its size in eight big endian bytes, with no flag.
#[test]
fn zlib_gnu_renames_the_section_and_gives_its_size_after_zlib() {
    let dir = fixture("gnu");
    let (ok, said) = run(&dir, &["-g"], "plain.o");
    assert!(ok, "{said}");
    let plain = std::fs::read(dir.join("plain.o")).expect("the object was written");
    let (ok, said) = run(&dir, &["-g", "-gz=zlib-gnu"], "packed.o");
    let packed = std::fs::read(dir.join("packed.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok && said.is_empty(), "{said}");
    let (plain, packed) = (sections(&plain), sections(&packed));
    assert!(packed.iter().all(|section| section.name != ".debug_info"));
    let info = named(&packed, ".zdebug_info");
    assert_eq!(info.flags & COMPRESSED, 0);
    assert_eq!(&info.bytes[..4], b"ZLIB");
    let size = u64::from_be_bytes(info.bytes[4..12].try_into().expect("eight bytes"));
    assert_eq!(size, named(&plain, ".debug_info").bytes.len() as u64);
    named(&packed, ".rela.zdebug_info");
}

#[test]
fn zstd_and_a_name_nothing_has_heard_of_are_refused() {
    let dir = fixture("never");
    let (ok, said) = run(&dir, &["-g", "-gz=zstd"], "never.o");
    assert!(!ok, "-gz=zstd is refused");
    assert!(said.contains("#2288"), "{said}");

    // A value nothing has heard of stops the compilation, so that a typo in a distribution's
    // flags is found here rather than by whoever later wonders why nothing got smaller.
    let (ok, said) = run(&dir, &["-gz=gzip"], "never.o");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "a value outside the list is refused");
    assert!(said.contains("is not a way to compress"), "{said}");
    assert!(said.contains("zstd"), "the refusal lists the names: {said}");
}

#[test]
fn splitting_the_debug_information_off_is_refused_and_not_splitting_it_is_not() {
    // gcc writes the `.dwo` whether or not it found anything to put in it, so a make rule that
    // depends on the file fires there and would not fire here. Refusing says so at the point the
    // flag is read, which is the only point where the answer is any use to the person reading it.
    let dir = fixture("split");
    let (ok, said) = run(&dir, &["-gsplit-dwarf", "-g"], "one.o");
    assert!(!ok, "the flag is refused: {said}");
    assert!(said.contains(".dwo"), "the refusal names the file it would have written: {said}");
    assert!(!dir.join("one.dwo").exists(), "and no such file was written");

    // The other direction describes what happens, so it is taken and says nothing, and it leaves
    // the question of how much debug information there is to the flag that asks that.
    let (ok, said) = run(&dir, &["-gno-split-dwarf", "-g"], "whole.o");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{said}");
    assert!(said.is_empty(), "{said}");
}
