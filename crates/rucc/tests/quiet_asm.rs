//! Inline assembly not written `volatile` whose outputs nothing reads, which goes, as gcc drops it.
//! The kernel's `RELOC_HIDE` is one, run for a sanitizer hook that is empty in a build without the
//! sanitizer, and it was a copy of an address in front of every bit operation.
//!
//! The ones that stay are each something a reader of the outputs cannot see: `volatile`, a
//! `memory` clobber, an operand in memory, and an output somebody reads.

use std::process::Command;

const SOURCE: &str = "\
void hidden(long *p) { long q; __asm__ (\"nop\" : \"=r\" (q) : \"0\" (p)); }
void kept(long *p) { long q; __asm__ volatile (\"nop\" : \"=r\" (q) : \"0\" (p)); }
void clobbers(long *p) { long q; __asm__ (\"nop\" : \"=r\" (q) : \"0\" (p) : \"memory\"); }
void stored(long *p) { __asm__ (\"nop\" : \"=m\" (*p)); }
long read(long *p) { long q; __asm__ (\"nop\" : \"=r\" (q) : \"0\" (p)); return q; }
void bare(void) { __asm__ (\"nop\"); }
";

fn listing(target: &str, level: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-quiet-asm-{}-{target}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), level, "-fno-asynchronous-unwind-tables"])
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

fn body(listing: &str, name: &str) -> String {
    let start = listing.find(&format!("\n{name}:\n")).expect(name);
    let rest = &listing[start + name.len() + 3..];
    let end = rest.find("\n\t.size").unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[test]
fn assembly_only_there_for_an_output_nobody_reads_is_dropped() {
    for target in ["x86_64-unknown-linux-gnu", "i686-unknown-linux-gnu"] {
        for level in ["-O1", "-O2", "-Os"] {
            let listing = listing(target, level);
            for (name, nops) in [
                ("hidden", 0),
                ("kept", 1),
                ("clobbers", 1),
                ("stored", 1),
                ("read", 1),
                ("bare", 1),
            ] {
                let body = body(&listing, name);
                assert_eq!(body.matches("nop").count(), nops, "{target} {level} {name}:\n{body}");
            }
        }
    }
}
