//! `-m16`, which is how the kernel builds its boot and real mode code: 32 bit x86 code assembled
//! as `.code16gcc`, so that each instruction does in real mode what it would have done in
//! protected mode.

use std::process::Command;

/// The listing says `.code16gcc` first, as gcc's does, and the object has the prefixes in it.
#[test]
fn a_sixteen_bit_unit_is_the_thirty_two_bit_one_with_its_prefixes_turned_round() {
    let dir = std::env::temp_dir().join(format!("rucc-sixteen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, "int f(int a, int b) { return a * b + 3; }\n")
        .expect("the fixture can be written");
    let run = |emit: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([
                "--target=x86_64-unknown-linux-gnu",
                "-m16",
                "-O2",
                "-fno-asynchronous-unwind-tables",
            ])
            .arg(format!("--emit={emit}"))
            .args(["-o", "-"])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    };
    let listing = String::from_utf8(run("asm")).expect("a listing is text");
    let object = run("obj");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(listing.starts_with("\t.code16gcc\n"), "{listing}");
    // `movl 4(%esp), %eax` with the address and the operand size both turned round, and the
    // return taking four bytes off the stack as the caller's `calll` put there.
    let has = |bytes: &[u8]| object.windows(bytes.len()).any(|w| w == bytes);
    assert!(has(&[0x67, 0x66, 0x8b, 0x44, 0x24, 0x04]), "{object:02x?}");
    assert!(has(&[0x66, 0xc3]), "{object:02x?}");
}
