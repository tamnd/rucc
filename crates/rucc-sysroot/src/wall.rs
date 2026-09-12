//! The two targets whose system headers are somebody else's to license.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4 and
//! `spec/cross-compile/08-sysroots.md` section 8.6.
//!
//! Almost everything a target needs from us is ours to ship. glibc's headers are LGPL, musl's are
//! MIT, mingw-w64's are permissive, the kernel's come with the system call note that says a program
//! using the interface is not covered by the GPL, and the rest of section 8.2's table is under a BSD
//! licence. Two rows are not. Apple's SDK is under the Xcode licence, which limits its use to Apple
//! branded hardware, and Microsoft's Windows SDK and universal CRT are not redistributable at all.
//!
//! So for those two there is no tree we may bundle, no artifact a release may pin, and nothing to
//! download on somebody's behalf, and that is a different situation from a tree that has not been
//! built yet. A compiler that said "there is no sysroot for this target" would send a person looking
//! for a command that will never exist, so the answer names the licence and the lawful ways to get
//! what is behind it.
//!
//! # Why this is an enum and not a flag
//!
//! Because the two walls bound different things for the person who hit one. A Windows program has a
//! fully redistributable alternative, which is mingw-w64, and a build for that environment needs
//! nothing installed at all, which is why the default Windows environment for a cross build is `gnu`.
//! A macOS program has no alternative: the two lawful ways to get the SDK are to compile on a mac,
//! where the installed one is found by asking `xcrun`, or to download Xcode yourself under its
//! licence, and both of them end at a path that somebody has to name.
//!
//! # What a wall is not
//!
//! It is not a statement about the back end. We emit Mach-O and we emit COFF, and section 8.6 calls
//! that targeting a platform rather than compiling for it. What is missing in both cases is headers
//! and import libraries, which is why this sits beside the search path rather than anywhere near the
//! code generator.

use std::fmt;

use rucc_tuple::{Env, Os, TargetTuple};

/// A target whose system headers and link inputs are not ours to distribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wall {
    /// Apple's, which is macOS and iOS.
    Apple,
    /// Microsoft's, which is a Windows target in the MSVC environment and not a mingw-w64 one.
    Microsoft,
}

impl Wall {
    /// The wall this target is behind, or [`None`] for the rest of the table.
    ///
    /// The environment decides the Windows answer and the operating system decides the Apple one,
    /// because the two Windows environments are two different sets of link inputs under two
    /// different licences while every Apple platform reads one SDK.
    #[must_use]
    pub const fn of(target: TargetTuple) -> Option<Self> {
        match (target.os(), target.env()) {
            (Os::MacOs | Os::IOs, _) => Some(Wall::Apple),
            (Os::Windows, Env::Msvc) => Some(Wall::Microsoft),
            _ => None,
        }
    }

    /// What is behind the wall, named the way the platform's own documentation names it.
    #[must_use]
    pub const fn sdk(self) -> &'static str {
        match self {
            Wall::Apple => "a macOS SDK",
            Wall::Microsoft => "the Windows SDK and its universal CRT",
        }
    }

    /// The licence that puts it there, as a clause a sentence can be built around.
    #[must_use]
    pub const fn licence(self) -> &'static str {
        match self {
            Wall::Apple => {
                "it is under Apple's Xcode licence, which limits its use to Apple branded hardware"
            }
            Wall::Microsoft => "Microsoft does not allow it to be redistributed",
        }
    }

    /// The licence as an identifier rather than as a clause, which is what a record carries.
    ///
    /// Two spellings of one fact, and they are both here because a sentence and a field want
    /// different things. [`Wall::licence`] is what a person is told and reads as English.
    /// This is what [`crate::distribution`] writes into a column, where a clause would be
    /// unreadable and a second vocabulary of licence names would be a thing to keep in step with
    /// [`crate::Licence`].
    #[must_use]
    pub const fn under(self) -> crate::Licence {
        match self {
            Wall::Apple => crate::Licence::AppleSdk,
            Wall::Microsoft => crate::Licence::MicrosoftSdk,
        }
    }

    /// The lawful ways to get what is behind the wall, which every message here ends with.
    ///
    /// A flag in each, because section 8.6's third rule is that the fetch is never automatic and
    /// never silent, so every one of these paths is a person naming a path once.
    #[must_use]
    pub const fn ways(self) -> &'static str {
        match self {
            Wall::Apple => {
                "Compile on a macOS machine, where the installed SDK is found by asking xcrun, or \
                 download Xcode yourself under that licence and name the SDK with -isysroot <dir> \
                 or in SDKROOT"
            }
            Wall::Microsoft => {
                "Build for the mingw-w64 environment instead, which is fully redistributable and \
                 needs nothing installed, or install the SDK yourself and name it with \
                 --sysroot=<dir>, or on Windows run the vcvarsall.bat that puts it in INCLUDE"
            }
        }
    }

    /// Why a compile for `target` has no library headers to read.
    ///
    /// The target is spelled by the caller rather than taken from the tuple, so that the message
    /// says the target the way the command line said it.
    #[must_use]
    pub fn no_headers(self, target: &str) -> String {
        format!(
            "{target} needs {} to compile against and none of it is on this machine. This compiler \
             does not ship it and will not download it for you, because {}. {}, or pass -nostdinc \
             for a program that includes none of the library",
            self.sdk(),
            self.licence(),
            self.ways()
        )
    }

    /// Why a fetch cannot get the sysroot for `target`, which no release of this compiler will pin.
    #[must_use]
    pub fn no_fetch(self, target: &str) -> String {
        format!(
            "there is nothing to fetch for {target} and there never will be. What a program for it \
             compiles against is {}, and {}, so no release of this compiler pins it. {}",
            self.sdk(),
            self.licence(),
            self.ways()
        )
    }
}

impl fmt::Display for Wall {
    /// Whose wall it is, for a message that has already said what is behind it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Wall::Apple => "Apple's licence wall",
            Wall::Microsoft => "Microsoft's licence wall",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(tuple: &str) -> TargetTuple {
        tuple.parse().expect("a target this understands")
    }

    #[test]
    fn the_apple_platforms_are_behind_one_wall_and_the_msvc_environment_behind_the_other() {
        assert_eq!(Wall::of(target("aarch64-macos")), Some(Wall::Apple));
        assert_eq!(Wall::of(target("x86_64-macos")), Some(Wall::Apple));
        assert_eq!(Wall::of(target("aarch64-ios")), Some(Wall::Apple));
        assert_eq!(Wall::of(target("x86_64-windows-msvc")), Some(Wall::Microsoft));
        assert_eq!(Wall::of(target("arm64ec-windows-msvc")), Some(Wall::Microsoft));
    }

    #[test]
    fn the_windows_target_that_needs_nothing_installed_is_not_behind_a_wall() {
        // The whole reason the default Windows environment for a cross build is `gnu`. mingw-w64's
        // headers and import libraries are ours to ship, and the target next to it is not.
        assert_eq!(Wall::of(target("x86_64-pc-windows-gnu")), None);
        assert_eq!(Wall::of(target("aarch64-windows-gnu")), None);
        for tuple in ["x86_64-linux-gnu", "riscv64-linux-musl", "armv7m-none-eabi", "wasm32-wasi"] {
            assert_eq!(Wall::of(target(tuple)), None, "{tuple}");
        }
    }

    #[test]
    fn the_licence_behind_each_wall_is_the_one_that_is_never_redistributable() {
        for wall in [Wall::Apple, Wall::Microsoft] {
            assert!(!wall.under().redistributable(), "{wall}");
        }
        assert_eq!(Wall::Apple.under().as_str(), "apple-sdk");
        assert_eq!(Wall::Microsoft.under().as_str(), "microsoft-sdk");
    }

    #[test]
    fn each_message_names_the_target_the_licence_and_a_way_out() {
        let said = Wall::Apple.no_headers("aarch64-macos");
        assert!(said.contains("aarch64-macos"), "{said}");
        assert!(said.contains("Xcode licence"), "{said}");
        assert!(said.contains("-isysroot"), "{said}");
        // And the escape hatch for a program that reads no library headers at all, which is what
        // section 8.6 means by being able to target the platform without the SDK.
        assert!(said.contains("-nostdinc"), "{said}");

        let said = Wall::Microsoft.no_fetch("x86_64-windows-msvc");
        assert!(said.contains("x86_64-windows-msvc"), "{said}");
        assert!(said.contains("mingw-w64"), "{said}");
        // The part that tells this apart from a target whose artifact has not been published yet.
        assert!(said.contains("there never will be"), "{said}");
    }
}
