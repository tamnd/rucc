//! The link line, for each of the three cases a target's libc can be.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.2, `spec/cross-compile/09-libc-stubs.md`
//! section 9.3 and `spec/cross-compile/11-linking.md` section 11.3.

use std::path::{Path, PathBuf};

use rucc_sysroot::argv::{Invocation, argv};
use rucc_sysroot::link::{glibc_loader, libc, loader, musl_loader};
use rucc_sysroot::{Libc, LinkLine, LinkMode, Sysroot};
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
fn the_modes_differ_in_the_first_start_file() {
    // The one that runs before `main`. A static-pie binary has to relocate itself before anything
    // else happens and `rcrt1.o` is what does that, so the mode picks the file rather than a flag
    // being enough. A shared object has no first start file at all, since nothing starts one.
    for (mode, first) in [
        (LinkMode::Static, Some("crt1.o")),
        (LinkMode::StaticPie, Some("rcrt1.o")),
        (LinkMode::Dynamic, Some("Scrt1.o")),
        (LinkMode::DynamicNoPie, Some("crt1.o")),
        (LinkMode::Shared, None),
    ] {
        let line = LinkLine::musl(&sysroot("x86_64-linux-musl"), mode);
        let names = names(&line.start);
        assert_eq!(names.first().map(String::as_str), first.or(Some("crti.o")), "{mode:?}");
        assert_eq!(names.last().map(String::as_str), Some("crti.o"), "{mode:?}");
    }
}

#[test]
fn a_dynamic_link_names_the_loader_and_a_static_one_does_not() {
    // The flags are `argv`'s and the loader is this module's, which is the division: what will
    // start the program is a fact about the target, and how it is told to the linker is not.
    let args = |mode| {
        let sysroot = sysroot("aarch64-linux-musl");
        let options = Invocation { mode, ..Invocation::default() };
        argv(target("aarch64-linux-musl"), &sysroot, &options).expect("a line")
    };
    let dynamic = args(LinkMode::Dynamic);
    let at = dynamic.iter().position(|arg| arg == "-dynamic-linker").expect("the flag");
    assert_eq!(dynamic[at + 1], "/lib/ld-musl-aarch64.so.1");
    assert_eq!(loader(target("aarch64-linux-musl")), Some("/lib/ld-musl-aarch64.so.1"));

    let still = args(LinkMode::Static);
    assert!(still.contains(&"-static".to_owned()));
    assert!(!still.contains(&"-dynamic-linker".to_owned()));
}

#[test]
fn every_line_says_the_stack_is_not_executable() {
    // Several linkers still assume an executable stack when no input object says otherwise, and one
    // assembly file with no `.note.GNU-stack` is enough to get there. Saying it on the line is
    // cheaper than finding out which object failed to.
    for mode in [LinkMode::Static, LinkMode::StaticPie, LinkMode::Dynamic, LinkMode::Shared] {
        let sysroot = sysroot("riscv64-linux-musl");
        let options = Invocation { mode, ..Invocation::default() };
        let args = argv(target("riscv64-linux-musl"), &sysroot, &options).expect("a line");
        assert!(args.join(" ").contains("-z noexecstack"), "{mode:?} did not say it");
    }
}

#[test]
fn glibc_and_musl_name_different_loaders_for_the_same_machine() {
    // Not two spellings of one path. glibc's names come from each port's history, so three of them
    // are called `ld64.so` and i386's has no architecture in it at all, and a binary naming the
    // wrong one does not start.
    assert_eq!(glibc_loader(target("x86_64-linux-gnu")), "/lib64/ld-linux-x86-64.so.2");
    assert_eq!(musl_loader(target("x86_64-linux-musl")), "/lib/ld-musl-x86_64.so.1");
    // Two files called `ld64.so` with two different numbers in two different directories, which is
    // the clearest case for this being a table rather than a rule.
    assert_eq!(glibc_loader(target("s390x-linux-gnu")), "/lib/ld64.so.1");
    assert_eq!(glibc_loader(target("powerpc64le-linux-gnu")), "/lib64/ld64.so.2");
    assert_eq!(glibc_loader(target("i686-linux-gnu")), "/lib/ld-linux.so.2");
    assert_eq!(glibc_loader(target("armv7a-linux-gnueabihf")), "/lib/ld-linux-armhf.so.3");
    assert_eq!(glibc_loader(target("riscv64-linux-gnu")), "/lib/ld-linux-riscv64-lp64d.so.1");
}

#[test]
fn a_target_with_no_libc_of_ours_has_no_loader_to_name() {
    // Three different reasons for the same answer: freestanding has no libc, WASI has no loader of
    // this kind, and Darwin and Windows have one whose path is not on the link line.
    for spelling in ["x86_64-none", "wasm32-wasi", "aarch64-macos", "x86_64-windows-gnu"] {
        assert_eq!(loader(target(spelling)), None, "{spelling}");
    }
}

#[test]
fn the_libc_the_target_names_picks_the_line() {
    // glibc is linked against dynamically, so what goes on the line is the generated `libc.so`
    // rather than an archive, and musl's is the real `libc.a`. The shape is the same and the files
    // are not, which is why the dispatch is one function.
    let gnu = LinkLine::for_target(&sysroot("x86_64-linux-gnu"), LinkMode::Dynamic);
    assert_eq!(names(&gnu.libraries), ["libc.so", "librucc_builtins.a"]);
    let musl = LinkLine::for_target(&sysroot("x86_64-linux-musl"), LinkMode::Static);
    assert_eq!(names(&musl.libraries), ["libc.a", "librucc_builtins.a"]);
}

#[test]
fn the_cases_of_section_8_2_are_that_many_different_lines() {
    // What the sysroot holds rather than what the target is called: nothing, a real archive, a stub
    // shared object, or a set of import libraries. Every other difference between these targets
    // leaves the line alone.
    assert_eq!(libc(target("x86_64-linux-musl")), Libc::Archive);
    assert_eq!(libc(target("x86_64-linux-gnu")), Libc::Stub);
    assert_eq!(libc(target("x86_64-freebsd")), Libc::Stub);
    assert_eq!(libc(target("aarch64-linux-android")), Libc::Stub);
    assert_eq!(libc(target("armv7m-none-eabi")), Libc::None);
    assert_eq!(libc(target("x86_64-windows-gnu")), Libc::Import);
    // The format decides rather than the environment, because `gnu` means mingw-w64 here and glibc
    // one line above.
    assert_eq!(libc(target("aarch64-windows-msvc")), Libc::Import);

    // And a freestanding line is our runtime and nothing else, with no start files at either end,
    // because the files that would be there come from a libc this target does not have.
    let bare = LinkLine::for_target(&sysroot("armv7m-none-eabi"), LinkMode::Static);
    assert!(bare.start.is_empty());
    assert!(bare.end.is_empty());
    assert_eq!(names(&bare.libraries), ["librucc_builtins.a"]);
}

#[test]
fn a_windows_line_is_one_start_file_and_a_set_of_libraries_rather_than_one() {
    // The import library case, which is the same idea as a stub in a different container and a
    // different number of files. There is no `crti.o` and no `crtn.o` either, because PE has no
    // `.init` and `.fini` sections for a pair of files to open and close.
    let line = LinkLine::for_target(&sysroot("x86_64-windows-gnu"), LinkMode::Dynamic);
    assert_eq!(names(&line.start), ["crt2.o"]);
    assert!(line.end.is_empty());
    assert_eq!(
        names(&line.libraries),
        [
            "libmingw32.a",
            "libmoldname.a",
            "libmingwex.a",
            "libmsvcrt.a",
            "libadvapi32.a",
            "libshell32.a",
            "libuser32.a",
            "libkernel32.a",
            "librucc_builtins.a",
        ]
    );
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
