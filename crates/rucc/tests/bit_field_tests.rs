//! `a || b` and `a && b` over bit-fields of one object are one load and one compare, the way gcc's
//! `fold_truth_andor` writes them. The kernel's `tlb_flush_mmu_tlbonly` asks whether any of six one
//! bit fields of its `mmu_gather` is set, and gcc asks that with one `testw` where rucc loaded the
//! same byte six times with a shift and a branch each, which left the body too large to inline.

use std::path::PathBuf;
use std::process::Command;

const GATHER: &str = "\
struct gather {
\tvoid *mm;
\tunsigned long start, end;
\tunsigned int fullmm : 1, need_flush_all : 1, freed_tables : 1, delayed_rmap : 1;
\tunsigned int cleared_ptes : 1, cleared_pmds : 1, cleared_puds : 1, cleared_p4ds : 1;
\tunsigned int vma_exec : 1, vma_huge : 1, vma_pfn : 1, unshared_tables : 1;
};
void flush(void *);
void only(struct gather *tlb)
{
\tif (!(tlb->freed_tables || tlb->cleared_ptes || tlb->cleared_pmds ||
\t      tlb->cleared_puds || tlb->cleared_p4ds || tlb->unshared_tables))
\t\treturn;
\tflush(tlb->mm);
}
int kept(volatile struct gather *tlb) { return tlb->fullmm || tlb->need_flush_all; }
";

/// Every shape the merge takes, checked against the same question asked a field at a time
/// of a `volatile` copy, which is read a field at a time.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CHECKED: &str = "\
struct s { unsigned a : 1, b : 1, c : 3, d : 1; unsigned e : 7; int f : 1; };
int any(struct s *p) { return p->a || p->c || p->e; }
int none(struct s *p) { return !p->a && !p->c && p->e == 0; }
int mixed(struct s *p) { return p->a && !p->b && p->d; }
int either(struct s *p) { return !p->a || p->b || !p->f; }
int wide(struct s *p) { return p->a && p->c; }
int between(struct s *p, int x) { return p->a || x || p->b || p->d; }
int main(void)
{
\tint bad = 0;
\tfor (unsigned i = 0; i < 1u << 16; i++) {
\t\tunion { struct s s; unsigned w; } u = { .w = i * 2654435761u };
\t\tstruct s *p = &u.s;
\t\tvolatile struct s c = *p;
\t\tbad |= any(p) != (c.a != 0 || c.c != 0 || c.e != 0);
\t\tbad |= none(p) != (c.a == 0 && c.c == 0 && c.e == 0);
\t\tbad |= mixed(p) != (c.a != 0 && c.b == 0 && c.d != 0);
\t\tbad |= either(p) != (c.a == 0 || c.b != 0 || c.f == 0);
\t\tbad |= wide(p) != (c.a ? c.c != 0 : 0);
\t\tbad |= between(p, i & 1) != (c.a || (i & 1) || c.b || c.d);
\t}
\treturn bad;
}
";

fn dir(name: &str) -> PathBuf {
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("rucc-bit-field-tests-{name}-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The lines of one function, from its label to the next function's.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let label = format!("{name}:");
    let lines = text.lines().skip_while(|line| *line != label).skip(1);
    let lines = lines.take_while(|line| !line.starts_with("\t.size"));
    lines.map(str::trim).filter(|line| !line.starts_with('.')).collect()
}

#[test]
fn six_one_bit_fields_are_asked_with_one_test() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), GATHER).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-", "a.c"])
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("assembly is text");
    assert_eq!(body(&text, "only")[0], "testw\t$2292, 24(%rdi)", "{text}");
    // A `volatile` object is read as the program wrote it, a field at a time.
    let loads = body(&text, "kept").iter().filter(|line| line.contains("24(%rdi)")).count();
    assert_eq!(loads, 2, "{text}");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_merged_test_gives_the_answer_the_fields_one_at_a_time_give() {
    for level in ["-O0", "-O2"] {
        let dir = dir(&level[1..]);
        std::fs::write(dir.join("a.c"), CHECKED).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([level, "a.c", "-o", "prog"])
            .current_dir(&dir)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let ran = Command::new(dir.join("prog")).status().expect("what was linked can be run");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ran.success(), "{level}: a merged test got the wrong answer");
    }
}
