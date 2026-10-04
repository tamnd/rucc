//! `__builtin_has_attribute` and `__builtin_counted_by_ref`, end to end.
//!
//! Design: tamnd/rucc-kernel#6, the K5 item for the two builtins.
//!
//! Every answer here is one gcc 15 gave for the same question, measured one at a time. The
//! kernel's `__is_cstr` is the reason the `nonstring` answers matter: it asks about every buffer
//! handed to a string copy and stops the build on a wrong yes or a wrong no.

use std::path::PathBuf;
use std::process::Command;

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-annotate-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, what it wrote and what it said.
fn run(what: &str, source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    (out.status.success(), text(out.stdout), text(out.stderr))
}

/// The listing for a source that has to compile.
fn listing(what: &str, source: &str) -> String {
    let (ok, out, err) = run(what, source);
    assert!(ok, "the compiler refused the fixture:\n{err}");
    out
}

/// What the compiler said about a source it has to refuse.
fn refused(what: &str, source: &str) -> String {
    let (ok, _, err) = run(what, source);
    assert!(!ok, "the compiler took a source it should have refused");
    err
}

/// The declarations the questions below are asked about.
const DECLS: &str = "char gstr[8] __attribute__((nonstring));\n\
     char __attribute__((__nonstring__)) gstr2[8];\n\
     char gplain[8];\n\
     struct S { int n; short cnt; char name[8] __attribute__((nonstring)); char plain[8];\n\
                int fam[] __attribute__((counted_by(cnt))); };\n\
     struct U { int n; int fam[]; };\n\
     struct P { char c; int i; } __attribute__((packed));\n\
     typedef int a16 __attribute__((aligned(16)));\n\
     a16 ta;\n\
     int w __attribute__((unused, aligned(32)));\n\
     __attribute__((noreturn, cold)) void f(void);\n\
     _Noreturn void nr(void);\n\
     int dec __attribute__((deprecated));\n\
     typedef int v4 __attribute__((vector_size(16)));\n\
     v4 vv;\n\
     int __attribute__((section(\".foo\"))) sec;\n\
     struct S s;\n\
     struct S *ps = &s;\n\
     char *p = gstr;\n\
     #define yes(...) _Static_assert(__builtin_has_attribute(__VA_ARGS__), #__VA_ARGS__)\n\
     #define no(...) _Static_assert(!__builtin_has_attribute(__VA_ARGS__), #__VA_ARGS__)\n";

/// Each question and gcc 15's answer, as assertions the compiler has to agree with to compile.
#[test]
fn every_answer_is_the_one_gcc_gives() {
    let questions = "yes(gstr, nonstring);\n\
         yes(gstr2, __nonstring__);\n\
         no(gplain, nonstring);\n\
         yes(s.name, nonstring);\n\
         yes(ps->name, nonstring);\n\
         no(s.plain, nonstring);\n\
         yes(ps->fam, counted_by);\n\
         no(p, nonstring);\n\
         no(&gstr[0], nonstring);\n\
         no(gstr + 0, nonstring);\n\
         yes((gstr), nonstring);\n\
         yes(w, unused);\n\
         yes(w, aligned);\n\
         yes(w, aligned(32));\n\
         no(w, aligned(16));\n\
         no(w, packed);\n\
         yes(f, noreturn);\n\
         yes(f, cold);\n\
         no(f, hot);\n\
         yes(nr, noreturn);\n\
         yes(dec, deprecated);\n\
         yes(struct P, packed);\n\
         no(struct S, packed);\n\
         yes(a16, aligned);\n\
         yes(a16, aligned(16));\n\
         no(a16, aligned(8));\n\
         yes(ta, aligned(16));\n\
         no(int, aligned);\n\
         no(s.n, aligned);\n\
         no(struct P, aligned);\n\
         yes(v4, vector_size);\n\
         yes(vv, vector_size(16));\n\
         no(vv, vector_size(32));\n\
         yes(sec, section);\n\
         yes(sec, section(\".foo\"));\n\
         no(sec, section(\".bar\"));\n\
         no(struct S, nonstring);\n";
    listing("answers", &format!("{DECLS}{questions}"));
}

/// The kernel's `__is_cstr` and `__must_be_cstr`, as `compiler.h` writes them from 6.13.
#[test]
fn the_kernel_s_string_check_takes_a_string_and_refuses_a_buffer() {
    let kernel = "#define __annotated(var, attr) __builtin_has_attribute(var, attr)\n\
         #define __is_cstr(a) (!__annotated(a, nonstring))\n\
         #define __must_be_cstr(p) _Static_assert(__is_cstr(p), \"must be cstr\")\n";
    listing("cstr", &format!("{DECLS}{kernel}__must_be_cstr(gplain);\n__must_be_cstr(s.plain);\n"));
    let err = refused("buffer", &format!("{DECLS}{kernel}__must_be_cstr(s.name);\n"));
    assert!(err.contains("must be cstr"), "{err}");
}

/// A counted flexible array's counter is reached through the same object, and one with no
/// counter answers a null `void *`, which is what `overflow.h` tells the two apart by.
#[test]
fn the_counter_is_reached_through_the_array() {
    let out = listing(
        "counter",
        "typedef __SIZE_TYPE__ size_t;\n\
         struct S { int n; short cnt; int fam[] __attribute__((counted_by(cnt))); };\n\
         struct U { int n; int fam[]; };\n\
         #define flex(FAM) __builtin_counted_by_ref(FAM)\n\
         #define set(FAM, COUNT) ({ *_Generic(flex(FAM), void *: &(size_t){0}, \
                                    default: flex(FAM)) = (COUNT); })\n\
         _Static_assert(_Generic(flex(((struct S *)0)->fam), short *: 1, default: 0), \"short\");\n\
         _Static_assert(_Generic(flex(((struct U *)0)->fam), void *: 1, default: 0), \"void\");\n\
         void counted(struct S *p) { set(p->fam, 5); }\n\
         void uncounted(struct U *p) { set(p->fam, 5); }\n",
    );
    assert!(out.contains(", 4(%rdi)"), "the counter is not written:\n{out}");
    let uncounted = &out[out.find("uncounted:").expect("the second function is there")..];
    assert!(!uncounted.contains("(%rdi)"), "the object is written without a counter:\n{out}");
}

/// What gcc 15 refuses, in its words.
#[test]
fn what_gcc_refuses_is_refused() {
    let cases = [
        (
            "struct A { int n; int arr[4] __attribute__((counted_by(n))); };",
            "'counted_by' attribute is not allowed for a non-flexible array member field",
        ),
        (
            "struct B { float f; int fam[] __attribute__((counted_by(f))); };",
            "argument 'f' to the 'counted_by' attribute is not a field declaration with an \
             integer type",
        ),
        (
            "struct C { int n; int fam[] __attribute__((counted_by(nope))); };",
            "argument 'nope' to the 'counted_by' attribute is not a field declaration in the same \
             structure as 'fam'",
        ),
        (
            "struct D { int n; int x __attribute__((counted_by(n))); };",
            "'counted_by' attribute is not allowed for a non-array field",
        ),
        (
            "int *q; void *h(void) { return __builtin_counted_by_ref(q); }",
            "the argument to '__builtin_counted_by_ref' must be an array",
        ),
        ("int g; int k = __builtin_has_attribute(g, nonsuch);", "unknown attribute 'nonsuch'"),
    ];
    for (index, (source, message)) in cases.into_iter().enumerate() {
        let err = refused(&format!("refused{index}"), &format!("{source}\n"));
        assert!(err.contains(message), "no `{message}` in:\n{err}");
    }
}

/// gcc 12 has never heard of `counted_by`, and the kernel finds that out by compiling one under
/// `-Werror`, so under `-fgnuc-version=12.2.0` the attribute is a warning and the operator says no.
#[test]
fn a_persona_before_gcc_15_has_no_counted_by() {
    let source = "struct flex { int count; int array[] __attribute__((__counted_by__(count))); };\n\
                  int asked = __has_attribute(__counted_by__);\n";
    let path = fixture("persona", source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fgnuc-version=12.2.0", "-Werror", "-E"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("int asked = 0;"), "gcc 12 answers no:\n{text}");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fgnuc-version=12.2.0", "-Werror", "-c"])
        .args(["-o", "/dev/null"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "-Werror turns the warning into a refusal");
    assert!(err.contains("'counted_by' attribute directive ignored"), "{err}");
}
