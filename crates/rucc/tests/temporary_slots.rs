//! The slot behind a call that returns a structure, and the locals of a statement expression, are
//! shared with the ones in other statements.
//!
//! C gives the object a call returns automatic storage until the end of the full expression the
//! call is in, and a local of a statement expression goes when the expression has been read. The
//! front end said neither, so every such slot kept bytes of its own for the whole of the function
//! as soon as its address went anywhere. The AVX-512 intrinsics are calls and macros of exactly
//! that shape over a 64 byte vector, and Postgres' `pg_comp_crc32c_avx512` had a frame of 1920
//! bytes where gcc 14 has none. The front end now ends each of them with the statement it was made
//! in, and the fixture's three functions take what clang takes or close to it.

use std::process::Command;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/temporary_slots.c");

/// The size of the structure the fixture passes around, and so the least any of its frames is.
const ONE: u32 = 288;

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-temporary-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The `.su` one compile of the fixture wrote.
fn usage(target: &str, args: &[&str]) -> String {
    let dir = scratch(&format!("{target}{}", args.join("")));
    std::fs::copy(FIXTURE, dir.join("one.c")).expect("the fixture can be copied");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .arg(format!("--target={target}"))
        .args(args)
        .args(["-fstack-usage", "-S", "one.c", "-o", "one.s"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let usage = std::fs::read_to_string(dir.join("one.su")).expect("one.su was written");
    let _ = std::fs::remove_dir_all(&dir);
    usage
}

/// The bytes the `.su` says a function takes.
fn frame(usage: &str, function: &str) -> u32 {
    usage
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let place = fields.next()?;
            place.ends_with(&format!(":{function}")).then(|| fields.next()?.parse().ok())?
        })
        .unwrap_or_else(|| panic!("no line for {function} in\n{usage}"))
}

/// rucc 0.24.8 took 1152 bytes for each of the first two at `-O2`, which is four structures side
/// by side, and 1440 for the third. clang takes 288, 288 and 576. Now the first two are one
/// structure and the third is the two locals it declares and one slot for the three expressions.
#[test]
fn four_results_in_four_statements_are_one_structure_s_bytes() {
    for target in TARGETS {
        for args in [&["-O2"][..], &["-O1"]] {
            let usage = usage(target, args);
            for function in ["calls", "macros"] {
                let bytes = frame(&usage, function);
                assert!(
                    (ONE..=ONE + 64).contains(&bytes),
                    "{target} {args:?}: {bytes} bytes for {function}, wanted one structure's worth"
                );
            }
            let bytes = frame(&usage, "kept");
            assert!(bytes <= 3 * ONE + 64, "{target} {args:?}: {bytes} bytes for kept");
        }
    }
}

/// `-O0` keeps everything in a place of its own, and so does `-fstack-reuse=none` at any level.
#[test]
fn nothing_shares_where_stack_reuse_is_off() {
    for target in TARGETS {
        for args in [&["-O0"][..], &["-O2", "-fstack-reuse=none"]] {
            let usage = usage(target, args);
            for function in ["calls", "macros"] {
                let bytes = frame(&usage, function);
                assert!(bytes >= 4 * ONE, "{target} {args:?}: {bytes} bytes for {function}");
            }
        }
    }
}

/// The value of a statement expression that is its own local is still there when the expression
/// around it reads it, at every level.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn the_results_are_read_before_their_bytes_are_given_to_anything_else() {
    let dir = scratch("run");
    let prog = dir.join("prog");
    let prog = prog.to_str().expect("the temporary directory has a name that is text");
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([level, "-DRUN", "-o", prog, FIXTURE])
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let out = Command::new(prog).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(got, "38520 38520 85320\n", "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
