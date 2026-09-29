//! What a Windows listing tells the unwinder, and that it is the same thing an object says.
//!
//! Windows cannot unwind through a function with no row in `.pdata`, and a structured exception
//! raised in one, such as an integer divided by zero under a `SIGFPE` handler, then ends the
//! program instead of reaching the handler. The object writer has always written the rows. A
//! listing asks the assembler for them with `.seh_` directives, which is what gcc writes, and those
//! have to come out as the same records whether gas or the reader here assembles them.

use std::path::PathBuf;
use std::process::Command;

const TARGET: &str = "--target=x86_64-windows-gnu";

/// A leaf, a function that keeps doubles in callee saved vector registers across calls, and one
/// that pushes registers and takes a frame too big for the short code.
const SHAPES: &str = "\
double g(double);
void bottom(void);
int leaf(int a, int b) { return a + b; }
double keep(double a, double b, double c) { double x = g(a); double y = g(b); return x * a + y * b + c * g(c); }
long big(long a, long b, long c, long d, long e, long f, long n) {
    char buf[5000];
    long s = 0;
    for (long i = 0; i < n; i++) { buf[i % 5000] = (char)i; s += a * b + c * d + e * f + i + buf[(i * 7) % 5000]; }
    bottom();
    return s;
}
";

fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-winunwind-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// What the compiler wrote for one input under these flags.
fn compile(input: &PathBuf, flags: &[&str]) -> Vec<u8> {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(TARGET)
        .args(flags)
        .args(["-o", "-"])
        .arg(input)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
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

    /// The bytes of the section of this name, which is never one of the long names here.
    fn section(&self, name: &str) -> Option<&[u8]> {
        let headers = 20 + self.u16_at(16);
        (0..self.u16_at(2)).find_map(|section| {
            let header = headers + section * 40;
            let short = String::from_utf8_lossy(&self.bytes[header..header + 8]);
            if short.trim_end_matches('\0') != name {
                return None;
            }
            let (size, start) = (self.u32_at(header + 16), self.u32_at(header + 20));
            Some(&self.bytes[start..start + size])
        })
    }
}

/// Every function is wrapped, the leaf included, and each code the object writer records is asked
/// for in gcc's words.
#[test]
fn every_function_gets_seh_directives() {
    let dir = dir("listing");
    let c = dir.join("one.c");
    std::fs::write(&c, SHAPES).expect("the fixture can be written");
    let text = String::from_utf8(compile(&c, &["-O2", "-S"])).expect("a listing is text");
    let _ = std::fs::remove_dir_all(&dir);
    for name in ["leaf", "keep", "big"] {
        assert!(text.contains(&format!("\t.seh_proc\t{name}\n")), "{name}:\n{text}");
    }
    assert_eq!(text.matches(".seh_endprologue").count(), 3, "{text}");
    assert_eq!(text.matches(".seh_endproc").count(), 3, "{text}");
    for code in [".seh_pushreg\t%rbx", ".seh_stackalloc\t", ".seh_savexmm\t%xmm6, "] {
        assert!(text.contains(code), "{code}:\n{text}");
    }
    assert!(!text.contains(".cfi"), "{text}");
}

/// An object assembled from the listing has the same descriptions as the one written directly,
/// and a row for every function, at each level that shapes the prologue differently.
#[test]
fn the_listing_assembles_to_the_same_unwind_table() {
    for opt in ["-O0", "-O2"] {
        let dir = dir(&opt[1..]);
        let c = dir.join("one.c");
        std::fs::write(&c, SHAPES).expect("the fixture can be written");
        let direct = Coff { bytes: compile(&c, &[opt, "-c"]) };
        let s = dir.join("one.s");
        std::fs::write(&s, compile(&c, &[opt, "-S"])).expect("the listing can be written");
        let read = Coff { bytes: compile(&s, &["-c"]) };
        let _ = std::fs::remove_dir_all(&dir);
        let xdata = direct.section(".xdata").expect("a direct object has descriptions");
        assert_eq!(read.section(".xdata"), Some(xdata), "{opt}");
        let pdata = read.section(".pdata").expect("an assembled listing has rows");
        assert_eq!(pdata.len(), 3 * 12, "{opt}");
        assert_eq!(direct.section(".pdata").map(<[u8]>::len), Some(pdata.len()), "{opt}");
    }
}
