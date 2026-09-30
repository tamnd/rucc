//! What the Linux kernel memory model asks of the compiler, checked in the code it writes.
//!
//! The kernel does not use C11 atomics for most of its shared data. It uses `READ_ONCE`,
//! `WRITE_ONCE` and `barrier()`, which are a volatile access and an empty `asm` with a `memory`
//! clobber, and it relies on a few promises about them that the C standard does not spell out:
//! a volatile access is one access of its own width, two of them are not merged or reordered, a
//! value read once is not read again, a store the program made conditional stays conditional, and
//! nothing is read or written across `barrier()` from a copy made before it. `memory-barriers.txt`
//! in the kernel is the list. Each test below is one of those promises, checked in the assembly
//! rucc writes at `-O2` for x86-64. See tamnd/rucc-kernel#4.

use std::path::PathBuf;
use std::process::Command;

const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The three macros the way the kernel defines them, reduced to what matters here.
const PRELUDE: &str = "\
#define READ_ONCE(x) (*(const volatile __typeof__(x) *)&(x))
#define WRITE_ONCE(x, v) (*(volatile __typeof__(x) *)&(x) = (v))
#define barrier() __asm__ __volatile__(\"\" ::: \"memory\")
";

/// The assembly for that source at `-O2`, under a directory of its own.
fn assembly(what: &str, source: &str) -> String {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rucc-lkmm-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, format!("{PRELUDE}{source}")).expect("the fixture can be written");
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={TARGET}"), "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    String::from_utf8(done.stdout).expect("the listing is text")
}

/// The instructions of one function, without directives, labels kept.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.contains(".cfi_endproc") && !line.starts_with(".Lfunc_end"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') || line.ends_with(':'))
        .map(str::to_owned)
        .collect()
}

/// How many instructions of a function mention that operand.
fn touching(lines: &[String], operand: &str) -> usize {
    lines.iter().filter(|line| !line.ends_with(':') && line.contains(operand)).count()
}

#[test]
fn a_volatile_access_is_one_access_of_its_own_width() {
    let text = assembly(
        "width",
        "struct s { unsigned long a; unsigned int b; unsigned short c; } *p;
         unsigned long r8(struct s *p) { return READ_ONCE(p->a); }
         unsigned int r4(struct s *p) { return READ_ONCE(p->b); }
         unsigned short r2(struct s *p) { return READ_ONCE(p->c); }
         void w8(struct s *p) { WRITE_ONCE(p->a, 0x1234567812345678UL); }
         void w2(struct s *p, unsigned short v) { WRITE_ONCE(p->c, v); }
        ",
    );
    for (name, operand, width) in [
        ("r8", "(%rdi)", 'q'),
        ("r4", "8(%rdi)", 'l'),
        ("r2", "12(%rdi)", 'w'),
        ("w8", "(%rdi)", 'q'),
        ("w2", "12(%rdi)", 'w'),
    ] {
        let lines = body(&text, name);
        let at: Vec<&String> = lines.iter().filter(|line| line.contains(operand)).collect();
        assert_eq!(at.len(), 1, "{name} is one access: {lines:#?}");
        let mnemonic = at[0].split_whitespace().next().unwrap_or_default();
        assert!(mnemonic.ends_with(width), "{name} is {width} wide: {lines:#?}");
    }
}

#[test]
fn volatile_stores_are_neither_merged_nor_dropped() {
    let text = assembly(
        "merge",
        "struct p { unsigned char a, b, c, d; };
         void bytes(struct p *q) {
             WRITE_ONCE(q->a, 1); WRITE_ONCE(q->b, 2); WRITE_ONCE(q->c, 3); WRITE_ONCE(q->d, 4);
         }
         void twice(long *p) { WRITE_ONCE(*p, 1); WRITE_ONCE(*p, 2); }
        ",
    );
    let lines = body(&text, "bytes");
    for operand in ["(%rdi)", "1(%rdi)", "2(%rdi)", "3(%rdi)"] {
        let at: Vec<&String> =
            lines.iter().filter(|line| line.ends_with(&format!(", {operand}"))).collect();
        assert_eq!(at.len(), 1, "one store to {operand}: {lines:#?}");
        assert!(at[0].starts_with("movb"), "a byte store: {lines:#?}");
    }
    assert_eq!(touching(&body(&text, "twice"), "(%rdi)"), 2, "both stores are made");
}

#[test]
fn a_value_read_once_is_not_read_again() {
    let text = assembly(
        "once",
        "long once(long *a, long *y, long *z) {
             long v = READ_ONCE(*a);
             if (v) *y = v; else *z = v;
             return v;
         }
        ",
    );
    assert_eq!(touching(&body(&text, "once"), "(%rdi)"), 1, "{text}");
}

#[test]
fn barrier_stops_a_plain_access_from_being_merged_across_it() {
    let text = assembly(
        "barrier",
        "long reread(long *p) { long a = *p; barrier(); return a + *p; }
         void rewrite(long *p) { *p = 1; barrier(); *p = 2; }
        ",
    );
    assert_eq!(touching(&body(&text, "reread"), "(%rdi)"), 2, "read again after it: {text}");
    assert_eq!(touching(&body(&text, "rewrite"), "(%rdi)"), 2, "both stores stay: {text}");
}

#[test]
fn a_loop_waiting_on_memory_reads_it_every_time_round() {
    let text = assembly(
        "spin",
        "void spin(long *p) { while (!READ_ONCE(*p)) barrier(); }
         void plain(long *p) { while (!*p) barrier(); }
        ",
    );
    for name in ["spin", "plain"] {
        let lines = body(&text, name);
        // The loop is the stretch from a label to a jump back to it, and the load is inside it.
        let looped = lines.iter().enumerate().any(|(at, line)| {
            let Some(label) = line.strip_suffix(':') else { return false };
            lines[at..].iter().enumerate().any(|(to, jump)| {
                jump.starts_with('j')
                    && jump.ends_with(label)
                    && lines[at..at + to].iter().any(|inside| inside.contains("(%rdi)"))
            })
        });
        assert!(looped, "{name} reads inside the loop: {lines:#?}");
    }
}

#[test]
fn a_conditional_store_is_not_made_unconditional() {
    let text = assembly(
        "conditional",
        "void maybe(int c, long *p, long v) { if (c) *p = v; }
         void ctrl(long *a, long *b) { if (READ_ONCE(*a)) WRITE_ONCE(*b, 1); }
        ",
    );
    for (name, store) in [("maybe", "(%rsi)"), ("ctrl", "(%rsi)")] {
        let lines = body(&text, name);
        let at = lines.iter().position(|line| line.ends_with(store)).expect("the store is there");
        let branch =
            lines.iter().position(|line| line.starts_with('j') && !line.starts_with("jmp"));
        assert!(branch.is_some_and(|branch| branch < at), "{name} branches first: {lines:#?}");
    }
}

#[test]
fn a_bit_field_store_does_not_write_the_member_beside_it() {
    let text = assembly(
        "bitfield",
        "struct a { unsigned x:4; char c; unsigned y:4; };
         void sety(struct a *p, unsigned v) { p->y = v; }
         struct b { unsigned long x:40; unsigned char tail; };
         void setx(struct b *p, unsigned long v) { p->x = v; }
        ",
    );
    let lines = body(&text, "sety");
    assert_eq!(touching(&lines, "1(%rdi)"), 0, "c is left alone: {lines:#?}");
    let lines = body(&text, "setx");
    assert_eq!(touching(&lines, "5(%rdi)"), 0, "tail is left alone: {lines:#?}");
    assert!(
        !lines.iter().any(|line| line.starts_with("movq") && line.ends_with(", (%rdi)")),
        "no eight byte store over the tail: {lines:#?}"
    );
}

#[test]
fn ordered_atomics_are_the_instructions_x86_needs() {
    let text = assembly(
        "atomics",
        "long load(long *p) { return __atomic_load_n(p, __ATOMIC_ACQUIRE); }
         void release(long *p, long v) { __atomic_store_n(p, v, __ATOMIC_RELEASE); }
         void seq(long *p, long v) { __atomic_store_n(p, v, __ATOMIC_SEQ_CST); }
         int swap(int *p, int v) { return __atomic_exchange_n(p, v, __ATOMIC_SEQ_CST); }
         int cas(int *p, int o, int n) { return __sync_val_compare_and_swap(p, o, n); }
         int add(int *p) { return __atomic_add_fetch(p, 1, __ATOMIC_RELAXED); }
         void full(void) { __atomic_thread_fence(__ATOMIC_SEQ_CST); }
        ",
    );
    let has = |name: &str, what: &str| body(&text, name).iter().any(|line| line.contains(what));
    assert!(!has("load", "fence") && !has("release", "fence"), "no barrier: {text}");
    let seq = body(&text, "seq");
    let store = seq.iter().position(|line| line.ends_with("(%rdi)")).expect("a store");
    assert!(
        seq[store].starts_with("xchg") || seq[store..].iter().any(|line| line == "mfence"),
        "a sequentially consistent store is fenced: {seq:#?}"
    );
    assert!(has("swap", "xchg"), "{text}");
    assert!(has("cas", "lock") && has("cas", "cmpxchg"), "{text}");
    assert!(has("add", "lock") && has("add", "xadd"), "{text}");
    assert!(has("full", "mfence") || has("full", "lock"), "{text}");
}

#[test]
fn a_kept_asm_with_a_memory_clobber_keeps_the_stores_on_their_sides() {
    let text = assembly(
        "sides",
        "void sides(long *x, long *y) { *x = 1; __asm__ volatile(\"sfence\" ::: \"memory\"); *y = 1; }",
    );
    let lines = body(&text, "sides");
    let at = |what: &str| lines.iter().position(|line| line.contains(what)).expect(what);
    assert!(at("(%rdi)") < at("sfence") && at("sfence") < at("(%rsi)"), "{lines:#?}");
}
