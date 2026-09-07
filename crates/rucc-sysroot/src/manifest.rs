//! What a produced sysroot carries: every input, where it came from, its hash and its licence.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.6 for the licence half and
//! `spec/cross-compile/02-the-goal.md` claim 5 for the rest.
//!
//! # Two jobs, one file
//!
//! Claim 5 asks for byte identical output from two hosts for the same target. Checking that over a
//! sysroot means comparing several thousand files, and the first thing anybody does when the
//! comparison fails is ask which file and where it came from. A manifest answers both, and comparing
//! two manifests is a diff of a few hundred lines rather than of a directory tree.
//!
//! The other job is the licence wall. Section 8.6 says the macOS SDK and the Windows SDK cannot be
//! redistributed, and the way that rule gets enforced rather than remembered is that every input
//! carries its licence and [`Manifest::redistributable`] is a function anything that publishes an
//! artifact can call. A rule in a document is a rule somebody breaks in eighteen months.
//!
//! # The format
//!
//! Tab separated lines, sorted by path, with a two line header. Not JSON, because the thing this is
//! optimized for is a person reading a diff between two of them, and not TOML, because it has no
//! nesting and a parser for it is thirty lines. Sorted because the order files come out of a
//! directory walk is a property of the filesystem, and a manifest whose line order depended on that
//! would report a difference between two identical sysroots.
//!
//! ```
//! use rucc_sysroot::{Input, Licence, Manifest};
//! use rucc_tuple::TargetTuple;
//!
//! let target: TargetTuple = "aarch64-linux-musl".parse().unwrap();
//! let mut manifest = Manifest::new(target);
//! manifest.push(Input {
//!     path: "include/generic/stdio.h".into(),
//!     source: "musl-1.2.5".into(),
//!     sha256: "0".repeat(64),
//!     licence: Licence::Mit,
//! });
//!
//! let text = manifest.render();
//! assert_eq!(Manifest::parse(&text).unwrap(), manifest);
//! assert!(manifest.redistributable());
//! ```

use std::fmt;
use std::str::FromStr;

use rucc_tuple::TargetTuple;

/// The licence an input arrives under.
///
/// The list is section 8.2's table with one variant per row, rather than a free text field, because
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
    /// Two of the seven answer false, and they are the two section 8.6 calls legal walls rather
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
            "mingw-permissive" => Ok(Licence::MingwPermissive),
            "apache-2.0" => Ok(Licence::Apache2),
            "apple-sdk" => Ok(Licence::AppleSdk),
            "microsoft-sdk" => Ok(Licence::MicrosoftSdk),
            other => Err(ManifestError::UnknownLicence(other.to_string())),
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
    /// The hash of the file, lowercase hex.
    pub sha256: String,
    /// What it may be done with.
    pub licence: Licence,
}

/// The record of one produced sysroot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    target: TargetTuple,
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
    /// A line did not have the four fields an input has.
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
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::NotAManifest => write!(f, "this does not start like a sysroot manifest"),
            ManifestError::UnknownVersion(v) => {
                write!(f, "manifest format version {v}, which this build does not read")
            }
            ManifestError::BadTarget(t) => write!(f, "`{t}` is not a target this understands"),
            ManifestError::BadInput { line, fields } => {
                write!(f, "line {line} has {fields} fields where an input has four")
            }
            ManifestError::BadHash { line, found } => {
                write!(f, "line {line} has `{found}` where a sha256 belongs")
            }
            ManifestError::UnknownLicence(l) => write!(f, "`{l}` is not a licence this knows"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// The first line of every manifest, which is also how one is recognized.
const HEADER: &str = "rucc sysroot manifest 1";

impl Manifest {
    /// An empty manifest for this target.
    #[must_use]
    pub const fn new(target: TargetTuple) -> Self {
        Manifest { target, inputs: Vec::new() }
    }

    /// The target this sysroot is for.
    #[must_use]
    pub const fn target(&self) -> TargetTuple {
        self.target
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
        for input in &sorted {
            text.push_str(&input.path);
            text.push('\t');
            text.push_str(&input.source);
            text.push('\t');
            text.push_str(&input.sha256);
            text.push('\t');
            text.push_str(input.licence.as_str());
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
        let mut lines = text.lines().enumerate();

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
        for (index, line) in lines {
            if line.is_empty() {
                continue;
            }
            let number = index + 1;
            let fields: Vec<&str> = line.split('\t').collect();
            let [path, source, sha256, licence] = fields.as_slice() else {
                return Err(ManifestError::BadInput { line: number, fields: fields.len() });
            };
            if !is_sha256(sha256) {
                return Err(ManifestError::BadHash { line: number, found: (*sha256).to_string() });
            }
            manifest.push(Input {
                path: (*path).to_string(),
                source: (*source).to_string(),
                sha256: (*sha256).to_string(),
                licence: licence.parse()?,
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
