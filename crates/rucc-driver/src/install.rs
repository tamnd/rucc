//! Checking a sysroot artifact and putting it in the cache.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.8, which divides a fetch in two. A
//! downloader the machine already has moves the bytes, and everything that decides whether the
//! result is correct is here. That division is why this file has no URL in it and runs no network
//! code: what it is handed is a file on disk and the hash that file is supposed to have.
//!
//! # The order, which is the whole of the argument
//!
//! The hash of the archive is checked before anything is unpacked. So what `tar` is pointed at is
//! always a file we have already identified, and an unpacker's behaviour on a file somebody else
//! chose is not a question this has to have an answer to.
//!
//! Then the files that came out are checked against the manifest the producer put in the archive,
//! in both directions. Every line has to name a file with the sha256 it recorded, and every file
//! has to be named by a line. The second direction is the one that is easy to leave out and it is
//! the one that matters: [`rucc_sysroot::Manifest::digest`] is a claim about what is under a
//! directory, and a file nobody recorded makes it a claim about less than what is there.
//!
//! The manifest is the producer's rather than ours because of what is in it. An input carries a
//! source, a URL and a licence, and a walk of a directory tree knows none of the three. The
//! producer in `tamnd/rucc-cross` knows all of them, so it writes the record and this checks it
//! against the bytes, which is the same shape as section 13.6's rule about a generated artifact
//! being checked against its generator.
//!
//! Only then is the result renamed into place, which is section 13.2's concurrency rule. The
//! staging directory is inside the cache so that the rename is a rename and not a copy, because
//! the two paths are on one filesystem by construction.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rucc_sysroot::{Manifest, Sysroot, sha256};
use rucc_tuple::TargetTuple;

use crate::{CliError, err};

/// What was at the destination before an install put something there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Before {
    /// Nothing, which is the first install of this target on this machine.
    Nothing,
    /// A sysroot whose manifest has the same digest, so the install was not needed and nothing was
    /// moved. Two rucc versions share a directory for the reason section 13.2 gives, and a second
    /// fetch of the same artifact is the ordinary way this happens.
    TheSame,
    /// A sysroot with a different digest, which was replaced. The string is the digest that was
    /// there, so that whatever reports the install can say what it stood on.
    Different(String),
}

/// What an install left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// Where it is, which is `sysroots/<tuple>` under the cache.
    pub root: PathBuf,
    /// The digest of its manifest, which is what `rucc -print-sysroot-digest` prints for it.
    pub digest: String,
    /// How many files the manifest records, all of which were checked.
    pub files: usize,
    /// What the destination held before this.
    pub before: Before,
}

/// Check a file against the hash it is supposed to have.
///
/// The hash comes from the rucc release rather than from the artifact or from the server that
/// served it, which is the only arrangement where a check means anything. Section 13.2 says a
/// mismatch is a hard failure with no override flag, so there is no argument here that could turn
/// one into a warning.
///
/// # Errors
///
/// A file that cannot be read, and a hash that does not match. The message names both hashes,
/// because the first thing anybody does with a mismatch is ask whether they downloaded the wrong
/// thing or the right thing badly, and the two look different.
pub fn verify(archive: &Path, expected: &str) -> Result<(), CliError> {
    let bytes = fs::read(archive).map_err(|why| err(format!("{}: {why}", archive.display())))?;
    let found = sha256::hex(&bytes);
    if found == expected {
        return Ok(());
    }
    Err(err(format!(
        "{} has sha256 {found} where this release pins {expected}, so it is not the artifact this \
         build knows about",
        archive.display()
    )))
}

/// Check an artifact and install it as the sysroot for a target.
///
/// The steps are §13.8's, in order: the hash, the unpack, the manifest against the tree and the
/// tree against the manifest, the rename. Nothing is written outside the cache and nothing outside
/// the cache is read except the archive.
///
/// # Errors
///
/// Every step, and each message says which step. An artifact for the wrong target, a missing
/// manifest, a file whose hash disagrees with the record, a file no line names, and anything the
/// filesystem or `tar` refuses. A failure leaves the destination as it was: the work happens in a
/// staging directory and the last thing that happens is the rename.
pub fn install(
    archive: &Path,
    expected: &str,
    target: TargetTuple,
    cache: &Path,
) -> Result<Installed, CliError> {
    verify(archive, expected)?;

    let staging = staging_dir(cache, target);
    fs::create_dir_all(&staging).map_err(|why| err(format!("{}: {why}", staging.display())))?;
    // Every exit from here on has to take the staging directory with it, including the ones that
    // are somebody else's fault, or a machine that fetches a broken artifact twice a day fills its
    // cache with half unpacked trees.
    let outcome = install_staged(archive, target, cache, &staging);
    if outcome.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    outcome
}

/// The install, with the staging directory already made and cleaned up by the caller.
fn install_staged(
    archive: &Path,
    target: TargetTuple,
    cache: &Path,
    staging: &Path,
) -> Result<Installed, CliError> {
    unpack(archive, staging)?;

    let record = staging.join("manifest");
    let text = fs::read_to_string(&record).map_err(|why| {
        if why.kind() == io::ErrorKind::NotFound {
            err(format!(
                "{} has no manifest in it, so there is nothing to check its files against",
                archive.display()
            ))
        } else {
            err(format!("{}: {why}", record.display()))
        }
    })?;
    let manifest =
        Manifest::parse(&text).map_err(|why| err(format!("{}: {why}", archive.display())))?;

    if manifest.target() != target {
        return Err(err(format!(
            "{} is a sysroot for {}, which is not {}",
            archive.display(),
            manifest.target().to_canonical_string(),
            target.to_canonical_string()
        )));
    }

    check(staging, &manifest).map_err(|why| err(format!("{}: {why}", archive.display())))?;

    let digest = manifest.digest();
    let root = Sysroot::in_cache(cache, target).root().to_path_buf();
    let before = swap(staging, &root, &digest)?;
    Ok(Installed { root, digest, files: manifest.inputs().len(), before })
}

/// Where this install does its work.
///
/// Inside the cache, because the rename at the end is only atomic if the two paths are on one
/// filesystem, and a name nothing else will pick, because two builds fetching the same target at
/// the same time is the ordinary case rather than the unlucky one. The process id is not enough on
/// its own: one process can install the same target twice.
fn staging_dir(cache: &Path, target: TargetTuple) -> PathBuf {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let unique =
        format!("{}-{}-{}", target.to_canonical_string(), std::process::id(), now.as_nanos());
    cache.join("staging").join(unique)
}

/// Unpack a verified archive with the platform's own `tar`.
///
/// Section 13.8's decision keeps an archive reader out of the compiler for the same reason it keeps
/// a TLS stack out, and `tar` is on every host in the support table, including Windows since 1803.
/// The members are unpacked as they are, with no component stripped, because the staging directory
/// is one we made for this and a single directory inside the archive would only be a name to
/// disagree about.
fn unpack(archive: &Path, into: &Path) -> Result<(), CliError> {
    let output =
        Command::new("tar").arg("-xzf").arg(archive).arg("-C").arg(into).output().map_err(
            |why| err(format!("could not run `tar`, which is how an artifact is unpacked: {why}")),
        )?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim();
    let detail = if said.is_empty() { String::new() } else { format!(": {said}") };
    Err(err(format!("`tar` could not unpack {}{detail}", archive.display())))
}

/// Check a tree against a manifest and the manifest against the tree.
///
/// The errors are collected rather than returned one at a time. An artifact that fails this is
/// either the wrong artifact or a broken producer, and which of the two it is shows in how many
/// files disagree, so a report that stopped at the first one would hide the thing that tells them
/// apart.
fn check(tree: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();
    let mut recorded: Vec<&str> = Vec::new();

    for input in manifest.inputs() {
        recorded.push(&input.path);
        let at = match relative(tree, &input.path) {
            Ok(at) => at,
            Err(why) => {
                problems.push(why);
                continue;
            }
        };
        match fs::read(&at) {
            Ok(bytes) => {
                let found = sha256::hex(&bytes);
                if found != input.sha256 {
                    problems.push(format!(
                        "{} has sha256 {found} where the record says {}",
                        input.path, input.sha256
                    ));
                }
            }
            Err(why) if why.kind() == io::ErrorKind::NotFound => {
                problems.push(format!("{} is in the record and not in the archive", input.path));
            }
            Err(why) => problems.push(format!("{}: {why}", input.path)),
        }
    }

    let mut found = Vec::new();
    walk(tree, String::new(), &mut found).map_err(|why| format!("{}: {why}", tree.display()))?;
    recorded.sort_unstable();
    for path in &found {
        // The manifest is the record and is not a line in itself, which is the one file in a
        // sysroot that is allowed to be there without being recorded.
        if path == "manifest" {
            continue;
        }
        if recorded.binary_search(&path.as_str()).is_err() {
            problems.push(format!("{path} is in the archive and not in the record"));
        }
    }

    if problems.is_empty() {
        return Ok(());
    }
    problems.sort();
    let first = &problems[0];
    if problems.len() == 1 {
        return Err(format!("the archive does not match its own manifest: {first}"));
    }
    Err(format!(
        "the archive does not match its own manifest: {first}, and {} more files disagree",
        problems.len() - 1
    ))
}

/// The path a recorded input names, refusing one that is not under the tree.
///
/// A manifest is checked before it is trusted and a path is part of a manifest. `..` in a recorded
/// path, or a path that starts at the root of the filesystem, would write the check against a file
/// the archive never carried, so neither is a path this reads.
fn relative(tree: &Path, path: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(path);
    let ordinary = candidate.components().all(|part| matches!(part, Component::Normal(_)));
    if !ordinary {
        return Err(format!("{path} is not a path inside a sysroot"));
    }
    Ok(tree.join(candidate))
}

/// Every file under a directory, as paths relative to it with `/` between the parts.
///
/// `/` whatever the host separator is, because that is the spelling a manifest uses and the
/// comparison is against a manifest. A symlink is a leaf rather than something to follow: following
/// one would count a file twice, or walk forever, and what a recorded hash is about is the bytes a
/// compiler reads through that name, which `fs::read` gets by following it once.
fn walk(dir: &Path, prefix: String, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if entry.file_type()?.is_dir() {
            walk(&entry.path(), path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// Put the staged tree where a compiler will look for it.
///
/// Section 13.2 asks for an atomic rename and never an in place mutation, which is a rule about
/// what a parallel build sees. A compile that is reading the old sysroot while this happens keeps
/// reading the files it has open, and one that starts during the swap finds either the old tree or
/// the new one.
///
/// An existing tree with the same digest is left alone. That is not an optimization: a second fetch
/// of the same artifact is the ordinary case, and replacing a directory that is already correct
/// would move files under a build for no reason at all.
fn swap(staging: &Path, root: &Path, digest: &str) -> Result<Before, CliError> {
    let before = match existing(root) {
        Some(found) if found == digest => {
            let _ = fs::remove_dir_all(staging);
            return Ok(Before::TheSame);
        }
        Some(found) => Before::Different(found),
        None => Before::Nothing,
    };

    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent).map_err(|why| err(format!("{}: {why}", parent.display())))?;
    }

    // The old tree is moved aside rather than deleted first. Deleting it first would leave the path
    // missing for as long as the delete takes, which on a sysroot is thousands of files, and the
    // window this way is one rename wide.
    let aside = root.with_extension(format!("old.{}", std::process::id()));
    if before != Before::Nothing {
        let _ = fs::remove_dir_all(&aside);
        fs::rename(root, &aside)
            .map_err(|why| err(format!("could not move {} aside: {why}", root.display())))?;
    }
    let renamed = fs::rename(staging, root);
    if let Err(why) = renamed {
        // Put back what was there. A failed install that also took the working sysroot away would
        // be worse than the failure it started as.
        if before != Before::Nothing {
            let _ = fs::rename(&aside, root);
        }
        return Err(err(format!("could not put {} in place: {why}", root.display())));
    }
    if before != Before::Nothing {
        let _ = fs::remove_dir_all(&aside);
    }
    Ok(before)
}

/// The digest of the sysroot that is already at this path, if there is one with a manifest.
///
/// A directory with no manifest in it answers [`None`] and is replaced like anything else. It is
/// either a tree somebody assembled by hand, which `--sysroot` is the flag for and the cache is not
/// the place for, or the wreckage of an install from before this code existed.
fn existing(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("manifest")).ok()?;
    Manifest::parse(&text).ok().map(|manifest| manifest.digest())
}

#[cfg(test)]
mod tests {
    use super::{Before, Installed, check, install, relative, verify, walk};
    use rucc_sysroot::{Input, Licence, Manifest, Provenance, sha256};
    use rucc_tuple::TargetTuple;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// A directory that goes away with the test, and the files in it.
    struct Tree(PathBuf);

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let dir =
                std::env::temp_dir().join(format!("rucc-install-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temporary directory should be writable");
            Tree(dir)
        }

        fn write(&self, path: &str, text: &str) {
            let at = self.0.join(path);
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent).expect("a subdirectory should be creatable");
            }
            std::fs::write(&at, text).expect("a temporary file should be writable");
        }
    }

    /// The target every test here uses, and a musl one so that nothing depends on the host.
    fn target() -> TargetTuple {
        "x86_64-linux-musl".parse().expect("a tuple the table knows")
    }

    /// A manifest for these files, with their real hashes in it.
    fn manifest_for(files: &[(&str, &str)]) -> Manifest {
        let mut manifest = Manifest::new(target());
        for (path, text) in files {
            manifest.push(Input {
                path: (*path).to_owned(),
                source: "musl-1.2.5".to_owned(),
                url: "https://musl.libc.org/releases/musl-1.2.5.tar.gz".to_owned(),
                sha256: sha256::hex(text.as_bytes()),
                licence: Licence::Mit,
                provenance: Provenance::Bundled,
            });
        }
        manifest
    }

    /// An artifact holding these files and a manifest that describes them, and its sha256.
    ///
    /// Built with the same `tar` the install unpacks with, which is the point: a test that wrote its
    /// own archive format would be testing a reader nobody uses.
    fn artifact(tree: &Tree, files: &[(&str, &str)], manifest: &Manifest) -> (PathBuf, String) {
        let staged = tree.0.join("staged");
        std::fs::create_dir_all(&staged).expect("a staging directory should be creatable");
        for (path, text) in files {
            let at = staged.join(path);
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent).expect("a subdirectory should be creatable");
            }
            std::fs::write(&at, text).expect("a file should be writable");
        }
        std::fs::write(staged.join("manifest"), manifest.render())
            .expect("the manifest should be writable");

        let archive = tree.0.join("artifact.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staged)
            .arg(".")
            .status()
            .expect("tar should be on a machine that runs these tests");
        assert!(status.success(), "tar should be able to write an archive");
        std::fs::remove_dir_all(&staged).expect("the staged tree should be removable");

        let bytes = std::fs::read(&archive).expect("the archive should be readable");
        let hash = sha256::hex(&bytes);
        (archive, hash)
    }

    const FILES: &[(&str, &str)] =
        &[("include/stdio.h", "int puts(const char *);\n"), ("lib/libc.so", "not really\n")];

    #[test]
    fn an_artifact_that_matches_its_record_is_installed() {
        let tree = Tree::new("good");
        let manifest = manifest_for(FILES);
        let (archive, hash) = artifact(&tree, FILES, &manifest);
        let cache = tree.0.join("cache");

        let done = install(&archive, &hash, target(), &cache).expect("this one should install");
        assert_eq!(done.before, Before::Nothing);
        assert_eq!(done.files, 2);
        assert_eq!(done.digest, manifest.digest());
        assert_eq!(done.root, cache.join("sysroots").join("x86_64-linux-musl"));

        // The files are where a compiler looks for them, and the record is beside them, which is
        // what `-print-sysroot-provenance` reads.
        assert!(done.root.join("include/stdio.h").is_file());
        assert_eq!(
            std::fs::read_to_string(done.root.join("manifest")).expect("a manifest"),
            manifest.render()
        );
        // And nothing is left in the staging area, which is a cache that would otherwise grow a
        // copy of every sysroot it ever installed.
        let left: Vec<PathBuf> = std::fs::read_dir(cache.join("staging"))
            .expect("the staging directory")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        assert_eq!(left, Vec::<PathBuf>::new(), "a staging tree was left behind");
    }

    /// `--fetch` from end to end, with the artifact already where a downloader would have put it.
    ///
    /// No downloader runs, and that is the case rather than a way around one: section 13.8 says a
    /// machine with none of the three is told the path to put a file at and a second run carries on
    /// from the check, so this is that machine. What it tests is that the table, the check, the
    /// unpack and the rename are wired to each other, which is the one thing neither module's own
    /// tests can see.
    ///
    /// It is here rather than beside the parser because the fixtures for an artifact are here, and a
    /// second copy of them next door is the thing most likely to drift away from this one.
    #[test]
    fn a_fetch_of_an_artifact_that_is_already_on_the_machine_installs_it() {
        let tree = Tree::new("fetch");
        let manifest = manifest_for(FILES);
        let (built, hash) = artifact(&tree, FILES, &manifest);
        let cache = tree.0.join("cache");

        // A table row names an artifact by a URL and a hash, and both are static strings there
        // because a release is what writes them. A test computes the hash as it goes, so it leaks
        // two strings into a process that is about to end.
        let pinned = rucc_sysroot::Pinned {
            tuple: "x86_64-linux-musl",
            url: "https://example.invalid/rucc-sysroot-x86_64-linux-musl.tar.gz",
            sha256: String::leak(hash),
        };
        // Where a downloader would have written it, which is what the fetch looks at first.
        let at = pinned.archive_in(&cache);
        std::fs::create_dir_all(at.parent().expect("a parent")).expect("a downloads directory");
        std::fs::copy(&built, &at).expect("the artifact should be placeable");

        assert_eq!(crate::fetch_sysroot(&pinned, target(), &cache), 0);
        let root = cache.join("sysroots").join("x86_64-linux-musl");
        assert!(root.join("include/stdio.h").is_file());
        assert_eq!(
            std::fs::read_to_string(root.join("manifest")).expect("a manifest"),
            manifest.render()
        );
        // And again, which is the ordinary second run: the archive is still there, it still matches,
        // and the tree it would install is the tree that is already installed.
        assert_eq!(crate::fetch_sysroot(&pinned, target(), &cache), 0);
    }

    #[test]
    fn the_same_artifact_twice_does_not_move_anything() {
        let tree = Tree::new("again");
        let manifest = manifest_for(FILES);
        let (archive, hash) = artifact(&tree, FILES, &manifest);
        let cache = tree.0.join("cache");

        let first = install(&archive, &hash, target(), &cache).expect("the first install");
        let second = install(&archive, &hash, target(), &cache).expect("the second install");
        assert_eq!(second.before, Before::TheSame);
        assert_eq!(second.root, first.root);
        assert_eq!(second.digest, first.digest);
    }

    #[test]
    fn a_hash_that_does_not_match_is_refused_before_anything_is_unpacked() {
        let tree = Tree::new("hash");
        let manifest = manifest_for(FILES);
        let (archive, _) = artifact(&tree, FILES, &manifest);
        let cache = tree.0.join("cache");

        let wrong = "0".repeat(64);
        let why =
            install(&archive, &wrong, target(), &cache).expect_err("this is not the artifact");
        assert!(why.message.contains("where this release pins"), "{}", why.message);
        // Nothing was unpacked, which is the order section 13.8 asks for: the cache does not even
        // have the directories in it.
        assert!(!cache.exists(), "a refused artifact should not have reached the cache");
    }

    #[test]
    fn an_artifact_for_another_target_is_refused() {
        let tree = Tree::new("target");
        let mut manifest = Manifest::new("aarch64-linux-musl".parse().expect("a tuple"));
        for (path, text) in FILES {
            manifest.push(Input {
                path: (*path).to_owned(),
                source: "musl-1.2.5".to_owned(),
                url: "https://musl.libc.org/releases/musl-1.2.5.tar.gz".to_owned(),
                sha256: sha256::hex(text.as_bytes()),
                licence: Licence::Mit,
                provenance: Provenance::Bundled,
            });
        }
        let (archive, hash) = artifact(&tree, FILES, &manifest);
        let cache = tree.0.join("cache");

        let why = install(&archive, &hash, target(), &cache).expect_err("the wrong target");
        assert!(why.message.contains("aarch64-linux-musl"), "{}", why.message);
        assert!(why.message.contains("x86_64-linux-musl"), "{}", why.message);
        assert!(!cache.join("sysroots").exists(), "nothing should have been installed");
    }

    #[test]
    fn an_archive_with_no_record_in_it_is_refused() {
        let tree = Tree::new("bare");
        let staged = tree.0.join("staged");
        std::fs::create_dir_all(&staged).expect("a directory");
        std::fs::write(staged.join("include.h"), "int x;\n").expect("a file");
        let archive = tree.0.join("bare.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staged)
            .arg(".")
            .status()
            .expect("tar should run");
        assert!(status.success());
        let hash = sha256::hex(&std::fs::read(&archive).expect("readable"));
        let cache = tree.0.join("cache");

        let why = install(&archive, &hash, target(), &cache).expect_err("no manifest");
        assert!(why.message.contains("has no manifest in it"), "{}", why.message);
    }

    #[test]
    fn a_file_the_record_does_not_name_is_refused() {
        // The direction that is easy to leave out. A digest is a claim about what is under a
        // directory, so an unrecorded file makes it a claim about less than what is there.
        let tree = Tree::new("extra");
        let manifest = manifest_for(FILES);
        let mut with_extra: Vec<(&str, &str)> = FILES.to_vec();
        with_extra.push(("lib/surprise.o", "nobody wrote this down\n"));
        let (archive, hash) = artifact(&tree, &with_extra, &manifest);
        let cache = tree.0.join("cache");

        let why = install(&archive, &hash, target(), &cache).expect_err("an unrecorded file");
        assert!(why.message.contains("lib/surprise.o"), "{}", why.message);
        assert!(why.message.contains("not in the record"), "{}", why.message);
    }

    #[test]
    fn a_file_whose_bytes_changed_is_refused_and_so_is_one_that_is_missing() {
        let tree = Tree::new("bytes");
        // The manifest describes what the files were supposed to be and the archive holds something
        // else, which is what a producer with a bug in it looks like from here.
        let manifest = manifest_for(&[("include/stdio.h", "what the record says\n")]);
        let (archive, hash) = artifact(&tree, &[("include/stdio.h", "what is there\n")], &manifest);
        let cache = tree.0.join("cache");
        let why = install(&archive, &hash, target(), &cache).expect_err("changed bytes");
        assert!(why.message.contains("include/stdio.h has sha256"), "{}", why.message);
        assert!(why.message.contains("where the record says"), "{}", why.message);

        let gone = Tree::new("gone");
        let manifest = manifest_for(FILES);
        let (archive, hash) = artifact(&gone, &FILES[..1], &manifest);
        let cache = gone.0.join("cache");
        let why = install(&archive, &hash, target(), &cache).expect_err("a missing file");
        assert!(why.message.contains("lib/libc.so"), "{}", why.message);
        assert!(why.message.contains("not in the archive"), "{}", why.message);
    }

    #[test]
    fn more_than_one_disagreement_says_how_many() {
        // Which of the two failures this is shows in the count, so the count is in the message.
        let tree = Tree::new("count");
        let manifest = manifest_for(&[("a.h", "one\n"), ("b.h", "two\n"), ("c.h", "three\n")]);
        let (archive, hash) = artifact(
            &tree,
            &[("a.h", "not one\n"), ("b.h", "not two\n"), ("c.h", "three\n")],
            &manifest,
        );
        let cache = tree.0.join("cache");

        let why = install(&archive, &hash, target(), &cache).expect_err("two files disagree");
        assert!(why.message.contains("and 1 more files disagree"), "{}", why.message);
    }

    #[test]
    fn an_install_over_a_different_sysroot_says_what_it_replaced() {
        let tree = Tree::new("replace");
        let cache = tree.0.join("cache");
        let first = manifest_for(&[("include/stdio.h", "the old one\n")]);
        let (archive, hash) = artifact(&tree, &[("include/stdio.h", "the old one\n")], &first);
        let done = install(&archive, &hash, target(), &cache).expect("the first install");
        let was = done.digest.clone();

        let second = Tree::new("replace-second");
        let manifest = manifest_for(&[("include/stdio.h", "the new one\n")]);
        let (archive, hash) = artifact(&second, &[("include/stdio.h", "the new one\n")], &manifest);
        let done = install(&archive, &hash, target(), &cache).expect("the second install");

        assert_eq!(done.before, Before::Different(was));
        assert_eq!(
            std::fs::read_to_string(done.root.join("include/stdio.h")).expect("the new file"),
            "the new one\n"
        );
        // And the tree that was moved aside is gone rather than left beside the one that replaced
        // it, since a cache that keeps every version it ever had is a cache nobody can size.
        let kept: Vec<String> = std::fs::read_dir(cache.join("sysroots"))
            .expect("the sysroots directory")
            .map(|entry| entry.expect("an entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept, vec!["x86_64-linux-musl".to_owned()]);
    }

    #[test]
    fn a_record_that_names_a_path_outside_the_tree_is_refused() {
        // Not a likely producer bug, and the check is here because a manifest is data that has not
        // been checked yet at the moment its paths are read.
        assert!(relative(Path::new("/cache/sysroots/t"), "include/stdio.h").is_ok());
        for path in ["../outside.h", "/etc/passwd", "include/../../outside.h"] {
            let why = relative(Path::new("/cache/sysroots/t"), path)
                .expect_err("this is not a path inside a sysroot");
            assert!(why.contains(path), "{why}");
        }
    }

    #[test]
    fn the_walk_names_files_the_way_a_manifest_does() {
        // With `/` between the parts whatever the host separator is, because the comparison is
        // against a manifest and a manifest has one spelling.
        let tree = Tree::new("walk");
        tree.write("include/sys/types.h", "typedef int t;\n");
        tree.write("manifest", "rucc sysroot manifest 3\n");
        let mut found = Vec::new();
        walk(&tree.0, String::new(), &mut found).expect("the walk should work");
        found.sort();
        assert_eq!(found, vec!["include/sys/types.h".to_owned(), "manifest".to_owned()]);
    }

    #[test]
    fn verify_is_the_hash_of_the_file_and_says_both_when_it_is_not() {
        let tree = Tree::new("verify");
        tree.write("thing", "bytes\n");
        let at = tree.0.join("thing");
        let hash = sha256::hex(b"bytes\n");
        assert!(verify(&at, &hash).is_ok());
        let why = verify(&at, &"f".repeat(64)).expect_err("a mismatch");
        assert!(why.message.contains(&hash), "{}", why.message);
        assert!(why.message.contains(&"f".repeat(64)), "{}", why.message);

        // A file that is not there is not a mismatch and does not read as one.
        let why = verify(&tree.0.join("absent"), &hash).expect_err("nothing to hash");
        assert!(!why.message.contains("where this release pins"), "{}", why.message);
    }

    #[test]
    fn the_check_passes_a_tree_that_matches() {
        // The unit underneath the install, so that a failure in the install tells you which half.
        let tree = Tree::new("check");
        for (path, text) in FILES {
            tree.write(path, text);
        }
        tree.write("manifest", "rucc sysroot manifest 3\n");
        let manifest = manifest_for(FILES);
        assert_eq!(check(&tree.0, &manifest), Ok(()));
        // An empty directory is not a file and is not a disagreement, which is what a tree that
        // went through `tar` on one host and not another looks like.
        std::fs::create_dir_all(tree.0.join("lib/empty")).expect("a directory");
        assert_eq!(check(&tree.0, &manifest), Ok(()));
    }

    #[test]
    fn installed_says_where_and_what() {
        // The type the caller reports from, asserted once so that a change to it is a change to a
        // test rather than to a message nobody reads.
        let made = Installed {
            root: PathBuf::from("/cache/sysroots/x86_64-linux-musl"),
            digest: "0".repeat(64),
            files: 3,
            before: Before::Nothing,
        };
        assert_eq!(made.files, 3);
        assert_eq!(made.before, Before::Nothing);
    }
}
