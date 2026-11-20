//! `%z` on an x86 `asm` operand in memory. net/rxrpc's `shiftr_adv_rotr` writes `shr%z1 %1` over
//! a `u8` it hands over as `"+m"`, and rucc refused the statement because the back end only has the
//! address of a memory operand and not its type. The suffix is now the one for the object's width,
//! `shrb` for the byte, as gcc writes it.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned char u8;
unsigned long f(u8 *acks, int n)
{
  unsigned long extracted = ~0UL;
  for (int i = 0; i < n; i++)
    asm(\" shr%z1 %1\\n inc %0\\n rcr%z2 %2\\n\"
        : \"+d\"(acks), \"+m\"(*acks), \"+rm\"(extracted));
  return extracted;
}
unsigned short g(unsigned short *p)
{
  asm(\"add%z0 $1, %0\" : \"+m\"(*p));
  return *p;
}
";

fn assembly(target: &str, level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-asm-z-memory-{}-{target}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{target} {level}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

#[test]
fn a_memory_operand_takes_the_suffix_of_its_width() {
    for (target, long) in [("i686-unknown-linux-gnu", "rcrl"), ("x86_64-unknown-linux-gnu", "rcrq")]
    {
        for level in ["-O0", "-O2"] {
            let text = assembly(target, level);
            for wanted in ["shrb", long, "addw"] {
                assert!(text.contains(wanted), "{target} {level} has no {wanted}:\n{text}");
            }
            assert!(!text.contains("%z"), "{target} {level}:\n{text}");
        }
    }
}
