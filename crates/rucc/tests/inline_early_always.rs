//! A static function whose one line calls an `always_inline` body is that body by the time a
//! caller weighs it, as in gcc's early inliner, so it is not copied into every caller as a
//! function no larger than the call.
//!
//! zstd's `HUF_DGEN` wrappers in lib/zstd/decompress/huf_decompress.c are this shape. Each is one
//! line around a `FORCE_INLINE_TEMPLATE` decoder, and with the wrapper copied into each of its
//! callers the 32 bit kernel had the decoder four times, about 18KB a copy where gcc's callers are
//! 64 bytes.

use std::process::Command;

const SOURCE: &str = "\
static inline __attribute__((always_inline)) unsigned body(const unsigned char *p, unsigned n, const unsigned *t)
{
	unsigned h = 0, i;
	for (i = 0; i + 4 <= n; i += 4) {
		h = h * 31 + t[p[i]];
		h ^= t[p[i + 1]] << 3;
		h += t[p[i + 2]] >> 5;
		h = (h << 7) | (h >> 25);
		h -= t[p[i + 3]] * 17;
	}
	for (; i < n; i++)
		h = h * 33 + p[i] + t[p[i]];
	return h;
}
static unsigned fn(const unsigned char *p, unsigned n, const unsigned *t) { return body(p, n, t); }
unsigned a(const unsigned char *p, unsigned n, const unsigned *t) { return fn(p, n, t); }
unsigned b(const unsigned char *p, unsigned n, const unsigned *t) { return fn(p, n, t) + 1; }
unsigned c(const unsigned char *p, unsigned n, const unsigned *t, int k) { return k ? fn(p, n, t) : 0; }
";

/// The IR the compiler produced for the source at `-O2`.
fn ir(target: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-inline-early-always-{}-{target}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-fno-inline-functions-called-once", "--emit=ir", "-w", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_wrapper_around_an_always_inline_body_stays_a_call() {
    for target in ["i686-linux-gnu", "x86_64-linux-gnu"] {
        let text = ir(target);
        assert!(text.contains("func @fn("), "{target}: the wrapper went\n{text}");
        let calls =
            text.lines().filter(|line| line.contains("= call") && line.contains(" @fn(")).count();
        assert_eq!(calls, 3, "{target}\n{text}");
    }
}
