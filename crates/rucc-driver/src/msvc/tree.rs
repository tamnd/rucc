//! Where each file in a Microsoft package goes in the tree `--sysroot` reads.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.5. The shape is the one
//! [`crate::library`] already looks in for an MSVC target, which is `xwin`'s: `crt/include` and
//! `crt/lib/<arch>` for the Visual C++ runtime, and `sdk/include/<part>` and
//! `sdk/lib/<part>/<arch>` for the Windows SDK. Nothing here writes anything. It is the mapping
//! only, so that what the tree is can be tested without a download.
//!
//! Every answer is a relative path with forward slashes in it rather than a `PathBuf`, because the
//! caller has two things to do with it and both want a string: hand it to [`rucc_unpack::under`],
//! which is what decides whether a name out of an archive may be written at all, and record it in
//! the sysroot manifest, where a path with a host's separators in it would be a fact about the
//! machine that unpacked the tree.
//!
//! # What is left behind, and why
//!
//! The packages hold a good deal that a C compiler cannot use, and the measurements under each of
//! these are of the September 2026 kit for one architecture.
//!
//! `winrt` and `cppwinrt` are 219 MB of the Windows SDK's 267 MB of headers and every one of them
//! is C++. `cppwinrt` needs C++20 to parse at all. This compiler compiles C, so they are not
//! unpacked, and the reason to say that here rather than to quietly take everything is that
//! [`crate::library`] still names both directories in the search path: a tree somebody produced
//! with `xwin`, which does take everything, works unchanged, and ours has two of the directories
//! the search path allows for missing.
//!
//! The Visual C++ packages carry debug symbols beside the libraries, and `lib/<chip>` has an
//! `enclave`, a `store` and a `uwp` subdirectory under it. The subdirectories are the same
//! libraries built against a narrower API surface, which is a thing to pick deliberately rather
//! than to merge into one directory, so only the top of `lib/<chip>` is taken and only the files a
//! linker can read out of it.
//!
//! The Windows SDK's `Catalogs` are signatures for the installer, `Source` is the C source of the
//! universal CRT, `DesignTime` is for Visual Studio's designer, `bin` is tools built for Windows,
//! and `ucrt_enclave` is the enclave build again. None of them is a header or a library.

use rucc_sysroot::msvc::Chip;

/// The parts of the Windows SDK's headers that get unpacked.
///
/// `um` is user mode, which is `windows.h` and what it reaches, `shared` is what user mode and
/// kernel mode headers both include, and `ucrt` is the C library. That is the whole of the C
/// surface of the kit. See this module's note for the two that are not here.
const PARTS: [&str; 3] = ["ucrt", "um", "shared"];

/// How the tree spells an architecture in a directory name.
///
/// LLVM's names rather than Microsoft's, because that is what `xwin` produces and the tree is the
/// one it produces. So a target that was fetched with `xwin` and one that was fetched with this
/// both answer to the same `--sysroot`, and the `x64` in Microsoft's own package names stays in
/// the packages where a person reading a manifest would go looking for it.
#[must_use]
pub const fn in_tree(chip: Chip) -> &'static str {
    match chip {
        Chip::X86 => "x86",
        Chip::X64 => "x86_64",
        Chip::Arm => "aarch",
        Chip::Arm64 => "aarch64",
    }
}

/// Where a member of a Visual C++ package goes, or [`None`] for one the tree does not want.
///
/// The names in a vsix all start `Contents/VC/Tools/MSVC/<version>/`, which is where the Visual
/// Studio installer would put them under an installation. The version is read off the member
/// rather than matched against anything, because the package it came out of is what said which
/// version this is and a member that disagreed with its own package would not be a thing this
/// could do anything about.
#[must_use]
pub fn crt(member: &str, chip: Chip) -> Option<String> {
    let rest = under_tools(member)?;
    if let Some(under) = rest.strip_prefix("include/") {
        return (!under.is_empty()).then(|| format!("crt/include/{under}"));
    }
    let under = rest.strip_prefix("lib/")?;
    let mut parts = under.split('/');
    let dir = parts.next()?;
    let file = parts.next()?;
    // A third component is `enclave`, `store` or `uwp`, which this module's note says are
    // deliberately not merged in.
    if parts.next().is_some() || !dir.eq_ignore_ascii_case(chip.in_installer()) {
        return None;
    }
    linkable(file).then(|| format!("crt/lib/{}/{file}", in_tree(chip)))
}

/// Where a file a Windows SDK installer describes goes, or [`None`] for one the tree does not want.
///
/// `directory` is what the installer's own tables say, which is `Windows Kits/10/Include/<version>`
/// and the part under it, and `name` is what the file is called there. The two are separate because
/// that is how an MSI holds them, and joining them here rather than at the call site keeps the one
/// place that knows the tree's shape in this module.
#[must_use]
pub fn sdk(directory: &str, name: &str, chip: Chip) -> Option<String> {
    let parts: Vec<&str> = directory.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    if parts.len() < 5 || !parts[0].eq_ignore_ascii_case("Windows Kits") || parts[1] != "10" {
        return None;
    }
    let part = parts[4].to_ascii_lowercase();
    if parts[2].eq_ignore_ascii_case("Include") {
        if !PARTS.contains(&part.as_str()) {
            return None;
        }
        // Everything from the part down, because the kit nests: `ucrt/sys` and
        // `shared/netcx/shared/1.0/net` are both real and both keep the name they have there.
        let deeper = parts[5..].iter().map(|part| format!("{part}/")).collect::<String>();
        return Some(format!("sdk/include/{part}/{deeper}{name}"));
    }
    if parts[2].eq_ignore_ascii_case("Lib") {
        // `Lib/<version>/<part>/<chip>` and nothing deeper. Two of the three parts, because
        // `shared` is headers that both sides of the kernel boundary include and has no libraries
        // in it. `ucrt_enclave` is refused by the same check, since it is not one of the parts.
        if parts.len() != 6 || !matches!(part.as_str(), "ucrt" | "um") {
            return None;
        }
        if !parts[5].eq_ignore_ascii_case(chip.in_installer()) {
            return None;
        }
        return Some(format!("sdk/lib/{part}/{}/{name}", in_tree(chip)));
    }
    None
}

/// The all lowercase spelling of the last component of `path`, or [`None`] when it has no capital
/// in it.
///
/// Windows file names are not case sensitive and the SDK's are not consistent: 129 of the 366
/// libraries in the desktop kit for x86-64 have a capital in them, `AclUI.Lib` among them, and the
/// headers are the same story with `Windows.h`. A program that says `#pragma comment(lib, "aclui")`
/// or includes `windows.h` is correct on Windows and finds nothing on a host where the name is a
/// string of bytes. So the file is written under the name the package gives it and the lowercase
/// spelling is a symlink beside it, which is what `xwin` does and for the same reason.
#[must_use]
pub fn lowercase(path: &str) -> Option<String> {
    let (directory, name) = match path.rsplit_once('/') {
        Some((directory, name)) => (directory, name),
        None => ("", path),
    };
    let lower = name.to_ascii_lowercase();
    if lower == name {
        return None;
    }
    Some(if directory.is_empty() { lower } else { format!("{directory}/{lower}") })
}

/// The part of a vsix member under the version directory, or [`None`] for a member of the package
/// rather than of the installation.
///
/// A vsix is a zip with metadata in it: `manifest.json`, `[Content_Types].xml`, `_rels/.rels` and a
/// signature. None of those is a file the installation would have, which is what `Contents/` in
/// front of a name means, so they fall out here rather than being listed by name.
fn under_tools(member: &str) -> Option<&str> {
    let rest = member.strip_prefix("Contents/VC/Tools/MSVC/")?;
    let (_version, rest) = rest.split_once('/')?;
    (!rest.is_empty()).then_some(rest)
}

/// Whether a file in a Visual C++ library directory is one a linker reads.
///
/// The directory also holds a `.pdb` per library, which is debug symbols for a debugger this
/// compiler does not have and is most of the bytes, and one `.dll`, which is a managed assembly
/// for C++/CLI. Neither is an input to a link.
fn linkable(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    ext.eq_ignore_ascii_case("lib") || ext.eq_ignore_ascii_case("obj")
}

#[cfg(test)]
mod tests {
    use super::{crt, in_tree, lowercase, sdk};
    use rucc_sysroot::msvc::Chip;

    /// What a mapping is expected to produce, since every one of these is an owned string.
    fn at(path: &str) -> Option<String> {
        Some(path.to_owned())
    }

    #[test]
    fn the_crt_headers_keep_their_shape_under_the_tree() {
        // Every one of these is a member of the September 2026 headers package.
        let member = "Contents/VC/Tools/MSVC/14.44.35207/include/stdio.h";
        assert_eq!(crt(member, Chip::X64), at("crt/include/stdio.h"));
        let member = "Contents/VC/Tools/MSVC/14.44.35207/include/CodeAnalysis/sourceannotations.h";
        assert_eq!(crt(member, Chip::X64), at("crt/include/CodeAnalysis/sourceannotations.h"));
        // The headers package is one package for every architecture, so which chip is being asked
        // about does not come into it.
        let member = "Contents/VC/Tools/MSVC/14.44.35207/include/vcruntime.h";
        assert_eq!(crt(member, Chip::Arm64), crt(member, Chip::X86));
    }

    #[test]
    fn a_crt_library_goes_under_the_architecture_the_tree_names() {
        let member = "Contents/VC/Tools/MSVC/14.44.35207/lib/x64/libcmt.lib";
        assert_eq!(crt(member, Chip::X64), at("crt/lib/x86_64/libcmt.lib"));
        // The store package's object fragments are libraries as far as this is concerned, because
        // they are inputs to a link. `chkstk.obj` is the one nothing links without.
        let member = "Contents/VC/Tools/MSVC/14.44.35207/lib/x64/chkstk.obj";
        assert_eq!(crt(member, Chip::X64), at("crt/lib/x86_64/chkstk.obj"));
        // The directory is spelled `arm64` in the package even though the package id says `ARM64`.
        let member = "Contents/VC/Tools/MSVC/14.44.35207/lib/arm64/libvcruntime.lib";
        assert_eq!(crt(member, Chip::Arm64), at("crt/lib/aarch64/libvcruntime.lib"));
        // And a library for another chip is not this target's business.
        assert_eq!(crt(member, Chip::X64), None);
    }

    #[test]
    fn what_a_link_cannot_read_is_left_in_the_package() {
        for member in [
            "Contents/VC/Tools/MSVC/14.44.35207/lib/x64/libcmt.amd64.pdb",
            "Contents/VC/Tools/MSVC/14.44.35207/lib/x64/Microsoft.VisualC.STLCLR.dll",
            "Contents/VC/Tools/MSVC/14.44.35207/lib/x64/enclave/libvcruntime.lib",
            "Contents/VC/Tools/MSVC/14.44.35207/lib/x64/store/msvcrt.lib",
            "Contents/VC/Tools/MSVC/14.44.35207/lib/x64/uwp/vccorlib.lib",
            "Contents/VC/Tools/MSVC/14.44.35207/crt/src/x64/memmove.asm",
            "Contents/VC/Tools/MSVC/14.44.35207/Auxiliary/Microsoft.VC.Paths.x64.Store.props",
            "manifest.json",
            "[Content_Types].xml",
            "_rels/.rels",
        ] {
            assert_eq!(crt(member, Chip::X64), None, "{member}");
        }
    }

    #[test]
    fn the_three_parts_of_the_kit_a_c_compiler_reads_are_the_ones_that_land() {
        let kit = "Windows Kits/10/Include/10.0.26100.0";
        let um = format!("{kit}/um");
        assert_eq!(sdk(&um, "windows.h", Chip::X64), at("sdk/include/um/windows.h"));
        assert_eq!(
            sdk(&format!("{kit}/ucrt"), "stdio.h", Chip::X64),
            at("sdk/include/ucrt/stdio.h")
        );
        assert_eq!(
            sdk(&format!("{kit}/ucrt/sys"), "stat.h", Chip::X64),
            at("sdk/include/ucrt/sys/stat.h")
        );
        assert_eq!(
            sdk(&format!("{kit}/shared/netcx/shared/1.0/net"), "ring.h", Chip::X64),
            at("sdk/include/shared/netcx/shared/1.0/net/ring.h")
        );
        // The headers are the same whatever the target, so the chip does not come into it here
        // either. It is a parameter because the libraries below share this function.
        assert_eq!(sdk(&um, "windows.h", Chip::Arm), sdk(&um, "windows.h", Chip::X86));
    }

    #[test]
    fn the_cpp_only_headers_and_the_installer_s_own_files_are_left_in_the_kit() {
        for directory in [
            "Windows Kits/10/Include/10.0.26100.0/winrt",
            "Windows Kits/10/Include/10.0.26100.0/cppwinrt/winrt/impl",
            "Windows Kits/10/Catalogs",
            "Windows Kits/10/Source/10.0.26100.0/ucrt/string",
            "Windows Kits/10/DesignTime/CommonConfiguration/Neutral",
            "Windows Kits/10/bin/10.0.26100.0/x64/ucrt",
            "Windows Kits/10/Lib/10.0.26100.0/ucrt_enclave/x64",
        ] {
            assert_eq!(sdk(directory, "thing.h", Chip::X64), None, "{directory}");
        }
    }

    #[test]
    fn a_kit_library_goes_under_its_part_and_its_architecture() {
        let kit = "Windows Kits/10/Lib/10.0.26100.0";
        assert_eq!(
            sdk(&format!("{kit}/um/x64"), "kernel32.Lib", Chip::X64),
            at("sdk/lib/um/x86_64/kernel32.Lib")
        );
        assert_eq!(
            sdk(&format!("{kit}/ucrt/arm64"), "libucrt.lib", Chip::Arm64),
            at("sdk/lib/ucrt/aarch64/libucrt.lib")
        );
        // One installer holds the user mode libraries for all three architectures, which is why
        // the chip is asked about at all.
        assert_eq!(sdk(&format!("{kit}/um/x86"), "kernel32.Lib", Chip::X64), None);
        assert_eq!(sdk(&format!("{kit}/um/arm64"), "kernel32.Lib", Chip::X64), None);
        // `shared` is headers only, so a directory that claimed to be a library one is not a
        // directory this writes into.
        assert_eq!(sdk(&format!("{kit}/shared/x64"), "thing.lib", Chip::X64), None);
    }

    #[test]
    fn a_name_with_a_capital_in_it_gets_a_lowercase_one_beside_it() {
        assert_eq!(lowercase("sdk/lib/um/x86_64/AclUI.Lib"), at("sdk/lib/um/x86_64/aclui.lib"));
        assert_eq!(lowercase("sdk/include/um/Windows.h"), at("sdk/include/um/windows.h"));
        // A directory is the same question, because `#include <CodeAnalysis/sourceannotations.h>`
        // is how Microsoft's own documentation spells it and a program that spells it in lower case
        // is as correct on Windows as that one is.
        assert_eq!(lowercase("crt/include/CodeAnalysis"), at("crt/include/codeanalysis"));
        assert_eq!(lowercase("sdk/include/um/winbase.h"), None);
    }

    #[test]
    fn the_tree_spells_architectures_the_way_the_tool_that_produced_this_shape_does() {
        assert_eq!(in_tree(Chip::X86), "x86");
        assert_eq!(in_tree(Chip::X64), "x86_64");
        assert_eq!(in_tree(Chip::Arm), "aarch");
        assert_eq!(in_tree(Chip::Arm64), "aarch64");
    }
}
