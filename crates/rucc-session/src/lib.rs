//! The `Session`: the options, the interner and the diagnostic sink that every stage of a
//! single compilation is handed.
//!
//! Design: `spec/03-architecture.md` and `spec/04-driver-and-cli.md`. Layer rank 4, see
//! `spec/18-package-layout.md`.
//!
//! Everything below the driver reaches the outside world through this type and not through
//! `std::fs`, `std::env` or `println!`. That is the whole reason the compiler can be used as
//! a library and tested without spawning a process, and it is enforced by the layer rule
//! rather than by discipline.
//!
//! # Status
//!
//! Options, optimisation levels, emit kinds, diagnostic counting, the source map every span
//! is resolved against, the file system the compiler reads through, the include search path
//! and the headers the compiler itself ships are real. The parallel job model is still a
//! placeholder.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-session/0.10.37")]

mod fs;
pub mod runtime;

pub use crate::fs::{Dir, FileSystem, Found, IncludeForm, MemoryFileSystem, SearchPath, path_key};

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use rucc_base::Interner;
use rucc_diag::{Diagnostic, Severity, SourceMap};
use rucc_target::{TargetInfo, Triple};

/// An optimisation level.
///
/// `spec/16-performance.md` section 16.4 gives each level a throughput budget and a code
/// quality budget, and the levels exist to make that tradeoff explicit rather than to be a
/// dial. There is no `-O4`, because a level nobody can state the contract for is a level
/// nobody can test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum OptLevel {
    /// `-O0`. Compile as fast as possible and keep every variable inspectable.
    #[default]
    O0,
    /// `-O1`. The cheap wins, at roughly the cost of `-O0`.
    O1,
    /// `-O2`. The full pipeline. This is the level the code quality claim is about.
    O2,
    /// `-O3`. `-O2` plus the transformations that trade size for speed.
    O3,
    /// `-Os`. Optimise for size, at roughly `-O2` compile time.
    Os,
    /// `-Oz`. Optimise for size, aggressively.
    Oz,
}

impl OptLevel {
    /// The flag that selects this level.
    pub const fn as_flag(self) -> &'static str {
        match self {
            OptLevel::O0 => "-O0",
            OptLevel::O1 => "-O1",
            OptLevel::O2 => "-O2",
            OptLevel::O3 => "-O3",
            OptLevel::Os => "-Os",
            OptLevel::Oz => "-Oz",
        }
    }

    /// Whether this level optimises for size rather than speed.
    pub const fn is_size(self) -> bool {
        matches!(self, OptLevel::Os | OptLevel::Oz)
    }

    /// Whether the middle end runs at all.
    pub const fn runs_optimizer(self) -> bool {
        !matches!(self, OptLevel::O0)
    }
}

impl fmt::Display for OptLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_flag())
    }
}

impl FromStr for OptLevel {
    type Err = ();

    /// Parses the part after `-O`, so `""` is `-O` which GCC treats as `-O1`.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "0" => OptLevel::O0,
            "" | "1" => OptLevel::O1,
            "2" => OptLevel::O2,
            // GCC accepts `-O4` and above and treats them as `-O3`. Build systems in the
            // wild do pass them, so matching that is cheaper than being right.
            "3" | "4" | "5" | "6" | "7" | "8" | "9" => OptLevel::O3,
            "s" => OptLevel::Os,
            "z" => OptLevel::Oz,
            _ => return Err(()),
        })
    }
}

/// How much of the memory safety monitor is on, from `-fsafety=`.
///
/// Design: `spec/safe-memory/15-integration.md` section 15.4. One flag rather than a plane at a
/// time, because the tiers of `spec/safe-memory/02-threat-model.md` are the product and the
/// modifiers are how somebody who has read that document departs from one.
///
/// The tiers agree about which accesses are checked and disagree about what happens when a check
/// says no and about how much of the boundary is covered. That is why they are one value here and
/// not three booleans: a build asks for a tier, and everything else follows from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Safety {
    /// `-fsafety=off`. No checks and no runtime. The default, and what every existing build gets.
    #[default]
    Off,
    /// `-fsafety=detect`. Tier D: report and carry on, for a test run or a fuzzer.
    Detect,
    /// `-fsafety=enforce`. Tier E: report and stop, for a program that faces the network.
    Enforce,
    /// `-fsafety=kernel`. Tier K: what a kernel can afford, with the allocator and the libc
    /// wrappers taken out because a kernel has neither.
    Kernel,
}

impl Safety {
    /// The spelling this tier is asked for by, without the flag in front of it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Safety::Off => "off",
            Safety::Detect => "detect",
            Safety::Enforce => "enforce",
            Safety::Kernel => "kernel",
        }
    }

    /// Whether checks are inserted at all.
    ///
    /// The three tiers that are not `off` all insert the same checks at this milestone. What
    /// separates them is the reporter and the boundary, which are milestones S2 and S3 in
    /// `spec/safe-memory/16-milestones.md`.
    pub const fn instruments(self) -> bool {
        !matches!(self, Safety::Off)
    }
}

impl fmt::Display for Safety {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Safety {
    type Err = ();

    /// Parses the part after `-fsafety=`.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "off" => Safety::Off,
            "detect" => Safety::Detect,
            "enforce" => Safety::Enforce,
            "kernel" => Safety::Kernel,
            _ => return Err(()),
        })
    }
}

/// Whether padding participates in the init plane, from `-fsafety-init=`.
///
/// Design: `spec/safe-memory/09-type-init-and-races.md` section 9.3.
///
/// The correct rule is that a store which writes an object as a whole initializes it as a whole,
/// padding included, and that a fill done a member at a time leaves the padding alone. That rule
/// reports a structure filled member by member and then hashed, compared or written to a file,
/// and it is right to: that is CWE-200 and it is the kernel infoleak KMSAN was built to find.
///
/// It is also every third program in a userspace corpus, where the bytes never leave the process
/// and nobody is hunting an infoleak. So section 9.3 makes it a flag and splits the default:
/// padding participates for the kernel profile, where the leak is the thing being looked for, and
/// does not for library code, where it would be a torrent of reports about programs nobody is
/// worried about. Document 12's scoreboard reports the two configurations separately for the same
/// reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Padding {
    /// `-fsafety-init=nopadding`. A store through a member says the padding after it holds
    /// something too, so a record filled a member at a time comes out entirely written.
    #[default]
    Ignored,
    /// `-fsafety-init=padding`. A store through a member says only what it wrote, which is
    /// section 9.3's rule and is what makes the infoleak visible.
    Tracked,
}

impl Padding {
    /// The spelling this is asked for by, without the flag in front of it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Padding::Ignored => "nopadding",
            Padding::Tracked => "padding",
        }
    }
}

impl fmt::Display for Padding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Padding {
    type Err = ();

    /// Parses the part after `-fsafety-init=`.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "nopadding" => Padding::Ignored,
            "padding" => Padding::Tracked,
            _ => return Err(()),
        })
    }
}

/// Whether an access has to stay inside the member it names, from `-fsafety-subobject`.
///
/// Design: `spec/safe-memory/09-type-init-and-races.md` section 9.4, which is row S4 of document
/// 03 and is the class Fil-C, CHERI by default and ARM MTE all miss. Their metadata is per
/// allocation and a member is not an allocation, so an overflow from one member of a structure
/// into the next is invisible to all three. The type plane is byte granular, so it is not
/// invisible here.
///
/// A flag rather than a default because of what a store means. C 6.5 says a store to allocated
/// storage sets that storage's effective type, so a write that leaves one member and lands in the
/// next is, read literally, a program retyping bytes it owns. Every buffer that gets reused for a
/// second kind of value does the same thing on purpose. So the question a store asks is only asked
/// when somebody has said they want it asked, and what they get in return is the write half of
/// S4 that nothing else catches.
///
/// The read half is not behind this and never was: a read that disagrees with the plane is
/// judgement J1 at every tier, because reading bytes back through a type they were not stored
/// through is undefined however the pointer got there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Subobject {
    /// No `-fsafety-subobject`. A store records what it wrote and is asked nothing.
    #[default]
    Off,
    /// `-fsafety-subobject`. A store asks the plane whether the bytes it is about to write agree
    /// with the type it writes them through, which catches an overflow out of a member into a
    /// member of a different type.
    ///
    /// Two adjacent members of the same type are indistinguishable to this, which section 9.4
    /// states plainly: `struct { int a; int b; }` overflowing from `a` into `b` writes `int` over
    /// `int` and there is nothing for the plane to disagree with. That is what
    /// `-fsafety-subobject=strict` is for and it is not here yet.
    Members,
}

impl Subobject {
    /// The spelling this is asked for by, without the flag in front of it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Subobject::Off => "off",
            Subobject::Members => "members",
        }
    }

    /// Whether a store asks the type plane anything.
    pub const fn asks(self) -> bool {
        matches!(self, Subobject::Members)
    }
}

impl fmt::Display for Subobject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether pointer races are watched, from `-fsafety-races=`.
///
/// Design: `spec/safe-memory/09-type-init-and-races.md` section 9.5, which is document 03's C1
/// through C4 and is judgement J9 of document 04 section 4.4. A thread counts its own metadata
/// stores, a store through a pointer shaped slot leaves that count in the epoch plane, and an
/// access that finds a count from another thread which nothing it has done orders is a race that
/// really happened in the interleaving that really ran.
///
/// A flag rather than a default, and the reason is not cost. It is that this is the one plane in
/// the compiler where instrumentation nobody wrote costs a false report instead of a missed one.
/// Every ordering the monitor has was carried by a synchronization edge somebody interposed, so two
/// threads that an edge nobody saw really did join look exactly like two threads nothing joined.
/// The edges that are calls are interposed already, and the ordering that is not a call at all is
/// emitted by this pass beside the checks: an atomic that publishes gets a `meta_release` in front
/// of it and one that takes gets a `meta_acquire` after it. A bare `atomic_thread_fence` gets the
/// same pair with no key, since it orders against every thread rather than against an object and so
/// has no address an edge could be keyed on, and the runtime holds one clock for every fence in the
/// program rather than a table.
///
/// Which is also why the default stays [`Races::Off`] after the flag works. Turning it on is a
/// decision about a program, not about a build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Races {
    /// `-fsafety-races=off`. Nothing records into the epoch plane and nothing asks it anything.
    #[default]
    Off,
    /// `-fsafety-races=metadata`. The classes that produce a wrong pointer rather than a wrong
    /// number, which section 9.5 lists as C1, C3 and C4, and which Tier E carries.
    Metadata,
    /// `-fsafety-races=pointer`. The same, and C2 as well, which is a race on a pointer word
    /// reported in its own right rather than only used to decide one of the other three.
    Pointer,
}

impl Races {
    /// The spelling this is asked for by, without the flag in front of it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Races::Off => "off",
            Races::Metadata => "metadata",
            Races::Pointer => "pointer",
        }
    }

    /// Whether a store through a pointer shaped slot records which thread made it, and asks first
    /// whether another thread got there with nothing in between.
    ///
    /// Both of the modes that are not off. Every class section 9.5 lists is decided by comparing
    /// against a stamp a store left behind, so both of them record, and the question a store puts
    /// is C3, the metadata race, which both of them report.
    pub const fn records(self) -> bool {
        !matches!(self, Races::Off)
    }

    /// Whether a load of a pointer asks the same question, which is where the two modes differ.
    ///
    /// C2 of section 9.5, the general pointer word race, which the section lists apart from the
    /// other three because it is the class reported in its own right rather than used to decide one
    /// of them. Tier E carries `metadata` and not this, so a build that wants every race a load can
    /// see has to ask for it by name.
    pub const fn reads(self) -> bool {
        matches!(self, Races::Pointer)
    }
}

impl fmt::Display for Races {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Races {
    type Err = ();

    /// Parses the part after `-fsafety-races=`.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "off" => Races::Off,
            "metadata" => Races::Metadata,
            "pointer" => Races::Pointer,
            _ => return Err(()),
        })
    }
}

/// Whether the `restrict` contract is checked, from `-fsafety-restrict`.
///
/// Design: `spec/safe-memory/09-type-init-and-races.md` section 9.6, which is row Y8 of document
/// 03 and is judgement J8. C 6.7.3.1 says that if an object reachable through a `restrict` pointer
/// declared in a block is modified anywhere in that block, every access to that object in that
/// block goes through that pointer. Nothing about one access decides it, which is why document 04
/// section 4.6 keeps it out of J1.
///
/// A flag rather than a default for two reasons, and neither of them is the one
/// [`Subobject`] has. The first is cost, and it is a bad distribution rather than a large number:
/// an access inside a block that declares `restrict` pointers pays a scan of that block's record,
/// and blocks that declare them are the numeric kernels and the `mem` functions, which is exactly
/// where the hot loops are. Code with no `restrict` in it pays nothing at all. The second is that
/// the record is the union of what each pointer reached, so two pointers striding through one array
/// without ever landing on the same byte are reported, and by the letter of the standard those are
/// different objects and that is not a violation.
///
/// The second one is not an imprecision to apologise for. This check exists because a violated
/// `restrict` is a miscompilation, and what the optimizer acts on is that the ranges are disjoint,
/// so a program the union rule reports is a program the optimizer is entitled to break. It is
/// still a report about a program the standard permits, which is a decision that belongs to the
/// build rather than to this compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Promise {
    /// No `-fsafety-restrict`. An access says which `restrict` pointer it went through, because
    /// the alias analysis reads that, and nothing asks whether two of them met.
    #[default]
    Off,
    /// `-fsafety-restrict`. Every block that declares `restrict` pointers keeps a record of what
    /// each of them reached, and every access through one asks whether another got there first.
    Blocks,
}

impl Promise {
    /// The spelling this is asked for by, without the flag in front of it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Promise::Off => "off",
            Promise::Blocks => "blocks",
        }
    }

    /// Whether a block keeps a record and an access asks about it.
    pub const fn checks(self) -> bool {
        matches!(self, Promise::Blocks)
    }
}

impl fmt::Display for Promise {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How far a name reaches outside a shared library when nothing in the source said.
///
/// `-fvisibility=`, which is written on every cmake project that cares about its exports and is
/// the way a library ships a small documented interface instead of every name it happens to
/// define. The attribute in the source wins wherever one was written, which is what makes the
/// flag a default rather than an override and what lets `-fvisibility=hidden` be put on a whole
/// tree and the dozen exported names marked one at a time.
///
/// Three answers to four spellings. `internal` is `hidden` plus a promise about never taking the
/// address across a component boundary, and nothing here derives anything from that promise, so
/// what it gets is the same symbol with a weaker claim on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Visibility {
    /// `-fvisibility=default`. Exported and interposable, which is what a name gets when the flag
    /// is not written at all and what gcc does by default too.
    #[default]
    Default,
    /// `-fvisibility=hidden` and `-fvisibility=internal`. Not in the dynamic symbol table.
    Hidden,
    /// `-fvisibility=protected`. In the dynamic symbol table, and a reference from inside the
    /// library binds to the definition inside it.
    Protected,
}

impl Visibility {
    /// The spelling this is asked for by, without the flag in front of it.
    ///
    /// One spelling each, so `internal` is not here: it is a way of asking for `hidden` rather
    /// than an answer of its own.
    pub const fn as_str(self) -> &'static str {
        match self {
            Visibility::Default => "default",
            Visibility::Hidden => "hidden",
            Visibility::Protected => "protected",
        }
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Visibility {
    type Err = ();

    /// Parses the part after `-fvisibility=`.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "default" => Visibility::Default,
            "hidden" | "internal" => Visibility::Hidden,
            "protected" => Visibility::Protected,
            _ => return Err(()),
        })
    }
}

/// How the debug sections are compressed, which is what `-gz` asks.
///
/// Debug information is much larger than the code it describes and almost never read, so an ELF
/// section holding it may be stored compressed: the section keeps its name, gains the
/// `SHF_COMPRESSED` flag and starts with a header saying what it decompresses to, and every reader
/// that understands the flag unpacks it on the way in. A distribution that ships debug symbols for
/// everything it builds saves more from this than from anything else it passes.
///
/// This compiler writes no debug sections at all yet, so every answer here produces the same bytes,
/// and an object built with `-gz=zstd` is identical to one built without the flag. It is recorded
/// rather than dropped for the reason section 4.1 gives for the rest of the family: the answer has
/// to be sitting in the options on the day `rucc-debug` has something to compress, and a build that
/// asked for it and got silence would have no way of noticing the difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Compress {
    /// `-gz=none`, and what a command line that says nothing gets. gcc's default is the same.
    #[default]
    None,
    /// `-gz` and `-gz=zlib`. The ELF way, with the `SHF_COMPRESSED` flag and an `Elf64_Chdr` in
    /// front of the data. Bare `-gz` means this one, which is worth knowing because the manual
    /// describes the flag without saying so.
    Zlib,
    /// `-gz=zlib-gnu`. The older way, where the section is renamed from `.debug_info` to
    /// `.zdebug_info` and carries `ZLIB` and a length instead of a real header. Kept because
    /// binutils still reads it and some build systems still ask for it by name.
    ZlibGnu,
    /// `-gz=zstd`. The same arrangement as `Zlib` with a different algorithm in the header, which
    /// packs debug information smaller and unpacks it faster.
    Zstd,
}

impl Compress {
    /// The spelling this is asked for by, without the `-gz=` in front of it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Compress::None => "none",
            Compress::Zlib => "zlib",
            Compress::ZlibGnu => "zlib-gnu",
            Compress::Zstd => "zstd",
        }
    }
}

impl fmt::Display for Compress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Compress {
    type Err = ();

    /// Parses the part after `-gz=`. Bare `-gz` is not this function's business because there is
    /// nothing after the flag to hand it.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "none" => Compress::None,
            "zlib" => Compress::Zlib,
            "zlib-gnu" => Compress::ZlibGnu,
            "zstd" => Compress::Zstd,
            _ => return Err(()),
        })
    }
}

/// How many processes the link time work is spread over, which is what `-flto=` takes.
///
/// Named for the flag rather than for what it counts, because `Jobs` in the driver is already the
/// answer to how many files are compiled at once and the two numbers are not the same number.
///
/// The link time half of link time optimization is where all of the time goes, because it is the
/// half that has the whole program in front of it, and gcc's answer is to cut the program into
/// pieces and generate code for the pieces at once. This says how many at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum LtoJobs {
    /// Bare `-flto`, and `-flto=1`. One process, which is what gcc does when the flag is written
    /// without a number after it.
    #[default]
    One,
    /// `-flto=auto`. As many as the machine has, worked out when the link runs.
    Auto,
    /// `-flto=jobserver`. As many as `make` is willing to hand out, asked for through the
    /// jobserver pipe it puts in the environment, which is the only answer that does not fight
    /// with the rest of a parallel build for the same cores.
    Jobserver,
    /// `-flto=<n>`. Exactly that many. gcc refuses a zero, so this is never one.
    Count(u32),
}

impl FromStr for LtoJobs {
    type Err = ();

    /// Parses the part after `-flto=`. A number has to be positive, which is gcc's rule: `-flto=0`
    /// is refused rather than read as `-fno-lto`.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "auto" => LtoJobs::Auto,
            "jobserver" => LtoJobs::Jobserver,
            _ => match s.parse::<u32>() {
                Ok(1) => LtoJobs::One,
                Ok(n) if n > 1 => LtoJobs::Count(n),
                _ => return Err(()),
            },
        })
    }
}

impl fmt::Display for LtoJobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LtoJobs::One => f.write_str("1"),
            LtoJobs::Auto => f.write_str("auto"),
            LtoJobs::Jobserver => f.write_str("jobserver"),
            LtoJobs::Count(n) => write!(f, "{n}"),
        }
    }
}

/// How the program is cut up before the link time work is spread over it, from `-flto-partition=`.
///
/// A partition is a set of functions that are generated together, and where the cuts fall decides
/// both how well the work spreads and how much is visible from inside one piece. The names are
/// gcc's and so are the shapes: one piece per input file, pieces balanced by size, one piece for
/// the whole program, a piece per function, or no partitioning at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Partition {
    /// `-flto-partition=balanced`, and what gcc does when nothing asks. Pieces of roughly equal
    /// size, which is the answer that spreads the work best and is why it is the default.
    #[default]
    Balanced,
    /// `-flto-partition=1to1`. One piece per input file, which keeps the generated code in the
    /// same order the inputs were in and is what a build comparing two outputs wants.
    OneToOne,
    /// `-flto-partition=one`. The whole program in one piece, which is the most the optimizer can
    /// see at once and the least the work can be spread over.
    One,
    /// `-flto-partition=max`. A piece per function, which is the other end of the same trade.
    Max,
    /// `-flto-partition=none`. No partitioning, and no streaming back out to be generated in
    /// pieces either.
    None,
}

impl Partition {
    /// The spelling this is asked for by, without the `-flto-partition=` in front of it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Partition::Balanced => "balanced",
            Partition::OneToOne => "1to1",
            Partition::One => "one",
            Partition::Max => "max",
            Partition::None => "none",
        }
    }
}

impl fmt::Display for Partition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Partition {
    type Err = ();

    /// Parses the part after `-flto-partition=`.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "balanced" => Partition::Balanced,
            "1to1" => Partition::OneToOne,
            "one" => Partition::One,
            "max" => Partition::Max,
            "none" => Partition::None,
            _ => return Err(()),
        })
    }
}

/// What the `-flto` family asked for, which is a whole optimization this compiler does not do yet.
///
/// Link time optimization is the optimizer run once over the whole program instead of once per
/// translation unit, which is the only way an inliner ever sees across a file boundary and is
/// where most of what is left on the table after `-O2` is. `spec/09-optimizer.md` says how it will
/// work here: the IR goes into a section of the object, the driver finds those sections at link
/// time, merges them into one module and generates code with everything visible.
///
/// None of that exists, so the whole family is read, checked and recorded rather than acted on.
/// That is a different answer from the one `-gsplit-dwarf` gets in the same specification, and the
/// difference is what ignoring each of them does. Ignoring `-gsplit-dwarf` means a file a build
/// asked for never appears. Ignoring this means a program that is correct and slower than it could
/// have been, which is what section 4.1 means by a hint about speed, and which is also what every
/// compilation at `-O0` already is.
///
/// The other half of the argument is about the object. gcc's `-flto` object holds the bytecode and
/// no machine code at all, so it is only useful to a link that knows about it; the objects here
/// always hold the code, which is what `-ffat-lto-objects` asks gcc for. So a build that passes
/// `-flto` to this compiler gets objects that are strictly more usable than the ones it would have
/// got, rather than different ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Lto {
    /// Whether the last of `-flto` and `-fno-lto` on the command line was the first of the two.
    pub requested: bool,
    /// How many processes to spread the link time work over.
    pub jobs: LtoJobs,
    /// How the program is cut up before the work is spread.
    pub partition: Partition,
    /// How hard to compress the IR on its way into the object, from `-flto-compression-level=`,
    /// where `None` means whatever the compressor does when nobody says. Between 0 and 19, which
    /// is zstd's range and is the range gcc checks against.
    pub compression: Option<u8>,
}

/// What the profile reading half of the `-fprofile` family asked for.
///
/// A profile is a count per edge, gathered by running a build of the program that was instrumented
/// to count, and read back on a second compilation so that the optimizer knows which way each
/// branch actually went. It is worth more than any single optimization, because almost everything
/// the optimizer decides is a guess about a frequency that the counts simply state.
///
/// Nothing here reads one yet, so this is recorded rather than acted on, and the family splits in
/// two rather than being taken or refused as a whole. The half recorded here is the half that only
/// costs speed when it is ignored: a build that asks to read a profile and is not read one gets the
/// program it would have got anyway, which is what section 4.1 means by a hint about speed. The
/// other half writes files, and that half is refused by the driver rather than landing here, on the
/// same reading `-gsplit-dwarf` gets: a program instrumented by `-fprofile-generate` writes a
/// `.gcda` when it runs and `-ftest-coverage` writes a `.gcno` beside the object, and ignoring
/// either means a build waits for a file that never arrives and then quietly optimizes against no
/// counts at all.
///
/// gcc's own measurement is the argument for the split. `-fprofile-use` on a file with no counts
/// beside it produces an object byte for byte identical to the one no flag produces, and warns; the
/// same file under `-fprofile-generate` grows from 71 bytes of code to 375 with 296 bytes of
/// counters beside it. So one half of the family is already a no-op in gcc when there is nothing to
/// read, and the other half is never one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profile {
    /// Whether the last of `-fprofile-use` and `-fno-profile-use` on the command line was the
    /// first of the two.
    pub requested: bool,
    /// Where to read the counts from, from `-fprofile-use=<path>`, where `None` means beside the
    /// object the way gcc looks when nobody says. A directory or a file, which is gcc's rule and
    /// is not something this can tell apart without looking at the filesystem.
    pub path: Option<String>,
    /// Where the whole family's files live, from `-fprofile-dir=`. Separate from `path` because
    /// gcc keeps them separate: this one moves the counts for the generating half as well.
    pub dir: Option<String>,
    /// Whether the path recorded in those files is made absolute, from `-fprofile-abs-path`. It is
    /// what a build with several object directories under one source tree needs so that two files
    /// of the same name do not land on one set of counts.
    pub absolute: bool,
    /// Whether counts that do not add up are repaired rather than refused, from
    /// `-fprofile-correction`. A program that forked or was killed while it ran leaves counts that
    /// no single execution could have produced, and this says to make the best of them.
    pub correction: bool,
    /// Whether the parts of the program the training run never reached are optimized as if they
    /// were cold rather than as if nothing were known about them, from `-fprofile-partial-training`.
    pub partial_training: bool,
}

/// Which functions get a stack protector, which is what the `-fstack-protector` family asks.
///
/// A canary is a word the prologue copies into the frame above everything a local can be written
/// through, and the epilogue compares it against the copy the runtime still holds before it
/// returns. A write that runs off the end of a local and keeps going passes the canary on its way
/// to the return address, so a function that returns with the word changed calls
/// `__stack_chk_fail` instead of returning at all.
///
/// Which functions are worth the slot and the comparison is what the three levels disagree about,
/// and the middle one is the one that matters: every distribution has built its packages with
/// `-fstack-protector-strong` for a decade, so a compiler that cannot take the flag cannot be the
/// `CC` of a package build whatever else it can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Protector {
    /// `-fno-stack-protector`, and what a command line that says nothing gets. gcc's own default
    /// is the same, and it is the distributions rather than the compiler that turn it on.
    #[default]
    None,
    /// `-fstack-protector`. A function with a local array of at least eight bytes, or one whose
    /// stack grows while it runs.
    Buffers,
    /// `-fstack-protector-strong`. Any of those, and any function with a local array at all, a
    /// local holding one, or a local whose address is taken.
    Strong,
    /// `-fstack-protector-all`. Every function that has a frame.
    All,
}

/// What overflows rather than being undefined, from `-fwrapv` and its relatives.
///
/// C says a signed addition that overflows and a pointer that walks off the end of the object it
/// points into are both undefined, and an optimizer that believes it reads a great deal into every
/// loop: that a counter going up one at a time never turns round, that an index widened to an
/// address may be widened before the arithmetic rather than after, that a bound is reached. These
/// flags withdraw exactly that. They do not make the program mean something else, they make it mean
/// less, and the code that asks for them is code that overflows on purpose and wants the answer the
/// machine gives rather than the answer the standard declines to give.
///
/// Two of them because gcc has two, and a build that wants one usually wants the other. Signed
/// arithmetic and pointer arithmetic are separate assumptions and a kernel turns both off.
///
/// `-ftrapv` is the third answer to the first question and is here for that reason. Undefined,
/// wrapping and stopping are the three things a signed overflow can be, and a command line picks
/// one of them: the last of `-fwrapv` and `-ftrapv` wins, which is gcc's behaviour and what makes
/// them one field rather than two that can both be set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wrapping {
    /// Whether signed arithmetic wraps, from `-fwrapv`.
    pub signed: bool,
    /// Whether pointer arithmetic wraps, from `-fwrapv-pointer`.
    pub pointer: bool,
    /// Whether a signed overflow stops the program instead, from `-ftrapv`.
    ///
    /// Never set at the same time as [`Wrapping::signed`], since a program cannot both wrap and
    /// stop, and the driver is what keeps that true by clearing each when the other is asked for.
    pub trap: bool,
}

impl Wrapping {
    /// Both of them, which is what `-fno-strict-overflow` asks for.
    ///
    /// gcc says so itself: its help text for `-fstrict-overflow` reads "negated as `-fwrapv`
    /// `-fwrapv-pointer`", so the older flag is a name for the pair rather than a third knob. And
    /// asking for wrapping is asking for not stopping, so this is the whole answer and not two
    /// thirds of one.
    pub const ALL: Self = Self { signed: true, pointer: true, trap: false };

    /// Neither, which is the default and what a command line that says nothing about any of this
    /// gets.
    pub const NONE: Self = Self { signed: false, pointer: false, trap: false };
}

/// A list of `old=new` rewrites to apply to a path before it is written into the output, which is
/// what the `-f*-prefix-map=` family asks for.
///
/// The point of them is a build whose output does not depend on where it was built. A path is the
/// last thing in an object that a second machine cannot reproduce: two people who check out the
/// same commit and run the same compiler get the same instructions and different `__FILE__`
/// strings, and a distribution that wants to prove its binaries came from its sources has to make
/// that difference go away. So the build says what its root is called, and every path that would
/// name the real one names that instead.
///
/// The rule is a plain string prefix and nothing more, which is worth saying because it looks like
/// it ought to be about directories. gcc compares the characters, so `s=B` turns `sub/h.h` into
/// `Bub/h.h`, and an empty `old` matches everything and puts `new` in front of it. The path
/// compared against is the one the search found, so a header reached through a relative `-I` is
/// mapped as a relative path and the same header reached through an absolute one is mapped as an
/// absolute path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrefixMap {
    /// The rewrites, in the order the command line gave them.
    entries: Vec<(String, String)>,
}

impl PrefixMap {
    /// No rewrites, which is what a command line that says nothing about this gets.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing was asked for, which is the case worth not spending anything on.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds a rewrite, which is what one flag on the command line is.
    pub fn push(&mut self, old: impl Into<String>, new: impl Into<String>) {
        self.entries.push((old.into(), new.into()));
    }

    /// The two halves of one flag's argument, split at the last `=` rather than the first.
    ///
    /// That is where gcc splits it, and it is the answer that makes a path containing an `=`
    /// mappable: `-ffile-prefix-map=/home/a=b=/src` maps the directory `/home/a=b`. The cost is
    /// that a replacement cannot contain one, which is the rarer thing to want. `None` when there
    /// is no `=` at all, which gcc refuses rather than reading as a mapping to nothing.
    #[must_use]
    pub fn split(arg: &str) -> Option<(&str, &str)> {
        arg.rsplit_once('=')
    }

    /// `path` with the last rewrite that matches it applied, or `path` where none does.
    ///
    /// The last rather than the first, because that is gcc's answer and because it is the one a
    /// build relies on: a mapping set for the whole project and a narrower one set for one
    /// directory is a command line where the second is meant to win.
    #[must_use]
    pub fn apply<'a>(&self, path: &'a str) -> Cow<'a, str> {
        for (old, new) in self.entries.iter().rev() {
            if let Some(rest) = path.strip_prefix(old.as_str()) {
                return Cow::Owned(format!("{new}{rest}"));
            }
        }
        Cow::Borrowed(path)
    }
}

/// The three answers to the question the `-f*-prefix-map=` family asks, which is one question
/// asked about three kinds of output.
///
/// They are separate because gcc's flags are separate and a build uses that: a distribution maps
/// its debug paths to something a debugger can find the sources under and leaves `__FILE__` alone,
/// or maps `__FILE__` so that an assertion message does not name a build directory and leaves the
/// debug info pointing at the real tree. `-ffile-prefix-map=` is the shorthand for all three and is
/// what a build that simply wants to be reproducible writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrefixMaps {
    /// What `__FILE__` and `__BASE_FILE__` are rewritten by, from `-fmacro-prefix-map=`.
    ///
    /// The only one of the three this compiler acts on today, because it is the only one whose
    /// output exists: `__FILE__` is a string literal in the binary and an assertion message a user
    /// reads.
    pub macros: PrefixMap,
    /// What a path in the debug info is rewritten by, from `-fdebug-prefix-map=`.
    ///
    /// Nothing reads this yet, because no debug info is generated yet. It is kept rather than
    /// dropped so that the crate that generates it has the answer waiting rather than a flag to
    /// go and add, and `crates/rucc-debug` says so where the work will start.
    pub debug: PrefixMap,
    /// What a path in the profile data is rewritten by, from `-fprofile-prefix-map=`.
    ///
    /// Nothing reads this yet either, and for the same reason: there is no profile data.
    pub profile: PrefixMap,
}

/// How far a multiply and an addition may be fused into one rounding, from `-ffp-contract=`.
///
/// A fused multiply add computes `a * b + c` with one rounding instead of two, which is both
/// faster and closer to the exact answer, and is therefore a different answer. C lets an
/// implementation do it within one expression and lets a program turn it off with the
/// `FP_CONTRACT` pragma, gcc does it across a whole function by default, and code that cares about
/// reproducing a result bit for bit turns it off everywhere.
///
/// This is the command line's answer to that question, and it is carried into the IR as an
/// attribute on each function so that the code generator still has it by the time it would matter.
/// It is a separate question from the flag on one instruction: a licence granted to an expression
/// the optimizer has since taken apart is a licence about operations that no longer sit together,
/// and only the function level answer survives that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Contract {
    /// `-ffp-contract=off`. Never, so every rounding the source asked for happens.
    ///
    /// The default here, which is not gcc's. gcc defaults to `fast` under its own dialects and to
    /// `off` under a strict `-std=`, and the reason the default is this one anyway is that nothing
    /// in this compiler fuses anything: the two settings are the same program today, and of the two
    /// this is the one that does not write a licence nobody reads onto every function in the file.
    /// The day the code generator learns to fuse, the default moves to gcc's, and that is a change
    /// to the code generator rather than to this flag.
    #[default]
    Off,
    /// `-ffp-contract=on`. Within one expression, which is what C allows an implementation to do
    /// without being asked.
    On,
    /// `-ffp-contract=fast`. Anywhere in the function, across statements and across whatever the
    /// optimizer has rearranged, which is what gcc does under its own dialects.
    Fast,
}

impl Contract {
    /// The spelling after the `=`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Contract::Off => "off",
            Contract::On => "on",
            Contract::Fast => "fast",
        }
    }
}

impl fmt::Display for Contract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Contract {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "off" => Contract::Off,
            "on" => Contract::On,
            "fast" => Contract::Fast,
            _ => return Err(()),
        })
    }
}

impl Protector {
    /// The spelling this is asked for by, which is the whole flag rather than a part of one,
    /// because these are four flags and not one flag with an argument.
    pub const fn as_str(self) -> &'static str {
        match self {
            Protector::None => "-fno-stack-protector",
            Protector::Buffers => "-fstack-protector",
            Protector::Strong => "-fstack-protector-strong",
            Protector::All => "-fstack-protector-all",
        }
    }
}

impl fmt::Display for Protector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which control flow transfers are checked, which is what `-fcf-protection=` asks.
///
/// Two mechanisms and one flag, because the hardware turns them on together and a program built
/// for one and not the other is a program with a hole in whichever half was left out. The forward
/// edge is an indirect call or jump, and it is checked by a landing pad at every address one is
/// allowed to arrive at, so a corrupted function pointer reaches somewhere somebody meant rather
/// than any byte of the program. The backward edge is a return, and it is checked against a second
/// copy of the return address the program cannot write to, which needs no instructions at all: the
/// machine keeps the copy and the loader turns it on.
///
/// Which is why the marker matters as much as the code. An object says in a note which halves it
/// was built for, the linker takes the intersection over every input, and the loader turns on what
/// survives. One object built without the note is enough to turn the whole program's protection
/// off, so the note goes in even for a mode that changes no instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Control {
    /// `-fcf-protection=none` and `-fno-cf-protection`, and what a command line that says nothing
    /// gets. gcc's own default is the same on the targets this compiler has a back end for.
    #[default]
    None,
    /// `-fcf-protection=branch`. The forward edge alone: a landing pad at every function, and a
    /// note that asks for the check on indirect transfers and not on returns.
    Branch,
    /// `-fcf-protection=return`. The backward edge alone, which is the note and nothing else,
    /// since the copy of the return address is the machine's own and no instruction maintains it.
    Return,
    /// `-fcf-protection=full`, and what the bare `-fcf-protection` means. Both halves.
    Full,
    /// `-fcf-protection=check`. Asks that the compilation be checked for compatibility with the
    /// mode rather than built in it, so nothing is instrumented and no note is written, which is
    /// exactly what gcc emits for it.
    Check,
}

impl Control {
    /// Whether a landing pad goes at the top of every function.
    #[must_use]
    pub const fn branch(self) -> bool {
        matches!(self, Control::Branch | Control::Full)
    }

    /// Whether returns are asked to be checked against the machine's own copy.
    #[must_use]
    pub const fn ret(self) -> bool {
        matches!(self, Control::Return | Control::Full)
    }

    /// Whether anything at all is asked for, which is what decides whether the file says what it
    /// was built for.
    ///
    /// False for the two modes that build nothing. [`Control::None`] asks for nothing and
    /// [`Control::Check`] asks that the compilation be looked at rather than changed, and gcc
    /// writes no note for either.
    #[must_use]
    pub const fn any(self) -> bool {
        self.branch() || self.ret()
    }

    /// What the argument was spelled as, which is the part after the equals sign.
    pub const fn as_str(self) -> &'static str {
        match self {
            Control::None => "none",
            Control::Branch => "branch",
            Control::Return => "return",
            Control::Full => "full",
            Control::Check => "check",
        }
    }
}

impl fmt::Display for Control {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Control {
    type Err = ();

    /// Parses the part after `-fcf-protection=`.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "none" => Control::None,
            "branch" => Control::Branch,
            "return" => Control::Return,
            "full" => Control::Full,
            "check" => Control::Check,
            _ => return Err(()),
        })
    }
}

/// Where the call `-pg` puts at the top of every function goes, which `-mfentry` chooses.
///
/// Two conventions for one job, and the difference is what the hook can see when it runs. See
/// [`rucc_target::Trace`] for what each of them is and why a kernel needs the earlier one.
///
/// A third answer, because a command line that named neither has not asked a question: the
/// platform's own answer is the one it gets, and that is a fact about the target rather than about
/// the flags, so it is settled where the target is known and not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Hook {
    /// Whichever the platform puts first, which is what a command line that said neither gets.
    #[default]
    Platform,
    /// `-mfentry`. In front of the prologue, so the return address is the top thing on the stack
    /// and the arguments are still where the call left them.
    Early,
    /// `-mno-fentry`. Once the frame is taken, so the hook can walk back through the frame pointer,
    /// which is why a function that has this one is given a frame pointer whatever else was said.
    Late,
}

impl Hook {
    /// That answer as it is written on a command line, which is what `--print-config` reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Hook::Platform => "platform",
            Hook::Early => "fentry",
            Hook::Late => "mcount",
        }
    }

    /// Whether the call goes in front of the prologue, given what the platform puts first.
    #[must_use]
    pub const fn early(self, fentry: bool) -> bool {
        match self {
            Hook::Platform => fentry,
            Hook::Early => true,
            Hook::Late => false,
        }
    }
}

impl fmt::Display for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How much room at the top of every function is reserved for somebody to write over later, which
/// `-fpatchable-function-entry=` asks for.
///
/// Room rather than instructions. What goes there is a run of the shortest instruction the machine
/// has that does nothing, and the point of them is that they are never executed for long: a tracer
/// or a live patcher overwrites them with a jump or a call once the program is running, and what it
/// needs from the compiler is a known address, a known number of bytes, and a promise that nothing
/// in the function jumps into the middle of them.
///
/// Two numbers because the room can be on either side of the function's own label, and the two
/// sides are not the same thing. Room after the label is room inside the function, which is what a
/// patcher that redirects a call into the function wants. Room in front of the label is outside it,
/// so what goes there is reached only by something that already knows the address, and a patcher
/// that wants somewhere to put a whole instruction it can reach from the first one needs it.
///
/// The address recorded for the function is the start of the room, which is the front of the part
/// before the label when there is one and the front of the part after it when there is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Patchable {
    /// How many bytes in total, which is the first number and the one a command line must give.
    pub total: u32,
    /// How many of them go in front of the function's own label, which is the second number and is
    /// zero on a command line that gave one number.
    pub before: u32,
}

impl Patchable {
    /// Whether any room at all was asked for, which is what decides whether a function gets a
    /// record.
    ///
    /// `=0` is a command line that asked for none, and gcc accepts it and writes nothing, so the
    /// question is about the number rather than about whether the flag was written.
    #[must_use]
    pub const fn any(self) -> bool {
        self.total > 0
    }

    /// How many bytes go after the function's own label, which is the rest of them.
    #[must_use]
    pub const fn after(self) -> u32 {
        self.total - self.before
    }
}

impl FromStr for Patchable {
    type Err = ();

    /// Parses the part after `-fpatchable-function-entry=`, which is a number or two of them.
    ///
    /// A second number larger than the first is refused rather than clamped, because it asks for
    /// more room in front of the label than there is room at all and there is no reading of that a
    /// caller meant. So is a third, and so is anything that is not a number, which is what gcc does
    /// with each of them.
    fn from_str(s: &str) -> Result<Self, ()> {
        let (total, before) = match s.split_once(',') {
            Some((total, before)) => (total, before),
            None => (s, "0"),
        };
        let total: u32 = total.parse().map_err(|_| ())?;
        let before: u32 = before.parse().map_err(|_| ())?;
        if before > total {
            return Err(());
        }
        Ok(Patchable { total, before })
    }
}

impl fmt::Display for Patchable {
    /// Written the way it was asked for, which is one number when the second is zero.
    ///
    /// Not because the two forms mean different things, they do not, but because that is the form
    /// a command line reaching for this feature writes and reading back what was written is what
    /// `--print-config` is for.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.before {
            0 => write!(f, "{}", self.total),
            before => write!(f, "{},{before}", self.total),
        }
    }
}

/// Which of the two position independent questions the output is answering.
///
/// Everything this compiler writes is position independent, so this is not about whether there are
/// absolute addresses in the text. It is about whether the link that reads the object is one that
/// puts every name in the same program. An executable is such a link and a shared library is not,
/// and the difference decides how a name is reached: from the instruction pointer where the
/// distance is a number the linker has, and out of the global offset table where it is not.
///
/// The expensive answer is the one that has to be asked for, which is gcc's arrangement and is why
/// `-fPIC` is on the compile line of every library and nowhere else. A name is only reached the
/// expensive way when it is one another object may define or replace, so `-fPIC -fvisibility=hidden`
/// costs no more than an executable does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Pic {
    /// `-fPIE`, `-fpie` and nothing at all. The link puts every name in one program, so a name this
    /// file defines is at a distance from the instruction asking, and a name it declares ends up at
    /// one too, because the linker answers a reference to a variable defined in a library by making
    /// room for it here and copying it. That is what a distribution's default build is.
    #[default]
    Executable,
    /// `-fPIC` and `-fpic`. The output may end up in a shared library, where a name the file
    /// exports is one something loaded earlier may define too, and where a name defined elsewhere
    /// is not copied in. Both are reached through the global offset table.
    Library,
}

impl Pic {
    /// The spelling this is asked for by, which is the one gcc's manual leads with.
    pub const fn as_str(self) -> &'static str {
        match self {
            Pic::Executable => "-fPIE",
            Pic::Library => "-fPIC",
        }
    }
}

impl fmt::Display for Pic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the compiler should produce.
///
/// The intermediate forms are not a debugging convenience bolted on later. Every one of them
/// is a documented textual form that round-trips, which is what makes the per-stage testing
/// in `spec/15-testing.md` section 15.2 possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
// Deliberately not `#[non_exhaustive]`. Adding a variant here has to break every
// match that needs to change, in this workspace and in anyone else's code. That is
// the property `spec/10-backend.md` section 10.8 is claiming when it says adding a
// target is a data change: the compiler tells you every place the data is read.
pub enum EmitKind {
    /// A linked executable. The default.
    #[default]
    Executable,
    /// An object file, `-c`.
    Object,
    /// A static library holding the objects of every input, `--emit=archive`.
    ///
    /// Not a GCC mode, because GCC has `ar` beside it and we have said we ship a toolchain rather
    /// than half of one. What needs it first is `cargo xtask builtins`, which has to turn a
    /// directory of C files into the `librucc_builtins.a` a cross link looks for, for a target
    /// whose machine may have no `ar` that knows the format.
    ///
    /// It is a mode of the compiler rather than a second program because of the symbol index. A
    /// static link resolves through it, so writing one means knowing what each member defines, and
    /// the compiler has just finished compiling them. An `ar` would have to read the objects back
    /// to find out the same thing.
    Archive,
    /// Assembly text, `-S`.
    Asm,
    /// Preprocessed source, `-E`.
    Preprocessed,
    /// The typed AST, `--emit=tast`.
    Tast,
    /// The IR, `--emit=ir`.
    Ir,
    /// The machine IR after register allocation, `--emit=mir-final`.
    MirFinal,
    /// The safety summary, `--emit=safety-summary`.
    ///
    /// Not an intermediate form of the program the way the three above are. It is the answer to
    /// "what does this build's guarantee actually rest on", which
    /// `spec/safe-memory/07-check-elimination.md` section 7.8 asks for and
    /// `spec/safe-memory/10-boundaries.md` section 10.2 says why.
    SafetySummary,
    /// How the bytes of the translation unit's records fall into granules,
    /// `--emit=type-granules`.
    ///
    /// Not an intermediate form either. It is the measurement
    /// `spec/safe-memory/17-open-questions.md` question 6 asks for, which decides whether the
    /// type plane fits inside Tier D's memory budget, and it needs nothing past the type
    /// checker because it is a question about layouts rather than about code.
    TypeGranules,
}

impl EmitKind {
    /// The name used by `--emit=` and by `--print-config`.
    pub const fn as_str(self) -> &'static str {
        match self {
            EmitKind::Executable => "exe",
            EmitKind::Object => "obj",
            EmitKind::Archive => "archive",
            EmitKind::Asm => "asm",
            EmitKind::Preprocessed => "preprocessed",
            EmitKind::Tast => "tast",
            EmitKind::Ir => "ir",
            EmitKind::MirFinal => "mir-final",
            EmitKind::SafetySummary => "safety-summary",
            EmitKind::TypeGranules => "type-granules",
        }
    }
}

impl FromStr for EmitKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "exe" => EmitKind::Executable,
            "obj" => EmitKind::Object,
            "archive" => EmitKind::Archive,
            "asm" => EmitKind::Asm,
            "preprocessed" => EmitKind::Preprocessed,
            "tast" => EmitKind::Tast,
            "ir" => EmitKind::Ir,
            "mir-final" => EmitKind::MirFinal,
            "safety-summary" => EmitKind::SafetySummary,
            "type-granules" => EmitKind::TypeGranules,
            _ => return Err(()),
        })
    }
}

/// Which C the source is written in.
///
/// The GNU variants are the same language with `__STRICT_ANSI__` left undefined, so the
/// dialect and the extension question are two fields rather than ten variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Std {
    /// `-std=c89`, and `-ansi`.
    C89,
    /// `-std=c99`.
    C99,
    /// `-std=c11`.
    C11,
    /// `-std=c17`, which is C11 with the defect reports applied.
    C17,
    /// `-std=c23`. The default, matching current GCC.
    #[default]
    C23,
}

impl Std {
    /// What `__STDC_VERSION__` says, which C89 does not define at all.
    pub const fn stdc_version(self) -> Option<&'static str> {
        match self {
            Std::C89 => None,
            Std::C99 => Some("199901L"),
            Std::C11 => Some("201112L"),
            Std::C17 => Some("201710L"),
            Std::C23 => Some("202311L"),
        }
    }

    /// The name in `-std=`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Std::C89 => "c89",
            Std::C99 => "c99",
            Std::C11 => "c11",
            Std::C17 => "c17",
            Std::C23 => "c23",
        }
    }

    /// Whether this dialect has `_Atomic`, `_Thread_local` and the rest of C11.
    pub const fn has_c11(self) -> bool {
        matches!(self, Std::C11 | Std::C17 | Std::C23)
    }

    /// Reads a `-std=` argument, and says whether the GNU extensions came with it.
    ///
    /// Every alias GCC takes is here, including the `iso9899` spellings and the year based
    /// ones, because a build system that passes `-std=iso9899:1999` is passing what its
    /// author tested against and rejecting it helps nobody. An unknown dialect is `None`
    /// rather than a guess, since guessing means compiling a different language than the one
    /// asked for.
    #[must_use]
    pub fn from_flag(name: &str) -> Option<(Std, bool)> {
        let gnu = name.starts_with("gnu");
        let std = match name {
            "c89" | "c90" | "gnu89" | "gnu90" | "iso9899:1990" | "iso9899:199409" => Std::C89,
            "c99" | "c9x" | "gnu99" | "gnu9x" | "iso9899:1999" | "iso9899:199x" => Std::C99,
            "c11" | "c1x" | "gnu11" | "gnu1x" | "iso9899:2011" => Std::C11,
            "c17" | "c18" | "gnu17" | "gnu18" | "iso9899:2017" | "iso9899:2018" => Std::C17,
            "c23" | "c2x" | "gnu23" | "gnu2x" => Std::C23,
            _ => return None,
        };
        Some((std, gnu))
    }
}

/// The GCC release the compiler claims to be, as `__GNUC__`, `__GNUC_MINOR__` and
/// `__GNUC_PATCHLEVEL__`.
///
/// Design: `spec/04-driver-and-cli.md` section 4.5, which makes this a knob rather than a
/// constant and says to start conservative and raise it as the matrix in `rucc-gnu` fills in.
///
/// The default is seven, which is the lowest claim that gets a modern glibc. glibc gates most
/// of what it hands a caller on `__GNUC_PREREQ`, so the claim decides which half of
/// `sys/cdefs.h` we get, and below seven `bits/floatn-common.h` writes `typedef float _Float32;`
/// over a keyword this compiler already has. Every header that reaches it stops there, which
/// was most of them: on Ubuntu 24.04's glibc 2.39 the claim of 4.2.1 that stood here before got
/// 180 of 214 headers through and seven gets 202, and the amalgamated sqlite goes from four
/// errors to none.
///
/// It is still deliberately low. Claiming a version whose promises have not been kept means
/// being handed syntax the compiler cannot parse, so this moves when there is a measurement
/// saying it can. Thirteen and sixteen were measured alongside seven and came out identical on
/// glibc, on the macOS SDK and on sqlite, so the next move up is cheap; it is a separate one
/// because nothing yet needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GnucVersion {
    /// `__GNUC__`.
    pub major: u32,
    /// `__GNUC_MINOR__`.
    pub minor: u32,
    /// `__GNUC_PATCHLEVEL__`.
    pub patch: u32,
}

impl Default for GnucVersion {
    fn default() -> GnucVersion {
        GnucVersion { major: 7, minor: 0, patch: 0 }
    }
}

impl FromStr for GnucVersion {
    type Err = String;

    /// Reads `-fgnuc-version=`, which is `15`, `15.1` or `15.1.0`.
    ///
    /// The short forms are not a convenience, they are what people write. A missing component
    /// is zero, the same way GCC treats a release with no patchlevel.
    fn from_str(text: &str) -> Result<GnucVersion, String> {
        let mut parts = text.split('.');
        let mut next = |what: &str| -> Result<u32, String> {
            match parts.next() {
                None => Ok(0),
                Some(field) => {
                    field.parse().map_err(|_| format!("`{text}` has a {what} that is not a number"))
                }
            }
        };
        let major = next("major")?;
        let minor = next("minor")?;
        let patch = next("patchlevel")?;
        if parts.next().is_some() {
            return Err(format!("`{text}` has more than three components"));
        }
        Ok(GnucVersion { major, minor, patch })
    }
}

/// What the `-d` family asks to be dumped alongside, or instead of, the preprocessed output.
///
/// Design: `spec/04-driver-and-cli.md` section 4.4.
///
/// GCC spells these as letters packed into one flag, so `-dDI` is two of them, and a letter it
/// does not know is ignored rather than rejected. That last part is deliberate on GCC's side
/// and worth copying: the family is a debugging aid and a build that passes `-dumpbase` should
/// not die on the `-d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dumps {
    /// `-dM`. Print the macros that are defined at the end, and nothing else.
    pub macros: bool,
}

impl Dumps {
    /// The letters GCC's preprocessor takes after `-d`.
    ///
    /// `M` is the macros, `D` is the macros in place, `N` is their names only, `I` is the
    /// `#include` lines and `U` is the macros as they are used. Only `M` does anything so far.
    const LETTERS: &'static str = "MDNIU";

    /// Whether `arg` is a flag from this family rather than something else beginning with
    /// `-d`.
    ///
    /// The check is here rather than in the driver so that the set of letters and the set of
    /// flags accepted cannot drift apart. It matters because `-dumpversion` also begins with
    /// `-d`, and a family that swallowed every such flag would turn a flag we have not written
    /// into a dump of nothing.
    #[must_use]
    pub fn is_family(arg: &str) -> bool {
        match arg.strip_prefix("-d") {
            Some("") | None => false,
            Some(letters) => letters.chars().all(|c| Dumps::LETTERS.contains(c)),
        }
    }

    /// Reads the letters after `-d`, ignoring the ones we do not implement yet.
    pub fn add(&mut self, letters: &str) {
        for letter in letters.chars() {
            if letter == 'M' {
                self.macros = true;
            }
        }
    }

    /// Whether anything at all was asked for.
    #[must_use]
    pub const fn any(self) -> bool {
        self.macros
    }
}

/// A file `-imacros` or `-include` named, read before the source file.
///
/// Design: `spec/04-driver-and-cli.md` section 4.4.
///
/// The flag a build reaches for when a whole tree has to see a definition that is not in any of
/// its files. The kernel builds every object with `-include` of its own configuration header, and
/// a configure script that has produced a `config.h` gets it into a third party source tree the
/// same way, without a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preinclude {
    /// The name as it was written, which is looked for the way a quoted include is looked for.
    pub name: String,
    /// Whether only the definitions it makes are wanted, which is what `-imacros` asks for.
    ///
    /// The text of an `-imacros` file is read and thrown away, so a header full of declarations
    /// contributes its macros and nothing else. That is what makes it usable on a file that has
    /// already been included by the source: the definitions arrive early and the declarations do
    /// not arrive twice.
    pub macros_only: bool,
}

/// What the `-M` family asks for, which is a make rule saying what a source file was built from.
///
/// Design: `spec/04-driver-and-cli.md` section 4.4.
///
/// This is a compiler flag rather than a separate tool because the answer is the set of files the
/// preprocessor opened, and nothing outside the preprocessor knows what that was. A build system
/// that generates its own makefiles asks for it on every compilation, which is why section 4.4
/// calls the family required rather than convenient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deps {
    /// Whether a rule is produced at all, which is any of `-M`, `-MM`, `-MD` and `-MMD`.
    pub emit: bool,
    /// Whether the rule is produced instead of compiling, which is `-M` and `-MM` and not the
    /// two that end in `D`.
    ///
    /// The split is GCC's and it is about who reads the answer. The two that stop after the rule
    /// write it to standard output for a person, and the two that do not write it to a file
    /// beside the object for `make` to include on the next run.
    pub instead_of_compiling: bool,
    /// Whether a header found in a system directory is listed, which `-MM` and `-MMD` turn off.
    ///
    /// A build that lists them is a build that rebuilds the world when the C library is updated,
    /// which is either what somebody wanted or the reason they reached for the other spelling.
    ///
    /// On unless a flag turned it off, and nothing turns it back on. That is GCC's behaviour and
    /// not an oversight: `-MM -M` leaves the system headers out, because the flag that asks for
    /// fewer of them is read as the answer to a question the other one never asked.
    pub system_headers: bool,
    /// Where the rule is written, from `-MF`, with `-` meaning standard output.
    ///
    /// `None` is the default, which is standard output when the rule replaces the compilation and
    /// the output file with a `.d` suffix when it does not.
    pub file: Option<String>,
    /// What the rule's targets are, from `-MT` and `-MQ`, in the order they were given.
    ///
    /// Already escaped, because that is the whole of the difference between the two flags: `-MQ`
    /// escapes what it is given and `-MT` writes it through untouched. Empty means the target is
    /// worked out from the output file, which is what a build that passes neither expects.
    pub targets: Vec<String>,
    /// Whether every prerequisite except the source gets a target of its own with no recipe,
    /// from `-MP`.
    ///
    /// This is what stops `make` failing outright when a header is deleted. Without it the old
    /// rule names a file that is gone and no rule makes it, and the build stops on a header that
    /// nothing needs any more.
    pub phony: bool,
}

impl Default for Deps {
    fn default() -> Deps {
        Deps {
            emit: false,
            instead_of_compiling: false,
            system_headers: true,
            file: None,
            targets: Vec::new(),
            phony: false,
        }
    }
}

/// Whether `-save-temps` was given and where it puts the files it keeps.
///
/// Design: `spec/04-driver-and-cli.md` section 4.10.
///
/// The flag is how a build gets at the preprocessed source of the file that failed without running
/// the compiler a second time under different flags, which is the one way to be sure the text being
/// read is the text that was compiled. A bug report against a compiler is usually a preprocessed
/// file and nothing else, and this is where that file comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SaveTemps {
    /// Not asked for, and nothing is kept.
    #[default]
    No,
    /// Beside the file the compilation produced, which is `-save-temps=obj`.
    ///
    /// This is what the bare `-save-temps` does as well. GCC's manual says the bare spelling is
    /// `-save-temps=cwd`, and gcc 16 does not do that: `-save-temps -c a.c -o out/a.o` leaves
    /// `out/a.i` and `out/a.s` rather than `a.i` and `a.s`. The measurement is what is followed
    /// here, because a build that reads the manual and a build that reads the compiler both end up
    /// looking for the files where the compiler put them.
    Object,
    /// In the working directory, which is `-save-temps=cwd`.
    Cwd,
}

impl SaveTemps {
    /// Whether anything is kept at all.
    #[must_use]
    pub const fn wanted(self) -> bool {
        !matches!(self, SaveTemps::No)
    }
}

impl FromStr for SaveTemps {
    type Err = String;

    /// Reads what came after the `=`, which is the only part that varies.
    ///
    /// # Errors
    ///
    /// Returns the offending word. GCC treats an unknown one as fatal rather than ignoring it,
    /// which is right: a misspelled keyword here means the files a person went looking for are not
    /// written and nothing said so.
    fn from_str(s: &str) -> Result<SaveTemps, String> {
        match s {
            "obj" => Ok(SaveTemps::Object),
            "cwd" => Ok(SaveTemps::Cwd),
            _ => Err(format!("`{s}` is not a -save-temps option; accepted: cwd, obj")),
        }
    }
}

/// Everything a compilation was asked to do.
///
/// Options are a plain value with no interior mutability, so a caller can build one, clone
/// it, tweak one field and run a second compilation, which is exactly what the differential
/// testing in `spec/15-testing.md` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// The target to generate code for.
    pub target: Triple,
    /// The optimisation level.
    pub opt_level: OptLevel,
    /// How much of the memory safety monitor is on, from `-fsafety=`.
    ///
    /// Off unless it was asked for. A program built without the flag is compiled by exactly the
    /// pipeline it was compiled by before the monitor existed, which is the only way the feature
    /// can be developed in the open without every build paying for it.
    pub safety: Safety,
    /// Whether padding participates in the init plane, from `-fsafety-init=`.
    ///
    /// Means nothing unless `safety` asked for a tier. The default is the one section 9.3 gives
    /// library code, which is that it does not, so a record filled a member at a time is not
    /// reported when something later reads it whole.
    pub padding: Padding,
    /// Whether an access has to stay inside the member it names, from `-fsafety-subobject`.
    ///
    /// Means nothing unless `safety` asked for a tier. Off by default, which section 9.4 argues
    /// for: this is the row most likely to fire on code that is doing what its author meant.
    pub subobject: Subobject,
    /// Whether the `restrict` contract is checked, from `-fsafety-restrict`.
    ///
    /// Means nothing unless `safety` asked for a tier. Off by default, which section 9.6 argues
    /// for: the cost lands entirely inside the loops `restrict` is written for.
    pub promise: Promise,
    /// Whether pointer races are watched, from `-fsafety-races=`.
    ///
    /// Means nothing unless `safety` asked for a tier. Off by default, and [`Races`] says why that
    /// one is not a cost argument like the others.
    pub races: Races,
    /// What to produce.
    pub emit: EmitKind,
    /// Whether to emit debug information.
    pub debug_info: bool,
    /// How the debug sections are compressed, from `-gz`.
    ///
    /// Nothing reads this yet because nothing writes a debug section yet. It is the same shape of
    /// answer `prefix_map.debug` is, and it is waiting for the same crate.
    pub compress: Compress,
    /// What the `-flto` family asked for, which nothing does yet.
    pub lto: Lto,
    /// What the profile reading half of the `-fprofile` family asked for, which nothing reads yet.
    ///
    /// Named for the data rather than for the flag, because `profile` next door is already the
    /// answer to whether `-pg` asked for a call to a profiler on the way into every function, and
    /// the two are different questions about the same word.
    pub profile_data: Profile,
    /// Whether every function keeps a frame pointer, from `-fno-omit-frame-pointer`.
    ///
    /// Off by default, which is what gcc does at every level above `-O0` and what leaves the
    /// register free for the allocator. A profiler that walks the stack by following saved frame
    /// pointers needs it on, and so does any code a debugger has to unwind without unwind tables.
    pub frame_pointer: bool,
    /// Whether the red zone may be used, from `-mno-red-zone` turned around.
    ///
    /// The 128 bytes below the stack pointer that the System V psABI promises no signal handler
    /// will touch, which lets a small leaf function keep its locals without moving the stack
    /// pointer at all. A kernel turns this off, because an interrupt taken on the kernel stack
    /// makes the promise false, and every kernel build in the wild passes `-mno-red-zone` for
    /// exactly that reason. A convention without a red zone ignores this.
    pub red_zone: bool,
    /// Whether the blocks of a function are put in the order their weights say rather than in the
    /// order the shape of the graph gives, from `-freorder-blocks` and `-fno-reorder-blocks`.
    ///
    /// `None` is a command line that said neither, which is nearly every one, and then the level
    /// decides: on above `-O0`, which is where gcc turns it on. It is a three way answer rather
    /// than a `bool` because `-O2 -fno-reorder-blocks` and `-O0` have to be different things and
    /// a `bool` set from the level could not tell them apart.
    pub reorder_blocks: Option<bool>,
    /// Which functions get a stack protector, from the `-fstack-protector` family.
    pub protector: Protector,
    /// Whether a prologue takes its frame a page at a time, from `-fstack-clash-protection`.
    ///
    /// An operating system leaves one page unmapped below every stack so that a stack growing
    /// into it faults. A function whose frame is larger than that page moves the stack pointer
    /// clean over it in one subtraction and can then write below it, into whatever the program
    /// mapped next, which is a way of reaching one allocation from another that costs an attacker
    /// nothing but a large local array. A prologue that takes the frame a page at a time and
    /// writes to each page as it arrives faults on the first one that is not there.
    ///
    /// Off by default, which is gcc's default. Distributions that build with it build everything
    /// with it, because the hole is in whichever function was left out.
    pub stack_clash: bool,
    /// Which control flow transfers are checked, from `-fcf-protection=`.
    ///
    /// See [`Control`]. Off by default, which is gcc's default on these targets, and on again in
    /// every distribution's global flags for the same reason the stack protector is.
    pub control: Control,
    /// Whether every function calls a profiler's hook on the way in, from `-pg` and `-p`.
    ///
    /// A profiler wants a count of which function called which, and the moment a function is
    /// entered is the only place a compiler can hand it one. It changes the link as well as the
    /// code, since the counts have to be started before `main` and written out after it, and the
    /// start file that does that is a different one.
    ///
    /// A tracer wants the same call for a different reason. The hook is one instruction the kernel
    /// can overwrite while the program runs, which is what makes a function traceable without
    /// rebuilding it, and it is why Linux is built this way rather than to be profiled.
    pub profile: bool,
    /// Where that call goes, from `-mfentry` and `-mno-fentry`.
    ///
    /// See [`Hook`]. Read even on a command line that did not ask for the call, since gcc accepts
    /// the flag on its own and does nothing with it.
    pub hook: Hook,
    /// How much room every function opens with for somebody to write over later, from
    /// `-fpatchable-function-entry=`.
    ///
    /// See [`Patchable`]. A kernel asks for this so that a function can be traced without being
    /// rebuilt: the room is a known number of bytes at a known address, and the addresses are
    /// collected into a section of their own so that whatever does the patching can find every one
    /// of them without reading the symbol table.
    pub patchable: Patchable,
    /// What happens rather than nothing being defined when arithmetic overflows, from `-fwrapv`,
    /// `-fwrapv-pointer`, `-fno-strict-overflow` and `-ftrapv`.
    ///
    /// See [`Wrapping`]. Nothing wraps and nothing stops by default, which is what C says and what
    /// lets the optimizer read a loop counter as a number rather than as a number that may turn
    /// round.
    pub wrapping: Wrapping,
    /// What a plain `char` is, from `-fsigned-char` and `-funsigned-char`, with nothing meaning
    /// the answer the target's ABI gives.
    ///
    /// Plain `char` is a third type either way, distinct from both `signed char` and
    /// `unsigned char` in every place a type is compared, and this says which of the two it has
    /// the range of. Changing it changes the ABI, so it is a decision about the whole program
    /// rather than about one file, and `__CHAR_UNSIGNED__` is defined when the answer is unsigned
    /// so that a header can see what was decided.
    pub char_signed: Option<bool>,
    /// Whether an enumeration nothing wrote an underlying type for is represented in the smallest
    /// integer type that holds its enumerators, from `-fshort-enums`.
    ///
    /// The default is `int` or wider, which is what C says and what every psABI in the table
    /// expects. This makes it `char` or wider instead, so `enum { A }` is one byte, and that
    /// changes the size and the alignment of anything holding one. It is here because a great deal
    /// of embedded C and every ARM EABI object is built with it, and mixing the two answers in one
    /// program is a silent disagreement about layout rather than a link error.
    pub short_enums: bool,
    /// Whether an access names the type it goes through, from `-fstrict-aliasing` and
    /// `-fno-strict-aliasing`.
    ///
    /// On, which is gcc's answer at every level above `-O0` and is what C 6.5 paragraph 7 already
    /// says. Clearing it makes the front end leave the type off every load and every store, and an
    /// access with no type on it is one the alias analysis has no type based reason to separate
    /// from any other, which is what the flag asks for.
    pub strict_aliasing: bool,
    /// How far a multiply and an addition may be fused into one rounding, from `-ffp-contract=`.
    ///
    /// See [`Contract`]. Most of the floating point flags have nowhere to be kept, because they
    /// withdraw licences that nothing here takes in the first place: no arithmetic in a function
    /// body is folded at any level, so a flag saying the rounding mode may have changed describes
    /// what already happens. This one and [`Options::trapping_math`] are the two that have
    /// somewhere to go.
    pub fp_contract: Contract,
    /// Whether an operation may raise an exception the program then looks at, from
    /// `-ftrapping-math` and `-fno-trapping-math`.
    ///
    /// On, which is gcc's default. What clearing it licenses here is one thing: the conversion of
    /// a constant floating value to an integer type it does not fit in. Left to the hardware that
    /// conversion is one instruction and the answer is the integer indefinite value, which is what
    /// both compilers give by default. gcc folds it under this flag instead, to the nearest end of
    /// the integer's range, and the difference is visible because the conversion is undefined
    /// behaviour rather than a value, so neither answer is wrong and the one a program was written
    /// against is gcc's.
    pub trapping_math: bool,
    /// What a path is rewritten by before it is written into the output, from the
    /// `-f*-prefix-map=` family.
    ///
    /// See [`PrefixMaps`]. This is what makes a build reproducible from a different directory, and
    /// it is three lists rather than one because gcc has three flags and a build uses them apart.
    pub prefix_map: PrefixMaps,
    /// Whether warnings are errors.
    pub warnings_are_errors: bool,
    /// Whether a warning is raised at all, which is `-w` turned around.
    ///
    /// A build that passes this has decided it does not want to hear about anything that is not
    /// fatal, and the flag is dropped at the one place every diagnostic goes through rather than
    /// tested at each site that raises one. `-w` beats `-Werror` where both are given, because a
    /// warning that was never raised cannot be promoted.
    pub warnings: bool,
    /// How many diagnostics to print before giving up. Past a certain point the output is
    /// noise from a single earlier mistake, and GCC's default of no limit is not a kindness.
    pub error_limit: u32,
    /// The dialect, from `-std=`.
    pub std: Std,
    /// Whether the GNU extensions are on, which is `-std=gnu23` rather than `-std=c23`.
    pub gnu_extensions: bool,
    /// Whether `-pedantic` was given, which is what turns a use of an extension from silence
    /// into a diagnostic. It is not the same knob as the dialect: `-std=c17 -pedantic` warns
    /// about a construct that `-std=c17` alone accepts without a word.
    pub pedantic: bool,
    /// Whether `-fpermissive` was given, which turns the rules gcc 14 promoted from errors back
    /// into warnings.
    ///
    /// Six of them, all about code written before the language settled: a declaration with no
    /// type in it, a call to a function nothing declared, a parameter in an old style definition
    /// with no type, a pointer made from an integer, a pointer assigned from a pointer to
    /// something else, and a `return` whose value disagrees with what was promised. The flag says
    /// nothing about any other diagnostic, and it does not say to compile something different: a
    /// program it accepts is compiled the way the rule it broke says it means.
    pub permissive: bool,
    /// Whether the whole unit is under GNU's reading of `inline` rather than C's, which is
    /// `-fgnu89-inline`.
    ///
    /// Under C's reading a definition every file-scope declaration wrote `inline` for and none
    /// wrote `extern` for emits nothing, and under GNU's it is the definition alone that decides
    /// and `extern inline` is the one that emits nothing. The C89 dialects are under GNU's
    /// whatever this says, since that is where the older reading came from, so this is the flag a
    /// program written against it reaches for when it is being compiled under a later dialect.
    pub gnu89_inline: bool,
    /// What a name that nothing in the source said anything about reaches, from `-fvisibility=`.
    pub visibility: Visibility,
    /// Whether the object may end up in a shared library, from `-fPIC` and `-fPIE`.
    pub pic: Pic,
    /// Whether a definition in this unit may be replaced at load time by one in another object,
    /// from `-fsemantic-interposition` and `-fno-semantic-interposition`.
    ///
    /// True is the honest answer and is gcc's default, because that is what an exported name in a
    /// shared library means: the dynamic linker takes the first definition it finds in load order,
    /// so a function this unit defines and calls may not be the one that runs. Everything the
    /// optimizer reads off a body has to stop at a name like that.
    ///
    /// False is a promise the build makes, and every distribution makes it, because otherwise a
    /// library cannot inline its own functions into each other. It is a promise rather than a
    /// deduction: nothing checks it, and a program that then interposes one of those names gets a
    /// mixture of the two definitions. It says nothing about `-fPIE`, where no name is replaceable
    /// to begin with, and it says nothing about how an address is reached, which is the separate
    /// question `-fPIC` decides.
    pub interposition: bool,
    /// Whether a function is described to an unwinder at every instruction, from
    /// `-fasynchronous-unwind-tables` and `-fno-asynchronous-unwind-tables`.
    ///
    /// True is the default, which is gcc's wherever anything reads the table, and the reason is
    /// that the programs that read it are not the ones being compiled. C++ exceptions,
    /// `backtrace`, a profiler sampling a stack and a crash handler printing one all walk frames
    /// belonging to code that knew nothing about them, so a unit that opts out stops a walk that
    /// started somewhere else.
    ///
    /// What `asynchronous` asks for on top of a table is that the answer is right at every
    /// instruction and not only where a call is, because a signal can arrive anywhere, including
    /// the middle of a prologue. Rows come off the prologue as it is built here, so that is the
    /// only kind of table there is to write and the weaker request below is answered with it.
    ///
    /// False is for a build that knows nothing will ever walk it, which in practice is a kernel or
    /// a freestanding image, and what it saves is the section rather than any instruction.
    pub async_unwind_tables: bool,
    /// Whether a function is described to an unwinder at all, from `-funwind-tables` and
    /// `-fno-unwind-tables`.
    ///
    /// The weaker of the two requests and off by default, because the one above is on and implies
    /// it. A table is written when either of them is standing, which is what [`Self::unwinds`]
    /// answers and is how gcc resolves a line that asks for a table and against an asynchronous
    /// one.
    ///
    /// Neither of them is about anything but ELF. Mach-O and COFF have their own arrangements and
    /// neither is written yet, so on those targets nothing reads these.
    pub unwind_tables: bool,
    /// Whether each function gets a section of its own, from `-ffunction-sections`.
    ///
    /// A linker can leave out a section nothing reaches and cannot leave out half of one, so this
    /// is what makes `--gc-sections` able to drop a function this file defines and nothing calls.
    /// A kernel and an embedded image are both linked that way and are both a good deal larger
    /// without it, and the cost is one section header per function.
    pub function_sections: bool,
    /// Whether each variable gets a section of its own, from `-fdata-sections`.
    ///
    /// The same bargain for the data, and a separate flag because gcc has two of them: a build
    /// that wants one and not the other is a build that measured something. Splitting the data can
    /// cost more than it saves, since two variables a loop reads together are no longer certain to
    /// land in the same page.
    pub data_sections: bool,
    /// The GCC release claimed, from `-fgnuc-version=`.
    pub gnuc: GnucVersion,
    /// Whether there is a standard library, which is `-ffreestanding` turned around.
    pub hosted: bool,
    /// Whether a call to a C library function written under its own plain name may be taken to
    /// mean that function, which is `-fno-builtin` turned around.
    ///
    /// The names are reserved, so `llabs` is the library's `llabs` and the compiler is allowed to
    /// know what it does. A program that means something else by one of them is the reason the
    /// flag exists, and `-ffreestanding` turns it off as well, because a freestanding program has
    /// no C library for the name to be the name of. The `__builtin_` spellings are not affected by
    /// either, since the prefix is the program saying which function it means.
    pub builtins: bool,
    /// The names `-fno-builtin-<name>` took away one at a time, without the prefix.
    ///
    /// A build that means its own `memcpy` and the library's everything else writes this rather
    /// than the whole flag, which is what the kernel does for a handful of names.
    pub no_builtin: Vec<String>,
    /// The glibc release the headers on the search path are, as the minor number alone.
    ///
    /// `Some` means two things together: this is a glibc target, and step 3 of
    /// `spec/cross-compile/08-sysroots.md` section 8.5 resolved to the tree we bundle. Then the
    /// compiler defines `__GLIBC_MINOR__`, because one tree serves every version and the version is
    /// the part of it the target supplies. `__GLIBC__` is not ours to define either way, since it is
    /// in the tree and a real `features.h` defines it too.
    ///
    /// `None` is every other case, and the cases matter more than the value. A host glibc's
    /// `features.h` defines the macro itself, and a tree the user named has a `features.h` of its
    /// own, so defining it as well would be two definitions with different values, which is a
    /// warning on every compilation of every file. A musl or mingw target has no such macro at all.
    pub glibc_minor: Option<u32>,
    /// `-D` in command line order. `FOO` means `FOO=1`, as GCC has it.
    pub defines: Vec<String>,
    /// `-U` in command line order, applied after the defines because `-U` wins.
    pub undefines: Vec<String>,
    /// Where a header is looked for.
    pub search: SearchPath,
    /// What `-imacros` and `-include` named, in command line order.
    pub preincludes: Vec<Preinclude>,
    /// Whether `-E` writes line markers, which `-P` turns off.
    pub line_markers: bool,
    /// What the `-d` family asks for.
    pub dumps: Dumps,
    /// What the `-M` family asks for.
    pub deps: Deps,
    /// Whether the intermediate files are kept, from `-save-temps`.
    pub save_temps: SaveTemps,
    /// Whether each step says how long it took, from `-time`.
    pub time: bool,
    /// What `-f<pass>` and `-fno-<pass>` said about an optimizer pass, in the order the command
    /// line said it, so that the last mention of a pass is the one that decides.
    ///
    /// The pipeline the level chose is the starting point and this is what is added to and taken
    /// away from it. The names are checked against the pass list while the arguments are parsed,
    /// so anything in here is a pass the compiler has.
    pub passes: Vec<(String, bool)>,
    /// What `-fpass-fuel=<pass>=<n>` limited a pass to, by pass name.
    ///
    /// A pass with an entry here performs exactly that many transformations and then stops
    /// transforming, which is what bisects a miscompilation to one rewrite. See section 9.10 of
    /// `spec/09-optimizer.md`.
    pub pass_fuel: Vec<(String, u32)>,
    /// What `-fpass-fuel-global=<n>` limited the whole pipeline to, across every pass.
    ///
    /// The outer of the two searches in section 4.5 of `spec/optimizer/04-pass-manager.md`.
    /// Halving this says which pass holds the bad rewrite, and halving `-fpass-fuel` for that
    /// pass says which rewrite it is. Where both are given, a pass is stopped by whichever of
    /// the two is tighter.
    pub pass_fuel_global: Option<u32>,
    /// What `-fdisable-<pass>[=<range>]` and `-fenable-<pass>[=<range>]` said, in the order the
    /// command line said it, with `true` for the enabling half.
    ///
    /// A rule covers the functions it names and nothing else, and the last rule that covers a
    /// function is the one that decides for it, so the order has to survive. This is the second
    /// half of the bisection interface in section 41.6 of `spec/optimizer/41-correctness.md`:
    /// `-fpass-fuel` finds the rewrite and this finds the function. The pass names are checked
    /// against the pass list while the arguments are parsed.
    pub pass_gates: Vec<(bool, String)>,
    /// What `-fdump-ir=` asked to see, as it was written, which is `all`, `before-<pass>` or
    /// `after-<pass>`.
    pub dump_ir: Vec<String>,
    /// What `-fopt-info` asked to hear about, as the keywords were written, with the leading
    /// hyphen taken off, so a bare `-fopt-info` is the empty string in here.
    ///
    /// The keywords are `optimized`, `missed`, `note` and `all`, and two flags add up rather than
    /// the second replacing the first. Checked while the arguments are parsed, so anything in
    /// here is a spelling the optimizer understands. See section 42.2 of
    /// `spec/optimizer/42-measurement.md` for why `missed` is the one that earns the feature.
    pub opt_info: Vec<String>,
    /// Where `-fopt-info=<file>` sends the remarks, or `None` for standard error.
    ///
    /// One file for the whole run rather than one per input, the way GCC does it, and the last
    /// one on the command line is the one that decides. A harness that wants the remarks kept
    /// away from the diagnostics gives a file, which is what the corpus in `tamnd/rucc-corpus`
    /// does with GCC so that a rejection can still be matched against the diagnostic stream.
    pub opt_info_file: Option<String>,
    /// Whether the IR verifier runs after every pass that changed anything.
    ///
    /// On in a debug build without being asked, since that is where a broken pass should be
    /// caught. `-Zverify-each` turns it on in a release build, which is what CI wants.
    pub verify_each: bool,
    /// Where `-Zrule-coverage=FILE` writes which lowering rules fired, if it was given.
    ///
    /// A measurement rather than a thing a build asks for, which is why it is spelled with a `-Z`
    /// the way an unstable option is everywhere else: it is here for the harness in
    /// `tamnd/rucc-compat` to union over a corpus and report, and nothing about the code that comes
    /// out changes when it is on. One file per run of the compiler, holding the whole rule set with
    /// the rules this run reached marked, whatever the run compiled and however many files it was.
    pub rule_coverage: Option<String>,
    /// Where `-Zregister-pressure=FILE` writes what the allocator had to put on the stack.
    ///
    /// A measurement and spelled with a `-Z` for the same reason as the one above: nothing about
    /// the code that comes out changes when it is on. One file per run of the compiler, one line
    /// per function, holding how many values went to the stack and how many stores and reloads
    /// that cost. What reads it is `cargo xtask pressure`, which compiles the benchmarks in
    /// `bench/safety` with the monitor off and on and reports the difference, since
    /// `spec/safe-memory/13-performance.md` section 13.1 asks for that number and section 5.2.1
    /// says why: a capability in flight is four words, and if materializing one spills something
    /// else in a hot loop then check elimination cannot save it.
    pub register_pressure: Option<String>,
}

impl Options {
    /// Default options for `target`.
    pub fn new(target: Triple) -> Self {
        Self {
            target,
            opt_level: OptLevel::default(),
            safety: Safety::default(),
            padding: Padding::default(),
            subobject: Subobject::default(),
            promise: Promise::default(),
            races: Races::default(),
            emit: EmitKind::default(),
            debug_info: false,
            compress: Compress::None,
            lto: Lto::default(),
            profile_data: Profile::default(),
            frame_pointer: false,
            red_zone: true,
            reorder_blocks: None,
            protector: Protector::default(),
            stack_clash: false,
            control: Control::default(),
            profile: false,
            hook: Hook::default(),
            patchable: Patchable::default(),
            wrapping: Wrapping::NONE,
            char_signed: None,
            short_enums: false,
            strict_aliasing: true,
            fp_contract: Contract::Off,
            trapping_math: true,
            prefix_map: PrefixMaps::default(),
            warnings_are_errors: false,
            warnings: true,
            error_limit: 20,
            std: Std::default(),
            gnu_extensions: true,
            pedantic: false,
            permissive: false,
            gnu89_inline: false,
            visibility: Visibility::default(),
            pic: Pic::default(),
            interposition: true,
            async_unwind_tables: true,
            unwind_tables: false,
            function_sections: false,
            data_sections: false,
            gnuc: GnucVersion::default(),
            hosted: true,
            builtins: true,
            no_builtin: Vec::new(),
            glibc_minor: None,
            defines: Vec::new(),
            undefines: Vec::new(),
            search: SearchPath::new(),
            preincludes: Vec::new(),
            line_markers: true,
            dumps: Dumps::default(),
            deps: Deps::default(),
            save_temps: SaveTemps::default(),
            time: false,
            passes: Vec::new(),
            pass_fuel: Vec::new(),
            pass_fuel_global: None,
            pass_gates: Vec::new(),
            dump_ir: Vec::new(),
            opt_info: Vec::new(),
            opt_info_file: None,
            verify_each: cfg!(debug_assertions),
            rule_coverage: None,
            register_pressure: None,
        }
    }

    /// Whether a function in this unit is described to an unwinder.
    ///
    /// Either request is answered with the same table, so what decides is whether either of them
    /// is standing. Asked here rather than worked out at the two places that write a table, since
    /// those two writing different answers for one function is what `spec/11-asm-objects-debug.md`
    /// section 11.1 says must not be possible.
    #[must_use]
    pub const fn unwinds(&self) -> bool {
        self.async_unwind_tables || self.unwind_tables
    }
}

/// One compilation.
///
/// Holds the options, the string interner and the diagnostics raised so far. Passing a
/// `&mut Session` is how a stage reports a problem, and the return value of a stage says
/// what it produced, never whether it succeeded: that question is answered by
/// [`Session::has_errors`].
#[derive(Debug)]
pub struct Session {
    /// What this compilation was asked to do.
    pub opts: Options,
    /// Everything known about the target.
    pub target: TargetInfo,
    /// The one interner for the compilation.
    pub interner: Interner,
    /// Every file read during the compilation, and the flat coordinate space their spans
    /// live in.
    ///
    /// This is on the session rather than passed around separately because a span is only
    /// meaningful against the map that issued it, and one map per compilation is the rule
    /// that makes that true by construction.
    pub sources: SourceMap,
    diagnostics: Vec<Diagnostic>,
    error_count: u32,
    warning_count: u32,
}

impl Session {
    /// A session for `opts`.
    ///
    /// The command line's answer about plain `char` is put into the target here rather than
    /// carried beside it, because every place that asks what a `char` is asks the target, and two
    /// answers to one question is how a front end ends up disagreeing with its own back end.
    pub fn new(opts: Options) -> Self {
        let mut target = TargetInfo::new(opts.target);
        if let Some(signed) = opts.char_signed {
            target.char_is_signed = signed;
        }
        Self {
            opts,
            target,
            interner: Interner::with_capacity(1024),
            sources: SourceMap::new(),
            diagnostics: Vec::new(),
            error_count: 0,
            warning_count: 0,
        }
    }

    /// Records a diagnostic.
    ///
    /// Under `-Werror` a warning is promoted here, once, rather than at every site that
    /// raises one, and under `-w` it is dropped here for the same reason. A warning that `-w`
    /// dropped is not counted, so `-w -Werror` compiles rather than failing on a warning
    /// nobody was going to see.
    pub fn emit(&mut self, mut diag: Diagnostic) {
        if !self.opts.warnings && diag.severity == Severity::Warning {
            return;
        }
        if self.opts.warnings_are_errors && diag.severity == Severity::Warning {
            diag.severity = Severity::Error;
        }
        match diag.severity {
            Severity::Error | Severity::Ice => self.error_count += 1,
            Severity::Warning => self.warning_count += 1,
            Severity::Note | Severity::Help => {}
        }
        self.diagnostics.push(diag);
    }

    /// Everything raised so far, in the order it was raised.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Whether anything fatal has been raised.
    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    /// How many errors have been raised.
    pub fn error_count(&self) -> u32 {
        self.error_count
    }

    /// How many warnings have been raised.
    pub fn warning_count(&self) -> u32 {
        self.warning_count
    }

    /// Whether the error limit has been reached and the caller should stop.
    pub fn error_limit_reached(&self) -> bool {
        self.opts.error_limit != 0 && self.error_count >= self.opts.error_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(Options::new("x86_64-unknown-linux-gnu".parse().unwrap()))
    }

    #[test]
    fn a_version_claim_reads_the_way_gcc_prints_one() {
        // `gcc -dumpfullversion` gives all three, `gcc -dumpversion` gives one, and both are
        // things a script pastes straight into a flag.
        let all = |v: &str| v.parse::<GnucVersion>().unwrap();
        assert_eq!(all("15.1.0"), GnucVersion { major: 15, minor: 1, patch: 0 });
        assert_eq!(all("15"), GnucVersion { major: 15, minor: 0, patch: 0 });
        assert_eq!(all("4.2"), GnucVersion { major: 4, minor: 2, patch: 0 });
        assert!("".parse::<GnucVersion>().is_err());
        assert!("15.".parse::<GnucVersion>().is_err(), "a trailing dot is a typo, not a zero");
        assert!("1.2.3.4".parse::<GnucVersion>().is_err());
    }

    #[test]
    fn a_prefix_map_rewrites_the_front_of_a_path_and_nothing_else() {
        let map = |pairs: &[(&str, &str)]| {
            let mut map = PrefixMap::new();
            for &(old, new) in pairs {
                map.push(old, new);
            }
            map
        };
        assert!(PrefixMap::new().is_empty());
        assert_eq!(PrefixMap::new().apply("sub/h.h"), "sub/h.h");

        let one = map(&[("sub", "SUB")]);
        assert_eq!(one.apply("sub/h.h"), "SUB/h.h");
        assert_eq!(one.apply("a.c"), "a.c", "a path the mapping does not start");
        assert_eq!(one.apply("x/sub/h.h"), "x/sub/h.h", "the middle of a path is not the front");

        // Characters rather than directories, which is what gcc compares and is worth a test of
        // its own because it is the part that looks like it ought to be otherwise.
        assert_eq!(map(&[("s", "B")]).apply("sub/h.h"), "Bub/h.h");
        assert_eq!(map(&[("sub/", "SUB/")]).apply("sub/h.h"), "SUB/h.h");
        assert_eq!(map(&[("sub", "")]).apply("sub/h.h"), "/h.h", "mapping to nothing");
        assert_eq!(map(&[("", "PRE")]).apply("a.c"), "PREa.c", "an empty old is in front of all");

        // The last one that matches wins, whether or not the two ask about the same prefix, which
        // is what a project wide mapping plus a narrower one for a directory relies on.
        assert_eq!(map(&[("sub", "ONE"), ("sub", "TWO")]).apply("sub/h.h"), "TWO/h.h");
        assert_eq!(map(&[("sub", "A"), ("s", "B")]).apply("sub/h.h"), "Bub/h.h");
        assert_eq!(map(&[("s", "B"), ("sub", "A")]).apply("sub/h.h"), "A/h.h");
        assert_eq!(map(&[("nope", "X"), ("sub", "A")]).apply("sub/h.h"), "A/h.h");
    }

    #[test]
    fn the_argument_is_split_at_the_last_equals_sign() {
        assert_eq!(PrefixMap::split("old=new"), Some(("old", "new")));
        assert_eq!(PrefixMap::split("=new"), Some(("", "new")), "an empty old is allowed");
        assert_eq!(PrefixMap::split("old="), Some(("old", "")), "and so is an empty new");
        // The last rather than the first, so a directory whose name has an `=` in it can be
        // mapped and a replacement whose name has one cannot. That is gcc's choice of which of
        // the two to make possible, and it is the right way round.
        assert_eq!(PrefixMap::split("/home/a=b=/src"), Some(("/home/a=b", "/src")));
        assert_eq!(PrefixMap::split("nope"), None);
    }

    #[test]
    fn optimisation_levels_parse_the_way_gcc_spells_them() {
        assert_eq!("".parse::<OptLevel>().unwrap(), OptLevel::O1);
        assert_eq!("0".parse::<OptLevel>().unwrap(), OptLevel::O0);
        assert_eq!("2".parse::<OptLevel>().unwrap(), OptLevel::O2);
        assert_eq!("9".parse::<OptLevel>().unwrap(), OptLevel::O3);
        assert_eq!("s".parse::<OptLevel>().unwrap(), OptLevel::Os);
        assert!("q".parse::<OptLevel>().is_err());
    }

    #[test]
    fn only_o0_skips_the_optimizer() {
        assert!(!OptLevel::O0.runs_optimizer());
        assert!(OptLevel::O1.runs_optimizer());
        assert!(OptLevel::Oz.runs_optimizer());
    }

    #[test]
    fn the_safety_tiers_round_trip_and_nothing_else_is_one() {
        for tier in [Safety::Off, Safety::Detect, Safety::Enforce, Safety::Kernel] {
            assert_eq!(tier.as_str().parse::<Safety>().unwrap(), tier);
        }
        // `on` is the obvious thing to try and it is not a tier, because which tier somebody
        // means by it is the whole question document 02 answers.
        assert!("on".parse::<Safety>().is_err());
        assert!("".parse::<Safety>().is_err());
    }

    #[test]
    fn room_for_a_patcher_is_written_the_way_it_was_asked_for() {
        for (written, total, before) in
            [("0", 0, 0), ("2", 2, 0), ("16", 16, 0), ("5,3", 5, 3), ("3,3", 3, 3)]
        {
            let room: Patchable = written.parse().unwrap();
            assert_eq!(room, Patchable { total, before });
            assert_eq!(room.to_string(), written);
            assert_eq!(room.after(), total - before);
            assert_eq!(room.any(), total > 0);
        }
        // A second number of zero is the same request as no second number, and it is written back
        // the shorter way, which is the way somebody reaching for the flag writes it.
        assert_eq!("2,0".parse::<Patchable>().unwrap().to_string(), "2");
    }

    #[test]
    fn more_room_in_front_of_the_label_than_there_is_room_at_all_is_refused() {
        // Rather than clamped, because there is no reading of it a caller meant. gcc says the same
        // about each of these.
        assert!("1,2".parse::<Patchable>().is_err());
        assert!("1,2,3".parse::<Patchable>().is_err());
        assert!("a".parse::<Patchable>().is_err());
        assert!("".parse::<Patchable>().is_err());
        assert!("-1".parse::<Patchable>().is_err());
    }

    #[test]
    fn the_two_places_the_intermediate_files_can_go_are_the_two_words_that_are_taken() {
        assert_eq!("obj".parse::<SaveTemps>().unwrap(), SaveTemps::Object);
        assert_eq!("cwd".parse::<SaveTemps>().unwrap(), SaveTemps::Cwd);
        // The names of the two flags that mean the same thing as `=obj` are not themselves
        // arguments of it, and neither is silence.
        assert!("obj,cwd".parse::<SaveTemps>().is_err());
        assert!("".parse::<SaveTemps>().is_err());
        // Nothing is kept unless something asked, and both of the words that ask do ask.
        assert_eq!(SaveTemps::default(), SaveTemps::No);
        assert!(!SaveTemps::No.wanted());
        assert!(SaveTemps::Object.wanted());
        assert!(SaveTemps::Cwd.wanted());
    }

    #[test]
    fn a_build_that_did_not_ask_for_the_monitor_does_not_get_it() {
        assert_eq!(Safety::default(), Safety::Off);
        assert!(!Safety::Off.instruments());
        assert!(Safety::Detect.instruments());
        assert!(Safety::Enforce.instruments());
        assert!(Safety::Kernel.instruments());
    }

    #[test]
    fn emit_kinds_round_trip_through_their_names() {
        for k in [
            EmitKind::Executable,
            EmitKind::Object,
            EmitKind::Asm,
            EmitKind::Preprocessed,
            EmitKind::Tast,
            EmitKind::Ir,
            EmitKind::MirFinal,
        ] {
            assert_eq!(k.as_str().parse::<EmitKind>().unwrap(), k);
        }
    }

    #[test]
    fn errors_are_counted_and_warnings_are_not() {
        let mut s = session();
        s.emit(Diagnostic::error("no", rucc_diag::Span::DUMMY));
        s.emit(Diagnostic::warning("hmm", rucc_diag::Span::DUMMY));
        assert_eq!(s.error_count(), 1);
        assert_eq!(s.warning_count(), 1);
        assert!(s.has_errors());
        assert_eq!(s.diagnostics().len(), 2);
    }

    #[test]
    fn werror_promotes_once_at_the_sink() {
        let mut opts = Options::new("x86_64-unknown-linux-gnu".parse().unwrap());
        opts.warnings_are_errors = true;
        let mut s = Session::new(opts);
        s.emit(Diagnostic::warning("hmm", rucc_diag::Span::DUMMY));
        assert_eq!(s.error_count(), 1);
        assert_eq!(s.warning_count(), 0);
        assert_eq!(s.diagnostics()[0].severity, Severity::Error);
    }

    #[test]
    fn the_error_limit_can_be_switched_off() {
        let mut opts = Options::new("x86_64-unknown-linux-gnu".parse().unwrap());
        opts.error_limit = 0;
        let mut s = Session::new(opts);
        for _ in 0..100 {
            s.emit(Diagnostic::error("no", rucc_diag::Span::DUMMY));
        }
        assert!(!s.error_limit_reached());
    }

    #[test]
    fn the_session_carries_the_source_map_spans_are_resolved_against() {
        let mut s = session();
        let file = s.sources.add("a.c", b"int x;\n".to_vec()).unwrap();
        let start = s.sources.file(file).start;
        assert_eq!(s.sources.render_position(start + 4), "a.c:1:5");
    }

    #[test]
    fn the_session_carries_the_resolved_target() {
        let s = session();
        assert_eq!(s.target.pointer_width, 64);
        assert!(s.target.char_is_signed);
    }
}
