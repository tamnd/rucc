//! What each barrier and atomic PostgreSQL is built on becomes on AArch64, instruction by
//! instruction, at every `-march` a build of it might pass.
//!
//! PostgreSQL built with gcc reaches every one of these through `generic-gcc.h` and `s_lock.h`:
//! `pg_memory_barrier()` is a sequentially consistent fence, the read and write barriers are an
//! acquire and a release fence, the spinlock is `__sync_lock_test_and_set` and
//! `__sync_lock_release`, the compare and exchange and the exchange are the C11 builtins at
//! `__ATOMIC_SEQ_CST`, and fetch and add and its neighbours are the `__sync` ones. What each has to
//! be is set by what gcc documents it as, and the reference is what gcc writes with its outline
//! atomics turned off, which is the same exclusive loop this compiler writes and so the one to
//! compare against.
//!
//! The one place the two families differ is what comes after the loop. A `__sync` read modify
//! write is a full barrier, so a plain load after it may not be answered before its store is seen,
//! and gcc puts a `dmb ish` after the loop to say so. A C11 one at `__ATOMIC_SEQ_CST` promises that
//! only to other sequentially consistent accesses, whose loads are `ldar` and wait for the `stlxr`
//! by themselves, so there is no `dmb` after it. Store buffering in `tests/litmus/sb.c` fails on
//! real hardware without the `dmb`.
//!
//! The comparison is over the instructions that order memory, which are the exclusive and the
//! ordered loads and stores and the barriers, each with the width of the register it moves. The
//! moves and the arithmetic around them are the register allocator's business and differ between
//! levels. The loops themselves are checked whole as well.
//!
//! This compiler has no LSE instructions and no outline atomics yet, so every level gets the loop.
//! gcc with `-march=armv8.1-a` writes `ldaddal`, `casal` and `swpal` instead, which are full
//! barriers by themselves and need nothing after them.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const TARGET: &str = "aarch64-unknown-linux-gnu";

/// What a build of PostgreSQL might pass, and nothing, which is the compiler's own default.
const MARCHES: [Option<&str>; 3] = [None, Some("armv8-a"), Some("armv8.1-a")];

const LEVELS: [&str; 2] = ["-O0", "-O2"];

/// One function per builtin, each over pointers it is handed so that the listing has no address
/// arithmetic for a global in it.
const SOURCE: &str = "\
typedef unsigned int u32;
typedef unsigned long u64;
void memory_barrier(void) { __atomic_thread_fence(__ATOMIC_SEQ_CST); }
void sync_synchronize(void) { __sync_synchronize(); }
void read_barrier(void) { __asm__ __volatile__(\"\" ::: \"memory\"); __atomic_thread_fence(__ATOMIC_ACQUIRE); }
void write_barrier(void) { __asm__ __volatile__(\"\" ::: \"memory\"); __atomic_thread_fence(__ATOMIC_RELEASE); }
int tas(volatile int *lock) { return __sync_lock_test_and_set(lock, 1); }
void s_unlock(volatile int *lock) { __sync_lock_release(lock); }
_Bool cas32(volatile u32 *p, u32 *expected, u32 v) {
  return __atomic_compare_exchange_n(p, expected, v, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
}
_Bool cas64(volatile u64 *p, u64 *expected, u64 v) {
  return __atomic_compare_exchange_n(p, expected, v, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
}
u32 exchange32(volatile u32 *p, u32 v) { return __atomic_exchange_n(p, v, __ATOMIC_SEQ_CST); }
u64 exchange64(volatile u64 *p, u64 v) { return __atomic_exchange_n(p, v, __ATOMIC_SEQ_CST); }
u32 fetch_add32(volatile u32 *p, int v) { return __sync_fetch_and_add(p, v); }
u64 fetch_add64(volatile u64 *p, long v) { return __sync_fetch_and_add(p, v); }
unsigned short fetch_add16(volatile unsigned short *p) { return __sync_fetch_and_add(p, 1); }
unsigned char fetch_add8(volatile unsigned char *p) { return __sync_fetch_and_add(p, 1); }
u32 fetch_sub32(volatile u32 *p, int v) { return __sync_fetch_and_sub(p, v); }
u64 fetch_sub64(volatile u64 *p, long v) { return __sync_fetch_and_sub(p, v); }
u32 add_fetch32(volatile u32 *p, int v) { return __sync_add_and_fetch(p, v); }
u32 fetch_and32(volatile u32 *p, u32 v) { return __sync_fetch_and_and(p, v); }
u64 fetch_and64(volatile u64 *p, u64 v) { return __sync_fetch_and_and(p, v); }
u32 fetch_or32(volatile u32 *p, u32 v) { return __sync_fetch_and_or(p, v); }
u64 fetch_or64(volatile u64 *p, u64 v) { return __sync_fetch_and_or(p, v); }
u32 val_cas32(volatile u32 *p, u32 old, u32 v) { return __sync_val_compare_and_swap(p, old, v); }
u64 val_cas64(volatile u64 *p, u64 old, u64 v) { return __sync_val_compare_and_swap(p, old, v); }
_Bool bool_cas32(volatile u32 *p, u32 old, u32 v) { return __sync_bool_compare_and_swap(p, old, v); }
_Bool bool_cas64(volatile u64 *p, u64 old, u64 v) { return __sync_bool_compare_and_swap(p, old, v); }
u32 atomic_fetch_add32(volatile u32 *p, u32 v) { return __atomic_fetch_add(p, v, __ATOMIC_SEQ_CST); }
u64 atomic_fetch_add64(volatile u64 *p, u64 v) { return __atomic_fetch_add(p, v, __ATOMIC_SEQ_CST); }
u32 load32(volatile u32 *p) { return __atomic_load_n(p, __ATOMIC_SEQ_CST); }
u64 load64(volatile u64 *p) { return __atomic_load_n(p, __ATOMIC_SEQ_CST); }
void store32(volatile u32 *p, u32 v) { __atomic_store_n(p, v, __ATOMIC_SEQ_CST); }
void store64(volatile u64 *p, u64 v) { __atomic_store_n(p, v, __ATOMIC_SEQ_CST); }
u32 acquire32(volatile u32 *p) { return __atomic_load_n(p, __ATOMIC_ACQUIRE); }
void release32(volatile u32 *p, u32 v) { __atomic_store_n(p, v, __ATOMIC_RELEASE); }
int buffered(volatile int *mine, volatile int *theirs) { __sync_fetch_and_add(mine, 1); return *theirs; }
";

/// Each function, and the instructions in it that order memory, in the order they are written.
const EXPECTED: &[(&str, &[&str])] = &[
    ("memory_barrier", &["dmb ish"]),
    ("sync_synchronize", &["dmb ish"]),
    ("read_barrier", &["dmb ishld"]),
    ("write_barrier", &["dmb ish"]),
    // Only an acquire, as gcc documents. The loop acquires and releases at every ordering.
    ("tas", &["ldaxr w", "stlxr w"]),
    ("s_unlock", &["stlr w"]),
    ("cas32", &["ldaxr w", "stlxr w"]),
    ("cas64", &["ldaxr x", "stlxr x"]),
    ("exchange32", &["ldaxr w", "stlxr w"]),
    ("exchange64", &["ldaxr x", "stlxr x"]),
    ("fetch_add32", &["ldaxr w", "stlxr w", "dmb ish"]),
    ("fetch_add64", &["ldaxr x", "stlxr x", "dmb ish"]),
    ("fetch_add16", &["ldaxrh w", "stlxrh w", "dmb ish"]),
    ("fetch_add8", &["ldaxrb w", "stlxrb w", "dmb ish"]),
    ("fetch_sub32", &["ldaxr w", "stlxr w", "dmb ish"]),
    ("fetch_sub64", &["ldaxr x", "stlxr x", "dmb ish"]),
    ("add_fetch32", &["ldaxr w", "stlxr w", "dmb ish"]),
    // The two with no instruction are a compare and exchange loop, and the barrier comes once,
    // after the loop has succeeded, as gcc's does.
    ("fetch_and32", &["ldaxr w", "stlxr w", "dmb ish"]),
    ("fetch_and64", &["ldaxr x", "stlxr x", "dmb ish"]),
    ("fetch_or32", &["ldaxr w", "stlxr w", "dmb ish"]),
    ("fetch_or64", &["ldaxr x", "stlxr x", "dmb ish"]),
    ("val_cas32", &["ldaxr w", "stlxr w", "dmb ish"]),
    ("val_cas64", &["ldaxr x", "stlxr x", "dmb ish"]),
    ("bool_cas32", &["ldaxr w", "stlxr w", "dmb ish"]),
    ("bool_cas64", &["ldaxr x", "stlxr x", "dmb ish"]),
    ("atomic_fetch_add32", &["ldaxr w", "stlxr w"]),
    ("atomic_fetch_add64", &["ldaxr x", "stlxr x"]),
    ("load32", &["ldar w"]),
    ("load64", &["ldar x"]),
    ("store32", &["stlr w"]),
    ("store64", &["stlr x"]),
    ("acquire32", &["ldar w"]),
    ("release32", &["stlr w"]),
    ("buffered", &["ldaxr w", "stlxr w", "dmb ish"]),
];

fn fixture(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-a64-atomics-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The whole listing for one `-march` and one level.
fn listing(march: Option<&str>, level: &str) -> String {
    let path = fixture(&format!("{}{level}", march.unwrap_or("default")));
    let mut command = Command::new(env!("CARGO_BIN_EXE_rucc"));
    command.arg(format!("--target={TARGET}")).arg(level);
    if let Some(march) = march {
        command.arg(format!("-march={march}"));
    }
    let out = command
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// The instructions of one function, trimmed, from its label to the next label that is not a
/// local one, with the directives, the comments and the local labels left out.
fn function(text: &str, name: &str) -> Vec<String> {
    let label = format!("{name}:");
    let mut lines = text.lines().map(str::trim).skip_while(|line| *line != label);
    assert!(lines.next().is_some(), "no {name} in the listing\n{text}");
    lines
        .take_while(|line| !line.ends_with(':') || line.starts_with('.'))
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.starts_with("//"))
        .map(str::to_owned)
        .collect()
}

/// The mnemonic, and the operand that says the width of what is moved, which for an exclusive
/// store is the second, after the register that says whether it worked.
fn ordering(line: &str) -> Option<String> {
    let (mnemonic, operands) = line.split_once([' ', '\t']).unwrap_or((line, ""));
    let operands: Vec<&str> = operands.split(',').map(str::trim).collect();
    if mnemonic == "dmb" {
        return Some(format!("dmb {}", operands[0]));
    }
    let exclusive_store = mnemonic.starts_with("stlx") || mnemonic.starts_with("stx");
    let ordered = exclusive_store
        || ["ldax", "ldx", "ldar", "stlr", "cas", "swp", "ldadd", "ldset", "ldclr", "ldeor"]
            .iter()
            .any(|prefix| mnemonic.starts_with(prefix));
    if !ordered {
        return None;
    }
    let data = operands[usize::from(exclusive_store)];
    Some(format!("{mnemonic} {}", &data[..1]))
}

fn orderings(body: &[String]) -> Vec<String> {
    body.iter().filter_map(|line| ordering(line)).collect()
}

fn mnemonics(body: &[String]) -> Vec<&str> {
    body.iter().map(|line| line.split([' ', '\t']).next().unwrap_or("")).collect()
}

/// Every builtin's ordering instructions at every `-march` and every level.
#[test]
fn each_barrier_and_atomic_orders_memory_as_gcc_does() {
    for march in MARCHES {
        for level in LEVELS {
            let text = listing(march, level);
            for (name, want) in EXPECTED {
                let body = function(&text, name);
                assert_eq!(
                    orderings(&body),
                    *want,
                    "{name} at {march:?} {level}\n{}",
                    body.join("\n")
                );
            }
        }
    }
}

/// The loops are gcc's, less the acquire it leaves off the load of a `__sync` one: an exclusive
/// load, the operation, an exclusive store and a branch back when the store failed, and for a
/// compare and exchange a branch out when the value is not the one expected. The barrier after a
/// `__sync` one is outside the loop, where it costs once.
#[test]
fn the_loops_are_whole() {
    let windows: &[(&str, &[&str])] = &[
        ("fetch_add32", &["ldaxr", "add", "stlxr", "cbnz"]),
        ("fetch_sub64", &["ldaxr", "sub", "stlxr", "cbnz"]),
        ("exchange32", &["ldaxr", "stlxr", "cbnz"]),
        ("tas", &["ldaxr", "stlxr", "cbnz"]),
        ("cas32", &["ldaxr", "cmp", "b.ne", "stlxr", "cbnz", "cset"]),
        ("val_cas64", &["ldaxr", "cmp", "b.ne", "stlxr", "cbnz", "cset"]),
        ("fetch_or32", &["ldaxr", "cmp", "b.ne", "stlxr", "cbnz", "cset"]),
    ];
    for march in MARCHES {
        for level in LEVELS {
            let text = listing(march, level);
            for (name, window) in windows {
                let body = function(&text, name);
                let seen = mnemonics(&body);
                assert!(
                    seen.windows(window.len()).any(|w| w == *window),
                    "{name} at {march:?} {level} has no {window:?}\n{}",
                    body.join("\n")
                );
            }
        }
    }
}

/// Store buffering over `__sync_fetch_and_add`, which is `tests/litmus/sb.c`'s shape: the plain
/// load of the other thread's flag is after the barrier, where the optimizer may not move it back
/// over.
#[test]
fn a_plain_load_after_a_sync_read_modify_write_stays_after_the_barrier() {
    for march in MARCHES {
        let text = listing(march, "-O2");
        let body = function(&text, "buffered");
        let barrier = body.iter().position(|line| line == "dmb ish");
        let load = body.iter().rposition(|line| line.starts_with("ldr "));
        assert!(
            matches!((barrier, load), (Some(b), Some(l)) if b < l),
            "{march:?}\n{}",
            body.join("\n")
        );
    }
}
