//! `-fsanitize=integer-divide-by-zero`, `-fsanitize=shift`, `-fsanitize=bool`, `-fsanitize=enum`,
//! `-fsanitize=bounds-strict`, `-fsanitize=bounds`, `-fsanitize=vla-bound`,
//! `-fsanitize=signed-integer-overflow`, `-fsanitize=null`, `-fsanitize=alignment`,
//! `-fsanitize=object-size`, `-fsanitize=pointer-overflow`, `-fsanitize=nonnull-attribute`,
//! `-fsanitize=returns-nonnull-attribute` and `-fsanitize=builtin`, which the kernel's
//! `CONFIG_UBSAN` and a userland built with `-fsanitize=undefined` put on every unit that does
//! not opt out. `-fsanitize=undefined` is all of them but `-fsanitize=bounds-strict`. The calls carry gcc's names, the record each one is
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
#[cfg(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "linux")))]
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
    for asked in ["-fsanitize=float-cast-overflow", "-fsanitize=shift,float-divide-by-zero"] {
        let out = compile(&dir, SOURCE, &[asked, "-S", "-o", "-"]);
        assert!(!out.status.success(), "{asked} is refused");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.contains("-fsafety=detect"), "{asked}: {said}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// gcc's `undefined` group is every check this compiler makes but `bounds-strict`, so a userland
/// built with it, NetBSD's `t_ubsan.c` among them, compiles.
#[test]
fn the_undefined_group_is_every_check_and_is_taken() {
    let (text, _) = listing("x86_64-unknown-linux-gnu", &["-O2", "-fsanitize=undefined"]);
    assert!(text.contains("__ubsan_handle_divrem_overflow"), "{text}");
    assert!(text.contains("__ubsan_handle_shift_out_of_bounds"), "{text}");
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

/// A promise that control does not get somewhere, kept in `f` and `g` and turned off in `quiet`.
const UNREACHABLE: &str = r#"int f(int x) { if (x > 3) __builtin_unreachable(); return x; }
int g(int x) { switch (x) { case 1: return 2; default: __builtin_unreachable(); } }
__attribute__((no_sanitize("unreachable"))) int quiet(int x) { if (x) __builtin_unreachable(); return 1; }
"#;

fn unreachable(args: &[&str]) -> std::process::Output {
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).collect();
    let dir =
        std::env::temp_dir().join(format!("rucc-ubsan-unreachable-{}{tag}", std::process::id()));
    let mut all =
        vec!["--target=x86_64-unknown-linux-gnu", "-fsanitize=unreachable", "-S", "-o", "-"];
    all.extend(args);
    let out = compile(&dir, UNREACHABLE, &all);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn reaching_an_unreachable_builtin_calls_the_runtime() {
    for level in ["-O0", "-O2"] {
        let out = unreachable(&[level]);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).expect("a listing is text");
        let calls = text.matches("call\t__ubsan_handle_builtin_unreachable").count();
        assert_eq!(calls, 2, "{level}:\n{text}");
        assert!(text.contains("\t.long\t27\n"), "{level} has no column 27:\n{text}");
        let start = text.find("\nquiet:").expect("quiet is defined");
        assert!(!text[start..].contains("call\t__ubsan"), "{level}:\n{text}");
    }
    let out = unreachable(&["-O2", "-fno-sanitize-recover=unreachable"]);
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    assert_eq!(text.matches("call\t__ubsan_handle_builtin_unreachable").count(), 2, "{text}");
    let out = unreachable(&["-O2", "-fsanitize-trap=unreachable"]);
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    assert!(!text.contains("__ubsan_handle"), "{text}");
    assert!(text.contains("ud2"), "{text}");
    let out = unreachable(&["-O2", "-fsanitize-recover=unreachable"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not supported"));
}

/// The length of a counted array whose counter is in an anonymous member is read at its offset.
#[test]
fn a_counter_in_an_anonymous_member_bounds_the_index() {
    let source = "struct e { int x; };\n\
        struct acl { union { struct { int r; unsigned int a_count; }; \
        struct hdr { int r; unsigned int a_count; } hdr; }; \
        struct e a_entries[] __attribute__((__counted_by__(a_count))); };\n\
        int f(struct acl *a, int i) { return a->a_entries[i].x; }\n";
    let dir = std::env::temp_dir().join(format!("rucc-ubsan-anonymous-{}", std::process::id()));
    let args = ["--target=x86_64-unknown-linux-gnu", "-fsanitize=bounds-strict", "-O2", "-S"];
    let out = compile(&dir, source, &[&args[..], &["-o", "-"]].concat());
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    assert_eq!(text.matches("call\t__ubsan_handle_out_of_bounds").count(), 1, "{text}");
    assert!(text.contains("\tmovl\t4(%r"), "the counter is not read:\n{text}");
}

/// Signed arithmetic that can leave its type, each operator on a line and in a column the program
/// below checks, and after `uadd` the operations gcc makes no check for.
const OVERFLOW: &str = r#"struct loc { const char *file; unsigned line, column; };
struct type { unsigned short kind, info; char name[]; };
struct data { struct loc loc; struct type *type; };
void _exit(int);
__attribute__((noinline)) int add(int a, int b) { return a + b; }
__attribute__((noinline)) long sub(long a, long b) { return a - b; }
__attribute__((noinline)) int mul(int a, int b) { a *= b; return a; }
__attribute__((noinline)) int neg(int a) { return -a; }
__attribute__((noinline)) int quot(int a, int b) { return a % b; }
__attribute__((noinline)) int post(int a) { a ++; return a; }
__attribute__((noinline)) int pre(int a) { --a; return a; }
__attribute__((noinline)) unsigned uadd(unsigned a, unsigned b) { return a + b; }
__attribute__((noinline)) short step(short s) { s++; return s; }
__attribute__((noinline, no_sanitize("signed-integer-overflow"))) int quiet(int a, int b) {
    return a + b;
}
"#;

/// The runtime for [`OVERFLOW`], which exits with a status that says whether it was told the right
/// place, type and operands, and a `main` that picks the case from how many arguments it has.
#[cfg(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "linux")))]
const OVERFLOWS: &str = r#"
static int same(const char *a, const char *b) {
    while (*a && *a == *b)
        a++, b++;
    return *a == *b;
}
static int count, fired;
static int at(struct data *d, unsigned line, unsigned column, unsigned info, const char *name) {
    return d->loc.line == line && d->loc.column == column && d->type->kind == 0
        && d->type->info == info && same(d->type->name, name);
}
static int is_int(struct data *d, unsigned line, unsigned column) {
    return at(d, line, column, 11, "'int'");
}
#define LEAST (-2147483647L - 1)
void __ubsan_handle_add_overflow_abort(struct data *d, long l, long r) {
    if (count == 1)
        _exit(is_int(d, 5, 60) && l == 2147483647 && r == 1 ? 0 : 11);
    _exit(is_int(d, 10, 47) && l == 2147483647 && r == 1 ? 0 : 16);
}
void __ubsan_handle_sub_overflow_abort(struct data *d, long l, long r) {
    if (count == 2) {
        unsigned info = sizeof(long) == 8 ? 13 : 11;
        _exit(at(d, 6, 63, info, "'long int'") && l == -9223372036854775807L - 1 && r == 1
            ? 0 : 12);
    }
    _exit(is_int(d, 11, 44) && l == LEAST && r == 1 ? 0 : 17);
}
void __ubsan_handle_mul_overflow_abort(struct data *d, long l, long r) {
    _exit(is_int(d, 7, 53) && l == 65536 && r == 65536 ? 0 : 13);
}
void __ubsan_handle_negate_overflow_abort(struct data *d, long v) {
    _exit(is_int(d, 8, 51) && v == LEAST ? 0 : 14);
}
void __ubsan_handle_divrem_overflow_abort(struct data *d, long l, long r) {
    _exit(is_int(d, 9, 61) && l == LEAST && r == -1 ? 0 : 15);
}
void __ubsan_handle_add_overflow(struct data *d, long l, long r) { fired++; }
void __ubsan_handle_sub_overflow(struct data *d, long l, long r) { _exit(21); }
void __ubsan_handle_mul_overflow(struct data *d, long l, long r) { _exit(22); }
void __ubsan_handle_negate_overflow(struct data *d, long v) { _exit(23); }
void __ubsan_handle_divrem_overflow(struct data *d, long l, long r) { _exit(24); }
int main(int argc, char **argv) {
    volatile int most = 2147483647, least = -2147483647 - 1;
    count = argc;
    switch (argc) {
    case 1: return add(most, 1) + 90;
    case 2: return (int)sub(sizeof(long) == 8 ? -9223372036854775807L - 1 : least, 1) + 90;
    case 3: return mul(65536, 65536) + 90;
    case 4: return neg(least) + 90;
    case 5: return quot(least, -1) + 90;
    case 6: return post(most) + 90;
    case 7: return pre(least) + 90;
    }
    if (uadd(4294967295u, 1) != 0 || step(32767) != -32768 || quiet(most, 1) != least)
        return 31;
    if (add(3, 4) != 7 || sub(3, 4) != -1 || mul(-3, 4) != -12 || neg(5) != -5)
        return 32;
    if (quot(7, -2) != 1 || post(5) != 6 || pre(5) != 4 || fired != 0)
        return 33;
    add(most, 1);
    return fired == 1 ? 0 : 34;
}
"#;

fn overflow(target: &str, args: &[&str]) -> String {
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).collect();
    let dir = std::env::temp_dir()
        .join(format!("rucc-ubsan-overflow-{}{target}{tag}", std::process::id()));
    let target = format!("--target={target}");
    let mut all = vec![target.as_str(), "-fsanitize=signed-integer-overflow", "-S", "-o", "-"];
    all.extend(args);
    let out = compile(&dir, OVERFLOW, &all);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn signed_arithmetic_that_leaves_its_type_calls_the_runtime() {
    let targets =
        ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "i686-unknown-linux-gnu"];
    let names = ["add", "sub", "mul", "negate", "divrem"];
    for target in targets {
        for level in ["-O0", "-O2"] {
            let text = overflow(target, &[level]);
            for name in names {
                let wanted = format!("__ubsan_handle_{name}_overflow");
                assert!(text.contains(&wanted), "{target} {level} has no {wanted}:\n{text}");
            }
            let start = text.find("\nuadd:").expect("uadd is defined");
            assert!(!text[start..].contains("__ubsan_handle"), "{target} {level}:\n{text}");
        }
    }
    // `-fwrapv` defines the overflow, but gcc still checks the quotient of the least value and
    // minus one.
    let text = overflow("x86_64-unknown-linux-gnu", &["-O2", "-fwrapv"]);
    assert!(text.contains("__ubsan_handle_divrem_overflow"), "{text}");
    for name in ["add", "sub", "mul", "negate"] {
        assert!(!text.contains(&format!("__ubsan_handle_{name}")), "{text}");
    }
    let text = overflow("x86_64-unknown-linux-gnu", &["-O2", "-fsanitize-trap=all"]);
    assert!(!text.contains("__ubsan_handle"), "{text}");
    assert!(text.contains("ud2"), "{text}");
}

#[cfg(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "linux")))]
#[test]
fn the_runtime_is_told_which_operation_left_its_type() {
    for level in ["-O0", "-O2"] {
        for (recover, cases) in
            [("-fno-sanitize-recover=all", 1..8), ("-fsanitize-recover=all", 8..9)]
        {
            let dir = std::env::temp_dir().join(format!(
                "rucc-ubsan-overflow-run-{}{level}{}",
                std::process::id(),
                cases.start
            ));
            let program = dir.join("one");
            let program_text = program.to_str().expect("a temporary path is text");
            let source = format!("{OVERFLOW}{OVERFLOWS}");
            let args = [level, "-fsanitize=signed-integer-overflow", recover, "-o", program_text];
            let out = compile(&dir, &source, &args);
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

/// The body of function `name` in a listing.
fn body<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text.find(&format!("\n{name}:")).unwrap_or_else(|| panic!("{name} is defined"));
    let end = text[start..].find(&format!(".size\t{name}")).map_or(text.len(), |at| start + at);
    &text[start..end]
}

/// Arrays at the end of a structure and arrays that are not, reached through a pointer and not.
const TRAILING: &str = r#"struct tail { int n; int a[4]; };
struct mid { int a[4]; int n; };
struct outer { struct tail t; };
struct outer2 { struct tail t; int z; };
union either { int a[4]; long b; };
int last(struct tail *p, int i) { return p->a[i]; }
int middle(struct mid *p, int i) { return p->a[i]; }
int nested(struct outer *p, int i) { return p->t.a[i]; }
int inner(struct outer2 *p, int i) { return p->t.a[i]; }
int local(int i) { struct tail t = {0}; return t.a[i]; }
int member(union either *p, int i) { return p->a[i]; }
"#;

fn trailing(args: &[&str]) -> String {
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).collect();
    let dir = std::env::temp_dir().join(format!("rucc-ubsan-trailing-{}{tag}", std::process::id()));
    let mut all = vec!["--target=x86_64-unknown-linux-gnu", "-S", "-o", "-"];
    all.extend(args);
    let out = compile(&dir, TRAILING, &all);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn bounds_leaves_an_array_at_the_end_of_a_structure_through_a_pointer() {
    let checked = |text: &str, name| body(text, name).contains("__ubsan_handle_out_of_bounds");
    let all = ["last", "middle", "nested", "inner", "local", "member"];
    for level in ["-O0", "-O2"] {
        let text = trailing(&[level, "-fsanitize=bounds"]);
        for name in all {
            let wanted = matches!(name, "middle" | "inner" | "local");
            assert_eq!(checked(&text, name), wanted, "{level} {name}:\n{text}");
        }
        for strict in [&["-fsanitize=bounds-strict"][..], &["-fsanitize=bounds,bounds-strict"]] {
            let mut args = vec![level];
            args.extend(strict);
            let text = trailing(&args);
            for name in all {
                assert!(checked(&text, name), "{args:?} {name}:\n{text}");
            }
        }
        let text = trailing(&[level, "-fsanitize=bounds", "-fstrict-flex-arrays=1"]);
        for name in all {
            assert!(checked(&text, name), "{level} -fstrict-flex-arrays=1 {name}:\n{text}");
        }
    }
    let text = trailing(&["-O2", "-fsanitize=bounds", "-fsanitize-trap=bounds"]);
    assert!(!text.contains("__ubsan_handle"), "{text}");
    assert!(text.contains("ud2"), "{text}");
}

/// Variable length arrays whose lengths are checked, and in `quiet` one that is not. The lines and
/// the columns of the names are the ones gcc 13 gives.
const VLAS: &str = r#"void g(int *);
void f(short n, unsigned char u){ int a[n]; typedef int T[u]; T b; g(a); g(b); }
void h(long n){ int (*p)[n] = 0; g(*p); }
__attribute__((no_sanitize("vla-bound"))) void quiet(int n) { int a[n]; g(a); }
"#;

#[test]
fn a_length_that_is_not_positive_calls_the_runtime() {
    let calls = |text: &str| text.matches("call\t__ubsan_handle_vla_bound_not_positive").count();
    let dir = std::env::temp_dir().join(format!("rucc-ubsan-vla-{}", std::process::id()));
    for level in ["-O0", "-O2"] {
        let args =
            ["--target=x86_64-unknown-linux-gnu", "-fsanitize=vla-bound", level, "-S", "-o", "-"];
        let out = compile(&dir, VLAS, &args);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).expect("a listing is text");
        assert_eq!(calls(body(&text, "f")), 2, "{level}:\n{text}");
        assert_eq!(calls(body(&text, "h")), 1, "{level}:\n{text}");
        assert_eq!(calls(body(&text, "quiet")), 0, "{level}:\n{text}");
        for wanted in ["'short int'", "'unsigned char'", "'long int'"] {
            assert!(text.contains(wanted), "{level} has no {wanted}:\n{text}");
        }
        for column in [39, 57, 23] {
            assert!(text.contains(&format!("\t.long\t{column}\n")), "{level} {column}:\n{text}");
        }
    }
    let args = ["-fsanitize=vla-bound", "-fno-sanitize-recover=vla-bound", "-S", "-o", "-"];
    let out = compile(&dir, VLAS, &args);
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    assert!(text.contains("__ubsan_handle_vla_bound_not_positive_abort"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "linux")))]
#[test]
fn the_runtime_is_told_the_length_that_was_not_positive() {
    let source = r#"struct loc { const char *file; unsigned line, column; };
struct type { unsigned short kind, info; char name[]; };
struct data { struct loc loc; struct type *type; };
void _exit(int);
static int same(const char *a, const char *b) {
    while (*a && *a == *b)
        a++, b++;
    return *a == *b;
}
void __ubsan_handle_vla_bound_not_positive(struct data *d, unsigned long v) {
    _exit(d->loc.line == 16 && d->loc.column == 9 && d->type->info == 11
        && same(d->type->name, "'int'") && (int)v == -2 ? 0 : 1);
}
__attribute__((noinline)) int use(int n) {
    volatile int k = n;
    int a[k];
    a[0] = 5;
    return a[0];
}
int main(int argc, char **argv) {
    if (use(3) != 5)
        return 2;
    use(1 - 3 * argc);
    return 3;
}
"#;
    for level in ["-O0", "-O2"] {
        let dir =
            std::env::temp_dir().join(format!("rucc-ubsan-vla-run-{}{level}", std::process::id()));
        let program = dir.join("one");
        let program_text = program.to_str().expect("a temporary path is text");
        let out = compile(&dir, source, &[level, "-fsanitize=vla-bound", "-o", program_text]);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let status = Command::new(&program).status().expect("the program runs");
        assert_eq!(status.code(), Some(0), "{level}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Loads, stores and member accesses through pointers, each in a function of its own, a runtime
/// that prints what it is told, and a `main` that makes each access through a pointer that is
/// not aligned. `ch` reads a `char`, which has no alignment to check.
const POINTED: &str = r#"struct loc { const char *file; unsigned line, column; };
struct type { unsigned short kind, info; char name[]; };
struct data { struct loc loc; struct type *type; unsigned char align, kind; };
int printf(const char *, ...);
void *memcpy(void *, const void *, unsigned long);
void __ubsan_handle_type_mismatch_v1(struct data *d, unsigned long p) {
    printf("%u:%u %u %u %x %s %d\n", d->loc.line, d->loc.column, d->align, d->kind,
           d->type->kind, d->type->name, p != 0);
}
struct S { int a; long b; char c[4]; struct { int x; } in; };
typedef struct S T;
union U { int i; float f; };
int load(int *p) { return *p; }
void store(int *p, int v) { *p = v; }
long member(struct S *s) { return s->b; }
void mstore(struct S *s) { s->b = 1; }
int elem(int *p, int i) { return p[i]; }
char arr(struct S *s, int i) { return s->c[i]; }
void copy(struct S *d, struct S *s) { *d = *s; }
int *addr(struct S *s) { return &s->a; }
char ch(char *p) { return *p; }
int cv(const volatile int *p) { return *p; }
int pp(int **p) { return **p; }
char *dec(struct S *s) { return s->c; }
void cas(int *p) { *p += 3; }
void inc(int *p) { (*p)++; }
long td(T *t) { return t->b; }
int un(union U *u) { return u->i; }
int idx(struct S *s, int i) { return s[i].a; }
char *ea(struct S *s, int i) { return &s->c[i]; }
int ar(int (*q)[4], int i) { return (*q)[i]; }
int nload(struct S *s) { return s->in.x; }
void cstore(struct S *s, int i) { s->c[i] = 3; }
struct S ret(struct S *s) { return *s; }
void take(struct S s) { (void)s; }
void arg(struct S *s) { take(*s); }
void pre(int *p) { ++*p; }
__attribute__((no_sanitize("null", "alignment"))) int quiet(int *p) { return *p; }
int main(void) {
    static long buf[32];
    char *odd = (char *)buf + 1;
    void *inner = odd + 64;
    memcpy(odd, &inner, sizeof inner);
    printf("load\n"); load((int *)odd);
    printf("store\n"); store((int *)odd, 1);
    printf("member\n"); member((struct S *)odd);
    printf("mstore\n"); mstore((struct S *)odd);
    printf("elem\n"); elem((int *)odd, 2);
    printf("arr\n"); arr((struct S *)odd, 1);
    printf("copy\n"); copy((struct S *)(odd + 128), (struct S *)odd);
    printf("addr\n"); addr((struct S *)odd);
    printf("ch\n"); ch(odd);
    printf("cv\n"); cv((int *)odd);
    memcpy(odd, &inner, sizeof inner);
    printf("pp\n"); pp((int **)odd);
    printf("dec\n"); dec((struct S *)odd);
    printf("cas\n"); cas((int *)odd);
    printf("inc\n"); inc((int *)odd);
    printf("td\n"); td((T *)odd);
    printf("un\n"); un((union U *)odd);
    printf("idx\n"); idx((struct S *)odd, 0);
    printf("ea\n"); ea((struct S *)odd, 1);
    printf("ar\n"); ar((int (*)[4])odd, 1);
    printf("nload\n"); nload((struct S *)odd);
    printf("cstore\n"); cstore((struct S *)odd, 1);
    printf("ret\n"); ret((struct S *)odd);
    printf("arg\n"); arg((struct S *)odd);
    printf("pre\n"); pre((int *)odd);
    printf("quiet\n"); quiet((int *)odd);
    return 0;
}
"#;

/// What gcc 13 tells the runtime for [`POINTED`]: the line and the column, the alignment as a
/// power of two, what the access is, what the descriptor says, the type and whether the pointer
/// was passed.
const POINTED_GCC: &str = "load
13:27 2 0 ffff 'int' 1
store
14:32 2 1 ffff 'int' 1
member
15:36 3 3 ffff 'struct S' 1
mstore
16:33 3 3 ffff 'struct S' 1
elem
17:35 2 0 ffff 'int' 1
arr
18:43 3 3 ffff 'struct S' 1
copy
19:42 3 1 ffff 'struct S' 1
19:42 3 0 ffff 'struct S' 1
addr
20:33 3 3 ffff 'struct S' 1
ch
cv
22:40 2 0 ffff 'volatile const int' 1
pp
23:27 3 0 ffff 'int *' 1
23:26 2 0 ffff 'int' 1
dec
24:33 3 3 ffff 'struct S' 1
cas
25:20 2 0 ffff 'int' 1
25:23 2 1 ffff 'int' 1
inc
26:21 2 0 ffff 'int' 1
26:24 2 1 ffff 'int' 1
td
27:25 3 3 ffff 'struct T' 1
un
28:30 2 3 ffff 'union U' 1
idx
29:42 3 3 ffff 'struct S' 1
ea
30:39 3 3 ffff 'struct S' 1
ar
31:41 2 0 ffff 'int' 1
nload
32:38 3 3 ffff 'struct S' 1
cstore
33:43 3 3 ffff 'struct S' 1
ret
34:36 3 0 ffff 'struct S' 1
arg
36:25 3 0 ffff 'struct S' 1
pre
37:22 2 0 ffff 'int' 1
37:20 2 1 ffff 'int' 1
quiet
";

fn pointed(args: &[&str]) -> String {
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).collect();
    let dir = std::env::temp_dir().join(format!("rucc-ubsan-pointed-{}{tag}", std::process::id()));
    let mut all = vec!["--target=x86_64-unknown-linux-gnu", "-S", "-o", "-"];
    all.extend(args);
    let out = compile(&dir, POINTED, &all);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn an_access_through_a_pointer_is_checked_for_null_and_alignment() {
    let calls =
        |text: &str, name| body(text, name).matches("__ubsan_handle_type_mismatch_v1").count();
    for level in ["-O0", "-O2"] {
        let both = pointed(&[level, "-fsanitize=null,alignment"]);
        let null = pointed(&[level, "-fsanitize=null"]);
        let alignment = pointed(&[level, "-fsanitize=alignment"]);
        for (name, wanted) in [("load", 1), ("copy", 2), ("pp", 2), ("cas", 2), ("arg", 1)] {
            assert_eq!(calls(&both, name), wanted, "{level} {name}:\n{both}");
        }
        assert_eq!(calls(&null, "ch"), 1, "{level}:\n{null}");
        assert_eq!(calls(&alignment, "ch"), 0, "{level}:\n{alignment}");
        assert_eq!(calls(&alignment, "load"), 1, "{level}:\n{alignment}");
        for text in [&both, &null, &alignment] {
            assert_eq!(calls(text, "quiet"), 0, "{level}:\n{text}");
            assert!(!text.contains("_abort"), "{level}:\n{text}");
        }
    }
    // The handler is the one that returns when either check asked for goes on, as in gcc.
    let text = pointed(&["-fsanitize=null,alignment", "-fno-sanitize-recover=alignment"]);
    assert!(!text.contains("__ubsan_handle_type_mismatch_v1_abort"), "{text}");
    let text = pointed(&["-fsanitize=null,alignment", "-fno-sanitize-recover=null,alignment"]);
    assert!(text.contains("__ubsan_handle_type_mismatch_v1_abort"), "{text}");
    let text = pointed(&["-O2", "-fsanitize=null,alignment", "-fsanitize-trap=null"]);
    assert_eq!(calls(&text, "load"), 0, "{text}");
    assert!(body(&text, "load").contains("ud2"), "{text}");
}

#[cfg(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "linux")))]
#[test]
fn the_runtime_is_told_what_gcc_tells_it_about_an_access_through_a_pointer() {
    for level in ["-O0", "-O2"] {
        let dir = std::env::temp_dir()
            .join(format!("rucc-ubsan-pointed-run-{}{level}", std::process::id()));
        let program = dir.join("one");
        let program_text = program.to_str().expect("a temporary path is text");
        let args = [level, "-fsanitize=null,alignment", "-o", program_text];
        let out = compile(&dir, POINTED, &args);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let out = Command::new(&program).output().expect("the program runs");
        assert_eq!(out.status.code(), Some(0), "{level}");
        // A plain typedef keeps no name of its own here, so `T *` names the record by its tag
        // where gcc writes `'struct T'`. That is the one difference.
        let wanted = POINTED_GCC.replace("'struct T'", "'struct S'");
        assert_eq!(String::from_utf8_lossy(&out.stdout), wanted, "{level}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// One function for each place the checks of `-fsanitize=nonnull-attribute`,
/// `-fsanitize=returns-nonnull-attribute`, `-fsanitize=pointer-overflow`,
/// `-fsanitize=builtin` and `-fsanitize=object-size` go, and one beside each that has none.
const REST: &str = r#"int glob[4] = { 1, 2, 3, 4 };
__attribute__((nonnull)) void nn(int *a);
void call(int *p) { nn(p); }
__attribute__((returns_nonnull)) int *rn(int *p) { return p; }
int *plain(int *p) { return p; }
int *step(int *p, long n) { return p + n; }
int *zero(int *p) { return p + 0; }
int named(void) { return glob[2]; }
int clz(unsigned x) { return __builtin_clz(x); }
int clz8(void) { return __builtin_clz(8); }
int past(void) { int *p = glob + 5; return *p; }
__attribute__((no_sanitize("undefined"))) int *quiet(int *p, long n) { nn(p); return p + n; }
"#;

fn rest(args: &[&str]) -> String {
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).collect();
    let dir = std::env::temp_dir().join(format!("rucc-ubsan-rest-{}{tag}", std::process::id()));
    let mut all = vec!["--target=x86_64-unknown-linux-gnu", "-S", "-o", "-"];
    all.extend(args);
    let out = compile(&dir, REST, &all);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn the_checks_of_attributes_steps_and_builtins_call_the_runtime() {
    let asked = "-fsanitize=nonnull-attribute,returns-nonnull-attribute,pointer-overflow,builtin";
    for level in ["-O0", "-O2"] {
        let text = rest(&[level, asked]);
        for (name, routine) in [
            ("call", "__ubsan_handle_nonnull_arg"),
            ("rn", "__ubsan_handle_nonnull_return_v1"),
            ("step", "__ubsan_handle_pointer_overflow"),
            ("clz", "__ubsan_handle_invalid_builtin"),
        ] {
            assert!(body(&text, name).contains(routine), "{level} {name}:\n{text}");
        }
        for name in ["plain", "zero", "named", "clz8", "quiet"] {
            assert!(!body(&text, name).contains("__ubsan"), "{level} {name}:\n{text}");
        }
        assert!(!text.contains("_abort"), "{level}:\n{text}");
    }
    let text = rest(&[asked, "-fno-sanitize-recover=all"]);
    assert!(body(&text, "call").contains("__ubsan_handle_nonnull_arg_abort"), "{text}");
    let text = rest(&["-O2", asked, "-fsanitize-trap=all"]);
    assert!(!body(&text, "step").contains("__ubsan"), "{text}");
    assert!(body(&text, "step").contains("ud2"), "{text}");
}

#[test]
fn object_size_is_checked_where_the_optimizer_sees_the_object() {
    // gcc answers how much is left of an object from `-O1` up, and so does this.
    let text = rest(&["-O0", "-fsanitize=object-size"]);
    assert!(!text.contains("__ubsan"), "{text}");
    let text = rest(&["-O2", "-fsanitize=object-size"]);
    assert!(body(&text, "past").contains("__ubsan_handle_type_mismatch_v1"), "{text}");
    assert!(!body(&text, "named").contains("__ubsan"), "{text}");
}

/// gcc's own probe of the checks [`REST`] has, with handlers that print what they are told and
/// a `main` that runs one case for each number it is given. A case may go on to read through
/// the pointer it was handed and stop there, so each case is a program of its own.
#[cfg(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "linux")))]
const PROBE: &str = r#"struct loc { const char *file; unsigned line, column; };
struct type { unsigned short kind, info; char name[]; };
struct tm { struct loc loc; struct type *type; unsigned char align, kind; };
struct nna { struct loc loc, attr; int index; };
struct nnr { struct loc loc; };
struct po { struct loc loc; };
struct ib { struct loc loc; unsigned char kind; };
int printf(const char *, ...); int fflush(void *);
static void pl(const char *w, struct loc *l) { printf("%s %s %u:%u\n", w, l->file ? "f" : "-", l->line, l->column); }
void __ubsan_handle_type_mismatch_v1(struct tm *d, unsigned long p) {
    pl("tm", &d->loc); printf(" %u %u %s %d\n", d->align, d->kind, d->type->name, p != 0); fflush(0);
}
void __ubsan_handle_nonnull_arg(struct nna *d) { pl("nna", &d->loc); pl(" attr", &d->attr); printf(" %d size %d\n", d->index, (int)sizeof *d); fflush(0); }
void __ubsan_handle_nonnull_return_v1(struct nnr *d, struct nnr *d2) { pl("nnr", &d->loc); pl(" at", &d2->loc); fflush(0); }
void __ubsan_handle_pointer_overflow(struct po *d, unsigned long b, unsigned long r) { pl("po", &d->loc); printf(" %lx %lx\n", b, r); fflush(0); }
void __ubsan_handle_invalid_builtin(struct ib *d) { pl("ib", &d->loc); printf(" %u\n", d->kind); fflush(0); }
__attribute__((nonnull)) void nn1(int *a, int *b) { (void)a; (void)b; }
__attribute__((nonnull(2))) void nn2(int *a, int x, int *b) { (void)a; (void)x; (void)b; }
__attribute__((returns_nonnull)) int *rn(int *p) { return p; }
int *padd(int *p, long n) { return p + n; }
int *psub(int *p, long n) { return p - n; }
int *pinc(int *p) { p++; return p; }
int *pcomp(int *p, long n) { p += n; return p; }
int pidx(int *p, long n) { return p[n]; }
struct S { int a; long b; int c[4]; };
long pmem(struct S *s) { return s->b; }
int pmemi(struct S *s, int i) { return s->c[i]; }
int *paddr(int *p, int i) { return &p[i]; }
int bclz(unsigned x) { return __builtin_clz(x); }
int bctz(unsigned long x) { return __builtin_ctzl(x); }
int glob[4];
int gidx(int i) { return glob[i]; }
int atoi(const char *);
int main(int argc, char **argv) {
    static int v[8];
    switch (atoi(argv[1])) {
    case 0: nn1(v, 0); nn2(0, 1, 0); printf("rn\n"); fflush(0); rn(0); break;
    case 1: padd((int *)-16, 8); psub((int *)16, 8); pinc((int *)-4); pcomp((int *)-16, 8); paddr((int *)-16, 8); break;
    case 2: pidx((int *)-16, 8); break;
    case 3: pmem((struct S *)-4); break;
    case 4: pmemi((struct S *)-8, 1); break;
    case 5: bclz(0); bctz(0); break;
    }
    return 0;
}
"#;

/// What gcc 13 at `-O0` tells the runtime for each case of [`PROBE`].
#[cfg(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "linux")))]
const PROBE_GCC: [&str; 6] = [
    "nna f 37:13\n attr - 0:0\n 2 size 40\nrn\nnnr - 0:0\n at f 19:59\n",
    "po f 20:38\n fffffffffffffff0 10\npo f 21:38\n 10 fffffffffffffff0\n\
     po f 22:22\n fffffffffffffffc 0\npo f 23:32\n fffffffffffffff0 10\n\
     po f 28:36\n fffffffffffffff0 10\n",
    "po f 24:36\n fffffffffffffff0 10\n",
    "po f 26:34\n fffffffffffffffc 4\n",
    "po f 27:44\n fffffffffffffff8 c\n",
    "ib f 29:31\n 1\nib f 30:36\n 0\n",
];

#[cfg(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "linux")))]
#[test]
fn the_runtime_is_told_what_gcc_tells_it_about_attributes_steps_and_builtins() {
    let dir = std::env::temp_dir().join(format!("rucc-ubsan-probe-{}", std::process::id()));
    let program = dir.join("one");
    let program_text = program.to_str().expect("a temporary path is text");
    let asked = "-fsanitize=nonnull-attribute,returns-nonnull-attribute,pointer-overflow,builtin";
    let out = compile(&dir, PROBE, &["-O0", "-w", asked, "-o", program_text]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    for (case, gcc) in PROBE_GCC.iter().enumerate() {
        let out = Command::new(&program).arg(case.to_string()).output().expect("the program runs");
        // gcc checks `s->c[i]` as one step from `s`, at the `[`. This checks the step to the
        // member at the `->` and the step to the element apart, and the first is the one that
        // wraps. That is the one difference.
        let wanted = if case == 4 { "po f 27:41\n fffffffffffffff8 8\n" } else { gcc };
        assert_eq!(String::from_utf8_lossy(&out.stdout), wanted, "case {case}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
