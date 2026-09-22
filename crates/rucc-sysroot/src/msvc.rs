//! Microsoft's installer manifest, and the few packages in it an MSVC sysroot is made of.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4, which is the Microsoft half of
//! [`crate::Wall`].
//!
//! # What this is for
//!
//! The Windows SDK and the MSVC universal CRT are not ours to redistribute, so no release of this
//! compiler will ever pin an artifact for an MSVC target the way it pins one for mingw-w64. What
//! Microsoft does publish is an installer manifest that names every file the Visual Studio
//! installer would download, and a licence that lets a person who accepts it download those files.
//! That is the mechanism `cargo-xwin` uses and section 13.4 says we copy it. This module is the
//! reading half of that: given the two documents, which packages does a compiler need and which
//! files are those packages made of.
//!
//! It fetches nothing and it writes nothing. Everything here is a function of text the caller was
//! handed, which is the same rule the rest of this crate is held to.
//!
//! # The chain, and the one link in it that is not a hash
//!
//! There are three documents. The channel manifest, at a fixed `aka.ms` address, which names the
//! installer manifest. The installer manifest, which names every package and gives a sha256 for
//! every file in every one of them. And the files.
//!
//! Every file is verified against the installer manifest, so the interesting question is what
//! verifies the installer manifest. The channel gives a sha256 for it, and as of this writing that
//! hash is wrong: `aka.ms/vs/17/release/channel` says the manifest for 17.14.37710.0 is 30443537
//! bytes long and hashes to `6e470016...`, and the file served at the URL it names in the same
//! breath is 17954732 bytes and hashes to `f0a50ea1...`, from two different Microsoft regions on
//! two different days. Microsoft replaced the file and did not update the record.
//!
//! So the record is not a pin, it is a note, and treating it as a pin means a command that never
//! works. [`Channel`] carries what the channel said and leaves the decision to the caller, which is
//! the honest arrangement: what actually protects the install is the per file hash a level down,
//! and what the channel adds is only a check that the CDN served the index the channel described.
//! A caller that reports both hashes gives a person auditing the download the one thing that
//! matters, which is exactly which bytes they got.
//!
//! # What is chosen, and why it is so little
//!
//! A C compiler needs headers and import libraries and nothing else. No linker, no assembler, no
//! debugger, no redistributables, no spectre mitigated variants, no store or onecore flavours of
//! the desktop libraries, and no tools of any kind, because the tool is this compiler. That comes
//! to the CRT headers, one CRT library package per architecture, and six of the Windows SDK's
//! installers, out of a manifest with nineteen thousand packages in it.
//!
//! The newest version of each is taken rather than a pinned one. A pinned version would be a
//! promise about a file on somebody else's server, which section 13.8 already declines to make for
//! the sysroots we do publish, and Microsoft retires old versions from the manifest.

use std::collections::BTreeMap;
use std::fmt;

use rucc_tuple::{Arch, TargetTuple};

use crate::json::{JsonError, Reader};

/// One file Microsoft publishes, as the manifest describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// The name the manifest gives it, which for the SDK has a `Installers\` in front of it.
    pub name: String,
    /// Where to get it.
    pub url: String,
    /// What it must hash to, as sixty four lowercase hex characters.
    pub sha256: String,
    /// How many bytes it is, which is what lets a caller say the total before it starts.
    pub size: u64,
}

/// The architectures Microsoft ships a CRT for, spelled the way each document spells them.
///
/// Two spellings, because the package ids and the SDK installer names disagree about case and
/// about `arm64`. That is not a thing to normalise away: both are keys into somebody else's
/// document and a key is what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Chip {
    /// 32-bit x86.
    X86,
    /// x86-64.
    X64,
    /// 32-bit ARM.
    Arm,
    /// 64-bit ARM.
    Arm64,
}

impl Chip {
    /// Which chip a target is, or [`None`] for a target Microsoft ships nothing for.
    ///
    /// ARM64EC has no answer here on purpose. It is tier 4 in
    /// `spec/cross-compile/04-target-matrix.md`, nothing in this compiler emits code for it, and
    /// the manifest's ARM64EC packages are a few kilobytes of thunks rather than a C library.
    #[must_use]
    pub const fn of(target: TargetTuple) -> Option<Self> {
        match target.arch() {
            Arch::X86 => Some(Chip::X86),
            Arch::X86_64 => Some(Chip::X64),
            Arch::Arm => Some(Chip::Arm),
            Arch::Aarch64 => Some(Chip::Arm64),
            _ => None,
        }
    }

    /// How a Visual C++ package id spells it.
    #[must_use]
    pub const fn in_package(self) -> &'static str {
        match self {
            Chip::X86 => "x86",
            Chip::X64 => "x64",
            Chip::Arm => "arm",
            // The one that is not lower case, which is Microsoft's inconsistency and not ours.
            Chip::Arm64 => "ARM64",
        }
    }

    /// How a Windows SDK installer name spells it.
    #[must_use]
    pub const fn in_installer(self) -> &'static str {
        match self {
            Chip::X86 => "x86",
            Chip::X64 => "x64",
            Chip::Arm => "arm",
            Chip::Arm64 => "arm64",
        }
    }
}

/// What the channel manifest says, which is the entry point and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// The release as a person reads it, such as `17.14.41 (September 2026)`.
    pub release: String,
    /// The build, such as `17.14.37710.0`, which is what the installer manifest is versioned by.
    pub build: String,
    /// The installer manifest, as the channel describes it. See this module's note about the hash.
    pub manifest: Payload,
    /// Where Microsoft publishes the licence that permits this download, taken from the build
    /// tools product in the channel rather than written down here, so that the address a person is
    /// sent to is the one Microsoft is serving today.
    pub licence: String,
}

impl Channel {
    /// Read a channel manifest.
    ///
    /// # Errors
    ///
    /// When the document does not parse, when it has no installer manifest in it, and when the
    /// build tools product it takes the licence from is not there.
    pub fn parse(text: &str) -> Result<Self, MsvcError> {
        let mut release = String::new();
        let mut build = String::new();
        let mut manifest = None;
        let mut licence = String::new();

        let mut reader = Reader::new(text);
        reader.enter_object()?;
        while let Some(key) = reader.next_key()? {
            match &*key {
                "info" => {
                    reader.enter_object()?;
                    while let Some(field) = reader.next_key()? {
                        match &*field {
                            "productDisplayVersion" => release = reader.string()?.into_owned(),
                            "buildVersion" => build = reader.string()?.into_owned(),
                            _ => reader.skip()?,
                        }
                    }
                }
                "channelItems" => {
                    reader.enter_array()?;
                    while reader.next_item()? {
                        let item = channel_item(&mut reader)?;
                        if item.kind == "Manifest" {
                            manifest = item.payload;
                        } else if item.id == BUILD_TOOLS && !item.licence.is_empty() {
                            licence = item.licence;
                        }
                    }
                }
                _ => reader.skip()?,
            }
        }

        let manifest = manifest.ok_or(MsvcError::NoManifest)?;
        if licence.is_empty() {
            return Err(MsvcError::NoLicence);
        }
        Ok(Channel { release, build, manifest, licence })
    }
}

/// The product the licence is taken from, which is the one a person who wants a compiler and no
/// IDE would install.
const BUILD_TOOLS: &str = "Microsoft.VisualStudio.Product.BuildTools";

/// One entry of `channelItems`, reduced to the three things [`Channel::parse`] looks at.
struct ChannelItem {
    id: String,
    kind: String,
    payload: Option<Payload>,
    licence: String,
}

/// Read one `channelItems` entry.
fn channel_item(reader: &mut Reader<'_>) -> Result<ChannelItem, MsvcError> {
    let mut item = ChannelItem {
        id: String::new(),
        kind: String::new(),
        payload: None,
        licence: String::new(),
    };
    reader.enter_object()?;
    while let Some(field) = reader.next_key()? {
        match &*field {
            "id" => item.id = reader.string()?.into_owned(),
            "type" => item.kind = reader.string()?.into_owned(),
            "payloads" => {
                let mut all = payloads(reader)?;
                item.payload = (!all.is_empty()).then(|| all.remove(0));
            }
            // Every language says the same address, so the first one that has it wins rather than
            // the document being searched for a locale nothing here is in a position to choose.
            "localizedResources" => {
                reader.enter_array()?;
                while reader.next_item()? {
                    reader.enter_object()?;
                    while let Some(inner) = reader.next_key()? {
                        if inner == "license" && item.licence.is_empty() {
                            item.licence = reader.string()?.into_owned();
                        } else {
                            reader.skip()?;
                        }
                    }
                }
            }
            _ => reader.skip()?,
        }
    }
    Ok(item)
}

/// Read a `payloads` array.
fn payloads(reader: &mut Reader<'_>) -> Result<Vec<Payload>, MsvcError> {
    let mut all = Vec::new();
    reader.enter_array()?;
    while reader.next_item()? {
        let mut one =
            Payload { name: String::new(), url: String::new(), sha256: String::new(), size: 0 };
        reader.enter_object()?;
        while let Some(field) = reader.next_key()? {
            match &*field {
                "fileName" => one.name = reader.string()?.into_owned(),
                "url" => one.url = reader.string()?.into_owned(),
                // Lower cased here rather than wherever it is compared, because the comparison is
                // against a hash we computed and those come out lower case.
                "sha256" => one.sha256 = reader.string()?.to_ascii_lowercase(),
                "size" => one.size = reader.integer()?,
                _ => reader.skip()?,
            }
        }
        all.push(one);
    }
    Ok(all)
}

/// One file to download, and which package of Microsoft's it came out of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// The package id, which is what a provenance record names as the source.
    pub package: String,
    /// The package version, which is the version of that source.
    pub version: String,
    /// The file itself.
    pub payload: Payload,
}

/// Which files an MSVC sysroot for a set of architectures is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The Visual C++ release the CRT came from, such as `14.44.35220`.
    pub crt: String,
    /// The Windows SDK release, such as `10.0.26100.15`.
    pub sdk: String,
    /// Every file, sorted by package and then by name so that two runs agree about the order.
    pub files: Vec<Wanted>,
}

impl Selection {
    /// Choose what to download out of an installer manifest.
    ///
    /// `chips` is which architectures to get libraries for. The headers are shared, so a selection
    /// for four architectures is four library packages and one of everything else.
    ///
    /// # Errors
    ///
    /// When the document does not parse, when it has no Visual C++ or no Windows SDK in it, and
    /// when a package or an installer the selection needs is not among its files.
    pub fn parse(text: &str, chips: &[Chip]) -> Result<Self, MsvcError> {
        let found = collect(text)?;
        let crt = newest(found.keys().filter_map(|id| vc_release(id)))
            .ok_or(MsvcError::NothingFound("a Visual C++ CRT"))?;
        let sdk_id = newest_sdk(&found).ok_or(MsvcError::NothingFound("a Windows SDK"))?;

        let mut files = Vec::new();
        let headers = format!("Microsoft.VC.{crt}.CRT.Headers.base");
        let mut wanted = vec![headers.clone()];
        for chip in sorted(chips) {
            wanted.push(format!("Microsoft.VC.{crt}.CRT.{}.Desktop.base", chip.in_package()));
        }
        // The headers package's own version is what the CRT is reported as, not the last library
        // package's. They are two numbers of one release and the headers are the one to name.
        let mut crt_version = String::new();
        for id in wanted {
            let package = found.get(&id).ok_or_else(|| MsvcError::NoPackage(id.clone()))?;
            if id == headers {
                crt_version.clone_from(&package.version);
            }
            for payload in &package.payloads {
                files.push(Wanted {
                    package: id.clone(),
                    version: package.version.clone(),
                    payload: payload.clone(),
                });
            }
        }

        let sdk = found.get(&sdk_id).ok_or_else(|| MsvcError::NoPackage(sdk_id.clone()))?;
        for installer in installers(chips) {
            let payload = sdk
                .payloads
                .iter()
                .find(|payload| leaf(&payload.name) == installer)
                .ok_or_else(|| MsvcError::NoInstaller(installer.clone()))?;
            files.push(Wanted {
                package: sdk_id.clone(),
                version: sdk.version.clone(),
                payload: payload.clone(),
            });
        }

        files.sort_by(|a, b| (&a.package, &a.payload.name).cmp(&(&b.package, &b.payload.name)));
        Ok(Selection { crt: crt_version, sdk: sdk.version.clone(), files })
    }

    /// How many bytes the whole selection is, which is what a person is told before accepting.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.files.iter().map(|file| file.payload.size).sum()
    }
}

/// The Windows SDK installers a selection needs, in a stable order.
///
/// The x86 desktop headers are here whatever was asked for, because that installer is where the
/// headers that are not per architecture live and it is four times the size of the other two for
/// that reason. The universal CRT is one installer for every architecture. The store app headers
/// and libraries are here because a desktop program still includes `windows.h`, and the desktop
/// installers do not carry all of what that reaches.
fn installers(chips: &[Chip]) -> Vec<String> {
    let mut all = vec![
        "Universal CRT Headers Libraries and Sources-x86_en-us.msi".to_owned(),
        "Windows SDK Desktop Headers x86-x86_en-us.msi".to_owned(),
        "Windows SDK OnecoreUap Headers x86-x86_en-us.msi".to_owned(),
        "Windows SDK for Windows Store Apps Headers-x86_en-us.msi".to_owned(),
        "Windows SDK for Windows Store Apps Libs-x86_en-us.msi".to_owned(),
    ];
    for chip in sorted(chips) {
        let arch = chip.in_installer();
        all.push(format!("Windows SDK Desktop Headers {arch}-x86_en-us.msi"));
        all.push(format!("Windows SDK Desktop Libs {arch}-x86_en-us.msi"));
    }
    all.sort();
    all.dedup();
    all
}

/// The chips asked for, in one order and without repeats, so that a selection does not depend on
/// how the command line happened to be written.
fn sorted(chips: &[Chip]) -> Vec<Chip> {
    let mut all = chips.to_vec();
    all.sort_unstable();
    all.dedup();
    all
}

/// A package the manifest has and this module might want.
#[derive(Debug)]
struct Package {
    version: String,
    payloads: Vec<Payload>,
}

/// Walk the installer manifest and keep the packages whose ids could matter.
///
/// The document is eighteen megabytes and nineteen thousand packages, and the way this stays cheap
/// is that a package whose id is not interesting has its payloads skipped rather than read. That
/// works because the manifest writes `id` before `payloads`, and a manifest that stopped doing so
/// would be a manifest this refuses rather than one it quietly misreads.
fn collect(text: &str) -> Result<BTreeMap<String, Package>, MsvcError> {
    let mut found: BTreeMap<String, Package> = BTreeMap::new();
    let mut reader = Reader::new(text);
    reader.enter_object()?;
    while let Some(key) = reader.next_key()? {
        if key != "packages" {
            reader.skip()?;
            continue;
        }
        reader.enter_array()?;
        while reader.next_item()? {
            let mut id = String::new();
            let mut version = String::new();
            let mut keep = None;
            let mut listed = false;
            let mut read = false;
            reader.enter_object()?;
            while let Some(field) = reader.next_key()? {
                match &*field {
                    "id" => {
                        id = reader.string()?.into_owned();
                        keep = Some(interesting(&id));
                    }
                    "version" => version = reader.string()?.into_owned(),
                    "payloads" => {
                        listed = true;
                        read = keep == Some(true);
                        if read {
                            let all = payloads(&mut reader)?;
                            found
                                .entry(id.clone())
                                .or_insert(Package { version: version.clone(), payloads: all });
                        } else {
                            reader.skip()?;
                        }
                    }
                    _ => reader.skip()?,
                }
            }
            // A package with no files at all is nothing to complain about. A package that had
            // some, and that turned out to be one of ours only after they had been stepped over,
            // is a manifest laid out the other way round, and guessing is worse than saying so.
            if keep == Some(true) && listed && !read && !found.contains_key(&id) {
                return Err(MsvcError::OutOfOrder(id));
            }
            // The version is read after the payloads in no manifest Microsoft has written, but an
            // entry that ended up without one is a record with a hole in it rather than a package.
            if let Some(package) = found.get_mut(&id) {
                if package.version.is_empty() {
                    package.version.clone_from(&version);
                }
            }
        }
    }
    Ok(found)
}

/// Whether a package id is one of the two shapes this module chooses from.
///
/// Deliberately coarse. It is the filter that keeps the walk cheap, and narrowing it down to the
/// exact ids happens afterwards, where the newest release is already known.
fn interesting(id: &str) -> bool {
    (id.starts_with("Microsoft.VC.") && id.contains(".CRT.") && id.ends_with(".base"))
        || id.starts_with("Win10SDK_10.0.")
        || id.starts_with("Win11SDK_10.0.")
}

/// The `14.44.17.14` out of `Microsoft.VC.14.44.17.14.CRT.Headers.base`.
///
/// Four numbers, the Visual C++ release and the Visual Studio release it shipped with, and they
/// are what the newest is chosen by. The package's own `version` field is the fifth number as
/// well, and it is not what to sort on: two packages of one release can differ in it.
fn vc_release(id: &str) -> Option<&str> {
    let rest = id.strip_prefix("Microsoft.VC.")?;
    let at = rest.find(".CRT.")?;
    let release = &rest[..at];
    release
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(release)
}

/// The largest of a set of dotted number strings, compared number by number.
///
/// Not as text, because `10.0.9.0` is the larger string and the older release, which is the same
/// trap the Windows SDK search in the driver documents.
fn newest<'a>(all: impl Iterator<Item = &'a str>) -> Option<String> {
    all.max_by(|left, right| numbers(left).cmp(&numbers(right))).map(ToOwned::to_owned)
}

/// A dotted number string as the numbers it is, for comparing.
fn numbers(text: &str) -> Vec<u64> {
    text.split('.').map(|part| part.parse().unwrap_or(0)).collect()
}

/// The newest Windows SDK package id among the ones collected.
///
/// Both generations are candidates, because a manifest carries the Windows 10 kits beside the
/// Windows 11 ones and the newest of all of them is the one to take. They compare by the build in
/// the id and then by the package version, which is how two revisions of one build are ordered.
fn newest_sdk(found: &BTreeMap<String, Package>) -> Option<String> {
    found
        .iter()
        .filter(|(id, _)| id.starts_with("Win10SDK_10.0.") || id.starts_with("Win11SDK_10.0."))
        .max_by_key(|(id, package)| {
            let build = id.rsplit('.').next().and_then(|last| last.parse::<u64>().ok());
            (build.unwrap_or(0), numbers(&package.version))
        })
        .map(|(id, _)| id.clone())
}

/// A payload name without the Windows directory Microsoft puts in front of it.
fn leaf(name: &str) -> &str {
    name.rsplit('\\').next().unwrap_or(name)
}

/// Why a manifest could not be read or could not be chosen from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsvcError {
    /// The document did not parse.
    ///
    /// The reader behind this is not part of the interface, so what comes out of it is the two
    /// things a person needs rather than the reader's own type: what was expected and where.
    Json {
        /// The byte offset in the document.
        at: usize,
        /// What was expected there, in the words a person would use.
        wanted: &'static str,
    },
    /// The channel manifest has no installer manifest in it, which means it is not one.
    NoManifest,
    /// The channel manifest names no licence, and a download that cannot show one is a download
    /// that does not happen.
    NoLicence,
    /// The installer manifest has none of something there has to be one of.
    NothingFound(&'static str),
    /// A package the selection needs is not in the manifest.
    NoPackage(String),
    /// A Windows SDK installer the selection needs is not among the SDK package's files.
    NoInstaller(String),
    /// A package wrote its payloads before its id, which is a manifest laid out in a way this
    /// reader was written not to guess at.
    OutOfOrder(String),
}

impl From<JsonError> for MsvcError {
    fn from(why: JsonError) -> Self {
        MsvcError::Json { at: why.at, wanted: why.wanted }
    }
}

impl fmt::Display for MsvcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MsvcError::Json { at, wanted } => {
                write!(f, "this is not the document it was taken for: {wanted} at byte {at} of it")
            }
            MsvcError::NoManifest => {
                write!(f, "this channel names no installer manifest, so it is not a channel")
            }
            MsvcError::NoLicence => write!(
                f,
                "this channel names no licence for the build tools, and the download it describes \
                 is one nobody may make without reading one"
            ),
            MsvcError::NothingFound(what) => {
                write!(f, "this manifest has no {what} in it")
            }
            MsvcError::NoPackage(id) => {
                write!(
                    f,
                    "this manifest has no {id}, which is a package an MSVC sysroot is made of"
                )
            }
            MsvcError::NoInstaller(name) => write!(
                f,
                "the Windows SDK in this manifest has no {name} in it, which is an installer an \
                 MSVC sysroot is made of"
            ),
            MsvcError::OutOfOrder(id) => write!(
                f,
                "{id} lists its files before it says what it is, which this reader relies on the \
                 manifest not doing"
            ),
        }
    }
}

impl std::error::Error for MsvcError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel manifest cut down to the two entries that are read, with the real shape and the
    /// real addresses of the September 2026 release.
    const CHANNEL: &str = r#"{
      "manifestVersion": "1.1",
      "info": { "buildVersion": "17.14.37710.0", "productDisplayVersion": "17.14.41 (September 2026)" },
      "channelItems": [
        {
          "id": "Microsoft.VisualStudio.Manifests.VisualStudio",
          "version": "17.14.37710.0",
          "type": "Manifest",
          "payloads": [
            {
              "fileName": "VisualStudio.vsman",
              "sha256": "6E470016E4324C84C255FFD0BEB3767D17EC89CC8561E9409EE3E1F6D29400F5",
              "size": 30443537,
              "url": "https://download.visualstudio.microsoft.com/download/pr/bc92e2cb/VisualStudio.vsman"
            }
          ]
        },
        {
          "id": "Microsoft.VisualStudio.Product.BuildTools",
          "type": "ChannelProduct",
          "localizedResources": [
            { "language": "en-US", "license": "https://go.microsoft.com/fwlink/?LinkId=2179911" }
          ]
        }
      ]
    }"#;

    /// An installer manifest cut down to what is chosen and a little of what is not: an older
    /// Visual C++ release, an older kit, a language pack, and a package with no payload this
    /// wants.
    const MANIFEST: &str = r#"{
      "manifestVersion": "1.1",
      "packages": [
        { "id": "Microsoft.VC.14.29.16.11.CRT.Headers.base", "version": "14.29.30157", "type": "Vsix",
          "payloads": [ { "fileName": "old.vsix", "sha256": "aa", "size": 1, "url": "https://example.invalid/old" } ] },
        { "id": "Microsoft.VC.14.44.17.14.CRT.Headers.base", "version": "14.44.35220", "type": "Vsix",
          "payloads": [ { "fileName": "headers.vsix", "sha256": "B1", "size": 2128977, "url": "https://example.invalid/headers" } ] },
        { "id": "Microsoft.VC.14.44.17.14.CRT.Headers.Resources", "language": "de-DE", "version": "14.44.35220", "type": "Vsix",
          "payloads": [ { "fileName": "de.vsix", "sha256": "cc", "size": 3, "url": "https://example.invalid/de" } ] },
        { "id": "Microsoft.VC.14.44.17.14.CRT.x64.Desktop.base", "version": "14.44.35226", "type": "Vsix",
          "payloads": [ { "fileName": "x64.vsix", "sha256": "b2", "size": 51521199, "url": "https://example.invalid/x64" } ] },
        { "id": "Microsoft.VC.14.44.17.14.CRT.ARM64.Desktop.base", "version": "14.44.35226", "type": "Vsix",
          "payloads": [ { "fileName": "arm64.vsix", "sha256": "b3", "size": 49166761, "url": "https://example.invalid/arm64" } ] },
        { "id": "Microsoft.VC.14.44.17.14.CRT.x64.Desktop.spectre.base", "version": "14.44.35226", "type": "Vsix",
          "payloads": [ { "fileName": "spectre.vsix", "sha256": "dd", "size": 4, "url": "https://example.invalid/spectre" } ] },
        { "id": "Microsoft.VisualStudio.Component.Windows11SDK", "version": "17.14.35", "type": "Component",
          "payloads": [ { "fileName": "nothing.vsix", "sha256": "ee", "size": 5, "url": "https://example.invalid/nothing" } ] },
        { "id": "Win10SDK_10.0.19041", "version": "10.0.19041.4", "type": "Exe",
          "payloads": [ { "fileName": "Installers\\Windows SDK Desktop Headers x86-x86_en-us.msi", "sha256": "ff", "size": 6, "url": "https://example.invalid/old-sdk" } ] },
        { "id": "Win11SDK_10.0.26100", "version": "10.0.26100.15", "type": "Exe",
          "payloads": [
            { "fileName": "Installers\\Universal CRT Headers Libraries and Sources-x86_en-us.msi", "sha256": "c1", "size": 589824, "url": "https://example.invalid/ucrt" },
            { "fileName": "Installers\\Windows SDK Desktop Headers x86-x86_en-us.msi", "sha256": "c2", "size": 790528, "url": "https://example.invalid/hx86" },
            { "fileName": "Installers\\Windows SDK Desktop Headers x64-x86_en-us.msi", "sha256": "c3", "size": 450560, "url": "https://example.invalid/hx64" },
            { "fileName": "Installers\\Windows SDK Desktop Headers arm64-x86_en-us.msi", "sha256": "c4", "size": 446464, "url": "https://example.invalid/harm64" },
            { "fileName": "Installers\\Windows SDK Desktop Libs x86-x86_en-us.msi", "sha256": "c5", "size": 528384, "url": "https://example.invalid/lx86" },
            { "fileName": "Installers\\Windows SDK Desktop Libs x64-x86_en-us.msi", "sha256": "c6", "size": 528384, "url": "https://example.invalid/lx64" },
            { "fileName": "Installers\\Windows SDK Desktop Libs arm64-x86_en-us.msi", "sha256": "c7", "size": 528384, "url": "https://example.invalid/larm64" },
            { "fileName": "Installers\\Windows SDK OnecoreUap Headers x86-x86_en-us.msi", "sha256": "c8", "size": 495616, "url": "https://example.invalid/onecore" },
            { "fileName": "Installers\\Windows SDK for Windows Store Apps Headers-x86_en-us.msi", "sha256": "c9", "size": 1060864, "url": "https://example.invalid/store-h" },
            { "fileName": "Installers\\Windows SDK for Windows Store Apps Libs-x86_en-us.msi", "sha256": "ca", "size": 528384, "url": "https://example.invalid/store-l" },
            { "fileName": "Installers\\Windows SDK Desktop Tools x64-x86_en-us.msi", "sha256": "cb", "size": 475136, "url": "https://example.invalid/tools" },
            { "fileName": "0f1a2b3c.cab", "sha256": "cc", "size": 9999, "url": "https://example.invalid/cab" }
          ] }
      ]
    }"#;

    fn target(tuple: &str) -> TargetTuple {
        tuple.parse().expect("a target this understands")
    }

    #[test]
    fn a_channel_says_the_release_the_manifest_and_where_the_licence_is() {
        let channel = Channel::parse(CHANNEL).expect("a channel");
        assert_eq!(channel.release, "17.14.41 (September 2026)");
        assert_eq!(channel.build, "17.14.37710.0");
        assert_eq!(channel.manifest.name, "VisualStudio.vsman");
        assert_eq!(channel.manifest.size, 30_443_537);
        // Lower cased on the way in, because it is compared against a hash we computed.
        assert!(channel.manifest.sha256.starts_with("6e470016"), "{}", channel.manifest.sha256);
        assert_eq!(channel.licence, "https://go.microsoft.com/fwlink/?LinkId=2179911");
    }

    #[test]
    fn a_channel_with_no_manifest_or_no_licence_in_it_says_which() {
        let without = CHANNEL.replace("\"type\": \"Manifest\"", "\"type\": \"Bootstrapper\"");
        assert_eq!(Channel::parse(&without).expect_err("no manifest"), MsvcError::NoManifest);

        let without = CHANNEL.replace("Microsoft.VisualStudio.Product.BuildTools", "Other.Product");
        assert_eq!(Channel::parse(&without).expect_err("no licence"), MsvcError::NoLicence);
    }

    #[test]
    fn the_newest_visual_cpp_and_the_newest_kit_are_the_ones_chosen() {
        let chosen = Selection::parse(MANIFEST, &[Chip::X64]).expect("a selection");
        assert_eq!(chosen.crt, "14.44.35220");
        assert_eq!(chosen.sdk, "10.0.26100.15");
        let packages: Vec<&str> = chosen.files.iter().map(|file| file.package.as_str()).collect();
        assert!(!packages.contains(&"Microsoft.VC.14.29.16.11.CRT.Headers.base"), "{packages:?}");
        assert!(!packages.contains(&"Win10SDK_10.0.19041"), "{packages:?}");
    }

    #[test]
    fn a_selection_is_the_headers_one_library_package_per_chip_and_the_installers() {
        let one = Selection::parse(MANIFEST, &[Chip::X64]).expect("a selection");
        assert_eq!(one.files.len(), 1 + 1 + 7);

        let two = Selection::parse(MANIFEST, &[Chip::X64, Chip::Arm64]).expect("a selection");
        // One more library package and two more installers, and the headers are still one copy.
        assert_eq!(two.files.len(), one.files.len() + 3);
        assert!(two.size() > one.size());

        // The order is the same however the command line was written, and nothing appears twice.
        let again =
            Selection::parse(MANIFEST, &[Chip::Arm64, Chip::X64, Chip::X64]).expect("a selection");
        assert_eq!(again, two);
    }

    #[test]
    fn nothing_that_is_not_a_header_or_a_library_is_chosen() {
        let chosen = Selection::parse(MANIFEST, &[Chip::X64, Chip::Arm64]).expect("a selection");
        let names: Vec<&str> = chosen.files.iter().map(|file| leaf(&file.payload.name)).collect();
        for unwanted in ["spectre.vsix", "de.vsix", "nothing.vsix"] {
            assert!(!names.contains(&unwanted), "{unwanted} is in {names:?}");
        }
        // The tools are not a compiler's business, and the cabs are not chosen here because the
        // installer is what says which of them it needs.
        assert!(!names.iter().any(|name| name.contains("Tools")), "{names:?}");
        assert!(!names.iter().any(|name| name.ends_with(".cab")), "{names:?}");
    }

    #[test]
    fn a_missing_package_or_installer_says_which_one_by_name() {
        let without = MANIFEST.replace("Microsoft.VC.14.44.17.14.CRT.x64.Desktop.base", "Other");
        let why = Selection::parse(&without, &[Chip::X64]).expect_err("a refusal");
        assert_eq!(
            why,
            MsvcError::NoPackage("Microsoft.VC.14.44.17.14.CRT.x64.Desktop.base".into())
        );

        let without =
            MANIFEST.replace("Windows SDK Desktop Libs x64", "Windows SDK Desktop Libs mips");
        let why = Selection::parse(&without, &[Chip::X64]).expect_err("a refusal");
        assert_eq!(
            why,
            MsvcError::NoInstaller("Windows SDK Desktop Libs x64-x86_en-us.msi".into())
        );

        let empty = r#"{ "packages": [] }"#;
        assert_eq!(
            Selection::parse(empty, &[Chip::X64]).expect_err("a refusal"),
            MsvcError::NothingFound("a Visual C++ CRT")
        );
    }

    #[test]
    fn the_windows_path_in_an_installer_name_survives_being_read() {
        let chosen = Selection::parse(MANIFEST, &[Chip::X64]).expect("a selection");
        let ucrt = chosen
            .files
            .iter()
            .find(|file| leaf(&file.payload.name).starts_with("Universal CRT"))
            .expect("the universal CRT");
        assert_eq!(
            ucrt.payload.name,
            r"Installers\Universal CRT Headers Libraries and Sources-x86_en-us.msi"
        );
        assert_eq!(ucrt.version, "10.0.26100.15");
    }

    #[test]
    fn a_target_maps_to_the_chip_microsoft_spells_two_ways() {
        assert_eq!(Chip::of(target("x86_64-windows-msvc")), Some(Chip::X64));
        assert_eq!(Chip::of(target("aarch64-windows-msvc")), Some(Chip::Arm64));
        assert_eq!(Chip::of(target("i686-windows-msvc")), Some(Chip::X86));
        // Tier 4 and nothing emits code for it, so there is nothing to fetch a library for.
        assert_eq!(Chip::of(target("arm64ec-windows-msvc")), None);
        assert_eq!(Chip::of(target("riscv64-linux-gnu")), None);

        assert_eq!(Chip::Arm64.in_package(), "ARM64");
        assert_eq!(Chip::Arm64.in_installer(), "arm64");
    }

    #[test]
    fn a_dotted_version_is_compared_as_numbers_and_not_as_text() {
        // The trap the driver's own Windows SDK search documents: the larger string is the older
        // release.
        assert_eq!(
            newest(["10.0.9.0", "10.0.22000.0"].into_iter()).as_deref(),
            Some("10.0.22000.0")
        );
        assert_eq!(vc_release("Microsoft.VC.14.44.17.14.CRT.Headers.base"), Some("14.44.17.14"));
        assert_eq!(vc_release("Microsoft.VC.Runtimes.x64.base"), None);
    }

    #[test]
    fn a_package_that_lists_its_files_before_it_says_what_it_is_is_refused() {
        let backwards = r#"{ "packages": [
          { "payloads": [ { "fileName": "a", "sha256": "b", "size": 1, "url": "c" } ],
            "id": "Microsoft.VC.14.44.17.14.CRT.Headers.base", "version": "14.44.35220" } ] }"#;
        let why = Selection::parse(backwards, &[Chip::X64]).expect_err("a refusal");
        assert_eq!(
            why,
            MsvcError::OutOfOrder("Microsoft.VC.14.44.17.14.CRT.Headers.base".to_owned())
        );
    }
}
