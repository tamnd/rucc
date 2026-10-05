//! `__attribute__((nodirect_extern_access))`, end to end: a name a declaration said it of is
//! reached through the global offset table even in position dependent code, a call to it is the
//! call it was, the file says it needs that in a note gcc writes, and what gcc says about the
//! attribute on the way, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;

const X86_64: &str = "x86_64-unknown-linux-gnu";
const I686: &str = "i686-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-nodirect-{}-{what}", std::process::id()));
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
    let mut args = vec![target.as_str(), "-S", "-o", "-", "-fcf-protection=none"];
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

/// Names said `nodirect_extern_access` of and names that were not, read and taken the address of
/// and called.
const SOURCE: &str = "extern int v __attribute__((nodirect_extern_access));\n\
    extern int w;\n\
    extern int hv __attribute__((nodirect_extern_access, visibility(\"hidden\")));\n\
    int def __attribute__((nodirect_extern_access)) = 1;\n\
    extern void f(void) __attribute__((nodirect_extern_access));\n\
    [[gnu::nodirect_extern_access]] extern void s(void);\n\
    extern void g(void);\n\
    int r(void) { return v + w + hv + def; }\n\
    void *p(void) { return f; }\n\
    void *t(void) { return s; }\n\
    void *q(void) { return g; }\n\
    void c(void) { f(); g(); }\n";

#[test]
fn a_name_kept_from_direct_access_is_read_out_of_the_table() {
    for level in ["-O0", "-O2"] {
        for pic in ["-fno-pic", "-fpie", "-fpic"] {
            let at = format!("x86-64 {level} {pic}");
            let text = listing("x86-64", X86_64, &[level, pic], SOURCE);
            for name in ["v", "f", "s"] {
                assert!(text.contains(&format!("{name}@GOTPCREL(%rip)")), "{at}: {name}:\n{text}");
            }
            // Hidden is in this image whatever was said, and a definition is here.
            assert!(!text.contains("hv@GOT"), "{at}:\n{text}");
            assert!(text.contains("hv(%rip)"), "{at}:\n{text}");
            assert_eq!(text.contains("def@GOTPCREL"), pic == "-fpic", "{at}:\n{text}");
            // The rest are where they were.
            assert_eq!(text.contains("w@GOTPCREL"), pic == "-fpic", "{at}:\n{text}");
            assert_eq!(text.contains("g@GOTPCREL"), pic != "-fno-pic", "{at}:\n{text}");
            // And a call is a call, not one through the slot.
            assert!(!text.contains("*f@"), "{at}:\n{text}");
            let call = if pic == "-fno-pic" { "\tcall\tf\n" } else { "\tcall\tf@PLT\n" };
            assert!(text.contains(call), "{at}:\n{text}");
        }
        // i386 has no addressing from the instruction pointer, so in position dependent code
        // the slot is named by its own address, and in position independent code it is counted
        // from the table's address in a register.
        let text = listing("i686", I686, &[level, "-fno-pic"], SOURCE);
        for name in ["v", "f", "s"] {
            assert!(text.contains(&format!("\t{name}@GOT, %e")), "{level}: {name}:\n{text}");
        }
        for name in ["w", "g", "hv", "def"] {
            assert!(!text.contains(&format!("{name}@GOT")), "{level}: {name}:\n{text}");
        }
        assert!(text.contains("\tcall\tf\n"), "{level}:\n{text}");
        let text = listing("i686-pic", I686, &[level, "-fpic"], SOURCE);
        for name in ["v", "f", "s"] {
            assert!(text.contains(&format!("\t{name}@GOT(%e")), "{level}: {name}:\n{text}");
        }
        assert!(text.contains("hv@GOTOFF(%e"), "{level}:\n{text}");
    }
    // Nothing changes anywhere else, where gcc does not have it.
    let plain = SOURCE
        .replace("nodirect_extern_access, ", "")
        .replace("__attribute__((nodirect_extern_access))", "")
        .replace("[[gnu::nodirect_extern_access]]", "");
    for target in ["aarch64-unknown-linux-gnu", "x86_64-apple-darwin"] {
        let said = listing("elsewhere", target, &["-O2"], SOURCE);
        assert_eq!(said, listing("plain", target, &["-O2"], &plain), "{target}");
    }
}

/// The lines of the note that says the file needs what is defined elsewhere reached indirectly,
/// on a machine whose addresses are that wide.
fn needed(align: u32) -> Vec<String> {
    let mut lines = vec![
        "\t.long\t4".to_owned(),
        format!("\t.long\t{}", 12u32.next_multiple_of(align)),
        "\t.long\t5".to_owned(),
        "\t.asciz\t\"GNU\"".to_owned(),
        "\t.long\t0xb0008000".to_owned(),
        "\t.long\t4".to_owned(),
        "\t.long\t0x1".to_owned(),
    ];
    if align == 8 {
        lines.push("\t.long\t0".to_owned());
    }
    lines
}

/// The lines of the section that holds the notes, from its directive to the next section.
fn notes(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let Some(at) = lines.iter().position(|line| line.contains(".note.gnu.property")) else {
        return Vec::new();
    };
    lines[at + 1..]
        .iter()
        .take_while(|line| !line.starts_with("\t.section") && !line.starts_with("\t.ident"))
        .map(|&line| line.to_owned())
        .collect()
}

#[test]
fn the_file_says_it_needs_names_reached_indirectly() {
    for (target, align) in [(X86_64, 8), (I686, 4)] {
        let mut wanted = vec![format!("\t.p2align\t{}", align.trailing_zeros())];
        wanted.extend(needed(align));
        for level in ["-O0", "-O2"] {
            let text = listing("noted", target, &[level], SOURCE);
            assert_eq!(notes(&text), wanted, "{target} {level}:\n{text}");
        }
        // Once a name it was said of is defined, called, read or written into an image, which
        // is when gcc asks about it.
        for source in [
            "int d __attribute__((nodirect_extern_access));\n",
            "void d(void) __attribute__((nodirect_extern_access));\nvoid d(void) {}\n",
            "extern void d(void) __attribute__((nodirect_extern_access));\nvoid u(void) { d(); }\n",
            "extern int d __attribute__((nodirect_extern_access));\nint *u = &d;\n",
        ] {
            let text = listing("used", target, &["-O2"], source);
            assert_eq!(notes(&text), wanted, "{target}: {source}\n{text}");
        }
        // Not for a declaration nothing uses, nor for a file that never said it.
        for source in [
            "extern int d __attribute__((nodirect_extern_access));\nint k;\n",
            "extern int d;\nint u(void) { return d; }\n",
        ] {
            let text = listing("unused", target, &["-O2"], source);
            assert!(!text.contains(".note.gnu.property"), "{target}: {source}\n{text}");
        }
    }
    // After the feature word, as a note of its own, where both are said.
    let text = listing("both", X86_64, &["-O2", "-fcf-protection=full"], SOURCE);
    let mut wanted = vec![
        "\t.p2align\t3".to_owned(),
        "\t.long\t4".to_owned(),
        "\t.long\t16".to_owned(),
        "\t.long\t5".to_owned(),
        "\t.asciz\t\"GNU\"".to_owned(),
        "\t.long\t0xc0000002".to_owned(),
        "\t.long\t4".to_owned(),
        "\t.long\t0x3".to_owned(),
        "\t.long\t0".to_owned(),
    ];
    wanted.extend(needed(8));
    assert_eq!(notes(&text), wanted, "{text}");
}

/// Eight bytes at that offset, as a number.
fn eight(bytes: &[u8], at: usize) -> usize {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes")) as usize
}

/// Four bytes at that offset, as a number.
fn four(bytes: &[u8], at: usize) -> usize {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes")) as usize
}

/// Two bytes at that offset, as a number.
fn two(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes(bytes[at..at + 2].try_into().expect("two bytes")))
}

/// What a section of a 64-bit ELF object holds, found by its name.
fn holds<'a>(object: &'a [u8], wanted: &str) -> Option<&'a [u8]> {
    let start = eight(object, 0x28);
    let (size, count, names) = (two(object, 0x3a), two(object, 0x3c), two(object, 0x3e));
    let at = |which: usize| start + which * size;
    let strings = eight(object, at(names) + 24);
    (0..count).find_map(|which| {
        let header = at(which);
        let name = strings + four(object, header);
        let end = object[name..].iter().position(|&byte| byte == 0).expect("a name ends");
        (&object[name..name + end] == wanted.as_bytes()).then(|| {
            let (offset, size) = (eight(object, header + 24), eight(object, header + 32));
            &object[offset..offset + size]
        })
    })
}

/// The object the compiler writes itself says the same, from C and from the listing.
#[test]
fn the_object_has_the_same_note() {
    let words: [u32; 8] = [4, 16, 5, u32::from_le_bytes(*b"GNU\0"), 0xb000_8000, 4, 1, 0];
    let wanted: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let (ok, listed, err) = assembly("listed", X86_64, &["-O2"], SOURCE);
    assert!(ok, "{err}");
    for (what, file, text) in [("c", "a.c", SOURCE), ("listed", "a.s", listed.as_str())] {
        let dir = dir(what);
        std::fs::write(dir.join(file), text).expect("the fixture can be written");
        let target = format!("--target={X86_64}");
        let args = [target.as_str(), "-fcf-protection=none", "-O2", "-c", file, "-o", "a.o"];
        let (ok, _, err) = run(&dir, &args);
        assert!(ok, "{what}: {err}");
        let object = std::fs::read(dir.join("a.o")).expect("the object was written");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(holds(&object, ".note.gnu.property"), Some(&wanted[..]), "{what}");
    }
}

/// A program that reads a variable and takes the address of a function another file defines,
/// through the slots, links and gets the right answers, position independent or not.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_program_kept_from_direct_access_runs() {
    let user = "extern int v __attribute__((nodirect_extern_access));\n\
        extern int g(void) __attribute__((nodirect_extern_access));\n\
        int (*volatile p)(void);\n\
        int main(void) { p = g; return v + p() + g() == 44 ? 0 : 1; }\n";
    let owner = "int v = 40;\nint g(void) { return 2; }\n";
    for flags in [&["-O0"][..], &["-O2"], &["-O2", "-fno-pic", "-no-pie"], &["-O0", "-fpic"]] {
        let dir = dir("runs");
        std::fs::write(dir.join("a.c"), user).expect("the fixture can be written");
        std::fs::write(dir.join("b.c"), owner).expect("the fixture can be written");
        let mut args = flags.to_vec();
        args.extend_from_slice(&["a.c", "b.c", "-o", "prog"]);
        let (ok, _, err) = run(&dir, &args);
        assert!(ok, "{flags:?}: {err}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{flags:?}: {out:?}");
    }
}

/// What gcc 13 says about the attribute, in its words, and whether it stops there.
#[test]
fn nodirect_extern_access_is_checked_in_gcc_s_words() {
    let public = "warning: 'nodirect_extern_access' attribute have effect only on public objects";
    let ignored = "warning: 'nodirect_extern_access' attribute ignored";
    let arity = "error: wrong number of arguments specified for 'nodirect_extern_access' attribute";
    for (source, said, errors, warnings) in [
        ("static int v __attribute__((nodirect_extern_access));\n", public, 0, 1),
        ("__attribute__((nodirect_extern_access)) static int v;\n", public, 0, 1),
        ("static void f(void) __attribute__((nodirect_extern_access));\n", public, 0, 1),
        (
            "__attribute__((nodirect_extern_access)) static void f(void) {}\n",
            public,
            0,
            1,
        ),
        (
            "void g(void) { int l __attribute__((nodirect_extern_access)); (void)l; }\n",
            public,
            0,
            1,
        ),
        (
            "void g(void) { static int l __attribute__((nodirect_extern_access)); (void)l; }\n",
            public,
            0,
            1,
        ),
        ("struct s { int m __attribute__((nodirect_extern_access)); };\n", ignored, 0, 1),
        ("typedef int t __attribute__((nodirect_extern_access));\n", ignored, 0, 1),
        ("void f(int x __attribute__((nodirect_extern_access)));\n", ignored, 0, 1),
        ("int v __attribute__((nodirect_extern_access(1)));\n", arity, 1, 0),
        ("int v __attribute__((nodirect_extern_access(1)));\n", "expected 0, found 1", 1, 0),
    ] {
        let (ok, _, err) = assembly("words", X86_64, &["-O2"], source);
        assert_eq!(ok, errors == 0, "{source}\n{err}");
        assert!(err.contains(said), "{source}\nwanted {said:?}, got:\n{err}");
        assert_eq!(err.matches("error:").count(), errors, "{source}\n{err}");
        assert_eq!(err.matches("warning:").count(), warnings, "{source}\n{err}");
    }
    // Nothing to say about a name with external linkage, in a block or out of one, a definition
    // or a declaration, or about asking for it.
    let quiet = "extern int v __attribute__((nodirect_extern_access));\n\
        int d __attribute__((__nodirect_extern_access__)) = 2;\n\
        void f(void) __attribute__((nodirect_extern_access));\n\
        __attribute__((nodirect_extern_access)) void h(void) {}\n\
        int g(void) { extern int e __attribute__((nodirect_extern_access)); return e + v; }\n\
        _Static_assert(__has_attribute(nodirect_extern_access), \"gcc 12\");\n";
    for target in [X86_64, I686] {
        let (ok, _, err) = assembly("quiet", target, &["-O2", "-Wall", "-Wextra"], quiet);
        assert!(ok, "{target}: {err}");
        assert_eq!(err, "", "{target}");
    }
    // gcc 11 has never heard of it.
    let old = "extern int v __attribute__((nodirect_extern_access));\n";
    let (ok, _, err) = assembly("old", X86_64, &["-fgnuc-version=11.4.0"], old);
    assert!(ok, "{err}");
    assert!(err.contains("warning: 'nodirect_extern_access' attribute directive ignored"), "{err}");
}
