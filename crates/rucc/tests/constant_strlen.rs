//! Where `strlen` of a string gcc can read is a constant, and where the call is answered later.
//!
//! The kernel's `lib/tests/fortify_kunit.c` asks both questions in `fortify_test_known_sizes`.
//! gcc folds `strlen` of a const array with a string initializer while it reads the source, so
//! `__is_constexpr` says yes for it, and with optimization on it folds through a const pointer
//! initialized with one as well. It does not for an array the program can write, a pointer that
//! can change, a weak definition or a static array inside a function. `strlen` of a local array
//! is not a constant at all, but from `-O2` gcc knows what an initializer put there, even past
//! the stores KUnit makes through `test` in front of the call, and the test expects five.
//!
//! Every answer below is what gcc 14 gives for the same source. A constant is checked by
//! `_Static_assert`, and the local by a call to a function declared with `error` under a test the
//! right answer makes false.

use std::path::PathBuf;
use std::process::{Command, Output};

const SOURCE: &str = r#"
typedef unsigned long size_t;
#define __is_constexpr(x) (sizeof(int) == sizeof(*(8 ? ((void *)((long)(x) * 0l)) : (int *)8)))
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (want)) name(); \
    } while (0)
struct loc { int line; const char *file; };
static const char array_of_10[] = "this is 10";
static const char *ptr_of_11 = "this is 11!";
static const char *const unchanging_12 = "this is 12!!";
static char array_unknown[] = "compiler thinks I might change";
extern const char weak_6[] __attribute__((weak));
const char weak_6[] = "weak 6";
void f(struct loc *last) {
    char stack[80] = "Test!";
    static const char kept_4[] = "kept";
    _Static_assert(!__is_constexpr(__builtin_strlen(stack)), "stack");
    _Static_assert(__is_constexpr(__builtin_strlen("88888888")), "literal");
    _Static_assert(__builtin_strlen("88888888") == 8, "literal length");
    _Static_assert(__is_constexpr(__builtin_strlen(array_of_10)), "array");
    _Static_assert(__builtin_strlen(array_of_10 + 2) == 8, "array offset");
    _Static_assert(!__is_constexpr(__builtin_strlen(ptr_of_11)), "pointer");
    _Static_assert(!__is_constexpr(__builtin_strlen(array_unknown)), "unknown");
    _Static_assert(!__is_constexpr(__builtin_strlen(weak_6)), "weak");
    _Static_assert(!__is_constexpr(__builtin_strlen(kept_4)), "block scope static");
#ifdef __OPTIMIZE__
    _Static_assert(__is_constexpr(__builtin_strlen(unchanging_12)), "unchanging");
#else
    _Static_assert(!__is_constexpr(__builtin_strlen(unchanging_12)), "unchanging");
#endif
#ifdef STORES
    do {
        last->file = "one.c";
        last->line = __LINE__;
    } while (0);
    WANT(wrong_stack, __builtin_strlen(stack), 5);
#endif
}
"#;

fn compile(name: &str, flags: &[&str]) -> Output {
    let dir =
        std::env::temp_dir().join(format!("rucc-constant-strlen-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(flags)
        .args(["-c", "-o"])
        .arg(dir.join("one.o"))
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn strlen_is_a_constant_where_gcc_reads_the_string() {
    for level in ["-O0", "-O1", "-O2", "-Os", "-O3"] {
        let out = compile(&format!("constant{level}"), &[level]);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn strlen_of_a_local_is_read_past_stores_that_cannot_reach_it() {
    for level in ["-O2", "-O3"] {
        let out = compile(&format!("stores{level}"), &[level, "-DSTORES"]);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}
