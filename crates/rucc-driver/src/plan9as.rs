//! The Plan 9 assembler driver: rucc with the command line of `6a` and `7a`.
//!
//! Design: 07.5 of the rucc-plan9 plan.
//!
//! The 9front mkfiles call the assembler as `$AS`, such as `6a x.s`. rucc-plan9 puts rucc in
//! that slot, and the command line comes through unchanged. This module reads that command line
//! the way `main` in `6a/lex.c` reads it, and assembles each file with
//! [`rucc_asm::plan9::syntax`], which writes the same object as the C program.
//!
//! The driver is used when the program name is `6a` or `7a`, or when the first argument is
//! `--driver=plan9-as`. These options can come before the options of the assembler:
//!
//! - `--target=` gives the machine, `x86_64-plan9` or `aarch64-plan9`. The default is the
//!   machine of the program name, or else amd64.
//! - `--sysroot=` gives the root of the Plan 9 name space. A file name of the assembler is a
//!   name in that name space. With `--sysroot=9p:tcp!host!port`, the files come from a 9P
//!   server. Without it, the names are names on the disk.
//! - `--cwd=` gives the current directory in the Plan 9 name space. The assembler opens a
//!   relative name from it, and the object has it in its history. The default is the current
//!   directory of rucc.
//!
//! The rules that this module copies from the C program:
//!
//! - The options stop at the first argument that does not start with `-`. The letters `o`, `D`
//!   and `I` take a value: the rest of the argument, or else the next argument. Every other
//!   letter is a debug flag, which has no effect on the object.
//! - The object of `dir/x.s` is `x.6` or `x.7` in the current directory. `-o` names the object.
//! - With more than one file, the assembler writes the name of each file and a colon before it
//!   assembles the file.
//!
//! The C program writes its messages on standard output. This driver writes them on standard
//! error, as the other drivers of rucc do.

use std::io::{self, Write};
use std::net::TcpStream;
use std::sync::Mutex;

use rucc_9p::Client;
use rucc_asm::plan9::syntax::{assemble6, assemble7, namelenerror};

/// `Enonexist` of the Plan 9 kernel.
const ENONEXIST: &str = "file does not exist";

/// The option that selects this driver as the first argument.
pub const DRIVER_OPTION: &str = "--driver=plan9-as";

/// A machine of the assembler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Machine {
    Amd64,
    Arm64,
}

impl Machine {
    fn letter(self) -> char {
        match self {
            Machine::Amd64 => '6',
            Machine::Arm64 => '7',
        }
    }

    fn from_target(target: &str) -> Option<Machine> {
        match target.split('-').next()? {
            "x86_64" | "amd64" => Some(Machine::Amd64),
            "aarch64" | "arm64" => Some(Machine::Arm64),
            _ => None,
        }
    }
}

/// The program name without its directory and without a trailing `.exe`.
fn base_name(program: &str) -> &str {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name.strip_suffix(".exe").unwrap_or(name)
}

/// The machine of the program name `6a` or `7a`.
fn from_program(program: &str) -> Option<Machine> {
    match base_name(program) {
        "6a" => Some(Machine::Amd64),
        "7a" => Some(Machine::Arm64),
        _ => None,
    }
}

/// Whether `program` is `6a` or `7a`.
#[must_use]
pub fn is_plan9_as(program: &str) -> bool {
    from_program(program).is_some()
}

/// A command line of the assembler.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Command {
    /// The machine letter, `6` or `7`.
    pub letter: char,
    /// The value of `--sysroot=`.
    pub sysroot: Option<String>,
    /// The value of `--cwd=`.
    pub cwd: Option<String>,
    /// The value of `-o`.
    pub output: Option<String>,
    /// The values of `-D`, in their order.
    pub defines: Vec<String>,
    /// The values of `-I`, in their order.
    pub includes: Vec<String>,
    /// The files.
    pub files: Vec<String>,
}

/// Read the command line `args` of the assembler started as `program`.
///
/// # Errors
///
/// The message when the command line is not correct.
pub fn parse(program: &str, args: &[String]) -> Result<Command, String> {
    let mut machine = from_program(program);
    let mut cmd = Command::default();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if let Some(t) = a.strip_prefix("--target=") {
            machine = Some(
                Machine::from_target(t)
                    .ok_or_else(|| format!("the assembler has no machine for --target={t}"))?,
            );
        } else if let Some(s) = a.strip_prefix("--sysroot=") {
            cmd.sysroot = Some(s.to_owned());
        } else if let Some(d) = a.strip_prefix("--cwd=") {
            cmd.cwd = Some(d.to_owned());
        } else if a == DRIVER_OPTION {
        } else {
            break;
        }
        i += 1;
    }
    // ARGBEGIN: the options stop at an argument that does not start with `-`, at `-` alone, and
    // after `--`.
    while let Some(a) = args.get(i) {
        if a == "--" {
            i += 1;
            break;
        }
        let Some(letters) = a.strip_prefix('-').filter(|l| !l.is_empty()) else {
            break;
        };
        i += 1;
        for (k, c) in letters.char_indices() {
            if !matches!(c, 'o' | 'D' | 'I') {
                continue;
            }
            let rest = &letters[k + 1..];
            let value = if rest.is_empty() {
                let v = args.get(i).cloned();
                if v.is_some() {
                    i += 1;
                }
                v
            } else {
                Some(rest.to_owned())
            };
            // ARGF gives nil with no value. `-D` and `-I` then do nothing, and `-o` gives no
            // name, which is the default.
            match (c, value) {
                ('o', v) => cmd.output = v,
                ('D', Some(v)) => cmd.defines.push(v),
                ('I', Some(v)) => cmd.includes.push(v),
                _ => {}
            }
            break;
        }
    }
    cmd.files = args[i..].to_vec();
    cmd.letter = machine.unwrap_or(Machine::Amd64).letter();
    Ok(cmd)
}

/// The object name of `file` when there is no `-o`: the last element of the path with a
/// trailing `.s` taken off, then a dot and the machine letter.
#[must_use]
pub fn object_name(file: &str, letter: char) -> String {
    let base = file.rsplit('/').next().unwrap_or(file);
    let base = base.strip_suffix(".s").unwrap_or(base);
    format!("{base}.{letter}")
}

/// The name `name` of the assembler as an absolute name in the name space, from the directory
/// `cwd`. Empty and `.` elements are taken out, and `..` stays for the server to walk.
#[must_use]
pub fn absolute(cwd: &str, name: &str) -> String {
    let joined = if name.starts_with('/') { name.to_owned() } else { format!("{cwd}/{name}") };
    let parts: Vec<&str> = joined.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    format!("/{}", parts.join("/"))
}

/// Where the files of the assembler come from.
enum Files {
    /// The disk, under a root that can be empty.
    Disk(String),
    /// A 9P server.
    Server(Mutex<Client<TcpStream>>),
}

impl Files {
    fn new(sysroot: Option<&str>) -> Result<Files, String> {
        match sysroot {
            Some(s) if s.starts_with("9p:") => {
                let addr = &s[3..];
                let fail = |e: io::Error| {
                    format!("cannot read the sysroot from the 9P server {addr}: {e}")
                };
                let stream = rucc_9p::dial(addr).map_err(fail)?;
                let user = std::env::var("USER")
                    .ok()
                    .filter(|u| !u.is_empty())
                    .unwrap_or_else(|| "none".to_owned());
                let client = Client::connect(stream, &user, "").map_err(fail)?;
                Ok(Files::Server(Mutex::new(client)))
            }
            Some(s) => Ok(Files::Disk(s.trim_end_matches('/').to_owned())),
            None => Ok(Files::Disk(String::new())),
        }
    }

    fn read(&self, cwd: &str, name: &str, rooted: bool) -> Result<Vec<u8>, String> {
        let text = match self {
            Files::Disk(_) if !rooted => std::fs::read(name).ok(),
            Files::Disk(root) => std::fs::read(format!("{root}{}", absolute(cwd, name))).ok(),
            Files::Server(client) => {
                let path = absolute(cwd, name);
                let mut c = client.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                c.read_file(path.trim_start_matches('/')).ok().map(|b| b.to_vec())
            }
        };
        text.ok_or_else(|| namelenerror(name, self.missing(cwd, name, rooted), ENONEXIST))
    }

    /// The end of the first element of `name` that does not exist, as the kernel finds it when
    /// it walks the name. It is the length of the name when each element exists.
    fn missing(&self, cwd: &str, name: &str, rooted: bool) -> usize {
        let ends = name
            .char_indices()
            .filter(|&(i, c)| c == '/' && i > 0 && !name[..i].ends_with('/'))
            .map(|(i, _)| i);
        for end in ends {
            let prefix = &name[..end];
            let found = match self {
                Files::Disk(_) if !rooted => std::path::Path::new(prefix).exists(),
                Files::Disk(root) => {
                    std::path::Path::new(&format!("{root}{}", absolute(cwd, prefix))).exists()
                }
                Files::Server(client) => {
                    let path = absolute(cwd, prefix);
                    let mut c = client.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    c.stat(path.trim_start_matches('/')).is_ok()
                }
            };
            if !found {
                return end;
            }
        }
        name.len()
    }
}

/// Run the assembler started as `program` with the arguments `args`, and give the exit status.
#[must_use]
pub fn run(program: &str, args: &[String]) -> i32 {
    let mut stderr = io::stderr();
    let cmd = match parse(program, args) {
        Ok(c) => c,
        Err(e) => {
            let _ =
                writeln!(stderr, "{}a: {e}", from_program(program).map_or('6', Machine::letter));
            return 1;
        }
    };
    if cmd.files.is_empty() {
        let _ = writeln!(stderr, "usage: {}a [-options] file.s", cmd.letter);
        return 1;
    }
    let files = match Files::new(cmd.sysroot.as_deref()) {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(stderr, "{}a: {e}", cmd.letter);
            return 1;
        }
    };
    let rooted = cmd.sysroot.is_some();
    let cwd = cmd.cwd.clone().unwrap_or_else(|| {
        std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
    });
    let read = |name: &str| files.read(&cwd, name, rooted);
    let assemble = if cmd.letter == '7' { assemble7 } else { assemble6 };
    let mut failed = false;
    for file in &cmd.files {
        if cmd.files.len() > 1 {
            let _ = writeln!(stderr, "{file}:");
        }
        let out = cmd.output.clone().unwrap_or_else(|| object_name(file, cmd.letter));
        match assemble(file, &cwd, &cmd.defines, &cmd.includes, &read) {
            Ok(object) => {
                if let Err(e) = std::fs::write(&out, object) {
                    let _ = writeln!(stderr, "{}a: cannot create {out}: {e}", cmd.letter);
                    failed = true;
                }
            }
            Err(messages) => {
                let _ = stderr.write_all(messages.as_bytes());
                failed = true;
            }
        }
    }
    i32::from(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_options_are_read_as_argbegin_reads_them() {
        let c =
            parse("/bin/7a", &args(&["-x", "-Dname=1", "-I", "inc", "-oout.7", "a.s"])).unwrap();
        assert_eq!(c.letter, '7');
        assert_eq!(c.defines, ["name=1"]);
        assert_eq!(c.includes, ["inc"]);
        assert_eq!(c.output.as_deref(), Some("out.7"));
        assert_eq!(c.files, ["a.s"]);
    }

    #[test]
    fn the_driver_options_come_first() {
        let a =
            args(&[DRIVER_OPTION, "--target=aarch64-plan9", "--cwd=/sys/src", "-I", "x", "l.s"]);
        let c = parse("rucc", &a).unwrap();
        assert_eq!(c.letter, '7');
        assert_eq!(c.cwd.as_deref(), Some("/sys/src"));
        assert_eq!(c.files, ["l.s"]);
    }

    #[test]
    fn a_file_after_the_options_ends_them() {
        let c = parse("6a", &args(&["a.s", "-o", "x"])).unwrap();
        assert_eq!(c.files, ["a.s", "-o", "x"]);
        assert_eq!(c.output, None);
    }

    #[test]
    fn the_object_name_drops_only_a_trailing_s() {
        assert_eq!(object_name("../port/l.s", '6'), "l.6");
        assert_eq!(object_name("x.S", '7'), "x.S.7");
        assert_eq!(object_name("x", '7'), "x.7");
    }

    #[test]
    fn the_files_come_from_the_9p_server() {
        use std::collections::HashMap;
        use std::net::TcpListener;
        let files = HashMap::from([
            ("/sys/src/a/x.s", "#include \"../h/m.h\"\nTEXT f(SB), $0\n\tMOVQ $N, AX\n\tRET\n"),
            ("/sys/src/h/m.h", "#define N 7\n"),
        ]);
        let mut tree = rucc_9p::serve::Tree::new();
        for (name, text) in &files {
            tree.insert(name.trim_start_matches('/'), text.as_bytes());
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = rucc_9p::serve::serve(stream, &Mutex::new(tree));
        });
        let out = std::env::temp_dir().join(format!("rucc-plan9as-{}.6", std::process::id()));
        let a = args(&[
            &format!("--sysroot=9p:tcp!127.0.0.1!{port}"),
            "--cwd=/sys/src/a",
            "-o",
            out.to_str().unwrap(),
            "x.s",
        ]);
        assert_eq!(run("6a", &a), 0);
        server.join().unwrap();
        let read = |name: &str| {
            let key = absolute("/sys/src/a", name).replace("/a/../", "/");
            files.get(key.as_str()).map(|t| t.as_bytes().to_vec()).ok_or_else(String::new)
        };
        let want = assemble6("x.s", "/sys/src/a", &[], &[], &read).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), want);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn a_relative_name_is_under_the_directory() {
        assert_eq!(absolute("/sys/src/9/pc64", "../port/x.h"), "/sys/src/9/pc64/../port/x.h");
        assert_eq!(absolute("/sys/src", "/amd64/include/u.h"), "/amd64/include/u.h");
        assert_eq!(absolute("/", "./a.s"), "/a.s");
    }
}
