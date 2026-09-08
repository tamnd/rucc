//! The sysroot manifest.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.6 and `spec/cross-compile/02-the-goal.md`
//! claim 5.

use rucc_sysroot::{Input, Licence, Manifest, ManifestError};
use rucc_tuple::TargetTuple;

/// The target with this spelling.
fn target(tuple: &str) -> TargetTuple {
    tuple.parse().expect("a target this understands")
}

/// An input with a hash that is the right shape and means nothing.
fn input(path: &str, source: &str, licence: Licence) -> Input {
    let mut sha256 = String::new();
    for (index, byte) in path.bytes().enumerate() {
        if index == 32 {
            break;
        }
        sha256.push_str(&format!("{:02x}", byte));
    }
    while sha256.len() < 64 {
        sha256.push('0');
    }
    Input { path: path.into(), source: source.into(), sha256, licence }
}

/// A manifest with three inputs, added in an order that is not their sorted order.
fn sample() -> Manifest {
    let mut manifest = Manifest::new(target("aarch64-linux-musl"));
    manifest.push(input("lib/libc.a", "musl-1.2.5", Licence::Mit));
    manifest.push(input("include/generic/stdio.h", "musl-1.2.5", Licence::Mit));
    manifest.push(input("include/aarch64/bits/alltypes.h", "musl-1.2.5", Licence::Mit));
    manifest
}

#[test]
fn a_manifest_survives_being_written_and_read() {
    let manifest = sample();
    let read = Manifest::parse(&manifest.render()).expect("what we just wrote");
    assert_eq!(read.target(), manifest.target());
    // Rendering sorts, so the read back inputs are the same set in the sorted order rather than the
    // order they were pushed in. That is the point of the sort and not a wrinkle of it.
    let mut expected = manifest.inputs().to_vec();
    expected.sort();
    assert_eq!(read.inputs(), expected.as_slice());
}

#[test]
fn the_order_files_were_added_in_does_not_reach_the_file() {
    // Claim 5 restated at the manifest level. A directory walk returns files in whatever order the
    // filesystem keeps them, which differs between ext4 and APFS, and a manifest carrying that
    // order would report a difference between two identical sysroots.
    let forwards = sample();

    let mut backwards = Manifest::new(target("aarch64-linux-musl"));
    for input in forwards.inputs().iter().rev() {
        backwards.push(input.clone());
    }

    assert_eq!(forwards.render(), backwards.render());
}

#[test]
fn one_input_that_cannot_be_shipped_makes_the_whole_sysroot_unshippable() {
    // The only reading of a licence wall that is worth anything. Section 8.6 is a rule in a
    // document, and this is the rule as a function something that publishes an artifact calls.
    let mut manifest = Manifest::new(target("aarch64-macos"));
    manifest.push(input("lib/librucc_builtins.a", "rucc", Licence::Apache2));
    assert!(manifest.redistributable());

    manifest.push(input("include/generic/stdio.h", "MacOSX15.sdk", Licence::AppleSdk));
    assert!(!manifest.redistributable());
}

#[test]
fn the_two_sdks_are_the_only_licences_that_refuse() {
    for licence in
        [Licence::Mit, Licence::Lgpl, Licence::Bsd, Licence::MingwPermissive, Licence::Apache2]
    {
        assert!(licence.redistributable(), "{licence}");
    }
    assert!(!Licence::AppleSdk.redistributable());
    assert!(!Licence::MicrosoftSdk.redistributable());
}

#[test]
fn every_licence_spelling_reads_back_as_itself() {
    for licence in [
        Licence::Mit,
        Licence::Lgpl,
        Licence::Bsd,
        Licence::MingwPermissive,
        Licence::Apache2,
        Licence::AppleSdk,
        Licence::MicrosoftSdk,
    ] {
        let read: Licence = licence.as_str().parse().expect("its own spelling");
        assert_eq!(read, licence);
    }
}

#[test]
fn sources_are_listed_once_and_sorted() {
    let mut manifest = Manifest::new(target("x86_64-linux-musl"));
    manifest.push(input("lib/librucc_builtins.a", "rucc", Licence::Apache2));
    manifest.push(input("lib/libc.a", "musl-1.2.5", Licence::Mit));
    manifest.push(input("include/generic/stdio.h", "musl-1.2.5", Licence::Mit));
    assert_eq!(manifest.sources(), ["musl-1.2.5", "rucc"]);
}

#[test]
fn a_manifest_that_does_not_parse_says_which_line_and_why() {
    // A manifest that fails to parse is a cache entry somebody has to decide about, and "invalid
    // manifest" is not enough to decide with.
    assert_eq!(Manifest::parse("something else\n"), Err(ManifestError::NotAManifest));
    assert_eq!(
        Manifest::parse("rucc sysroot manifest 2\n"),
        Err(ManifestError::UnknownVersion("2".into()))
    );
    assert_eq!(
        Manifest::parse("rucc sysroot manifest 1\ntarget\tmars-linux-gnu\n"),
        Err(ManifestError::BadTarget("mars-linux-gnu".into()))
    );

    let short = "rucc sysroot manifest 1\ntarget\tx86_64-linux-musl\nlib/libc.a\tmusl-1.2.5\n";
    assert_eq!(Manifest::parse(short), Err(ManifestError::BadInput { line: 3, fields: 2 }));

    let truncated =
        "rucc sysroot manifest 1\ntarget\tx86_64-linux-musl\nlib/libc.a\tmusl-1.2.5\tabc\tmit\n";
    assert_eq!(
        Manifest::parse(truncated),
        Err(ManifestError::BadHash { line: 3, found: "abc".into() })
    );
}

#[test]
fn a_truncated_hash_is_refused_rather_than_kept() {
    // A manifest with a short hash in it verifies nothing while looking like it does, which is
    // worse than one that verifies nothing and says so.
    let uppercase = "rucc sysroot manifest 1\ntarget\tx86_64-linux-musl\nlib/libc.a\tmusl-1.2.5\t\
                     ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789\tmit\n";
    assert!(matches!(Manifest::parse(uppercase), Err(ManifestError::BadHash { line: 3, .. })));
}
