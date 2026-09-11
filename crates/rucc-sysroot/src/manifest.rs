//! What a produced sysroot carries: every input, where it came from, its hash and its licence.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.6 for the licence half,
//! `spec/cross-compile/13-distribution.md` section 13.5 for the provenance half, and
//! `spec/cross-compile/02-the-goal.md` claim 5 for the rest.
//!
//! # Three jobs, one file
//!
//! Claim 5 asks for byte identical output from two hosts for the same target. Checking that over a
//! sysroot means comparing several thousand files, and the first thing anybody does when the
//! comparison fails is ask which file and where it came from. A manifest answers both, and comparing
//! two manifests is a diff of a few hundred lines rather than of a directory tree.
//!
//! The second job is the licence wall. Section 8.6 says the macOS SDK and the Windows SDK cannot be
//! redistributed, and the way that rule gets enforced rather than remembered is that every input
//! carries its licence and [`Manifest::redistributable`] is a function anything that publishes an
//! artifact can call. A rule in a document is a rule somebody breaks in eighteen months.
//!
//! The third is provenance. Section 13.5 asks the compiler to emit, for every input that is not its
//! own code, the name, the upstream project, the version, the source URL, the content hash, the
//! licence and whether the input was bundled, generated or fetched. That is this file's line with
//! two fields added, so `-print-sysroot-provenance` prints a manifest rather than a second format
//! saying the same things in a different order.
//!
//! # The format
//!
//! Tab separated lines, sorted by path, under a header that is two lines and sometimes three: the
//! format version, the target, and the Linux release the kernel headers came out of when the sysroot
//! has kernel headers in it. Not JSON, because the thing this is optimized for is a person reading a
//! diff between two of them, and not TOML, because it has no nesting and a parser for it is thirty
//! lines. Sorted because the order files come out of a directory walk is a property of the
//! filesystem, and a manifest whose line order depended on that would report a difference between
//! two identical sysroots.
//!
//! ```
//! use rucc_sysroot::{Input, Licence, Manifest, Provenance};
//! use rucc_tuple::{TargetTuple, Version};
//!
//! let target: TargetTuple = "aarch64-linux-musl".parse().unwrap();
//! let mut manifest = Manifest::new(target);
//! manifest.set_kernel(Version::new(6, 12));
//! manifest.push(Input {
//!     path: "include/generic/stdio.h".into(),
//!     source: "musl-1.2.5".into(),
//!     url: "https://musl.libc.org/releases/musl-1.2.5.tar.gz".into(),
//!     sha256: "0".repeat(64),
//!     licence: Licence::Mit,
//!     provenance: Provenance::Bundled,
//! });
//!
//! let text = manifest.render();
//! assert_eq!(Manifest::parse(&text).unwrap(), manifest);
//! assert!(manifest.redistributable());
//! ```

use std::fmt;
use std::str::FromStr;

use rucc_tuple::{TargetTuple, Version};

/// The licence an input arrives under.
///
/// The list is section 8.2's table with one variant per row, plus the kernel headers, which every
/// Linux row needs and which no row is about. A closed list rather than a free text field, because
/// the question [`Licence::redistributable`] answers has to have an answer for every input and a
/// string does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Licence {
    /// musl, which is MIT and the reason it goes first.
    Mit,
    /// glibc, which is LGPL. Redistributable, with obligations that
    /// `spec/cross-compile/13-distribution.md` owns.
    Lgpl,
    /// The BSD libcs, which are permissive.
    Bsd,
    /// The Linux uapi headers, which are GPL-2.0 with the syscall note.
    ///
    /// The note is the whole reason this is a separate variant and not a refusal: it says that
    /// using the headers to make a system call does not put the calling program under the GPL,
    /// which is what every libc and every cross toolchain relies on. Redistributing the headers
    /// themselves carries the GPL's own obligation, and the pinned source URL in the manifest is
    /// how it is met, the same way it is for glibc.
    LinuxUapi,
    /// mingw-w64, which is a mix of permissive licences and public domain headers.
    MingwPermissive,
    /// Ours. The compiler's own headers and its runtime.
    Apache2,
    /// The macOS SDK, restricted by the Xcode agreement to Apple-branded hardware. Never shipped,
    /// only ever pointed at.
    AppleSdk,
    /// The Windows SDK and the universal CRT, which are not redistributable.
    MicrosoftSdk,
}

impl Licence {
    /// Whether an artifact containing this input can be published.
    ///
    /// Two of the eight answer false, and they are the two section 8.6 calls legal walls rather
    /// than engineering. A sysroot containing either is a local thing on the machine of somebody
    /// who accepted the licence themselves.
    #[must_use]
    pub const fn redistributable(self) -> bool {
        !matches!(self, Licence::AppleSdk | Licence::MicrosoftSdk)
    }

    /// The spelling in a manifest file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Licence::Mit => "mit",
            Licence::Lgpl => "lgpl",
            Licence::Bsd => "bsd",
            Licence::LinuxUapi => "gpl-2.0-with-linux-syscall-note",
            Licence::MingwPermissive => "mingw-permissive",
            Licence::Apache2 => "apache-2.0",
            Licence::AppleSdk => "apple-sdk",
            Licence::MicrosoftSdk => "microsoft-sdk",
        }
    }
}

impl fmt::Display for Licence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Licence {
    type Err = ManifestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mit" => Ok(Licence::Mit),
            "lgpl" => Ok(Licence::Lgpl),
            "bsd" => Ok(Licence::Bsd),
            "gpl-2.0-with-linux-syscall-note" => Ok(Licence::LinuxUapi),
            "mingw-permissive" => Ok(Licence::MingwPermissive),
            "apache-2.0" => Ok(Licence::Apache2),
            "apple-sdk" => Ok(Licence::AppleSdk),
            "microsoft-sdk" => Ok(Licence::MicrosoftSdk),
            other => Err(ManifestError::UnknownLicence(other.to_string())),
        }
    }
}

/// How an input got to where it is.
///
/// Section 13.5 asks for this field and does not say what the three answers mean, which makes the
/// meaning a decision rather than a transcription. The question each answer has to settle is what a
/// person reproducing a file has to have: an upstream release is enough for one of them, our own
/// generator and its version is needed for the second, and the third did not exist on any machine
/// until somebody's network fetched it.
///
/// The test to apply is whether the bytes can be found in the upstream release. A header we unpack
/// and ship unchanged can be, so it is [`Provenance::Bundled`]. A stub shared object, a merged header
/// tree or a linker script cannot be, because this compiler wrote it, so it is
/// [`Provenance::Generated`] even though what it was derived from is upstream's. Anything that arrived
/// over the network after the release was built is [`Provenance::Fetched`], whoever wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provenance {
    /// Shipped inside the distribution, byte for byte as upstream released it. Reproducing it needs
    /// the release named in [`Input::source`] and nothing of ours.
    Bundled,
    /// Written by this compiler from something upstream describes, which is the stubs of
    /// `spec/cross-compile/09-libc-stubs.md` and the merged header trees of section 8.3.
    /// Reproducing it needs our generator at the version that wrote it as well as the release.
    Generated,
    /// Downloaded onto this machine, which for the two licence walls of section 13.4 is the only way
    /// the input can legally arrive at all. The record says so because a sysroot holding one is not
    /// the same artifact as a sysroot that shipped complete.
    Fetched,
}

impl Provenance {
    /// The spelling in a manifest file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Provenance::Bundled => "bundled",
            Provenance::Generated => "generated",
            Provenance::Fetched => "fetched",
        }
    }
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Provenance {
    type Err = ManifestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "bundled" => Ok(Provenance::Bundled),
            "generated" => Ok(Provenance::Generated),
            "fetched" => Ok(Provenance::Fetched),
            other => Err(ManifestError::UnknownProvenance(other.to_string())),
        }
    }
}

/// One file in a sysroot, and everything that has to be true of it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Input {
    /// Where it sits, relative to the sysroot root. Relative because an absolute path is a fact
    /// about the machine that built it, and two hosts have different ones.
    pub path: String,
    /// What it came out of, named so that the same manifest can be produced again. A release name
    /// and version rather than a URL, because a URL moves and a release does not.
    pub source: String,
    /// Where that release was fetched from, which is the field section 13.5 asks for by name.
    ///
    /// The URL of the release rather than of the file, because what anybody checking this does is
    /// download the release and look inside it, and because a per file URL would be a claim about
    /// somebody else's directory layout. It is here in spite of a URL moving and a release not,
    /// which is the reason [`Input::source`] exists and is not replaced by this: the two fields
    /// answer what it is and where it was got, and only the first of those is still true in ten
    /// years.
    pub url: String,
    /// The hash of the file, lowercase hex.
    pub sha256: String,
    /// What it may be done with.
    pub licence: Licence,
    /// Whether it was bundled, generated or fetched.
    pub provenance: Provenance,
}

/// The record of one produced sysroot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    target: TargetTuple,
    kernel: Option<Version>,
    inputs: Vec<Input>,
}

/// What went wrong reading a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// The first line was not the one this format starts with.
    NotAManifest,
    /// The version in the header is one this build does not read.
    UnknownVersion(String),
    /// The second line did not name a target, or named one that does not parse.
    BadTarget(String),
    /// The `kernel` line was there and what followed it is not a Linux release.
    BadKernel(String),
    /// A line did not have the six fields an input has.
    BadInput {
        /// Which line, counting from one.
        line: usize,
        /// How many tab separated fields it had.
        fields: usize,
    },
    /// A hash that is not sixty four lowercase hex characters.
    BadHash {
        /// Which line, counting from one.
        line: usize,
        /// What was there instead.
        found: String,
    },
    /// A licence spelling nothing here knows.
    UnknownLicence(String),
    /// A provenance spelling nothing here knows.
    UnknownProvenance(String),
    /// An empty field where a source, a URL or a path belongs. A record with a hole in it is worse
    /// than no record, because it reads as an answer.
    EmptyField {
        /// Which line, counting from one.
        line: usize,
        /// Which field was empty, spelled the way this module names it.
        field: &'static str,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::NotAManifest => write!(f, "this does not start like a sysroot manifest"),
            ManifestError::UnknownVersion(v) => {
                write!(f, "manifest format version {v}, which this build does not read")
            }
            ManifestError::BadTarget(t) => write!(f, "`{t}` is not a target this understands"),
            ManifestError::BadKernel(k) => {
                write!(f, "`{k}` is not a Linux release, which is what a kernel line carries")
            }
            ManifestError::BadInput { line, fields } => {
                write!(f, "line {line} has {fields} fields where an input has six")
            }
            ManifestError::BadHash { line, found } => {
                write!(f, "line {line} has `{found}` where a sha256 belongs")
            }
            ManifestError::UnknownLicence(l) => write!(f, "`{l}` is not a licence this knows"),
            ManifestError::UnknownProvenance(o) => {
                write!(f, "`{o}` is not bundled, generated or fetched")
            }
            ManifestError::EmptyField { line, field } => {
                write!(f, "line {line} has nothing where its {field} belongs")
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// The first line of every manifest, which is also how one is recognized.
///
/// Version 2 is version 1 with the source URL and the provenance on every line, which is what
/// section 13.5 asks a record of an input to carry. The number went up rather than the two fields
/// being optional, because a reader that accepted both would have to decide what a missing
/// provenance means and there is no honest answer to that: an input nobody wrote a provenance for is
/// an input whose provenance nobody knows. Nothing has written a version 1 file to a place that
/// outlives a build, so the only cost of the bump is this sentence.
///
/// Version 3 adds the optional `kernel` line. The number went up even though the line is optional,
/// because the whole point of a format version is that a reader can say it does not read a file, and
/// a version 2 reader handed a file with a `kernel` line in it would report the line as an input
/// with two fields rather than as a format it does not know.
const HEADER: &str = "rucc sysroot manifest 3";

impl Manifest {
    /// An empty manifest for this target.
    #[must_use]
    pub const fn new(target: TargetTuple) -> Self {
        Manifest { target, kernel: None, inputs: Vec::new() }
    }

    /// The target this sysroot is for.
    #[must_use]
    pub const fn target(&self) -> TargetTuple {
        self.target
    }

    /// The Linux release the kernel headers in this sysroot came out of, when one was recorded.
    ///
    /// Absent has one meaning and it is not "nobody knows": it is that this sysroot has no kernel
    /// headers in it. Every target that is not Linux is in that case, and so is a Linux sysroot
    /// produced before a kernel tree was installed beside it, which is a state the producer allows
    /// because the two trees are two commands. That is the one thing an optional line can mean here
    /// and it is why this one is optional where the provenance field is not: a file with no kernel
    /// headers in it has no kernel version, and an input always came from somewhere.
    ///
    /// What it is for is the question somebody asks after a cross build read a header nobody
    /// expected. `-print-sysroot` answers where and the manifest answers what, and a sysroot whose
    /// record names the release its `linux/` headers came out of makes a stale pairing visible
    /// instead of leaving it to be guessed at. Nothing here checks the version against the headers
    /// themselves, which is tamnd/rucc#925's argument applied to the kernel tree rather than to
    /// glibc.
    #[must_use]
    pub const fn kernel(&self) -> Option<Version> {
        self.kernel
    }

    /// Record which Linux release the kernel headers came out of.
    ///
    /// Infallible, and in particular it does not refuse a target that has no kernel headers. The
    /// property this type owes its callers is that [`Manifest::parse`] reads back what
    /// [`Manifest::render`] wrote, so the reader accepts every manifest a producer can build and a
    /// `kernel` line on a Windows sysroot is a bug in the producer rather than a corrupt file.
    pub const fn set_kernel(&mut self, version: Version) {
        self.kernel = Some(version);
    }

    /// Every input, in the order they were added.
    #[must_use]
    pub fn inputs(&self) -> &[Input] {
        &self.inputs
    }

    /// Record one input.
    pub fn push(&mut self, input: Input) {
        self.inputs.push(input);
    }

    /// Whether an artifact containing this whole sysroot can be published.
    ///
    /// One input under a licence that says no makes the answer no, which is the only reading of a
    /// licence wall that is worth anything.
    #[must_use]
    pub fn redistributable(&self) -> bool {
        self.inputs.iter().all(|input| input.licence.redistributable())
    }

    /// Every distinct source in the manifest, sorted.
    ///
    /// What a person asks first when two manifests differ, and what a licence notice is generated
    /// from.
    #[must_use]
    pub fn sources(&self) -> Vec<&str> {
        let mut sources: Vec<&str> =
            self.inputs.iter().map(|input| input.source.as_str()).collect();
        sources.sort_unstable();
        sources.dedup();
        sources
    }

    /// The manifest as text, sorted by path.
    ///
    /// The sort is what makes two runs comparable. A directory walk returns files in whatever order
    /// the filesystem keeps them, which differs between ext4 and APFS and sometimes between two
    /// runs on one of them, and a manifest that carried that order would report a difference
    /// between two identical sysroots.
    #[must_use]
    pub fn render(&self) -> String {
        let mut sorted = self.inputs.clone();
        sorted.sort();

        let mut text = String::new();
        text.push_str(HEADER);
        text.push('\n');
        text.push_str("target\t");
        text.push_str(&self.target.to_canonical_string());
        text.push('\n');
        if let Some(kernel) = self.kernel {
            text.push_str("kernel\t");
            text.push_str(&kernel.to_string());
            text.push('\n');
        }
        for input in &sorted {
            text.push_str(&input.path);
            text.push('\t');
            text.push_str(&input.source);
            text.push('\t');
            text.push_str(&input.url);
            text.push('\t');
            text.push_str(&input.sha256);
            text.push('\t');
            text.push_str(input.licence.as_str());
            text.push('\t');
            text.push_str(input.provenance.as_str());
            text.push('\n');
        }
        text
    }

    /// Read a manifest back.
    ///
    /// # Errors
    ///
    /// Returns which line was wrong and what was wrong with it. A manifest that fails to parse is
    /// a cache entry somebody has to decide about, and "invalid manifest" is not enough to decide
    /// with.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let mut lines = text.lines().enumerate().peekable();

        let (_, first) = lines.next().ok_or(ManifestError::NotAManifest)?;
        if first != HEADER {
            let Some(version) = first.strip_prefix("rucc sysroot manifest ") else {
                return Err(ManifestError::NotAManifest);
            };
            return Err(ManifestError::UnknownVersion(version.to_string()));
        }

        let (_, second) = lines.next().ok_or(ManifestError::NotAManifest)?;
        let spelling = second
            .strip_prefix("target\t")
            .ok_or_else(|| ManifestError::BadTarget(second.into()))?;
        let target = TargetTuple::from_str(spelling)
            .map_err(|_| ManifestError::BadTarget(spelling.to_string()))?;

        let mut manifest = Manifest::new(target);

        // The kernel line is read only here, immediately after the target, rather than wherever it
        // turns up. The render order is what makes two manifests comparable with `diff`, and a
        // reader that took the line anywhere would accept files that do not compare.
        if let Some(spelling) = lines.peek().and_then(|(_, line)| line.strip_prefix("kernel\t")) {
            let version = Version::parse(spelling)
                .ok_or_else(|| ManifestError::BadKernel(spelling.into()))?;
            manifest.set_kernel(version);
            lines.next();
        }

        for (index, line) in lines {
            if line.is_empty() {
                continue;
            }
            let number = index + 1;
            let fields: Vec<&str> = line.split('\t').collect();
            let [path, source, url, sha256, licence, provenance] = fields.as_slice() else {
                return Err(ManifestError::BadInput { line: number, fields: fields.len() });
            };
            if !is_sha256(sha256) {
                return Err(ManifestError::BadHash { line: number, found: (*sha256).to_string() });
            }
            for (value, field) in [(path, "path"), (source, "source"), (url, "url")] {
                if value.is_empty() {
                    return Err(ManifestError::EmptyField { line: number, field });
                }
            }
            manifest.push(Input {
                path: (*path).to_string(),
                source: (*source).to_string(),
                url: (*url).to_string(),
                sha256: (*sha256).to_string(),
                licence: licence.parse()?,
                provenance: provenance.parse()?,
            });
        }
        Ok(manifest)
    }
}

/// Whether this is sixty four lowercase hex characters.
///
/// Checked on the way in rather than assumed, because a manifest with a truncated hash in it is a
/// manifest that verifies nothing while looking like it does.
fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
