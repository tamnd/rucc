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
//! `#include` that could not be resolved to a declaration that is quietly wrong.
//!
//! What a cross build gets instead is the target's own headers, out of the sysroot for that
//! target, and [`header_dirs`] is where the two cases meet. It is the header half of what
//! [`crate::link`] does for the libraries, it decides nothing itself, and the rule it asks is
//! `rucc_sysroot::search`, which is section 8.5 written once.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use rucc_sysroot::{Kernel, Options, Sysroot, Wall, include_paths};
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
    /// The Windows SDK to compile against, as an `INCLUDE` spells one, once it has been found.
    ///
    /// `INCLUDE` itself when the environment has it, which is what `vcvarsall.bat` sets and what
    /// every build on that platform already reads, and otherwise the same list assembled from the
    /// Visual Studio installation this machine has. The entries are separated by `;`, which is a
    /// path separator there and a legal character in a file name nowhere.
    pub include: Option<String>,
}

/// The directories the library's headers might be in, in search order.
///
/// Every candidate, whether or not it is there. [`system_dirs`] is what filters them, and the
/// split is so that this half can be read as the platform knowledge it is.
#[must_use]
pub fn candidates(target: Triple, machine: &Machine) -> Vec<PathBuf> {
    // A target that is not this machine has no directories on this machine. There are two
    // exceptions and they are the same exception twice: a sysroot and an SDK are both somebody
    // saying that the headers for that target are over there, and `INCLUDE` is a third somebody
    // saying it in the words that platform uses. The SDK case is how `SDKROOT` reaches an Apple
    // target from a machine that is not a mac, which is the path
    // `spec/cross-compile/08-sysroots.md` section 8.6 leaves open when it says a user supplies one.
    if machine.sysroot.is_none()
        && machine.sdk.is_none()
        && machine.include.is_none()
        && machine.host.is_some_and(|host| host.os != target.os)
    {
        return Vec::new();
    }
    let root = machine.sysroot.as_deref();
    match target.os {
        Os::Linux => linux(target, root),
        Os::Darwin => darwin(machine.sdk.as_deref().or(root)),
        Os::Windows => windows(root, machine.include.as_deref()),
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
/// only makes the diagnostic longer, and the diagnostic is `rucc_sysroot::Wall::no_headers`,
/// which the driver leaves on the search path: an Apple target with no SDK anywhere is Apple's
/// licence wall rather than a missing directory, and the include that failed is where it is said.
fn darwin(sdk: Option<&Path>) -> Vec<PathBuf> {
    sdk.map(|sdk| vec![sdk.join("usr/include")]).unwrap_or_default()
}

/// A tree somebody named, or whatever `INCLUDE` says, in the order it says it.
///
/// Windows has no fixed place for the headers. The MSVC ones move with the toolchain version
/// and the SDK ones move with the SDK version, and the way both are found is the environment
/// that `vcvarsall.bat` sets, which is what every compiler on that platform reads and what
/// every build there already has. So `INCLUDE` is a list of directories rather than a root,
/// and it is taken as it stands.
///
/// A named tree is the other way in, and it is the one a cross compile uses, because nothing on
/// a Linux box ran `vcvarsall.bat`. The layout is the one `xwin` writes and `cargo-xwin` builds
/// against, which is the only relocatable shape an MSVC tree has: the CRT's headers under
/// `crt/include` and the Windows SDK's under `sdk/include`, lowercase, with the version
/// directories already resolved away. A copied Visual Studio installation is reached by setting
/// `INCLUDE` instead, which is that platform's own spelling for it.
fn windows(sysroot: Option<&Path>, include: Option<&str>) -> Vec<PathBuf> {
    if let Some(root) = sysroot {
        return ["crt/include", "sdk/include/ucrt", "sdk/include/shared", "sdk/include/um"]
            .into_iter()
            .chain(["sdk/include/winrt", "sdk/include/cppwinrt"])
            .map(|dir| root.join(dir))
            .collect();
    }
    include
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// The header directories of a Visual Studio installation, in the order `vcvarsall.bat` puts them
/// in `INCLUDE`.
///
/// `vc` is the versioned directory under `VC/Tools/MSVC` and `kit` is the versioned directory under
/// the Windows Kit's `Include`, because the two halves are versioned separately and installed by
/// different things: one comes with the compiler and holds the CRT, and the other is the platform
/// and holds `windows.h` and the universal CRT. A machine can have several of each.
///
/// The five kit directories rather than the one, because they are five search roots and not a
/// hierarchy. `ucrt` is the C library, `um` is the Win32 API, `shared` is what those two have in
/// common, and the last two are for a language this compiler does not compile, so they are here for
/// the same reason `vcvarsall.bat` puts them there: a header in one of them includes one of the
/// others by its bare name.
fn msvc_dirs(vc: &Path, kit: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![vc.join("include")];
    for dir in ["ucrt", "shared", "um", "winrt", "cppwinrt"] {
        dirs.push(kit.join(dir));
    }
    dirs
}

/// A version directory's name as numbers, for comparing two of them.
///
/// Text comparison is wrong here and quietly so. `10.0.9.0` sorts after `10.0.22621.0` as text and
/// before it as a version, and the Windows Kit's directories are exactly that shape, so a compiler
/// that picked the larger string would compile against an SDK from several years before the one the
/// machine has. [`None`] for a name that is not a version at all, which is how a `Catalogs` or a
/// `Source` directory beside the versioned ones is passed over.
fn version_key(name: &str) -> Option<Vec<u64>> {
    let parts: Vec<u64> = name.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
    (!parts.is_empty()).then_some(parts)
}

/// The newest version directory under `dir`, which is the one to compile against.
///
/// The newest rather than a configured one, because there is nothing to configure it with and a
/// person who installed a second SDK installed a newer one. A named `--sysroot` is how somebody
/// says which tree they meant, and `INCLUDE` is how they say it in that platform's own words.
fn newest(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let Some(key) = name.to_str().and_then(version_key) else { continue };
        if !entry.path().is_dir() {
            continue;
        }
        if best.as_ref().is_none_or(|(found, _)| key > *found) {
            best = Some((key, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// What this machine's own Visual Studio installation says, as an `INCLUDE` would say it.
///
/// Asked at most once per process, for the reason [`xcrun`] is: it costs two subprocesses and the
/// answer does not change inside one compile. Joined with `;` rather than kept as a list so that
/// there is one parser for both ways in, which is lossless because `;` is a path separator on that
/// platform and a legal character in a file name nowhere.
///
/// This is the Windows half of what `xcrun` is on a mac, and it exists for the same reason: the
/// headers of a platform whose SDK is not ours to ship are on the machine or they are nowhere, and
/// the only way to be told where is to ask the thing that installed them. `vswhere.exe` is at a
/// fixed path on every machine with Visual Studio 2017 or later, which is what makes it askable at
/// all, and the kit is in the registry because that is where its installer puts it.
fn installed_msvc() -> Option<String> {
    static ANSWER: OnceLock<Option<String>> = OnceLock::new();
    ANSWER
        .get_or_init(|| {
            let vc = newest(&visual_studio()?.join("VC").join("Tools").join("MSVC"))?;
            let kit = newest(&windows_kit()?.join("Include"))?;
            let dirs: Vec<String> =
                msvc_dirs(&vc, &kit).iter().map(|dir| dir.display().to_string()).collect();
            Some(dirs.join(";"))
        })
        .clone()
}

/// Where Visual Studio is, according to the installer that put it there.
///
/// `-products *` because the C++ build tools are a product of their own and a machine with those and
/// no Visual Studio is the ordinary shape of a build server. `-latest` because the alternative is to
/// read a list and pick, which is [`newest`]'s job one level down.
fn visual_studio() -> Option<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles(x86)")?;
    let vswhere = PathBuf::from(program_files)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    if !vswhere.is_file() {
        return None;
    }
    let out = Command::new(vswhere)
        .args(["-latest", "-products", "*", "-property", "installationPath"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.lines().next()?.trim());
    path.is_dir().then_some(path)
}

/// Where the Windows Kit is, which is the Windows SDK and the universal CRT.
///
/// The registry first and the default location second, rather than the default location only,
/// because the installer lets somebody move it and writes down where it went. `reg.exe` is how a
/// program with no dependencies reads a key, and the value is the rest of the line after the type
/// because a path there has spaces in it and `Program Files (x86)` has two.
fn windows_kit() -> Option<PathBuf> {
    const KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots";
    if let Ok(out) = Command::new("reg.exe").args(["query", KEY, "/v", "KitsRoot10"]).output() {
        if let Ok(text) = String::from_utf8(out.stdout) {
            for line in text.lines() {
                let Some((name, value)) = line.trim().split_once("REG_SZ") else { continue };
                if name.trim() != "KitsRoot10" {
                    continue;
                }
                let root = PathBuf::from(value.trim());
                if root.is_dir() {
                    return Some(root);
                }
            }
        }
    }
    let default = PathBuf::from(std::env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10");
    default.is_dir().then_some(default)
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
        // Asked for only on the platforms that have one, since finding either can mean running a
        // program and a compile for Linux should not wait on Xcode or on the Visual Studio
        // installer. The MSVC environment and not every Windows target, because a mingw-w64 target's
        // headers are ours and are in the cache, and handing it Microsoft's would be giving a
        // program the declarations of a C library it is not being linked against.
        sdk: if target.os == Os::Darwin { sdk(sysroot) } else { None },
        include: if target.os == Os::Windows && target.env == Env::Msvc {
            msvc(sysroot)
        } else {
            None
        },
    };
    candidates(target, &machine).into_iter().filter(|dir| dir.is_dir()).collect()
}

/// The system header directories for this compile, which is step 3 of section 8.5.
///
/// Design: `spec/cross-compile/08-sysroots.md` section 8.5.
///
/// Three sources and the first that has anything wins: a tree the user named with `--sysroot` or
/// `-isysroot`, then the sysroot for this target, then this machine's own directories and only when
/// the target is this machine. `bundled` is [`crate::link::cross_sysroot`], which is the one place
/// the two kinds of compile are told apart, so the headers a file is compiled against and the
/// libraries it is linked against cannot disagree about which kind it is.
///
/// The ordering between the three is not decided here. It is `rucc_sysroot::search::include_paths`,
/// which is section 8.5 as a function, and the condition that a host directory is legal only when
/// the target is the host lives there and nowhere else. What this adds is the part that has to talk
/// to the machine, which is [`system_dirs`] above.
///
/// Steps 1 and 2 are the driver's own. `-I` and its relatives are in [`rucc_session`]'s search path
/// already, in the order the command line gave them, and the compiler's own headers are not a
/// directory at all but the `<builtin>` entry the caller pushes before this.
///
/// A sysroot that is not on the disk yet is still named. The list is not filtered for existence the
/// way [`system_dirs`] filters the machine's, because the answer to `rucc --target=... -v` on a
/// machine where the tree has not been built should be the path it would be at rather than silence.
///
/// `kernel` is [`crate::link::cross_kernel`], and on a Linux target it adds two more directories
/// after the libc's. They are the kernel's `asm/` for the architecture and its shared `linux/` and
/// `asm-generic/`, they are not under any sysroot because every target sharing an architecture reads
/// the same files, and they come last for the reason section 8.5 gives: both trees have a `sys/` and
/// the libc's is the one a program means.
#[must_use]
pub fn header_dirs(
    target: Triple,
    sysroot: Option<&Path>,
    bundled: Option<&Sysroot>,
    kernel: Option<&Kernel>,
) -> Vec<PathBuf> {
    // Once, because asking can mean running `xcrun` or the Visual Studio installer. The answer goes
    // to whichever of the three fields it belongs in, and which one that is decides how step 3 treats
    // it rather than being a detail of how it was found. With a `--sysroot` these are the directories
    // under the tree the user named. Without one, on a target behind a licence wall, they are an SDK
    // this machine has, which is the target's own headers for every architecture of that platform and
    // not this machine's library, so one Xcode serves `x86_64-macos` on an arm64 mac and one Windows
    // Kit serves `aarch64-windows-msvc` on an x86_64 box, which is how the platform's own tools use
    // them. Otherwise they are the machine's own directories and step 3 will only take them when the
    // target is the host.
    let dirs = system_dirs(target, sysroot);
    // `Wall` rather than a second list of the two operating systems, because the targets whose
    // headers are found as an SDK are exactly the targets whose headers are not ours to ship, and a
    // copy of that rule here is a copy that can disagree with the one the diagnostic reads.
    let walled = Wall::of(target.tuple()).is_some();
    let (named, sdk, host) = match (sysroot.is_some(), walled) {
        (true, _) => (dirs, Vec::new(), Vec::new()),
        (false, true) => (Vec::new(), dirs, Vec::new()),
        (false, false) => (Vec::new(), Vec::new(), dirs),
    };
    let options = Options {
        sysroot: &named,
        sdk: &sdk,
        bundled,
        kernel,
        host_include: &host,
        ..Options::default()
    };
    include_paths(target.tuple(), Triple::host().map(Triple::tuple), &options)
        .into_iter()
        .map(|entry| entry.path)
        .collect()
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

/// The Windows SDK to compile against, in the order somebody would expect to be obeyed.
///
/// A tree somebody named beats `INCLUDE` beats the installation this machine has, which is the order
/// [`sdk`] uses on the Apple side and for the same reasons: the cheap answers are the ones somebody
/// gave us, and the one that costs a process is last. A named tree answers nothing here because it is
/// not a list of directories, and [`windows`] is handed the root itself.
fn msvc(sysroot: Option<&Path>) -> Option<String> {
    if sysroot.is_some() {
        return None;
    }
    match std::env::var("INCLUDE") {
        Ok(include) if !include.trim().is_empty() => Some(include),
        _ => installed_msvc(),
    }
}

/// What `xcrun` said, asked at most once in a process.
///
/// A compiler that ran it for the header search and again for the diagnostic that explains an empty
/// one would pay for a process twice to be told the same path. The answer cannot change underneath us
/// in a way that matters either: a run of the compiler compiles against one SDK.
fn xcrun() -> Option<PathBuf> {
    static ANSWER: OnceLock<Option<PathBuf>> = OnceLock::new();
    ANSWER.get_or_init(ask_xcrun).clone()
}

/// Asks `xcrun` for the SDK path, and says nothing if it is not there to ask.
fn ask_xcrun() -> Option<PathBuf> {
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
        // Joined rather than spelled out, because a path prints with the separator the host
        // uses and this test runs on a host where that is a backslash.
        let under = |dir| PathBuf::from("/opt/cross").join(dir);
        assert_eq!(
            dirs,
            [
                under("usr/local/include"),
                under("usr/include/x86_64-linux-gnu"),
                under("usr/include")
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
    fn an_sdk_reaches_an_apple_target_from_a_host_that_is_not_a_mac() {
        // A Linux box with `SDKROOT` pointing at an SDK somebody downloaded under their own licence,
        // which is the path section 8.6 leaves open on every host that is not a mac. It is the same
        // exception a `--sysroot` gets and for the same reason: somebody said where the headers are.
        let machine = Machine { sdk: Some("/S.sdk".into()), ..on(Os::Linux) };
        let dirs = candidates(triple(Os::Darwin, Env::None), &machine);
        assert_eq!(dirs, [PathBuf::from("/S.sdk/usr/include")]);
        // And with no SDK there is nothing, which is what the licence wall's message is about.
        assert!(candidates(triple(Os::Darwin, Env::None), &on(Os::Linux)).is_empty());
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
    fn an_sdk_reaches_an_msvc_target_from_a_host_that_is_not_windows() {
        // The same exception the Apple side gets, and the case it is for is a Linux build machine
        // with a tree `xwin` assembled on it under a licence its owner accepted.
        let machine = Machine { include: Some(r"C:\sdk\um".to_owned()), ..on(Os::Linux) };
        let dirs = candidates(triple(Os::Windows, Env::Msvc), &machine);
        assert_eq!(dirs, [PathBuf::from(r"C:\sdk\um")]);
    }

    #[test]
    fn a_named_tree_for_an_msvc_target_is_the_layout_a_relocatable_one_has() {
        let machine = Machine { sysroot: Some("/opt/xwin".into()), ..on(Os::Linux) };
        let dirs = candidates(triple(Os::Windows, Env::Msvc), &machine);
        let under = |dir| PathBuf::from("/opt/xwin").join(dir);
        assert_eq!(
            dirs,
            [
                under("crt/include"),
                under("sdk/include/ucrt"),
                under("sdk/include/shared"),
                under("sdk/include/um"),
                under("sdk/include/winrt"),
                under("sdk/include/cppwinrt"),
            ]
        );
    }

    #[test]
    fn the_mingw_target_is_not_offered_microsofts_headers() {
        // Its headers are ours, they are in the cache, and Microsoft's are the declarations of a
        // library it is not linked against. `system_dirs` is what decides this, by asking for an
        // `INCLUDE` only in the MSVC environment, so a `Machine` with one set is the test.
        let machine = Machine { include: Some(r"C:\sdk\um".to_owned()), ..on(Os::Windows) };
        assert!(system_dirs(triple(Os::Windows, Env::Gnu), None).is_empty());
        // And the field itself is still obeyed when it is set, which is what keeps this test honest
        // about where the decision is rather than asserting it twice.
        assert!(!candidates(triple(Os::Windows, Env::Gnu), &machine).is_empty());
    }

    #[test]
    fn the_headers_of_an_installation_are_in_the_order_the_developer_prompt_puts_them() {
        // Joined rather than spelled out, because this test runs on hosts whose separator is not
        // the one a path like this is written with.
        let vc = PathBuf::from(r"C:\BuildTools\VC\Tools\MSVC\14.44.35207");
        let dirs = msvc_dirs(&vc, Path::new("/k"));
        assert_eq!(dirs[0], vc.join("include"));
        let rest: Vec<String> =
            dirs[1..].iter().map(|dir| dir.file_name().unwrap().to_string_lossy().into()).collect();
        assert_eq!(rest, ["ucrt", "shared", "um", "winrt", "cppwinrt"]);
    }

    #[test]
    fn a_version_directory_is_compared_as_numbers_and_not_as_text() {
        // The case that makes this matter. As text the first of these is the larger.
        assert!(version_key("10.0.9.0") < version_key("10.0.22621.0"));
        assert!(version_key("10.0.22621.0") < version_key("10.0.26100.0"));
        assert!(version_key("14.44.35207") > version_key("14.39.33519"));
        // And the directories that sit beside the versioned ones in a Windows Kit.
        assert_eq!(version_key("Catalogs"), None);
        assert_eq!(version_key("wdf"), None);
        assert_eq!(version_key(""), None);
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
