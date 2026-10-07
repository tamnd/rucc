//! Each select on AArch64 reads the flags of its own test.
//!
//! A one-bit test that a select reads is a `ubfx` of the bit until the selects are written out, and
//! the `ubfx` does not write the flags. When it became a `tst` in front of the `csel`, the `tst`
//! stayed where the `ubfx` was, which could be in front of another test's `csel`, and that `csel`
//! read the wrong flags. The kernel's ELF loader builds the protection of a segment that way, one
//! bit at a time, and mapped a writable segment without its write bit, so init died on its first
//! store.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
int adjust(int prot, int s);
static inline int make_prot(unsigned p_flags, int s)
{
    int prot = 0;
    if (p_flags & 4)
        prot |= 1;
    if (p_flags & 2)
        prot |= 2;
    if (p_flags & 1)
        prot |= 4;
    return adjust(prot, s);
}
struct phdr { unsigned type, flags; };
int load(struct phdr *ph, int n, int s)
{
    int r = 0;
    for (int i = 0; i < n; i++, ph++) {
        if (ph->type != 1)
            continue;
        r += make_prot(ph->flags, s);
    }
    return r;
}
int one(unsigned p, int s) { return make_prot(p, s); }
";

fn fixture(name: &str, source: &str) -> std::path::PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-sel-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join(format!("{name}.c"));
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

fn listing(source: &str) -> String {
    let path = fixture("one", source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-fno-pic", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

/// No `tst` is written while the flags of the one before it are still to be read.
#[test]
fn a_test_is_read_before_the_next_one() {
    let text = listing(SOURCE);
    for name in ["load", "one"] {
        let body = body(&text, name);
        let mut waiting = false;
        for line in &body {
            if line.starts_with("tst ") {
                assert!(!waiting, "{name} tests twice before reading:\n{}", body.join("\n"));
                waiting = true;
            } else if line.starts_with("csel ")
                || line.starts_with("cset ")
                || line.starts_with("b.")
            {
                waiting = false;
            }
        }
        assert_eq!(
            body.iter().filter(|line| line.starts_with("tst ")).count(),
            2,
            "{name}:\n{text}"
        );
    }
}

/// Every protection comes out right for every set of bits, checked by running it.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn every_set_of_bits_gives_its_protection() {
    let source = format!(
        "{SOURCE}\
__attribute__((noinline)) int adjust(int prot, int s) {{ return prot + s; }}
int main(void) {{
    struct phdr ph[8];
    int bad = 0;
    for (unsigned p = 0; p < 8; p++) {{
        int want = (p & 4 ? 1 : 0) | (p & 2 ? 2 : 0) | (p & 1 ? 4 : 0);
        if (one(p, 0) != want) bad++;
        ph[p].type = 1;
        ph[p].flags = p;
        if (load(&ph[p], 1, 0) != want) bad++;
    }}
    return bad;
}}
"
    );
    let path = fixture("run", &source);
    let prog = path.with_extension("");
    let built = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-O2", "-o"])
        .arg(&prog)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&prog).output().expect("what was linked can be run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert_eq!(ran.status.code(), Some(0));
}
