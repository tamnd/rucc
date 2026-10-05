//! `__attribute__((noinit))` and `__attribute__((persistent))`, end to end: a variable the startup
//! code leaves alone is in `.noinit` or `.persistent`, or a section of its own under
//! `-fdata-sections`, in the listing and in the object the compiler writes itself, and what gcc
//! says about the two on the way, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The ELF targets the listing is read for, written down rather than taken from the host.
const TARGETS: [&str; 3] =
    ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "i686-unknown-linux-gnu"];

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-noinit-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, what it wrote on its standard output and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The assembly of one source for a target under those flags, and what was said.
fn assembly(what: &str, target: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={target}");
    let mut args = vec![target.as_str(), "-S", "-o", "-"];
    args.extend_from_slice(flags);
    args.push("a.c");
    let said = run(&dir, &args);
    let _ = std::fs::remove_dir_all(&dir);
    said
}

/// The listing of a source that has to compile without a word.
fn listing(what: &str, target: &str, flags: &[&str], source: &str) -> String {
    let (ok, text, err) = assembly(what, target, flags, source);
    assert!(ok, "{target} {flags:?}: {err}");
    assert_eq!(err, "", "{target} {flags:?}");
    text
}

/// The directive that opened the section a label is in: the last one in front of it.
fn opened<'a>(text: &'a str, label: &str) -> &'a str {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.strip_suffix(':').is_some_and(|name| name == label))
        .unwrap_or_else(|| panic!("{label} is in the listing:\n{text}"));
    lines[..at]
        .iter()
        .rev()
        .find(|line| {
            line.starts_with("\t.section\t") || matches!(**line, "\t.data" | "\t.bss" | "\t.text")
        })
        .unwrap_or_else(|| panic!("{label} is in a section:\n{text}"))
}

/// The label a `static` in a function was given, which has the name the program wrote in it.
fn local<'a>(text: &'a str, name: &str) -> &'a str {
    text.lines()
        .filter_map(|line| line.strip_suffix(':'))
        .find(|label| !label.starts_with('.') && label.contains(name))
        .unwrap_or_else(|| panic!("{name} is in the listing:\n{text}"))
}

/// Every place either attribute is written to be kept, and the places it is not.
const KEPT: &str = "int counter __attribute__((noinit));\n\
    int table[64] __attribute__((noinit));\n\
    int boots __attribute__((persistent)) = 3;\n\
    int zeroed __attribute__((persistent)) = 0;\n\
    static int hidden __attribute__((noinit));\n\
    __attribute__((__noinit__)) int armoured;\n\
    [[gnu::persistent]] int standard = 4;\n\
    extern int later __attribute__((noinit));\n\
    int later;\n\
    extern int given __attribute__((noinit));\n\
    int given = 5;\n\
    _Thread_local int each __attribute__((noinit));\n\
    _Thread_local int every __attribute__((persistent)) = 6;\n\
    int plain;\n\
    int *use(int i) {\n\
        static int kept __attribute__((persistent)) = 7;\n\
        static int lost __attribute__((noinit));\n\
        return i ? &kept : i > 1 ? &lost : &hidden;\n\
    }\n";

const NOINIT: &str = "\t.section\t.noinit,\"aw\",@nobits";
const PERSISTENT: &str = "\t.section\t.persistent,\"aw\",@progbits";

#[test]
fn a_variable_the_startup_code_leaves_alone_is_in_a_section_of_its_own() {
    for target in TARGETS {
        for level in ["-O0", "-O2"] {
            for common in ["-fno-common", "-fcommon"] {
                let text = listing("kept", target, &[level, common], KEPT);
                let at = format!("{target} {level} {common}");
                for name in ["counter", "table", "hidden", "armoured", "later"] {
                    assert_eq!(opened(&text, name), NOINIT, "{at}: {name}:\n{text}");
                }
                assert_eq!(opened(&text, local(&text, "lost")), NOINIT, "{at}:\n{text}");
                for name in ["boots", "zeroed", "standard"] {
                    assert_eq!(opened(&text, name), PERSISTENT, "{at}: {name}:\n{text}");
                }
                assert_eq!(opened(&text, local(&text, "kept")), PERSISTENT, "{at}:\n{text}");
                // The definition has an initializer, and a thread-local is a copy per thread.
                assert_eq!(opened(&text, "given"), "\t.data", "{at}:\n{text}");
                assert!(opened(&text, "each").starts_with("\t.section\t.tbss"), "{at}:\n{text}");
                assert!(opened(&text, "every").starts_with("\t.section\t.tdata"), "{at}:\n{text}");
                // Zeros a reset leaves alone are not the linker's to merge.
                assert!(!text.contains(".comm\tcounter"), "{at}:\n{text}");
                assert_eq!(text.contains(".comm\tplain"), common == "-fcommon", "{at}:\n{text}");
            }
        }
        // A section of each variable's own, named after the one it would have gone in.
        let text = listing("split", target, &["-O2", "-fdata-sections"], KEPT);
        for name in ["counter", "table", "hidden", "armoured", "later"] {
            let wanted = format!("\t.section\t.noinit.{name},\"aw\",@nobits");
            assert_eq!(opened(&text, name), wanted, "{target}: {name}:\n{text}");
        }
        for name in ["boots", "zeroed", "standard"] {
            let wanted = format!("\t.section\t.persistent.{name},\"aw\"");
            assert_eq!(opened(&text, name), wanted, "{target}: {name}:\n{text}");
        }
        let lost = opened(&text, local(&text, "lost"));
        assert!(lost.starts_with("\t.section\t.noinit."), "{target}:\n{text}");
        let kept = opened(&text, local(&text, "kept"));
        assert!(kept.starts_with("\t.section\t.persistent."), "{target}:\n{text}");
    }
    // Which of a `section` and one of these is kept is which was written first, as in gcc, and
    // the same for the two of these together.
    let source = "int a __attribute__((section(\".x\"), noinit));\n\
        int b __attribute__((noinit, section(\".y\")));\n\
        int c __attribute__((noinit)) __attribute__((section(\".z\")));\n\
        int d __attribute__((noinit, persistent)) = 1;\n\
        int e __attribute__((persistent, noinit)) = 1;\n\
        int f __attribute__((noinit, persistent));\n";
    let (ok, text, err) = assembly("order", TARGETS[0], &[], source);
    assert!(ok, "{err}");
    assert!(opened(&text, "a").starts_with("\t.section\t.x,"), "{text}");
    for name in ["b", "c", "f"] {
        assert_eq!(opened(&text, name), NOINIT, "{name}:\n{text}");
    }
    for name in ["d", "e"] {
        assert_eq!(opened(&text, name), PERSISTENT, "{name}:\n{text}");
    }
    // Only ELF has either section, so the variables are where they would have been.
    for target in ["x86_64-pc-windows-gnu", "x86_64-apple-darwin"] {
        let (ok, text, err) = assembly("elsewhere", target, &["-O2"], KEPT);
        assert!(ok, "{target}: {err}");
        assert!(!text.contains(".noinit") && !text.contains(".persistent"), "{target}:\n{text}");
    }
}

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

/// Every section header of a 64-bit ELF file, in the order they are in it, written out for the
/// reason `sections.rs` gives.
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
const SHT_NOBITS: usize = 8;
const SHF_WRITE: usize = 1;
const SHF_ALLOC: usize = 2;

/// The section headers of the object made from one file with those flags.
fn object(what: &str, file: &str, text: &str, flags: &[&str]) -> Vec<Header> {
    let dir = dir(what);
    std::fs::write(dir.join(file), text).expect("the fixture can be written");
    let mut args = vec!["--target=x86_64-unknown-linux-gnu", "-c", file, "-o", "a.o"];
    args.extend_from_slice(flags);
    let (ok, _, err) = run(&dir, &args);
    assert!(ok, "{file} {flags:?}: {err}");
    let bytes = std::fs::read(dir.join("a.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    let mut headers = headers(&bytes);
    // What the section holds, for the one that holds anything, in place of where it is.
    for header in &mut headers {
        if header.kind == SHT_PROGBITS && header.name.starts_with(".persistent") {
            header.offset = four(&bytes, header.offset);
        }
    }
    headers
}

/// The object the compiler writes itself has the same sections with the types gas gives them,
/// from C, from the listing, and from a listing written the way gcc writes it, which leaves the
/// type to the name.
#[test]
fn the_object_has_the_same_sections() {
    let source = "int counter __attribute__((noinit));\n\
        int boots __attribute__((persistent)) = 3;\n";
    let kept = SHF_ALLOC | SHF_WRITE;
    let gcc = "\t.globl\tcounter\n\t.section\t.noinit,\"aw\"\n\t.align 4\ncounter:\n\t.zero\t4\n\
        \t.globl\tboots\n\t.section\t.persistent,\"aw\"\n\t.align 4\nboots:\n\t.long\t3\n";
    let (ok, listed, err) = assembly("listed", "x86_64-unknown-linux-gnu", &["-O2"], source);
    assert!(ok, "{err}");
    for (what, file, text, flags) in [
        ("c", "a.c", source, &["-O2"][..]),
        ("listed", "a.s", listed.as_str(), &[][..]),
        ("gcc", "a.s", gcc, &[][..]),
    ] {
        let headers = object(what, file, text, flags);
        let find = |name: &str| {
            headers
                .iter()
                .find(|header| header.name == name)
                .unwrap_or_else(|| panic!("{what}: {name}: {headers:?}"))
        };
        let noinit = find(".noinit");
        assert_eq!((noinit.kind, noinit.flags, noinit.size), (SHT_NOBITS, kept, 4), "{what}");
        let persistent = find(".persistent");
        assert_eq!(
            (persistent.kind, persistent.flags, persistent.size, persistent.offset),
            (SHT_PROGBITS, kept, 4, 3),
            "{what}"
        );
    }
    let headers = object("split", "a.c", source, &["-O2", "-fdata-sections"]);
    let find = |name: &str| {
        headers.iter().find(|header| header.name == name).unwrap_or_else(|| panic!("{headers:?}"))
    };
    assert_eq!((find(".noinit.counter").kind, find(".noinit.counter").size), (SHT_NOBITS, 4));
    assert_eq!(find(".persistent.boots").kind, SHT_PROGBITS);
    // And a section the program names `.noinit` holds no bytes either, as gas makes it.
    let named = "int v __attribute__((section(\".noinit\")));\n";
    let headers = object("named", "a.c", named, &["-O2"]);
    let noinit = headers.iter().find(|header| header.name == ".noinit").expect("it is there");
    assert_eq!(noinit.kind, SHT_NOBITS);
}

/// What gcc 13 says about the two, in its words, and whether it stops there.
#[test]
fn noinit_and_persistent_are_checked_in_gcc_s_words() {
    let target = TARGETS[0];
    let conflicts = |name: &str, with: &str| {
        format!("warning: ignoring attribute '{name}' because it conflicts with attribute '{with}'")
    };
    let elsewhere = |name: &str| format!("warning: ignoring '{name}' attribute not set on a variable");
    let local = |name: &str| format!("error: '{name}' attribute cannot be specified for local variables");
    let arity = "error: wrong number of arguments specified for 'noinit' attribute".to_owned();
    for (source, said, errors, warnings) in [
        (
            "int v __attribute__((noinit)) = 1;\n",
            "warning: ignoring 'noinit' attribute set on initialized variable".to_owned(),
            0,
            1,
        ),
        (
            "int v __attribute__((persistent));\n",
            "warning: ignoring 'persistent' attribute set on uninitialized variable".to_owned(),
            0,
            1,
        ),
        (
            "extern int v __attribute__((persistent));\n",
            "warning: ignoring 'persistent' attribute set on uninitialized variable".to_owned(),
            0,
            1,
        ),
        (
            "const int v __attribute__((noinit));\n",
            "warning: ignoring 'noinit' attribute set on const variable".to_owned(),
            0,
            1,
        ),
        (
            "const int v[2] __attribute__((persistent)) = {1};\n",
            "warning: ignoring 'persistent' attribute set on const variable".to_owned(),
            0,
            1,
        ),
        ("__attribute__((noinit)) void f(void);\n", elsewhere("noinit"), 0, 1),
        ("__attribute__((persistent)) void f(void) {}\n", elsewhere("persistent"), 0, 1),
        ("struct s { int m __attribute__((noinit)); };\n", elsewhere("noinit"), 0, 1),
        ("typedef int t __attribute__((persistent));\n", elsewhere("persistent"), 0, 1),
        ("void f(int p __attribute__((noinit))) { (void)p; }\n", elsewhere("noinit"), 0, 1),
        ("void f(void) { int x __attribute__((noinit)); (void)x; }\n", local("noinit"), 1, 0),
        (
            "void f(void) { int x __attribute__((persistent)) = 1; (void)x; }\n",
            local("persistent"),
            1,
            0,
        ),
        ("int v __attribute__((noinit(1)));\n", arity.clone(), 1, 0),
        ("int v __attribute__((noinit(1)));\n", "expected 0, found 1".to_owned(), 1, 0),
        ("int v __attribute__((persistent(1, 2))) = 1;\n", "expected 0, found 2".to_owned(), 1, 0),
        ("int v __attribute__((section(\".x\"), noinit));\n", conflicts("noinit", "section"), 0, 1),
        ("int v __attribute__((noinit, section(\".x\")));\n", conflicts("section", "noinit"), 0, 1),
        (
            "int v __attribute__((persistent)) __attribute__((section(\".x\"))) = 1;\n",
            conflicts("section", "persistent"),
            0,
            1,
        ),
        ("int v __attribute__((noinit, persistent));\n", conflicts("persistent", "noinit"), 0, 1),
        ("int v __attribute__((persistent, noinit)) = 1;\n", conflicts("noinit", "persistent"), 0, 1),
    ] {
        let (ok, _, err) = assembly("words", target, &[], source);
        assert_eq!(ok, errors == 0, "{source}\n{err}");
        assert!(err.contains(&said), "{source}\nwanted {said:?}, got:\n{err}");
        assert_eq!(err.matches("error:").count(), errors, "{source}\n{err}");
        assert_eq!(err.matches("warning:").count(), warnings, "{source}\n{err}");
    }
    // Nothing to say about a declaration with no initializer, a `static` in a function, one in a
    // function that is defined elsewhere, a thread-local one, the same one said twice, or about
    // asking for either.
    let quiet = "extern int a __attribute__((noinit));\n\
        int *pa(void) { return &a; }\n\
        int *ps(void) { static int s __attribute__((noinit)); extern int e __attribute__((noinit));\n\
            return s ? &s : &e; }\n\
        _Thread_local int t __attribute__((noinit));\n\
        int twice __attribute__((noinit, noinit));\n\
        int again __attribute__((persistent)) __attribute__((persistent)) = 1;\n\
        _Static_assert(__has_attribute(noinit) && __has_attribute(persistent), \"gcc 11\");\n";
    for level in ["-O0", "-O2"] {
        let (ok, _, err) = assembly("quiet", target, &[level, "-Wall", "-Wextra"], quiet);
        assert!(ok, "{level}: {err}");
        assert_eq!(err, "", "{level}");
    }
    // gcc 10 had neither on the targets this reads them for.
    let old = "int v __attribute__((noinit));\n";
    let (ok, text, err) = assembly("old", target, &["-fgnuc-version=10.2.0"], old);
    assert!(ok, "{err}");
    assert!(err.contains("warning: 'noinit' attribute directive ignored"), "{err}");
    assert!(!text.contains(".noinit"), "{text}");
}
