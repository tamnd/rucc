//! The header search path, against the rule in section 8.5.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.5.
//!
//! The rule exists to prevent one failure, and the failure is quiet: a cross build picks up a
//! header from the machine it is running on, works there, and does not work anywhere else. So most
//! of what is asserted here is the absence of something rather than the presence of it, which is
//! the shape a test of a negative rule has to have.

use std::path::{Path, PathBuf};

use rucc_sysroot::{Kernel, Options, Origin, Sysroot, include_paths};
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
    let kernel = Kernel::for_target(Path::new("/cache"), host).expect("a Linux target has one");

    let found = include_paths(
        host,
        Some(host),
        &Options {
            user: &user,
            resources: Some(Path::new("/opt/rucc")),
            bundled: Some(&bundled),
            kernel: Some(&kernel),
            host_include: &[path("/usr/include")],
            ..Options::default()
        },
    );

    // Both `-I` first and in the order given, then the compiler's own, then the libc's. The user's
    // two are in their order and not sorted, because a user who put one before the other meant it.
    let origins: Vec<Origin> = found.iter().map(|entry| entry.origin).collect();
    assert_eq!(
        origins,
        [
            Origin::User,
            Origin::User,
            Origin::Compiler,
            Origin::Bundled,
            Origin::Bundled,
            Origin::Kernel,
            Origin::Kernel,
        ]
    );
    assert_eq!(found[0].path, path("/project/include"));
    assert_eq!(found[1].path, path("/project/vendor"));
    assert_eq!(found[2].path, path("/opt/rucc/include"));

    // Step 3 is four directories for a Linux target, in the order `zig cc -E -v` prints. The libc's
    // per architecture tree first, because a `bits/` header that exists for the x86 family has to
    // beat the generic one of the same name, and glibc's directory is named after the family.
    assert_eq!(found[3].path, path("/cache/sysroots/x86_64-linux-gnu/include/x86"));
    assert_eq!(found[4].path, path("/cache/sysroots/x86_64-linux-gnu/include/generic"));

    // Then the kernel's, which are shared between targets and so sit beside the sysroots rather
    // than inside one.
    assert_eq!(found[5].path, path("/cache/kernel-headers/x86"));
    assert_eq!(found[6].path, path("/cache/kernel-headers/generic"));

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
fn an_installed_sdk_serves_every_apple_target_and_not_only_the_one_this_machine_is() {
    // One SDK holds the headers of every Apple architecture, which is why Apple ships one for arm64
    // and x86-64 together. So the directories in it are the target's own rather than this machine's,
    // they reach step 3 through their own field, and a cross compile between two Apple targets on a
    // mac is served by the SDK that is already there.
    let host = target("aarch64-macos");
    let sdk = [path("/SDKs/MacOSX.sdk/usr/include")];
    for tuple in ["aarch64-macos", "x86_64-macos", "aarch64-ios"] {
        let found = include_paths(
            target(tuple),
            Some(host),
            &Options { sdk: &sdk, ..Options::default() },
        );
        assert_eq!(found.len(), 1, "{tuple}");
        assert_eq!(found[0].origin, Origin::Sdk, "{tuple}");
        assert_eq!(found[0].path, sdk[0], "{tuple}");
        // And it is not a host directory, because what is in it belongs to the target. The cross
        // compilation test above is written against that distinction.
        assert!(!found[0].origin.is_host(), "{tuple}");
    }
}

#[test]
fn a_target_behind_a_licence_wall_is_never_given_a_tree_of_ours() {
    // Section 13.4. There is no bundled sysroot for an Apple or an MSVC target and there will not be
    // one, so a path under the cache for either would name a directory nothing can ever put a file
    // in. The caller passes the layout it computed and the rule here is what refuses it.
    for tuple in ["aarch64-macos", "aarch64-ios", "x86_64-pc-windows-msvc"] {
        let walled = target(tuple);
        let bundled = Sysroot::in_cache(Path::new("/cache"), walled);
        let found = include_paths(
            walled,
            None,
            &Options {
                resources: Some(Path::new("/opt/rucc")),
                bundled: Some(&bundled),
                ..Options::default()
            },
        );
        assert_eq!(found.len(), 1, "{tuple} was offered a tree of ours");
        assert_eq!(found[0].origin, Origin::Compiler, "{tuple}");
    }
    // And the mingw-w64 target beside the MSVC one does get one, because its headers are ours to
    // ship and that is the whole reason it is the default Windows environment.
    let gnu = target("x86_64-pc-windows-gnu");
    let bundled = Sysroot::in_cache(Path::new("/cache"), gnu);
    let found =
        include_paths(gnu, None, &Options { bundled: Some(&bundled), ..Options::default() });
    assert!(found.iter().all(|entry| entry.origin == Origin::Bundled));
    assert_eq!(found.len(), 2);
}

#[test]
fn a_sysroot_the_user_named_beats_an_sdk_on_the_machine() {
    // `-isysroot` is the Darwin spelling of `--sysroot` and somebody who wrote one is naming the SDK
    // to compile against. An installed SDK winning over it would make the flag advice in the licence
    // wall's own message useless on the one machine where both exist.
    let host = target("aarch64-macos");
    let named = [path("/opt/sdks/MacOSX14.sdk/usr/include")];
    let sdk = [path("/SDKs/MacOSX.sdk/usr/include")];
    let found = include_paths(
        host,
        Some(host),
        &Options { sysroot: &named, sdk: &sdk, ..Options::default() },
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].origin, Origin::Sysroot);
    assert_eq!(found[0].path, named[0]);
}

#[test]
fn a_sysroot_the_user_named_beats_the_one_we_bundle() {
    let host = target("aarch64-linux-musl");
    let bundled = Sysroot::in_cache(Path::new("/cache"), host);
    // The shape a buildroot tree has, which is not the shape we lay one out in. The caller works
    // out what is under the root it was given, and step 3 is that list and nothing else.
    let named = [path("/opt/buildroot/usr/include")];

    let found = include_paths(
        host,
        Some(host),
        &Options {
            sysroot: &named,
            bundled: Some(&bundled),
            host_include: &[path("/usr/include")],
            ..Options::default()
        },
    );
    assert!(found.iter().all(|entry| entry.origin == Origin::Sysroot));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, path("/opt/buildroot/usr/include"));
}

#[test]
fn a_tree_the_user_laid_out_our_way_is_named_by_the_layout_rather_than_by_hand() {
    // The other half of the same field. A user whose tree has our shape passes the two directories
    // the layout names and gets them as step 3, so there is one spelling of the layout and not two.
    let host = target("aarch64-linux-musl");
    let named = Sysroot::at(path("/opt/ours"), host);
    let includes = named.includes();

    let found =
        include_paths(host, Some(host), &Options { sysroot: &includes, ..Options::default() });
    assert_eq!(found[0].path, path("/opt/ours/include/aarch64"));
    assert_eq!(found[1].path, path("/opt/ours/include/generic"));
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

#[test]
fn the_kernel_headers_come_after_the_libcs_and_not_before() {
    // Both trees have a `sys/` and the libc's is the one a program asking for `<sys/types.h>`
    // means. The kernel's own names, `asm/` and `linux/`, are not in a libc at all, so putting the
    // kernel last costs nothing and putting it first would shadow a header.
    let target = target("aarch64-linux-musl");
    let bundled = Sysroot::in_cache(Path::new("/cache"), target);
    let kernel = Kernel::for_target(Path::new("/cache"), target).expect("a Linux target has one");

    let found = include_paths(
        target,
        None,
        &Options { bundled: Some(&bundled), kernel: Some(&kernel), ..Options::default() },
    );
    let libc_last =
        found.iter().rposition(|entry| entry.origin == Origin::Bundled).expect("a libc");
    let kernel_first =
        found.iter().position(|entry| entry.origin == Origin::Kernel).expect("a kernel");
    assert!(libc_last < kernel_first);
}

#[test]
fn a_sysroot_the_user_named_does_not_get_our_kernel_headers_underneath_it() {
    // Section 8.5 replaces step 3 with the tree the user named rather than composing with it. A
    // buildroot or Yocto tree has its own `linux/` from its own kernel version, and mixing two
    // kernels' headers in one search path is how a structure gets one field from each.
    let target = target("x86_64-linux-gnu");
    let bundled = Sysroot::in_cache(Path::new("/cache"), target);
    let kernel = Kernel::for_target(Path::new("/cache"), target).expect("a Linux target has one");
    let named = [path("/opt/buildroot/usr/include")];

    let found = include_paths(
        target,
        None,
        &Options {
            sysroot: &named,
            bundled: Some(&bundled),
            kernel: Some(&kernel),
            ..Options::default()
        },
    );
    assert_eq!(found.len(), 1);
    assert!(!found.iter().any(|entry| entry.origin == Origin::Kernel));
}

#[test]
fn nostdinc_removes_the_kernel_headers_with_the_rest_of_step_three() {
    // A program compiled with `-nostdinc` is naming every system directory itself, and a `linux/`
    // it did not name is as much of a surprise as a `stdio.h` it did not name.
    let target = target("x86_64-linux-gnu");
    let bundled = Sysroot::in_cache(Path::new("/cache"), target);
    let kernel = Kernel::for_target(Path::new("/cache"), target).expect("a Linux target has one");

    let found = include_paths(
        target,
        None,
        &Options {
            resources: Some(Path::new("/opt/rucc")),
            bundled: Some(&bundled),
            kernel: Some(&kernel),
            no_std_inc: true,
            ..Options::default()
        },
    );
    assert_eq!(found.len(), 1, "only the compiler's own headers survive");
    assert_eq!(found[0].origin, Origin::Compiler);
}

#[test]
fn a_target_without_a_kernel_tree_searches_the_two_directories_it_always_did() {
    // The field is an `Option` and the driver fills it from `Kernel::for_target`, so a Windows or a
    // freestanding target passes `None` and step 3 is the libc's two directories. Asserted here as
    // well as in the layout tests, because the absence has to hold in the list and not only in the
    // function that decides it.
    for tuple in ["x86_64-pc-windows-gnu", "armv7m-none-eabi"] {
        let target = target(tuple);
        let bundled = Sysroot::in_cache(Path::new("/cache"), target);
        assert!(Kernel::for_target(Path::new("/cache"), target).is_none());

        let found = include_paths(
            target,
            None,
            &Options { bundled: Some(&bundled), kernel: None, ..Options::default() },
        );
        assert_eq!(found.len(), 2, "{tuple} searches more than the libc's two");
        assert!(found.iter().all(|entry| entry.origin == Origin::Bundled));
    }
}
