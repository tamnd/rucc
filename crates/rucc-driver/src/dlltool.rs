//! `rucc --dlltool`, which writes an import library from a module definition file.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` section 9.4, which says turning a `.def` file into
//! an import library is a mechanical transformation, and `rucc-stub` is where it is done:
//! [`rucc_stub::def::read`] reads the file and [`rucc_stub::coff::write`] writes the archive, byte for
//! byte the file `llvm-dlltool` writes. This is the command line in front of those two, so that a
//! build which expects a `dlltool` on the path can be handed this compiler instead.
//!
//! The build it is for is mingw-w64's own. `mingw-w64-crt` makes several hundred import libraries,
//! one per `.def` file it checks in, and it makes every one of them with the same command:
//!
//! ```text
//! $(DLLTOOL) --as-flags=--64 -m i386:x86-64 -k --as=$(AS) --output-lib $@ --input-def X.def
//! ```
//!
//! with `--as-flags=--32 -m i386` for 32-bit x86 and `-m arm64` for ARM64. So what is read here is
//! the part of GNU dlltool's command line that asks for an import library from a `.def` and nothing
//! else: the machine, the input, the output, the DLL name, `-k`, and the two options about an
//! assembler, which are taken and dropped because nothing here runs one. GNU dlltool can also build
//! the export side of a DLL out of object files, and that half is not here at all.
//!
//! Anything else is refused by name rather than skipped, and one refusal is load bearing.
//! mingw-w64's configure asks whether the dlltool takes `--temp-prefix` by running it with that
//! option and looking at the exit status, and adds it to every later command when it does. The
//! answer here is no, which is the answer `llvm-dlltool` gives and the one that keeps the command
//! above the command that runs.
//!
//! There are two ways in. `rucc --dlltool ARGS` is the one that needs nothing on the disk, and a
//! program name ending in `dlltool`, such as a link called `x86_64-w64-mingw32-dlltool`, is the one
//! a build that looks for `<host>-dlltool` on the path finds. With the second, what comes before
//! `-dlltool` is the target when no `-m` says otherwise, which is how a prefixed GNU tool behaves.
//!
//! # `-k` on i386
//!
//! With `-k`, `GetProcAddress@8` in the file is imported as `GetProcAddress`, which is what a system
//! DLL exports and what every mingw-w64 build asks for. Without it the DLL is asked for `f@8`, which
//! is what a DLL that gcc linked without `--kill-at` exports, and both dlltools do the same. The
//! symbol a program links against is `_f@8` either way, so a caller that left the `@N` out fails to
//! link. See [`rucc_stub::coff::Decoration`]. On every other machine there is no decoration and `-k`
//! changes nothing.
//!
//! # Where this is stricter than GNU dlltool
//!
//! A `-D` name with no dot in it. Both dlltools write such a name as it is, and a `LIBRARY` line with
//! no dot gets `.dll` added, which is the rule [`rucc_stub::def::Module::dll`] applies. Rather than
//! apply that rule to `-D` as well, and write a library that names a different DLL from the one the
//! command line named, a `-D` has to name the file as it is on disk.

use std::io::Write;

use rucc_stub::coff::Decoration;
use rucc_tuple::{Os, TargetTuple};

/// What `rucc --dlltool --help` prints.
const USAGE: &str = "\
usage: rucc --dlltool -m <machine> -d <file.def> -l <lib.a> [-D <name.dll>] [-k]

Writes an import library from a module definition file, as `dlltool -d X.def -l libX.a` does.
A program named <host>-dlltool is this as well, with <host> as the target.

options:
  -m, --machine <m>        i386, i386:x86-64, arm or arm64, and the target by default
  -d, --input-def <file>   the module definition file to read
  -l, --output-lib <file>  the import library to write
  -D, --dllname <name>     the DLL to import from, instead of the file's LIBRARY line
  -k, --kill-at            import an i386 name without its @N, as a system DLL exports it
  -S, --as <prog>, --as-flags <flags>   taken and ignored, since no assembler is run
  -h, --help               print this message and exit
";

/// What one command line asked for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Request {
    /// The `-m` value, as written.
    machine: Option<String>,
    /// The `.def` file.
    input: Option<String>,
    /// The import library to write.
    output: Option<String>,
    /// The DLL name from `-D`, which replaces the file's `LIBRARY`.
    dll: Option<String>,
    /// Whether `-k` was given.
    kill_at: bool,
    /// Whether `--help` was, which answers before anything else is checked.
    help: bool,
}

/// Whether a program name is a dlltool, which is the name without its directory and without a
/// trailing `.exe` being `dlltool` or ending in `-dlltool`.
pub fn is_dlltool(program: &str) -> bool {
    let name = base_name(program);
    name == "dlltool" || name.ends_with("-dlltool")
}

/// The program name without its directory and without a trailing `.exe`.
fn base_name(program: &str) -> &str {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name)
}

/// The target a program name implies, from `x86_64-w64-mingw32-dlltool` or from
/// `x86_64-windows-gnu-rucc`, when what comes before the suffix is a Windows target.
fn target_from_name(program: &str) -> Option<TargetTuple> {
    let name = base_name(program);
    let prefix = name.strip_suffix("-dlltool").or_else(|| name.strip_suffix("-rucc"))?;
    let tuple = prefix.parse::<TargetTuple>().ok()?;
    (tuple.os() == Os::Windows).then_some(tuple)
}

/// The target a `-m` value names.
///
/// The names are GNU dlltool's, which are BFD's architecture names, and they are the ones
/// mingw-w64's Makefile writes. `llvm-dlltool` takes the same four. The environment is always
/// `gnu`, since an import library for a mingw-w64 build is what this is for, and the environment
/// changes nothing about the file.
fn target_from_machine(machine: &str) -> Result<TargetTuple, String> {
    let tuple = match machine {
        "i386" => "i686-windows-gnu",
        "i386:x86-64" => "x86_64-windows-gnu",
        "arm" => "armv7-windows-gnu",
        "arm64" => "aarch64-windows-gnu",
        "arm64ec" => {
            return Err(
                "-m arm64ec: an ARM64EC import library needs every function name mangled, which \
                 this dlltool does not do"
                    .to_owned(),
            );
        }
        other => {
            return Err(format!(
                "-m {other} is not a machine this dlltool knows, which are i386, i386:x86-64, \
                 arm and arm64"
            ));
        }
    };
    tuple.parse().map_err(|e| format!("-m {machine}: {e}"))
}

/// Reads a dlltool command line.
///
/// GNU dlltool reads its options with `getopt_long`, so a short option takes its value joined or as
/// the next word and a long one takes it after `=` or as the next word. Both are read here, and a
/// value given twice is the last one, which is also `getopt_long`'s answer.
fn parse(args: &[String]) -> Result<Request, String> {
    let mut request = Request::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        let (flag, joined) = match arg.split_once('=') {
            Some((flag, value)) if arg.starts_with("--") => (flag, Some(value)),
            _ => (arg, None),
        };
        let slot = match flag {
            "-m" | "--machine" => Some(&mut request.machine),
            "-d" | "--input-def" => Some(&mut request.input),
            "-l" | "--output-lib" => Some(&mut request.output),
            "-D" | "--dllname" => Some(&mut request.dll),
            _ => None,
        };
        if let Some(slot) = slot {
            *slot = Some(value(flag, joined, args, &mut i)?);
            continue;
        }
        match flag {
            "-k" | "--kill-at" if joined.is_none() => request.kill_at = true,
            "-h" | "--help" if joined.is_none() => request.help = true,
            // The assembler and what to hand it. GNU dlltool runs `as` to build the objects in an
            // import library and these name it, and nothing here runs one, so the value is read to
            // keep the words after it in step and then dropped.
            "-S" | "--as" | "--as-flags" => {
                value(flag, joined, args, &mut i)?;
            }
            _ => {
                // `-mi386` and `-dX.def`, the joined spelling of a short option.
                let short = ["-m", "-d", "-l", "-D", "-S"].into_iter().find(|short| {
                    arg.len() > 2 && arg.starts_with(short) && !arg.starts_with("--")
                });
                match short {
                    Some("-m") => request.machine = Some(arg[2..].to_owned()),
                    Some("-d") => request.input = Some(arg[2..].to_owned()),
                    Some("-l") => request.output = Some(arg[2..].to_owned()),
                    Some("-D") => request.dll = Some(arg[2..].to_owned()),
                    Some(_) => {}
                    None => {
                        return Err(format!(
                            "{arg} is not an option this dlltool has. It writes an import library \
                             from a .def file and takes -m, -d, -l, -D, -k, --as and --as-flags"
                        ));
                    }
                }
            }
        }
    }
    Ok(request)
}

/// The value of an option, joined to it after `=` or the next word.
fn value(
    flag: &str,
    joined: Option<&str>,
    args: &[String],
    i: &mut usize,
) -> Result<String, String> {
    if let Some(value) = joined {
        return Ok(value.to_owned());
    }
    let next = args.get(*i).ok_or_else(|| format!("{flag} requires an argument"))?;
    *i += 1;
    Ok(next.clone())
}

/// Works out the import library a request asks for, and the file it goes in.
///
/// Everything that can be wrong is found here, before the output is touched, so a failed run leaves
/// whatever was at the output path alone.
fn build(program: &str, request: &Request) -> Result<(String, Vec<u8>), String> {
    let target = match &request.machine {
        Some(machine) => target_from_machine(machine)?,
        None => target_from_name(program).ok_or_else(|| {
            "no -m, and the program name does not say which machine either, so there is nothing to \
             say which machine the library is for"
                .to_owned()
        })?,
    };
    let input = request.input.as_deref().ok_or("no -d, so there is no .def file to read")?;
    let output =
        request.output.as_deref().ok_or("no -l, so there is nowhere to write the library")?;

    let text = std::fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?;
    let mut module = match rucc_stub::def::read(&text) {
        Ok(module) => module,
        // A file with no `LIBRARY` is fine when the command line names the DLL instead. The
        // statement goes on the end rather than the front so that a line number in a message is
        // still the line in the file, and the name is replaced below whatever it is.
        Err(rucc_stub::def::Error::NoLibrary) if request.dll.is_some() => {
            rucc_stub::def::read(&format!("{text}\nLIBRARY x.dll\n"))
                .map_err(|e| format!("{input}: {e}"))?
        }
        Err(e) => return Err(format!("{input}: {e}")),
    };
    if let Some(dll) = &request.dll {
        if !dll.contains('.') {
            return Err(format!(
                "-D {dll}: name the DLL as the file is called on disk, with its .dll. A LIBRARY \
                 line gets one added, and -D is written as it is by both dlltools"
            ));
        }
        module.library.clone_from(dll);
    }
    let decoration = if request.kill_at { Decoration::Cut } else { Decoration::Kept };
    let bytes = rucc_stub::coff::write_as(&module, target, decoration)
        .map_err(|e| format!("{input}: {e}"))?;
    Ok((output.to_owned(), bytes))
}

/// Runs dlltool mode and returns the process exit code.
///
/// `program` is `argv[0]`, which may say the target, and `args` excludes it and excludes the
/// `--dlltool` that asked for this mode.
pub fn run(program: &str, args: &[String]) -> i32 {
    let mut stderr = std::io::stderr().lock();
    let request = match parse(args) {
        Ok(request) => request,
        Err(why) => {
            let _ = writeln!(stderr, "rucc dlltool: error: {why}");
            return 1;
        }
    };
    if request.help {
        print!("{USAGE}");
        return 0;
    }
    let (output, bytes) = match build(program, &request) {
        Ok(built) => built,
        Err(why) => {
            let _ = writeln!(stderr, "rucc dlltool: error: {why}");
            return 1;
        }
    };
    // Written beside the output and renamed over it, so that a run that stops partway leaves no half
    // written archive where a later `ar` or `make` would find it. mingw-w64's build appends objects
    // to some of these libraries after dlltool has written them, and it does that to whatever file
    // is there.
    let partial = format!("{output}.part{}", std::process::id());
    let written =
        std::fs::write(&partial, &bytes).and_then(|()| std::fs::rename(&partial, &output));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        let _ = writeln!(stderr, "rucc dlltool: error: {output}: {e}");
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use rucc_tuple::Arch;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    /// A directory of its own for one test, so that tests running at once do not share files.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rucc-dlltool-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_command_mingw_w64_writes_is_read_whole() {
        let line = args(&[
            "--as-flags=--64",
            "-m",
            "i386:x86-64",
            "-k",
            "--as=x86_64-w64-mingw32-as",
            "--output-lib",
            "lib64/libkernel32.a",
            "--input-def",
            "lib64/kernel32.def",
        ]);
        let request = parse(&line).unwrap();
        assert_eq!(request.machine.as_deref(), Some("i386:x86-64"));
        assert_eq!(request.input.as_deref(), Some("lib64/kernel32.def"));
        assert_eq!(request.output.as_deref(), Some("lib64/libkernel32.a"));
        assert!(request.kill_at);
        assert_eq!(request.dll, None);
    }

    #[test]
    fn values_are_read_joined_separate_and_after_an_equals_sign() {
        let one = parse(&args(&["-mi386", "-dX.def", "-llibX.a", "-DX.dll"])).unwrap();
        let two =
            parse(&args(&["-m", "i386", "-d", "X.def", "-l", "libX.a", "-D", "X.dll"])).unwrap();
        let three = parse(&args(&[
            "--machine=i386",
            "--input-def=X.def",
            "--output-lib=libX.a",
            "--dllname=X.dll",
        ]))
        .unwrap();
        assert_eq!(one, two);
        assert_eq!(two, three);
        // The last of two is the one that counts, as with getopt_long.
        let later = parse(&args(&["-m", "i386", "-m", "arm64"])).unwrap();
        assert_eq!(later.machine.as_deref(), Some("arm64"));
    }

    #[test]
    fn an_option_it_does_not_have_is_refused_by_name() {
        // mingw-w64's configure asks this one and has to be told no.
        let why = parse(&args(&["--temp-prefix", "x", "-d", "t.def", "-l", "t.a"])).unwrap_err();
        assert!(why.contains("--temp-prefix"), "{why}");
        assert!(parse(&args(&["--output-delaylib", "x.a"])).is_err());
        assert!(parse(&args(&["x.o"])).is_err());
        assert!(parse(&args(&["-k=yes"])).is_err());
        let why = parse(&args(&["-d"])).unwrap_err();
        assert!(why.contains("requires an argument"), "{why}");
    }

    #[test]
    fn a_program_name_ending_in_dlltool_is_one() {
        assert!(is_dlltool("dlltool"));
        assert!(is_dlltool("/opt/bin/x86_64-w64-mingw32-dlltool"));
        assert!(is_dlltool("C:\\tools\\i686-w64-mingw32-dlltool.exe"));
        assert!(!is_dlltool("rucc"));
        assert!(!is_dlltool("x86_64-windows-gnu-rucc"));
        assert!(!is_dlltool("dlltool-wrapper"));
    }

    #[test]
    fn the_machine_names_are_the_ones_mingw_w64_writes() {
        let arch = |m: &str| target_from_machine(m).unwrap().arch();
        assert_eq!(arch("i386"), Arch::X86);
        assert_eq!(arch("i386:x86-64"), Arch::X86_64);
        assert_eq!(arch("arm64"), Arch::Aarch64);
        assert_eq!(arch("arm"), Arch::Arm);
        assert!(target_from_machine("arm64ec").is_err());
        assert!(target_from_machine("x86_64").is_err());
    }

    #[test]
    fn a_prefixed_name_is_the_target_when_no_machine_is_given() {
        let target = |p: &str| target_from_name(p).map(|t| t.arch());
        assert_eq!(target("x86_64-w64-mingw32-dlltool"), Some(Arch::X86_64));
        assert_eq!(target("/usr/bin/i686-w64-mingw32-dlltool"), Some(Arch::X86));
        assert_eq!(target("aarch64-windows-gnu-rucc"), Some(Arch::Aarch64));
        assert_eq!(target("dlltool"), None);
        // A prefix that is a target but not a Windows one says nothing about an import library.
        assert_eq!(target("x86_64-linux-gnu-dlltool"), None);
    }

    #[test]
    fn a_library_is_the_one_rucc_stub_writes_for_the_file() {
        let dir = scratch("write");
        let def = dir.join("k.def");
        let lib = dir.join("libk.a");
        let text = "LIBRARY \"KERNEL32.dll\"\nEXPORTS\nGetProcAddress@8\nDnsGlobals DATA\n";
        std::fs::write(&def, text).unwrap();
        let line = args(&[
            "-m",
            "i386",
            "-k",
            "--as-flags=--32",
            "-d",
            &def.display().to_string(),
            "-l",
            &lib.display().to_string(),
        ]);
        assert_eq!(run("rucc", &line), 0);
        let module = rucc_stub::def::read(text).unwrap();
        let want = rucc_stub::coff::write(&module, "i686-windows-gnu".parse().unwrap()).unwrap();
        assert_eq!(std::fs::read(&lib).unwrap(), want);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Without `-k` the DLL is asked for the name with its `@N`, which is what a DLL gcc linked
    /// without `--kill-at` exports, and the symbol is still the decorated one.
    #[test]
    fn an_i386_library_without_k_keeps_the_stack_size_in_the_imported_name() {
        let dir = scratch("kept");
        let def = dir.join("m.def");
        let text = "LIBRARY m.dll\nEXPORTS\nf@8\n";
        std::fs::write(&def, text).unwrap();
        let request = Request {
            machine: Some("i386".to_owned()),
            input: Some(def.display().to_string()),
            output: Some("unused.a".to_owned()),
            ..Request::default()
        };
        let (_, bytes) = build("rucc", &request).unwrap();
        let module = rucc_stub::def::read(text).unwrap();
        let target = "i686-windows-gnu".parse().unwrap();
        assert_eq!(bytes, rucc_stub::coff::write_as(&module, target, Decoration::Kept).unwrap());
        assert_ne!(bytes, rucc_stub::coff::write(&module, target).unwrap(), "-k would cut it");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dll_name_on_the_command_line_replaces_the_one_in_the_file() {
        let dir = scratch("dllname");
        let def = dir.join("f.def");
        std::fs::write(&def, "EXPORTS\nf\n").unwrap();
        let request = Request {
            machine: Some("i386:x86-64".to_owned()),
            input: Some(def.display().to_string()),
            output: Some("unused.a".to_owned()),
            dll: Some("other.dll".to_owned()),
            ..Request::default()
        };
        let (_, bytes) = build("rucc", &request).unwrap();
        let module = rucc_stub::def::read("LIBRARY other.dll\nEXPORTS\nf\n").unwrap();
        let want = rucc_stub::coff::write(&module, "x86_64-windows-gnu".parse().unwrap()).unwrap();
        assert_eq!(bytes, want);
        // A name with no dot would be written with a `.dll` the command line did not say.
        let bare = Request { dll: Some("other".to_owned()), ..request };
        assert!(build("rucc", &bare).unwrap_err().contains("-D other"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn what_is_missing_is_said_before_anything_is_written() {
        let request = |line: &[&str]| parse(&args(line)).unwrap();
        let why = build("rucc", &request(&["-d", "x.def", "-l", "x.a"])).unwrap_err();
        assert!(why.contains("no -m"), "{why}");
        let why = build("x86_64-w64-mingw32-dlltool", &request(&["-l", "x.a"])).unwrap_err();
        assert!(why.contains("no -d"), "{why}");
        let why = build("rucc", &request(&["-m", "arm64", "-d", "x.def"])).unwrap_err();
        assert!(why.contains("no -l"), "{why}");
        let why = build("rucc", &request(&["-m", "arm64", "-d", "/no/such.def", "-l", "x.a"]))
            .unwrap_err();
        assert!(why.starts_with("/no/such.def"), "{why}");
    }

    #[test]
    fn a_bad_file_is_reported_with_its_name_and_leaves_the_output_alone() {
        let dir = scratch("bad");
        let def = dir.join("bad.def");
        let lib = dir.join("libbad.a");
        std::fs::write(&def, "LIBRARY bad.dll\nEXPORTS\nf @0\n").unwrap();
        std::fs::write(&lib, b"before").unwrap();
        let line = args(&[
            "-m",
            "i386:x86-64",
            "-d",
            &def.display().to_string(),
            "-l",
            &lib.display().to_string(),
        ]);
        assert_eq!(run("rucc", &line), 1);
        assert_eq!(std::fs::read(&lib).unwrap(), b"before");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
