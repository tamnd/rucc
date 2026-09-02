//! Where the library's headers are.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.4.
//!
//! A hosted implementation is two halves and `rucc_session::runtime` is one of them. The
//! other is the library's, and finding it is the compiler's job because nothing else can do
//! it. A compiler that has to be told `-isystem /usr/include` on every command line is a
//! compiler nobody can run `make` with.
//!
//! gcc settles this at configure time, which it can do because a gcc is built for the machine
//! it will run on and the directories are baked into the binary. This compiler is one binary
//! that runs wherever it is copied, so it has to ask the machine instead, and the shape of
//! the answer is a list of candidates per platform of which the ones that exist are taken.
//!
//! Cross compiling to another operating system produces nothing here on purpose. The host's
//! `/usr/include` describes the host's library and handing it to a program being built for
//! somewhere else is worse than handing it nothing, because the failure moves from the
//! `#include` that could not be resolved to a declaration that is quietly wrong. A cross
//! build supplies the headers with `--sysroot` or with `-isystem`, which is what every cross
//! toolchain already does.

use std::path::{Path, PathBuf};
use std::process::Command;

use rucc_target::{Env, Os, Triple};

/// What the machine says about itself, and what the command line said over the top of it.
///
/// Separated from the lookup so that the lookup is a function of its arguments and can be
/// tested for a platform the test is not running on. Everything here is read once, in
/// [`system_dirs`], which is the only place that talks to the environment.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Machine {
    /// The triple of the machine the compiler is running on, when it is one we know.
    pub host: Option<Triple>,
    /// `--sysroot`, which prefixes the configured directories, or `-isysroot`, which is the
    /// spelling Apple's tools use and which means the same thing to us.
    pub sysroot: Option<PathBuf>,
    /// The SDK to compile against on an Apple platform, once it has been found.
    pub sdk: Option<PathBuf>,
    /// `INCLUDE`, which is how every Windows toolchain says where its headers are. The
    /// entries are separated by `;`, which is a path separator there and a legal character in
    /// a file name nowhere.
    pub include: Option<String>,
}

/// The directories the library's headers might be in, in search order.
///
/// Every candidate, whether or not it is there. [`system_dirs`] is what filters them, and the
/// split is so that this half can be read as the platform knowledge it is.
#[must_use]
pub fn candidates(target: Triple, machine: &Machine) -> Vec<PathBuf> {
    // A target that is not this machine has no directories on this machine. The one exception
    // is a sysroot, which is a statement that the headers for that target are over there.
    if machine.sysroot.is_none() && machine.host.is_some_and(|host| host.os != target.os) {
        return Vec::new();
    }
    let root = machine.sysroot.as_deref();
    match target.os {
        Os::Linux => linux(target, root),
        Os::Darwin => darwin(machine.sdk.as_deref().or(root)),
        Os::Windows => windows(machine.include.as_deref()),
        // Freestanding. There is no library, so there are no headers of one, and the nine the
        // compiler ships are the whole of what a program may include.
        Os::None => Vec::new(),
    }
}

/// gcc's order on a glibc system, which is what every Linux distribution lays out.
///
/// `/usr/local/include` first because that is where a locally built library installs and the
/// point of installing one there is that it wins. The multiarch directory before
/// `/usr/include` because that is where Debian and its derivatives put the headers that
/// differ between two architectures of the same machine, and a distribution that does not use
/// multiarch simply does not have the directory.
fn linux(target: Triple, sysroot: Option<&Path>) -> Vec<PathBuf> {
    let libc = match target.env {
        Env::Musl => "musl",
        // Not `Env::as_str`, which answers `none` for a target written without an environment.
        // A bare `x86_64-linux` on a Linux box means the machine's own libc, and on every
        // machine that lays its headers out per architecture that libc is glibc.
        Env::None | Env::Gnu | Env::Msvc => "gnu",
    };
    let multiarch = format!("{}-linux-{libc}", target.arch.as_str());
    ["/usr/local/include".into(), format!("/usr/include/{multiarch}"), "/usr/include".into()]
        .into_iter()
        .map(|dir| under(sysroot, &dir))
        .collect()
}

/// The SDK, which on an Apple platform is the whole of it.
///
/// There is no `/usr/include` on a Mac since the command line tools stopped installing one,
/// and the headers live inside the SDK that Xcode or the command line tools brought with
/// them. Nothing is offered when there is no SDK, because a guess at a path that is not there
/// only makes the diagnostic longer.
fn darwin(sdk: Option<&Path>) -> Vec<PathBuf> {
    sdk.map(|sdk| vec![sdk.join("usr/include")]).unwrap_or_default()
}

/// Whatever `INCLUDE` says, in the order it says it.
///
/// Windows has no fixed place for the headers. The MSVC ones move with the toolchain version
/// and the SDK ones move with the SDK version, and the way both are found is the environment
/// that `vcvarsall.bat` sets, which is what every compiler on that platform reads and what
/// every build there already has.
fn windows(include: Option<&str>) -> Vec<PathBuf> {
    include
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// A path under the sysroot, when there is one.
fn under(sysroot: Option<&Path>, dir: &str) -> PathBuf {
    match sysroot {
        // `strip_prefix` because joining an absolute path replaces the root rather than
        // extending it, which would make every entry the unprefixed one.
        Some(root) => root.join(dir.strip_prefix('/').unwrap_or(dir)),
        None => PathBuf::from(dir),
    }
}

/// The directories the library's headers are actually in, in search order.
///
/// This is the one function here that talks to the machine: it reads the environment, asks
/// `xcrun` where the SDK is when it has to, and keeps the candidates that exist.
#[must_use]
pub fn system_dirs(target: Triple, sysroot: Option<&Path>) -> Vec<PathBuf> {
    let machine = Machine {
        host: Triple::host(),
        sysroot: sysroot.map(Path::to_path_buf),
        // Asked for only on the platform that has one, since finding it can mean running a
        // program and a compile for Linux should not wait on Xcode.
        sdk: if target.os == Os::Darwin { sdk(sysroot) } else { None },
        include: if target.os == Os::Windows { std::env::var("INCLUDE").ok() } else { None },
    };
    candidates(target, &machine).into_iter().filter(|dir| dir.is_dir()).collect()
}

/// The SDK to compile against, in the order the platform's own tools look.
///
/// `-isysroot` beats `SDKROOT` beats `xcrun` beats the place the command line tools put it.
/// `xcrun` is a program rather than a path because the answer moves with the Xcode that is
/// selected and asking is the only way to be told which one that is, and it is third rather
/// than first because it costs a process and the two before it are free.
fn sdk(sysroot: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = sysroot {
        return Some(root.to_path_buf());
    }
    if let Some(root) = std::env::var_os("SDKROOT") {
        let root = PathBuf::from(root);
        if root.is_dir() {
            return Some(root);
        }
    }
    if let Some(root) = xcrun() {
        return Some(root);
    }
    let tools = PathBuf::from("/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk");
    tools.is_dir().then_some(tools)
}

/// Asks `xcrun` for the SDK path, and says nothing if it is not there to ask.
fn xcrun() -> Option<PathBuf> {
    let out = Command::new("/usr/bin/xcrun").args(["--show-sdk-path"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    (path.is_absolute() && path.is_dir()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rucc_target::Arch;

    fn triple(os: Os, env: Env) -> Triple {
        Triple::new(Arch::X86_64, os, env)
    }

    fn on(host: Os) -> Machine {
        Machine { host: Some(triple(host, Env::Gnu)), ..Machine::default() }
    }

    #[test]
    fn the_local_directory_comes_before_the_distributions_and_the_specific_before_the_general() {
        let dirs = candidates(triple(Os::Linux, Env::Gnu), &on(Os::Linux));
        let dirs: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        assert_eq!(dirs, ["/usr/local/include", "/usr/include/x86_64-linux-gnu", "/usr/include"]);
    }

    #[test]
    fn the_directory_headers_are_kept_apart_in_is_named_after_the_targets_own_library() {
        let of = |env| candidates(triple(Os::Linux, env), &on(Os::Linux))[1].display().to_string();
        assert_eq!(of(Env::Musl), "/usr/include/x86_64-linux-musl");
        assert_eq!(of(Env::Gnu), "/usr/include/x86_64-linux-gnu");
        // A triple written without an environment is the machine's own, and a machine that
        // sorts its headers by architecture at all is one running glibc.
        assert_eq!(of(Env::None), "/usr/include/x86_64-linux-gnu");
    }

    #[test]
    fn a_sysroot_is_in_front_of_every_one_of_them_rather_than_replacing_the_root() {
        let machine = Machine { sysroot: Some("/opt/cross".into()), ..on(Os::Linux) };
        let dirs = candidates(triple(Os::Linux, Env::Gnu), &machine);
        let dirs: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        assert_eq!(
            dirs,
            [
                "/opt/cross/usr/local/include",
                "/opt/cross/usr/include/x86_64-linux-gnu",
                "/opt/cross/usr/include"
            ]
        );
    }

    #[test]
    fn this_machines_headers_are_not_offered_to_a_program_being_built_for_another_system() {
        assert!(candidates(triple(Os::Windows, Env::Msvc), &on(Os::Linux)).is_empty());
        assert!(candidates(triple(Os::Linux, Env::Gnu), &on(Os::Darwin)).is_empty());
        // With a sysroot they are, because that is what naming one says.
        let machine = Machine { sysroot: Some("/opt/cross".into()), ..on(Os::Darwin) };
        assert!(!candidates(triple(Os::Linux, Env::Gnu), &machine).is_empty());
    }

    #[test]
    fn an_unknown_host_offers_the_targets_own_directories_rather_than_none() {
        // `Triple::host` answers nothing on a machine this compiler has no target for, and a
        // native compile there is still a native compile.
        let machine = Machine { host: None, ..Machine::default() };
        assert_eq!(candidates(triple(Os::Linux, Env::Gnu), &machine).len(), 3);
    }

    #[test]
    fn an_apple_target_is_the_sdk_and_nothing_else_and_nothing_without_one() {
        let machine = Machine { sdk: Some("/S.sdk".into()), ..on(Os::Darwin) };
        let dirs = candidates(triple(Os::Darwin, Env::None), &machine);
        assert_eq!(dirs, [PathBuf::from("/S.sdk/usr/include")]);
        assert!(candidates(triple(Os::Darwin, Env::None), &on(Os::Darwin)).is_empty());
    }

    #[test]
    fn windows_is_told_where_its_headers_are_and_is_not_guessed_at() {
        let machine =
            Machine { include: Some(r"C:\vc\include;C:\sdk\ucrt ;".to_owned()), ..on(Os::Windows) };
        let dirs = candidates(triple(Os::Windows, Env::Msvc), &machine);
        assert_eq!(dirs, [PathBuf::from(r"C:\vc\include"), PathBuf::from(r"C:\sdk\ucrt")]);
        assert!(candidates(triple(Os::Windows, Env::Msvc), &on(Os::Windows)).is_empty());
    }

    #[test]
    fn a_freestanding_target_has_no_library_to_find_the_headers_of() {
        let machine = Machine { sysroot: Some("/opt/cross".into()), ..Machine::default() };
        assert!(candidates(triple(Os::None, Env::None), &machine).is_empty());
    }

    #[test]
    fn what_is_offered_on_this_machine_is_there_because_it_was_checked_for() {
        for dir in system_dirs(Triple::host().unwrap_or(triple(Os::Linux, Env::Gnu)), None) {
            assert!(dir.is_dir(), "{}", dir.display());
        }
    }
}
