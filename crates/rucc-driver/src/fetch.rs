//! Getting a file onto this machine with a program the machine already has.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.8. That section divides a fetch in
//! two, and [`crate::install`] is the half that decides whether the result is correct. This is the
//! other half, the transport, which is the one part of a fetch that is not ours to get right and is
//! ours to get out of the way of.
//!
//! # Why there is no HTTP client here
//!
//! Because there is no HTTP client anywhere in rucc. `spec/18-package-layout.md` section 18.3 is a
//! dependency budget the compiler is held to, an HTTP client brings a TLS stack with it, and a TLS
//! stack is a thing with a security release schedule attached. So the bytes are moved by `curl`,
//! `wget` or PowerShell, all three of which are already on the hosts in the support table and all
//! three of which are somebody else's job to keep current.
//!
//! The division of trust that comes out of that is worth saying plainly. The downloader
//! authenticates the connection and we authenticate the bytes. A downloader that was lied to hands
//! us a file that does not match the hash this release pins, and that file is deleted rather than
//! unpacked, so the worst a bad connection can do is stop a fetch.
//!
//! # The order, and what a failure means
//!
//! `curl`, then `wget`, then PowerShell, which is section 13.8's order. A downloader that cannot be
//! run is the next one's turn. A downloader that ran and failed is the end of it: a server that
//! said no is not a reason to ask it again with a different client, and a disk that is full will be
//! full for the second one too.
//!
//! Nothing searches a `PATH` or a `PATHEXT` to find out whether a program is there, because the
//! question is whether the program can be run and the answer to that is what happens when it is
//! run.
//!
//! # The machine with none of the three
//!
//! It is told the URL, the hash and the exact path to put a file at, and a second run carries on
//! from the check rather than starting again. So a host with no downloader is still a host somebody
//! can cross compile on, which is what makes the decision above cheap rather than clever.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::install::verify;
use crate::{CliError, err};

/// A program that can move bytes off a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Downloader {
    /// `curl`, which is on every Unix we support and on Windows since 1803.
    Curl,
    /// `wget`, which is what a minimal Linux image has when it has one of the two.
    Wget,
    /// PowerShell's `Invoke-WebRequest`, which is the answer on a Windows that predates the bundled
    /// `curl` and on one where it was removed.
    PowerShell,
}

impl Downloader {
    /// The order they are tried in, which is section 13.8's order.
    pub const ORDER: [Downloader; 3] = [Downloader::Curl, Downloader::Wget, Downloader::PowerShell];

    /// The program to run.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Downloader::Curl => "curl",
            Downloader::Wget => "wget",
            // Not `pwsh`, which is the cross platform one and is not what a Windows install has
            // unless somebody put it there. This is the fallback for an old Windows, so it asks for
            // the shell an old Windows ships.
            Downloader::PowerShell => "powershell",
        }
    }

    /// The command line that downloads `url` to `into`.
    ///
    /// Quiet, because a compiler driver that prints a progress bar is printing it into a build log
    /// that nobody is watching, and loud about failures, because the message a downloader writes is
    /// the only thing that says whether the URL was wrong or the network was.
    #[must_use]
    pub fn argv(self, url: &str, into: &Path) -> Vec<String> {
        let path = into.display().to_string();
        match self {
            // `--fail` because an HTTP error is otherwise a successful download of an error page,
            // and `--location` because a release URL that redirects is the normal case.
            Downloader::Curl => vec![
                "--fail".to_owned(),
                "--location".to_owned(),
                "--silent".to_owned(),
                "--show-error".to_owned(),
                "--output".to_owned(),
                path,
                url.to_owned(),
            ],
            // wget fails on an HTTP error by default and follows redirects by default, so the two
            // flags curl needs have no counterpart here.
            Downloader::Wget => {
                vec!["--quiet".to_owned(), "--output-document".to_owned(), path, url.to_owned()]
            }
            // One argument holding the whole script, rather than the words of it, because
            // PowerShell joins what follows `-Command` and parses the result, and a path with a
            // space in it would not survive that. `$ProgressPreference` is set because the progress
            // display is slow as well as pointless here, and `-UseBasicParsing` because the other
            // kind needs a browser engine that a server edition does not have.
            Downloader::PowerShell => vec![
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                format!(
                    "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -UseBasicParsing \
                     -Uri '{}' -OutFile '{}'",
                    quote(url),
                    quote(&path)
                ),
            ],
        }
    }
}

/// A string inside a PowerShell single quoted string, where the only special character is the quote
/// itself and it is escaped by doubling.
fn quote(text: &str) -> String {
    text.replace('\'', "''")
}

/// How a file got to where it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetched {
    /// It was already there and it already matched the hash, so nothing ran. This is the ordinary
    /// case on a second fetch, and it is also the machine that was handed the file by hand.
    AlreadyThere,
    /// It was downloaded, by this one of the three.
    Downloaded(Downloader),
}

/// What running a downloader did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ran {
    /// It ran and said it worked, which is not the same as it having written the right bytes.
    Worked,
    /// It ran and failed, with whatever it said about that.
    Failed(String),
    /// It could not be run at all, so it is not on this machine.
    Absent,
}

/// Get the file at `url` to `into`, and refuse anything that is not the artifact `sha256` names.
///
/// The download goes to a temporary path beside `into` and is renamed only after the hash matches,
/// so nothing that looks for `into` can find a half written file, and a fetch that was interrupted
/// leaves nothing for the next one to mistake for the artifact.
///
/// # Errors
///
/// A machine with none of the three downloaders on it, and the message is then the instruction for
/// doing this by hand. A downloader that ran and failed, with what it said. Bytes that do not match
/// the hash, and those are deleted rather than kept, because a file under the name of an artifact it
/// is not would be worse than no file. Anything the filesystem refuses.
pub fn fetch(url: &str, sha256: &str, into: &Path) -> Result<Fetched, CliError> {
    fetch_with(url, sha256, into, &mut run)
}

/// The same, from a function that says what running a downloader did.
///
/// Split out for the reason [`crate::cache`] splits out its environment lookup: the cases worth
/// testing are a machine with no `curl`, a server that answered with a 404 and a download that
/// arrived corrupted, and a test cannot arrange any of the three on the machine it runs on. The
/// temporary path is passed as well as the command line so that a test can write the bytes a
/// downloader would have written.
fn fetch_with(
    url: &str,
    sha256: &str,
    into: &Path,
    run: &mut dyn FnMut(Downloader, &Path, &[String]) -> Ran,
) -> Result<Fetched, CliError> {
    if into.exists() {
        verify(into, sha256)?;
        return Ok(Fetched::AlreadyThere);
    }

    let parent = into
        .parent()
        .ok_or_else(|| err(format!("{} is not a path a file can be written to", into.display())))?;
    fs::create_dir_all(parent).map_err(|why| err(format!("{}: {why}", parent.display())))?;

    let partial = partial(into);
    let mut absent = Vec::new();
    for downloader in Downloader::ORDER {
        let argv = downloader.argv(url, &partial);
        match run(downloader, &partial, &argv) {
            Ran::Absent => {
                absent.push(downloader);
                continue;
            }
            Ran::Failed(said) => {
                let _ = fs::remove_file(&partial);
                let detail = if said.is_empty() { String::new() } else { format!(": {said}") };
                return Err(err(format!(
                    "`{}` could not download {url}{detail}",
                    downloader.program()
                )));
            }
            Ran::Worked => {
                // What a downloader reports is that the transfer finished, and what has to be true
                // is that the bytes are the artifact. Those are different claims and only the
                // second one is ours.
                if let Err(why) = verify(&partial, sha256) {
                    let _ = fs::remove_file(&partial);
                    return Err(err(format!(
                        "the download of {url} was deleted rather than kept: {}",
                        why.message
                    )));
                }
                fs::rename(&partial, into)
                    .map_err(|why| err(format!("{}: {why}", into.display())))?;
                return Ok(Fetched::Downloaded(downloader));
            }
        }
    }

    let tried: Vec<&str> = absent.iter().map(|downloader| downloader.program()).collect();
    Err(err(format!(
        "none of {} can be run on this machine and rucc has no downloader of its own, so \
         download {url}, check that its sha256 is {sha256}, put it at {}, and run this again, \
         which carries on from the check",
        tried.join(", "),
        into.display()
    )))
}

/// Where a download is written before it has been checked.
///
/// Beside the file it will become, so the rename at the end is on one filesystem, and under a name
/// nothing else will pick, because two builds fetching one artifact at the same time is the
/// ordinary case rather than the unlucky one.
fn partial(into: &Path) -> PathBuf {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let mut name = OsString::from(into.file_name().unwrap_or_default());
    name.push(format!(".part.{}.{}", std::process::id(), now.as_nanos()));
    into.with_file_name(name)
}

/// Run one downloader and say what happened.
fn run(downloader: Downloader, _partial: &Path, argv: &[String]) -> Ran {
    let output = Command::new(downloader.program()).args(argv).output();
    let output = match output {
        Ok(output) => output,
        // The one error that means try the next one. Everything else is a machine that has the
        // program and could not start it, which the next program will not fix either.
        Err(why) if why.kind() == io::ErrorKind::NotFound => return Ran::Absent,
        Err(why) => return Ran::Failed(why.to_string()),
    };
    if output.status.success() {
        return Ran::Worked;
    }
    let said = String::from_utf8_lossy(&output.stderr);
    Ran::Failed(said.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::{Downloader, Fetched, Ran, fetch_with, partial};
    use rucc_sysroot::sha256;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    /// A directory that goes away with the test.
    struct Tree(PathBuf);

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let dir =
                std::env::temp_dir().join(format!("rucc-fetch-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temporary directory should be writable");
            Tree(dir)
        }
    }

    const URL: &str = "https://musl.libc.org/releases/musl-1.2.5.tar.gz";
    const BYTES: &[u8] = b"what the release holds\n";

    fn hash() -> String {
        sha256::hex(BYTES)
    }

    #[test]
    fn the_three_command_lines() {
        let into = Path::new("/cache/downloads/musl.tar.gz");

        let curl = Downloader::Curl.argv(URL, into);
        assert_eq!(Downloader::Curl.program(), "curl");
        // `--fail` or an HTTP error page is a successful download, and `--location` or a release URL
        // that redirects is a failure.
        assert!(curl.contains(&"--fail".to_owned()));
        assert!(curl.contains(&"--location".to_owned()));
        assert_eq!(curl.last().expect("the url goes last"), URL);
        assert!(curl.contains(&"/cache/downloads/musl.tar.gz".to_owned()));

        let wget = Downloader::Wget.argv(URL, into);
        assert_eq!(Downloader::Wget.program(), "wget");
        assert_eq!(
            wget,
            vec![
                "--quiet".to_owned(),
                "--output-document".to_owned(),
                "/cache/downloads/musl.tar.gz".to_owned(),
                URL.to_owned(),
            ]
        );

        // The script is one argument and not several, because PowerShell joins what follows
        // `-Command` and parses it again, and a path with a space in it would not survive that.
        let shell = Downloader::PowerShell.argv(URL, Path::new(r"C:\Program Files\a.tar.gz"));
        assert_eq!(Downloader::PowerShell.program(), "powershell");
        let script = shell.last().expect("the script is the last argument");
        assert!(script.contains("Invoke-WebRequest"), "{script}");
        assert!(script.contains(r"-OutFile 'C:\Program Files\a.tar.gz'"), "{script}");
        assert!(script.contains(&format!("-Uri '{URL}'")), "{script}");
        assert_eq!(shell.len(), 4);
    }

    #[test]
    fn a_url_with_a_quote_in_it_does_not_end_the_powershell_string() {
        // Nobody pins a URL like this. The escaping is here because the alternative is a file name
        // deciding where a command ends.
        let argv = Downloader::PowerShell.argv("https://h/it's.tar.gz", Path::new("/tmp/a"));
        let script = argv.last().expect("the script");
        assert!(script.contains("-Uri 'https://h/it''s.tar.gz'"), "{script}");
    }

    #[test]
    fn the_order_is_tried_until_one_of_them_runs() {
        let tree = Tree::new("order");
        let into = tree.0.join("musl.tar.gz");
        let tried = RefCell::new(Vec::new());

        let done = fetch_with(URL, &hash(), &into, &mut |downloader, partial, _| {
            tried.borrow_mut().push(downloader);
            if downloader == Downloader::Curl {
                return Ran::Absent;
            }
            std::fs::write(partial, BYTES).expect("a downloader writes the file");
            Ran::Worked
        })
        .expect("wget should have been enough");

        assert_eq!(done, Fetched::Downloaded(Downloader::Wget));
        assert_eq!(tried.into_inner(), vec![Downloader::Curl, Downloader::Wget]);
        assert_eq!(std::fs::read(&into).expect("the file"), BYTES);
    }

    #[test]
    fn a_downloader_that_ran_and_failed_is_the_end_of_it() {
        // A server that said no is not a reason to ask it again with a different client.
        let tree = Tree::new("failed");
        let into = tree.0.join("musl.tar.gz");
        let tried = RefCell::new(Vec::new());

        let why = fetch_with(URL, &hash(), &into, &mut |downloader, _, _| {
            tried.borrow_mut().push(downloader);
            Ran::Failed("curl: (22) The requested URL returned error: 404".to_owned())
        })
        .expect_err("a 404 is a failure");

        assert!(why.message.contains("`curl` could not download"), "{}", why.message);
        assert!(why.message.contains("404"), "{}", why.message);
        assert_eq!(tried.into_inner(), vec![Downloader::Curl]);
        assert!(!into.exists(), "nothing should have been left under the artifact's name");
    }

    #[test]
    fn a_machine_with_none_of_them_is_told_what_to_do_by_hand() {
        let tree = Tree::new("none");
        let into = tree.0.join("musl.tar.gz");
        let tried = RefCell::new(Vec::new());

        let why = fetch_with(URL, &hash(), &into, &mut |downloader, _, _| {
            tried.borrow_mut().push(downloader);
            Ran::Absent
        })
        .expect_err("there is nothing to download with");

        // The three things somebody needs, and no more than that: where it is, what it has to hash
        // to, and where to put it.
        assert!(why.message.contains(URL), "{}", why.message);
        assert!(why.message.contains(&hash()), "{}", why.message);
        assert!(why.message.contains(&into.display().to_string()), "{}", why.message);
        assert!(why.message.contains("curl, wget, powershell"), "{}", why.message);
        assert_eq!(tried.into_inner(), Downloader::ORDER.to_vec());
    }

    #[test]
    fn bytes_that_do_not_match_are_deleted_rather_than_installed() {
        // The division of trust: the downloader authenticated the connection and said it worked,
        // and what the bytes are is still ours to decide.
        let tree = Tree::new("corrupt");
        let into = tree.0.join("musl.tar.gz");
        let written = RefCell::new(PathBuf::new());

        let why = fetch_with(URL, &hash(), &into, &mut |_, partial, _| {
            *written.borrow_mut() = partial.to_path_buf();
            std::fs::write(partial, b"half of it\n").expect("a downloader writes the file");
            Ran::Worked
        })
        .expect_err("these are not the bytes");

        assert!(why.message.contains("where this release pins"), "{}", why.message);
        assert!(why.message.contains("deleted rather than kept"), "{}", why.message);
        assert!(!into.exists(), "nothing should be under the artifact's name");
        assert!(!written.into_inner().exists(), "the partial file should be gone");
    }

    #[test]
    fn a_file_that_is_already_there_and_matches_is_left_alone() {
        // Which is a second fetch of one release, and is also the machine that was handed the file
        // by hand and is running this again to carry on from the check.
        let tree = Tree::new("again");
        let into = tree.0.join("musl.tar.gz");
        std::fs::write(&into, BYTES).expect("the file");

        let done = fetch_with(URL, &hash(), &into, &mut |_, _, _| {
            panic!("nothing should have been run");
        })
        .expect("it is already here");
        assert_eq!(done, Fetched::AlreadyThere);
        assert_eq!(std::fs::read(&into).expect("the file"), BYTES);
    }

    #[test]
    fn a_file_that_is_already_there_and_does_not_match_is_refused_rather_than_replaced() {
        // Somebody put a file there, so they are told it is the wrong one. Deleting it and
        // downloading over the top would answer a question they did not ask.
        let tree = Tree::new("wrong");
        let into = tree.0.join("musl.tar.gz");
        std::fs::write(&into, b"something else\n").expect("the file");

        let why = fetch_with(URL, &hash(), &into, &mut |_, _, _| {
            panic!("nothing should have been run");
        })
        .expect_err("that is not the artifact");
        assert!(why.message.contains("where this release pins"), "{}", why.message);
        assert!(into.exists(), "a file somebody placed should still be there");
    }

    #[test]
    fn a_download_in_progress_is_not_under_the_name_of_the_artifact() {
        // Which is what lets the already there check above be a check of one path rather than a
        // question about whether a previous run finished.
        let into = Path::new("/cache/downloads/musl-1.2.5.tar.gz");
        let partial = partial(into);
        assert_eq!(partial.parent(), into.parent());
        assert_ne!(partial, into);
        let name = partial.file_name().expect("a name").to_string_lossy().into_owned();
        assert!(name.starts_with("musl-1.2.5.tar.gz.part."), "{name}");
    }

    #[test]
    fn the_parent_directory_is_made_if_it_is_not_there() {
        // The downloads directory does not exist on a machine that has never fetched anything, and
        // a downloader told to write into a directory that is not there fails in its own words.
        let tree = Tree::new("parent");
        let into = tree.0.join("downloads").join("musl.tar.gz");

        let done = fetch_with(URL, &hash(), &into, &mut |_, partial, _| {
            assert!(partial.parent().expect("a parent").is_dir(), "the directory should be there");
            std::fs::write(partial, BYTES).expect("a downloader writes the file");
            Ran::Worked
        })
        .expect("this should work");
        assert_eq!(done, Fetched::Downloaded(Downloader::Curl));
    }
}
