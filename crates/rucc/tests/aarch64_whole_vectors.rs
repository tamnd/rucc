//! GNU vectors of sixteen bytes on AArch64, which are kept whole in a `v` register while they are
//! worked on rather than taken a lane at a time through memory. A load and a store are one `ldr q`
//! and one `str q`, and an add, a subtract, a bitwise operator, a shift by a constant, a lane
//! compare and a broadcast are one instruction each, at each of the four integer lane widths, and
//! so are `-v` and `~v`. How a vector is passed to a function and returned is not part of this,
//! and nothing here tests it: AAPCS64 says a `v` register, which this compiler does not do yet.
//! Under `-mgeneral-regs-only` there is no `v` register, and every operator is a lane at a time as
//! before. See tamnd/rucc#3775.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

/// One function for each operator, over each integer lane type of sixteen bytes.
const OPERATORS: &str = "\
typedef unsigned char v16 __attribute__((vector_size(16)));
typedef signed char v16s __attribute__((vector_size(16)));
typedef unsigned short v8u __attribute__((vector_size(16)));
typedef short v8 __attribute__((vector_size(16)));
typedef unsigned int v4u __attribute__((vector_size(16)));
typedef int v4 __attribute__((vector_size(16)));
typedef unsigned long long v2u __attribute__((vector_size(16)));
typedef long long v2 __attribute__((vector_size(16)));
v16 x(v16 a, v16 b) { return a ^ b; }
void ld(v16 *p, v16 *q) { *p ^= *q; }
void f(v16 *p, v16 *q, int n) { v16 a = *p; for (int i = 0; i < n; i++) a = (a << 1) ^ q[i]; *p = a; }
void add16(v16 *p, v16 *q) { *p += *q; }
void sub8(v8 *p, v8 *q) { *p -= *q; }
void and4(v4 *p, v4 *q) { *p &= *q; }
void or2(v2 *p, v2 *q) { *p |= *q; }
void add2(v2 *p, v2 *q) { *p += *q; }
void shl8(v8 *p) { *p = *p << 3; }
void shr16u(v16 *p) { *p = *p >> 7; }
void shr16s(v16s *p) { *p = *p >> 7; }
void shr4u(v4u *p) { *p = *p >> 31; }
void shr2s(v2 *p) { *p = *p >> 1; }
void lt16s(v16s *p, v16s *q) { *p = *p < *q; }
void gt8(v8 *p, v8 *q) { *p = *p > *q; }
void eq4(v4 *p, v4 *q) { *p = *p == *q; }
void le4u(v4u *p, v4u *q) { *p = (v4u)(*p <= *q); }
void ge2u(v2u *p, v2u *q) { *p = (v2u)(*p >= *q); }
void dup16(v16 *p, unsigned char c) { *p ^= (v16){0} + c; }
void dup4(v4 *p, int c) { *p += c; }
void neg16(v16s *p) { *p = -*p; }
void neg4(v4 *p) { *p = -*p; }
void neg2(v2 *p) { *p = -*p; }
void not16(v16 *p) { *p = ~*p; }
void not8(v8 *p) { *p = ~*p; }
";

/// The intrinsics of `<arm_neon.h>` that are the C operators on a whole vector.
const INTRINSICS: &str = "\
#include <arm_neon.h>
void veor(uint8_t *p, const uint8_t *q) { vst1q_u8(p, veorq_u8(vld1q_u8(p), vld1q_u8(q))); }
void vand(uint8_t *p, const uint8_t *q) { vst1q_u8(p, vandq_u8(vld1q_u8(p), vld1q_u8(q))); }
void vadd(uint32_t *p, const uint32_t *q) { vst1q_u32(p, vaddq_u32(vld1q_u32(p), vld1q_u32(q))); }
void vshl(uint8_t *p) { vst1q_u8(p, vshlq_n_u8(vld1q_u8(p), 1)); }
void vshr(int8_t *p) { vst1q_s8(p, vshrq_n_s8(vld1q_s8(p), 7)); }
void vshr8(uint8_t *p) { vst1q_u8(p, vshrq_n_u8(vld1q_u8(p), 8)); }
void vdup(uint8_t *p, uint8_t c) { vst1q_u8(p, veorq_u8(vld1q_u8(p), vdupq_n_u8(c))); }
";

/// The output of rucc for `source` on stdin, with `-S` to stdout at `-O2` on aarch64-linux-gnu.
fn assembly(source: &str, flags: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=aarch64-linux-gnu", "-O2", "-x", "c", "-", "-S", "-o", "-"])
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(source.as_bytes()).unwrap();
    child.wait_with_output().expect("the compiler finished")
}

/// The `-S` text of `source`, which has to compile.
fn text(source: &str, flags: &[&str]) -> String {
    let out = assembly(source, flags);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// The instructions of the function `name` in the `-S` text, one for each line.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let start = format!("{name}:");
    let lines: Vec<&str> = text
        .lines()
        .skip_while(|line| *line != start)
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with(".size"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.ends_with(':'))
        .collect();
    assert!(!lines.is_empty(), "no function {name} in\n{text}");
    lines
}

/// The mnemonic of an instruction line.
fn mnemonic(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// An instruction line with the number of every register taken out and one space after the
/// mnemonic, so that `eor v2.16b, v0.16b, v1.16b` is `eor v.16b, v.16b, v.16b` whichever
/// registers the allocator picked, and a check can still name the mnemonic and every arrangement.
fn shape(line: &str) -> String {
    let line = line.trim();
    let (head, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let mut out = String::new();
    for c in rest.trim_start().chars() {
        // A digit straight after a register letter that starts a word is the register's number.
        let mut back = out.chars().rev();
        let (last, before) = (back.next(), back.next());
        let register = matches!(last, Some('v' | 'q' | 'x' | 'w' | 'd' | 's' | 'h' | 'b'))
            && !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
        if !(c.is_ascii_digit() && register) {
            out.push(c);
        }
    }
    format!("{head} {out}")
}

/// Every instruction of the function `name`, as [`shape`] writes it, one for each line.
fn shapes(text: &str, name: &str) -> Vec<String> {
    body(text, name).into_iter().map(shape).collect()
}

/// Whether an instruction names a register of the other file, as a vector or as a scalar.
fn names_a_vector_register(line: &str) -> bool {
    line.split(|c: char| !c.is_ascii_alphanumeric() && c != '.').skip(1).any(|word| {
        let mut chars = word.chars();
        matches!(chars.next(), Some('v' | 'q' | 'd' | 's' | 'h' | 'b'))
            && chars.next().is_some_and(|c| c.is_ascii_digit())
            && word[1..].split('.').next().is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
    })
}

#[test]
fn each_operator_is_one_instruction_on_a_v_register() {
    let text = text(OPERATORS, &[]);
    for (name, want) in [
        ("x", &["eor v.16b, v.16b, v.16b"][..]),
        ("ld", &["ldr q, [x", "eor v.16b, v.16b, v.16b", "str q, [x"]),
        ("f", &["shl v.16b, v.16b, #1", "eor v.16b, v.16b, v.16b", "ldr q, [x", "str q, [x"]),
        ("add16", &["add v.16b, v.16b, v.16b", "ldr q, [x", "str q, [x"]),
        ("sub8", &["sub v.8h, v.8h, v.8h"]),
        ("and4", &["and v.16b, v.16b, v.16b"]),
        ("or2", &["orr v.16b, v.16b, v.16b"]),
        ("add2", &["add v.2d, v.2d, v.2d"]),
        ("shl8", &["shl v.8h, v.8h, #3"]),
        ("shr16u", &["ushr v.16b, v.16b, #7"]),
        ("shr16s", &["sshr v.16b, v.16b, #7"]),
        ("shr4u", &["ushr v.4s, v.4s, #31"]),
        ("shr2s", &["sshr v.2d, v.2d, #1"]),
        ("lt16s", &["cmgt v.16b, v.16b, v.16b"]),
        ("gt8", &["cmgt v.8h, v.8h, v.8h"]),
        ("eq4", &["cmeq v.4s, v.4s, v.4s"]),
        ("le4u", &["cmhs v.4s, v.4s, v.4s"]),
        ("ge2u", &["cmhs v.2d, v.2d, v.2d"]),
        ("dup16", &["dup v.16b, w", "eor v.16b, v.16b, v.16b"]),
        ("dup4", &["dup v.4s, w", "add v.4s, v.4s, v.4s"]),
        ("neg16", &["neg v.16b, v.16b"]),
        ("neg4", &["neg v.4s, v.4s"]),
        ("neg2", &["neg v.2d, v.2d"]),
        ("not16", &["not v.16b, v.16b"]),
        ("not8", &["not v.16b, v.16b"]),
    ] {
        let lines = body(&text, name);
        let joined = lines.join("\n");
        let shaped = shapes(&text, name);
        for want in want {
            assert!(
                shaped.iter().any(|line| line.starts_with(want)),
                "{name} has no {want}:\n{joined}"
            );
        }
        for line in &lines {
            assert!(
                !matches!(mnemonic(line), "ldrb" | "strb" | "ldrh" | "strh"),
                "{name} goes a lane at a time:\n{joined}"
            );
        }
        // One instruction for each operator, so nothing near a loop over sixteen lanes.
        assert!(lines.len() < 24, "{name} is {} instructions:\n{joined}", lines.len());
    }
}

/// `-v` and `~v` need no constant: a `neg` or a `not` reads only the vector, so there is no
/// `movi` of zero or of all ones in front of a `sub` or an `eor`.
#[test]
fn a_negation_and_a_complement_build_no_constant() {
    let text = text(OPERATORS, &[]);
    for name in ["neg16", "neg4", "neg2", "not16", "not8"] {
        let lines = body(&text, name);
        let joined = lines.join("\n");
        for line in &lines {
            assert!(
                !matches!(mnemonic(line), "movi" | "sub" | "eor" | "dup"),
                "{name} builds a constant:\n{joined}"
            );
        }
    }
}

#[test]
fn the_neon_intrinsics_that_are_operators_are_one_instruction_each() {
    let text = text(INTRINSICS, &[]);
    for (name, want) in [
        ("veor", &["ldr q, [x", "eor v.16b, v.16b, v.16b", "str q, [x"][..]),
        ("vand", &["and v.16b, v.16b, v.16b"]),
        ("vadd", &["add v.4s, v.4s, v.4s"]),
        ("vshl", &["shl v.16b, v.16b, #1"]),
        ("vshr", &["sshr v.16b, v.16b, #7"]),
        // A shift right by the whole width of the lane, which `ushr` cannot say in one, is a
        // shift by one less and then one more.
        ("vshr8", &["ushr v.16b, v.16b, #7", "ushr v.16b, v.16b, #1"]),
        ("vdup", &["dup v.16b, w", "eor v.16b, v.16b, v.16b"]),
    ] {
        let lines = body(&text, name);
        let joined = lines.join("\n");
        let shaped = shapes(&text, name);
        for want in want {
            assert!(
                shaped.iter().any(|line| line.starts_with(want)),
                "{name} has no {want}:\n{joined}"
            );
        }
        for line in &lines {
            assert!(!matches!(mnemonic(line), "ldrb" | "strb"), "{name} loops:\n{joined}");
        }
        assert!(lines.len() < 16, "{name} is {} instructions:\n{joined}", lines.len());
    }
}

#[test]
fn without_the_vector_registers_no_v_register_is_named() {
    for source in [OPERATORS, INTRINSICS] {
        let text = text(source, &["-mgeneral-regs-only"]);
        for line in text.lines().map(str::trim) {
            if line.starts_with('.') || line.ends_with(':') {
                continue;
            }
            assert!(!names_a_vector_register(line), "{line} under -mgeneral-regs-only");
        }
    }
}
