//! A `switch` on i386, which a bit test and a jump table both compute in the machine's word. The
//! masks and the index used to be 64 bits wide everywhere, which is a pair of registers on i386
//! and a shift by a register that the halves are not split into. The kernel builds with
//! `-fno-jump-tables`, but the EFI stub does not, and `memparse` and `vsnprintf` there were
//! refused with a `zext.i32.i64` nothing lowered. Position independent i386 code writes no table,
//! since the table would be found with an absolute address and the stub runs where the firmware
//! put it.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned long long u64;
u64 mp(const char *s) {
  u64 ret = *s - '0';
  switch (*++s) {
  case 'E': case 'e': ret <<= 10;
  case 'P': case 'p': ret <<= 10;
  case 'T': case 't': ret <<= 10;
  case 'G': case 'g': ret <<= 10;
  case 'M': case 'm': ret <<= 10;
  case 'K': case 'k': ret <<= 10; s++;
  default: break;
  }
  return ret + *s;
}
int dense(int c) {
  switch (c) { case 0: return 11; case 1: return 22; case 2: return 33; case 3: return 47;
  case 4: return 51; case 5: return 63; case 6: return 77; case 8: return 89; case 9: return 91;
  case 10: return 104; default: return -1; }
}
int wide(long long c) {
  switch (c) { case 3: case 7: case 9: case 20: return 1; case 4: case 30: case 31: return 2;
  case 5: case 6: case 40: return 3; default: return 0; }
}
";

fn listing(flags: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rucc-i386-switch-word-{}{}",
        std::process::id(),
        flags.join("")
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{flags:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn a_switch_is_lowered_in_the_word_with_a_table_only_where_it_can_be_found() {
    for level in ["-O0", "-O2", "-Os"] {
        let text = listing(&[level, "-fPIC"]);
        assert!(!text.contains("jmp\t*"), "{level} -fPIC wrote a table:\n{text}");
        listing(&[level, "-fno-pic"]);
    }
    let text = listing(&["-O2", "-fno-pic"]);
    assert!(text.contains("jmp\t*"), "-O2 -fno-pic wrote no table:\n{text}");
}
