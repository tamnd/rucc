//! A static array indexed on i386 is read as `off(,%ecx,4)`, the name and the index in the one
//! operand, where the code is not position independent. i386 has no instruction pointer relative
//! addressing, so the address of a name is a number and an index fits beside it, but the fold that
//! puts them together only did that under the x86-64 kernel code model. Every `__per_cpu_offset[cpu]`
//! in the kernel was a `leal __per_cpu_offset` and a load through it. Position independent code
//! still reaches the name through the global offset table.

use std::process::Command;

const SOURCE: &str = "\
extern int off[8];
int pick(int *p, unsigned c) { return p[off[c & 7]]; }
";

fn listing(args: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rucc-i386-name-index-{}-{}",
        std::process::id(),
        args.join("")
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-mregparm=3"])
        .args(args)
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn a_name_takes_the_index_beside_it() {
    for level in ["-O2", "-Os"] {
        let text = listing(&[level, "-fno-pic"]);
        assert!(text.contains("\tmovl\toff(,%"), "{level}:\n{text}");
        assert!(!text.contains("leal\toff"), "{level}:\n{text}");
    }
}

#[test]
fn position_independent_code_reaches_the_name_through_the_table() {
    let text = listing(&["-O2", "-fpic"]);
    assert!(text.contains("off@GOT(%ebx)"), "{text}");
    assert!(!text.contains("off(,%"), "{text}");
}
