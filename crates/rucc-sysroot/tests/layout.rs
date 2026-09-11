//! The sysroot layout and the cache key.
//!
//! Design: `spec/cross-compile/08-sysroots.md` sections 8.2 and 8.3.

use std::path::Path;

use rucc_sysroot::{Kernel, Sysroot, layout::can_be_bundled};
use rucc_tuple::{TARGETS, TargetEntry, TargetTuple};

/// The target with this spelling.
fn target(tuple: &str) -> TargetTuple {
    tuple.parse().expect("a target this understands")
}

#[test]
fn the_cache_key_is_the_whole_tuple() {
    // `spec/cross-compile/03-target-model.md` section 3.2's whole argument, checked. A key that
    // dropped a field would serve one target's sysroot to another, and the failure would be a
    // missing symbol at link time on one machine and not on another.
    let cache = Path::new("/cache");
    let mut keys: Vec<String> = TARGETS
        .iter()
        .map(|entry| {
            let row = TargetEntry::parse(entry).expect("the table parses");
            Sysroot::in_cache(cache, row).cache_key()
        })
        .collect();
    let total = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), total, "two rows of the table share a sysroot directory");
}

#[test]
fn a_pinned_libc_version_is_a_different_sysroot() {
    // The reason `env_version` is in the tuple. Headers for glibc 2.28 and headers for 2.34 are
    // different text, and a cache that served one for the other would be right until it was not.
    let cache = Path::new("/cache");
    let plain = Sysroot::in_cache(cache, target("x86_64-linux-gnu"));
    let pinned = Sysroot::in_cache(cache, target("x86_64-linux-gnu.2.28"));
    assert_ne!(plain.cache_key(), pinned.cache_key());
    assert_ne!(plain.root(), pinned.root());
}

#[test]
fn the_baseline_within_an_architecture_does_not_change_the_header_directory() {
    // Section 8.3's split is per architecture and not per baseline. `armv7a` and `armv5te` read the
    // same `arm` headers, because a header does not care which instructions the chip has. They are
    // still different sysroots, because the ABI differs, and the two facts are separate.
    let cache = Path::new("/cache");
    let hard = Sysroot::in_cache(cache, target("armv7a-linux-musleabihf"));
    let soft = Sysroot::in_cache(cache, target("armv5te-linux-musleabi"));
    assert_eq!(hard.header_arch(), "arm");
    assert_eq!(soft.header_arch(), "arm");
    assert_ne!(hard.cache_key(), soft.cache_key());
}

#[test]
fn the_two_libcs_split_x32_in_two_different_places() {
    // A 64-bit architecture with 32-bit pointers. Every type in the headers that carries a pointer
    // or a `long` is a different size, so reading the LP64 headers would be wrong, and the two
    // libcs say so differently. musl has a directory for it, which is `arch/x32` in its source.
    // glibc has one x86 family directory for i386, x86-64 and x32, and the files in it branch on
    // `__ILP32__` themselves, starting with `bits/wordsize.h`.
    let cache = Path::new("/cache");
    let x32 = Sysroot::in_cache(cache, target("x86_64-linux-gnux32"));
    let lp64 = Sysroot::in_cache(cache, target("x86_64-linux-gnu"));
    assert_eq!(x32.header_arch(), "x86");
    assert_eq!(lp64.header_arch(), "x86");
    // And still two sysroots, because the link inputs are not shared even where the headers are.
    assert_ne!(x32.cache_key(), lp64.cache_key());

    let musl_x32 = Sysroot::in_cache(cache, target("x86_64-linux-muslx32"));
    let musl_lp64 = Sysroot::in_cache(cache, target("x86_64-linux-musl"));
    assert_eq!(musl_x32.header_arch(), "x32");
    assert_eq!(musl_lp64.header_arch(), "x86_64");
}

#[test]
fn glibc_names_its_header_directory_after_the_family_and_musl_after_the_architecture() {
    // Checked against Zig 0.16, which ships twelve glibc directories named after families and
    // seventeen musl directories named after architectures. The producer installs what the libc's
    // own build installs, so the name has to follow the libc rather than a scheme of ours.
    let cache = Path::new("/cache");
    for (tuple, family, own) in [
        ("i686-linux-gnu", "x86", "i386"),
        ("riscv64-linux-gnu", "riscv", "riscv64"),
        ("powerpc64le-linux-gnu", "powerpc", "powerpc64"),
        ("loongarch64-linux-gnu", "loongarch", "loongarch64"),
    ] {
        let gnu = Sysroot::in_cache(cache, target(tuple));
        assert_eq!(gnu.header_arch(), family, "{tuple} reads the wrong glibc directory");
        let musl = Sysroot::in_cache(cache, target(&tuple.replace("-gnu", "-musl")));
        assert_eq!(musl.header_arch(), own, "{tuple} reads the wrong musl directory");
    }
}

#[test]
fn the_architecture_specific_headers_are_searched_before_the_generic_ones() {
    let sysroot = Sysroot::in_cache(Path::new("/cache"), target("aarch64-linux-musl"));
    let includes = sysroot.includes();
    assert_eq!(includes[0], sysroot.arch_include());
    assert_eq!(includes[1], sysroot.generic_include());
    assert!(includes[0].ends_with("include/aarch64"));
    assert!(includes[1].ends_with("include/generic"));
}

#[test]
fn everything_is_under_the_root_and_nothing_is_absolute_from_somewhere_else() {
    // A sysroot that reached outside its own directory would be a cache entry that could not be
    // deleted, moved or copied, and the cache in `spec/cross-compile/13-distribution.md` does all
    // three.
    let sysroot = Sysroot::in_cache(Path::new("/cache"), target("aarch64-linux-musl"));
    for path in
        [sysroot.arch_include(), sysroot.generic_include(), sysroot.lib(), sysroot.manifest_path()]
    {
        assert!(path.starts_with(sysroot.root()), "{} escaped the root", path.display());
    }
}

#[test]
fn the_two_licence_walls_are_the_only_targets_we_cannot_bundle() {
    // Section 8.2's table has seven rows and section 8.6 says two of them are legal rather than
    // technical. Everything else is engineering, and this is the list stated as code so that a new
    // row cannot quietly join the walls.
    for entry in TARGETS {
        let row = TargetEntry::parse(entry).expect("the table parses");
        let expected = !matches!(
            (row.os(), row.env()),
            (rucc_tuple::Os::MacOs | rucc_tuple::Os::IOs, _) | (_, rucc_tuple::Env::Msvc)
        );
        assert_eq!(can_be_bundled(row), expected, "{}", entry.tuple);
    }
    assert!(can_be_bundled(target("aarch64-linux-musl")));
    assert!(can_be_bundled(target("x86_64-pc-windows-gnu")));
    assert!(!can_be_bundled(target("x86_64-pc-windows-msvc")));
    assert!(!can_be_bundled(target("aarch64-macos")));
    // Freestanding needs nine compiler headers and no link inputs, so there is nothing to fetch and
    // nothing to refuse.
    assert!(can_be_bundled(target("armv7m-none-eabi")));
}

#[test]
fn the_kernel_headers_are_shared_rather_than_copied_per_target() {
    // The reason they are not under a sysroot. `linux/` and `asm-generic/` are nine megabytes of
    // files that are byte for byte the same whatever the target, so two targets sharing an
    // architecture share one tree and two architectures share everything but `asm/`.
    let cache = Path::new("/cache");
    let gnu = Kernel::for_target(cache, target("x86_64-linux-gnu")).expect("x86-64 has a kernel");
    let musl = Kernel::for_target(cache, target("x86_64-linux-musl")).expect("so does musl");
    let arm = Kernel::for_target(cache, target("aarch64-linux-gnu")).expect("so does aarch64");
    assert_eq!(gnu, musl, "the libc does not change the kernel's headers");
    assert_eq!(gnu.generic_include(), arm.generic_include());
    assert_ne!(gnu.arch_include(), arm.arch_include());
    assert!(!gnu.arch_include().starts_with("/cache/sysroots"));
}

#[test]
fn the_kernel_has_its_own_name_for_the_machine() {
    // A third spelling, after ours and the libc's. `arm64` is the directory under `arch/` in the
    // kernel's source and `aarch64` is not, and the 31-bit s390 port leaving did not rename the one
    // that stayed.
    let cache = Path::new("/cache");
    for (tuple, arch) in [
        ("aarch64-linux-gnu", "arm64"),
        ("s390x-linux-gnu", "s390"),
        ("x86_64-linux-gnu", "x86"),
        ("i686-linux-gnu", "x86"),
        ("riscv64-linux-gnu", "riscv"),
        ("powerpc64le-linux-gnu", "powerpc"),
        ("loongarch64-linux-gnu", "loongarch"),
        ("armv7a-linux-gnueabihf", "arm"),
    ] {
        let kernel = Kernel::for_target(cache, target(tuple)).expect("a Linux target has a kernel");
        assert_eq!(kernel.arch(), arch, "{tuple} reads the wrong asm directory");
        assert!(kernel.arch_include().ends_with(arch));
    }
}

#[test]
fn only_linux_with_a_libc_we_produce_has_kernel_headers() {
    // Windows, Darwin and the BSDs have their own system headers and no `linux/` at all,
    // freestanding has no system call interface, and bionic carries its own scrubbed copy of the
    // uapi headers rather than reading this tree.
    let cache = Path::new("/cache");
    for tuple in ["x86_64-pc-windows-gnu", "aarch64-macos", "armv7m-none-eabi", "wasm32-wasi"] {
        assert!(Kernel::for_target(cache, target(tuple)).is_none(), "{tuple} has no kernel tree");
    }
    assert!(Kernel::for_target(cache, target("aarch64-linux-android")).is_none());
    assert!(Kernel::for_target(cache, target("x86_64-linux-gnu")).is_some());
}

#[test]
fn the_kernels_asm_is_searched_before_its_shared_tree() {
    let cache = Path::new("/cache");
    let kernel = Kernel::for_target(cache, target("aarch64-linux-musl")).expect("a kernel");
    let includes = kernel.includes();
    assert_eq!(includes[0], kernel.arch_include());
    assert_eq!(includes[1], kernel.generic_include());
    assert!(includes[0].ends_with("kernel-headers/arm64"));
    assert!(includes[1].ends_with("kernel-headers/generic"));
}
