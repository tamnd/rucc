//! The header search path, as section 8.5 states it.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.5.
//!
//! # The failure this prevents
//!
//! Host contamination. A cross build picks up a header from the machine it is running on, produces
//! something that works there, and does not work anywhere else. It is a quiet failure: the build
//! succeeds, the tests pass on the build machine, and the binary is wrong somewhere the person who
//! made it will not look.
//!
//! `spec/cross-compile/02-the-goal.md` claim 5 is the test that catches it, byte identical output
//! from two different hosts, and it catches it only because the rule below makes step 3 a function
//! of the target when the target is not the host. That is why [`Options::host_include`] exists as a
//! separate field rather than as a default: there is exactly one place a host directory can enter,
//! it is guarded by one condition, and both are in [`include_paths`] where they can be read.
//!
//! # The rule
//!
//! 1. `-I` in the order given.
//! 2. The compiler's own headers. Always present, on every target including freestanding, and never
//!    taken from a sysroot, because `stddef.h` describes the compiler and not the C library.
//! 3. The target's libc headers, from `--sysroot` if given, otherwise from our bundled tree for
//!    that tuple, otherwise, and only when the target is the host, from the host's directories.
//! 4. Nothing else. No `/usr/local/include` in a cross build, ever.
//!
//! `-nostdinc` removes 3, `-nobuiltininc` removes 2, `--sysroot` replaces 3's root, and `-isysroot`
//! is the Darwin spelling of the same thing.

use std::path::{Path, PathBuf};

use rucc_tuple::TargetTuple;

use crate::layout::Sysroot;

/// Which of section 8.5's four steps put a directory in the list.
///
/// Carried rather than discarded because `-print-search-dirs` has to say it, because a user
/// debugging a wrong header needs to know which rule chose it, and because the test that no host
/// directory appears in a cross build is written against this rather than against path spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Step 1. A `-I` the user gave, in the position they gave it.
    User,
    /// Step 2. The compiler's own headers, which describe the compiler rather than the platform.
    Compiler,
    /// Step 3, taken from a `--sysroot` or `-isysroot` the user named.
    Sysroot,
    /// Step 3, taken from the tree we bundle for this target.
    Bundled,
    /// Step 3, taken from the host, which is legal only when the target is the host.
    Host,
}

impl Origin {
    /// Whether a directory from this origin belongs to the machine the compiler is running on.
    ///
    /// The property the cross compilation test asserts: for a target that is not the host, no entry
    /// in the search path answers true.
    #[must_use]
    pub const fn is_host(self) -> bool {
        matches!(self, Origin::Host)
    }

    /// A short word for `-print-search-dirs` and for a diagnostic that has to say where a header
    /// came from.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::User => "-I",
            Origin::Compiler => "compiler",
            Origin::Sysroot => "sysroot",
            Origin::Bundled => "bundled",
            Origin::Host => "host",
        }
    }
}

/// One directory in the search path, and the reason it is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The directory.
    pub path: PathBuf,
    /// Which step of section 8.5 put it there.
    pub origin: Origin,
}

/// What the driver knows that the rule needs.
///
/// A struct rather than seven arguments, because six of the seven are empty in the common case and
/// a function with six defaulted parameters is a function somebody calls wrong.
#[derive(Debug, Clone, Default)]
pub struct Options<'a> {
    /// `-I`, in the order given. Order is preserved exactly, because a user who put one `-I` before
    /// another meant it.
    pub user: &'a [PathBuf],
    /// The compiler's own header directory, which is where `stddef.h` and the intrinsic headers
    /// live. Supplied by the caller rather than found here, because finding it means asking the
    /// host where the compiler is installed and that is not this crate's business.
    pub resources: Option<&'a Path>,
    /// `--sysroot` or `-isysroot`, as the directories under the tree the user named.
    ///
    /// Paths rather than a [`Sysroot`], because a tree somebody else assembled has whatever shape
    /// they gave it. A buildroot or Yocto or distribution tree keeps its headers under
    /// `usr/include` and not under the two directories [`Sysroot::includes`] names, so the caller
    /// computes the list and this replaces step 3 with it wholesale. A user who did lay their tree
    /// out the way we lay one out passes [`Sysroot::includes`] and gets the same thing.
    pub sysroot: &'a [PathBuf],
    /// The tree we bundle for this target, when there is one.
    pub bundled: Option<&'a Sysroot>,
    /// The host's own include directories, as the driver computes them today.
    ///
    /// Used only when the target is the host. On any other target this field is ignored, and that
    /// is the whole of the cross compilation guarantee in this file.
    pub host_include: &'a [PathBuf],
    /// `-nostdinc`. Removes step 3.
    pub no_std_inc: bool,
    /// `-nobuiltininc`. Removes step 2.
    pub no_builtin_inc: bool,
}

/// The directories to search for an included file, in order.
///
/// `host` is what the compiler is running on, and it is an argument rather than something read from
/// the environment so that the rule can be tested for a host it is not running on. Passing [`None`]
/// says the host is unknown, which is treated as not being the target: an unknown host cannot be
/// proved to be the target, and guessing yes is the contamination this function is written against.
#[must_use]
pub fn include_paths(
    target: TargetTuple,
    host: Option<TargetTuple>,
    options: &Options<'_>,
) -> Vec<Entry> {
    let mut paths = Vec::new();

    // Step 1. Exactly what the user said, in the order they said it.
    for path in options.user {
        paths.push(Entry { path: path.clone(), origin: Origin::User });
    }

    // Step 2. The compiler's own headers, on every target including freestanding. They are not in
    // the sysroot and they never come from one: `stddef.h` describes what this compiler does with
    // `size_t`, and a copy of it belonging to some other compiler is a different `size_t`.
    if !options.no_builtin_inc {
        if let Some(resources) = options.resources {
            paths.push(Entry { path: resources.join("include"), origin: Origin::Compiler });
        }
    }

    // Step 3. The target's libc headers, from the first of three sources that has them.
    if !options.no_std_inc {
        if !options.sysroot.is_empty() {
            for path in options.sysroot {
                paths.push(Entry { path: path.clone(), origin: Origin::Sysroot });
            }
        } else if let Some(bundled) = options.bundled {
            for path in bundled.includes() {
                paths.push(Entry { path, origin: Origin::Bundled });
            }
        } else if host == Some(target) {
            // The only place a host directory enters, and it is guarded by the target being the
            // host. Everything about claim 5 rests on this one condition.
            for path in options.host_include {
                paths.push(Entry { path: path.clone(), origin: Origin::Host });
            }
        }
    }

    // Step 4 is that there is no step 4. No `/usr/local/include`, no `/usr/include` appended
    // because the list came out short, and nothing derived from an environment variable.
    paths
}
