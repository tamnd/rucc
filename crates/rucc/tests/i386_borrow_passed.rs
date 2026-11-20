//! An i386 asm that names `esi`, `edi`, `eax` and `ecx` and has a fifth operand on the stack.
//! `esi` and `edi` are the two registers held back for reloads, so reading the fifth one in has to
//! borrow a register, and the other two were the ones the first two inputs sat in on their way to
//! `esi` and `edi`. Nothing was left and the compiler panicked. `strncat` in
//! arch/x86/lib/string_32.c is the kernel's asm of that shape, and a register an input only passes
//! through is now borrowed for it once that input has moved on.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned int size_t;
char *strncat(char *dest, const char *src, size_t count) {
  int d0, d1, d2, d3;
  asm volatile(\"repne\\n\\tscasb\\n\\tdecl %1\\n\\tmovl %8,%3\\n1:\\tdecl %3\\n\\tjs 2f\\n\\tlodsb\\n\\t\"
               \"stosb\\n\\ttestb %%al,%%al\\n\\tjne 1b\\n2:\\txorl %2,%2\\n\\tstosb\"
               : \"=&S\"(d0), \"=&D\"(d1), \"=&a\"(d2), \"=&c\"(d3)
               : \"0\"(src), \"1\"(dest), \"2\"(0), \"3\"(0xffffffffu), \"g\"(count)
               : \"memory\");
  return dest;
}
";

#[test]
fn a_fifth_operand_borrows_a_register_an_input_passes_through() {
    for level in ["-O0", "-O1", "-O2"] {
        let dir = std::env::temp_dir()
            .join(format!("rucc-i386-borrow-passed-{}{level}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
        let path = dir.join("one.c");
        std::fs::write(&path, SOURCE).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=i686-unknown-linux-gnu", "-fno-pic", "-S", "-o", "-", level])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let count = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("movl %")?.strip_suffix(",%ecx"))
            .expect("the count is moved into ecx inside the asm");
        for named in ["esi", "edi", "eax", "ecx"] {
            assert_ne!(count, named, "{level}: the count is in a register the asm names\n{text}");
        }
    }
}
