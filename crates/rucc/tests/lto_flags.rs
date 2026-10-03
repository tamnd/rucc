//! What the `-flto` family does to an object here, which is to keep the module beside the code.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.7, and `spec/09-optimizer.md` section 9.8.
//!
//! Link time optimization is the optimizer run once over the whole program rather than once per
//! file, and what it needs from each file is the module rather than the code. gcc's `-flto` object
//! holds only the module, which is why it is only useful to a link that knows about it. An object
//! here holds the code as it always has, which is what `-ffat-lto-objects` asks gcc for, and the
//! module in a section of its own beside it that the linker leaves out of what it writes. So a
//! build that passes `-flto` and then runs `ar`, `nm` or a linker that knows nothing of this over
//! the result gets what it would have got without the flag, and this file holds the compiler to
//! that on the bytes.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Two files, so that the thing being asked for is an optimization across a boundary rather than
/// one inside a function. `helper` is exactly what an inliner would reach for across a file.
const CALLER: &str = "\
extern int helper(int n);
int twice_over(int n) { return helper(n) + helper(n); }
";

/// The other side of it.
const CALLEE: &str = "int helper(int n) { return n * 3; }\n";

/// The target the object is built for, written down rather than taken from the host, because an
/// object is only produced for the one this compiler has a back end for.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// Every spelling of the family that is taken, which is what the assertions below are quantified
/// over. `-flto=thin` is not here because it is clang's and is refused, which is its own test.
const TAKEN: [&str; 12] = [
    "-flto",
    "-flto=auto",
    "-flto=jobserver",
    "-flto=1",
    "-flto=8",
    "-fno-lto",
    "-flto-partition=balanced",
    "-flto-partition=one",
    "-flto-partition=none",
    "-flto-compression-level=9",
    "-ffat-lto-objects",
    "-fuse-linker-plugin",
];

/// A directory of this test's own, with both files already in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-lto-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("caller.c"), CALLER).expect("the fixture can be written");
    std::fs::write(dir.join("callee.c"), CALLEE).expect("the fixture can be written");
    dir
}

/// What the compiler did with those flags on one of the two files: whether it succeeded, and what
/// it said.
fn run(dir: &Path, flags: &[&str], source: &str, object: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-O2", "-c"])
        .args(flags)
        .arg("-o")
        .arg(dir.join(object))
        .arg(dir.join(source))
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The spellings that ask for the work, which every one of the family does that names a number of
/// jobs. The rest are about how the work is done and ask for none of it on their own.
const ASKING: [&str; 5] = ["-flto", "-flto=auto", "-flto=jobserver", "-flto=1", "-flto=8"];

/// Where the section headers are and how many there are, in a 64 bit ELF header. The two fields
/// adding a section changes, since the new table goes on the end of the file.
const TABLE: [std::ops::Range<usize>; 2] = [0x28..0x30, 0x3c..0x3e];

#[test]
fn a_spelling_that_asks_keeps_the_module_beside_the_code_it_would_have_written() {
    let dir = fixture("same");
    let (ok, said) = run(&dir, &[], "callee.c", "plain.o");
    assert!(ok, "{said}");
    let plain = std::fs::read(dir.join("plain.o")).expect("the object was written");
    // It holds machine code, which is the whole difference from what gcc writes here. A slim
    // object is headers and bytecode with an empty `.text`, and this one is a function with a
    // multiply in it.
    assert!(plain.len() > 200, "the object holds a compiled function: {} bytes", plain.len());
    assert_eq!(rucc_driver::lto::kept(&plain), None, "nothing is kept without the flag");

    for spelling in TAKEN {
        let (ok, said) = run(&dir, &[spelling], "callee.c", "asked.o");
        assert!(ok, "{spelling}: {said}");
        assert!(said.is_empty(), "{spelling} was taken without comment: {said}");
        let mut asked = std::fs::read(dir.join("asked.o")).expect("the object was written");
        if !ASKING.contains(&spelling) {
            assert_eq!(asked, plain, "{spelling} changed the object");
            continue;
        }
        let kept = rucc_driver::lto::kept(&asked).unwrap_or_else(|| panic!("{spelling} kept it"));
        let kept = rucc_driver::lto::read(kept).unwrap_or_else(|why| panic!("{spelling}: {why}"));
        assert_eq!(kept.version, rucc_driver::VERSION);
        assert!(kept.module.contains("func @helper(i32) -> i32"), "{}", kept.module);
        // Everything that was there is still there, in the same place. Only the header's word on
        // where the section headers are and how many changed, since the section is added on the
        // end with a longer copy of them after it.
        assert!(asked.len() > plain.len(), "{spelling}");
        asked.truncate(plain.len());
        let mut plain = plain.clone();
        for range in TABLE {
            asked[range.clone()].fill(0);
            plain[range].fill(0);
        }
        assert_eq!(asked, plain, "{spelling} changed what the object already held");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn objects_that_keep_their_module_link_and_run_and_the_program_does_not_hold_it() {
    let dir = fixture("linked");
    std::fs::write(
        dir.join("main.c"),
        "extern int twice_over(int n);\nint main(void) { return twice_over(7) == 42 ? 0 : 1; }\n",
    )
    .expect("the fixture can be written");
    for (source, object) in
        [("main.c", "main.o"), ("caller.c", "caller.o"), ("callee.c", "callee.o")]
    {
        let (ok, said) = run(&dir, &["-flto"], source, object);
        assert!(ok, "{source}: {said}");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-O2", "-flto", "-o"])
        .arg(dir.join("program"))
        .args(["main.o", "caller.o", "callee.o"].map(|object| dir.join(object)))
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let status = Command::new(dir.join("program")).status().expect("the program runs");
    assert!(status.success(), "{status}");
    let program = std::fs::read(dir.join("program")).expect("the program was written");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !program.windows(b"rucc-lto".len()).any(|bytes| bytes == b"rucc-lto"),
        "the linker left the module out"
    );
}

#[test]
fn a_value_gcc_does_not_take_is_not_taken_here_either() {
    // The family is taken, and that is not the same as the values being waved through. Somebody
    // who wrote `-flto=thin` meant clang, where it names a real and different arrangement, and
    // the useful thing to do with that command line is say so rather than compile it serially and
    // let them find out from a profile.
    let dir = fixture("refused");
    let refuse = |flag: &str, wanted: &str| {
        let (ok, said) = run(&dir, &[flag], "callee.c", "never.o");
        assert!(!ok, "{flag} is refused");
        assert!(said.contains(wanted), "{flag}: {said}");
    };

    refuse("-flto=thin", "link time jobs");
    refuse("-flto=full", "link time jobs");
    // gcc refuses a zero rather than reading it as a request for none, which is worth copying:
    // a build that computed the number from `nproc` and got zero has a bug either way.
    refuse("-flto=0", "link time jobs");
    refuse("-flto-partition=big", "partitioning model");
    // Nineteen is the top of zstd's range and the top of the one gcc checks against.
    refuse("-flto-compression-level=20", "compression level");
    let _ = std::fs::remove_dir_all(&dir);
}
