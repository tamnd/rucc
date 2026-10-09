//! `btf_decl_tag` and `btf_type_tag`, end to end.
//!
//! What gcc 16 says about each fixture is what its own tests for the two attributes,
//! `gcc.dg/attr-btf-decl-tag-*.c` and `gcc.dg/attr-btf-type-tag-*.c`, hold it to, and most of
//! the lines here are theirs.

use std::path::PathBuf;
use std::process::Command;

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-btf-tags-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    dir
}

/// Whether the compiler took the source and what it said, with the persona's options first.
fn run(what: &str, source: &str, options: &[&str]) -> (bool, String) {
    let dir = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(options)
        .args(["-S", "-o", "-", "one.c"])
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The lines with that code, without their columns.
fn coded(err: &str, code: &str) -> Vec<String> {
    err.lines()
        .filter(|line| line.contains(code))
        .map(|line| {
            let mut parts: Vec<&str> = line.splitn(4, ':').collect();
            parts.remove(2);
            parts.join(":")
        })
        .collect()
}

/// The kernel's `__user` and `__rcu` as a BTF build defines them, and the tags everywhere gcc
/// takes them, compile without a word, and `__has_attribute` says gcc 16 has both.
#[test]
fn the_tags_are_taken_where_gcc_takes_them() {
    let source = r#"
        #define __user __attribute__((btf_type_tag("user")))
        #define __rcu __attribute__((btf_type_tag("rcu")))
        #define __tag(x) __attribute__((btf_decl_tag(x)))
        #if __has_attribute(btf_type_tag) != 1 || __has_attribute(__btf_decl_tag__) != 1
        #error gcc 16 has both tags
        #endif
        struct node { struct node __rcu *next; int key __tag("key"); };
        typedef int __user *uptr;
        void *vptr __tag("vptr") __tag("perthread");
        int **my_ptr __attribute__((btf_decl_tag("my_ptr")));
        void * __attribute__((btf_type_tag ("A"), btf_type_tag ("vptr"))) a;
        int __attribute__((btf_type_tag (u8"u8str"))) z;
        int arr[8] __attribute__((btf_type_tag("tagged_arr")));
        [[gnu::btf_decl_tag("std")]] int std_tagged;
        union U { int x; char c __tag(u8"u8str"); };
        extern int foo(int x, int y __tag("y"));
        int (*fp)(void) __attribute__((btf_type_tag("fp")));
        void *alloc(void) __tag("alloc");
        int get(int __user *p) { return *(int __user *)p + (int)sizeof(uptr); }
        long __tag("defined") defined(void) { return 0; }
    "#;
    let (ok, err) = run("taken", source, &["-Werror"]);
    assert!(ok, "the compiler refused the fixture:\n{err}");
    assert_eq!(err, "", "nothing is said about a tag where gcc takes it");
}

/// A tag is one narrow string literal, and anything else is refused in gcc's words, wherever
/// the tag was written.
#[test]
fn an_argument_gcc_refuses_is_refused_in_its_words() {
    let source = r#"int __attribute__((btf_type_tag (5))) b;
char * __attribute__((btf_type_tag (L"Lstr"))) c;
int * __attribute__((btf_type_tag)) d;
char * __attribute__((btf_type_tag ("A", "B"))) e;
int __attribute__((btf_type_tag (U"Ustr"))) x;
int __attribute__((btf_type_tag (u"ustr"))) y;
struct Foo
{
  int x __attribute__((btf_decl_tag (0x55)));
  char *c __attribute__((btf_decl_tag (L"Lstr")));
};
extern int foo (int x, int y __attribute__((btf_decl_tag)));
char *str __attribute__((btf_decl_tag("A", "B")));
int n = 1;
int *g __attribute__((btf_type_tag(n)));
int h(void) { return (int)(int __attribute__((btf_type_tag("a" + 1))))n; }
"#;
    let (ok, err) = run("refused", source, &[]);
    assert!(!ok, "the compiler took a tag gcc refuses");
    let error = |line, what: String| format!("one.c:{line}: error: {what} [E0858]");
    let string = |line, name| error(line, format!("'{name}' attribute requires a string argument"));
    let wide = |line, name| {
        error(line, format!("unsupported wide string type argument in '{name}' attribute"))
    };
    let count = |line, name| {
        error(line, format!("wrong number of arguments specified for '{name}' attribute"))
    };
    let expected = [
        string(1, "btf_type_tag"),
        wide(2, "btf_type_tag"),
        count(3, "btf_type_tag"),
        count(4, "btf_type_tag"),
        wide(5, "btf_type_tag"),
        wide(6, "btf_type_tag"),
        string(9, "btf_decl_tag"),
        wide(10, "btf_decl_tag"),
        count(12, "btf_decl_tag"),
        count(13, "btf_decl_tag"),
        string(15, "btf_type_tag"),
        string(16, "btf_type_tag"),
    ];
    assert_eq!(coded(&err, "[E0858]"), expected, "in:\n{err}");
    assert!(err.contains("note: expected 1, found 0"), "the count is given:\n{err}");
    assert!(err.contains("note: expected 1, found 2"), "the count is given:\n{err}");
}

/// A `btf_type_tag` on a function and a `btf_decl_tag` on a structure, a union or an
/// enumeration are ignored with gcc's warnings, which are the only words said about them.
#[test]
fn a_tag_gcc_ignores_is_warned_about() {
    let source = r#"int __attribute__((btf_type_tag ("A"))) a (int x);
__attribute__((btf_type_tag ("B"))) int *b (int y);
int *c (int z) __attribute__((btf_type_tag ("C")));
struct __attribute__((btf_decl_tag("s"))) S { int x; };
union U { int x; } __attribute__((btf_decl_tag("u")));
enum __attribute__((btf_decl_tag("e"))) E { A };
__attribute__((btf_type_tag("d"))) int d(void) { return 0; }
int (*fp)(void) __attribute__((btf_type_tag("fp")));
typedef int fn_t(void) __attribute__((btf_type_tag("t")));
"#;
    let (ok, err) = run("ignored", source, &[]);
    assert!(ok, "the compiler refused a tag gcc only warns about:\n{err}");
    let warning = |line, what| format!("one.c:{line}: warning: {what} [E0703]");
    let functions = |line| warning(line, "'btf_type_tag' attribute does not apply to functions");
    let types = |line| warning(line, "'btf_decl_tag' attribute does not apply to types");
    let expected = [functions(1), functions(2), functions(3), types(4), types(5), types(6)];
    let expected: Vec<String> = expected.into_iter().chain([functions(7), functions(9)]).collect();
    assert_eq!(coded(&err, "[E0703]"), expected, "in:\n{err}");
    assert_eq!(err.lines().filter(|line| line.contains("warning:")).count(), 8, "in:\n{err}");
}

/// The tags came in gcc 16, so a persona before it has never heard of them: `__has_attribute`
/// says no, and a tag is a directive gcc ignores, whatever its argument.
#[test]
fn a_persona_before_gcc_16_has_no_tags() {
    let source = r#"#if __has_attribute(btf_type_tag) || __has_attribute(btf_decl_tag)
#error gcc 15 has no tags
#endif
int *p __attribute__((btf_type_tag("x")));
int q __attribute__((btf_decl_tag(5)));
"#;
    let (ok, err) = run("gcc15", source, &["-fgnuc-version=15.2.0"]);
    assert!(ok, "the compiler refused a tag gcc 15 ignores:\n{err}");
    let expected = [
        "one.c:4: warning: 'btf_type_tag' attribute directive ignored [E0703]",
        "one.c:5: warning: 'btf_decl_tag' attribute directive ignored [E0703]",
    ];
    assert_eq!(coded(&err, "[E0703]"), expected, "in:\n{err}");
    assert!(!err.contains("E0858"), "nothing more is said:\n{err}");
}
