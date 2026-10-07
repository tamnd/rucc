//! A call or a tail jump through a pointer a load has just read is one instruction that reads the
//! pointer out of memory on x86-64, with no load in front of it.
//!
//! tamnd/rucc#1994. Postgres calls through a table of methods everywhere: `pfree` ends in a jump
//! through the method its chunk header picks, and every node of a plan runs through the pointer it
//! carries. rucc loaded the pointer into a register and went through that, where gcc writes
//! `jmp *16(%rdi)` and `call *(%rax)`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-call-memory-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A tail call through a field, a call through a field of a field, a tail call through a table a
/// header picks the entry of, and a `main` that runs all three over a spread of values.
const PROGRAM: &str = r#"
int printf(const char *, ...);

struct node;
struct ops {
  int (*go)(struct node *, int);
  int (*done)(struct node *);
};
struct node {
  long id;
  const struct ops *ops;
  int (*exec)(struct node *);
};

typedef int (*method)(long);
method methods[4];

__attribute__((noinline)) int exec(struct node *n) { return n->exec(n); }

__attribute__((noinline)) int go(struct node *n, int x) { return n->ops->go(n, x) + 1; }

__attribute__((noinline)) int dispatch(const long *p) { return methods[p[-1] & 3](p[0]); }

static int twice(struct node *n, int x) { return (int) n->id * 2 + x; }
static int thrice(struct node *n, int x) { return (int) n->id * 3 + x; }
static int ran(struct node *n) { return n->ops->done(n) + (int) n->id; }
static int left(struct node *n) { return (int) n->id - 1; }
static int right(struct node *n) { return (int) n->id + 1; }
static int one(long v) { return (int) v; }
static int two(long v) { return (int) v * 2; }
static int neg(long v) { return (int) -v; }
static int sq(long v) { return (int) (v * v); }

static const struct ops first = {twice, left};
static const struct ops second = {thrice, right};

int main(void) {
  methods[0] = one;
  methods[1] = two;
  methods[2] = neg;
  methods[3] = sq;
  for (int i = 0; i < 16; i++) {
    struct node n = {i, (i & 1) ? &first : &second, ran};
    long cell[2] = {i * 7, i - 5};
    printf("%d %d %d\n", exec(&n), go(&n, i), dispatch(&cell[1]));
  }
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_call_through_memory_gives_the_same_answers() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let mut want = None;
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        let want = want.get_or_insert_with(|| got.clone());
        assert_eq!(&got, want, "{level} printed something -O0 did not");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lines of one function in what the compiler wrote, up to the label of the next, without
/// the directives.
fn body<'a>(asm: &'a str, name: &str, next: &str) -> Vec<&'a str> {
    let start = asm.find(&format!("\n{name}:")).expect("the function is there");
    let end = asm[start..].find(&format!("\n{next}:")).map_or(asm.len(), |at| start + at);
    asm[start..end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.ends_with(':'))
        .collect()
}

/// What the compiler wrote for the program at `-O2` with these flags besides.
fn assembly(what: &str, flags: &[&str]) -> String {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let mut args = vec!["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"];
    args.extend(flags);
    let (ok, said) = run(&dir, &args);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    let _ = std::fs::remove_dir_all(&dir);
    asm
}

/// At `-O2` each branch reads where it goes out of memory, and the load that read it for the
/// register is gone.
#[test]
fn a_call_through_a_pointer_just_loaded_reads_the_pointer_itself() {
    let asm = assembly("asm", &[]);
    let exec = body(&asm, "exec", "go");
    assert_eq!(exec, ["jmp\t*16(%rdi)"], "{exec:#?}");
    let go = body(&asm, "go", "dispatch");
    assert!(go.contains(&"call\t*(%rax)"), "{go:#?}");
    assert!(!go.iter().any(|line| line.ends_with("(%rax), %rax")), "{go:#?}");
    let dispatch = body(&asm, "dispatch", "main");
    assert_eq!(dispatch.last(), Some(&"jmp\t*(%rcx,%rax,8)"), "{dispatch:#?}");
}

/// A thunk is named after the register a branch goes through, so under `-mindirect-branch=` the
/// pointer is still loaded into one.
#[test]
fn a_branch_sent_through_a_thunk_still_goes_through_a_register() {
    let asm = assembly("thunk", &["-mindirect-branch=thunk-extern"]);
    let exec = body(&asm, "exec", "go");
    assert!(exec.contains(&"movq\t16(%rdi), %rax"), "{exec:#?}");
    assert!(!exec.iter().any(|line| line.contains('*')), "{exec:#?}");
}
