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

/// A function with `__builtin_setjmp` in it keeps every register Windows x64 asks a function to
/// keep, which is RSI and RDI as well as the ones SysV keeps, and XMM6 to XMM15, and tells the
/// unwinder where each went. The restore puts back only the stack pointer and the frame pointer,
/// so it is this function's own epilogue that hands its caller the rest back after control comes
/// back to it. Postgres builds its error handling on the pair on MinGW64, through a buffer of five
/// `intptr_t`. tamnd/rucc#1993.
#[test]
fn a_function_that_saves_a_place_keeps_every_register_windows_asks_it_to() {
    let source = "\
typedef long long sigjmp_buf[5];
sigjmp_buf *stack;
int f(void) { return __builtin_setjmp(*stack); }
void g(void) { __builtin_longjmp(*stack, 1); }
";
    for opt in ["-O0", "-O2"] {
        let dir = dir(&format!("setjmp{opt}"));
        let c = dir.join("one.c");
        std::fs::write(&c, source).expect("the fixture can be written");
        let listing = compile(&c, &[opt, "-S"]);
        let direct = Coff { bytes: compile(&c, &[opt, "-c"]) };
        let s = dir.join("one.s");
        std::fs::write(&s, &listing).expect("the listing can be written");
        let read = Coff { bytes: compile(&s, &["-c"]) };
        let _ = std::fs::remove_dir_all(&dir);
        let text = String::from_utf8(listing).expect("a listing is text");
        let f = text.split_once("\t.seh_proc\tf\n").expect("f is wrapped").1;
        let f = f.split_once(".seh_endproc").expect("and the wrapping ends").0;
        for reg in ["%rbx", "%rsi", "%rdi", "%r12", "%r13", "%r14", "%r15"] {
            assert!(f.contains(&format!("\t.seh_pushreg\t{reg}\n")), "{opt} {reg}:\n{text}");
            assert!(f.contains(&format!("\tpopq\t{reg}\n")), "{opt} {reg}:\n{text}");
        }
        for n in 6..=15 {
            let reg = format!("%xmm{n}");
            assert!(f.contains(&format!("\t.seh_savexmm\t{reg}, ")), "{opt} {reg}:\n{text}");
            let restored = f.lines().any(|line| {
                line.starts_with("\tmov")
                    && line.contains("(%r")
                    && line.ends_with(&format!(", {reg}"))
            });
            assert!(restored, "{opt} {reg} is put back:\n{text}");
        }
        // The two words the restore puts back, out of the buffer the save wrote them into.
        assert!(f.contains("\tmovq\t%rbp, ("), "{opt}:\n{text}");
        assert!(f.contains("\tmovq\t%rsp, 16("), "{opt}:\n{text}");
        let g = text.split_once("\t.seh_proc\tg\n").expect("g is wrapped").1;
        let jump = g.find("\tjmp\t*%").unwrap_or_else(|| panic!("{opt} an indirect jump:\n{text}"));
        // A register copied into each, which is neither the prologue's `movq %rsp, %rbp` nor the
        // epilogue's `movq %rbp, %rsp`, ahead of the jump.
        let back = |into: &str, not: &str| {
            g[..jump].lines().any(|line| {
                line.starts_with("\tmovq\t%")
                    && line.ends_with(&format!(", {into}"))
                    && !line.starts_with(&format!("\tmovq\t{not},"))
            })
        };
        assert!(back("%rsp", "%rbp"), "{opt} the stack goes back first:\n{text}");
        assert!(back("%rbp", "%rsp"), "{opt} and so does the frame:\n{text}");
        // And what the listing tells the unwinder is what the object says.
        let xdata = direct.section(".xdata").expect("a direct object has descriptions");
        assert_eq!(read.section(".xdata"), Some(xdata), "{opt}");
    }
}
