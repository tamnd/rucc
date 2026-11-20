//! No integer code in the vector registers on Plan 9, end to end.
//!
//! Design: tamnd/rucc#4148, and 09.3 and open question 8 of the rucc-plan9 plan.
//!
//! The kernels of Plan 9 save the vector registers for user processes only, and kencc never puts
//! integer work in them. The kernels link `libsec` and `libc`, so the rule is for each unit of a
//! Plan 9 target and not for the kernel directories alone. Before this rule, the loop of
//! `blake2sblock.c` in `libsec` came out four lanes at a time in `X0`.

use std::path::PathBuf;
use std::process::Command;

/// A loop that `vectorize` does four lanes at a time where the target lets it.
const SOURCE: &str = "unsigned a[32], b[32];\n\
                      void f(void)\n\
                      {\n\
                      \tint j;\n\
                      \tfor(j = 0; j < 32; j++)\n\
                      \t\ta[j] = (a[j] ^ b[j]) * 16777619;\n\
                      }\n";

fn fixture(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-plan9-vectors-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The assembly that the compiler writes for the source on `target`.
fn assembly(target: &str) -> String {
    let path = fixture(target);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-S", "-w", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// True when a line of the assembly names a vector register.
fn names_a_vector_register(asm: &str) -> bool {
    asm.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("%xmm")
            || line.split(|c: char| !c.is_ascii_alphanumeric() && c != '.').any(|word| {
                word.strip_prefix('v')
                    .and_then(|rest| rest.split_once('.'))
                    .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
    })
}

#[test]
fn linux_does_the_loop_in_the_vector_registers() {
    let asm = assembly("x86_64-linux-gnu");
    assert!(
        names_a_vector_register(&asm),
        "the loop is not vectorized, so the test shows nothing:\n{asm}"
    );
}

#[test]
fn plan9_does_the_loop_in_the_integer_registers() {
    for target in ["x86_64-unknown-plan9", "aarch64-unknown-plan9"] {
        let asm = assembly(target);
        assert!(!names_a_vector_register(&asm), "{target} uses a vector register:\n{asm}");
    }
}
