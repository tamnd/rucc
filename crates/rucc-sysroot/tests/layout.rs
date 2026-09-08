//! The sysroot layout and the cache key.
//!
//! Design: `spec/cross-compile/08-sysroots.md` sections 8.2 and 8.3.

use std::path::Path;

use rucc_sysroot::{Sysroot, layout::can_be_bundled};
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
fn x32_gets_its_own_headers_because_its_types_are_a_different_width() {
    // A 64-bit architecture with 32-bit pointers. Every type in the headers that carries a pointer
    // or a `long` is a different size, so it cannot read the x86-64 tree.
    let cache = Path::new("/cache");
    let x32 = Sysroot::in_cache(cache, target("x86_64-linux-gnux32"));
    let lp64 = Sysroot::in_cache(cache, target("x86_64-linux-gnu"));
    assert_eq!(x32.header_arch(), "x32");
    assert_eq!(lp64.header_arch(), "x86_64");
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
