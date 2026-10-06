//! The `wasm-ld` command line, as far as this linker covers it.
//!
//! The driver writes one line for a wasm link, and it is the line for `wasm-ld`. When the link
//! runs in the process, this module reads that same line. So the start file, the libraries, the
//! order and the words a person passes with `-Wl,` come from one place, and the two linkers cannot
//! get different lines.
//!
//! The options this module takes are the ones the driver writes and the ones that a person
//! passes most often for a static module. An option that changes nothing here, such as
//! `--gc-sections` or `--start-group`, is taken and has no effect. Any other option is an error
//! that names it, because a link that drops an option it does not know can write a module that is
//! wrong and say nothing.

use std::path::{Path, PathBuf};

use crate::{Error, Input, Options, link};

/// A parsed line: what to link, from what, and into which file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub options: Options,
    /// The objects and the archives in line order, with each `-l` found in the `-L` directories.
    pub inputs: Vec<PathBuf>,
    pub output: PathBuf,
}

/// An input before the `-L` directories are known, because a `-L` applies to every `-l` on the
/// line, also to one before it.
enum Pending {
    File(PathBuf),
    Library(String),
}

/// The options that change nothing in a static module made by this linker. Each is taken and has
/// no effect.
const NO_EFFECT: &[&str] = &[
    "--gc-sections",
    "--stack-first",
    "--start-group",
    "--end-group",
    "-(",
    "-)",
    "--as-needed",
    "--no-as-needed",
    "-Bstatic",
    "-static",
    "--no-whole-archive",
    "--strip-debug",
    "-S",
    "--color-diagnostics",
    "--no-color-diagnostics",
    "--fatal-warnings",
    "--no-demangle",
    "--demangle",
    "-O0",
    "-O1",
    "-O2",
    "-O3",
];

/// The value of an option that is written `-o out`, `-oout`, `--output out` or `--output=out`.
/// `short` is the one-dash name and `long` the two-dash name, either of which can be empty.
fn value(
    args: &[String],
    at: &mut usize,
    short: &str,
    long: &str,
) -> Option<Result<String, Error>> {
    let arg = &args[*at];
    let next = |at: &mut usize| {
        *at += 1;
        args.get(*at).cloned().ok_or_else(|| Error::new(format!("{arg} needs a value after it")))
    };
    if (!short.is_empty() && arg == short) || (!long.is_empty() && arg == long) {
        return Some(next(at));
    }
    if !long.is_empty() {
        if let Some(rest) = arg.strip_prefix(long).and_then(|rest| rest.strip_prefix('=')) {
            return Some(Ok(rest.to_owned()));
        }
    }
    // A one-letter option can have its value joined to it, as `-L/usr/lib` and `-lm`.
    if short.len() == 2 {
        if let Some(rest) = arg.strip_prefix(short).filter(|rest| !rest.is_empty()) {
            return Some(Ok(rest.to_owned()));
        }
    }
    None
}

/// A size in bytes, in decimal or with `0x` in hexadecimal, as `wasm-ld` takes it.
fn size(option: &str, text: &str) -> Result<u32, Error> {
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| Error::new(format!("{option}: {text} is not a size in bytes")))
}

fn unsupported(arg: &str) -> Error {
    Error::new(format!("the rucc linker does not take {arg}; link with wasm-ld for it"))
}

impl Command {
    /// Reads a `wasm-ld` line, without the program name. `is_file` says whether a path is a
    /// file, which is how a `-l` is found in the `-L` directories.
    ///
    /// # Errors
    ///
    /// An option this linker does not take, an option with no value, a size that is not a
    /// number, a `-l` that is in no `-L` directory, and a line with no output.
    pub fn parse(args: &[String], is_file: impl Fn(&Path) -> bool) -> Result<Self, Error> {
        let mut options = Options::default();
        let mut pending = Vec::new();
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut output = None;
        let mut at = 0;
        while at < args.len() {
            let arg = args[at].as_str();
            if !arg.starts_with('-') || arg == "-" {
                pending.push(Pending::File(PathBuf::from(arg)));
            } else if NO_EFFECT.contains(&arg) || arg.starts_with("--sysroot=") {
            } else if matches!(arg, "--strip-all" | "-s") {
                options.strip = true;
            } else if arg == "--no-entry" {
                options.entry = None;
            } else if arg == "--allow-undefined" {
                options.allow_undefined = true;
            } else if let Some(out) = value(args, &mut at, "-o", "--output") {
                output = Some(PathBuf::from(out?));
            } else if let Some(emulation) = value(args, &mut at, "-m", "") {
                let emulation = emulation?;
                if emulation != "wasm32" {
                    return Err(Error::new(format!(
                        "-m {emulation}: the rucc linker writes only wasm32; link with wasm-ld"
                    )));
                }
            } else if let Some(dir) = value(args, &mut at, "-L", "--library-path") {
                dirs.push(PathBuf::from(dir?));
            } else if let Some(name) = value(args, &mut at, "-l", "--library") {
                pending.push(Pending::Library(name?));
            } else if let Some(entry) = value(args, &mut at, "-e", "--entry") {
                options.entry = Some(entry?);
            } else if let Some(name) = value(args, &mut at, "", "--export") {
                options.exports.push(name?);
            } else if let Some(name) = value(args, &mut at, "-u", "--undefined") {
                options.undefined.push(name?);
            } else if let Some(bytes) = value(args, &mut at, "", "--initial-memory") {
                options.initial_memory = Some(size("--initial-memory", &bytes?)?);
            } else if let Some(bytes) = value(args, &mut at, "", "--max-memory") {
                options.max_memory = Some(size("--max-memory", &bytes?)?);
            } else if let Some(word) = value(args, &mut at, "-z", "") {
                let word = word?;
                match word.strip_prefix("stack-size=") {
                    Some(bytes) => options.stack_size = size("-z stack-size", bytes)?,
                    None => return Err(unsupported(&format!("-z {word}"))),
                }
            } else {
                return Err(unsupported(arg));
            }
            at += 1;
        }
        let output = output.ok_or_else(|| Error::new("there is no -o on the line".to_owned()))?;
        options.name = output.file_name().map(|name| name.to_string_lossy().into_owned());
        let mut inputs = Vec::with_capacity(pending.len());
        for input in pending {
            match input {
                Pending::File(path) => inputs.push(path),
                Pending::Library(name) => {
                    // `-l:name` is the file name as it is, and `-lname` is `libname.a`. A shared
                    // library is not looked for, because this linker makes static modules.
                    let file = match name.strip_prefix(':') {
                        Some(file) => file.to_owned(),
                        None => format!("lib{name}.a"),
                    };
                    let found = dirs.iter().map(|dir| dir.join(&file)).find(|path| is_file(path));
                    inputs.push(
                        found.ok_or_else(|| {
                            Error::new(format!("unable to find library -l{name}"))
                        })?,
                    );
                }
            }
        }
        Ok(Command { options, inputs, output })
    }
}

/// Links what a `wasm-ld` line asks for and writes the module.
///
/// # Errors
///
/// What [`Command::parse`] and [`link`] refuse, and an input that cannot be read or an output
/// that cannot be written.
pub fn run(args: &[String]) -> Result<(), Error> {
    let command = Command::parse(args, Path::is_file)?;
    let mut bytes = Vec::with_capacity(command.inputs.len());
    for path in &command.inputs {
        let read = std::fs::read(path)
            .map_err(|e| Error::new(format!("cannot open {}: {e}", path.display())))?;
        bytes.push(read);
    }
    let names: Vec<String> = command.inputs.iter().map(|p| p.display().to_string()).collect();
    let inputs: Vec<Input<'_>> =
        names.iter().zip(&bytes).map(|(name, bytes)| Input { name, bytes }).collect();
    let module = link(&command.options, &inputs)?;
    let out = &command.output;
    let failed = |e: std::io::Error| Error::new(format!("cannot write {}: {e}", out.display()));
    std::fs::write(out, module).map_err(failed)?;
    // A command module is a program, so it is marked as one, as `wasm-ld` marks it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755)).map_err(failed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(words: &str) -> Vec<String> {
        words.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_line_the_driver_writes_for_wasip1_parses() {
        let args = line(
            "-o a.wasm -m wasm32 /s/lib/crt1-command.o -L/s/lib a.o -lm /s/lib/libsetjmp.a \
             /s/lib/libc.a /r/librucc_builtins.a",
        );
        let command = Command::parse(&args, |path| path == Path::new("/s/lib/libm.a")).unwrap();
        assert_eq!(command.output, Path::new("a.wasm"));
        assert_eq!(command.options.name.as_deref(), Some("a.wasm"));
        let inputs: Vec<_> = command.inputs.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            inputs,
            [
                "/s/lib/crt1-command.o",
                "a.o",
                "/s/lib/libm.a",
                "/s/lib/libsetjmp.a",
                "/s/lib/libc.a",
                "/r/librucc_builtins.a"
            ]
        );
        assert_eq!(
            command.options,
            Options { name: Some("a.wasm".to_owned()), ..Options::default() }
        );
    }

    #[test]
    fn the_options_for_a_reactor_and_its_memory_are_read() {
        let args = line(
            "-o a.wasm --entry _initialize --export=f --export g -u h -z stack-size=0x2000 \
             --initial-memory=131072 --max-memory=262144 --strip-all --gc-sections a.o",
        );
        let options = Command::parse(&args, |_| false).unwrap().options;
        assert_eq!(options.entry.as_deref(), Some("_initialize"));
        assert_eq!(options.exports, ["f", "g"]);
        assert_eq!(options.undefined, ["h"]);
        assert_eq!(options.stack_size, 0x2000);
        assert_eq!((options.initial_memory, options.max_memory), (Some(131_072), Some(262_144)));
        assert!(options.strip);
        let options = Command::parse(&line("-o a.wasm --no-entry a.o"), |_| false).unwrap().options;
        assert_eq!(options.entry, None);
    }

    #[test]
    fn an_option_this_linker_does_not_take_is_an_error_that_names_it() {
        let error = Command::parse(&line("-o a.wasm --shared-memory a.o"), |_| false).unwrap_err();
        assert_eq!(
            error.message(),
            "the rucc linker does not take --shared-memory; link with wasm-ld for it"
        );
        let error = Command::parse(&line("-o a.wasm -lfoo a.o"), |_| false).unwrap_err();
        assert_eq!(error.message(), "unable to find library -lfoo");
        let error = Command::parse(&line("-o a.wasm -m wasm64 a.o"), |_| false).unwrap_err();
        assert!(error.message().starts_with("-m wasm64"), "{error}");
        let error = Command::parse(&line("a.o -o"), |_| false).unwrap_err();
        assert_eq!(error.message(), "-o needs a value after it");
    }
}
