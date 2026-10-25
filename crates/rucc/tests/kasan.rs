//! `-fsanitize=kernel-address`, which the kernel's `CONFIG_KASAN_GENERIC` puts on every unit that
//! does not opt out. Each access reads its shadow byte and calls the runtime gcc names when the
//! byte says the access is bad, a function marked `no_sanitize_address` makes no checks, and the
//! `--param` settings the kernel gives are read.

use std::path::Path;
use std::process::Command;

/// One access of each size the inline check has, an access that is not aligned, a copy whose
/// length the program works out, and a function that asks for no checks.
const SOURCE: &str = r#"typedef unsigned long u64;
__attribute__((noinline)) int ld4(int *p) { return *p; }
__attribute__((noinline)) void st4(int *p, int v) { *p = v; }
__attribute__((noinline)) char ld1(char *p) { return *p; }
__attribute__((noinline)) long ld8(long *p) { return *p; }
__attribute__((noinline)) void st16(__int128 *p) { *p = 0; }
struct __attribute__((packed)) pk { char c; int i; };
__attribute__((noinline)) int ldu(struct pk *p) { return p->i; }
struct two { void *a, *b; };
__attribute__((noinline)) void cp16(struct two *d, struct two *s) { *d = *s; }
__attribute__((noinline)) void cp(void *d, const void *s, u64 n) { __builtin_memcpy(d, s, n); }
__attribute__((noinline)) int over(int i) { int a[8] = {0}; a[i] = 1; return a[i ^ 1]; }
__attribute__((no_sanitize_address)) int quiet(int *p) { return *p; }
int table[10];
__attribute__((noinline)) int xadd(int *p) { return __atomic_fetch_add(p, 1, 5); }
__attribute__((noinline)) int at(int i) { return table[i]; }
__attribute__((noinline)) char vla(int n, int i) {
    char a[n];
    for (int k = 0; k < n; k++)
        a[k] = k;
    return a[i];
}
#ifdef __SANITIZE_ADDRESS__
int sanitizes_address;
#endif
"#;

/// The runtime, which counts the reports and keeps the last one, and a `main` that maps a buffer
/// and its shadow at fixed addresses, poisons parts of the shadow and checks what was reported.
///
/// The count is `volatile` because the checks go in after the optimizer, which takes a call to
/// `over` as a call that writes nothing. In the kernel the runtime is in another unit.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
const RUNS: &str = r#"
void *mmap(void *, u64, int, int, int, long);
#define BUF ((char *)0x200000000000UL)
#define SHADOW ((signed char *)((0x200000000000UL >> 3) + 0x100000000000UL))
#define QUIET __attribute__((no_sanitize_address))
static volatile int reports, kind;
static void *volatile last;
QUIET static void report(void *p, int k) { reports++; last = p; kind = k; }
QUIET void __asan_report_load1_noabort(void *p) { report(p, 1); }
QUIET void __asan_report_store1_noabort(void *p) { report(p, 11); }
QUIET void __asan_report_load4_noabort(void *p) { report(p, 4); }
QUIET void __asan_report_store4_noabort(void *p) { report(p, 14); }
QUIET void __asan_report_load8_noabort(void *p) { report(p, 8); }
QUIET void __asan_report_load16_noabort(void *p) { report(p, 16); }
QUIET void __asan_report_store16_noabort(void *p) { report(p, 116); }
QUIET void __asan_store4_noabort(void *p) { report(p, 24); }
QUIET void __asan_report_load_n_noabort(void *p, u64 n) { report(p, 1000 + (int)n); }
QUIET void *__asan_memcpy(void *d, const void *s, u64 n) {
    char *to = d;
    const char *from = s;
    report(d, 99);
    while (n--)
        *to++ = *from++;
    return d;
}
QUIET static int frames(void) {
    char probe;
    u64 at = (((u64)&probe >> 3) + 0x100000000000UL) & ~4095UL;
    signed char *shadow = (signed char *)(at - (1 << 16));
    if (mmap(shadow, 1 << 17, 3, 0x100032, -1, 0) != shadow)
        return 60;
    if (over(2) != 0 || reports != 7)
        return 61;
    over(9);
    if (reports != 9 || kind != 4)
        return 62;
    for (int n = 0; n < 1 << 17; n++)
        if (shadow[n] != 0)
            return 63;
    return 0;
}
QUIET static int saw(int count, void *p, int k) { return reports == count && last == p && kind == k; }
// The kernel's `struct kasan_global`, and what `__asan_register_globals` does with it: the shadow
// of the redzone after each global says its bytes are not to be used.
struct global { u64 beg, size, size_with_redzone; const char *name, *module; u64 dynamic, at, odr; };
static volatile int registered;
QUIET void __asan_register_globals(struct global *g, u64 n) {
    for (; n--; g++) {
        signed char *s = (signed char *)((g->beg >> 3) + 0x100000000000UL);
        u64 end = (u64)s + g->size_with_redzone / 8;
        for (u64 page = (u64)s & ~4095UL; page < end; page += 4096)
            mmap((void *)page, 4096, 3, 0x100022, -1, 0);
        for (u64 b = g->size / 8; b < g->size_with_redzone / 8; b++)
            s[b] = -7;
        if (g->size % 8)
            s[g->size / 8] = g->size % 8;
        registered++;
    }
}
QUIET void __asan_unregister_globals(struct global *g, u64 n) {}
QUIET static int globals(void) {
    if (registered == 0)
        return 70;
    if (at(9) != 0 || reports != 9)
        return 71;
    if (at(10) != 0 || !saw(10, &table[10], 4))
        return 72;
    // An atomic is a call whatever the shadow says, and the runtime does the check.
    if (xadd((int *)(BUF + 64)) != 9 || !saw(11, BUF + 64, 24) || *(int *)(BUF + 64) != 10)
        return 73;
    return 0;
}
// What the kernel's `__asan_alloca_poison` and `__asan_allocas_unpoison` do with the shadow.
#define SHADOW_OF(p) (*(signed char *)(((u64)(p) >> 3) + 0x100000000000UL))
static volatile int poisoned, unpoisoned;
QUIET void __asan_alloca_poison(char *a, u64 n) {
    u64 end = ((n + 31) & ~31UL) + 32;
    poisoned += (u64)a % 32 ? 100 : 1;
    for (int b = 1; b <= 4; b++)
        SHADOW_OF(a - 8 * b) = -54;
    for (u64 b = 0; b < end; b += 8)
        SHADOW_OF(a + b) = b + 8 <= n ? 0 : b < n ? (signed char)(n - b) : -53;
}
QUIET void __asan_allocas_unpoison(char *top, char *bottom) {
    unpoisoned++;
    for (; top < bottom; top += 8)
        SHADOW_OF(top) = 0;
}
QUIET static int allocas(void) {
    char probe;
    if (vla(20, 19) != 19 || reports != 11 || poisoned != 1 || unpoisoned == 0)
        return 80;
    vla(20, 20);
    if (reports != 12 || kind != 1)
        return 82;
    vla(20, -1);
    if (reports != 13 || kind != 1 || poisoned != 3)
        return 83;
    for (char *p = &probe - 4096; p < &probe; p += 8)
        if (SHADOW_OF(p) != 0)
            return 84;
    return 0;
}
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
    if (quiet((int *)(BUF + 12)) != 9 || reports != 6)
        return 50;
    // Bytes 6 to 9 end in the group whose first four bytes are good, and bytes 10 to 13 run past
    // them. The shadow of the first byte is good for both.
    if (ldu((struct pk *)(BUF + 5)) != 0 || reports != 6)
        return 51;
    if (ldu((struct pk *)(BUF + 9)) != 9 << 16 || !saw(7, BUF + 10, 1004))
        return 52;
    int bad = frames();
    bad = bad ? bad : globals();
    return bad ? bad : allocas();
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
    let tag: String = args.concat().chars().filter(char::is_ascii_alphanumeric).collect();
    let dir = std::env::temp_dir()
        .join(format!("rucc-kasan-{}-{target}-{function}-{tag}", std::process::id()));
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
                "__asan_report_load_n_noabort",
                "__asan_memcpy",
            ] {
                assert!(text.contains(wanted), "{target} {level} has no {wanted}:\n{text}");
            }
            assert!(!text.contains("__asan_load"), "{target} {level}:\n{text}");
            assert!(!quiet.contains("__asan"), "{target} {level}:\n{quiet}");
            assert!(text.contains("sanitizes_address"), "{target} {level}: the macro is defined");
            assert!(text.contains("__asan_gen_frame.0"), "{target} {level}: a frame record");
            let magic = text.contains("1102416563") || text.contains("#16821, lsl #16");
            assert!(magic, "{target} {level}: the frame magic 0x41b58ab3");
        }
    }
    let mut args = vec!["-O2"];
    args.extend(KERNEL);
    let (_, ld8) = listing("x86_64-unknown-linux-gnu", &args, "ld8");
    assert!(ld8.contains("$-2305847407260205056"), "the offset is the one asked for:\n{ld8}");
    assert!(ld8.contains("shrq\t$3"), "{ld8}");
    assert!(ld8.contains("cmpb\t$0"), "{ld8}");
    let (_, ldu) = listing("x86_64-unknown-linux-gnu", &args, "ldu");
    assert_eq!(ldu.matches("shrq\t$3").count(), 2, "the first and the last byte:\n{ldu}");
    assert!(ldu.contains("movl\t$4, %esi"), "the size goes to the report:\n{ldu}");
    let (_, cp16) = listing("x86_64-unknown-linux-gnu", &args, "cp16");
    assert!(cp16.contains("__asan_report_load16_noabort"), "an aligned copy:\n{cp16}");
    assert!(cp16.contains("__asan_report_store16_noabort"), "an aligned copy:\n{cp16}");
    let (_, xadd) = listing("x86_64-unknown-linux-gnu", &args, "xadd");
    assert!(xadd.contains("call\t__asan_store4_noabort"), "an atomic is a call:\n{xadd}");
    assert!(!xadd.contains("shrq"), "and has no inline check:\n{xadd}");
}

#[test]
fn a_global_gets_a_redzone_and_a_line_in_the_table() {
    let mut args = vec!["-O2"];
    args.extend(KERNEL);
    let (text, _) = listing("x86_64-unknown-linux-gnu", &args, "at");
    let start = text.find("\ntable:").expect("the array is defined");
    let table = &text[start..];
    assert!(text.contains("\t.p2align\t5\n\t.type\ttable, @object\n"), "aligned to 32:\n{text}");
    assert!(
        table.contains("\t.size\ttable, .-table\n\t.space\t56\n"),
        "a redzone after the size:\n{text}"
    );
    for word in
        ["\t.quad\ttable\n\t.quad\t40\n\t.quad\t96\n", "\"table\\000\"", "__asan_gen_module"]
    {
        assert!(text.contains(word), "{word}:\n{text}");
    }
    assert!(text.contains(".init_array"), "{text}");
    assert!(text.contains("\tjmp\t__asan_register_globals"), "{text}");
    assert!(text.contains("\tjmp\t__asan_unregister_globals"), "{text}");
}

#[test]
fn the_memory_of_a_variable_length_array_has_redzones_around_it() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let mut args = vec!["-O2"];
        args.extend(KERNEL);
        args.extend(["--param", "asan-instrument-allocas=1"]);
        let (_, vla) = listing(target, &args, "vla");
        assert_eq!(vla.matches("__asan_alloca_poison").count(), 1, "{target}:\n{vla}");
        assert!(vla.contains("__asan_allocas_unpoison"), "{target}:\n{vla}");
        args.extend(["--param", "asan-instrument-allocas=0"]);
        let (_, vla) = listing(target, &args, "vla");
        assert!(!vla.contains("__asan_alloca"), "{target}:\n{vla}");
    }
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
    assert!(text.contains("__asan_loadN_noabort"), "an access that is not aligned:\n{text}");
    assert!(!text.contains("__asan_report"), "{text}");
    assert!(!text.contains("__asan_memcpy"), "the routines are named only when asked:\n{text}");
    assert!(!text.contains("__asan_gen_frame"), "asan-stack=0 makes no frame record:\n{text}");
    assert!(!text.contains("__asan_register_globals"), "asan-globals=0 makes no table:\n{text}");

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
