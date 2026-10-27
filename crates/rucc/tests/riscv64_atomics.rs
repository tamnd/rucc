//! What each barrier and atomic becomes on RISC-V, instruction by instruction.
//!
//! The reference is the mapping in the RISC-V psABI, which is what gcc writes. A fence is
//! `fence r, rw` for an acquire, `fence rw, w` for a release and `fence rw, rw` for anything
//! stronger. An acquiring load is the load and then `fence r, rw`, and at the strongest ordering
//! it has `fence rw, rw` in front as well. A releasing store is `fence rw, w` and then the store,
//! and at the strongest ordering it has `fence rw, rw` after it as well. An exchange and an add
//! at a word or a doubleword are one `amo` instruction, and a compare and exchange there is a loop
//! of `lr.aqrl` and `sc.rl`.
//!
//! The base machine has no `amo` instructions and no `lr` and `sc` for a byte or a half, so a
//! compare and exchange of one is the same loop on the aligned word around it, and every other
//! read modify write of one is a loop around that. gcc writes the same loop.
//!
//! The comparison is over the instructions that order memory and the loads and stores of the
//! values the program handed over, which are everything that touches memory except the frame.
//! The moves and the arithmetic around them are the register allocator's business. `tas` is an
//! acquire, where gcc writes `amoswap.w.aq`, and the `aqrl` here is stronger and never wrong.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const TARGET: &str = "riscv64-linux-gnu";

const LEVELS: [&str; 2] = ["-O0", "-O2"];

/// One function per builtin, each over pointers it is handed so that the listing has no address
/// arithmetic for a global in it.
const SOURCE: &str = "\
typedef unsigned int u32;
typedef unsigned long u64;
void memory_barrier(void) { __atomic_thread_fence(__ATOMIC_SEQ_CST); }
void sync_synchronize(void) { __sync_synchronize(); }
void read_barrier(void) { __atomic_thread_fence(__ATOMIC_ACQUIRE); }
void write_barrier(void) { __atomic_thread_fence(__ATOMIC_RELEASE); }
int tas(volatile int *lock) { return __sync_lock_test_and_set(lock, 1); }
void s_unlock(volatile int *lock) { __sync_lock_release(lock); }
_Bool cas32(volatile u32 *p, u32 *expected, u32 v) {
  return __atomic_compare_exchange_n(p, expected, v, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
}
_Bool cas64(volatile u64 *p, u64 *expected, u64 v) {
  return __atomic_compare_exchange_n(p, expected, v, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
}
_Bool cas16(volatile unsigned short *p, unsigned short *expected, unsigned short v) {
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
u32 fetch_or32(volatile u32 *p, u32 v) { return __sync_fetch_and_or(p, v); }
u64 val_cas64(volatile u64 *p, u64 old, u64 v) { return __sync_val_compare_and_swap(p, old, v); }
u32 load32(volatile u32 *p) { return __atomic_load_n(p, __ATOMIC_SEQ_CST); }
u64 load64(volatile u64 *p) { return __atomic_load_n(p, __ATOMIC_SEQ_CST); }
void store32(volatile u32 *p, u32 v) { __atomic_store_n(p, v, __ATOMIC_SEQ_CST); }
void store64(volatile u64 *p, u64 v) { __atomic_store_n(p, v, __ATOMIC_SEQ_CST); }
u32 acquire32(volatile u32 *p) { return __atomic_load_n(p, __ATOMIC_ACQUIRE); }
void release32(volatile u32 *p, u32 v) { __atomic_store_n(p, v, __ATOMIC_RELEASE); }
u32 relaxed32(volatile u32 *p) { return __atomic_load_n(p, __ATOMIC_RELAXED); }
";

/// Each function, and the instructions in it that touch memory, in the order they are written.
const EXPECTED: &[(&str, &[&str])] = &[
    ("memory_barrier", &["fence rw, rw"]),
    ("sync_synchronize", &["fence rw, rw"]),
    ("read_barrier", &["fence r, rw"]),
    ("write_barrier", &["fence rw, w"]),
    ("tas", &["amoswap.w.aqrl"]),
    ("s_unlock", &["fence rw, w", "sw"]),
    // The value expected is read before the loop and written back after it when it was not the
    // one there.
    ("cas32", &["lw", "lr.w.aqrl", "sc.w.rl", "sw"]),
    ("cas64", &["ld", "lr.d.aqrl", "sc.d.rl", "sd"]),
    ("cas16", &["lhu", "lr.w.aqrl", "sc.w.rl", "sh"]),
    ("exchange32", &["amoswap.w.aqrl"]),
    ("exchange64", &["amoswap.d.aqrl"]),
    ("fetch_add32", &["amoadd.w.aqrl"]),
    ("fetch_add64", &["amoadd.d.aqrl"]),
    // A loop around a compare and exchange, which starts from a plain load of what is there.
    ("fetch_add16", &["lhu", "lr.w.aqrl", "sc.w.rl"]),
    ("fetch_add8", &["lbu", "lr.w.aqrl", "sc.w.rl"]),
    ("fetch_sub32", &["amoadd.w.aqrl"]),
    ("fetch_sub64", &["amoadd.d.aqrl"]),
    ("fetch_or32", &["lw", "lr.w.aqrl", "sc.w.rl"]),
    ("val_cas64", &["lr.d.aqrl", "sc.d.rl"]),
    ("load32", &["fence rw, rw", "lw", "fence r, rw"]),
    ("load64", &["fence rw, rw", "ld", "fence r, rw"]),
    ("store32", &["fence rw, w", "sw", "fence rw, rw"]),
    ("store64", &["fence rw, w", "sd", "fence rw, rw"]),
    ("acquire32", &["lw", "fence r, rw"]),
    ("release32", &["fence rw, w", "sw"]),
    ("relaxed32", &["lw"]),
];

fn fixture(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-rv64-atomics-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The whole listing at one level.
fn listing(level: &str) -> String {
    let path = fixture(level);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .arg(level)
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// The instructions of one function, trimmed, from its label to the next label that is not a
/// local one, with the directives and the local labels left out.
fn function(text: &str, name: &str) -> Vec<String> {
    let label = format!("{name}:");
    let mut lines = text.lines().map(str::trim).skip_while(|line| *line != label);
    assert!(lines.next().is_some(), "no {name} in the listing\n{text}");
    lines
        .take_while(|line| !line.ends_with(':') || line.starts_with('.'))
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// A fence with what it orders, an atomic by its mnemonic, and a plain load or store by its
/// mnemonic when it is not one of the frame's.
fn touching(line: &str) -> Option<String> {
    let (mnemonic, operands) = line.split_once([' ', '\t']).unwrap_or((line, ""));
    let operands = operands.trim();
    if mnemonic == "fence" {
        return Some(format!("fence {operands}"));
    }
    if ["lr.", "sc.", "amo"].iter().any(|prefix| mnemonic.starts_with(prefix)) {
        return Some(mnemonic.to_owned());
    }
    let plain = ["lb", "lbu", "lh", "lhu", "lw", "lwu", "ld", "sb", "sh", "sw", "sd"];
    let frame = operands.ends_with("(sp)") || operands.ends_with("(s0)");
    (plain.contains(&mnemonic) && !frame).then(|| mnemonic.to_owned())
}

fn mnemonics(body: &[String]) -> Vec<&str> {
    body.iter().map(|line| line.split([' ', '\t']).next().unwrap_or("")).collect()
}

#[test]
fn each_barrier_and_atomic_orders_memory_as_the_psabi_maps_it() {
    for level in LEVELS {
        let text = listing(level);
        for (name, want) in EXPECTED {
            let body = function(&text, name);
            let got: Vec<String> = body.iter().filter_map(|line| touching(line)).collect();
            assert_eq!(got, *want, "{name} at {level}\n{}", body.join("\n"));
        }
    }
}

/// The loops are gcc's: a reserved load, a branch out when the value is not the one expected, a
/// conditional store and a branch back when the store lost the reservation. A byte or a half
/// picks its bits out of the word and puts the rest of the word back as it was found.
#[test]
fn the_loops_are_whole() {
    let windows: &[(&str, &[&str])] = &[
        ("cas32", &["lr.w.aqrl", "bne", "sc.w.rl", "bnez", "xor", "seqz"]),
        ("val_cas64", &["lr.d.aqrl", "bne", "sc.d.rl", "bnez"]),
        ("cas16", &["lr.w.aqrl", "and", "bne", "xor", "or", "sc.w.rl", "bnez"]),
        ("fetch_add8", &["lr.w.aqrl", "and", "bne", "xor", "or", "sc.w.rl", "bnez"]),
    ];
    for level in LEVELS {
        let text = listing(level);
        for (name, window) in windows {
            let body = function(&text, name);
            let seen = mnemonics(&body);
            assert!(
                seen.windows(window.len()).any(|w| w == *window),
                "{name} at {level} has no {window:?}\n{}",
                body.join("\n")
            );
        }
    }
}

/// The branches inside a loop land where they should: out of it past the store, and back to the
/// reserved load. Every instruction is four bytes, since nothing here is compressed.
#[test]
fn the_branches_of_a_loop_land_on_its_ends() {
    let text = listing("-O2");
    for (name, out, back) in [("cas32", ".+12", ".-12"), ("cas16", ".+20", ".-24")] {
        let body = function(&text, name);
        let branches: Vec<&str> = body
            .iter()
            .filter(|line| line.contains(".+") || line.contains(".-"))
            .map(|line| line.rsplit(", ").next().unwrap_or(""))
            .collect();
        assert_eq!(branches, [out, back], "{name}\n{}", body.join("\n"));
    }
}
