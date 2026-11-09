//! The link line of FreeBSD, NetBSD, OpenBSD and DragonFly BSD, which is the line each system's
//! GCC hands to its linker.
//!
//! Every word is from the GCC 15.2 sources rather than from what a BSD machine happens to have.
//! The order is `LINK_COMMAND_SPEC` in `gcc.cc`: `--sysroot`, `LINK_EH_SPEC`, `LINK_SPEC`, `-pie`,
//! `-o`, `-s`, the start files, the `-L` directories, the objects, `libgcc`, the C library,
//! `libgcc` again and the end files. The words of each part are the target headers:
//! `freebsd-spec.h`, `i386/freebsd.h`, `i386/freebsd64.h`, `aarch64/aarch64-freebsd.h` and
//! `riscv/freebsd.h` for FreeBSD, `netbsd.h`, `netbsd-elf.h`, `i386/netbsd-elf.h`,
//! `i386/netbsd64.h` and `aarch64/aarch64-netbsd.h` for NetBSD, `openbsd.h`,
//! `openbsd-libpthread.h` and `i386/openbsdelf.h` for OpenBSD, and `dragonfly.h` for DragonFly.
//! NetBSD's RISC-V line and its `libgcc` words are the ones in NetBSD's own copy of GCC, because
//! upstream GCC has no RISC-V NetBSD target and NetBSD's base compiler is that copy.
//!
//! GCC on each BSD runs GNU ld, and the line is the one GNU ld gets. Three words of it are left
//! off when the linker is lld, each because lld refuses it or reads it differently:
//!
//! - `-maarch64fbsd` on an AArch64 FreeBSD line, which lld does not know as an emulation. Clang's
//!   FreeBSD driver passes no `-m` there either.
//! - `-dc -dp` on a NetBSD line, which lld rejects as unknown options. They ask GNU ld to allocate
//!   common symbols in a relocatable link too.
//! - `-dynamic-linker` on a static OpenBSD line. lld writes a `PT_INTERP` whenever it is given a
//!   loader, so the program would ask for `ld.so` and not be static. GNU ld writes one only when
//!   the link has a shared object in it.
//!
//! The linker is GNU ld when `<linker> --version` starts with `GNU ld`, as `bsd.linker.mk` reads
//! it on FreeBSD. Any other linker, and a link with no linker found, gets the line for lld.

use std::path::{Path, PathBuf};

use rucc_target::{Arch, Os, Triple};

use super::{Error, Item, LinkOptions, builtins_archive, find_file, push_items, under};

/// Which linker reads the line, for the three words that only GNU ld takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ld {
    /// GNU ld, which GCC on each BSD runs and which takes the whole of GCC's line.
    Gnu,
    /// lld, or any linker that is not GNU ld, which gets the line without those words.
    Lld,
}

/// The linkers to look for, in order.
///
/// The machine's own `ld` first when this is the machine the program is for, as GCC on it would
/// use. For another machine, the `ld` in each `-B` directory first, because that is where GCC
/// looks for the linker and where FreeBSD's build puts the binutils of the target: it passes
/// `-B${WORLDTMP}/usr/bin`, or `-B` and its `CROSS_BINUTILS_PREFIX`. The `ld` on `PATH` is left
/// alone then, because it links for this machine. Then `<triple>-ld`, the name a cross binutils
/// installs on `PATH`, which is the name collect2 of a cross GCC looks for there. lld comes last,
/// which links for every BSD from one binary.
pub(super) fn order(target: Triple, host: Option<Triple>, prefixes: &[PathBuf]) -> Vec<String> {
    let mut names = Vec::new();
    if native(target, host) {
        names.push("ld".to_owned());
    } else {
        // A path, so that the search does not go on to `PATH` with it.
        names.extend(prefixes.iter().map(|dir| dir.join("ld").display().to_string()));
        names.push(format!("{}-ld", triple(target)));
    }
    names.extend(["ld.lld".to_owned(), "lld".to_owned()]);
    names
}

/// The triple a GCC for the target is configured with, as `-dumpmachine` prints it on another
/// machine: `x86_64-unknown-freebsd15` or `aarch64-unknown-netbsd`.
fn triple(target: Triple) -> String {
    format!("{}-unknown-{}", target.arch.as_str(), target.os)
}

/// Which linker `--version` says it is.
pub(super) fn reads(version: &str) -> Ld {
    if version.starts_with("GNU ld") { Ld::Gnu } else { Ld::Lld }
}

/// Whether the program is for this machine: the same architecture and the same BSD, whatever the
/// FreeBSD release, since one machine runs the programs of an older release.
fn native(target: Triple, host: Option<Triple>) -> bool {
    host.is_some_and(|host| {
        host.arch == target.arch && host.os.without_release() == target.os.without_release()
    })
}

/// The directories the start files, the C library and `libgcc` are in.
///
/// Each BSD keeps all three in `/usr/lib`. DragonFly keeps the files of its base GCC in
/// `/usr/lib/gcc80`, which comes first because GCC puts its own directory before the system's.
pub(super) fn dirs(target: Triple, root: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if target.os == Os::DragonFly {
        let gcc = under(root, "/usr/lib/gcc80");
        if gcc.is_dir() {
            dirs.push(gcc);
        }
    }
    dirs.push(under(root, "/usr/lib"));
    dirs
}

/// The `-m` words of the line, which are the same on a relocatable link.
pub(super) fn emulation(target: Triple) -> Vec<String> {
    match (target.os, target.arch) {
        (Os::FreeBsd(_), Arch::X86_64) => vec!["-m".to_owned(), "elf_x86_64_fbsd".to_owned()],
        (Os::FreeBsd(_) | Os::NetBsd, Arch::Riscv64) => vec!["-melf64lriscv".to_owned()],
        _ => Vec::new(),
    }
}

/// The words of a relocatable link before `-r`: the `-m` of the line, and the words of
/// `LINK_SPEC` that GNU ld reads in a relocatable link, `-maarch64fbsd` and NetBSD's `-dc -dp`.
pub(super) fn relocatable(target: Triple, ld: Ld) -> Vec<String> {
    let mut args = emulation(target);
    if ld == Ld::Gnu {
        match (target.os, target.arch) {
            (Os::FreeBsd(_), Arch::Aarch64) => args.push("-maarch64fbsd".to_owned()),
            (Os::NetBsd, _) => args.extend(["-dc".to_owned(), "-dp".to_owned()]),
            _ => {}
        }
    }
    args
}

/// The name of the system, for the messages.
fn system(os: Os) -> &'static str {
    match os {
        Os::FreeBsd(_) => "FreeBSD",
        Os::NetBsd => "NetBSD",
        Os::OpenBsd => "OpenBSD",
        _ => "DragonFly BSD",
    }
}

/// Why a compile for `target` has no system headers when this machine is not that BSD.
pub(super) fn no_headers(target: Triple) -> String {
    let name = system(target.os);
    format!(
        "{tuple} compiles against the headers of {name}, and this machine is not {name}. Pass \
         --sysroot=<dir> naming a copy of its root, the directory with usr/include in it, or pass \
         -nostdinc for a program that includes none of the library",
        tuple = target.tuple().to_canonical_string()
    )
}

/// The program that starts a dynamic program, whose path is stored in it.
fn loader(target: Triple) -> &'static str {
    match target.os {
        // `FBSD_DYNAMIC_LINKER`, which moved out of `/usr` in FreeBSD 6.
        Os::FreeBsd(release) if release < 6 => "/usr/libexec/ld-elf.so.1",
        Os::FreeBsd(_) => "/libexec/ld-elf.so.1",
        Os::NetBsd => "/usr/libexec/ld.elf_so",
        Os::OpenBsd => "/usr/libexec/ld.so",
        _ => "/usr/libexec/ld-elf.so.2",
    }
}

/// The first start file of the C library, which a shared object does not take.
fn crt(os: Os) -> &'static str {
    match os {
        Os::NetBsd | Os::OpenBsd => "crt0.o",
        _ => "crt1.o",
    }
}

/// The line for a FreeBSD, NetBSD, OpenBSD or DragonFly target.
///
/// # Errors
///
/// [`Error::Cross`] when the target is not this machine and no `--sysroot` names a copy of its
/// root, because there is no tree of a BSD in the cache, and for `-static-pie`, which none of
/// their GCCs links.
pub(super) fn line(
    target: Triple,
    opts: &LinkOptions,
    items: &[Item],
    output: &str,
    host: Option<Triple>,
    ld: Ld,
) -> Result<Vec<String>, Error> {
    let name = system(target.os);
    let root = opts.sysroot.as_deref();
    if root.is_none() && !native(target, host) {
        return Err(Error::Cross {
            why: format!(
                "a link for {tuple} is against the C library of {name}, and this machine is \
                 not {name}. Pass --sysroot=<dir> naming a copy of its root, the directory with \
                 usr/lib/{crt} in it",
                tuple = target.tuple().to_canonical_string(),
                crt = crt(target.os)
            ),
        });
    }
    if opts.is_static && opts.pie == Some(true) {
        return Err(Error::Cross {
            why: format!(
                "-static-pie needs a start file that relocates the program, and the GCC of {name} \
                 has none. Link with -static or with -pie"
            ),
        });
    }
    let dirs = dirs(target, root);
    // `LINK_PIE_SPEC`, which is `-pie` only when the link is neither static nor shared.
    let pie = opts.pie == Some(true) && !opts.shared && !opts.is_static;

    let mut args = Vec::new();
    if let Some(root) = root {
        // Given to the linker as well, as GCC gives it, because `libc.so` on FreeBSD is a script
        // that names `/lib/libc.so.7`, and lld reads that path inside the sysroot only when it
        // was told the sysroot.
        args.push(format!("--sysroot={}", root.display()));
    }
    if !opts.is_static || target.os == Os::DragonFly {
        args.push("--eh-frame-hdr".to_owned());
    }
    args.extend(link_spec(target, opts, items, ld));
    if pie {
        args.push("-pie".to_owned());
    }
    args.push("-o".to_owned());
    args.push(output.to_owned());
    if opts.strip {
        args.push("-s".to_owned());
    }
    if opts.wants_startfiles() {
        args.extend(startfiles(target, opts, pie, &dirs));
    }
    for dir in opts.search.iter().chain(&dirs) {
        args.push(format!("-L{}", dir.display()));
    }
    push_items(&mut args, items);
    if opts.wants_defaultlibs() {
        args.extend(libraries(target, opts, &dirs));
    }
    if opts.wants_startfiles() {
        args.extend(endfiles(target, opts, pie, &dirs));
    }
    Ok(args)
}

/// `LINK_SPEC` of the target, without the words lld refuses when the linker is lld.
fn link_spec(target: Triple, opts: &LinkOptions, items: &[Item], ld: Ld) -> Vec<String> {
    let gnu = ld == Ld::Gnu;
    let mut args = emulation(target);
    let mut push = |word: &str| args.push(word.to_owned());
    let dynamic = !opts.is_static;
    // `%{!e*:-e ...}`, which reads the `-e` of the command line. The driver hands that on as the
    // word `-e` in the items.
    let entry = !opts.no_stdlib && !items.iter().any(|item| *item == Item::Linker("-e".to_owned()));
    let loader = loader(target);
    match (target.os, target.arch) {
        (Os::FreeBsd(_), Arch::Aarch64) => {
            if opts.shared {
                push("-Bshareable");
            }
            if opts.is_static {
                push("-Bstatic");
            } else {
                if opts.export_dynamic {
                    push("-export-dynamic");
                }
                if !opts.shared {
                    push("-dynamic-linker");
                    push(loader);
                }
            }
            push("-X");
            if gnu {
                push("-maarch64fbsd");
            }
        }
        (Os::FreeBsd(_), Arch::Riscv64) => {
            push("-X");
            if opts.shared {
                push("-Bshareable");
            }
            if opts.is_static {
                push("-Bstatic");
            }
            if !opts.shared && dynamic {
                if opts.export_dynamic {
                    push("-export-dynamic");
                }
                push("-dynamic-linker");
                push(loader);
            }
            if !opts.shared && opts.is_static {
                push("-static");
            }
        }
        (Os::FreeBsd(_) | Os::DragonFly, _) => {
            if opts.shared {
                push("-Bshareable");
            } else if dynamic {
                if opts.export_dynamic {
                    push("-export-dynamic");
                }
                push("-dynamic-linker");
                push(loader);
            } else {
                push("-Bstatic");
            }
            if target.os == Os::DragonFly && dynamic {
                push("--hash-style=gnu");
            }
        }
        (Os::NetBsd, _) => {
            if opts.shared {
                push("-shared");
            } else {
                if gnu {
                    push("-dc");
                    push("-dp");
                }
                if entry {
                    push("-e");
                    // `NETBSD_ENTRY_POINT`, which the i386 and AArch64 headers spell with two
                    // underscores and the x86-64 and RISC-V ones with one.
                    push(match target.arch {
                        Arch::X86 | Arch::Aarch64 => "__start",
                        _ => "_start",
                    });
                }
                if dynamic {
                    if opts.export_dynamic {
                        push("-export-dynamic");
                    }
                    push("-dynamic-linker");
                    push(loader);
                } else {
                    push("-static");
                }
            }
            if target.arch == Arch::Aarch64 {
                push("-X");
            }
        }
        _ => {
            if !opts.shared && entry {
                push("-e");
                push("__start");
            }
            if opts.shared {
                push("-shared");
            }
            // `%{static:-Bstatic} %{!static:-Bdynamic}`, and the loader on every line.
            push(if dynamic { "-Bdynamic" } else { "-Bstatic" });
            if dynamic || gnu {
                push("-dynamic-linker");
                push(loader);
            }
        }
    }
    args
}

/// The path of a file in the directories, or its bare name when none has it, which is what GCC's
/// `%s` does and makes the linker say which file is missing.
fn path(dirs: &[PathBuf], name: &str) -> String {
    find_file(dirs, name).map_or_else(|| name.to_owned(), |path| path.display().to_string())
}

/// The path of a file that goes on the line only if it is there, which is `%:if-exists`.
fn if_exists(dirs: &[PathBuf], name: &str) -> Option<String> {
    find_file(dirs, name).map(|path| path.display().to_string())
}

/// `STARTFILE_SPEC`.
fn startfiles(target: Triple, opts: &LinkOptions, pie: bool, dirs: &[PathBuf]) -> Vec<String> {
    let mut files = Vec::new();
    let moves = opts.shared || pie;
    match target.os {
        Os::NetBsd => {
            if !opts.shared {
                files.push(path(dirs, if opts.profile { "gcrt0.o" } else { "crt0.o" }));
            }
            files.extend(if_exists(dirs, "crti.o"));
            let begin = if opts.is_static {
                if_exists(dirs, "crtbeginT.o").unwrap_or_else(|| path(dirs, "crtbegin.o"))
            } else {
                path(dirs, if moves { "crtbeginS.o" } else { "crtbegin.o" })
            };
            files.push(begin);
        }
        Os::OpenBsd => {
            if opts.shared {
                files.push(path(dirs, "crtbeginS.o"));
            } else {
                files.push(path(dirs, if opts.profile { "gcrt0.o" } else { "crt0.o" }));
                files.push(path(dirs, "crtbegin.o"));
            }
        }
        _ => {
            if !opts.shared {
                let first = if opts.profile {
                    "gcrt1.o"
                } else if pie {
                    "Scrt1.o"
                } else {
                    "crt1.o"
                };
                files.push(path(dirs, first));
            }
            files.push(path(dirs, "crti.o"));
            // DragonFly's GCC has no `crtbeginT.o`, and a static program takes the plain one.
            let begin = if opts.is_static && target.os != Os::DragonFly {
                "crtbeginT.o"
            } else if moves {
                "crtbeginS.o"
            } else {
                "crtbegin.o"
            };
            files.push(path(dirs, begin));
        }
    }
    files
}

/// `ENDFILE_SPEC`.
fn endfiles(target: Triple, opts: &LinkOptions, pie: bool, dirs: &[PathBuf]) -> Vec<String> {
    let mut files = Vec::new();
    let moves = opts.shared || pie;
    match target.os {
        Os::OpenBsd => {
            files.push(path(dirs, if opts.shared { "crtendS.o" } else { "crtend.o" }));
        }
        Os::NetBsd => {
            files.push(path(dirs, if moves { "crtendS.o" } else { "crtend.o" }));
            files.extend(if_exists(dirs, "crtn.o"));
        }
        _ => {
            // `GNU_USER_TARGET_MATHFILE_SPEC`, which only the AArch64 FreeBSD header has.
            if target.arch == Arch::Aarch64 && opts.fast_math && !opts.shared {
                files.extend(if_exists(dirs, "crtfastmath.o"));
            }
            files.push(path(dirs, if moves { "crtendS.o" } else { "crtend.o" }));
            files.push(path(dirs, "crtn.o"));
        }
    }
    files
}

/// `LINK_GCC_C_SEQUENCE_SPEC`, which is `libgcc`, the C library and `libgcc` again, with our own
/// runtime in front of the second `libgcc`, as it is in front of `libgcc` on the Linux line.
///
/// The `libgcc` words are left off when the tree has no `libgcc.a`. OpenBSD's base system has
/// none, because its compiler is clang, which links `libcompiler_rt.a` instead, and our runtime
/// answers for the same calls.
fn libraries(target: Triple, opts: &LinkOptions, dirs: &[PathBuf]) -> Vec<String> {
    let mut args = Vec::new();
    if opts.gcov && find_file(dirs, "libgcov.a").is_some() {
        args.push("-lgcov".to_owned());
    }
    let gcc = if find_file(dirs, "libgcc.a").is_some() { libgcc(target, opts) } else { Vec::new() };
    args.extend(gcc.iter().cloned());
    args.extend(libc(target, opts));
    if !opts.no_builtins_lib {
        if let Some(ours) = builtins_archive(target, &opts.prefixes) {
            args.push(ours.display().to_string());
        }
    }
    args.extend(gcc);
    args
}

/// The `libgcc` words, which are `LIBGCC_SPEC` as `gcc.cc` rewrites it for a target with a
/// shared `libgcc`.
///
/// FreeBSD and DragonFly set `USE_LD_AS_NEEDED`, so a dynamic link asks for `libgcc_s` only if
/// something still needs it. OpenBSD does not, and its `LINK_EH_SPEC` makes a shared object take
/// the static `libgcc` alone. NetBSD's own GCC sets `REAL_LIBGCC_SPEC`, which has no `libgcc_eh`.
fn libgcc(target: Triple, opts: &LinkOptions) -> Vec<String> {
    let words: &[&str] = match target.os {
        Os::NetBsd if opts.is_static => &["-lgcc"],
        Os::NetBsd => &["--as-needed", "-lgcc_s", "--no-as-needed", "-lgcc"],
        Os::OpenBsd if opts.shared && !opts.is_static => &["-lgcc"],
        Os::OpenBsd => &["-lgcc", "-lgcc_eh"],
        _ if opts.is_static => &["-lgcc", "-lgcc_eh"],
        _ => &["-lgcc", "--as-needed", "-lgcc_s", "--no-as-needed"],
    };
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// `LIB_SPEC`, without `-lpthread`, which the driver puts after the objects for `-pthread`.
fn libc(target: Triple, opts: &LinkOptions) -> Option<String> {
    let profiled = opts.profile && !opts.shared;
    match target.os {
        // The profiled C library left FreeBSD in release 14, and `FBSD_LINK_PG_THREADS` stopped
        // naming it there.
        Os::FreeBsd(release) if profiled && release < 14 => Some("-lc_p".to_owned()),
        Os::NetBsd if profiled => Some("-lc_p".to_owned()),
        // A shared object on OpenBSD does not name the C library, which the program brings.
        Os::OpenBsd if opts.shared => None,
        _ => Some("-lc".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rucc_target::Env;

    use super::*;

    fn bsd(arch: Arch, os: Os) -> Triple {
        Triple::new(arch, os, Env::None)
    }

    fn freebsd() -> Triple {
        bsd(Arch::X86_64, Os::FreeBsd(14))
    }

    fn linux() -> Option<Triple> {
        Some(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// A copy of a BSD root with those files in it, empty, under a directory of its own.
    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rucc-bsd-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("usr/lib")).expect("a directory");
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().expect("a directory")).expect("a directory");
            fs::write(&path, b"").expect("a file");
        }
        root
    }

    /// The files a FreeBSD or DragonFly root has in `/usr/lib`.
    const FREEBSD: &[&str] = &[
        "usr/lib/crt1.o",
        "usr/lib/Scrt1.o",
        "usr/lib/gcrt1.o",
        "usr/lib/crti.o",
        "usr/lib/crtn.o",
        "usr/lib/crtbegin.o",
        "usr/lib/crtbeginS.o",
        "usr/lib/crtbeginT.o",
        "usr/lib/crtend.o",
        "usr/lib/crtendS.o",
        "usr/lib/libgcc.a",
    ];

    /// The options for a link against that root, with our runtime left off so that the line does
    /// not depend on whether one was built.
    fn against(root: &Path) -> LinkOptions {
        LinkOptions {
            sysroot: Some(root.to_path_buf()),
            no_builtins_lib: true,
            ..LinkOptions::default()
        }
    }

    /// The line with the root written as `=`, so that it can be compared as text.
    fn text(target: Triple, opts: &LinkOptions, items: &[Item]) -> String {
        let root = opts.sysroot.as_ref().expect("a sysroot").display().to_string();
        let args = line(target, opts, items, "a.out", linux(), Ld::Lld).expect("a line");
        args.join(" ").replace(&root, "=").replace('\\', "/")
    }

    /// The line GNU ld gets, written the same way.
    fn gnu(target: Triple, opts: &LinkOptions) -> String {
        let root = opts.sysroot.as_ref().expect("a sysroot").display().to_string();
        let args = line(target, opts, &[], "a.out", linux(), Ld::Gnu).expect("a line");
        args.join(" ").replace(&root, "=").replace('\\', "/")
    }

    fn main_o() -> Vec<Item> {
        vec![Item::File("main.o".to_owned())]
    }

    #[test]
    fn a_freebsd_program_is_linked_the_way_gcc_links_it() {
        let root = tree("freebsd", FREEBSD);
        assert_eq!(
            text(freebsd(), &against(&root), &main_o()),
            "--sysroot== --eh-frame-hdr -m elf_x86_64_fbsd -dynamic-linker /libexec/ld-elf.so.1 \
             -o a.out =/usr/lib/crt1.o =/usr/lib/crti.o =/usr/lib/crtbegin.o -L=/usr/lib main.o \
             -lgcc --as-needed -lgcc_s --no-as-needed -lc -lgcc --as-needed -lgcc_s \
             --no-as-needed =/usr/lib/crtend.o =/usr/lib/crtn.o"
        );
    }

    #[test]
    fn a_freebsd_shared_object_static_program_and_pie_take_their_own_files() {
        let root = tree("freebsd-kinds", FREEBSD);
        let shared = text(freebsd(), &LinkOptions { shared: true, ..against(&root) }, &[]);
        assert!(shared.contains("-m elf_x86_64_fbsd -Bshareable -o"), "{shared}");
        assert!(shared.contains("-o a.out =/usr/lib/crti.o =/usr/lib/crtbeginS.o"), "{shared}");
        assert!(shared.ends_with("=/usr/lib/crtendS.o =/usr/lib/crtn.o"), "{shared}");
        assert!(!shared.contains("dynamic-linker"), "{shared}");

        let fixed = text(freebsd(), &LinkOptions { is_static: true, ..against(&root) }, &[]);
        assert!(fixed.starts_with("--sysroot== -m elf_x86_64_fbsd -Bstatic -o"), "{fixed}");
        assert!(fixed.contains("=/usr/lib/crtbeginT.o"), "{fixed}");
        assert!(fixed.contains("-lgcc -lgcc_eh -lc -lgcc -lgcc_eh"), "{fixed}");

        let pie = text(freebsd(), &LinkOptions { pie: Some(true), ..against(&root) }, &[]);
        assert!(pie.contains("/libexec/ld-elf.so.1 -pie -o a.out =/usr/lib/Scrt1.o"), "{pie}");
        assert!(pie.contains("=/usr/lib/crtbeginS.o"), "{pie}");
        assert!(pie.contains("=/usr/lib/crtendS.o"), "{pie}");

        let exported = LinkOptions { export_dynamic: true, strip: true, ..against(&root) };
        let exported = text(freebsd(), &exported, &[]);
        assert!(exported.contains("-export-dynamic -dynamic-linker"), "{exported}");
        assert!(exported.contains("-o a.out -s "), "{exported}");
    }

    #[test]
    fn each_freebsd_machine_has_the_words_of_its_own_header() {
        let root = tree("freebsd-arches", FREEBSD);
        let i386 = text(bsd(Arch::X86, Os::FreeBsd(14)), &against(&root), &[]);
        assert!(i386.starts_with("--sysroot== --eh-frame-hdr -dynamic-linker "), "{i386}");

        let arm = bsd(Arch::Aarch64, Os::FreeBsd(14));
        let program = text(arm, &against(&root), &[]);
        assert!(program.contains("--eh-frame-hdr -dynamic-linker /libexec/ld-elf.so.1 -X -o"));
        assert!(!program.contains("aarch64fbsd"), "{program}");
        let shared = text(arm, &LinkOptions { shared: true, ..against(&root) }, &[]);
        assert!(shared.contains("--eh-frame-hdr -Bshareable -X -o"), "{shared}");

        let riscv = bsd(Arch::Riscv64, Os::FreeBsd(14));
        let program = text(riscv, &against(&root), &[]);
        assert!(program.contains("-melf64lriscv -X -dynamic-linker /libexec/ld-elf.so.1 -o"));
        let fixed = text(riscv, &LinkOptions { is_static: true, ..against(&root) }, &[]);
        assert!(fixed.contains("-melf64lriscv -X -Bstatic -static -o"), "{fixed}");

        let five = text(bsd(Arch::X86_64, Os::FreeBsd(5)), &against(&root), &[]);
        assert!(five.contains("-dynamic-linker /usr/libexec/ld-elf.so.1"), "{five}");
    }

    #[test]
    fn fast_math_takes_its_startup_file_on_aarch64_freebsd_when_there_is_one() {
        let mut files = FREEBSD.to_vec();
        files.push("usr/lib/crtfastmath.o");
        let root = tree("freebsd-fast", &files);
        let fast = LinkOptions { fast_math: true, ..against(&root) };
        let arm = text(bsd(Arch::Aarch64, Os::FreeBsd(14)), &fast, &[]);
        assert!(arm.ends_with("=/usr/lib/crtfastmath.o =/usr/lib/crtend.o =/usr/lib/crtn.o"));
        assert!(!text(freebsd(), &fast, &[]).contains("crtfastmath"));
    }

    /// The files a NetBSD root has, without `crtbeginT.o`, which only some ports have.
    const NETBSD: &[&str] = &[
        "usr/lib/crt0.o",
        "usr/lib/gcrt0.o",
        "usr/lib/crti.o",
        "usr/lib/crtn.o",
        "usr/lib/crtbegin.o",
        "usr/lib/crtbeginS.o",
        "usr/lib/crtend.o",
        "usr/lib/crtendS.o",
        "usr/lib/libgcc.a",
    ];

    #[test]
    fn a_netbsd_program_is_linked_the_way_gcc_links_it() {
        let root = tree("netbsd", NETBSD);
        assert_eq!(
            text(bsd(Arch::X86_64, Os::NetBsd), &against(&root), &main_o()),
            "--sysroot== --eh-frame-hdr -e _start -dynamic-linker /usr/libexec/ld.elf_so -o a.out \
             =/usr/lib/crt0.o =/usr/lib/crti.o =/usr/lib/crtbegin.o -L=/usr/lib main.o \
             --as-needed -lgcc_s --no-as-needed -lgcc -lc --as-needed -lgcc_s --no-as-needed \
             -lgcc =/usr/lib/crtend.o =/usr/lib/crtn.o"
        );
    }

    #[test]
    fn each_netbsd_machine_has_its_entry_point_and_words() {
        let root = tree("netbsd-arches", NETBSD);
        let i386 = text(bsd(Arch::X86, Os::NetBsd), &against(&root), &[]);
        assert!(i386.contains("--eh-frame-hdr -e __start -dynamic-linker"), "{i386}");
        let arm = text(bsd(Arch::Aarch64, Os::NetBsd), &against(&root), &[]);
        assert!(arm.contains("-e __start -dynamic-linker /usr/libexec/ld.elf_so -X -o"), "{arm}");
        let riscv = text(bsd(Arch::Riscv64, Os::NetBsd), &against(&root), &[]);
        assert!(riscv.contains("--eh-frame-hdr -melf64lriscv -e _start -dynamic-linker"));
        for line in [&i386, &arm, &riscv] {
            assert!(!line.contains("-dc") && !line.contains("-dp"), "{line}");
        }
    }

    #[test]
    fn a_netbsd_link_takes_crti_and_crtbegint_only_when_the_tree_has_them() {
        let target = bsd(Arch::X86_64, Os::NetBsd);
        let bare = tree("netbsd-bare", &["usr/lib/crt0.o", "usr/lib/crtbegin.o"]);
        let fixed = text(target, &LinkOptions { is_static: true, ..against(&bare) }, &[]);
        assert!(fixed.contains("-o a.out =/usr/lib/crt0.o =/usr/lib/crtbegin.o -L"), "{fixed}");
        assert!(!fixed.contains("crti") && !fixed.contains("crtn"), "{fixed}");
        assert!(fixed.contains("-e _start -static -o"), "{fixed}");
        assert!(!fixed.contains("-lgcc"), "{fixed}");

        let mut files = NETBSD.to_vec();
        files.push("usr/lib/crtbeginT.o");
        let full = tree("netbsd-full", &files);
        let fixed = text(target, &LinkOptions { is_static: true, ..against(&full) }, &[]);
        assert!(fixed.contains("=/usr/lib/crti.o =/usr/lib/crtbeginT.o"), "{fixed}");
        assert!(fixed.contains("-lgcc -lc -lgcc =/usr/lib/crtend.o"), "{fixed}");
    }

    #[test]
    fn an_entry_point_on_the_command_line_replaces_the_one_of_the_line() {
        let root = tree("netbsd-entry", NETBSD);
        let items = [Item::Linker("-e".to_owned()), Item::Linker("begin".to_owned())];
        let named = text(bsd(Arch::X86_64, Os::NetBsd), &against(&root), &items);
        assert!(!named.contains("-e _start"), "{named}");
        assert!(named.contains("-L=/usr/lib -e begin"), "{named}");
        let none = LinkOptions { no_stdlib: true, ..against(&root) };
        let none = text(bsd(Arch::X86_64, Os::OpenBsd), &none, &[]);
        assert_eq!(
            none,
            "--sysroot== --eh-frame-hdr -Bdynamic -dynamic-linker /usr/libexec/ld.so \
                          -o a.out -L=/usr/lib"
        );
    }

    /// An OpenBSD root, which has no `libgcc.a` because its compiler is clang.
    const OPENBSD: &[&str] = &[
        "usr/lib/crt0.o",
        "usr/lib/gcrt0.o",
        "usr/lib/crtbegin.o",
        "usr/lib/crtbeginS.o",
        "usr/lib/crtend.o",
        "usr/lib/crtendS.o",
    ];

    #[test]
    fn an_openbsd_program_is_linked_the_way_gcc_links_it() {
        let root = tree("openbsd", OPENBSD);
        let target = bsd(Arch::X86_64, Os::OpenBsd);
        assert_eq!(
            text(target, &against(&root), &main_o()),
            "--sysroot== --eh-frame-hdr -e __start -Bdynamic -dynamic-linker /usr/libexec/ld.so \
             -o a.out =/usr/lib/crt0.o =/usr/lib/crtbegin.o -L=/usr/lib main.o -lc \
             =/usr/lib/crtend.o"
        );
        let shared = text(target, &LinkOptions { shared: true, ..against(&root) }, &[]);
        assert_eq!(
            shared,
            "--sysroot== --eh-frame-hdr -shared -Bdynamic -dynamic-linker /usr/libexec/ld.so \
             -o a.out =/usr/lib/crtbeginS.o -L=/usr/lib =/usr/lib/crtendS.o"
        );
        let fixed = text(target, &LinkOptions { is_static: true, ..against(&root) }, &[]);
        assert!(fixed.starts_with("--sysroot== -e __start -Bstatic -o a.out"), "{fixed}");
        assert!(!fixed.contains("ld.so"), "a static program names no loader: {fixed}");
    }

    #[test]
    fn openbsd_takes_libgcc_where_its_gcc_does_when_the_tree_has_one() {
        let mut files = OPENBSD.to_vec();
        files.push("usr/lib/libgcc.a");
        let root = tree("openbsd-gcc", &files);
        let target = bsd(Arch::X86_64, Os::OpenBsd);
        let program = text(target, &against(&root), &[]);
        assert!(program.contains("-L=/usr/lib -lgcc -lgcc_eh -lc -lgcc -lgcc_eh ="), "{program}");
        let shared = text(target, &LinkOptions { shared: true, ..against(&root) }, &[]);
        assert!(shared.contains("-L=/usr/lib -lgcc -lgcc ="), "{shared}");
    }

    #[test]
    fn a_dragonfly_program_is_linked_the_way_gcc_links_it() {
        let mut files: Vec<&str> =
            FREEBSD.iter().copied().filter(|file| !file.contains("crtbegin")).collect();
        files.retain(|file| !file.contains("crtend") && !file.contains("libgcc"));
        files.extend([
            "usr/lib/gcc80/crtbegin.o",
            "usr/lib/gcc80/crtend.o",
            "usr/lib/gcc80/libgcc.a",
        ]);
        let root = tree("dragonfly", &files);
        let target = bsd(Arch::X86_64, Os::DragonFly);
        assert_eq!(
            text(target, &against(&root), &main_o()),
            "--sysroot== --eh-frame-hdr -dynamic-linker /usr/libexec/ld-elf.so.2 \
             --hash-style=gnu -o a.out =/usr/lib/crt1.o =/usr/lib/crti.o \
             =/usr/lib/gcc80/crtbegin.o -L=/usr/lib/gcc80 -L=/usr/lib main.o -lgcc --as-needed \
             -lgcc_s --no-as-needed -lc -lgcc --as-needed -lgcc_s --no-as-needed \
             =/usr/lib/gcc80/crtend.o =/usr/lib/crtn.o"
        );
        let fixed = text(target, &LinkOptions { is_static: true, ..against(&root) }, &[]);
        assert!(fixed.starts_with("--sysroot== --eh-frame-hdr -Bstatic -o a.out"), "{fixed}");
        assert!(fixed.contains("=/usr/lib/gcc80/crtbegin.o"), "no crtbeginT: {fixed}");
    }

    #[test]
    fn a_profiled_program_takes_the_profiled_start_file_and_library_of_its_release() {
        let root = tree("profiled", &[]);
        let profiled = LinkOptions { profile: true, ..against(&root) };
        let fourteen = text(freebsd(), &profiled, &[]);
        assert!(fourteen.contains("-o a.out gcrt1.o crti.o"), "{fourteen}");
        assert!(fourteen.contains(" -lc ") && !fourteen.contains("-lc_p"), "{fourteen}");
        let thirteen = text(bsd(Arch::X86_64, Os::FreeBsd(13)), &profiled, &[]);
        assert!(thirteen.contains(" -lc_p "), "{thirteen}");
        let netbsd = text(bsd(Arch::X86_64, Os::NetBsd), &profiled, &[]);
        assert!(netbsd.contains("-o a.out gcrt0.o crtbegin.o") && netbsd.contains(" -lc_p "));
    }

    #[test]
    fn our_runtime_goes_between_the_c_library_and_the_second_libgcc() {
        let root = tree("ours", FREEBSD);
        let prefix = std::env::temp_dir().join(format!("rucc-bsd-prefix-{}", std::process::id()));
        fs::create_dir_all(&prefix).expect("a directory");
        let archive = prefix.join(rucc_sysroot::link::BUILTINS);
        fs::write(&archive, b"").expect("a file");
        let opts = LinkOptions { no_builtins_lib: false, prefixes: vec![prefix], ..against(&root) };
        let args = line(freebsd(), &opts, &[], "a.out", linux(), Ld::Lld).expect("a line");
        let at = |word: &str| args.iter().position(|arg| arg == word).expect(word);
        let ours = at(&archive.display().to_string());
        assert_eq!(args[ours - 1], "-lc");
        assert_eq!(args[ours + 1], "-lgcc");
    }

    #[test]
    fn a_link_for_another_machine_needs_a_sysroot_and_its_own_machine_does_not() {
        let opts = LinkOptions { no_builtins_lib: true, ..LinkOptions::default() };
        let Err(Error::Cross { why }) = line(freebsd(), &opts, &[], "a.out", linux(), Ld::Lld)
        else {
            panic!("a FreeBSD link on Linux with no sysroot was not refused");
        };
        assert!(why.contains("--sysroot=<dir>") && why.contains("usr/lib/crt1.o"), "{why}");
        let Err(Error::Cross { why }) =
            line(bsd(Arch::X86_64, Os::NetBsd), &opts, &[], "a.out", linux(), Ld::Lld)
        else {
            panic!("a NetBSD link on Linux with no sysroot was not refused");
        };
        assert!(why.contains("usr/lib/crt0.o"), "{why}");

        let host = Some(bsd(Arch::X86_64, Os::FreeBsd(15)));
        let args = line(freebsd(), &opts, &[], "a.out", host, Ld::Lld).expect("a native line");
        assert!(!args.iter().any(|arg| arg.starts_with("--sysroot")), "{args:?}");
        assert!(args.contains(&"-L/usr/lib".to_owned()), "{args:?}");
    }

    #[test]
    fn a_static_pie_is_refused_by_name() {
        let root = tree("static-pie", FREEBSD);
        let opts = LinkOptions { is_static: true, pie: Some(true), ..against(&root) };
        let Err(Error::Cross { why }) = line(freebsd(), &opts, &[], "a.out", linux(), Ld::Lld)
        else {
            panic!("-static-pie was not refused");
        };
        assert!(why.starts_with("-static-pie"), "{why}");
    }

    #[test]
    fn gnu_ld_gets_the_three_words_that_lld_refuses() {
        let root = tree("gnu-ld", FREEBSD);
        let arm = bsd(Arch::Aarch64, Os::FreeBsd(14));
        let program = gnu(arm, &against(&root));
        assert!(program.contains("-dynamic-linker /libexec/ld-elf.so.1 -X -maarch64fbsd -o"));
        let shared = gnu(arm, &LinkOptions { shared: true, ..against(&root) });
        assert!(shared.contains("-Bshareable -X -maarch64fbsd -o"), "{shared}");

        let netbsd = bsd(Arch::X86_64, Os::NetBsd);
        let program = gnu(netbsd, &against(&root));
        assert!(program.contains("--eh-frame-hdr -dc -dp -e _start -dynamic-linker"), "{program}");
        let fixed = gnu(netbsd, &LinkOptions { is_static: true, ..against(&root) });
        assert!(fixed.contains("-dc -dp -e _start -static -o"), "{fixed}");
        // `%{!shared:-dc -dp ...}`, so a shared object has neither.
        let shared = gnu(netbsd, &LinkOptions { shared: true, ..against(&root) });
        assert!(!shared.contains("-dc") && !shared.contains("-dp"), "{shared}");

        let openbsd = bsd(Arch::X86_64, Os::OpenBsd);
        let fixed = gnu(openbsd, &LinkOptions { is_static: true, ..against(&root) });
        assert!(fixed.contains("-e __start -Bstatic -dynamic-linker /usr/libexec/ld.so -o"));
        // The other lines are the same for both linkers.
        for target in [freebsd(), bsd(Arch::X86, Os::NetBsd), bsd(Arch::X86_64, Os::DragonFly)] {
            let lld = line(target, &against(&root), &[], "a.out", linux(), Ld::Lld);
            let same = lld.expect("a line").join(" ").replace(&root.display().to_string(), "=");
            let same = same.replace('\\', "/");
            if target.os == Os::NetBsd {
                assert_eq!(gnu(target, &against(&root)).replace(" -dc -dp", ""), same);
            } else {
                assert_eq!(gnu(target, &against(&root)), same);
            }
        }
    }

    #[test]
    fn a_relocatable_link_by_gnu_ld_takes_its_words_too() {
        let arm = bsd(Arch::Aarch64, Os::FreeBsd(14));
        assert_eq!(relocatable(arm, Ld::Gnu), ["-maarch64fbsd"]);
        assert!(relocatable(arm, Ld::Lld).is_empty());
        let netbsd = bsd(Arch::Riscv64, Os::NetBsd);
        assert_eq!(relocatable(netbsd, Ld::Gnu), ["-melf64lriscv", "-dc", "-dp"]);
        assert_eq!(relocatable(netbsd, Ld::Lld), ["-melf64lriscv"]);
        assert_eq!(relocatable(freebsd(), Ld::Gnu), ["-m", "elf_x86_64_fbsd"]);
    }

    #[test]
    fn the_version_says_which_linker_it_is() {
        assert_eq!(reads("GNU ld (GNU Binutils) 2.44\nCopyright (C) 2025"), Ld::Gnu);
        assert_eq!(reads("GNU ld (NetBSD Binutils nb1) 2.45.1"), Ld::Gnu);
        assert_eq!(
            reads(
                "LLD 19.1.7 (FreeBSD llvmorg-19.1.7-0-gcd708029e0b2-1500001) \
             (compatible with GNU linkers)"
            ),
            Ld::Lld
        );
        assert_eq!(reads("mold 2.40.0 (compatible with GNU ld)"), Ld::Lld);
        assert_eq!(reads(""), Ld::Lld);
    }

    #[test]
    fn a_link_for_another_machine_looks_for_ld_in_the_b_directories_first() {
        let dirs = [PathBuf::from("/w/usr/bin"), PathBuf::from("/x/bin")];
        let ld = |dir: &str| Path::new(dir).join("ld").display().to_string();
        assert_eq!(
            order(freebsd(), linux(), &dirs),
            [
                ld("/w/usr/bin"),
                ld("/x/bin"),
                "x86_64-unknown-freebsd14-ld".to_owned(),
                "ld.lld".to_owned(),
                "lld".to_owned()
            ]
        );
        // On the machine itself the name `ld` is looked for, in the `-B` directories and then on
        // `PATH`.
        let host = Some(bsd(Arch::X86_64, Os::FreeBsd(15)));
        assert_eq!(order(freebsd(), host, &dirs), ["ld", "ld.lld", "lld"]);
    }

    #[test]
    fn the_machines_own_linker_is_first_only_on_that_machine() {
        let cross = ["x86_64-unknown-freebsd14-ld", "ld.lld", "lld"];
        assert_eq!(order(freebsd(), linux(), &[]), cross);
        let host = Some(bsd(Arch::X86_64, Os::FreeBsd(13)));
        assert_eq!(order(freebsd(), host, &[]), ["ld", "ld.lld", "lld"]);
        let arm = Some(bsd(Arch::Aarch64, Os::FreeBsd(14)));
        assert_eq!(order(freebsd(), arm, &[]), cross);
    }

    #[test]
    fn a_cross_binutils_on_path_is_looked_for_by_the_name_of_the_target() {
        let names = |arch, os| order(bsd(arch, os), linux(), &[]).remove(0);
        assert_eq!(names(Arch::Aarch64, Os::FreeBsd(14)), "aarch64-unknown-freebsd14-ld");
        assert_eq!(names(Arch::X86_64, Os::NetBsd), "x86_64-unknown-netbsd-ld");
        assert_eq!(names(Arch::X86, Os::OpenBsd), "i686-unknown-openbsd-ld");
        assert_eq!(names(Arch::X86_64, Os::DragonFly), "x86_64-unknown-dragonfly-ld");
    }
}
