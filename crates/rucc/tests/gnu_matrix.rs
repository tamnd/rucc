//! The rows of the GNU matrix whose tests are a whole program: the extensions `__has_extension`
//! is asked about, the attributes that exist to have something said at a use, and `nocommon`.
//!
//! Design: `spec/13-gnu-compat.md` sections 13.3 and 13.4, and `crates/rucc-gnu/features.toml`,
//! where each row names the test here that stands behind it.
//!
//! An extension row is only a claim that a program using the extension compiles to what gcc
//! compiles it to, so each test here is the smallest program that would come out differently if
//! it did not: a size, a value in an initializer, or an address. The warnings are compared
//! against gcc 16's wording, because a build that greps its log for them, or turns them into
//! errors with `-Werror=`, is reading gcc's words.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The target is written down rather than taken from the host, so that the sizes the tests look
/// at are the same wherever this runs.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-gnu-matrix-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler did with that source under those flags.
fn compile(what: &str, flags: &[&str], source: &str) -> Output {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    out
}

/// The IR for that source, which the compiler has to have accepted.
fn ir(what: &str, flags: &[&str], source: &str) -> String {
    let mut all = vec!["-O0", "--emit=ir"];
    all.extend_from_slice(flags);
    let out = compile(what, &all, source);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What the compiler said about a source it accepted as C23, which is the dialect that has the
/// standard's spellings of the attributes, with the IR thrown away.
fn said(what: &str, source: &str) -> String {
    let out = compile(what, &["--emit=ir", "-std=gnu23"], source);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The lines of what was said that contain a piece of text, which is how a test counts warnings
/// without depending on how the notes under them are laid out.
fn lines_with<'a>(said: &'a str, text: &str) -> Vec<&'a str> {
    said.lines().filter(|line| line.contains(text)).collect()
}

#[test]
fn an_array_of_no_elements_takes_no_room_and_its_address_is_the_end_of_the_record() {
    // The form the kernel wrote before C99 had flexible array members, and still writes in a
    // hundred headers: the record is as big as what is in front of the array, and the array
    // starts where the record ends. Both are constant expressions, so a wrong answer is a
    // refusal rather than a different number at run time.
    let got = ir(
        "zero",
        &[],
        "struct packet { int length; char data[0]; };\n\
         _Static_assert(sizeof(struct packet) == sizeof(int), \"no room\");\n\
         _Static_assert(__builtin_offsetof(struct packet, data) == 4, \"at the end\");\n\
         int none[0];\n\
         _Static_assert(sizeof none == 0, \"an array of nothing is nothing\");\n\
         char *data(struct packet *p) { return p->data; }\n",
    );
    assert!(got.contains("global @none : bytes 0"), "{got}");
    assert!(got.contains("iconst.i64 4"), "{got}");
}

#[test]
fn arithmetic_on_a_pointer_to_void_counts_in_bytes() {
    // GCC takes `void` to be one byte wide wherever a size is asked for, which is what makes
    // `p + n` on a `void *` move `n` bytes. The constant forms say so without running anything.
    let got = ir(
        "void",
        &[],
        "_Static_assert(sizeof(void) == 1, \"a byte\");\n\
         static char buffer[16];\n\
         char *third = (char *)((void *)buffer + 3);\n\
         void *step(void *p, long n) { return p + n; }\n\
         long apart(void *a, void *b) { return a - b; }\n",
    );
    assert!(got.contains("addr.8 @buffer + 3"), "{got}");
    assert!(got.contains("ptr_add %0, %1"), "{got}");
}

#[test]
fn a_range_in_a_designator_gives_every_element_in_it_the_value() {
    // `[first ... last] = value` is every element from the first to the last, both ends included,
    // and the array is as long as the last one named, which the initializer written out for the
    // object shows element by element.
    let got = ir(
        "range",
        &[],
        "static const int table[] = { [1 ... 3] = 7, [5] = 9 };\n\
         _Static_assert(sizeof table == 6 * sizeof(int), \"as long as the last one named\");\n\
         int first(void) { return table[0]; }\n\
         struct { char c[4]; } row = { .c[0 ... 2] = 'x' };\n",
    );
    assert!(got.contains("{ zero 4, i32 7, i32 7, i32 7, zero 4, i32 9 }"), "{got}");
    assert!(got.contains("{ i8 120, i8 120, i8 120, zero 1 }"), "{got}");
}

#[test]
fn a_name_marked_deprecated_is_said_at_every_use_in_gcc_s_words() {
    let got = said(
        "deprecated",
        "__attribute__((deprecated)) int f(void);\n\
         __attribute__((deprecated(\"use g2\"))) int g(void);\n\
         [[deprecated]] int h;\n\
         int (*fp)(void) = f;\n\
         int f(void) { return f() + g() + h; }\n\
         int fine(void) { return 0; }\n",
    );
    assert_eq!(lines_with(&got, "'f' is deprecated").len(), 2, "{got}");
    assert_eq!(lines_with(&got, "'g' is deprecated: use g2").len(), 1, "{got}");
    assert_eq!(lines_with(&got, "'h' is deprecated").len(), 1, "{got}");
    assert!(got.contains("declared here"), "{got}");
    assert!(!got.contains("'fine'"), "{got}");
}

#[test]
fn a_deprecated_name_can_be_used_quietly_under_the_option_that_turns_it_off() {
    let out = compile(
        "deprecated-off",
        &["--emit=ir", "-Wno-deprecated-declarations"],
        "__attribute__((deprecated)) int f(void);\nint g(void) { return f(); }\n",
    );
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

/// `unavailable` is `deprecated` made an error, at the same uses and in the same words, with no
/// option to turn it off. It outranks `deprecated` on one declaration and across two, the last
/// message `unavailable` gave is the one said, and a definition after it is not a use. The counts
/// are gcc 13's for this source.
#[test]
fn a_name_marked_unavailable_is_refused_at_every_use_in_gcc_s_words() {
    let source = "\
int f(void) __attribute__((unavailable));
int g(void) __attribute__((__unavailable__(\"use h\")));
typedef int T __attribute__((unavailable));
struct S { int m __attribute__((unavailable)); int n; } __attribute__((unavailable(\"old\")));
enum E { A __attribute__((unavailable)), B };
int a(void) __attribute__((deprecated(\"old\")));
int a(void) __attribute__((unavailable));
int b(void) __attribute__((unavailable(\"gone\"), deprecated(\"old\")));
int d(void) __attribute__((unavailable(\"u1\")));
int d(void) __attribute__((unavailable(\"u2\")));
[[gnu::unavailable]] int e(void);
int x(void) __attribute__((unavailable));
int x(void) { return B; }
int (*pf)(void) = f;
T t;
struct S s;
int use(struct S *p) { return f() + g() + p->m + p->n + A + a() + b() + d() + e(); }
_Static_assert(__has_attribute(unavailable), \"unavailable\");
";
    let flags = ["-fsyntax-only", "-std=gnu23", "-Wno-deprecated-declarations"];
    let out = compile("unavailable", &flags, source);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}");
    let count = |what: &str| lines_with(&said, &format!("error: {what}")).len();
    assert_eq!(count("'f' is unavailable"), 2, "{said}");
    assert_eq!(count("'g' is unavailable: use h"), 1, "{said}");
    assert_eq!(count("'T' is unavailable"), 1, "{said}");
    assert_eq!(count("'S' is unavailable: old"), 2, "{said}");
    assert_eq!(count("'m' is unavailable"), 1, "{said}");
    assert_eq!(count("'A' is unavailable"), 1, "{said}");
    assert_eq!(count("'a' is unavailable"), 1, "{said}");
    assert_eq!(count("'a' is unavailable:"), 0, "{said}");
    assert_eq!(count("'b' is unavailable: gone"), 1, "{said}");
    assert_eq!(count("'d' is unavailable: u2"), 1, "{said}");
    assert_eq!(count("'e' is unavailable"), 1, "{said}");
    assert_eq!(said.matches("error:").count(), 12, "{said}");
    assert!(!said.contains("deprecated") && !said.contains("'x'"), "{said}");
    assert!(said.contains("declared here"), "{said}");
}

#[test]
fn a_result_thrown_away_is_said_the_way_gcc_says_it_for_each_attribute() {
    let got = said(
        "unused",
        "__attribute__((warn_unused_result)) int u(void);\n\
         [[nodiscard]] int n1(void);\n\
         [[nodiscard(\"why\")]] int n2(void);\n\
         int plain(void);\n\
         int use(void) {\n\
             u();\n\
             (void)u();\n\
             (u(), 0);\n\
             n1();\n\
             (void)n1();\n\
             n2();\n\
             plain();\n\
             for (u(); 0; n1()) {}\n\
             int kept = u() + n1();\n\
             return kept;\n\
         }\n",
    );
    let u = "ignoring return value of 'u' declared with attribute 'warn_unused_result'";
    let n1 = "ignoring return value of 'n1', declared with attribute 'nodiscard'";
    let n2 = "ignoring return value of 'n2', declared with attribute 'nodiscard': \"why\"";
    // `u()`, `(void)u()`, the left of the comma and the first clause of the `for`: a cast to
    // `void` does not keep `warn_unused_result` quiet.
    assert_eq!(lines_with(&got, u).len(), 4, "{got}");
    // `n1()` and the step of the `for`, and not the one cast to `void`.
    assert_eq!(lines_with(&got, n1).len(), 2, "{got}");
    assert_eq!(lines_with(&got, n2).len(), 1, "{got}");
    assert!(!got.contains("'plain'"), "{got}");
}

#[test]
fn the_last_value_of_a_statement_expression_is_not_thrown_away_unless_the_whole_is() {
    let got = said(
        "stmt-expr",
        "__attribute__((warn_unused_result)) int u(void);\n\
         int use(void) {\n\
             int kept = ({ u(); });\n\
             ({ u(); });\n\
             ({ (void)0; u(); });\n\
             return kept;\n\
         }\n",
    );
    // The second only: gcc 14 is quiet about a statement expression with anything in front of
    // the call, which the kernel's `drmm_mutex_init` relies on.
    assert_eq!(lines_with(&got, "ignoring return value of 'u'").len(), 1, "{got}");
}

/// What the compiler said about a source it accepted, under these flags as well.
fn said_with(what: &str, flags: &[&str], source: &str) -> String {
    let mut all = vec!["--emit=ir"];
    all.extend_from_slice(flags);
    let out = compile(what, &all, source);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The `printf` calls gcc 16 has something to say about, one of each kind, beside ones it does
/// not.
///
/// `printf` and `scanf` are declared rather than taken from `stdio.h`, since the fixtures are
/// built for x86-64 Linux and a machine without that sysroot, the macOS runner, has no
/// `stdio.h` for it. Their formats are checked by name, as in gcc, so the declaration is enough.
const PRINTF: &str = "int printf(const char *, ...);\n\
     #include <stddef.h>\n\
     void f(int i, long l, double d, float g, void *v, char *s, size_t z, unsigned char *u) {\n\
         printf(\"%ld\\n\", i);\n\
         printf(\"%d\\n\", z);\n\
         printf(\"%d %d\\n\", i);\n\
         printf(\"%d\\n\", i, i);\n\
         printf(\"%Q\\n\");\n\
         printf(\"%s\\n\", v);\n\
         printf(\"%*d\\n\", l, i);\n\
         printf(\"%#d\\n\", i);\n\
         printf(\"%lp\\n\", v);\n\
         printf(\"%f %f %u %s %c %zu %p %m %%\\n\", d, g, i, u, s[0], z, s);\n\
         printf(s, i);\n\
         printf(\"\");\n\
     }\n";

#[test]
fn the_format_checks_are_quiet_until_wall_or_wformat_asks_for_them() {
    assert_eq!(said_with("format-quiet", &[], PRINTF), "");
    assert_eq!(said_with("format-off", &["-Wall", "-Wno-format"], PRINTF), "");
    let got = said_with("format-asked", &["-Wformat"], PRINTF);
    assert_eq!(lines_with(&got, "warning:").len(), 10, "{got}");
}

#[test]
fn a_printf_format_is_checked_against_its_arguments_in_gcc_s_words() {
    let got = said_with("format-printf", &["-Wall"], PRINTF);
    for line in [
        "format '%ld' expects argument of type 'long int', but argument 2 has type 'int'",
        // gcc says `'size_t' {aka 'long unsigned int'}` here, and this compiler keeps no node for
        // an ordinary typedef to say the first half from.
        "format '%d' expects argument of type 'int', but argument 2 has type 'long unsigned int'",
        "format '%d' expects a matching 'int' argument",
        "too many arguments for format",
        "unknown conversion type character 'Q' in format",
        "format '%s' expects argument of type 'char *', but argument 2 has type 'void *'",
        "field width specifier '*' expects argument of type 'int', but argument 2 has type 'long \
         int'",
        "'#' flag used with '%d' gnu_printf format",
        "use of 'l' length modifier with 'p' type character has either no effect or undefined \
         behavior",
        "zero-length gnu_printf format string",
    ] {
        assert_eq!(lines_with(&got, line).len(), 1, "{line}\n{got}");
    }
    // `%Q` reads nothing and was handed nothing, so nothing is left over, and the line of
    // conversions that are all right and the format that is a variable are quiet.
    assert_eq!(lines_with(&got, "warning:").len(), 10, "{got}");
    assert_eq!(lines_with(&got, "[E0786]").len(), 1, "{got}");
    assert_eq!(lines_with(&got, "[E0788]").len(), 1, "{got}");
}

#[test]
fn a_scanf_format_is_checked_against_the_pointers_it_writes_through() {
    let got = said_with(
        "format-scanf",
        &["-Wall"],
        "int scanf(const char *, ...);\n\
         void f(int *i, long *l, float *g, double *d, const char *c, char *s, unsigned *u) {\n\
             scanf(\"%d %ld %f %lf %u %d %s %5[a-z] %*d\", i, l, g, d, i, u, s, s);\n\
             scanf(\"%ld\", i);\n\
             scanf(\"%f\", d);\n\
             scanf(\"%s\", c);\n\
             scanf(\"%ms\", s);\n\
             scanf(\"%*d\", i);\n\
         }\n",
    );
    for line in [
        "format '%ld' expects argument of type 'long int *', but argument 2 has type 'int *'",
        "format '%f' expects argument of type 'float *', but argument 2 has type 'double *'",
        "writing into constant object (argument 2)",
        "format '%ms' expects argument of type 'char **', but argument 2 has type 'char *'",
        "too many arguments for format",
    ] {
        assert_eq!(lines_with(&got, line).len(), 1, "{line}\n{got}");
    }
    assert_eq!(lines_with(&got, "warning:").len(), 5, "{got}");
}

#[test]
fn a_format_attribute_and_format_arg_are_followed_the_way_gcc_follows_them() {
    let got = said_with(
        "format-attribute",
        &["-Wall"],
        "__attribute__((format(printf, 2, 3))) void log_to(int, const char *, ...);\n\
         __attribute__((format(printf, 1, 0))) void vlog(const char *, __builtin_va_list);\n\
         __attribute__((format_arg(1))) const char *tr(const char *);\n\
         int printf(const char *, ...);\n\
         void f(int c, long l, __builtin_va_list ap) {\n\
             log_to(1, \"%d\\n\", l);\n\
             vlog(\"%Q\", ap);\n\
             vlog(\"%d\", ap);\n\
             printf(tr(\"%s\\n\"), l);\n\
             printf(c ? \"%d\\n\" : \"%lu\\n\", l);\n\
             printf(\"%2$ld %1$d\\n\", c, l);\n\
             printf(\"%3$d\\n\", c, c, c);\n\
         }\n",
    );
    for line in [
        "format '%d' expects argument of type 'int', but argument 3 has type 'long int'",
        "unknown conversion type character 'Q' in format",
        "format '%s' expects argument of type 'char *', but argument 2 has type 'long int'",
        "format '%d' expects argument of type 'int', but argument 2 has type 'long int'",
        "format argument 1 unused before used argument 3 in '$'-style format",
        "format argument 2 unused before used argument 3 in '$'-style format",
    ] {
        assert_eq!(lines_with(&got, line).len(), 1, "{line}\n{got}");
    }
    assert_eq!(lines_with(&got, "warning:").len(), 6, "{got}");
}

#[test]
fn a_call_without_its_sentinel_is_said_under_wall() {
    let source = "#include <stddef.h>\n\
         __attribute__((sentinel)) void list(const char *, ...);\n\
         __attribute__((sentinel(1))) void pairs(const char *, ...);\n\
         void f(char *p) {\n\
             list(\"a\", \"b\", NULL);\n\
             list(\"a\", (char *)0);\n\
             list(\"a\", \"b\", 0);\n\
             list(\"a\", p);\n\
             list(\"a\");\n\
             pairs(\"a\", NULL, \"b\");\n\
             pairs(\"a\", NULL);\n\
         }\n";
    assert_eq!(said_with("sentinel-quiet", &[], source), "");
    let got = said_with("sentinel", &["-Wall"], source);
    assert_eq!(lines_with(&got, "missing sentinel in function call").len(), 2, "{got}");
    let short = "not enough variable arguments to fit a sentinel";
    assert_eq!(lines_with(&got, short).len(), 2, "{got}");
    assert_eq!(lines_with(&got, "warning:").len(), 4, "{got}");
}

#[test]
fn a_designated_init_structure_given_a_value_by_position_is_said_by_default() {
    let got = said_with(
        "designated-init",
        &[],
        "struct __attribute__((designated_init)) ops { int (*open)(void); int flags; };\n\
         struct plain { int a, b; };\n\
         int open_it(void);\n\
         struct ops a = { .open = open_it, .flags = 1 };\n\
         struct ops b = { open_it, 1 };\n\
         struct ops c = { 0 };\n\
         struct ops d = { };\n\
         struct plain e = { 1, 2 };\n",
    );
    let line = "positional initialization of field in 'struct' declared with 'designated_init' \
                attribute";
    assert_eq!(lines_with(&got, line).len(), 3, "{got}");
    assert_eq!(lines_with(&got, "warning:").len(), 3, "{got}");
    let quiet = compile(
        "designated-init-off",
        &["--emit=ir", "-Wno-designated-init"],
        "struct __attribute__((designated_init)) s { int x; };\nstruct s v = { 1 };\n",
    );
    assert_eq!(String::from_utf8_lossy(&quiet.stderr), "");
}

/// Which of `names` are offered to the linker to merge, in IR the compiler wrote.
fn merged<'a>(ir: &str, names: &[&'a str]) -> Vec<&'a str> {
    let line = |name: &str| {
        ir.lines()
            .find(|line| line.contains(&format!("@{name} ")) || line.contains(&format!("@{name}:")))
            .unwrap_or_else(|| panic!("no global {name} in\n{ir}"))
            .to_owned()
    };
    names.iter().copied().filter(|name| line(name).contains("common")).collect()
}

/// gcc 13's answers: `common` puts a tentative definition in the common block whatever the command
/// line says, the last declaration of the name that is not `extern` says whether it does, and
/// `weak`, an initializer and a section of its own keep it out.
#[test]
fn common_puts_a_tentative_definition_in_the_common_block_under_fno_common() {
    let source = "\
__attribute__((common)) int written;
int plain;
int later; int later __attribute__((common));
int first __attribute__((common)); int first;
int kept __attribute__((common)); extern int kept;
extern int declared __attribute__((common)); int declared;
__attribute__((common)) int weakly __attribute__((weak));
__attribute__((common)) int set = 1;
__attribute__((common, section(\"apart\"))) int placed;
";
    let names =
        ["written", "plain", "later", "first", "kept", "declared", "weakly", "set", "placed"];
    let got = ir("common", &["-fno-common"], source);
    assert_eq!(merged(&got, &names), ["written", "later", "kept"], "{got}");
}

/// Of `common` and `nocommon` the one gcc applies first stands and the other is ignored with its
/// warning: the first in a list, the one after the declarator over the one before it, and the
/// earlier declaration over the later one, which then says no more than a plain one would.
#[test]
fn common_and_nocommon_together_keep_the_one_gcc_applies_first() {
    let source = "\
int listed __attribute__((common, nocommon));
__attribute__((common)) int around __attribute__((nocommon));
int twice __attribute__((nocommon)); int twice __attribute__((common));
";
    let out = compile("common-nocommon", &["-O0", "--emit=ir", "-fcommon"], source);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    let got = String::from_utf8_lossy(&out.stdout);
    assert_eq!(merged(&got, &["listed", "around", "twice"]), ["listed", "twice"], "{got}");
    let ignored = |now: &str, before: &str| {
        let line = format!(
            "warning: ignoring attribute '{now}' because it conflicts with attribute '{before}'"
        );
        lines_with(&said, &line).len()
    };
    assert_eq!(ignored("nocommon", "common"), 1, "{said}");
    assert_eq!(ignored("common", "nocommon"), 2, "{said}");
    assert_eq!(lines_with(&said, "previous declaration here").len(), 1, "{said}");
}

/// gcc 13's checks and words, in its order: the argument count, then whether the attribute is on a
/// thread-local variable, which only warns and drops it, and then the string.
#[test]
fn tls_model_is_checked_the_way_gcc_checks_it() {
    let source = "\
__thread int a __attribute__((tls_model(\"bogus\")));
int b __attribute__((tls_model(\"initial-exec\")));
__thread int c __attribute__((tls_model(1)));
void f(void) __attribute__((tls_model(\"initial-exec\")));
__thread int d __attribute__((tls_model));
__thread int g __attribute__((tls_model(\"global-dynamic\", \"x\")));
int b2 __attribute__((tls_model(\"bogus\")));
struct s { int x __attribute__((tls_model(\"initial-exec\"))); };
void k(void) { int l __attribute__((tls_model(\"initial-exec\"))); (void)l; }
";
    let out = compile("tls-model-checked", &["-fsyntax-only"], source);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}");
    let count = |text: &str| lines_with(&said, text).len();
    let models = "error: 'tls_model' argument must be one of 'local-exec', 'initial-exec', \
                  'local-dynamic', or 'global-dynamic'";
    assert_eq!(count(models), 1, "{said}");
    assert_eq!(count("error: 'tls_model' argument not a string"), 1, "{said}");
    let arity = "error: wrong number of arguments specified for 'tls_model' attribute";
    assert_eq!(count(arity), 2, "{said}");
    assert_eq!(count("expected 1, found 0"), 1, "{said}");
    assert_eq!(count("expected 1, found 2"), 1, "{said}");
    let ignored =
        |why: &str| count(&format!("warning: 'tls_model' attribute ignored because {why}"));
    assert_eq!(ignored("'b' does not have thread storage duration"), 1, "{said}");
    assert_eq!(ignored("'b2' does not have thread storage duration"), 1, "{said}");
    assert_eq!(ignored("'l' does not have thread storage duration"), 1, "{said}");
    assert_eq!(ignored("'f' is not a variable"), 1, "{said}");
    assert_eq!(ignored("'x' is not a variable"), 1, "{said}");
    assert_eq!(count("error:"), 4, "{said}");
    assert_eq!(count("warning:"), 5, "{said}");
}

/// Each of the four models is taken without a word, and glibc's `initial-exec` is the sequence
/// it asks for, reading the offset out of the global offset table.
#[test]
fn a_thread_local_written_initial_exec_is_reached_through_the_initial_exec_sequence() {
    let source = "\
extern __thread int h __attribute__((tls_model(\"initial-exec\")));
__thread int e __attribute__((__tls_model__(\"local-exec\")));
static __thread int i __attribute__((tls_model(\"local-dynamic\")));
extern __thread int j __attribute__((tls_model(\"global-dynamic\")));
int use(void) { return h + e + i + j; }
_Static_assert(__has_attribute(tls_model), \"tls_model\");
";
    let out = compile("tls-model-ie", &["-O2", "-S"], source);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert_eq!(said, "");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("h@GOTTPOFF(%rip)"), "{text}");
    assert!(!text.contains("__tls_get_addr"), "{text}");
}

/// `noipa` keeps a call a call: nothing is inlined, no constant goes in for the parameter and
/// nothing comes back out as the answer, `always_inline` beside it included, and a declaration
/// that wrote it covers the definition after it. `plain` is the same body without the attribute,
/// which is folded to the constant, so the test is of the attribute and not of the optimizer.
#[test]
fn noipa_keeps_every_call_to_the_function_a_call() {
    let source = "\
__attribute__((noipa)) static int bump(int x) { return x + 1; }
int one(void) { return bump(41); }
static int plain(int x) { return x + 1; }
int two(void) { return plain(99); }
__attribute__((noipa, always_inline)) static inline int twice(int x) { return x * 2; }
int three(int y) { return twice(y); }
__attribute__((__noipa__)) static int later(int);
static int later(int x) { return x - 1; }
int four(void) { return later(7); }
[[gnu::noipa]] static void nothing(void) {}
int five(void) { nothing(); return 5; }
_Static_assert(__has_attribute(noipa), \"noipa\");
";
    let out = compile("noipa", &["-O2", "-S", "-std=gnu23"], source);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    let text = String::from_utf8_lossy(&out.stdout);
    let body = |name: &str| -> String {
        let start = text.find(&format!("\n{name}:")).unwrap_or_else(|| panic!("{name}\n{text}"));
        let rest = &text[start + 1..];
        let end = rest.find(".size").unwrap_or(rest.len());
        rest[..end].to_string()
    };
    let calls = |body: &str, callee: &str| {
        body.lines().any(|line| {
            let line = line.trim();
            (line.starts_with("call") || line.starts_with("jmp")) && line.ends_with(callee)
        })
    };
    assert!(body("two").contains("$100"), "{text}");
    for (caller, callee) in
        [("one", "bump"), ("three", "twice"), ("four", "later"), ("five", "nothing")]
    {
        assert!(calls(&body(caller), callee), "{caller} calls {callee}\n{text}");
    }
    assert!(!body("one").contains("$42"), "{text}");
    assert!(!body("four").contains("$6,"), "{text}");
    let bump = body("bump");
    assert!(bump.contains("%edi") || bump.contains("%rdi"), "{text}");
}

/// `nocf_check` under `-fcf-protection=branch`, measured against gcc 13: a function of the type
/// opens without `endbr64`, and a call or a tail jump through a pointer to one carries `notrack`,
/// whether the attribute was written after the declarator, beside the star or in a typedef. A
/// call through a plain pointer is left alone, and so is a function without the attribute.
#[test]
fn nocf_check_leaves_out_the_landing_pad_and_calls_through_a_pointer_with_notrack() {
    let source = "\
__attribute__((nocf_check)) void quiet(void) {}
void loud(void) {}
void (*p1)(void) __attribute__((nocf_check));
void (__attribute__((nocf_check)) *p2)(void);
typedef void (*fp)(void) __attribute__((__nocf_check__));
fp p3;
void (*plain)(void);
int calls(void) { p1(); p2(); plain(); return 1; }
void tail(void) { p3(); }
void late(void) { p1(); plain(); }
_Static_assert(__has_attribute(nocf_check), \"nocf_check\");
";
    let out = compile("nocf-check", &["-O2", "-S", "-fcf-protection=branch"], source);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && said.is_empty(), "{said}");
    let text = String::from_utf8_lossy(&out.stdout);
    let body = |name: &str| -> Vec<String> {
        let start = text.find(&format!("\n{name}:")).unwrap_or_else(|| panic!("{name}\n{text}"));
        let rest = &text[start + 1..];
        let end = rest.find(".size").unwrap_or(rest.len());
        rest[..end].lines().map(|line| line.trim().replace('\t', " ")).collect()
    };
    let count =
        |name: &str, start: &str| body(name).iter().filter(|l| l.starts_with(start)).count();
    assert_eq!(count("quiet", "endbr64"), 0, "{text}");
    for name in ["loud", "calls", "tail", "late"] {
        assert_eq!(count(name, "endbr64"), 1, "{name}\n{text}");
    }
    assert_eq!(count("calls", "notrack call *%"), 2, "{text}");
    assert_eq!(count("calls", "call *%"), 1, "{text}");
    assert_eq!(count("tail", "notrack jmp *%"), 1, "{text}");
    assert_eq!(count("late", "notrack call *%"), 1, "{text}");
    assert_eq!(count("late", "jmp *%"), 1, "the plain tail jump has no prefix\n{text}");

    // The object the compiler writes itself, whose call through `p1` starts with the prefix byte
    // in front of the call's opcode.
    let object = compile("nocf-check-object", &["-c", "-fcf-protection=branch"], source);
    assert!(object.status.success(), "{}", String::from_utf8_lossy(&object.stderr));
    assert!(object.stdout.windows(2).any(|pair| pair == [0x3e, 0xff]), "no notrack call");
}

/// What gcc 13 says about `nocf_check` written where it means nothing, with too many arguments,
/// on one declaration of a function and not the other, and on a pointer assigned to a plain one,
/// which is an error from gcc 14 on and a warning before. Without `-fcf-protection` it is ignored with a warning and the code is the plain code.
#[test]
fn nocf_check_is_checked_and_kept_in_the_type_in_gcc_s_words() {
    let source = "\
int v __attribute__((nocf_check));
__attribute__((nocf_check(1))) void counted(void);
void once(void);
__attribute__((nocf_check)) void once(void);
void (__attribute__((nocf_check)) *untracked)(void);
void use(void) { void (*plain)(void) = untracked; plain(); }
";
    let out = compile("nocf-check-said", &["-S", "-fcf-protection=full"], source);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}");
    let expected = [
        (1, "warning: 'nocf_check' attribute only applies to function types"),
        (2, "error: wrong number of arguments specified for 'nocf_check' attribute"),
        (4, "error: conflicting types for 'once'"),
        (
            6,
            "initialization of 'void (*)(void)' from incompatible pointer type \
             'void (__attribute__((nocf_check)) *)(void)'",
        ),
    ];
    for (line, what) in expected {
        let at = format!(".c:{line}:");
        assert!(
            said.lines().any(|said| said.contains(&at) && said.contains(what)),
            "missing {what:?} on line {line} in\n{said}"
        );
    }
    assert_eq!(said.matches("expected 0, found 1").count(), 1, "{said}");

    let source = "\
__attribute__((nocf_check)) void quiet(void) {}
void (*p1)(void) __attribute__((nocf_check));
void calls(void) { p1(); }
";
    let out = compile("nocf-check-ignored", &["-O2", "-S"], source);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    let ignored =
        "warning: 'nocf_check' attribute ignored. Use '-fcf-protection' option to enable it";
    assert_eq!(said.matches(ignored).count(), 2, "{said}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("notrack") && !text.contains("endbr64"), "{text}");
}

/// `cf_check` under `-fcf-protection=branch -mmanual-endbr`, measured against gcc 13: only a
/// function that says it opens with `endbr64`, whichever of its declarations said it, and a
/// static function whose address is taken is left without one like the rest. `nocf_check` beside
/// it wins, and the pads at labels whose address is taken stay. Without `-mmanual-endbr` the
/// attribute changes nothing.
#[test]
fn cf_check_under_manual_endbr_is_the_only_function_with_a_landing_pad() {
    let source = "\
void plain(void) {}
static void hidden(void) {}
void (*taken)(void) = hidden;
__attribute__((cf_check)) void checked(void) {}
void declared(void) __attribute__((__cf_check__));
void declared(void) {}
void later(void);
__attribute__((cf_check)) void later(void) {}
__attribute__((cf_check, nocf_check)) void both(void) {}
int jump(int x) { static void *t[] = { &&a, &&b }; goto *t[x]; a: return 1; b: return 2; }
_Static_assert(__has_attribute(cf_check), \"cf_check\");
";
    // Without `-fcf-protection` the one thing said is gcc's warning about `nocf_check`.
    let pads = |what: &str, flags: &[&str]| -> Vec<(&'static str, usize)> {
        let out = compile(what, flags, source);
        let said = String::from_utf8_lossy(&out.stderr);
        let ignored = "'nocf_check' attribute ignored";
        let quiet = flags.iter().any(|flag| flag.starts_with("-fcf-protection"));
        assert!(out.status.success(), "{said}");
        let told = said.lines().filter(|l| l.contains("warning:") || l.contains("error:"));
        assert_eq!(told.count(), usize::from(!quiet), "{said}");
        assert!(quiet || said.contains(ignored), "{said}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        ["plain", "hidden", "checked", "declared", "later", "both", "jump"]
            .into_iter()
            .map(|name| {
                let start =
                    text.find(&format!("\n{name}:")).unwrap_or_else(|| panic!("{name}\n{text}"));
                let rest = &text[start + 1..];
                let end = rest.find(".size").unwrap_or(rest.len());
                (name, rest[..end].lines().filter(|l| l.trim() == "endbr64").count())
            })
            .collect()
    };
    let manual = pads("cf-check-manual", &["-S", "-fcf-protection=branch", "-mmanual-endbr"]);
    let want = [
        ("plain", 0),
        ("hidden", 0),
        ("checked", 1),
        ("declared", 1),
        ("later", 1),
        ("both", 0),
        ("jump", 2),
    ];
    assert_eq!(manual, want);
    let every = pads("cf-check-every", &["-S", "-fcf-protection=branch"]);
    let want = [
        ("plain", 1),
        ("hidden", 1),
        ("checked", 1),
        ("declared", 1),
        ("later", 1),
        ("both", 0),
        ("jump", 3),
    ];
    assert_eq!(every, want);
    let none = pads("cf-check-none", &["-S", "-mmanual-endbr"]);
    assert!(none.iter().all(|&(_, count)| count == 0), "{none:?}");
}

/// What gcc 13 says about `cf_check` on something that is not a function and with an argument.
/// A function that says it is taken without a word, with `-fcf-protection` or without it.
#[test]
fn cf_check_is_checked_in_gcc_s_words() {
    let source = "\
int v __attribute__((cf_check));
typedef void fn(void) __attribute__((cf_check));
void (*p)(void) __attribute__((cf_check));
__attribute__((cf_check(1))) void counted(void);
__attribute__((cf_check)) void fine(void) {}
";
    let out = compile("cf-check-said", &["-S"], source);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}");
    let expected = [
        (1, "warning: 'cf_check' attribute only applies to functions"),
        (2, "warning: 'cf_check' attribute only applies to functions"),
        (3, "warning: 'cf_check' attribute only applies to functions"),
        (4, "error: wrong number of arguments specified for 'cf_check' attribute"),
    ];
    for (line, what) in expected {
        let at = format!(".c:{line}:");
        assert!(
            said.lines().any(|said| said.contains(&at) && said.contains(what)),
            "missing {what:?} on line {line} in\n{said}"
        );
    }
    assert_eq!(said.matches("expected 0, found 1").count(), 1, "{said}");
    assert!(!said.contains(".c:5:"), "{said}");
}

#[test]
fn nocommon_keeps_a_tentative_definition_out_of_the_common_block_under_fcommon() {
    let got = ir("nocommon", &["-fcommon"], "int merged;\n__attribute__((nocommon)) int alone;\n");
    let line = |name: &str| {
        got.lines()
            .find(|line| line.contains(&format!("@{name} ")) || line.contains(&format!("@{name}:")))
            .unwrap_or_else(|| panic!("no global {name} in\n{got}"))
            .to_owned()
    };
    assert!(line("merged").contains("common"), "{got}");
    assert!(!line("alone").contains("common"), "{got}");
}

#[test]
fn the_hints_gcc_answers_yes_for_are_taken_without_a_word() {
    // Each of these asks gcc to assume or to try something, and none of them changes what a
    // correct program does. They are taken and dropped, and the program is the same one.
    let got = said(
        "hints",
        "#include <stddef.h>\n\
         __attribute__((malloc)) void *make(size_t);\n\
         void unmake(void *);\n\
         __attribute__((malloc, malloc(unmake, 1))) void *make2(size_t);\n\
         __attribute__((returns_nonnull)) char *name(void);\n\
         __attribute__((format_arg(1))) const char *tr(const char *);\n\
         __attribute__((access(read_only, 1, 2))) int sum(const int *, int);\n\
         __attribute__((assume_aligned(16))) void *aligned(void);\n\
         __attribute__((flatten)) int all(void) { return sum(0, 0); }\n\
         __attribute__((sentinel)) void list(const char *, ...);\n\
         struct __attribute__((designated_init)) point { int x, y; };\n\
         struct point origin = { .x = 0, .y = 0 };\n",
    );
    assert_eq!(got, "");
}

#[test]
fn the_promises_and_the_tool_hints_gcc_answers_yes_for_are_taken_without_a_word() {
    // Each of these is a promise gcc may optimize on, a request about a transformation this
    // compiler never makes, or a word to a tool it does not have: a sanitizer, the analyzer or
    // the debugger. None of them changes what a correct program does, `__has_attribute` answers
    // yes for each as gcc 13 does, and they are taken without a word.
    let got = said(
        "promises",
        "#include <stddef.h>\n\
         __attribute__((nothrow, leaf)) int get(void);\n\
         __attribute__((__nothrow__, __leaf__)) int get2(void);\n\
         __attribute__((artificial, always_inline)) static inline int twice(int x) {\n\
           return 2 * x;\n\
         }\n\
         __attribute__((alloc_align(2), malloc)) void *grab(size_t, size_t);\n\
         __attribute__((noplt)) int far(void);\n\
         __attribute__((no_icf)) int same(void) { return 1; }\n\
         __attribute__((no_sanitize_address, no_address_safety_analysis, no_sanitize_thread,\n\
                        no_sanitize_undefined, no_sanitize_coverage))\n\
         int raw(int *p) { return *p; }\n\
         __attribute__((no_split_stack, no_stack_limit)) int deep(int x) {\n\
           return x ? deep(x - 1) : 0;\n\
         }\n\
         __attribute__((tainted_args)) int handle(int cmd);\n\
         __attribute__((fd_arg(1), fd_arg_read(2), fd_arg_write(3))) int pass(int, int, int);\n\
         int use(void) {\n\
           return get() + get2() + twice(far()) + handle(0) + pass(0, 1, 2) + (grab(8, 16) != 0);\n\
         }\n\
         #define HAS(name) _Static_assert(__has_attribute(name), #name)\n\
         HAS(nothrow); HAS(leaf); HAS(artificial); HAS(alloc_align); HAS(noplt); HAS(no_icf);\n\
         HAS(no_sanitize_address); HAS(no_address_safety_analysis);\n\
         HAS(no_sanitize_thread); HAS(no_sanitize_undefined); HAS(no_sanitize_coverage);\n\
         HAS(no_split_stack); HAS(no_stack_limit); HAS(tainted_args); HAS(fd_arg);\n\
         HAS(fd_arg_read); HAS(fd_arg_write);\n",
    );
    assert_eq!(got, "");
}
