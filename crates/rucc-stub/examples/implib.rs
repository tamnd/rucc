//! Writes an import library per case per Windows target, for `llvm-dlltool` to be held against.
//!
//! `cargo xtask implib` runs this, builds the same libraries with `llvm-dlltool`, and compares the
//! two byte for byte. The split is the one `cargo xtask stubs` and `cargo xtask disasm` use: xtask
//! depends on no crate in the workspace, so the data it checks arrives as output rather than as a
//! call.
//!
//! Takes one argument, a directory to write into. Each case is written out as the `.def` file it was
//! read from, so that both writers are given the same bytes and the file the comparison disagreed
//! about is sitting next to the answer. One line is printed per library, bars between the fields:
//!
//! ```text
//! sample|x86_64-windows-gnu|<def path>|<our path>|-m i386:x86-64
//! sample|arm64ec-windows-msvc|refused: an ARM64EC import library needs every function name ...
//! ```
//!
//! The last field is the arguments `llvm-dlltool` needs to write the same library, which is part of
//! the claim rather than a detail of the checker: `-k` is there for i386 because mingw-w64 builds its
//! own import libraries that way and that is the behaviour this writer implements.
//!
//! The cases are small and each one is here for a reason the comment above it gives. They are not a
//! substitute for the comparison against all 2124 `.def` files in mingw-w64, which found three of the
//! rules this module follows and wants a checkout of mingw-w64 to run. They are what can be checked on
//! any machine with llvm installed.

use std::path::PathBuf;

use rucc_stub::def;
use rucc_tuple::{Arch, ObjectFormat, TARGETS, TargetTuple};

/// Every case, as the name to write it under and the `.def` text to read.
const CASES: [(&str, &str); 8] = [
    // One export of each kind that changes a record: a plain name, a decorated one, an ordinal with
    // no name, a variable, and a rename nothing else in the library imports.
    (
        "sample",
        "LIBRARY sample.dll\n\
         EXPORTS\n\
         ordinary\n\
         decorated@8\n\
         byordinal @42 NONAME\n\
         globalvar DATA\n\
         renamed == realname\n",
    ),
    // The CRT's shape, where the DLL exports two spellings of the same thing and the library already
    // imports the one the second asks for. Both become aliases, and the data one gets only the
    // `__imp_` half of the pair.
    (
        "crt",
        "LIBRARY api-ms-win-crt-conio-l1-1-0\n\
         EXPORTS\n\
         _getch\n\
         getch == _getch\n\
         _commode DATA\n\
         commode == _commode DATA\n",
    ),
    // Two real i386 files whose `== name` a name type can say, so neither record is a rename. The
    // first is the symbol spelled out and the second is the undecorated export.
    (
        "spelled",
        "LIBRARY X3DAudio1_2.dll\n\
         EXPORTS\n\
         X3DAudioCalculate@20 == _X3DAudioCalculate@20\n\
         UpdateDriverForPlugAndPlayDevicesA@20==UpdateDriverForPlugAndPlayDevicesA\n",
    ),
    // Names that carry their own decoration. 9648 exports in the 32-bit definitions start with `?`,
    // and the `@` in the middle of one of those is not a stack size.
    (
        "mangled",
        "LIBRARY p.dll\n\
         EXPORTS\n\
         ?mangled@@YAXXZ\n\
         ?Data@Thing@@2HA DATA\n\
         @fastcall@8\n\
         plain@8\n\
         plain\n",
    ),
    // A library name longer than the 15 bytes a member header has for it, and with no dot in it, so
    // the archive needs its long names member and the DLL gets a `.dll` suffix.
    (
        "apiset",
        "LIBRARY api-ms-win-core-apiquery-l2-1-0\n\
         EXPORTS\n\
         ApiSetQueryApiSetPresence\n",
    ),
    // A library name with dots in it, which gets no suffix, and whose long names member comes to an
    // odd length and is padded with a newline. The descriptor symbol is cut at the last dot, so it is
    // `__IMPORT_DESCRIPTOR_windows.ai`, which looks wrong and is what both tools write.
    (
        "dotted",
        "LIBRARY windows.ai.machinelearning.dll\n\
         EXPORTS\n\
         MLCreateOperatorRegistry\n",
    ),
    // The forms a record carries in its low two bits, and an ordinal that is a hint rather than the
    // only way in, which is the difference between `@42` and `@42 NONAME`.
    (
        "forms",
        "LIBRARY f.dll\n\
         EXPORTS\n\
         hinted @7\n\
         constant CONSTANT\n\
         both DATA == other\n",
    ),
    // `PRIVATE` keeps a name out of the library altogether, so a program that refers to it gets an
    // undefined symbol, which is the whole point of writing it.
    (
        "private",
        "LIBRARY v.dll\n\
         EXPORTS\n\
         wanted\n\
         DllRegisterServer PRIVATE\n",
    ),
];

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(into) = args.next().map(PathBuf::from) else {
        eprintln!("implib: wants one argument, the directory to write the libraries into");
        std::process::exit(2);
    };
    if let Err(why) = std::fs::create_dir_all(&into) {
        eprintln!("implib: cannot make {}: {why}", into.display());
        std::process::exit(2);
    }

    for (name, text) in CASES {
        let module = match def::read(text) {
            Ok(module) => module,
            Err(why) => {
                // Not a refusal to report per target: the file does not read, so no target can be
                // written and the case itself is wrong.
                eprintln!("implib: {name}.def does not read: {why}");
                std::process::exit(1);
            }
        };
        let source = into.join(format!("{name}.def"));
        if let Err(why) = std::fs::write(&source, text) {
            eprintln!("implib: cannot write {}: {why}", source.display());
            std::process::exit(2);
        }

        for entry in TARGETS {
            let Ok(target) = entry.tuple.parse::<TargetTuple>() else { continue };
            if target.object_format() != ObjectFormat::Coff {
                continue;
            }
            let bytes = match rucc_stub::coff::write(&module, target) {
                Ok(bytes) => bytes,
                Err(why) => {
                    println!("{name}|{}|refused: {why}", entry.tuple);
                    continue;
                }
            };
            let Some(machine) = machine(target.arch()) else {
                println!("{name}|{}|refused: no dlltool machine name for it", entry.tuple);
                continue;
            };
            let dir = into.join(entry.tuple);
            if let Err(why) = std::fs::create_dir_all(&dir) {
                println!("{name}|{}|refused: cannot make a directory for it: {why}", entry.tuple);
                continue;
            }
            let path = dir.join(format!("{name}.a"));
            if let Err(why) = std::fs::write(&path, &bytes) {
                println!("{name}|{}|refused: cannot write it: {why}", entry.tuple);
                continue;
            }
            println!("{name}|{}|{}|{}|{machine}", entry.tuple, source.display(), path.display());
        }
    }
}

/// What `llvm-dlltool` has to be told to write the same library.
///
/// `-k` on i386 is the kill-at behaviour, which is what mingw-w64 asks for when it builds its own
/// import libraries and what this writer implements: the symbol keeps `@8` and the name looked for in
/// the DLL does not. Without it the reference would be a different file and the comparison would be
/// against a library no mingw program links with.
///
/// The two msvc tuples get the same arguments as their gnu counterparts because they get the same
/// bytes. Nothing in a record is about the environment, only about the architecture.
fn machine(arch: Arch) -> Option<&'static str> {
    match arch {
        Arch::X86 => Some("-m i386 -k"),
        Arch::X86_64 => Some("-m i386:x86-64"),
        Arch::Aarch64 => Some("-m arm64"),
        Arch::Arm => Some("-m arm"),
        _ => None,
    }
}
