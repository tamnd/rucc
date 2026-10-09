//! A thread-local in a shared library that a program opens with `dlopen`, end to end.
//!
//! Issue #3279. The loader gives a library that `dlopen` opens its thread-local storage later, and
//! only through `__tls_get_addr`. A library that reaches its variables with the initial exec model
//! must fit in the small room glibc keeps in each thread's first block. 64 KB does not fit, so
//! `dlopen` refuses it with "cannot allocate memory in static TLS block". The cases here check that
//! the library rucc builds with `-fPIC` loads and gives each thread its own copy, at `-O0` and at
//! `-O2`, and that `-ftls-model=initial-exec` still gives the old model. On x86-64 the same library is built under `-mtls-dialect=gnu2` too.

#![cfg(all(
    target_os = "linux",
    target_env = "gnu",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The library: 64 KB of thread-local data, a variable each of the two dynamic models reaches, a
/// hidden one, and one that asks for the initial exec model and is never used.
const LIBRARY: &str = r#"
__thread char big[65536];
__thread int counter = 5;
static __thread int hidden_count;
__attribute__((visibility("hidden"))) __thread int shy = 40;
__thread int fast __attribute__((tls_model("initial-exec"))) = 7;

int bump(void)
{
    hidden_count++;
    big[100] += 2;
    return ++counter + hidden_count + big[100] + shy;
}

char *big_addr(void)
{
    return big;
}

int *counter_addr(void)
{
    return &counter;
}
"#;

/// The program: it opens the library, calls into it twice, and then does the same from a second
/// thread, which must start from the initial values again.
const PROGRAM: &str = r#"
#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>

static int (*bump)(void);
static char *(*big_addr)(void);

static void *worker(void *arg)
{
    int a = bump();
    int b = bump();
    printf("thread %d %d %d\n", a, b, big_addr() != (char *)arg);
    return 0;
}

int main(void)
{
    void *lib = dlopen("./libbig.so", RTLD_NOW);
    if (!lib) {
        printf("dlopen: %s\n", dlerror());
        return 1;
    }
    bump = (int (*)(void))dlsym(lib, "bump");
    big_addr = (char *(*)(void))dlsym(lib, "big_addr");
    int *(*counter_addr)(void) = (int *(*)(void))dlsym(lib, "counter_addr");
    int *counter = (int *)dlsym(lib, "counter");
    int a = bump();
    int b = bump();
    printf("main %d %d %d\n", a, b, counter == counter_addr());
    pthread_t t;
    pthread_create(&t, 0, worker, big_addr());
    pthread_join(t, 0);
    printf("main %d\n", bump());
    return 0;
}
"#;

/// What the program writes when every thread sees its own copy.
const RIGHT: &str = "main 49 53 1\nthread 49 53 1\nmain 57\n";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-dyntls-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// Runs the compiler in `dir` and stops the test if it refused.
fn rucc(dir: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// Builds the library with `flags` and the program beside it, runs the program, and gives what it
/// wrote and whether the library's dynamic section asks for static TLS.
fn opened(what: &str, flags: &[&str]) -> (String, bool) {
    let dir = dir(what);
    std::fs::write(dir.join("libbig.c"), LIBRARY).expect("the fixture can be written");
    std::fs::write(dir.join("main.c"), PROGRAM).expect("the fixture can be written");
    let mut args = vec!["-fPIC", "-shared", "-o", "libbig.so", "libbig.c"];
    args.extend_from_slice(flags);
    rucc(&dir, &args);
    rucc(&dir, &["-O2", "-o", "main", "main.c", "-ldl", "-lpthread"]);
    let out = Command::new(dir.join("main"))
        .current_dir(&dir)
        .output()
        .expect("what was linked can be run");
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let image = std::fs::read(dir.join("libbig.so")).expect("the library was written");
    let _ = std::fs::remove_dir_all(&dir);
    (said, static_tls(&image))
}

/// Whether the `DT_FLAGS` entry of a 64 bit little endian ELF file has `DF_STATIC_TLS` in it.
fn static_tls(image: &[u8]) -> bool {
    const DT_FLAGS: u64 = 30;
    const DF_STATIC_TLS: u64 = 0x10;
    const PT_DYNAMIC: u32 = 2;
    let u16_at = |at: usize| u16::from_le_bytes(image[at..at + 2].try_into().unwrap()) as usize;
    let u32_at = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(image[at..at + 8].try_into().unwrap());
    let phoff = u64_at(0x20) as usize;
    let (size, count) = (u16_at(0x36), u16_at(0x38));
    for header in (0..count).map(|index| phoff + index * size) {
        if u32_at(header) != PT_DYNAMIC {
            continue;
        }
        let (offset, length) = (u64_at(header + 8) as usize, u64_at(header + 32) as usize);
        for entry in (offset..offset + length).step_by(16) {
            if u64_at(entry) == DT_FLAGS {
                return u64_at(entry + 8) & DF_STATIC_TLS != 0;
            }
        }
    }
    false
}

/// The library loads, and each thread has its own copy of each variable, at both levels.
#[test]
fn a_library_with_64_kb_of_thread_locals_opens_and_each_thread_has_its_own() {
    for level in ["-O0", "-O2"] {
        let (said, fixed) = opened(&level[1..], &[level]);
        assert_eq!(said, RIGHT, "{level}");
        assert!(!fixed, "{level}: the library asks for static TLS");
    }
}

/// `-ftls-model=initial-exec` asks for the model that does not fit, and `dlopen` says so.
#[test]
fn the_initial_exec_model_still_asks_for_static_tls() {
    let (said, fixed) = opened("ie", &["-O2", "-ftls-model=initial-exec"]);
    assert!(fixed, "the library does not ask for static TLS");
    assert!(said.contains("cannot allocate memory in static TLS block"), "{said}");
}

/// `-mtls-dialect=gnu2` reaches each variable through its TLS descriptor and not through
/// `__tls_get_addr`. The library still loads, and each thread still has its own copy.
#[cfg(target_arch = "x86_64")]
#[test]
fn the_descriptor_dialect_opens_and_each_thread_has_its_own() {
    for level in ["-O0", "-O2"] {
        let (said, fixed) = opened(&format!("gnu2{level}"), &[level, "-mtls-dialect=gnu2"]);
        assert_eq!(said, RIGHT, "{level}");
        assert!(!fixed, "{level}: the library asks for static TLS");
    }
}
