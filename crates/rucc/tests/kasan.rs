//! `-fsanitize=kernel-address`, which the kernel's `CONFIG_KASAN_GENERIC` puts on every unit that
//! does not opt out. Each access reads its shadow byte and calls the runtime gcc names when the
//! byte says the access is bad, a function marked `no_sanitize_address` makes no checks, and the
//! `--param` settings the kernel gives are read.

use std::path::Path;
use std::process::Command;

/// One access of each size the inline check has, a copy whose length the program works out, and
/// a function that asks for no checks.
const SOURCE: &str = r#"typedef unsigned long u64;
__attribute__((noinline)) int ld4(int *p) { return *p; }
__attribute__((noinline)) void st4(int *p, int v) { *p = v; }
__attribute__((noinline)) char ld1(char *p) { return *p; }
__attribute__((noinline)) long ld8(long *p) { return *p; }
__attribute__((noinline)) void st16(__int128 *p) { *p = 0; }
__attribute__((noinline)) void cp(void *d, const void *s, u64 n) { __builtin_memcpy(d, s, n); }
__attribute__((no_sanitize_address)) int quiet(int *p) { return *p; }
#ifdef __SANITIZE_ADDRESS__
int sanitizes_address;
#endif
"#;

/// The runtime, which counts the reports and keeps the last one, and a `main` that maps a buffer
/// and its shadow at fixed addresses, poisons parts of the shadow and checks what was reported.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
const RUNS: &str = r#"
void *mmap(void *, u64, int, int, int, long);
#define BUF ((char *)0x200000000000UL)
#define SHADOW ((signed char *)((0x200000000000UL >> 3) + 0x100000000000UL))
#define QUIET __attribute__((no_sanitize_address))
static int reports, kind;
static void *last;
QUIET static void report(void *p, int k) { reports++; last = p; kind = k; }
QUIET void __asan_report_load1_noabort(void *p) { report(p, 1); }
QUIET void __asan_report_load4_noabort(void *p) { report(p, 4); }
QUIET void __asan_report_store4_noabort(void *p) { report(p, 14); }
QUIET void __asan_report_load8_noabort(void *p) { report(p, 8); }
QUIET void __asan_report_store16_noabort(void *p) { report(p, 116); }
QUIET void *__asan_memcpy(void *d, const void *s, u64 n) {
    char *to = d;
    const char *from = s;
    report(d, 99);
    while (n--)
        *to++ = *from++;
    return d;
}
QUIET static int saw(int count, void *p, int k) { return reports == count && last == p && kind == k; }
QUIET int main(void) {
    if (mmap(BUF, 1 << 16, 3, 0x32, -1, 0) != BUF || mmap(SHADOW, 1 << 13, 3, 0x32, -1, 0) != SHADOW)
        return 40;
    *(int *)(BUF + 12) = 7;
    if (ld4((int *)BUF) != 0 || ld1(BUF + 15) != 0 || ld8((long *)BUF) != 0 || reports != 0)
        return 41;
    SHADOW[1] = 4;
    if (ld4((int *)(BUF + 8)) != 0 || ld1(BUF + 11) != 0 || reports != 0)
        return 42;
    if (ld4((int *)(BUF + 12)) != 7 || !saw(1, BUF + 12, 4))
        return 43;
    if (ld1(BUF + 12) != 7 || !saw(2, BUF + 12, 1))
        return 44;
    st4((int *)(BUF + 12), 9);
    if (!saw(3, BUF + 12, 14) || *(int *)(BUF + 12) != 9)
        return 45;
    SHADOW[2] = -1;
    if (ld8((long *)(BUF + 16)) != 0 || !saw(4, BUF + 16, 8))
        return 46;
    st16((__int128 *)(BUF + 32));
    if (reports != 4)
        return 47;
    st16((__int128 *)(BUF + 16));
    if (!saw(5, BUF + 16, 116))
        return 48;
    cp(BUF + 64, BUF + 12, 4);
    if (!saw(6, BUF + 64, 99) || *(int *)(BUF + 64) != 9)
        return 49;
    return quiet((int *)(BUF + 12)) == 9 && reports == 6 ? 0 : 50;
}
"#;

/// The flags the kernel gives for `CONFIG_KASAN_INLINE` on x86_64, less the code model.
const KERNEL: [&str; 6] = [
    "-fsanitize=kernel-address",
    "-fasan-shadow-offset=0xdffffc0000000000",
    "--param",
    "asan-instrumentation-with-call-threshold=10000",
    "--param",
    "asan-kernel-mem-intrinsic-prefix=1",
];

fn compile(dir: &Path, source: &str, args: &[&str]) -> std::process::Output {
    std::fs::create_dir_all(dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run")
}

/// The listing for that target and those flags, and the lines of one function in it.
fn listing(target: &str, args: &[&str], function: &str) -> (String, String) {
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).take(40).collect();
    let dir = std::env::temp_dir().join(format!("rucc-kasan-{}-{target}{tag}", std::process::id()));
    let mut all = vec![format!("--target={target}"), "-S".into(), "-o".into(), "-".into()];
    all.extend(args.iter().map(|&arg| arg.to_owned()));
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let out = compile(&dir, SOURCE, &all);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{target} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    let start = text.find(&format!("\n{function}:")).expect("the function is defined");
    let end = text[start..].find(&format!(".size\t{function}")).map_or(text.len(), |at| start + at);
    let lines = text[start..end].to_owned();
    (text, lines)
}

/// What the command line says, when it is refused.
fn refused(args: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-kasan-refused-{}", std::process::id()));
    let out = compile(&dir, SOURCE, args);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "{args:?} is refused");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn every_access_reads_its_shadow_and_reports_with_gcc_names() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for level in ["-O0", "-O2"] {
            let mut args = vec![level];
            args.extend(KERNEL);
            let (text, quiet) = listing(target, &args, "quiet");
            for wanted in [
                "__asan_report_load4_noabort",
                "__asan_report_store4_noabort",
                "__asan_report_load1_noabort",
                "__asan_report_load8_noabort",
                "__asan_report_store16_noabort",
                "__asan_memcpy",
            ] {
                assert!(text.contains(wanted), "{target} {level} has no {wanted}:\n{text}");
            }
            assert!(!text.contains("__asan_load"), "{target} {level}:\n{text}");
            assert!(!quiet.contains("__asan"), "{target} {level}:\n{quiet}");
            assert!(text.contains("sanitizes_address"), "{target} {level}: the macro is defined");
        }
    }
    let mut args = vec!["-O2"];
    args.extend(KERNEL);
    let (_, ld8) = listing("x86_64-unknown-linux-gnu", &args, "ld8");
    assert!(ld8.contains("$-2305847407260205056"), "the offset is the one asked for:\n{ld8}");
    assert!(ld8.contains("shrq\t$3"), "{ld8}");
    assert!(ld8.contains("cmpb\t$0"), "{ld8}");
}

#[test]
fn the_settings_the_kernel_gives_are_read() {
    let target = "x86_64-unknown-linux-gnu";
    let outline = [
        "-O2",
        "-fsanitize=kernel-address",
        "-fasan-shadow-offset=0xdffffc0000000000",
        "--param",
        "asan-instrumentation-with-call-threshold=0",
        "--param=asan-stack=0",
        "--param=asan-globals=0",
    ];
    let (text, _) = listing(target, &outline, "ld4");
    assert!(text.contains("__asan_load4_noabort"), "{text}");
    assert!(text.contains("__asan_store16_noabort"), "{text}");
    assert!(!text.contains("__asan_report"), "{text}");
    assert!(!text.contains("__asan_memcpy"), "the routines are named only when asked:\n{text}");

    let (text, _) =
        listing(target, &["-O2", "-fsanitize=kernel-address", "-fno-sanitize-recover=all"], "ld4");
    assert!(text.contains("__asan_report_load4\n"), "{text}");
    assert!(!text.contains("_noabort"), "{text}");

    let (text, _) = listing(target, &["-O2"], "ld4");
    assert!(!text.contains("__asan"), "{text}");
    assert!(!text.contains("sanitizes_address"), "{text}");
}

#[test]
fn what_is_not_made_yet_is_refused() {
    let on = "-fsanitize=kernel-address";
    for param in ["asan-stack=1", "asan-globals=1", "asan-instrument-allocas=1"] {
        let said = refused(&[on, "--param", param, "-S", "-o", "-"]);
        assert!(said.contains("redzones"), "{param}: {said}");
    }
    let said = refused(&["-fasan-shadow-offset=0x1000", "-S", "-o", "-"]);
    assert!(said.contains("should only be used with -fsanitize=kernel-address"), "{said}");
    let said = refused(&["--target=riscv64-unknown-linux-gnu", on, "-S", "-o", "-"]);
    assert!(said.contains("-fasan-shadow-offset="), "{said}");
    let said = refused(&["--target=i686-unknown-linux-gnu", on, "-S", "-o", "-"]);
    assert!(said.contains("not a 64-bit one"), "{said}");
    let said = refused(&["-fsanitize=address", "-S", "-o", "-"]);
    assert!(said.contains("-fsafety=detect"), "{said}");
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
#[test]
fn a_poisoned_byte_is_reported_and_the_program_goes_on() {
    for level in ["-O0", "-O2"] {
        let dir =
            std::env::temp_dir().join(format!("rucc-kasan-run-{}{level}", std::process::id()));
        let program = dir.join("one");
        let program_text = program.to_str().expect("a temporary path is text");
        let source = format!("{SOURCE}{RUNS}");
        let args = [
            level,
            "-fsanitize=kernel-address",
            "-fasan-shadow-offset=0x100000000000",
            "--param",
            "asan-kernel-mem-intrinsic-prefix=1",
            "-o",
            program_text,
        ];
        let out = compile(&dir, &source, &args);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let status = Command::new(&program).status().expect("the program runs");
        assert_eq!(status.code(), Some(0), "{level}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
