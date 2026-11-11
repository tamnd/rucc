//! A signed overflow under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4148, and 06.8 of the rucc-plan9 plan.
//!
//! kencc does not optimize on the rule that a signed overflow does not happen. libpcm of 9front
//! defines `MININT` as `MAXINT+1` and clips a sample with it. When rucc took the overflow as
//! undefined, the clip folded to `MAXINT`, and `audio/pcmconv` wrote `MAXINT` for each sample.

use std::path::PathBuf;
use std::process::Command;

/// The clip of `conv.c` in libpcm.
const SOURCE: &str = "typedef long long vlong;\n\
                      #define MAXINT ((int)(~0UL>>1))\n\
                      #define MININT (MAXINT+1)\n\
                      static int clip(vlong v)\n\
                      {\n\
                      \tif(v > MAXINT)\n\
                      \t\treturn MAXINT;\n\
                      \tif(v < MININT)\n\
                      \t\treturn MININT;\n\
                      \treturn v;\n\
                      }\n\
                      int f(vlong v) { return clip(v); }\n";

fn fixture(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-kencc-overflow-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The assembly that the compiler writes for the source.
fn assembly(what: &str, flags: &[&str]) -> String {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-plan9", "-O2", "-S", "-w", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn kencc_keeps_both_sides_of_the_clip() {
    let asm = assembly("kencc", &["-fkencc"]);
    assert!(asm.contains("-2147483648"), "the clip lost MININT:\n{asm}");
    assert!(asm.contains("cmpq"), "the clip lost its compare:\n{asm}");
}
