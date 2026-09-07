//! The header search path, against the rule in section 8.5.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.5.
//!
//! The rule exists to prevent one failure, and the failure is quiet: a cross build picks up a
//! header from the machine it is running on, works there, and does not work anywhere else. So most
//! of what is asserted here is the absence of something rather than the presence of it, which is
//! the shape a test of a negative rule has to have.

use std::path::{Path, PathBuf};

use rucc_sysroot::{Options, Origin, Sysroot, include_paths};
use rucc_tuple::{TARGETS, TargetEntry, TargetTuple};

/// The target with this spelling.
fn target(tuple: &str) -> TargetTuple {
    tuple.parse().expect("a target this understands")
}

/// A path, spelled once.
fn path(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn the_four_steps_come_out_in_the_order_the_rule_states_them() {
    let host = target("x86_64-linux-gnu");
    let user = [path("/project/include"), path("/project/vendor")];
    let bundled = Sysroot::in_cache(Path::new("/cache"), host);

    let found = include_paths(
        host,
        Some(host),
        &Options {
            user: &user,
            resources: Some(Path::new("/opt/rucc")),
            bundled: Some(&bundled),
            host_include: &[path("/usr/include")],
            ..Options::default()
        },
    );

    // Both `-I` first and in the order given, then the compiler's own, then the libc's. The user's
    // two are in their order and not sorted, because a user who put one before the other meant it.
    let origins: Vec<Origin> = found.iter().map(|entry| entry.origin).collect();
    assert_eq!(
        origins,
        [Origin::User, Origin::User, Origin::Compiler, Origin::Bundled, Origin::Bundled]
    );
    assert_eq!(found[0].path, path("/project/include"));
    assert_eq!(found[1].path, path("/project/vendor"));
    assert_eq!(found[2].path, path("/opt/rucc/include"));

    // The bundled tree is two directories, architecture specific first, because a `bits/` header
    // that exists for x86-64 has to beat the generic one of the same name.
    assert_eq!(found[3].path, path("/cache/sysroots/x86_64-linux-gnu/include/x86_64"));
    assert_eq!(found[4].path, path("/cache/sysroots/x86_64-linux-gnu/include/generic"));

    // And nothing from the host, even though the target is the host, because a bundled tree was
    // available and step 3 takes the first source that has one.
    assert!(!found.iter().any(|entry| entry.origin.is_host()));
}

#[test]
fn no_host_directory_reaches_a_cross_build() {
    // The whole point of the file. `spec/cross-compile/02-the-goal.md` claim 5 is byte identical
    // output from two hosts, and it holds because of this and only because of this.
    let host = target("x86_64-linux-gnu");
    let host_dirs = [path("/usr/include"), path("/usr/local/include")];

    for entry in TARGETS {
        let row = TargetEntry::parse(entry).expect("the table parses");
        if row == host {
            continue;
        }
        let found = include_paths(
            row,
            Some(host),
            &Options { host_include: &host_dirs, ..Options::default() },
        );
        assert!(
            !found.iter().any(|found| found.origin.is_host()),
            "{} picked up a host directory",
            entry.tuple
        );
    }
}

#[test]
fn an_unknown_host_is_treated_as_not_being_the_target() {
    // A driver that cannot say what it is running on cannot prove the target is the host, and
    // guessing yes is exactly the contamination the rule is written against. So the answer is the
    // careful one and the host directories are not used.
    let found = include_paths(
        target("x86_64-linux-gnu"),
        None,
        &Options { host_include: &[path("/usr/include")], ..Options::default() },
    );
    assert!(found.is_empty());
}

#[test]
fn the_host_directories_are_used_when_the_target_is_the_host_and_there_is_nothing_else() {
    // The native build with no bundled tree, which is what the compiler does today and has to keep
    // doing. This is the one place a host directory is legal.
    let host = target("aarch64-macos");
    let found = include_paths(
        host,
        Some(host),
        &Options { host_include: &[path("/usr/include")], ..Options::default() },
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].origin, Origin::Host);
}

#[test]
fn a_sysroot_the_user_named_beats_the_one_we_bundle() {
    let host = target("aarch64-linux-musl");
    let bundled = Sysroot::in_cache(Path::new("/cache"), host);
    let named = Sysroot::at(path("/opt/buildroot"), host);

    let found = include_paths(
        host,
        Some(host),
        &Options {
            sysroot: Some(&named),
            bundled: Some(&bundled),
            host_include: &[path("/usr/include")],
            ..Options::default()
        },
    );
    assert!(found.iter().all(|entry| entry.origin == Origin::Sysroot));
    assert_eq!(found[0].path, path("/opt/buildroot/include/aarch64"));
    assert_eq!(found[1].path, path("/opt/buildroot/include/generic"));
}

#[test]
fn nostdinc_removes_step_three_and_nobuiltininc_removes_step_two() {
    let host = target("x86_64-linux-musl");
    let bundled = Sysroot::in_cache(Path::new("/cache"), host);
    let base = Options {
        resources: Some(Path::new("/opt/rucc")),
        bundled: Some(&bundled),
        ..Options::default()
    };

    let without_libc =
        include_paths(host, Some(host), &Options { no_std_inc: true, ..base.clone() });
    assert_eq!(without_libc.len(), 1);
    assert_eq!(without_libc[0].origin, Origin::Compiler);

    let without_ours =
        include_paths(host, Some(host), &Options { no_builtin_inc: true, ..base.clone() });
    assert!(without_ours.iter().all(|entry| entry.origin == Origin::Bundled));

    let neither = include_paths(
        host,
        Some(host),
        &Options { no_std_inc: true, no_builtin_inc: true, ..base },
    );
    assert!(neither.is_empty());
}

#[test]
fn the_compilers_own_headers_are_offered_on_every_target_including_freestanding() {
    // Section 8.5 step 2 says always present, on every target, and never from a sysroot. A
    // freestanding target has no libc and still has `stddef.h`, because `stddef.h` describes this
    // compiler rather than a platform.
    for entry in TARGETS {
        let row = TargetEntry::parse(entry).expect("the table parses");
        let found = include_paths(
            row,
            None,
            &Options { resources: Some(Path::new("/opt/rucc")), ..Options::default() },
        );
        assert_eq!(found.len(), 1, "{} did not get the compiler's own headers", entry.tuple);
        assert_eq!(found[0].origin, Origin::Compiler);
        assert_eq!(found[0].path, path("/opt/rucc/include"));
    }
}

#[test]
fn the_answer_does_not_depend_on_which_host_is_asking() {
    // Claim 5 restated as the thing that makes it true. Two hosts, one target that is neither of
    // them, and the same list, path for path and origin for origin.
    let cross = target("riscv64-linux-musl");
    let bundled = Sysroot::in_cache(Path::new("/cache"), cross);
    let options = Options {
        resources: Some(Path::new("/opt/rucc")),
        bundled: Some(&bundled),
        host_include: &[path("/usr/include")],
        ..Options::default()
    };

    let from_linux = include_paths(cross, Some(target("x86_64-linux-gnu")), &options);
    let from_mac = include_paths(cross, Some(target("aarch64-macos")), &options);
    assert_eq!(from_linux, from_mac);
}
