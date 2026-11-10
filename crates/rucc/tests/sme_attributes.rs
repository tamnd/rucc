//! The AArch64 SME keyword attributes of gcc 14, end to end. They are part of the function type,
//! so a declaration that disagrees is conflicting types and a pointer that disagrees is
//! incompatible. This compiler builds AArch64 code without SME, so a definition that runs in
//! streaming mode or has SME state is refused, and so is a call that needs streaming mode or the
//! caller's state, in the words gcc built without `+sme` uses. tamnd/rucc#4225.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const TARGETS: [&str; 3] =
    ["aarch64-unknown-freebsd15.1", "aarch64-unknown-netbsd10.2", "aarch64-unknown-linux-gnu"];

/// A directory of this test's own, empty. The tests in this file run on threads of one process,
/// so the process id alone would give two of them the same one.
fn dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-sme-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

fn run_in(dir: &Path, target: &str, source: &str) -> (bool, String) {
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-fgnuc-version=14.2.0", "-std=gnu17", "-Wall", "-Wextra", "-S", "-o", "-"])
        .arg("a.c")
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn run(target: &str, source: &str) -> (bool, String) {
    run_in(&dir(), target, source)
}

/// What a header written for gcc 14 declares compiles quietly: the declarations, a pointer and a
/// typedef of each kind, and a `__arm_streaming_compatible` function defined and called, since
/// without SME the processor is never in streaming mode and such a function is an ordinary one.
#[test]
fn the_sme_attributes_are_quiet_in_declarations() {
    let source = "#if !defined __ARM_STATE_ZA || !defined __ARM_STATE_ZT0\n\
        #error no SME state macros\n\
        #endif\n\
        void s(void) __arm_streaming;\n\
        void sc(void) __arm_streaming_compatible;\n\
        void z(void) __arm_inout(\"za\");\n\
        void k(void) __arm_in(\"za\", \"zt0\") __arm_streaming;\n\
        void p(void) __arm_preserves(\"za\");\n\
        void o(void) __arm_out(\"zt0\");\n\
        __arm_new(\"za\") void n(void);\n\
        __arm_locally_streaming void l(void);\n\
        void (*ps)(void) __arm_streaming = s;\n\
        typedef void za_fn(void) __arm_inout(\"za\");\n\
        za_fn *pz = z;\n\
        void scdef(void) __arm_streaming_compatible { sc(); }\n\
        int call_sc(void) { sc(); scdef(); return 0; }\n";
    for target in TARGETS {
        let (ok, err) = run(target, source);
        assert!(ok, "{target}: {err}");
        assert_eq!(err, "", "{target}");
    }
}

/// The attributes make another type, as gcc makes it, so the mode and the state of a function
/// are kept by every declaration of it and by every pointer to it.
#[test]
fn an_sme_function_is_another_type() {
    let target = TARGETS[0];
    let cases = [
        ("void f(void) __arm_streaming;\nvoid f(void);\n", "conflicting types for 'f'"),
        (
            "void f(void) __arm_streaming_compatible;\nvoid f(void) __arm_streaming;\n",
            "conflicting types for 'f'",
        ),
        ("void f(void) __arm_out(\"za\");\nvoid f(void) __arm_inout(\"za\");\n", "conflicting"),
        ("void q(void);\nvoid (*p)(void) __arm_streaming = q;\n", "incompatible pointer type"),
        ("void q(void) __arm_in(\"za\");\nvoid (*p)(void) = q;\n", "incompatible pointer type"),
    ];
    for (source, said) in cases {
        let (_, err) = run(target, source);
        assert!(err.contains(said), "{source}: {err}");
    }
    // The same declaration twice is the same type.
    let (ok, err) = run(target, "void f(void) __arm_in(\"za\");\nvoid f(void) __arm_in(\"za\");\n");
    assert!(ok && err.is_empty(), "{err}");
}

/// A definition that needs SME is refused, with gcc's words for each kind, and a call that
/// needs streaming mode or the caller's state is refused too. A call in a definition that is
/// already refused gets nothing more, as in gcc.
#[test]
fn what_needs_sme_to_build_is_refused_in_gcc_s_words() {
    let cases: [(&str, &[&str]); 9] = [
        (
            "void f(void) __arm_streaming { }\n",
            &["streaming functions require the ISA extension 'sme'"],
        ),
        (
            "__arm_locally_streaming void f(void) { }\n",
            &["streaming functions require the ISA extension 'sme'"],
        ),
        (
            "void f(void) __arm_inout(\"za\") { }\n",
            &["functions with SME state require the ISA extension 'sme'"],
        ),
        (
            "__arm_new(\"zt0\") void f(void) { }\n",
            &[
                "functions with 'zt0' state require the ISA extension 'sme2'",
                "functions with SME state require the ISA extension 'sme'",
            ],
        ),
        (
            "void f(void) __arm_streaming __arm_in(\"za\") { }\n",
            &["streaming functions require the ISA extension 'sme'"],
        ),
        (
            "void s(void) __arm_streaming;\nvoid (*p)(void) __arm_streaming = s;\n\
             void c(void) { s(); p(); }\n",
            &[
                "calling a streaming function requires the ISA extension 'sme'",
                "calling a streaming function requires the ISA extension 'sme'",
            ],
        ),
        (
            "void s(void) __arm_streaming __arm_in(\"za\");\nvoid c(void) { s(); }\n",
            &[
                "calling a streaming function requires the ISA extension 'sme'",
                "call to a function that shares 'za' state from a function that has no 'za' \
                 state",
            ],
        ),
        (
            "void z(void) __arm_in(\"zt0\");\nvoid p(void) __arm_preserves(\"zt0\");\n\
             void c(void) { z(); p(); }\n",
            &[
                "call to a function that shares 'zt0' state from a function that has no 'zt0' \
                 state",
                "call to a function that shares SME state from a function that has no SME state",
            ],
        ),
        (
            "void s(void) __arm_streaming;\nvoid c(void) __arm_inout(\"za\") { s(); }\n",
            &["functions with SME state require the ISA extension 'sme'"],
        ),
    ];
    for target in TARGETS {
        for (source, said) in cases {
            let (ok, err) = run(target, source);
            assert!(!ok, "{target}: {source}");
            assert_eq!(err.matches("error:").count(), said.len(), "{target}: {source}: {err}");
            for line in said {
                assert!(err.contains(&format!("error: {line}")), "{target}: {source}: {err}");
            }
            assert!(err.contains("without the Scalable Matrix Extension"), "{target}: {err}");
        }
    }
}

/// What gcc says where an attribute is written wrong.
#[test]
fn the_sme_attributes_are_checked_in_gcc_s_words() {
    let target = TARGETS[0];
    let errors = [
        ("void f(void) __arm_in(\"zb\");\n", "error: unrecognized state string 'zb'"),
        (
            "void f(void) __arm_in(\"za\") __arm_out(\"za\");\n",
            "error: inconsistent attributes for state 'za'",
        ),
        ("void f(void) __arm_in(1);\n", "error: the arguments to 'in' must be constant strings"),
        (
            "void f(void) [[arm::streaming(1)]];\n",
            "error: 'streaming' attribute does not take any arguments",
        ),
        (
            "void f(void) [[arm::in]];\n",
            "error: wrong number of arguments specified for 'in' attribute",
        ),
    ];
    for (source, said) in errors {
        let (ok, err) = run(target, source);
        assert!(!ok, "{source}");
        assert!(err.contains(said), "{source}: {err}");
    }
    let warnings = [
        (
            "int v __arm_inout(\"za\");\n",
            "warning: 'inout' attribute only applies to function types",
        ),
        (
            "void f(void) __arm_streaming_compatible __arm_streaming;\n",
            "warning: ignoring attribute 'streaming' because it conflicts with attribute \
             'streaming_compatible'",
        ),
    ];
    for (source, said) in warnings {
        let (ok, err) = run(target, source);
        assert!(ok, "{source}: {err}");
        assert!(err.contains(said), "{source}: {err}");
    }
}

/// On another machine the namespace means nothing, and the macros are not there.
#[test]
fn the_sme_attributes_are_aarch64_alone() {
    let source = "#ifdef __arm_streaming\n#error defined\n#endif\n\
        void f(void) [[arm::streaming]];\nvoid f(void);\nvoid g(void) { f(); }\n";
    let (ok, err) = run("x86_64-unknown-freebsd15.1", source);
    assert!(ok, "{err}");
}
