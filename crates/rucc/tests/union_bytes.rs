//! A number written through a union and read back a byte or a half at a time.
//!
//! Scalar replacement cuts a local into pieces at the edges of its loads and stores. A `u8` read of
//! an `unsigned long` leaves seven bytes, which no machine has a register for, and those used to keep
//! the whole local in memory. bcachefs needs the byte folded, because it checks with `BUILD_BUG_ON`
//! that its lock bit lands in the first byte, so the bytes left over are now pieces of one, two and
//! four. This runs the program, so that every byte comes out where the target puts it.

use std::process::Command;

/// Every way the kernel shape can be read, against the same bytes taken apart with shifts.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SOURCE: &str = "\
#include <stdio.h>
typedef unsigned long u64; typedef unsigned char u8; typedef unsigned short u16; typedef unsigned u32;
union a { u64 l; u8 b; };
union b { u64 l; u16 h; };
union c { u64 l; u8 by[8]; };
union d { unsigned __int128 q; u8 b; u16 h; };
__attribute__((noipa)) u64 val(u64 x) { return x; }
int main(void) {
    int bad = 0;
    for (int s = 0; s < 64; s++) {
        u64 v = val(0x0123456789abcdefUL * (s + 1) >> s);
        if (((union a){ .l = v }).b != (u8)v) bad++, printf(\"a %d\\n\", s);
        if (((union b){ .l = v }).h != (u16)v) bad++, printf(\"b %d\\n\", s);
        union c c = { .l = v };
        for (int i = 0; i < 8; i++) if (c.by[i] != (u8)(v >> 8 * i)) bad++, printf(\"c %d %d\\n\", s, i);
        unsigned __int128 q = ((unsigned __int128)v << 64) | ~v;
        union d d = { .q = q };
        if (d.b != (u8)~v || d.h != (u16)~v) bad++, printf(\"d %d\\n\", s);
        union { u64 l; u8 by[8]; } m = { .l = v };
        m.by[3] = 0x5a;
        u64 want = (v & ~(0xffUL << 24)) | (0x5aUL << 24);
        if (m.l != want) bad++, printf(\"m %d\\n\", s);
        union { u32 w; u8 b; } t = { .w = (u32)v }; t.b = 7;
        if (t.w != (((u32)v & ~0xffu) | 7)) bad++, printf(\"t %d\\n\", s);
    }
    printf(\"%d\\n\", bad);
    return bad != 0;
}
";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn every_byte_of_a_number_written_through_a_union_is_where_the_target_puts_it() {
    let dir = std::env::temp_dir().join(format!("rucc-union-bytes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("bytes.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let prog = dir.join("bytes");
    for level in ["-O0", "-O1", "-O2", "-Os", "-O3"] {
        let built = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([level, "-o", prog.to_str().expect("a temporary path is text")])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(built.status.success(), "{level}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&prog).output().expect("what was linked can be run");
        assert_eq!(ran.status.code(), Some(0), "{level}: {}", String::from_utf8_lossy(&ran.stdout));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
