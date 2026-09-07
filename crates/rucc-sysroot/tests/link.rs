//! The musl link line.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.2 and `spec/cross-compile/09-libc-stubs.md`
//! section 9.3.

use std::path::{Path, PathBuf};

use rucc_sysroot::{LinkLine, LinkMode, Sysroot, link::musl_loader};
use rucc_tuple::TargetTuple;

/// The target with this spelling.
fn target(tuple: &str) -> TargetTuple {
    tuple.parse().expect("a target this understands")
}

/// A musl sysroot for this target, in a cache directory that does not have to exist.
fn sysroot(tuple: &str) -> Sysroot {
    Sysroot::in_cache(Path::new("/cache"), target(tuple))
}

/// The file names of a list of paths, which is what the assertions are about.
fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.file_name().expect("a file").to_string_lossy().into_owned())
        .collect()
}

#[test]
fn crtn_goes_after_the_libraries_and_crti_goes_before_them() {
    // The ordering this file exists to get right. `crti.o` and `crtn.o` open and close `.init` and
    // `.fini`, so anything contributing to those has to land between them. Getting it wrong
    // produces a binary that links, runs, and does not run its static constructors, which is a
    // failure nobody attributes to the link line.
    let line = LinkLine::musl(&sysroot("aarch64-linux-musl"), LinkMode::Static);
    let objects = [PathBuf::from("main.o")];
    assert_eq!(
        names(&line.with_objects(&objects)),
        ["crt1.o", "crti.o", "main.o", "libc.a", "librucc_builtins.a", "crtn.o"]
    );
}

#[test]
fn the_three_modes_differ_in_the_first_start_file() {
    // The one that runs before `main`. A static-pie binary has to relocate itself before anything
    // else happens and `rcrt1.o` is what does that, so the mode picks the file rather than a flag
    // being enough.
    for (mode, first) in [
        (LinkMode::Static, "crt1.o"),
        (LinkMode::StaticPie, "rcrt1.o"),
        (LinkMode::Dynamic, "Scrt1.o"),
    ] {
        let line = LinkLine::musl(&sysroot("x86_64-linux-musl"), mode);
        assert_eq!(names(&line.start)[0], first, "{mode:?}");
    }
}

#[test]
fn a_dynamic_link_names_the_loader_and_a_static_one_does_not() {
    let dynamic = LinkLine::musl(&sysroot("aarch64-linux-musl"), LinkMode::Dynamic);
    assert!(dynamic.flags.iter().any(|flag| flag == "-dynamic-linker"));
    assert!(dynamic.flags.iter().any(|flag| flag == "/lib/ld-musl-aarch64.so.1"));

    let stat = LinkLine::musl(&sysroot("aarch64-linux-musl"), LinkMode::Static);
    assert!(stat.flags.iter().any(|flag| flag == "-static"));
    assert!(!stat.flags.iter().any(|flag| flag == "-dynamic-linker"));
}

#[test]
fn every_line_says_the_stack_is_not_executable() {
    // Several linkers still assume an executable stack when no input object says otherwise, and one
    // assembly file with no `.note.GNU-stack` is enough to get there. Saying it on the line is
    // cheaper than finding out which object failed to.
    for mode in [LinkMode::Static, LinkMode::StaticPie, LinkMode::Dynamic] {
        let line = LinkLine::musl(&sysroot("riscv64-linux-musl"), mode);
        let joined = line.flags.join(" ");
        assert!(joined.contains("-z noexecstack"), "{mode:?} did not say it");
    }
}

#[test]
fn the_builtins_are_searched_after_the_libc_that_calls_them() {
    // An archive searched before the thing that needs it contributes nothing, and musl calls some
    // of the builtins.
    let line = LinkLine::musl(&sysroot("armv7a-linux-musleabihf"), LinkMode::Static);
    let libraries = names(&line.libraries);
    let libc = libraries.iter().position(|name| name == "libc.a").expect("a libc");
    let builtins =
        libraries.iter().position(|name| name == "librucc_builtins.a").expect("the builtins");
    assert!(libc < builtins);
}

#[test]
fn arm_has_two_loaders_and_the_float_abi_picks_between_them() {
    // musl builds hard float and soft float ARM separately and names them differently, and they are
    // not interchangeable. This is the only row in the table where the loader depends on something
    // other than the architecture, which is why the float ABI is a tuple field.
    assert_eq!(musl_loader(target("armv7a-linux-musleabihf")), "/lib/ld-musl-armhf.so.1");
    assert_eq!(musl_loader(target("armv5te-linux-musleabi")), "/lib/ld-musl-arm.so.1");
}

#[test]
fn x32_has_its_own_loader_because_it_is_its_own_abi() {
    assert_eq!(musl_loader(target("x86_64-linux-gnux32")), "/lib/ld-musl-x32.so.1");
    assert_eq!(musl_loader(target("x86_64-linux-musl")), "/lib/ld-musl-x86_64.so.1");
}

#[test]
fn every_path_on_the_line_is_inside_the_sysroot() {
    // A link line that reached outside the sysroot would be a link that depended on the machine,
    // which is the failure the whole of section 8.5 is written against, one layer down.
    let sysroot = sysroot("s390x-linux-musl");
    let line = LinkLine::musl(&sysroot, LinkMode::Static);
    for path in line.with_objects(&[]) {
        assert!(path.starts_with(sysroot.root()), "{} escaped the sysroot", path.display());
    }
}
