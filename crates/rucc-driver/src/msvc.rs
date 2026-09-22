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
//! It prints the licence, refuses to go on without explicit acceptance, and then downloads the
//! selection into the cache with every file held against the hash the manifest gives for it.
//!
//! # What this does not do yet
//!
//! Unpack. The CRT files are vsix archives, which are zips, and the SDK files are MSIs whose
//! contents are in cabs the MSIs name, so turning the download into the `crt/include` and
//! `sdk/include` tree that `--sysroot` already understands needs a zip reader and an MSI and cab
//! reader. That is its own change. Until it lands this command gets the bytes onto the machine and
//! says where they are, and the sysroot record that section 13.5 asks for is written by the change
//! that produces a sysroot to write it about.
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

use std::path::{Path, PathBuf};

use rucc_sysroot::msvc::{Channel, Chip, Selection};
use rucc_tuple::TargetTuple;

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
fn under(cache: &Path, build: &str) -> PathBuf {
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
/// a per cent over on three files out of fourteen is not worth a second source.
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

    let dir = under(cache, &channel.build);
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
    say("nothing has been unpacked, because the vsix files are zips and the SDK files are MSIs \
         whose contents are in cabs, and reading those is the next piece of work");
    Ok(0)
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
    use super::{files, mb, stored_as, under};
    use std::path::Path;

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
            under(Path::new("/cache"), "17.14.37710.0"),
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
