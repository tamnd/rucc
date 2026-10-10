//! On i386 outside position independent code, a store of the address of a name carries the name as
//! its immediate, `movl $g, (%eax)`, the way gcc writes it. It used to be a `leal g, %ecx` and a
//! store of `%ecx`. An address that something other than a store reads keeps its `leal`.

use std::process::Command;

const SOURCE: &str = "\
extern int g, *p;
extern void h(void);
struct s { int *p; void (*f)(void); };
extern struct s one;
void set(struct s *s) { s->p = &g; s->f = h; }
void glob(void) { p = &g; one.f = h; }
extern void use(int *, void (*)(void));
void call(void) { use(&g, h); }
int *keep(struct s *s) { s->p = &g; return &g; }
";

fn listing(level: &str, pic: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-store-name-{}-{level}{pic}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, pic])
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn a_stored_address_is_the_immediate_of_the_store() {
    for level in ["-O2", "-Os"] {
        let text = listing(level, "-fno-pic");
        for line in [
            "\tmovl\t$g, (%eax)\n",
            "\tmovl\t$h, 4(%eax)\n",
            "\tmovl\t$g, p\n",
            "\tmovl\t$h, one+4\n",
            "\tmovl\t$g, (%esp)\n",
            "\tmovl\t$h, 4(%esp)\n",
        ] {
            assert!(text.contains(line), "{level} {line:?}:\n{text}");
        }
        assert_eq!(text.matches("\tleal\tg, ").count(), 1, "{level}:\n{text}");
    }
}

#[test]
fn position_independent_code_keeps_the_address_in_a_register() {
    let text = listing("-O2", "-fpic");
    assert!(!text.contains("\tmovl\t$g"), "{text}");
    assert!(!text.contains("\tmovl\t$h"), "{text}");
}
