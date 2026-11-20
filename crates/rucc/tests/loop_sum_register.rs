//! A loop that sums into one value keeps the sum in one register and has no copy on the way round.
//!
//! Issue 3187. `loop_sum_register.c` is the `frame_fn` of the rucc-corpus case
//! `frame-size.8.16384.4.leaf`. With most registers busy holding what the four structures keep, the
//! allocator put the sum that comes into the loop in `rax` and worked out each trip's sum in `rbx`,
//! because the edge into the loop and the edge back each tied the sum to one of the two and it
//! counted those as the same. So every trip ended in a block of its own that was `movq %rbx, %rax`
//! and a jump. Its `gated` is the `main` of the cases `call-motion.*.a-call-under-a-false-test`,
//! whose sum leaves the loop for `printf` in `rsi`. Weighing the edge out of the loop as often as
//! the block it leaves runs put the sum in `rsi` for part of each trip and copied it in and out of
//! the register it was in for the rest. The unit tests in `rucc-regalloc` cover the allocator. This
//! runs the compiler and reads the loops it wrote.

use std::process::Command;

/// The assembly the compiler writes for the fixture for x86-64 at that level.
fn assembly(level: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/loop_sum_register.c");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(["-S", "-o", "-", level])
        .arg(path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// Every block that is one move from a register to a register, with a jump after it or not, and
/// that is on the way round a loop: a jump further down comes back to it, or it jumps back to a
/// label above it. That is the copy a value a loop carries costs on every trip when it is not in
/// the register its next value is worked out in.
fn copies_on_the_way_round(asm: &str) -> Vec<String> {
    let lines: Vec<&str> = asm
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && (!line.starts_with('.') || line.ends_with(':')))
        .collect();
    let target = |line: &str| line.split_whitespace().nth(1).map(str::to_string);
    let mut found = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let Some(label) = line.strip_suffix(':') else { continue };
        let body: Vec<&str> =
            lines[at + 1..].iter().copied().take_while(|line| !line.ends_with(':')).collect();
        let mov = body.first().and_then(|first| first.strip_prefix("movq\t"));
        let registers = mov
            .is_some_and(|operands| operands.split(", ").all(|operand| operand.starts_with('%')));
        let alone = match body[..] {
            [_] => true,
            [_, jump] => jump.starts_with("jmp"),
            _ => false,
        };
        if !registers || !alone {
            continue;
        }
        let jumps_back = body
            .get(1)
            .and_then(|jump| target(jump))
            .is_some_and(|to| lines[..at].contains(&format!("{to}:").as_str()));
        let comes_back = lines[at + 1..]
            .iter()
            .any(|line| line.starts_with('j') && target(line).as_deref() == Some(label));
        if jumps_back || comes_back {
            found.push(format!("{label}: {}", body.join("; ")));
        }
    }
    found
}

#[test]
fn the_sum_over_the_array_stays_in_one_register_round_the_loop() {
    for level in ["-O2", "-O3"] {
        let asm = assembly(level);
        assert!(asm.contains("$16384"), "the fixture no longer walks the array:\n{asm}");
        let copies = copies_on_the_way_round(&asm);
        assert!(copies.is_empty(), "{copies:?} at {level} in:\n{asm}");
    }
}
