//! kencc format checking under `-fkencc -fkencc-format`, end to end.
//!
//! Design: tamnd/rucc#4148, and 06.6 of the rucc-plan9 plan.
//!
//! The `-F` option of kencc checks each call of a function that `#pragma varargck argpos` names.
//! The format must be a string literal, and each verb, with its flags, must have a
//! `#pragma varargck type` line for the type of its argument. The messages are the ones of
//! `cc/dpchk.c`, and the types print as `Tconv` in `cc/lex.c` prints them.

use std::path::Path;
use std::process::Command;

const PRINT: &str = "typedef unsigned int uint;\n\
                     int print(char*, ...);\n\
                     #pragma varargck argpos print 1\n\
                     #pragma varargck type \"d\" int\n\
                     #pragma varargck type \"d\" uint\n\
                     #pragma varargck type \"ld\" long\n\
                     #pragma varargck type \"s\" char*\n\
                     #pragma varargck type \"r\" void\n";

/// What the compiler said about `body`, with `header` as `h.h` in the same directory.
fn check(what: &str, header: &str, body: &str, flags: &[&str]) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-format-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("h.h"), header).expect("the header can be written");
    std::fs::write(dir.join("one.c"), body).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fsyntax-only", "-fkencc"])
        .args(flags)
        .arg("-I")
        .arg(&dir)
        .arg(Path::new(&dir).join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The messages for `body` after the print lines, under `-fkencc-format`.
fn format(what: &str, body: &str) -> String {
    let (ok, err) = check(what, PRINT, &format!("#include \"h.h\"\n{body}"), &["-fkencc-format"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    err
}

#[test]
fn a_verb_with_the_wrong_type_is_a_mismatch() {
    let err = format("mismatch", "void f(long l, char *s) { print(\"%d %s %ld\", l, s, l); }\n");
    assert!(err.contains("format mismatch d LONG, arg 2"), "{err}");
    assert_eq!(err.matches("format mismatch").count(), 1, "{err}");
}

#[test]
fn the_types_print_as_kencc_prints_them() {
    let err = format("text", "void f(const char *s, int *p) { print(\"%d %d\", s, p); }\n");
    assert!(err.contains("format mismatch d IND CONST CHAR, arg 2"), "{err}");
    assert!(err.contains("format mismatch d IND INT, arg 3"), "{err}");
}

#[test]
fn a_void_verb_takes_no_argument() {
    let err = format("void", "void f(int i) { print(\"%r %d\", i); }\n");
    assert!(err.is_empty(), "{err}");
}

#[test]
fn the_counts_of_verbs_and_arguments_must_agree() {
    let err = format("count", "void f(int i) { print(\"%d\", i, i); print(\"%d %d\", i); }\n");
    assert!(err.contains("more arguments than format INT"), "{err}");
    assert!(err.contains("more format than arguments d"), "{err}");
}

#[test]
fn a_char_argument_is_an_int_as_kencc_promotes_it() {
    let err = format("promote", "void f(char c, short s) { print(\"%d %d\", c, s); }\n");
    assert!(err.is_empty(), "{err}");
}

#[test]
fn a_size_is_an_int() {
    let body = "typedef char signed_size[(-1 < sizeof(int)) ? 1 : -1];\n\
                void f(void) { char buf[8]; print(\"%d\", sizeof buf - 1); }\n";
    let err = format("sizeof", body);
    assert!(err.is_empty(), "{err}");
}

#[test]
fn a_line_is_read_after_the_declarations_before_it_in_a_header() {
    let header = "typedef unsigned char uchar;\n\
                  int print(char*, ...);\n\
                  #pragma varargck argpos print 1\n";
    let body = "#include \"h.h\"\n\
                typedef struct Jar Jar;\n\
                #pragma varargck type \"J\" Jar*\n\
                #pragma varargck type \"I\" uchar*\n\
                void f(Jar *j, uchar *ip) { print(\"%J %I\", j, ip); }\n";
    let (ok, err) = check("order", header, body, &["-fkencc-format"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(err.is_empty(), "{err}");
}

#[test]
fn a_type_not_yet_defined_is_reported() {
    let err = format("undefined", "#pragma varargck type \"Q\" Quux*\n");
    assert!(err.contains("Quux: newprot: type not defined"), "{err}");
}

#[test]
fn a_bad_line_is_an_error() {
    let body = "#pragma varargck nothing\n";
    let (ok, err) = check("bad", PRINT, &format!("#include \"h.h\"\n{body}"), &["-fkencc-format"]);
    assert!(!ok);
    assert!(err.contains("syntax in #pragma varargck"), "{err}");
}

#[test]
fn a_call_with_too_few_arguments_is_not_checked() {
    let header = "int cat(int, char*, ...);\n#pragma varargck argpos cat 2\n";
    let (_, err) =
        check("few", header, "#include \"h.h\"\nvoid f(void) { cat(1); }\n", &["-fkencc-format"]);
    assert!(!err.contains("cant find format arg"), "{err}");
}

#[test]
fn the_check_is_off_without_the_option() {
    let body = "#include \"h.h\"\nvoid f(long l) { print(\"%d\", l); }\n";
    let (ok, err) = check("off", PRINT, body, &[]);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(!err.contains("format mismatch"), "{err}");
}
