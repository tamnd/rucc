//! `-fsanitize=integer-divide-by-zero`, `-fsanitize=shift`, `-fsanitize=bool`, `-fsanitize=enum`
//! and `-fsanitize=bounds-strict`, which the kernel's `CONFIG_UBSAN` puts on every unit that does
//! not opt out. The calls carry gcc's names, the record each one is
//! handed is laid out the way gcc lays it out, and a function marked `no_sanitize` makes none.

use std::path::Path;
use std::process::Command;

/// A division, a shift and a compound remainder, each on a line and in a column the program below
/// checks, and a function that asks for no shift checks.
const SOURCE: &str = r#"struct loc { const char *file; unsigned line, column; };
struct type { unsigned short kind, info; char name[]; };
struct data { struct loc loc; struct type *lhs, *rhs; };
void _exit(int);
static int fired;
__attribute__((noinline)) unsigned long divide(unsigned long a, unsigned long b) {
    return a / b;
}
__attribute__((noinline)) int shift(int a, long n) {
    return a << n;
}
__attribute__((noinline)) int update(int a, unsigned char n) {
    a %= n;
    return a;
}
__attribute__((no_sanitize("shift"))) int quiet(int a, int n) { return a << n; }
"#;

/// The runtime, which looks at what it is handed and exits with a status that says whether it was
/// right, and a `main` that picks the case from how many arguments it was given.
const RUNS: &str = r#"
static int same(const char *a, const char *b) {
    while (*a && *a == *b)
        a++, b++;
    return *a == *b;
}
static int ends(const char *file, const char *tail) {
    const char *end = file;
    while (*end)
        end++;
    return end - file >= 5 && same(end - 5, tail);
}
static int type(struct type *t, unsigned info, const char *name) {
    return t->kind == 0 && t->info == info && same(t->name, name);
}
static int where(struct data *d, unsigned line, unsigned column) {
    return ends(d->loc.file, "one.c") && d->loc.line == line && d->loc.column == column;
}
static int ulong_info(void) { return sizeof(long) == 8 ? 12 : 10; }
static int long_info(void) { return sizeof(long) == 8 ? 13 : 11; }
static int count;
void __ubsan_handle_divrem_overflow_abort(struct data *d, unsigned long l, unsigned long r) {
    if (count == 1)
        _exit(where(d, 7, 14) && type(d->lhs, ulong_info(), "'long unsigned int'")
            && l == 7 && r == 0 ? 0 : 11);
    _exit(where(d, 13, 7) && type(d->lhs, 11, "'int'") && l == 5 && r == 0 ? 0 : 14);
}
void __ubsan_handle_shift_out_of_bounds_abort(struct data *d, long l, long r) {
    int ok = where(d, 10, 14) && type(d->lhs, 11, "'int'") && type(d->rhs, long_info(), "'long int'");
    if (count == 2)
        _exit(ok && l == 1 && r == 40 ? 0 : 12);
    _exit(ok && l == 3 && r == 31 ? 0 : 13);
}
void __ubsan_handle_divrem_overflow(struct data *d, unsigned long l, unsigned long r) { _exit(21); }
void __ubsan_handle_shift_out_of_bounds(struct data *d, long l, long r) { fired++; }
int main(int argc, char **argv) {
    volatile long big = 40;
    volatile unsigned long none = 0;
    count = argc;
    switch (argc) {
    case 1: return (int)divide(7, none) + 90;
    case 2: return shift(1, big) + 90;
    case 3: return shift(3, big - 9) + 90;
    case 4: return update(5, (unsigned char)none) + 90;
    }
    if (quiet(1, (int)big - 9) != (int)(1u << 31) || shift(1, 3) != 8 || divide(8, 2) != 4)
        return 31;
    if (update(7, 4) != 3 || fired != 0)
        return 32;
    shift(1, big);
    shift(1, big);
    return fired == 2 ? 0 : 33;
}
"#;

fn compile(dir: &Path, source: &str, args: &[&str]) -> std::process::Output {
    std::fs::create_dir_all(dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("-fsanitize=integer-divide-by-zero,shift")
        .args(args)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run")
}

/// The listing for that target and those flags, with the lines of `quiet` apart from the rest.
fn listing(target: &str, args: &[&str]) -> (String, String) {
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).collect();
    let dir = std::env::temp_dir().join(format!("rucc-ubsan-{}-{target}{tag}", std::process::id()));
    let mut all = vec![format!("--target={target}"), "-S".into(), "-o".into(), "-".into()];
    all.extend(args.iter().map(|&arg| arg.to_owned()));
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let out = compile(&dir, SOURCE, &all);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{target} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    let start = text.find("\nquiet:").expect("quiet is defined");
    let end = text[start..].find(".size\tquiet").map_or(text.len(), |at| start + at);
    let quiet = text[start..end].to_owned();
    (text, quiet)
}

#[test]
fn every_check_calls_the_runtime_gcc_names_with_gcc_records() {
    let targets =
        ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "i686-unknown-linux-gnu"];
    for target in targets {
        for level in ["-O0", "-O2"] {
            let (text, quiet) = listing(target, &[level]);
            for wanted in [
                "__ubsan_handle_divrem_overflow",
                "__ubsan_handle_shift_out_of_bounds",
                "'long unsigned int'",
                "'long int'",
                "'int'",
                "one.c",
            ] {
                assert!(text.contains(wanted), "{target} {level} has no {wanted}:\n{text}");
            }
            assert!(!text.contains("_abort"), "{target} {level}:\n{text}");
            assert!(!quiet.contains("__ubsan"), "{target} {level}:\n{quiet}");
        }
    }
}

#[test]
fn the_flags_that_say_what_a_check_does_are_read() {
    let target = "x86_64-unknown-linux-gnu";
    let (text, _) = listing(target, &["-O2", "-fno-sanitize-recover=all"]);
    assert!(text.contains("__ubsan_handle_divrem_overflow_abort"), "{text}");
    assert!(text.contains("__ubsan_handle_shift_out_of_bounds_abort"), "{text}");

    let (text, _) = listing(target, &["-O2", "-fsanitize-trap=all"]);
    assert!(!text.contains("__ubsan_handle"), "{text}");
    assert!(text.contains("ud2"), "{text}");

    let (text, _) = listing(target, &["-O2", "-fsanitize-undefined-trap-on-error"]);
    assert!(!text.contains("__ubsan_handle"), "{text}");

    let (text, _) = listing(target, &["-O2", "-fno-sanitize=shift"]);
    assert!(text.contains("__ubsan_handle_divrem_overflow"), "{text}");
    assert!(!text.contains("__ubsan_handle_shift"), "{text}");

    let (text, _) = listing(target, &["-O2", "-fno-sanitize=all"]);
    assert!(!text.contains("__ubsan"), "{text}");
}

#[test]
fn the_checks_the_compiler_does_not_have_are_still_refused() {
    let dir = std::env::temp_dir().join(format!("rucc-ubsan-refused-{}", std::process::id()));
    for asked in ["-fsanitize=undefined", "-fsanitize=bounds", "-fsanitize=shift,null"] {
        let out = compile(&dir, SOURCE, &[asked, "-S", "-o", "-"]);
        assert!(!out.status.success(), "{asked} is refused");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.contains("-fsafety=detect"), "{asked}: {said}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "linux")))]
#[test]
fn the_runtime_is_told_where_and_what_and_the_program_goes_on() {
    for level in ["-O0", "-O2"] {
        for (recover, cases) in
            [("-fno-sanitize-recover=all", 1..5), ("-fsanitize-recover=all", 5..6)]
        {
            let dir = std::env::temp_dir().join(format!(
                "rucc-ubsan-run-{}{level}{}",
                std::process::id(),
                cases.start
            ));
            let program = dir.join("one");
            let program_text = program.to_str().expect("a temporary path is text");
            let source = format!("{SOURCE}{RUNS}");
            let out = compile(&dir, &source, &[level, recover, "-o", program_text]);
            assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
            for case in cases {
                let status = Command::new(&program)
                    .args(std::iter::repeat_n("x", case - 1))
                    .status()
                    .expect("the program runs");
                assert_eq!(status.code(), Some(0), "{level} {recover} case {case}");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Reads of a `bool` from memory, which are checked, and after `local` reads that are not.
const BOOLS: &str = r#"struct s { int a; _Bool b; };
int deref(_Bool *p) { return *p ? 3 : 4; }
int member(struct s *s) { return s->b; }
int element(_Bool *arr, int i) { return arr[i]; }
int local(int x) { _Bool b = x; return b; }
__attribute__((no_sanitize("bool"))) int quiet(_Bool *p) { return *p; }
__attribute__((no_sanitize_undefined)) int still(_Bool *p) { return *p; }
"#;

fn bools(args: &[&str]) -> String {
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).collect();
    let dir = std::env::temp_dir().join(format!("rucc-ubsan-bool-{}{tag}", std::process::id()));
    let mut all = vec!["--target=x86_64-unknown-linux-gnu", "-std=gnu11", "-S", "-o", "-"];
    all.extend(args);
    let out = compile(&dir, BOOLS, &all);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn a_bool_read_from_memory_is_checked_and_enum_is_taken() {
    for level in ["-O0", "-O2"] {
        let text = bools(&[level, "-fsanitize=bool,enum"]);
        assert_eq!(text.matches("call\t__ubsan_handle_load_invalid_value").count(), 3, "{text}");
        assert!(text.contains("'_Bool'"), "{level}:\n{text}");
        let start = text.find("\nlocal:").expect("local is defined");
        assert!(!text[start..].contains("__ubsan"), "{level}:\n{text}");
    }
    let text = bools(&["-O2", "-std=c23", "-fsanitize=bool"]);
    assert!(text.contains("'bool'"), "{text}");
    let text = bools(&["-O2", "-fsanitize=bool", "-fno-sanitize-recover=bool"]);
    assert!(text.contains("__ubsan_handle_load_invalid_value_abort"), "{text}");
    let text = bools(&["-O2", "-fsanitize=bool", "-fsanitize-trap=bool"]);
    assert!(!text.contains("__ubsan_handle"), "{text}");
    let text = bools(&["-O2", "-fsanitize=enum"]);
    assert!(!text.contains("__ubsan_handle"), "{text}");
}

/// Subscripts of arrays whose length is known, the address of one past the end, and after
/// `pointer` subscripts that have no check or are in a function that turns the check off.
const BOUNDS: &str = r#"struct s { int n; int a[4]; };
struct c { int n; long a[] __attribute__((counted_by(n))); };
typedef struct { short x; } pair;
int g[10];
pair pairs[3];
int fixed(int i) { return g[i]; }
int member(struct s *s, unsigned char i) { return s->a[i]; }
int *end(long i) { return &g[i]; }
long counted(struct c *c, int i) { return c->a[i]; }
int variable(int n, int i) { int v[n]; v[0] = 1; return v[i]; }
short named(int i) { return pairs[i].x; }
int pointer(int *p, int i) { return p[i]; }
int constant(void) { return g[9]; }
__attribute__((no_sanitize("bounds"))) int quiet(int i) { return g[i]; }
__attribute__((no_sanitize_undefined)) int still(int i) { return g[i]; }
"#;

fn bounds(target: &str, args: &[&str]) -> String {
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).collect();
    let dir =
        std::env::temp_dir().join(format!("rucc-ubsan-bounds-{}{target}{tag}", std::process::id()));
    let target = format!("--target={target}");
    let mut all = vec![target.as_str(), "-fsanitize=bounds-strict", "-S", "-o", "-"];
    all.extend(args);
    let out = compile(&dir, BOUNDS, &all);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn an_index_past_a_known_length_calls_the_runtime() {
    let targets =
        ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "i686-unknown-linux-gnu"];
    for target in targets {
        for level in ["-O0", "-O2"] {
            let text = bounds(target, &[level]);
            let calls = text.matches("__ubsan_handle_out_of_bounds").count();
            assert!(calls >= 7, "{target} {level} has {calls} calls:\n{text}");
            for wanted in ["'int [10]'", "'int [4]'", "'long int [*]'", "'int [*]'", "'int'"] {
                assert!(text.contains(wanted), "{target} {level} has no {wanted}:\n{text}");
            }
            let start = text.find("\npointer:").expect("pointer is defined");
            assert!(!text[start..].contains("__ubsan"), "{target} {level}:\n{}", &text[start..]);
        }
    }
    let text = bounds("x86_64-unknown-linux-gnu", &["-O2", "-fsanitize-trap=bounds-strict"]);
    assert!(!text.contains("__ubsan_handle_out_of_bounds"), "{text}");
    let text = bounds("x86_64-unknown-linux-gnu", &["-O2", "-fno-sanitize=bounds-strict"]);
    assert!(!text.contains("__ubsan_handle_out_of_bounds"), "{text}");
}
