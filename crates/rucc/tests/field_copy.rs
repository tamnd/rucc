//! A small structure copied from a field of one object into a field of another is written as moves
//! that each carry the field's offset, with no address worked out in front of them, on x86-64.
//!
//! tamnd/rucc#1994. `ExecStoreBufferHeapTuple` copies the tuple's item pointer into the slot with
//! `slot->tts_tid = tuple->t_self`, six bytes four past one pointer into six bytes forty eight past
//! another. gcc writes four moves for it and rucc wrote two `lea` and then the four moves.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-field-copy-{}-{what}", std::process::id()));
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

/// The item pointer and the two structures it is copied between, laid out as Postgres lays them
/// out, and a `main` that copies a few and prints what landed where.
const PROGRAM: &str = r#"
int printf(const char *, ...);

typedef struct { unsigned short hi, lo; } Block;
typedef struct { Block block; unsigned short posid; } __attribute__((packed, aligned(2))) Pointer;
typedef struct { unsigned len; Pointer self; unsigned table; void *data; } Tuple;
typedef struct { int type; unsigned short flags; short nvalid; void *ops, *desc, *values, *isnull,
                 *mcxt; Pointer tid; unsigned table; } Slot;

__attribute__((noinline)) void store(Slot *slot, const Tuple *tuple) {
  slot->tid = tuple->self;
  slot->table = tuple->table;
}

__attribute__((noinline)) void scatter(Tuple *into, int n, const Tuple *from) {
  for (int i = 0; i < n; i++)
    into[i].self = from[n - 1 - i].self;
}

int main(void) {
  Tuple t[5];
  Slot s = {0};
  for (int i = 0; i < 5; i++) {
    t[i].len = 0x5a5a5a5a;
    t[i].self.block.hi = (unsigned short) (i * 7919);
    t[i].self.block.lo = (unsigned short) (i * 104729 + 3);
    t[i].self.posid = (unsigned short) (i + 1);
    t[i].table = 16384 + i;
  }
  for (int i = 0; i < 5; i++) {
    store(&s, &t[i]);
    printf("%u %u %u %u %d %d\n", s.tid.block.hi, s.tid.block.lo, s.tid.posid, s.table, s.type,
           s.nvalid);
  }
  Tuple u[5];
  for (int i = 0; i < 5; i++)
    u[i].len = 0xa5a5a5a5u, u[i].table = 7;
  scatter(u, 5, t);
  for (int i = 0; i < 5; i++)
    printf("%x %u %u %u %u\n", u[i].len, u[i].self.block.hi, u[i].self.block.lo, u[i].self.posid,
           u[i].table);
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_field_copied_with_its_offset_in_every_move_gives_the_same_answers() {
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

/// At `-O2` the copy in `store` is moves off the two pointers it was given and nothing else.
#[test]
fn the_copy_of_an_item_pointer_works_out_no_address() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    let start = asm.find("\nstore:").expect("the function is there");
    let end = asm[start..].find("\nscatter:").map_or(asm.len(), |at| start + at);
    let body = &asm[start..end];
    let lines: Vec<&str> = body.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    assert!(!lines.iter().any(|line| line.starts_with("lea")), "{body}");
    assert!(lines.iter().any(|line| line.contains("\t4(%rsi)")), "{body}");
    assert!(lines.iter().any(|line| line.ends_with(", 48(%rdi)")), "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}
