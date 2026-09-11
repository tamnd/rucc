//! The redefinitions a real program produces on purpose, and which have to stay quiet.
//!
//! Design: `spec/05-preprocessor.md` section 5.7.
//!
//! A macro may be defined twice as long as both definitions are the same, and a warning fires when
//! they differ. That rule is easy to get right in the small and easy to get wrong across a whole
//! toolchain, because two of the definitions in any hosted translation unit are not written by the
//! program at all. One is the built in set, which glibc's `<stdc-predef.h>` then writes again from
//! its own side before the first line of the file. The other is `<stddef.h>`, which a program is
//! allowed to reach after having defined `offsetof` itself.
//!
//! Both of those were warnings here, and neither is noise. sqlite's configure runs its feature
//! tests through autosetup's `cctest -nooutput 1`, which counts any output at all as a failed
//! test, so a warning on a line nobody wrote turned into a feature quietly switched off in the
//! generated Makefile. What this file holds the compiler to is that the two agreements hold, and
//! that a definition which really does differ is still reported.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The lines glibc's `<stdc-predef.h>` writes, in the form it writes them, which is a second
/// definition of five names the compiler has already defined by the time the file is read.
/// The tabs are glibc's, and they are in here rather than tidied away because the rule is that
/// the tokens are the same and not that the text is.
const PREDEF: &str = "\
#define __STDC_IEC_559__\t\t1
#define __STDC_IEC_60559_BFP__ \t201404L
#define __STDC_IEC_559_COMPLEX__\t1
#define __STDC_IEC_60559_COMPLEX__\t201404L
#define __STDC_ISO_10646__\t\t201706L
int main(void) { return 0; }
";

/// A program that defines `offsetof` before anything hands it one, which is what sqlite's
/// `shell.c` does, and then reaches the compiler's own header through an ordinary include.
const OWN_OFFSETOF: &str = "\
#ifndef offsetof
# define offsetof(ST,M) ((unsigned long)((char*)&((ST*)0)->M - (char*)0))
#endif
#include <stddef.h>
struct pair { int first; int second; };
int main(void) { return offsetof(struct pair, second) == 4 ? 0 : 1; }
";

/// A second definition that really does say something else, which is the case the warning is for.
const DISAGREES: &str = "\
#define __STDC_IEC_60559_BFP__ 999L
int main(void) { return 0; }
";

/// The target the object is built for, written down rather than taken from the host, because an
/// object is only produced for the one this compiler has a back end for.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// A directory of this test's own, with the source already in it.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-redef-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    dir
}

/// What the compiler did with that source: whether it succeeded, and what it said.
fn run(dir: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-Wall", "-O2", "-c"])
        .arg("-o")
        .arg(dir.join("a.o"))
        .arg(dir.join("a.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn the_definitions_glibc_writes_over_the_built_in_ones_agree_with_them() {
    let dir = fixture("predef", PREDEF);
    let (ok, said) = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{said}");
    assert!(said.is_empty(), "a hosted translation unit warned before its first line: {said}");
}

#[test]
fn a_program_that_defines_offsetof_first_still_gets_the_compilers_one_without_a_warning() {
    let dir = fixture("offsetof", OWN_OFFSETOF);
    let (ok, said) = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{said}");
    assert!(said.is_empty(), "including <stddef.h> warned about a name the program owns: {said}");
}

#[test]
fn a_second_definition_that_disagrees_is_still_reported() {
    // Which is what makes the two tests above a statement about agreement rather than about a
    // warning that was turned off.
    let dir = fixture("disagrees", DISAGREES);
    let (ok, said) = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "a redefinition is a warning and not an error: {said}");
    assert!(said.contains("__STDC_IEC_60559_BFP__"), "the warning names it: {said}");
    assert!(said.contains("redefined"), "{said}");
}
