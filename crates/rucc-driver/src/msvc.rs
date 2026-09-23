//! Getting the Windows SDK and the MSVC CRT onto this machine, with the licence said out loud and
//! accepted first.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4. Neither of those two things is
//! ours to redistribute, and neither ever will be, so there is no artifact a release of this
//! compiler pins for an MSVC target and `rucc --fetch` of one says so. What Microsoft does publish
//! is a manifest naming every file its own installer would fetch, and a licence that lets a person
//! who accepts it fetch them, which is the mechanism `cargo-xwin` uses and the one copied here.
//!
//! [`rucc_sysroot::msvc`] is the reading half: it takes the two documents and says which files a
//! compiler needs out of the nineteen thousand packages in them. This is the half that moves bytes.
//! It prints the licence, refuses to go on without explicit acceptance, downloads the selection into
//! the cache with every file held against the hash the manifest gives for it, and then lays the
//! downloads out as the tree `--sysroot` reads. [`tree`] is the mapping from a file in a package to
//! its place in that tree, and [`rucc_unpack`] is the four readers a download is four formats deep
//! of.
//!
//! # Why the tree is per target
//!
//! Because the record of it is. `spec/cross-compile/13-distribution.md` section 13.5 asks for a
//! manifest saying where every file came from and what may be done with it, [`Manifest`] is that
//! record, and it carries one target. A tree that served three architectures would carry a record
//! that named one of them and said nothing about the other two.
//!
//! What that costs is the headers, which are the same for every architecture and are written again
//! under each target that is fetched. That is 78 MB of the 330 MB a target comes to, measured on the
//! September 2026 kit, and it is only paid by somebody who fetched more than one architecture on one
//! machine. The libraries, which are the larger half, were never shared.
//!
//! # The two downloads with no hash behind them
//!
//! The channel manifest is the root of the chain, so nothing above it could name its hash. The
//! installer manifest has a hash published for it in the channel and that hash does not match the
//! file served at the URL the channel names in the same breath, which was measured rather than
//! assumed and is written down in section 13.4. Both go through [`crate::fetch::trusted`], which
//! carries that weaker claim in its name. Every file after them goes through [`crate::fetch::fetch`]
//! with the hash the installer manifest gives for it, and those hashes are exact.
//!
//! # Why the manifests are fetched before the licence is accepted
//!
//! Because the licence is in them. The channel manifest is where the link to the Build Tools
//! licence comes from, so a run that printed a licence without fetching it would be printing a URL
//! this compiler had made up and kept up to date by hand. The two documents are Microsoft's
//! description of what it publishes rather than the things the licence is about, and what the
//! acceptance guards is the download of the files themselves, which is what happens after it.

pub mod tree;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rucc_sysroot::msvc::{Channel, Chip, Selection, Wanted};
use rucc_sysroot::{Input, Licence, Manifest, Provenance, Sysroot, sha256};
use rucc_tuple::TargetTuple;
use rucc_unpack::cab::File as CabFile;
use rucc_unpack::{Cab, Cfb, Msi, Zip, under};

use crate::fetch;
use crate::{CliError, err};

/// The channel manifest, which is where every run starts.
///
/// 17 is the Visual Studio version and `release` is the channel, which together are the current
/// shipping Build Tools. It is a redirector rather than a storage URL on purpose, so that it keeps
/// naming the current one as builds come and go, which is also why nothing here pins a build.
pub const CHANNEL: &str = "https://aka.ms/vs/17/release/channel";

/// Where a run keeps what it downloaded.
///
/// Under the cache like everything else, and under the build rather than beside it, because two
/// builds of the Visual Studio installer name different files and a person who fetched one and then
/// the other should have both rather than a directory that is half of each.
fn downloads(cache: &Path, build: &str) -> PathBuf {
    cache.join("downloads").join("msvc").join(build)
}

/// The file name a payload is stored as.
///
/// The SDK's payloads are named `Installers\Windows SDK Desktop Headers x64-x86_en-us.msi`, with a
/// backslash in them, because the manifest names them the way the installer lays them out on a
/// Windows machine. A backslash is an ordinary character in a file name on a Unix, so writing that
/// name to disk unchanged would give one file with a slash-looking name here and a directory there,
/// and the two hosts would disagree about what is in the cache. So the last component is taken, by
/// either separator, and the directory it sat in is dropped.
fn stored_as(name: &str) -> &str {
    name.rsplit(['\\', '/']).next().unwrap_or(name)
}

/// A size a person can read, which is what a number this large is for here.
///
/// The number is Microsoft's rather than a count of bytes that arrived, and for the CRT half of the
/// selection it is a little over: the headers package is served 12,754 bytes smaller than the
/// manifest says and the x86-64 one 745,102 smaller, while every SDK payload sampled is served at
/// exactly its declared size, and all of their hashes match. Section 13.4 records that. It is a
/// figure to tell somebody what they are about to download and nothing is held against it, so being
/// a per cent over on the CRT half of the selection is not worth a second source.
fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Fetch the SDK for `target`, having printed the licence and been told it is accepted.
///
/// The exit code, since this is what an action runs and there is nothing above it to return an
/// error to. A run that has not been given the acceptance prints the licence and what would be
/// downloaded and exits non-zero, because it did not do what it was asked to do.
pub fn fetch_msvc_sdk(target: TargetTuple, accepted: bool, cache: &Path) -> i32 {
    let tuple = target.to_canonical_string();
    match run(target, &tuple, accepted, cache) {
        Ok(code) => code,
        Err(why) => crate::complain(why),
    }
}

/// The same, as something that can fail in the ordinary way.
fn run(target: TargetTuple, tuple: &str, accepted: bool, cache: &Path) -> Result<i32, CliError> {
    let say = |line: &str| println!("rucc: {tuple}: {line}");

    // Which targets this is for is `Wall::of` rather than a second list of the ones it covers, the
    // same as everywhere else the two walls come up. A mingw-w64 target is refused here and is not
    // an oversight: its headers are ours, they are in the archive or a release pins them, and
    // `--fetch` is the command that gets them.
    if rucc_sysroot::Wall::of(target) != Some(rucc_sysroot::Wall::Microsoft) {
        return Err(err(format!(
            "--fetch-msvc-sdk gets what is behind Microsoft's licence wall, and {tuple} is not \
             behind it, so there is nothing here to get for it. `rucc --fetch {tuple}` is the \
             command that gets a sysroot this release pins"
        )));
    }
    let Some(chip) = Chip::of(target) else {
        return Err(err(format!(
            "--fetch-msvc-sdk {tuple}: Microsoft publishes the SDK for x86, x86-64, arm and \
             arm64, and {tuple} is none of those, so there is nothing in the manifest to get for it"
        )));
    };

    let dir = cache.join("downloads").join("msvc");
    let channel = dir.join("channel.json");
    // Always downloaded rather than read from the cache. It is the pointer to the current build, so
    // a cached copy is an answer to a question about the day it was fetched.
    fetch::trusted(CHANNEL, &channel)?;
    let text = read(&channel)?;
    let channel = Channel::parse(&text)
        .map_err(|why| err(format!("{CHANNEL} is not a channel manifest: {why}")))?;
    say(&format!("Visual Studio {}, build {}", channel.release, channel.build));

    let dir = downloads(cache, &channel.build);
    let manifest = dir.join(stored_as(&channel.manifest.name));
    // Named by the build, so a second run for another architecture reads the one already here
    // rather than moving eighteen megabytes again. A copy that does not parse is a run that was
    // interrupted, and it is downloaded again once rather than reported, because there is no hash
    // to tell a truncated file from a Microsoft that changed its mind.
    let mut text = if manifest.exists() { read(&manifest).ok() } else { None };
    if text.as_deref().and_then(|text| Selection::parse(text, &[chip]).ok()).is_none() {
        fetch::trusted(&channel.manifest.url, &manifest)?;
        text = Some(read(&manifest)?);
    }
    let text = text.unwrap_or_default();
    let chosen = Selection::parse(&text, &[chip]).map_err(|why| {
        err(format!("{} is not an installer manifest: {why}", channel.manifest.url))
    })?;
    say(&format!("MSVC CRT {} and Windows SDK {}", chosen.crt, chosen.sdk));

    if !accepted {
        refuse(&channel.licence, &chosen, tuple);
        return Ok(1);
    }

    say(&format!("the licence at {} was accepted on the command line", channel.licence));
    let mut had = 0;
    for file in &chosen.files {
        let at = dir.join(&file.payload.sha256[..12]).join(stored_as(&file.payload.name));
        match fetch::fetch(&file.payload.url, &file.payload.sha256, &at)? {
            fetch::Fetched::AlreadyThere => had += 1,
            fetch::Fetched::Downloaded(by) => {
                say(&format!(
                    "{} ({}) with {}",
                    stored_as(&file.payload.name),
                    mb(file.payload.size),
                    by.program()
                ));
            }
        }
    }
    if had > 0 {
        say(&format!("{had} of the {} files were already here", chosen.files.len()));
    }
    say(&format!(
        "{} files totalling {} are at {}",
        chosen.files.len(),
        mb(chosen.size()),
        dir.display()
    ));

    let tree = unpack(target, chip, &chosen, &dir, cache, &say)?;
    say(&format!("compile for {tuple} with --sysroot={}", tree.display()));
    Ok(0)
}

/// Lay the downloaded packages out as the tree `--sysroot` reads, and record what went into it.
///
/// The record is written last and is what says the tree is finished, so a run that was interrupted
/// leaves a directory with no manifest in it and the next run lays it out again from the top. That
/// is cheaper than it sounds, because the downloads are held and nothing is fetched twice, and it is
/// the only test available: the files in the tree have no hashes published for them, only the
/// packages they came out of do, so there is nothing to hold a half written tree against.
fn unpack(
    target: TargetTuple,
    chip: Chip,
    chosen: &Selection,
    from: &Path,
    cache: &Path,
    say: &dyn Fn(&str),
) -> Result<PathBuf, CliError> {
    let version = format!("{}-{}", chosen.crt, chosen.sdk);
    let root = cache.join("msvc").join(version).join(target.to_canonical_string());
    let record = Sysroot::at(root.clone(), target).manifest_path();
    if std::fs::read_to_string(&record).is_ok_and(|text| Manifest::parse(&text).is_ok()) {
        say(&format!("the tree at {} was laid out already", root.display()));
        return Ok(root);
    }
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|why| err(format!("{}: {why}", root.display())))?;
    }

    let mut manifest = Manifest::new(target);
    for file in &chosen.files {
        let at = from.join(&file.payload.sha256[..12]).join(stored_as(&file.payload.name));
        let bytes = slurp(&at)?;
        if stored_as(&file.payload.name).to_ascii_lowercase().ends_with(".msi") {
            from_msi(&bytes, chip, &root, file, chosen, from, &mut manifest)?;
        } else {
            from_vsix(&bytes, chip, &root, file, &mut manifest)?;
        }
    }

    let written = manifest.inputs().len();
    let alike = aliases(&root)?;
    std::fs::create_dir_all(&root).map_err(|why| err(format!("{}: {why}", root.display())))?;
    std::fs::write(&record, manifest.render())
        .map_err(|why| err(format!("{}: {why}", record.display())))?;
    say(&format!("{written} files and {alike} lowercase names are at {}", root.display()));
    Ok(root)
}

/// Lay out the members of one Visual C++ package.
fn from_vsix(
    bytes: &[u8],
    chip: Chip,
    root: &Path,
    file: &Wanted,
    manifest: &mut Manifest,
) -> Result<(), CliError> {
    let name = stored_as(&file.payload.name);
    let zip = Zip::read(bytes).map_err(|why| err(format!("{name}: {why}")))?;
    for member in zip.members() {
        if member.is_dir() {
            continue;
        }
        let Some(at) = tree::crt(&member.name, chip) else {
            continue;
        };
        let body = zip.contents(member).map_err(|why| err(format!("{name}: {why}")))?;
        put(root, &at, &body, file, &file.payload.url, manifest)?;
    }
    Ok(())
}

/// Lay out the files one Windows SDK installer describes, fetching the cabinets they are in.
///
/// An installer holds no bytes of its own, so this is two steps rather than one: read the tables to
/// find out which cabinet every file it describes is in and what that cabinet calls it, then get the
/// cabinets that hold something this target wants. Most of them hold nothing it wants. The store
/// apps headers installer names three cabinets and the universal CRT one names eleven, and which of
/// those are worth 484 MB of downloading is a question only the tables can answer, which is why
/// [`Selection::cabs`] carries all of them and this picks.
fn from_msi(
    bytes: &[u8],
    chip: Chip,
    root: &Path,
    file: &Wanted,
    chosen: &Selection,
    from: &Path,
    manifest: &mut Manifest,
) -> Result<(), CliError> {
    let name = stored_as(&file.payload.name);
    let compound = Cfb::read(bytes).map_err(|why| err(format!("{name}: {why}")))?;
    let installer = Msi::read(&compound).map_err(|why| err(format!("{name}: {why}")))?;
    let describes = installer.payload().map_err(|why| err(format!("{name}: {why}")))?;

    let mut wanted: BTreeMap<&str, Vec<(&str, String)>> = BTreeMap::new();
    for payload in &describes {
        let Some(at) = tree::sdk(&payload.directory, &payload.name, chip) else {
            continue;
        };
        // A cabinet named with a `#` in front of it is a stream inside the installer rather than a
        // file beside it. No installer in the selection uses one, which was measured rather than
        // assumed, and a kit that started to would be losing headers quietly if this skipped it.
        if payload.cabinet.starts_with('#') || payload.cabinet.is_empty() {
            return Err(err(format!(
                "{name} keeps {} in {}, which is inside the installer rather than in a cabinet \
                 beside it, and this does not read those yet",
                payload.name,
                if payload.cabinet.is_empty() { "the media" } else { &payload.cabinet }
            )));
        }
        wanted.entry(&payload.cabinet).or_default().push((&payload.key, at));
    }

    for (cabinet, files) in wanted {
        let Some(published) = chosen.cab(cabinet) else {
            return Err(err(format!(
                "{name} says its files are in {cabinet}, which is not a file this Windows SDK \
                 publishes, so there is nowhere to get them from"
            )));
        };
        let at =
            from.join(&published.payload.sha256[..12]).join(stored_as(&published.payload.name));
        fetch::fetch(&published.payload.url, &published.payload.sha256, &at)?;
        let bytes = slurp(&at)?;
        let cab = Cab::read(&bytes).map_err(|why| err(format!("{}: {why}", at.display())))?;
        spill(&cab, &files, root, file, &published.payload.url, manifest)
            .map_err(|why| err(format!("{}: {why}", at.display())))?;
    }
    Ok(())
}

/// Write the files a cabinet holds, given what each one is called there and where it goes.
///
/// A folder in a cabinet is one compressed stream with the files laid end to end inside it, so it is
/// decompressed once and sliced rather than once per file. The SDK puts thousands of headers in a
/// folder, and asking for them one at a time would decompress the same megabytes thousands of times.
fn spill(
    cab: &Cab<'_>,
    files: &[(&str, String)],
    root: &Path,
    file: &Wanted,
    url: &str,
    manifest: &mut Manifest,
) -> Result<(), CliError> {
    let places: BTreeMap<&str, &str> = files.iter().map(|(key, at)| (*key, at.as_str())).collect();
    let mut folders: BTreeMap<usize, Vec<&CabFile>> = BTreeMap::new();
    for member in cab.files() {
        if places.contains_key(member.name.as_str()) {
            folders.entry(member.folder).or_default().push(member);
        }
    }
    for members in folders.into_values() {
        let folder = cab.folder(members[0]).map_err(|why| err(why.to_string()))?;
        for member in members {
            let at = usize::try_from(member.at).unwrap_or(usize::MAX);
            let size = usize::try_from(member.size).unwrap_or(usize::MAX);
            let body =
                at.checked_add(size).and_then(|end| folder.get(at..end)).ok_or_else(|| {
                    err(format!("{} is not where this cabinet's folder says it is", member.name))
                })?;
            put(root, places[member.name.as_str()], body, file, url, manifest)?;
        }
    }
    Ok(())
}

/// Write one file into the tree and record where it came from.
///
/// [`rucc_unpack::under`] is what decides whether the name may be written at all. The last component
/// of every one of these is a string out of somebody else's archive, so it goes through the same
/// check an unpacker owes its caller rather than being trusted because the directory in front of it
/// was ours.
fn put(
    root: &Path,
    at: &str,
    body: &[u8],
    file: &Wanted,
    url: &str,
    manifest: &mut Manifest,
) -> Result<(), CliError> {
    let to = under(root, at).ok_or_else(|| {
        err(format!("{at} is a name out of a Microsoft package that will not be written"))
    })?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|why| err(format!("{}: {why}", parent.display())))?;
    }
    std::fs::write(&to, body).map_err(|why| err(format!("{}: {why}", to.display())))?;
    manifest.push(Input {
        path: at.to_owned(),
        source: format!("{} {}", file.package, file.version),
        url: url.to_owned(),
        sha256: sha256::hex(body),
        licence: Licence::MicrosoftSdk,
        provenance: Provenance::Fetched,
    });
    Ok(())
}

/// Put a lowercase name beside every file and directory in the tree that has a capital in it.
///
/// See [`tree::lowercase`] for what this is for. They are not recorded in the manifest, because a
/// manifest says where files came from and these came from here.
///
/// An entry that is already there is left alone rather than reported. That happens on a host whose
/// filesystem answers to either spelling, where creating the link finds the file itself in the way,
/// and a compiler that refused to finish on such a host would be refusing over a tree that is
/// already correct.
#[cfg(unix)]
fn aliases(root: &Path) -> Result<usize, CliError> {
    let mut todo = vec![root.to_path_buf()];
    let mut made = 0;
    while let Some(dir) = todo.pop() {
        let mut here = Vec::new();
        let listing =
            std::fs::read_dir(&dir).map_err(|why| err(format!("{}: {why}", dir.display())))?;
        for entry in listing {
            let entry = entry.map_err(|why| err(format!("{}: {why}", dir.display())))?;
            // Not followed, so the links made below are not descended into on the way back up.
            let kind = entry.file_type().map_err(|why| err(format!("{}: {why}", dir.display())))?;
            if kind.is_dir() {
                todo.push(entry.path());
            }
            here.push(entry.file_name());
        }
        for name in here {
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(lower) = tree::lowercase(name) else {
                continue;
            };
            let link = dir.join(&lower);
            match std::os::unix::fs::symlink(name, &link) {
                Ok(()) => made += 1,
                Err(why) if why.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(why) => return Err(err(format!("{}: {why}", link.display()))),
            }
        }
    }
    Ok(made)
}

/// The same on a host where the question does not arise.
///
/// Windows filesystems are not case sensitive, so `windows.h` already finds `Windows.h` and a second
/// name for it would be a second file rather than a second spelling.
#[cfg(not(unix))]
fn aliases(_root: &Path) -> Result<usize, CliError> {
    Ok(0)
}

/// Read a file that was downloaded, saying which one when it cannot be read.
fn slurp(at: &Path) -> Result<Vec<u8>, CliError> {
    std::fs::read(at).map_err(|why| err(format!("{}: {why}", at.display())))
}

/// What a run that has not been given the acceptance prints.
///
/// The licence first and the list under it, because the list is what the licence is about, and the
/// exact words to type last, since that is what somebody who has read the licence wants next. It
/// goes to the output rather than to the error stream: it is what the command was asked for when it
/// was run without the acceptance, and a person reads it.
fn refuse(licence: &str, chosen: &Selection, tuple: &str) {
    println!(
        "The Windows SDK and the MSVC CRT are not ours to give you. Microsoft publishes them under\n\
         the Visual Studio Build Tools licence, which is at\n\
         \n    {licence}\n\
         \nand which you have to read and accept yourself. This compiler will not accept it for you\n\
         and will not download anything until you have said that you did.\n"
    );
    println!(
        "What would be downloaded for {tuple}, {} totalling {}:",
        files(chosen.files.len()),
        mb(chosen.size())
    );
    for file in &chosen.files {
        println!("  {:>9}  {}", mb(file.payload.size), stored_as(&file.payload.name));
    }
    println!(
        "\nThe Windows SDK installers in that list hold no bytes of their own. Each one is a small\n\
         database naming the cabinets its headers and libraries are in, and those cabinets are\n\
         separate files that this total does not count, because which of them a target needs is a\n\
         question only the installers can answer."
    );
    println!(
        "\nIf you accept that licence, run this again with --accept-licence on the command line.\n\
         If you would rather not, build for the mingw-w64 environment instead, which is fully\n\
         redistributable and needs nothing installed."
    );
}

/// `1 file` and `14 files`, because a message that says `1 files` was written by a program.
fn files(count: usize) -> String {
    if count == 1 { "1 file".to_owned() } else { format!("{count} files") }
}

/// Read a document that was just downloaded, saying which one when it cannot be read.
fn read(at: &Path) -> Result<String, CliError> {
    std::fs::read_to_string(at).map_err(|why| err(format!("{}: {why}", at.display())))
}

#[cfg(test)]
mod tests {
    use super::{aliases, downloads, files, mb, put, stored_as};
    use rucc_sysroot::{Manifest, Provenance};
    use std::path::{Path, PathBuf};

    /// A directory of this test's own, since these write files.
    fn scratch(name: &str) -> PathBuf {
        let at = std::env::temp_dir().join(format!("rucc-msvc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).expect("a directory to work in");
        at
    }

    /// One file out of a package, named the way the selection names one.
    fn wanted(package: &str) -> rucc_sysroot::msvc::Wanted {
        rucc_sysroot::msvc::Wanted {
            package: package.to_owned(),
            version: "10.0.26100.15".to_owned(),
            payload: rucc_sysroot::msvc::Payload {
                name: format!(r"Installers\{package}.msi"),
                url: "https://example.invalid/thing".to_owned(),
                sha256: "ab".repeat(32),
                size: 4,
            },
        }
    }

    #[test]
    fn a_file_is_written_where_the_tree_says_and_recorded_as_microsofts() {
        let root = scratch("put");
        let mut manifest = Manifest::new("x86_64-windows-msvc".parse().expect("a tuple"));
        let from = wanted("Win11SDK_10.0.26100");
        put(
            &root,
            "sdk/include/um/windows.h",
            b"#pragma once\n",
            &from,
            "https://ms/cab",
            &mut manifest,
        )
        .expect("a file written");
        assert_eq!(
            std::fs::read(root.join("sdk/include/um/windows.h")).expect("what was written"),
            b"#pragma once\n"
        );
        let input = &manifest.inputs()[0];
        assert_eq!(input.path, "sdk/include/um/windows.h");
        assert_eq!(input.source, "Win11SDK_10.0.26100 10.0.26100.15");
        // The URL is the cabinet the bytes came out of rather than the installer that named it,
        // because that is where they were.
        assert_eq!(input.url, "https://ms/cab");
        assert_eq!(input.sha256, rucc_sysroot::sha256::hex(b"#pragma once\n"));
        // Not redistributable, which is the whole reason this command exists.
        assert!(!input.licence.redistributable());
        assert_eq!(input.provenance, Provenance::Fetched);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_name_out_of_a_package_that_would_leave_the_tree_is_refused() {
        let root = scratch("escape");
        let mut manifest = Manifest::new("x86_64-windows-msvc".parse().expect("a tuple"));
        let from = wanted("Win11SDK_10.0.26100");
        // Nothing in the mapping produces one of these. It is refused here anyway, because the last
        // component of every path this writes is a string out of somebody else's archive.
        let escape = put(&root, "../../etc/passwd", b"no", &from, "https://ms/cab", &mut manifest);
        assert!(escape.is_err(), "a name that climbs out of the tree is not written");
        assert!(manifest.inputs().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn every_name_with_a_capital_in_it_gets_a_lowercase_one_beside_it() {
        let root = scratch("aliases");
        std::fs::create_dir_all(root.join("sdk/include/um")).expect("a directory");
        std::fs::create_dir_all(root.join("crt/include/CodeAnalysis")).expect("a directory");
        std::fs::write(root.join("sdk/include/um/Windows.h"), b"h").expect("a header");
        std::fs::write(root.join("sdk/include/um/winbase.h"), b"h").expect("a header");
        std::fs::write(root.join("crt/include/CodeAnalysis/warnings.h"), b"h").expect("a header");

        let made = aliases(&root).expect("the links");
        // Two on a host whose filesystem tells the spellings apart, and none on a Mac, where the
        // file already answers to the lowercase name and the link finds itself in the way. Both are
        // right, and what is worth asserting either way is what a compile goes on to find.
        assert!(made == 2 || made == 0, "{made} links for two names with a capital in them");
        assert_eq!(std::fs::read(root.join("sdk/include/um/windows.h")).expect("the link"), b"h");
        assert_eq!(
            std::fs::read(root.join("crt/include/codeanalysis/warnings.h")).expect("the link"),
            b"h"
        );
        // Run again over its own output, which is what a second fetch of another architecture into
        // the same cache would do, and nothing new is made and nothing fails.
        assert_eq!(aliases(&root).expect("the links again"), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_payload_keeps_its_name_and_loses_the_directory_the_manifest_put_it_in() {
        // The SDK's installers are named with a backslash, which is an ordinary character in a file
        // name on a Unix, so the two hosts would disagree about the cache if it were written down.
        assert_eq!(
            stored_as(r"Installers\Windows SDK Desktop Headers x64-x86_en-us.msi"),
            "Windows SDK Desktop Headers x64-x86_en-us.msi"
        );
        // The CRT's payloads have no directory on them at all.
        assert_eq!(
            stored_as("Microsoft.VC.14.44.17.14.CRT.Headers.base.vsix"),
            "Microsoft.VC.14.44.17.14.CRT.Headers.base.vsix"
        );
        // And a forward slash, in case a manifest ever spells one that way.
        assert_eq!(stored_as("a/b/c.msi"), "c.msi");
    }

    #[test]
    fn the_download_directory_is_under_the_build() {
        // Two builds of the installer name different files, so a person who fetched one and then
        // the other has both rather than a directory that is half of each.
        assert_eq!(
            downloads(Path::new("/cache"), "17.14.37710.0"),
            Path::new("/cache/downloads/msvc/17.14.37710.0")
        );
    }

    #[test]
    fn sizes_are_readable_and_counts_agree_with_themselves() {
        assert_eq!(mb(2_128_977), "2.1 MB");
        assert_eq!(mb(197_673_853), "197.7 MB");
        assert_eq!(files(1), "1 file");
        assert_eq!(files(14), "14 files");
    }
}
