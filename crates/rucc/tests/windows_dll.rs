//! What `dllimport`, `dllexport` and a variable another file defines reach the assembler as on
//! Windows, which is what gcc and clang write for `x86_64-w64-mingw32`.
//!
//! A name in a DLL is not at an address the link knows. The loader writes where it ended up into a
//! pointer called `__imp_` and the name, and `dllimport` is the program saying so, so the code reads
//! the pointer and calls or loads through it. A variable that is only declared may turn out to be in
//! a DLL too without the program saying so, so the code reads its address out of a pointer of this
//! file's own, `.refptr.` and the name, which the runtime writes if it has to. `dllexport` is an
//! option for the linker, written into `.drectve`.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-windll-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler agreed, what it wrote to standard output, and what it said.
fn run(what: &str, args: &[&str], source: &str) -> (bool, Vec<u8>, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The listing for that source, at the optimization level given.
fn listing(what: &str, opt: &str, source: &str) -> String {
    let (ok, out, said) = run(what, &["--target=x86_64-windows-gnu", opt, "-S"], source);
    assert!(ok, "{said}");
    String::from_utf8(out).expect("a listing is text")
}

const SOURCE: &str = "\
__declspec(dllimport) unsigned long GetCurrentProcessId(void);
__declspec(dllimport) extern int _fmode;
extern int away;
extern int unread;
static int mine = 1;
int here = 2;
extern int hidden __attribute__((visibility(\"hidden\")));

__declspec(dllexport) int count = 3;
__declspec(dllexport) int offered(void) { return 1; }
__declspec(dllexport) int promised(void);

int use(void) {
    return (int)GetCurrentProcessId() + _fmode + away + mine + here + hidden + count;
}
";

#[test]
fn a_name_in_a_dll_is_read_through_the_pointer_the_loader_fills_in() {
    for opt in ["-O0", "-O2"] {
        let text = listing("imp", opt, SOURCE);
        // The call reads the pointer and calls what it holds, and the variable's address is read
        // out of its pointer the same way. Neither name is ever reached directly.
        assert!(text.contains("__imp_GetCurrentProcessId(%rip)"), "{opt}: {text}");
        assert!(text.contains("__imp__fmode(%rip)"), "{opt}: {text}");
        assert!(!text.contains("\tGetCurrentProcessId"), "{opt}: {text}");
        assert!(!text.contains(" GetCurrentProcessId"), "{opt}: {text}");
        assert!(!text.contains("\t_fmode(%rip)"), "{opt}: {text}");
        assert!(!text.contains(".refptr._fmode"), "an imported name needs no second: {text}");
    }
}

#[test]
fn a_variable_only_declared_is_read_through_a_pointer_of_the_files_own() {
    for opt in ["-O0", "-O2"] {
        let text = listing("refptr", opt, SOURCE);
        assert!(text.contains(".refptr.away(%rip)"), "{opt}: {text}");
        let pointer = "\t.section\t.rdata$.refptr.away,\"dr\",discard,.refptr.away\n\
                       \t.globl\t.refptr.away\n\t.p2align\t3\n.refptr.away:\n\t.quad\taway\n";
        assert!(text.contains(pointer), "{opt}: {text}");
        // What clang leaves alone: a variable this file defines, whether or not other files can
        // see it, one that is hidden, and one nothing reads.
        for name in ["mine", "here", "count", "hidden", "unread"] {
            assert!(!text.contains(&format!(".refptr.{name}")), "{opt} {name}: {text}");
        }
    }
}

#[test]
fn a_name_offered_to_other_dlls_is_an_option_for_the_linker() {
    let text = listing("export", "-O0", SOURCE);
    let options = "\t.section\t.drectve,\"yni\"\n\t.ascii\t\" -export:offered\"\n\
                   \t.ascii\t\" -export:count,data\"\n";
    assert!(text.contains(options), "{text}");
    // Only what the file defines is offered, which is what clang does with the declaration.
    assert!(!text.contains("promised"), "{text}");
    // And nothing at all on a file that offers nothing.
    let plain = listing("plain", "-O0", "int f(void) { return 0; }\n");
    assert!(!plain.contains(".drectve"), "{plain}");
}

/// A COFF object, read just far enough to find its sections by name.
struct Coff {
    bytes: Vec<u8>,
}

impl Coff {
    fn u16_at(&self, at: usize) -> usize {
        usize::from(u16::from_le_bytes([self.bytes[at], self.bytes[at + 1]]))
    }

    fn u32_at(&self, at: usize) -> usize {
        u32::from_le_bytes(self.bytes[at..at + 4].try_into().expect("four bytes")) as usize
    }

    /// Each section's name, its characteristics and its bytes. A name longer than eight bytes is
    /// `/` and where it is in the string table, which starts after the symbol table.
    fn sections(&self) -> Vec<(String, usize, &[u8])> {
        let strings = self.u32_at(8) + 18 * self.u32_at(12);
        let headers = 20 + self.u16_at(16);
        (0..self.u16_at(2))
            .map(|section| {
                let header = headers + section * 40;
                let short = &self.bytes[header..header + 8];
                let short = String::from_utf8_lossy(short).trim_end_matches('\0').to_owned();
                let name = match short.strip_prefix('/') {
                    Some(at) => {
                        let at = strings + at.parse::<usize>().expect("a string table offset");
                        let end = self.bytes[at..].iter().position(|&b| b == 0).expect("an end");
                        String::from_utf8_lossy(&self.bytes[at..at + end]).into_owned()
                    }
                    None => short,
                };
                let (size, start) = (self.u32_at(header + 16), self.u32_at(header + 20));
                (name, self.u32_at(header + 36), &self.bytes[start..start + size])
            })
            .collect()
    }
}

#[test]
fn all_three_are_written_into_an_object() {
    let (ok, out, said) = run("obj", &["--target=x86_64-windows-gnu", "-c"], SOURCE);
    assert!(ok, "{said}");
    let coff = Coff { bytes: out };
    let sections = coff.sections();
    let names: Vec<&str> = sections.iter().map(|(name, _, _)| name.as_str()).collect();

    // IMAGE_SCN_LNK_INFO and IMAGE_SCN_LNK_REMOVE, which is `yn` and what makes the linker read the
    // section and then drop it.
    let (_, flags, bytes) = sections
        .iter()
        .find(|(name, _, _)| name == ".drectve")
        .unwrap_or_else(|| panic!("{names:?}"));
    assert_eq!(flags & 0x0a00, 0x0a00, "{flags:#x}");
    assert_eq!(
        *bytes,
        b" -export:offered -export:count,data",
        "{:?}",
        String::from_utf8_lossy(bytes)
    );

    // IMAGE_SCN_LNK_COMDAT, IMAGE_SCN_CNT_INITIALIZED_DATA and IMAGE_SCN_MEM_READ, and not
    // IMAGE_SCN_MEM_WRITE.
    let (_, flags, bytes) = sections
        .iter()
        .find(|(name, _, _)| name == ".rdata$.refptr.away")
        .unwrap_or_else(|| panic!("{names:?}"));
    assert_eq!(flags & 0x1040, 0x1040, "{flags:#x}");
    assert_eq!(flags & 0x4000_0000, 0x4000_0000, "{flags:#x}");
    assert_eq!(flags & 0x8000_0000, 0, "{flags:#x}");
    assert_eq!(bytes.len(), 8);
    for name in ["mine", "here", "count", "hidden", "unread", "_fmode"] {
        let section = format!(".rdata$.refptr.{name}");
        assert!(!names.contains(&section.as_str()), "{name}: {names:?}");
    }

    // The imported names are only ever named through their pointers, which a long name keeps in
    // the string table.
    let has = |what: &[u8]| coff.bytes.windows(what.len()).any(|window| window == what);
    assert!(has(b"__imp_GetCurrentProcessId\0"));
    assert!(has(b"__imp__fmode\0"));
}
