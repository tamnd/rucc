//! The places Windows on AArch64 parts from AAPCS64, each checked in the assembly rucc writes.
//!
//! Every one of them is about a function whose prototype ends in `...`. Such a function is passed
//! everything in the x registers and the stack, named arguments included, and homes x0 to x7 in
//! sixty four bytes it takes at the top of its own frame, so that its `va_list` is a `char *` over
//! one run of words. The instructions checked here are the ones clang for aarch64-w64-mingw32
//! writes, since the other half of every such call is built by clang or by Microsoft's compiler.

use std::path::PathBuf;
use std::process::Command;

const WINDOWS: &str = "aarch64-windows-gnu";

/// The fixture, under a directory of its own so that two of these running at once do not write
/// the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-winarm-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The instructions of the assembly for one fixture, with every directive and label left out.
fn insts(what: &str, source: &str) -> Vec<String> {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={WINDOWS}"))
        .args(["-O1", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.ends_with(':'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_variadic_function_homes_its_x_registers_above_its_frame() {
    let got = insts(
        "callee",
        "#include <stdarg.h>\n\
         double f(double d, ...) { va_list ap; va_start(ap, d); double r = va_arg(ap, double); \
         va_end(ap); return r + d; }\n",
    );
    // The home area is the first thing taken and the last thing given back.
    assert_eq!(got.first().map(String::as_str), Some("sub sp, sp, #64"), "{got:#?}");
    let ret = got.iter().position(|line| line == "ret").expect("a return");
    assert_eq!(got[ret - 1], "add sp, sp, #64", "{got:#?}");
    // The named `double` arrived in x0, and x1 to x7 are the ones the walk reads.
    assert!(
        got.iter().any(|line| line.starts_with("fmov d") && line.ends_with(", x0")),
        "{got:#?}"
    );
    for reg in 1..8 {
        let stored = format!("str x{reg}, [sp, #");
        assert!(got.iter().any(|line| line.starts_with(&stored)), "x{reg} is not homed: {got:#?}");
    }
    // And the walk starts at x1's home, the word after x0's.
    let start = got.iter().position(|line| line == "str x1, [sp, #24]").expect("x1 at sp+24");
    assert!(got[start..].iter().any(|line| line == "add x0, sp, #24"), "{got:#?}");
}

#[test]
fn an_ordinary_function_takes_no_home_area() {
    let got = insts("plain", "double f(double d, long x) { return d + x; }\n");
    assert!(!got.iter().any(|line| line.contains("#64")), "{got:#?}");
}

#[test]
fn a_variadic_call_passes_floats_and_small_structures_in_x_registers() {
    let got = insts(
        "caller",
        "struct F { float a, b, c, d; };\n\
         struct H { double a, b, c, d; };\n\
         void v(int, ...);\n\
         void n(int, double);\n\
         void c(struct F f, struct H h) { v(1, 2.0, f, h); n(1, 2.0); }\n",
    );
    let calls: Vec<usize> = got
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("bl "))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(calls.len(), 2, "{got:#?}");
    let variadic = &got[..calls[0]];
    // 2.0 is its bits in x1, the four floats are x2 and x3, and the four doubles, over sixteen
    // bytes, are the address of a copy in x4. No vector register carries anything.
    assert!(variadic.iter().any(|line| line == "mov x1, #4611686018427387904"), "{variadic:#?}");
    for reg in ["x2", "x3", "x4"] {
        let set = format!(" {reg}, ");
        assert!(variadic.iter().any(|line| line.contains(&set)), "{reg}: {variadic:#?}");
    }
    assert!(!variadic.iter().any(|line| sets_an_argument_vector(line)), "{variadic:#?}");
    // Without the `...` the same `double` is d0.
    let plain = &got[calls[0]..calls[1]];
    assert!(plain.iter().any(|line| sets_an_argument_vector(line)), "{plain:#?}");
}

/// Whether an instruction writes one of the vector registers that carry arguments, v0 to v7 under
/// any of their names. A store names its register first too, but writes memory.
fn sets_an_argument_vector(line: &str) -> bool {
    let Some((op, rest)) = line.split_once(' ') else { return false };
    if op.starts_with("st") {
        return false;
    }
    let dest = rest.split(',').next().unwrap_or("").trim();
    let mut chars = dest.chars();
    matches!(chars.next(), Some('s' | 'd' | 'q' | 'v'))
        && matches!(
            chars.as_str().split('.').next(),
            Some("0" | "1" | "2" | "3" | "4" | "5" | "6" | "7")
        )
}

/// What the compiler says about a fixture it is expected to refuse.
fn refused(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={WINDOWS}"))
        .args(["-O1", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(!out.status.success(), "the compiler took the fixture");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn the_teb_is_read_out_of_x18_as_winnt_h_declares_it() {
    // The declaration and the function are mingw-w64's, from `winnt.h`, and clang writes the one
    // move for it.
    let got = insts(
        "teb",
        "struct _TEB;\n\
         register struct _TEB *__mingw_current_teb __asm__(\"x18\");\n\
         static inline struct _TEB *NtCurrentTeb(void) { return __mingw_current_teb; }\n\
         void *teb(void) { return NtCurrentTeb(); }\n",
    );
    assert_eq!(got, ["mov x0, x18", "ret"]);
}

#[test]
fn x18_is_not_written_and_no_other_register_is_kept_for_the_program() {
    let said =
        refused("write", "register void *teb __asm__(\"x18\");\nvoid set(void *p) { teb = p; }\n");
    assert!(said.contains("a write to or the address of a global register variable"), "{said}");
    let said = refused("x19", "register long g __asm__(\"x19\");\nlong f(void) { return g; }\n");
    assert!(said.contains("'g' is a global register variable"), "{said}");
}
