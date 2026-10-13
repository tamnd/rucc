//! An asm that spells `%%rsp` and asks for a register it does not read afterwards. rucc gave that
//! register the stack pointer itself, since the text names it and the list does not, and the asm
//! then wrote the stack segment into `rsp`. The kernel's `sync_core` is that asm. It runs from
//! `text_poke_early` while the kernel boots, and the kernel that rucc 0.30.3 built took a triple
//! fault there.

use std::process::Command;

const SYNC_CORE: &str = "\
register unsigned long current_stack_pointer asm(\"rsp\");
void sync_core(void) {
  unsigned int tmp;
  asm volatile(\"mov %%ss, %0\\n\\t\"
               \"pushq %q0\\n\\t\"
               \"pushq %%rsp\\n\\t\"
               \"addq $8, (%%rsp)\\n\\t\"
               \"pushfq\\n\\t\"
               \"mov %%cs, %0\\n\\t\"
               \"pushq %q0\\n\\t\"
               \"pushq $1f\\n\\t\"
               \"iretq\\n\\t\"
               \"1:\"
               : \"=&r\"(tmp), \"+r\"(current_stack_pointer)
               :
               : \"cc\", \"memory\");
}
";

fn compile(level: &str, extra: &[&str]) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-asm-spelled-sp-{}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SYNC_CORE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fno-pic", "-S", "-o", "-", level])
        .args(extra)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn an_output_never_gets_the_stack_or_frame_pointer() {
    for level in ["-O0", "-O1", "-O2"] {
        for extra in [&[][..], &["-fno-omit-frame-pointer"][..]] {
            let text = compile(level, extra);
            assert!(text.contains("iretq"), "{level} {extra:?}: the asm is in the output\n{text}");
            for bad in ["%ss, %esp", "%ss, %ebp", "%cs, %esp", "%cs, %ebp"] {
                assert!(!text.contains(bad), "{level} {extra:?}: {bad}\n{text}");
            }
        }
    }
}
