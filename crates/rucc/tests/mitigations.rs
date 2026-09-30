//! The speculation mitigations a kernel builds with, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12, and tamnd/rucc#2280.
//!
//! The flags are read by the driver and the branches are rewritten by the last pass of the back
//! end, so a test of either alone can be green while the flag does nothing. What is read here is
//! the listing, which is what the kernel's objtool would complain about if a branch were left
//! plain.

use std::process::Command;

/// Every mitigation a `defconfig` with retpolines, return thunks and SLS hardening turns on.
const ALL: &[&str] = &[
    "-mindirect-branch=thunk-extern",
    "-mindirect-branch-register",
    "-mindirect-branch-cs-prefix",
    "-mfunction-return=thunk-extern",
    "-mharden-sls=all",
];

/// The listing of `source` compiled for x86-64 Linux at `-O2` with `flags`.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-mitigate-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-nostdinc", "-O2", "-S", "-o", "-"])
        .arg("-fno-asynchronous-unwind-tables")
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("the listing is text")
}

/// The instructions of one function, with the labels and directives left out.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.ends_with(':') || line.starts_with(".L"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.'))
        .collect()
}

#[test]
fn an_indirect_call_goes_through_the_thunk_and_a_return_through_the_return_thunk() {
    let text = asm("call", ALL, "int call(int (*p)(int), int x) { return p(x) + 1; }\n");
    let body = body(&text, "call");
    assert!(body.contains(&"call\t__x86_indirect_thunk_rax"), "{text}");
    assert_eq!(body.last(), Some(&"jmp\t__x86_return_thunk"), "{text}");
    assert!(!text.contains("*%") && !text.contains("\tret"), "{text}");
}

/// The thunks through `r8` to `r15` are a byte longer, and `cs` makes every one of them six, so
/// that the kernel can write the plain branch over any of them in place.
#[test]
fn a_thunk_through_a_high_register_gets_the_cs_prefix() {
    let source = "long six(long (*p)(long, long, long, long, long, long), long a, long b, long c, \
                  long d, long e) { return p(a, b, c, d, e, a) + p(b, c, d, e, a, b); }\n";
    let text = asm("cs", ALL, source);
    assert!(text.contains("cs call\t__x86_indirect_thunk_r"), "{text}");
    let plain: Vec<&str> = ALL.iter().copied().filter(|it| !it.ends_with("cs-prefix")).collect();
    let text = asm("no-cs", &plain, source);
    assert!(text.contains("\tcall\t__x86_indirect_thunk_r") && !text.contains("cs call"), "{text}");
}

#[test]
fn an_indirect_jump_goes_through_the_thunk_with_int3_after_it() {
    let source = "int go(int x) { static void *where[] = { &&a, &&b }; goto *where[x & 1]; \
                  a: return 1; b: return 2; }\n";
    let text = asm("goto", ALL, source);
    let body = body(&text, "go");
    let at = body.iter().position(|line| *line == "jmp\t__x86_indirect_thunk_rax");
    let at = at.unwrap_or_else(|| panic!("{text}"));
    assert_eq!(body.get(at + 1), Some(&"int3"), "{text}");
}

/// A `switch` a table would suit is a jump through a register, which the thunk would not see, so
/// under the retpoline it is compares, as it is for gcc, and `-fno-jump-tables` asks for the same.
#[test]
fn a_switch_is_not_a_table_under_the_retpoline_or_when_asked() {
    let cases: String =
        (0..16).map(|at| format!("case {at}: g({}); break; ", at * 7 + 3)).collect();
    let source = format!("void g(int); void sw(int x) {{ switch (x) {{ {cases}}} }}\n");
    let source = source.as_str();
    let table = |text: &str| text.contains("jmp\t*") || text.contains("indirect_thunk");
    assert!(table(&asm("table", &[], source)));
    assert!(!table(&asm("thunk", ALL, source)));
    assert!(!table(&asm("asked", &["-fno-jump-tables"], source)));
}

#[test]
fn sls_hardening_alone_puts_int3_after_every_return() {
    let text = asm("sls", &["-mharden-sls=return"], "int one(int x) { return x + 1; }\n");
    let body = body(&text, "one");
    assert_eq!(body[body.len() - 2..], ["ret", "int3"], "{text}");
    let text =
        asm("none", &["-mharden-sls=all", "-mharden-sls=none"], "int one(int x) { return x; }\n");
    assert!(!text.contains("int3"), "{text}");
}

/// The kernel writes these on code that runs before the thunks can, and gcc then leaves the branch
/// plain. `-mharden-sls=` still puts `int3` after it, since that asks for something else.
#[test]
fn a_function_that_says_keep_is_left_as_it_was() {
    let source = "\
__attribute__((function_return(\"keep\"))) int keepret(int x) { return x + 1; }
__attribute__((indirect_branch(\"keep\"))) int keepind(int (*p)(int)) { return p(1) + 1; }
int plain(int (*p)(int)) { return p(1) + 1; }
";
    let text = asm("keep", ALL, source);
    let keepret = body(&text, "keepret");
    assert_eq!(keepret[keepret.len() - 2..], ["ret", "int3"], "{text}");
    let keepind = body(&text, "keepind");
    assert!(keepind.contains(&"call\t*%rax"), "{text}");
    assert_eq!(keepind.last(), Some(&"jmp\t__x86_return_thunk"), "{text}");
    let plain = body(&text, "plain");
    assert!(plain.contains(&"call\t__x86_indirect_thunk_rax"), "{text}");
}
