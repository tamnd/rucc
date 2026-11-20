//! A loop over a `const` table of one entry, summed into a `BUILD_BUG_ON`. net/core/skbuff.c sums
//! `skb_ext_type_len` that way, and on a 32 bit defconfig only `CONFIG_XFRM` is on, so the table
//! has one entry. The test of that loop is `i + 1 == 0` by the time the unroller sees it, which it
//! could not count, so the loop stayed, the sum did not fold and the error call was left in.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned char u8;
static const u8 len[] = { 6 };
static inline __attribute__((__always_inline__)) unsigned total(void)
{
  unsigned l = 1;
  for (int i = 0; i < sizeof(len) / sizeof(len[0]); i++)
    l += len[i];
  return l;
}
extern void __attribute__((__noreturn__)) bug(void) __attribute__((__error__(\"too long\")));
extern int cache;
void init(void)
{
  if (total() > 255)
    bug();
  cache = total();
}
";

#[test]
fn the_sum_folds_and_the_error_call_goes() {
    for target in
        ["i686-unknown-linux-gnu", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]
    {
        let dir = std::env::temp_dir()
            .join(format!("rucc-one-entry-table-{}-{target}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
        let path = dir.join("one.c");
        std::fs::write(&path, SOURCE).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([&format!("--target={target}"), "-O2", "-S", "-o", "-"])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{target}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).expect("assembly is text");
        assert!(text.contains("$7") || text.contains("#7"), "{target}: the sum is not 7\n{text}");
        assert!(!text.contains("len"), "{target}: the table is still read\n{text}");
    }
}
