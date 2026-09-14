//! Which registers an aggregate holding a `_Float128` arrives in, end to end.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.2 and `spec/10-backend.md` section 10.8.
//!
//! `crates/rucc-abi` has the classification and its own tests, and this is a test of the whole
//! compiler for the reason `cf_protection.rs` beside it is one: the classification is an answer
//! about registers that three other layers have to carry. The front end has to hand the shape
//! over with the member's format on it, the lowering has to turn one slot into one IR value of a
//! type the machine has a register for, and the back end has to place that value. A wrong answer
//! in any of them is the same wrong program, and the only place all three are visible at once is
//! the assembly.
//!
//! The psABI classifies the two eightbytes of a `_Float128` as SSE and SSEUP, which name one
//! vector register between them rather than two. Spending two is the failure this is here to
//! catch, and it is a quiet one: the argument itself still arrives, because the caller wrote its
//! low half where the callee looks for it, and what breaks is every argument after it, which the
//! caller put one register further along than the callee reads.
//!
//! The expected registers are gcc 13's, from the same source at -O2 on x86-64 Linux:
//!
//! ```text
//! take_one:   endbr64; ret
//! make_one:   endbr64; ret
//! after_one:  endbr64; movapd %xmm1, %xmm0; ret
//! take_mixed: endbr64; movq %rdi, %rax; ret
//! take_both:  endbr64; ret
//! ninth:      endbr64; movdqa 8(%rsp), %xmm0; ret
//! tenth:      endbr64; movdqa 24(%rsp), %xmm0; ret
//! ```

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because the register names below
/// are one machine's.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Seven functions, each of which pins one place the classification has to choose.
///
/// `after_one` is the one that matters most. The `double` behind the aggregate is in the first
/// vector register the aggregate did not take, so where it arrives says how many of them the
/// aggregate spent, which a function taking the aggregate on its own cannot say.
///
/// The two unions are the post merge rule. In `mixed` the `long` makes the first eightbyte
/// INTEGER, which leaves the top of the `_Float128` above it with nothing to be the continuation
/// of, and the psABI turns that back into an ordinary vector register. In `both` the first
/// eightbyte stays SSE and the pair is one register holding the whole value.
///
/// The last two are about the other end of the convention, which is what happens once the vector
/// registers have run out. There are eight of them, so the ninth quad and the tenth are in the
/// caller's argument area, and where the tenth is says how much room the ninth was given.
const SOURCE: &str = "\
struct one { _Float128 x; };
union mixed { _Float128 q; long a; };
union both { _Float128 q; double d; };

_Float128 take_one(struct one v) { return v.x; }
struct one make_one(_Float128 x) { struct one v; v.x = x; return v; }
double after_one(struct one v, double d) { return d; }
long take_mixed(union mixed v) { return v.a; }
_Float128 take_both(union both v) { return v.q; }

_Float128 ninth(_Float128 a, _Float128 b, _Float128 c, _Float128 d, _Float128 e,
                _Float128 f, _Float128 g, _Float128 h, _Float128 i) { return i; }
_Float128 tenth(_Float128 a, _Float128 b, _Float128 c, _Float128 d, _Float128 e,
                _Float128 f, _Float128 g, _Float128 h, _Float128 i, _Float128 j) { return j; }
";

/// The fixture, under a directory of its own so that two of these running at once do not write
/// the same file and neither one deletes the other's.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-quad-abi-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for the fixture at that optimization level.
fn asm(what: &str, level: &str) -> String {
    let path = fixture(&format!("{what}{level}"));
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The lines of one function that are instructions, which is everything that is not a label and
/// not something said to the assembler.
fn insts<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("{name}:");
    let close = format!("\t.size\t{name},");
    text.lines()
        .skip_while(|line| **line != open)
        .take_while(|line| !line.starts_with(&close))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with('.') && !line.ends_with(':'))
        .collect()
}

/// Whether any instruction of that function names that register.
fn names(text: &str, function: &str, register: &str) -> bool {
    insts(text, function).iter().any(|line| line.contains(register))
}

/// A sixteen byte floating point value travels in one vector register and not in two.
///
/// Asked of both directions, because the argument and the return value are classified by the
/// same rule and placed by different code.
#[test]
fn an_aggregate_holding_a_quad_travels_in_one_vector_register() {
    for level in ["-O0", "-O2"] {
        let text = asm("one", level);
        for function in ["take_one", "make_one"] {
            assert!(names(&text, function, "%xmm0"), "{level} {function}:\n{text}");
            assert!(
                !names(&text, function, "%xmm1"),
                "{level} {function} spent a second vector register:\n{text}"
            );
        }
    }
}

/// And the argument behind it gets the register it did not take.
///
/// This is the assertion the whole file is for. A compiler that classified the aggregate as two
/// eightbytes of SSE would put the `double` in `%xmm2`, the caller would still put it in `%xmm1`,
/// and the callee would return whatever the caller happened to leave behind.
#[test]
fn the_argument_behind_a_quad_arrives_in_the_next_vector_register() {
    for level in ["-O0", "-O2"] {
        let text = asm("after", level);
        assert!(names(&text, "after_one", "%xmm1"), "{level}:\n{text}");
        assert!(
            !names(&text, "after_one", "%xmm2"),
            "{level}: the aggregate ahead of it spent two registers:\n{text}"
        );
    }
}

/// An upper half with something other than its own lower half under it is a register of its own.
///
/// `union mixed` is one general purpose register and one vector register, which is the post merge
/// rule stated as an answer. Without the rule the classification is a lone SSEUP, and a
/// classification that read it as the top of a sixteen byte value would pass the whole union in
/// `%xmm0` and lose the `long` the callee reads out of `%rdi`.
#[test]
fn an_upper_half_with_nothing_under_it_gets_a_register_of_its_own() {
    for level in ["-O0", "-O2"] {
        let text = asm("mixed", level);
        assert!(names(&text, "take_mixed", "%rdi"), "{level}: the integer eightbyte:\n{text}");
        assert!(names(&text, "take_mixed", "%xmm0"), "{level}: the one above it:\n{text}");
        assert!(
            !names(&text, "take_mixed", "%rsi"),
            "{level}: two general purpose registers is the __int128 answer:\n{text}"
        );

        // The same union with a `double` in it instead keeps both eightbytes in the vector file,
        // so it is the one register the quad on its own gets.
        assert!(names(&text, "take_both", "%xmm0"), "{level}:\n{text}");
        assert!(!names(&text, "take_both", "%rdi"), "{level}:\n{text}");
        assert!(!names(&text, "take_both", "%xmm1"), "{level}:\n{text}");
    }
}

/// A quad the vector registers ran out before takes two words of the argument area and not one.
///
/// There are eight vector registers, so the ninth of these is the first one in the caller's area
/// and the tenth is the one that says how much room it was given. gcc 13 reads them at `8(%rsp)`
/// and `24(%rsp)`, which is sixteen bytes apart and each of them on a sixteen byte boundary.
///
/// A word apart is the failure, and it is not a wrong value. The tenth would sit on top of the
/// upper half of the ninth, and what reads either of them is a `movaps`, which faults on an
/// address that is not a multiple of sixteen rather than being slow about it. The signature corpus
/// found this by segfaulting in `q_quads_past_the_registers` on both sides of the call.
///
/// Only at -O2, because the frame a lower level builds moves both numbers by its own size, and
/// what is being asserted here is where the caller's area is rather than where this function put
/// anything of its own.
#[test]
fn a_quad_past_the_last_vector_register_gets_two_words_of_the_argument_area() {
    let text = asm("stack", "-O2");
    assert!(names(&text, "ninth", "8(%rsp)"), "the first one in the area:\n{text}");
    assert!(!names(&text, "ninth", "16(%rsp)"), "it starts at the bottom of the area:\n{text}");
    assert!(names(&text, "tenth", "24(%rsp)"), "sixteen bytes above the ninth:\n{text}");
    assert!(
        !names(&text, "tenth", "16(%rsp)"),
        "one word above the ninth is the top half of the ninth:\n{text}"
    );
}
