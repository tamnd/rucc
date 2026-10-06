//! `-fzero-call-used-regs=` and the `zero_call_used_regs` attribute, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12, tamnd/rucc#2281 and tamnd/rucc#2335.
//!
//! What is compared is the run of clearing instructions in front of each `ret`, against what gcc
//! 16 writes for the same function on x86-64 and gcc 13 on AArch64. The rest of the body is this compiler's own and is not the
//! same as gcc's, so the functions here are ones where both put their values in the same
//! registers.

use std::process::Command;

/// The listing of `source` compiled for `target` Linux at `-O2` with `flags`.
fn asm(target: &str, what: &str, flags: &[&str], source: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-zero-{}-{target}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}-unknown-linux-gnu"))
        .args(["-nostdinc", "-O2", "-S", "-o", "-", "-fno-asynchronous-unwind-tables"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("the listing is text")
}

/// The instructions of one function, with the labels and directives left out and the spaces
/// between an instruction and its operands made one.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.ends_with(':') || line.starts_with(".L"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// The registers cleared in front of the last instruction of one function, in order.
fn cleared(text: &str, name: &str) -> Vec<String> {
    let body = body(text, name);
    let mut found: Vec<String> = body[..body.len() - 1]
        .iter()
        .rev()
        .map_while(|line| {
            let x86 = line.strip_prefix("xorl %").and_then(|rest| rest.split_once(','));
            let a64 = line.strip_prefix("mov ").and_then(|rest| rest.strip_suffix(", #0"));
            x86.map(|(reg, _)| reg.to_string()).or(a64.map(str::to_string))
        })
        .collect();
    found.reverse();
    found
}

const USED: &[&str] = &["-fzero-call-used-regs=used-gpr"];

const SOURCE: &str = "\
long g(long);
long f(long a, long b) { return a * b + 3; }
long callr(long a, long b) { return g(a) + b; }
long six(long a, long b, long c, long d, long e, long f) { return a+b+c+d+e+f; }
long div(long a, long b) { return a / b; }
long mid(long a) { long r = g(a); g(0); return r; }
";

#[test]
fn used_gpr_clears_what_gcc_clears_on_x86_64() {
    let text = asm("x86_64", "used", USED, SOURCE);
    for (name, gcc) in [
        ("f", &["esi", "edi"][..]),
        ("callr", &["esi", "edi"]),
        ("six", &["edx", "ecx", "esi", "edi", "r8d", "r9d"]),
        ("div", &["edx", "esi", "edi"]),
        ("mid", &["edi"]),
    ] {
        assert_eq!(cleared(&text, name), gcc, "{name}: {text}");
        assert_eq!(body(&text, name).last().map(String::as_str), Some("ret"), "{text}");
    }
    let text = asm("x86_64", "none", &[], SOURCE);
    for name in ["f", "callr", "six", "div", "mid"] {
        assert_eq!(cleared(&text, name), Vec::<String>::new(), "{name}: {text}");
    }
}

/// A function that makes a call has used `x16` and `x17`, since a veneer the linker puts between
/// the call and its target may write them.
#[test]
fn used_gpr_clears_what_gcc_clears_on_aarch64() {
    let text = asm("aarch64", "used", USED, SOURCE);
    for (name, gcc) in [
        ("f", &["x1"][..]),
        ("callr", &["x1", "x16", "x17"]),
        ("six", &["x1", "x2", "x3", "x4", "x5"]),
        ("div", &["x1"]),
    ] {
        assert_eq!(cleared(&text, name), gcc, "{name}: {text}");
    }
}

/// `all-gpr` is every register a call may clobber but the one the answer is in. gcc also clears
/// `x18` on AArch64, which this compiler keeps for the platform.
#[test]
fn a_function_that_names_a_choice_gets_that_choice() {
    let source = "\
__attribute__((zero_call_used_regs(\"skip\"))) long s(long a, long b) { return a + b; }
__attribute__((zero_call_used_regs(\"all-gpr\"))) void ag(void) { }
__attribute__((zero_call_used_regs(\"all-gpr-arg\"))) long aga(long a) { return a; }
__attribute__((zero_call_used_regs(\"used-gpr-arg\"))) long uga(long a, long b) { return a*b+7; }
";
    let text = asm("x86_64", "attr", USED, source);
    assert_eq!(cleared(&text, "s"), Vec::<String>::new(), "{text}");
    let all = ["eax", "edx", "ecx", "esi", "edi", "r8d", "r9d", "r10d", "r11d"];
    assert_eq!(cleared(&text, "ag"), all, "{text}");
    assert_eq!(cleared(&text, "aga"), all[1..7], "{text}");
    assert_eq!(cleared(&text, "uga"), ["esi", "edi"], "{text}");
    // The attribute is read when the command line said nothing as well.
    assert_eq!(cleared(&asm("x86_64", "alone", &[], source), "ag"), all);

    let text = asm("aarch64", "attr", USED, source);
    let all: Vec<String> = (0..18).map(|n| format!("x{n}")).collect();
    assert_eq!(cleared(&text, "s"), Vec::<String>::new(), "{text}");
    assert_eq!(cleared(&text, "ag"), all, "{text}");
    assert_eq!(cleared(&text, "aga"), all[1..8], "{text}");
    assert_eq!(cleared(&text, "uga"), ["x1"], "{text}");
}

/// A call that became a jump leaves through the callee's `ret`, and a result nobody reads is not
/// a register the body used. A function that returns nothing jumps to the call it ends with even
/// when that call gives something back, so the second half is checked with sibling calls off.
#[test]
fn a_tail_call_clears_nothing_and_a_thrown_away_result_is_not_cleared() {
    let source = "long g(long); long tail(long a) { return g(a + 1); } \
                  void ign(long a) { g(a); }\n";
    let text = asm("x86_64", "tail", USED, source);
    assert_eq!(body(&text, "tail").last().map(String::as_str), Some("jmp g"), "{text}");
    assert_eq!(cleared(&text, "tail"), Vec::<String>::new(), "{text}");
    assert_eq!(body(&text, "ign").last().map(String::as_str), Some("jmp g"), "{text}");
    assert_eq!(cleared(&text, "ign"), Vec::<String>::new(), "{text}");
    let flags = [USED[0], "-fno-optimize-sibling-calls"];
    let text = asm("x86_64", "ign", &flags, source);
    assert_eq!(cleared(&text, "ign"), ["edi"], "{text}");
}

/// The registers are clear before the return thunk is jumped to, as they are before `ret`.
#[test]
fn the_registers_are_cleared_before_the_return_thunk() {
    let flags = [USED[0], "-mfunction-return=thunk-extern"];
    let text = asm("x86_64", "thunk", &flags, "long f(long a, long b) { return a * b + 3; }\n");
    let body = body(&text, "f");
    assert_eq!(body.last().map(String::as_str), Some("jmp __x86_return_thunk"), "{text}");
    assert_eq!(cleared(&text, "f"), ["esi", "edi"], "{text}");
}

/// The epilogue of one function, from the first clearing instruction to the one before the last:
/// every line that clears a register, a vector register, a mask or the x87 stack, in order.
fn epilogue(text: &str, name: &str) -> Vec<String> {
    let body = body(text, name);
    let clears = |line: &&String| {
        let x86 = ["xorl %", "pxor %", "vxorps %", "vpxord %", "kxorw %", "fldz", "fstp %st(0)"];
        x86.iter().any(|start| line.starts_with(start))
            || line == &"vzeroall"
            || line.starts_with("movi v") && line.ends_with(".2d, #0")
            || line.starts_with("mov x") && line.ends_with(", #0")
    };
    let mut found: Vec<String> =
        body[..body.len() - 1].iter().rev().take_while(clears).cloned().collect();
    found.reverse();
    found
}

/// The functions gcc 16 was asked about, each with what it does with floating point.
const WIDE: &str = "\
double hd(double);
long f(long a, long b) { return a * b + 3; }
double d(double a, double b) { return a * b; }
long mix(long a, double b) { return a + (long)b; }
long double q(long double a, long double b) { return a * b; }
long double ld(long double a) { return a; }
int qi(long double a) { return (int)a; }
double k(double a) { return hd(a) + 1.0; }
";

/// `n` copies of `line`.
fn times(n: usize, line: &str) -> Vec<String> {
    vec![line.to_string(); n]
}

/// The x87 stack cleared but for `kept` registers, as gcc writes it.
fn x87(kept: usize) -> Vec<String> {
    let mut lines = times(8 - kept, "fldz");
    lines.extend(times(8 - kept, "fstp %st(0)"));
    lines
}

fn strings(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| line.to_string()).collect()
}

/// `used` clears the vector registers the body wrote and did not return, and the whole x87 stack
/// when the body put something on it, but never what a call clobbered.
#[test]
fn used_clears_the_vector_registers_and_the_x87_stack_like_gcc_16() {
    let text = asm("x86_64", "wide-used", &["-fzero-call-used-regs=used"], WIDE);
    assert_eq!(epilogue(&text, "f"), ["xorl %esi, %esi", "xorl %edi, %edi"], "{text}");
    assert_eq!(epilogue(&text, "d"), ["pxor %xmm1, %xmm1"], "{text}");
    assert_eq!(epilogue(&text, "q"), x87(1), "{text}");
    assert_eq!(epilogue(&text, "ld"), Vec::<String>::new(), "{text}");
    // What else the two bodies use is their own, but the x87 stack is cleared whole.
    assert_eq!(epilogue(&text, "qi")[..16], x87(0), "{text}");
    let text = asm("x86_64", "wide-used-arg", &["-fzero-call-used-regs=used-arg"], WIDE);
    assert_eq!(epilogue(&text, "d"), ["pxor %xmm1, %xmm1"], "{text}");
    assert_eq!(epilogue(&text, "q"), Vec::<String>::new(), "{text}");
}

/// `all` clears the x87 stack first and then every register a call may clobber, in gcc's order.
#[test]
fn all_clears_every_register_in_the_order_gcc_16_does() {
    let gprs = ["eax", "edx", "ecx", "esi", "edi", "r8d", "r9d", "r10d", "r11d"];
    let every = |skip: &str, kept: usize| {
        let mut lines = x87(kept);
        for (at, gpr) in gprs.iter().enumerate() {
            if *gpr != skip {
                lines.push(format!("xorl %{gpr}, %{gpr}"));
            }
            let vectors = match at {
                4 => 0..8,
                8 => 8..16,
                _ => 0..0,
            };
            for n in vectors.filter(|n| skip != format!("xmm{n}")) {
                lines.push(format!("pxor %xmm{n}, %xmm{n}"));
            }
        }
        lines
    };
    let text = asm("x86_64", "wide-all", &["-fzero-call-used-regs=all"], WIDE);
    assert_eq!(epilogue(&text, "f"), every("eax", 0), "{text}");
    assert_eq!(epilogue(&text, "d"), every("xmm0", 0), "{text}");
    assert_eq!(epilogue(&text, "q"), every("", 1), "{text}");

    let text = asm("x86_64", "wide-all-arg", &["-fzero-call-used-regs=all-arg"], WIDE);
    let mut arg = strings(&["xorl %edx, %edx", "xorl %ecx, %ecx"]);
    arg.extend(strings(&["xorl %esi, %esi", "xorl %edi, %edi"]));
    arg.extend((0..8).map(|n| format!("pxor %xmm{n}, %xmm{n}")));
    arg.extend(strings(&["xorl %r8d, %r8d", "xorl %r9d, %r9d"]));
    assert_eq!(epilogue(&text, "f"), arg, "{text}");
}

/// With AVX the vector registers go all at once with `vzeroall` when none is returned, and
/// AVX-512 adds the upper sixteen and the mask registers. The listing is gcc 16's for the same
/// `-march=`.
#[test]
fn all_uses_vzeroall_and_clears_the_avx_512_registers_like_gcc_16() {
    let flags = ["-fzero-call-used-regs=all", "-march=x86-64-v3"];
    let text = asm("x86_64", "wide-avx", &flags, WIDE);
    let mut f = strings(&["vzeroall"]);
    f.extend(x87(0));
    let rest = ["edx", "ecx", "esi", "edi", "r8d", "r9d", "r10d", "r11d"];
    f.extend(rest.iter().map(|r| format!("xorl %{r}, %{r}")));
    assert_eq!(epilogue(&text, "f"), f, "{text}");
    let d = epilogue(&text, "d");
    assert!(d.contains(&"vxorps %xmm1, %xmm1, %xmm1".to_string()), "{text}");
    assert!(!d.iter().any(|line| line.contains("xmm0") || line == "vzeroall"), "{text}");

    let flags = ["-fzero-call-used-regs=all", "-march=x86-64-v4"];
    let text = asm("x86_64", "wide-avx512", &flags, WIDE);
    let mut f = strings(&["vzeroall"]);
    f.extend((16..32).map(|n| format!("vxorps %xmm{n}, %xmm{n}, %xmm{n}")));
    f.extend(x87(0));
    f.extend(rest.iter().map(|r| format!("xorl %{r}, %{r}")));
    f.extend((0..8).map(|n| format!("kxorw %k{n}, %k{n}, %k{n}")));
    assert_eq!(epilogue(&text, "f"), f, "{text}");
}

/// Without the x87 unit or the vector registers there is nothing of theirs to clear.
#[test]
fn a_unit_without_the_registers_does_not_clear_them() {
    let source = "long f(long a, long b) { return a * b + 3; }\n";
    let flags = ["-fzero-call-used-regs=all", "-mgeneral-regs-only"];
    let text = asm("x86_64", "gpr-only", &flags, source);
    let rest = ["edx", "ecx", "esi", "edi", "r8d", "r9d", "r10d", "r11d"];
    let gprs: Vec<String> = rest.iter().map(|r| format!("xorl %{r}, %{r}")).collect();
    assert_eq!(epilogue(&text, "f"), gprs, "{text}");
}

/// i386 clears `edx` and `ecx` and never a 64-bit register, which it does not have. The vDSO's
/// 32-bit half is built with `-m32` and `used-gpr` in an allmodconfig kernel, and this compiler
/// used to stop there on a register its file did not name.
#[test]
fn i386_clears_its_own_registers_in_gcc_order() {
    let source = "\
int f(int a, int b) { return a * b + 3; }
long long wide(long long a, long long b) { return a + b; }
__attribute__((regparm(3))) int three(int a, int b, int c) { return a + b + c; }
";
    let all = asm("i686", "all-gpr", &["-fzero-call-used-regs=all-gpr"], source);
    assert_eq!(cleared(&all, "f"), ["edx", "ecx"], "{all}");
    assert_eq!(cleared(&all, "wide"), ["ecx"], "{all}");
    assert_eq!(cleared(&all, "three"), ["edx", "ecx"], "{all}");
    let used = asm("i686", "used", USED, source);
    assert_eq!(cleared(&used, "three"), ["edx", "ecx"], "{used}");
    for name in ["f", "wide"] {
        let found = cleared(&used, name);
        let order: Vec<&str> =
            ["edx", "ecx"].into_iter().filter(|r| found.iter().any(|f| f == r)).collect();
        assert_eq!(found, order, "{used}");
    }
}

/// AArch64 clears `v0` to `v7` and `v16` to `v31` after the general purpose registers, and never
/// the callee saved `v8` to `v15`.
#[test]
fn all_clears_the_vector_registers_on_aarch64_like_gcc() {
    let text = asm("aarch64", "wide-used", &["-fzero-call-used-regs=used"], WIDE);
    assert_eq!(epilogue(&text, "d"), ["movi v1.2d, #0"], "{text}");
    let text = asm("aarch64", "wide-all", &["-fzero-call-used-regs=all"], WIDE);
    let mut f: Vec<String> = (1..18).map(|n| format!("mov x{n}, #0")).collect();
    f.extend((0..8).chain(16..32).map(|n| format!("movi v{n}.2d, #0")));
    assert_eq!(epilogue(&text, "f"), f, "{text}");
    let text = asm("aarch64", "wide-all-arg", &["-fzero-call-used-regs=all-arg"], WIDE);
    let mut f: Vec<String> = (1..8).map(|n| format!("mov x{n}, #0")).collect();
    f.extend((0..8).map(|n| format!("movi v{n}.2d, #0")));
    assert_eq!(epilogue(&text, "f"), f, "{text}");
}

/// Every choice gcc 16 has is taken, so `__has_attribute` can answer as gcc does.
#[test]
fn every_choice_of_the_attribute_is_taken() {
    let mut source = String::new();
    for (at, choice) in ["used", "used-arg", "all", "all-arg"].iter().enumerate() {
        source.push_str(&format!(
            "__attribute__((zero_call_used_regs(\"{choice}\"))) long u{at}(long a) {{ return a; }}\n"
        ));
    }
    let text = asm("x86_64", "every", &[], &source);
    assert_eq!(epilogue(&text, "u0"), ["xorl %edi, %edi"], "{text}");
    assert!(epilogue(&text, "u2").contains(&"pxor %xmm15, %xmm15".to_string()), "{text}");
}
