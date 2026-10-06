//! `__attribute__((shared))`, end to end: on x86 Windows the section a variable is in becomes one
//! every process running the image shares, `"dws"` in the listing and `IMAGE_SCN_MEM_SHARED` in
//! the object, and what gcc says about the attribute where it is wrong or means nothing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const MINGW64: &str = "x86_64-w64-windows-gnu";
const MINGW32: &str = "i686-w64-windows-gnu";
const LINUX: &str = "x86_64-unknown-linux-gnu";

/// IMAGE_SCN_MEM_SHARED.
const SHARED: usize = 0x1000_0000;
/// IMAGE_SCN_MEM_WRITE.
const WRITE: usize = 0x8000_0000;

/// A directory of this test's own, empty. The tests in this file run on threads of one process,
/// so the process id alone would give two of them the same one.
fn dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-shared-section-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// Whether the compiler agreed, what it wrote to standard output, and what it said, for `source`
/// in a file called `file`.
fn run(dir: &Path, file: &str, flags: &[&str], source: &[u8]) -> (bool, Vec<u8>, String) {
    std::fs::write(dir.join(file), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .args(["-o", "-"])
        .arg(file)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(dir);
    (out.status.success(), out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// What the compiler said about `source` for `target`, and whether it agreed, with `-S`.
fn said(target: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let target = format!("--target={target}");
    let mut all = vec![target.as_str(), "-O2", "-S"];
    all.extend_from_slice(flags);
    let (ok, out, said) = run(&dir(), "a.c", &all, source.as_bytes());
    (ok, String::from_utf8_lossy(&out).into_owned(), said)
}

/// Each section's name and characteristics in a COFF object. A name longer than eight bytes is
/// `/` and where it is in the string table, which starts after the symbol table.
fn sections(bytes: &[u8]) -> Vec<(String, usize)> {
    let u16_at = |at: usize| usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
    let u32_at = |at: usize| {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes")) as usize
    };
    let strings = u32_at(8) + 18 * u32_at(12);
    let headers = 20 + u16_at(16);
    (0..u16_at(2))
        .map(|section| {
            let header = headers + section * 40;
            let short = String::from_utf8_lossy(&bytes[header..header + 8]);
            let short = short.trim_end_matches('\0').to_owned();
            let name = match short.strip_prefix('/') {
                Some(at) => {
                    let at = strings + at.parse::<usize>().expect("a string table offset");
                    let end = bytes[at..].iter().position(|&b| b == 0).expect("an end");
                    String::from_utf8_lossy(&bytes[at..at + end]).into_owned()
                }
                None => short,
            };
            (name, u32_at(header + 36))
        })
        .collect()
}

fn flags_of(sections: &[(String, usize)], name: &str) -> usize {
    sections
        .iter()
        .find(|(section, _)| section == name)
        .unwrap_or_else(|| panic!("no {name} in {sections:?}"))
        .1
}

const SOURCE: &str = "\
int counter __attribute__((section(\"shared\"), shared)) = 0;
__attribute__((shared, section(\".shr\"))) int table[4] = {1, 2, 3, 4};
extern int late __attribute__((shared));
int late __attribute__((section(\"late\"))) = 2;
int plain __attribute__((section(\"unshared\"))) = 1;
const int fixed __attribute__((section(\".shrc\"), shared)) = 5;
int alone __attribute__((shared)) = 6;
int bump(void) {
    int local __attribute__((shared)) = 1;
    return ++counter + table[1] + late + plain + fixed + alone + local;
}
_Static_assert(__has_attribute(shared), \"x86 Windows\");
";

/// A written section with a shared variable in it is shared, in the listing, in the object the
/// compiler writes and in the object the listing assembles into. One a constant is in is read
/// only and gcc gives it no such flag, and a variable in no section of its own changes nothing.
#[test]
fn a_shared_variable_s_section_is_shared_between_the_copies_of_its_image() {
    for target in [MINGW64, MINGW32] {
        let (ok, listing, err) = said(target, &["-Wall", "-Wextra"], SOURCE);
        assert!(ok, "{target}: {err}");
        assert_eq!(err, "", "{target}");
        for line in [
            "\t.section\tshared,\"dws\"\n",
            "\t.section\t.shr,\"dws\"\n",
            "\t.section\tlate,\"dws\"\n",
            "\t.section\tunshared,\"dw\"\n",
            "\t.section\t.shrc,\"dr\"\n",
        ] {
            assert!(listing.contains(line), "{target}: wanted {line:?} in:\n{listing}");
        }
        assert_eq!(listing.matches("\"dws\"").count(), 3, "{target}:\n{listing}");

        let target_flag = format!("--target={target}");
        let (ok, object, err) = run(&dir(), "a.c", &[&target_flag, "-O2", "-c"], SOURCE.as_bytes());
        assert!(ok, "{target}: {err}");
        let (ok, assembled, err) =
            run(&dir(), "a.s", &[&target_flag, "-c"], listing.as_bytes());
        assert!(ok, "{target}: {err}");
        for object in [object, assembled] {
            let sections = sections(&object);
            for name in ["shared", ".shr", "late"] {
                let flags = flags_of(&sections, name);
                assert_eq!(flags & (SHARED | WRITE), SHARED | WRITE, "{target} {name}: {flags:#x}");
            }
            for name in ["unshared", ".data"] {
                let flags = flags_of(&sections, name);
                assert_eq!(flags & (SHARED | WRITE), WRITE, "{target} {name}: {flags:#x}");
            }
            let flags = flags_of(&sections, ".shrc");
            assert_eq!(flags & (SHARED | WRITE), 0, "{target} .shrc: {flags:#x}");
        }
    }
}

/// What gcc says about the attribute where it is wrong, and where it has never heard of it.
#[test]
fn shared_is_checked_in_gcc_s_words() {
    let name = "'shared' attribute";
    let anywhere_else = format!("warning: {name} only applies to variables");
    for (source, said_here, errors, warnings) in [
        ("__attribute__((shared)) void f(void);\n", anywhere_else.clone(), 0, 1),
        ("__attribute__((shared)) void f(void) {}\n", anywhere_else.clone(), 0, 1),
        ("typedef int t __attribute__((shared));\n", anywhere_else.clone(), 0, 1),
        ("void f(int x __attribute__((shared)));\n", anywhere_else.clone(), 0, 1),
        (
            "int v __attribute__((shared(1), section(\"s\"))) = 1;\n",
            format!("error: wrong number of arguments specified for {name}"),
            1,
            0,
        ),
        (
            "int v __attribute__((shared(1, 2), section(\"s\"))) = 1;\n",
            "expected 0, found 2".to_owned(),
            1,
            0,
        ),
    ] {
        for target in [MINGW64, MINGW32] {
            let (ok, _, err) = said(target, &[], source);
            assert_eq!(ok, errors == 0, "{target}: {source}\n{err}");
            assert!(err.contains(&said_here), "{target}: {source}\nwanted {said_here:?}, got:\n{err}");
            assert_eq!(err.matches("error:").count(), errors, "{target}: {source}\n{err}");
            assert_eq!(err.matches("warning:").count(), warnings, "{target}: {source}\n{err}");
        }
    }

    // gcc has it in its PE back end for x86 alone, so on Linux and on Arm Windows it is a name
    // gcc has never heard of, and the section is what it would have been.
    let source = "int v __attribute__((shared, section(\"s\"))) = 1;\n\
        [[gnu::shared]] int w __attribute__((section(\"s\"))) = 2;\n\
        _Static_assert(!__has_attribute(shared), \"not here\");\n";
    for target in [LINUX, "aarch64-w64-mingw32"] {
        let (ok, listing, err) = said(target, &["-std=gnu23"], source);
        assert!(ok, "{target}: {err}");
        assert!(err.contains(&format!("warning: {name} directive ignored")), "{target}: {err}");
        assert!(
            err.contains("warning: 'gnu::shared' scoped attribute directive ignored"),
            "{target}: {err}"
        );
        assert_eq!(err.matches("warning:").count(), 2, "{target}: {err}");
        assert!(!listing.contains("dws"), "{target}:\n{listing}");
    }
}
