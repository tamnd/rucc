//! The target table: which targets exist, what is true of each one today, and what this
//! specification is committed to making true of it.
//!
//! This is `spec/cross-compile/04-target-matrix.md` section 4.3 as data. It is the plan rather than the
//! report: section 4.7 is explicit that a tier is computed from corpus results by the reporting
//! job and never asserted by a human, so the `tier` column here is what the last reporting run
//! established and the `planned` column is the commitment. Raising a `tier` in this file
//! without a corpus run behind it is the one edit that makes the whole table worthless.
//!
//! The spec writes `*-none` as a single row covering every architecture. It is expanded here,
//! and the expansion caps each freestanding row at the tier its architecture reaches when
//! hosted, because a freestanding target and a hosted one share a back end and the freestanding
//! one cannot be better tested than the code generator underneath it.

use core::fmt;
use core::str::FromStr;

use crate::{Error, TargetTuple};

/// What is known to be true about a target.
///
/// The ordering is the useful one: `Tier::Supported` is the smallest, so `min` picks the better
/// of two tiers and a sorted list starts with the best supported targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Rungs 0, 1 and 2 pass on hardware at every optimization level, the ABI differential is
    /// green against GCC, code quality is measured and within bound, and every hardening flag
    /// works.
    Supported,
    /// Rungs 0 and 1 pass, on hardware or under qemu-user, the ABI differential is green, a
    /// sysroot ships or fetches, and code quality is measured and published whatever it says.
    SupportedWithEvidence,
    /// Objects are emitted and a linked program runs the smoke test. No rung passes yet.
    InProgress,
    /// The tuple parses and `--print-config` is correct. Nothing is emitted.
    ///
    /// This tier exists so that a user asking for a target we have no back end for is told
    /// exactly that, with an issue number, rather than being told the compiler has never heard
    /// of their machine. Those two messages lead a user to opposite conclusions.
    Recognized,
}

impl Tier {
    /// The number, which is how the spec and every issue refer to these.
    pub const fn number(self) -> u8 {
        match self {
            Tier::Supported => 1,
            Tier::SupportedWithEvidence => 2,
            Tier::InProgress => 3,
            Tier::Recognized => 4,
        }
    }

    /// The words the README uses for this tier, which are deliberately weaker than the tier
    /// number as the tier gets worse.
    pub const fn describe(self) -> &'static str {
        match self {
            Tier::Supported => "supported",
            Tier::SupportedWithEvidence => "supported, with the evidence column saying how",
            Tier::InProgress => "in progress, named in the table, not claimed",
            Tier::Recognized => "recognized",
        }
    }

    /// Whether a program for this target can be compiled and linked today.
    ///
    /// True for tiers 1, 2 and 3. `spec/cross-compile/04-target-matrix.md` measures claim 1 against the count
    /// of rows for which this holds.
    pub const fn compiles_and_links(self) -> bool {
        !matches!(self, Tier::Recognized)
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}

/// Whether rucc itself runs on this target, which is a separate and much cheaper question from
/// whether rucc compiles for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Host {
    /// A release artifact is built for it and CI runs on it.
    Yes,
    /// Planned, but not yet a runner.
    Planned,
    /// Not a host, and not intended to be one.
    No,
}

impl Host {
    /// The word the table prints.
    pub const fn as_str(self) -> &'static str {
        match self {
            Host::Yes => "yes",
            Host::Planned => "later",
            Host::No => "no",
        }
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row of the target table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetEntry {
    /// The canonical spelling. Every entry parses back to itself, which the tests check,
    /// because a table whose keys are not canonical is a cache with two entries per target.
    pub tuple: &'static str,
    /// What the last reporting run established.
    pub tier: Tier,
    /// What the plan in `spec/cross-compile/15-plan.md` commits to.
    pub planned: Tier,
    /// Whether rucc runs on this target.
    pub host: Host,
    /// Why this row is in the table, or what makes it awkward. Taken from the spec.
    pub note: &'static str,
}

impl TargetEntry {
    /// Parse the row's tuple.
    ///
    /// # Errors
    ///
    /// Only if the table itself is wrong, which the tests are there to prevent.
    pub fn parse(&self) -> Result<TargetTuple, Error> {
        TargetTuple::from_str(self.tuple)
    }
}

/// Every target the compiler has an opinion about.
///
/// Adding a row here is the entry price for `spec/cross-compile/02-the-goal.md` claim 3: bringing up a target
/// should be a data change, and this is the data. A row that needs code outside the target crate
/// is a row that has found a leak in the abstraction, and the leak is the bug rather than the
/// row.
pub const TARGETS: &[TargetEntry] = &[
    // Linux.
    TargetEntry {
        tuple: "x86_64-linux-gnu",
        tier: Tier::SupportedWithEvidence,
        planned: Tier::Supported,
        host: Host::Yes,
        note: "the reference target, and everything is measured here first",
    },
    TargetEntry {
        tuple: "x86_64-linux-musl",
        tier: Tier::Recognized,
        planned: Tier::Supported,
        host: Host::Yes,
        note: "the easiest sysroot in existence, and the first cross target",
    },
    TargetEntry {
        tuple: "aarch64-linux-gnu",
        tier: Tier::Recognized,
        planned: Tier::Supported,
        host: Host::Yes,
        note: "char is unsigned here and signed on x86-64, which is the classic bug",
    },
    TargetEntry {
        tuple: "aarch64-linux-musl",
        tier: Tier::Recognized,
        planned: Tier::Supported,
        host: Host::Yes,
        note: "the pair that proves the libc and the architecture are separate axes",
    },
    TargetEntry {
        tuple: "riscv64-linux-gnu",
        tier: Tier::Recognized,
        planned: Tier::Supported,
        host: Host::Planned,
        note: "the middle end canary, with no condition codes and no complex addressing",
    },
    TargetEntry {
        tuple: "riscv64-linux-musl",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "",
    },
    TargetEntry {
        tuple: "i686-linux-gnu",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "x87 long double is the type here rather than a corner case",
    },
    TargetEntry {
        tuple: "armv7-linux-gnueabihf",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "soft, softfp and hard is an ABI, so it is in the tuple",
    },
    TargetEntry {
        tuple: "armv7-linux-musleabihf",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "",
    },
    TargetEntry {
        tuple: "loongarch64-linux-gnu",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "LP64D only, and r21 is reserved by the psABI and used by Linux as a percpu base",
    },
    TargetEntry {
        tuple: "powerpc64le-linux-gnu",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "ELFv2, with the TOC and the split between local and global entry points",
    },
    TargetEntry {
        tuple: "s390x-linux-gnu",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "the big endian row, and the only one with a commercial user base",
    },
    TargetEntry {
        tuple: "aarch64-linux-android",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "bionic, API level stub sets, and the packed relocation trap below API 35",
    },
    TargetEntry {
        tuple: "x86_64-linux-android",
        tier: Tier::Recognized,
        planned: Tier::InProgress,
        host: Host::No,
        note: "the emulator target, which is cheap once aarch64 works",
    },
    TargetEntry {
        tuple: "riscv64-linux-android",
        tier: Tier::Recognized,
        planned: Tier::Recognized,
        host: Host::No,
        note: "requires an RVA23 baseline, so it is recognized only",
    },
    TargetEntry {
        tuple: "x86_64-linux-gnux32",
        tier: Tier::Recognized,
        planned: Tier::Recognized,
        host: Host::No,
        note: "ILP32 on a 64-bit ISA, and it exists to prove the data model field is real",
    },
    // Darwin.
    TargetEntry {
        tuple: "aarch64-macos",
        tier: Tier::Recognized,
        planned: Tier::Supported,
        host: Host::Yes,
        note: "the four AAPCS64 divergences of the compiler repository's document 12.3",
    },
    TargetEntry {
        tuple: "x86_64-macos",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::Yes,
        note: "still shipping, and Rosetta makes it how an aarch64 host tests x86-64",
    },
    TargetEntry {
        tuple: "aarch64-ios",
        tier: Tier::Recognized,
        planned: Tier::InProgress,
        host: Host::No,
        note: "a distinct OS from macOS, with its own platform value and availability macros",
    },
    TargetEntry {
        tuple: "aarch64-ios-simulator",
        tier: Tier::Recognized,
        planned: Tier::Recognized,
        host: Host::No,
        note: "three table rows rather than work, once the Darwin path exists",
    },
    TargetEntry {
        tuple: "aarch64-ios-macabi",
        tier: Tier::Recognized,
        planned: Tier::Recognized,
        host: Host::No,
        note: "Mac Catalyst, which zig added in 0.16 and which costs a platform value",
    },
    // Windows.
    TargetEntry {
        tuple: "x86_64-windows-gnu",
        tier: Tier::Recognized,
        planned: Tier::Supported,
        host: Host::Yes,
        note: "mingw-w64, and the default env for Windows because it is the one we can ship",
    },
    TargetEntry {
        tuple: "x86_64-windows-msvc",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::Yes,
        note: "needs a fetched SDK, so the ABI is ours and the dialect is not",
    },
    TargetEntry {
        tuple: "aarch64-windows-gnu",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::Yes,
        note: "",
    },
    TargetEntry {
        tuple: "aarch64-windows-msvc",
        tier: Tier::Recognized,
        planned: Tier::InProgress,
        host: Host::Yes,
        note: "",
    },
    TargetEntry {
        tuple: "arm64ec-windows-msvc",
        tier: Tier::Recognized,
        planned: Tier::Recognized,
        host: Host::No,
        note: "a separate symbol namespace, thunks and hybrid images, declined in document 06.8",
    },
    TargetEntry {
        tuple: "i686-windows-gnu",
        tier: Tier::Recognized,
        planned: Tier::InProgress,
        host: Host::No,
        note: "stdcall, fastcall and thiscall decoration, the only place C has mangling",
    },
    // BSD, illumos, freestanding and wasm.
    TargetEntry {
        tuple: "x86_64-freebsd",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::Planned,
        note: "stub libraries, as zig does, and FreeBSD 14 or later",
    },
    TargetEntry {
        tuple: "aarch64-freebsd",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::Planned,
        note: "",
    },
    TargetEntry {
        tuple: "x86_64-netbsd",
        tier: Tier::Recognized,
        planned: Tier::InProgress,
        host: Host::No,
        note: "NetBSD 10.1 or later",
    },
    TargetEntry {
        tuple: "aarch64-netbsd",
        tier: Tier::Recognized,
        planned: Tier::InProgress,
        host: Host::No,
        note: "",
    },
    TargetEntry {
        tuple: "x86_64-openbsd",
        tier: Tier::Recognized,
        planned: Tier::InProgress,
        host: Host::No,
        note: "dynamic libc only, and zig took a whole release to add it",
    },
    TargetEntry {
        tuple: "x86_64-illumos",
        tier: Tier::Recognized,
        planned: Tier::Recognized,
        host: Host::No,
        note: "open, therefore admissible, therefore not excluded on principle",
    },
    TargetEntry {
        tuple: "x86_64-none",
        tier: Tier::InProgress,
        planned: Tier::Supported,
        host: Host::No,
        note: "the kernel target, which rung 4 needs and needs at tier 1",
    },
    TargetEntry {
        tuple: "aarch64-none",
        tier: Tier::InProgress,
        planned: Tier::Supported,
        host: Host::No,
        note: "",
    },
    TargetEntry {
        tuple: "riscv64-none",
        tier: Tier::InProgress,
        planned: Tier::Supported,
        host: Host::No,
        note: "",
    },
    TargetEntry {
        tuple: "i686-none",
        tier: Tier::InProgress,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "capped at its architecture's tier, per the note at the top of this module",
    },
    TargetEntry {
        tuple: "armv7-none-eabi",
        tier: Tier::InProgress,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "soft float, because a bare metal core may have no FPU",
    },
    TargetEntry {
        tuple: "armv7m-none-eabi",
        tier: Tier::InProgress,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "Thumb only, and the smallest target in the table",
    },
    TargetEntry {
        tuple: "wasm32-wasip1",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "one linear memory, no signals, and structured control flow",
    },
    TargetEntry {
        tuple: "wasm32-wasip3",
        tier: Tier::Recognized,
        planned: Tier::InProgress,
        host: Host::No,
        note: "a different ABI from p1 and p2, with the stack pointer and TLS base in a context",
    },
    TargetEntry {
        tuple: "wasm32-none",
        tier: Tier::Recognized,
        planned: Tier::SupportedWithEvidence,
        host: Host::No,
        note: "freestanding wasm, which is the second back end without the libc question",
    },
];

/// Find the table row for a target, by canonical spelling.
///
/// Takes a parsed tuple rather than a string so that `x86_64-unknown-linux-gnu` and
/// `x86_64-linux-gnu` find the same row. That is the reason canonicalization exists.
pub fn lookup(target: &TargetTuple) -> Option<&'static TargetEntry> {
    let canonical = target.to_canonical_string();
    TARGETS.iter().find(|entry| entry.tuple == canonical)
}

/// How many rows are at each tier today, and how many the plan commits to.
///
/// Returned rather than written down, because `spec/cross-compile/04-target-matrix.md` section 4.7 forbids a
/// count that a human maintains next to a table that changes.
pub fn counts_by_planned_tier() -> [usize; 4] {
    let mut counts = [0usize; 4];
    for entry in TARGETS {
        counts[usize::from(entry.planned.number()) - 1] += 1;
    }
    counts
}

/// How many rows the plan commits to compiling and linking, which is the number claim 1 is
/// measured against.
pub fn planned_working_count() -> usize {
    TARGETS.iter().filter(|entry| entry.planned.compiles_and_links()).count()
}
