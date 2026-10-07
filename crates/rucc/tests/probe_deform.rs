//! Temporary: prints what rucc and gcc write for the tuple deforming loop.

use std::process::Command;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn probe() {
    let dir = std::env::temp_dir().join(format!("rucc-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/probe_deform.c");
    let flags = ["-O2", "-fwrapv", "-fno-strict-aliasing", "-S", src, "-o"];
    let rucc = dir.join("rucc.s");
    let gcc = dir.join("gcc.s");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc")).args(flags).arg(&rucc).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let _ = Command::new("gcc").args(flags).arg(&gcc).output();
    let rucc = std::fs::read_to_string(rucc).unwrap();
    let gcc = std::fs::read_to_string(gcc).unwrap_or_default();
    panic!("PROBE-RUCC-BEGIN\n{rucc}\nPROBE-RUCC-END\nPROBE-GCC-BEGIN\n{gcc}\nPROBE-GCC-END");
}
