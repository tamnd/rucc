//! What a call to a function carrying `__attribute__((error("...")))` or `warning("...")` gets said
//! about it, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! The attribute is a promise about the optimizer as much as about the call: gcc reports a call
//! that survives to code generation and nothing about one it took out. The kernel's
//! `BUILD_BUG_ON` is written against that, a call under a condition that is only a constant once
//! the inline function it is in has been inlined, so the only place the behaviour can be checked
//! is from the outside, over a source that needs the optimizer to settle it. What is compared is
//! what gcc 16 says for the same sources.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that what this reads is the same
/// on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-notices-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took that source with those flags, the listing and what it said.
fn compile(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(flags)
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), text, said)
}

/// The kernel's `BUILD_BUG_ON`, as `include/linux/compiler_types.h` and `build_bug.h` spell it,
/// inside an inline function whose argument is what settles it.
const KERNEL: &str = "\
#define __compiletime_error(msg) __attribute__((__error__(msg)))
#define ___compiletime_assert(condition, msg, prefix, suffix) \\
\tdo { \\
\t\t__attribute__((__noreturn__)) extern void prefix ## suffix(void) __compiletime_error(msg); \\
\t\tif (!(condition)) prefix ## suffix(); \\
\t} while (0)
#define _compiletime_assert(c, m, p, s) ___compiletime_assert(c, m, p, s)
#define compiletime_assert(c, m) _compiletime_assert(c, m, __compiletime_assert_, __COUNTER__)
#define BUILD_BUG_ON(c) compiletime_assert(!(c), \"BUILD_BUG_ON failed: \" #c)

static inline __attribute__((always_inline)) int shift(int by) {
\tBUILD_BUG_ON(by >= 32);
\treturn 1 << by;
}
int small(void) { return shift(3); }
";

#[test]
fn a_build_bug_on_the_inliner_settles_false_says_nothing() {
    for level in ["-O1", "-O2", "-Os"] {
        let (ok, text, said) = compile(&format!("kernel-fine{level}"), &[level], KERNEL);
        assert!(ok, "{level}: {said}");
        assert!(said.is_empty(), "{level}: {said}");
        assert!(!text.contains("__compiletime_assert_0"), "{level}: {text}");
    }
}

#[test]
fn a_build_bug_on_the_inliner_settles_true_is_an_error_quoting_the_message_at_the_call() {
    let source = format!("{KERNEL}int large(void) {{ return shift(40); }}\n");
    for level in ["-O1", "-O2"] {
        let (ok, _, said) = compile(&format!("kernel-bad{level}"), &[level], &source);
        assert!(!ok, "{level}: the call survived and was not refused");
        // The line of the `BUILD_BUG_ON`, which is where the call was written. gcc names the same
        // line, through a note about the macro the call is spelled in.
        let wanted = "one.c:12:2: error: call to '__compiletime_assert_0' declared with attribute \
                      error: BUILD_BUG_ON failed: by >= 32";
        assert!(said.contains(wanted), "{level}: {said}");
        assert_eq!(said.matches(": error: ").count(), 1, "{level}: {said}");
    }
}

/// A local's address handed to an inline function is not null, which is what landlock's
/// `copy_min_struct_from_user` checks with `BUILD_BUG_ON (!dst)`. gcc settles it the same way under
/// the kernel's `-fno-delete-null-pointer-checks`.
#[test]
fn a_build_bug_on_that_a_locals_address_is_null_says_nothing() {
    let source = format!(
        "{KERNEL}struct attr {{ int a, b; }};
static inline __attribute__((always_inline)) int copy(void *const dst, unsigned long size) {{
\tBUILD_BUG_ON(!dst);
\t__builtin_memset(dst, 0, size);
\treturn 0;
}}
int user(void) {{ struct attr at; copy(&at, sizeof(at)); return at.a; }}
"
    );
    for flags in [&["-O2"][..], &["-O2", "-fno-delete-null-pointer-checks"], &["-Os"]] {
        let (ok, _, said) = compile("local-address", flags, &source);
        assert!(ok, "{flags:?}: {said}");
        assert!(said.is_empty(), "{flags:?}: {said}");
    }
}

/// A member of a `const` table read at a constant index is what the table was initialized to, and
/// an address with a body here is not null. madera checks its mixer names that way, defined after
/// the check, and rtw89 its SAR handlers. A function only declared may be weak and stay undefined,
/// and gcc keeps that test under the kernel's flags too.
#[test]
fn a_build_bug_on_a_name_read_out_of_a_constant_table_says_nothing() {
    let source = format!(
        "{KERNEL}extern const char *const texts[3];
static int common(void) {{ return 0; }}
int declared(void);
struct handler {{ const char *descr; int factor; int (*query)(void); }};
static const struct handler handlers[2] = {{
\t[0] = {{ .descr = \"COMMON\", .factor = 2, .query = common }},
\t[1] = {{ .descr = \"ACPI\", .factor = 3, .query = declared }},
}};
void check(void) {{
\tint s = 0;
\tBUILD_BUG_ON(!texts[2]);
\tBUILD_BUG_ON(!handlers[s].descr);
\tBUILD_BUG_ON(!handlers[s].query);
\tBUILD_BUG_ON(handlers[1].descr == 0);
}}
void weak(void) {{ BUILD_BUG_ON(!handlers[1].query); }}
const char *const texts[] = {{ \"a\", \"b\", \"c\" }};
"
    );
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let flags = [level, "-fno-delete-null-pointer-checks"];
        let (ok, _, said) = compile(&format!("table-name{level}"), &flags, &source);
        assert!(!ok, "{level}: the test of a declared function was settled");
        assert_eq!(said.matches(": error: ").count(), 1, "{level}: {said}");
        assert!(said.contains("BUILD_BUG_ON failed: !handlers[1].query"), "{level}: {said}");
    }
}

/// A number cast to a pointer and back is the number, however wide, which is what lib/test_printf.c
/// leans on in `BUILD_BUG_ON (IS_ERR (PTR))` with a sixty four bit `PTR`.
#[test]
fn a_build_bug_on_a_wide_number_cast_to_a_pointer_and_back_says_nothing() {
    let source = format!(
        "{KERNEL}#define MAX_ERRNO 4095
static inline __attribute__((always_inline)) _Bool IS_ERR(const void *ptr) {{
\treturn __builtin_expect((unsigned long)(void *)((unsigned long)ptr) >= (unsigned long)-MAX_ERRNO, 0);
}}
#define PTR ((void *)0xffff0123456789abUL)
void errptr(void) {{ BUILD_BUG_ON(IS_ERR(PTR)); BUILD_BUG_ON(!IS_ERR((void *)-12L)); }}
"
    );
    for level in ["-O1", "-O2", "-Os"] {
        let (ok, _, said) = compile(&format!("wide-number{level}"), &[level], &source);
        assert!(ok, "{level}: {said}");
        assert!(said.is_empty(), "{level}: {said}");
    }
}

/// Two places in one object are in the order of how far into it they are, which is what mm/ksm.c
/// checks of a list head and the pointer inside it with `BUILD_BUG_ON`. The same goes for a local.
#[test]
fn a_build_bug_on_the_order_of_two_places_in_one_object_says_nothing() {
    let source = format!(
        "{KERNEL}struct list_head {{ struct list_head *next, *prev; }};
static struct list_head migrate_nodes = {{ &migrate_nodes, &migrate_nodes }};
#define DUP_HEAD ((struct list_head *)&migrate_nodes.prev)
int ksm(void) {{
	struct list_head here;
	BUILD_BUG_ON(DUP_HEAD <= &migrate_nodes);
	BUILD_BUG_ON(DUP_HEAD >= &migrate_nodes + 1);
	BUILD_BUG_ON((unsigned long)&here.prev < (unsigned long)&here.next);
	BUILD_BUG_ON(&here.prev == &here.next);
	return migrate_nodes.next == &migrate_nodes;
}}
"
    );
    for level in ["-O1", "-O2", "-Os"] {
        let (ok, _, said) = compile(&format!("one-object{level}"), &[level], &source);
        assert!(ok, "{level}: {said}");
        assert!(said.is_empty(), "{level}: {said}");
    }
}

/// A byte read out of a number written through a union is the byte the target puts first, which
/// is what fs/bcachefs/buckets.h checks of its lock bit with
/// `BUILD_BUG_ON(!((union ulong_byte_assert) { .ulong = 1UL << BUCKET_LOCK_BITNR }).byte)`. The
/// half and the byte after it come out of the same number.
#[test]
fn a_build_bug_on_a_byte_of_a_union_says_nothing() {
    let source = format!(
        "{KERNEL}union ulong_byte_assert {{ unsigned long ulong; unsigned char byte; }};
union halves {{ unsigned long ulong; unsigned short half; unsigned char bytes[8]; }};
void bucket_unlock(void) {{
	BUILD_BUG_ON(!((union ulong_byte_assert) {{ .ulong = 1UL << 0 }}).byte);
	BUILD_BUG_ON(((union halves) {{ .ulong = 0x30201UL }}).half != 0x201);
	BUILD_BUG_ON(((union halves) {{ .ulong = 0x30201UL }}).bytes[2] != 3);
}}
"
    );
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let (ok, _, said) = compile(&format!("union-byte{level}"), &[level], &source);
        assert!(ok, "{level}: {said}");
        assert!(said.is_empty(), "{level}: {said}");
    }
}

/// A case pins the value it switches on even in a function that tests that value in many blocks,
/// which is what `savic_read` in arch/x86/kernel/apic/x2apic_savic.c leans on with
/// `BUILD_BUG_ON(reg != APIC_ICR)` under `case APIC_ICR`, among four case ranges.
#[test]
fn a_build_bug_on_a_case_settles_in_a_switch_of_ranges_says_nothing() {
    let source = format!(
        "{KERNEL}extern int pr(unsigned);
static inline __attribute__((always_inline)) unsigned get(unsigned *ap, int reg) {{
\tBUILD_BUG_ON(reg != 0x300);
\treturn ap[reg];
}}
unsigned savic_read(unsigned *ap, unsigned reg) {{
\tswitch (reg) {{
\tcase 0x500 ... 0x530:
\t\treturn ap[reg];
\tcase 0x300:
\t\treturn get(ap, reg);
\tcase 0x100 ... 0x170:
\tcase 0x180 ... 0x1f0:
\t\tif (reg & 15)
\t\t\treturn pr(reg);
\t\treturn ap[reg];
\tcase 0x200 ... 0x274:
\t\tif ((reg & 15) && ((reg - 4) & 15))
\t\t\treturn pr(reg);
\t\treturn ap[reg];
\t}}
\treturn 1;
}}
"
    );
    for level in ["-O1", "-O2", "-Os"] {
        let (ok, _, said) = compile(&format!("case-ranges{level}"), &[level], &source);
        assert!(ok, "{level}: {said}");
        assert!(said.is_empty(), "{level}: {said}");
    }
}

/// A call in a branch the optimizer took out is not in the program, at every level: gcc folds an
/// `if (0)` at `-O0` as well, and so does this compiler.
#[test]
fn a_call_in_code_that_was_taken_out_says_nothing() {
    let source = "\
extern void bad(void) __attribute__((error(\"never\")));
void f(void) { if (0) bad(); }
void g(void) { if (sizeof(int) == 3) bad(); }
static inline void check(int n) { if (n > 4) bad(); }
void h(void) { check(2); }
";
    for level in ["-O0", "-O1", "-O2"] {
        let flags: &[&str] = &[level];
        let (ok, text, said) = compile(&format!("gone{level}"), flags, source);
        if level == "-O0" {
            // Nothing is inlined at `-O0`, so the copy of `check` is emitted and its call is one
            // the program makes, which gcc reports too.
            assert!(!ok, "{said}");
            assert!(said.contains("one.c:4:46: error: call to 'bad'"), "{said}");
            continue;
        }
        assert!(ok, "{level}: {said}");
        assert!(said.is_empty(), "{level}: {said}");
        // Every call to `check` went in, so gcc emits no copy of it and neither does this.
        assert!(!text.contains("check"), "{level}: {text}");
    }
}

#[test]
fn a_warning_attribute_is_a_warning_and_the_listing_is_still_written() {
    let source = "\
extern void meh(void) __attribute__((warning(\"meh is slow\")));
void f(int x) { if (x) meh(); }
";
    let (ok, text, said) = compile("warning", &["-O2"], source);
    assert!(ok, "{said}");
    let wanted = "one.c:2:24: warning: call to 'meh' declared with attribute warning: meh is slow";
    assert!(said.contains(wanted), "{said}");
    assert!(text.contains("meh"), "{text}");
    let (ok, _, said) = compile("werror", &["-O2", "-Werror"], source);
    assert!(!ok, "-Werror let a warning through: {said}");
}

/// A function may carry both, and a later declaration's message is the one a call is reported
/// with, which is what gcc does.
#[test]
fn both_attributes_are_said_and_the_latest_message_stands() {
    let source = "\
void c(void) __attribute__((error(\"one\"), warning(\"two\")));
void d(void) __attribute__((error(\"first\")));
void d(void) __attribute__((error(\"second\")));
void (*p)(void) = d;
void u(void) { c(); d(); p(); }
";
    let (ok, _, said) = compile("both", &["-O2"], source);
    assert!(!ok, "{said}");
    for wanted in [
        "one.c:5:16: error: call to 'c' declared with attribute error: one",
        "one.c:5:16: warning: call to 'c' declared with attribute warning: two",
        "one.c:5:21: error: call to 'd' declared with attribute error: second",
    ] {
        assert!(said.contains(wanted), "{wanted} in\n{said}");
    }
    // A call through a pointer is not a call to `d` as far as anyone can tell, and gcc says nothing
    // about it either.
    assert_eq!(said.matches("call to").count(), 3, "{said}");
}
