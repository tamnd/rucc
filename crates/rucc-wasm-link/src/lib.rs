//! A static linker for wasm, inside the compiler.
//!
//! Design: section 9.8 of the WebAssembly notes (decision D8). Layer rank 0, see
//! `spec/18-package-layout.md`.
//!
//! # Why rucc has a linker for wasm
//!
//! rucc that runs as a wasm module cannot start a process, so it cannot run `wasm-ld`. Without a
//! linker in the process, `rucc.wasm hello.c -o hello.wasm` stops after the object. This crate is
//! that linker. On a native host the driver still uses `wasm-ld` when it finds one, and uses this
//! crate when `-fuse-ld=rucc` is given or when there is no `wasm-ld`.
//!
//! # What it covers
//!
//! Static core modules for wasm32-wasip1 and wasm32-none, as a command (`_start`) or a reactor
//! (`_initialize`). The inputs are relocatable objects from rucc or clang, and `ar` archives of
//! them. An input that needs more, such as a shared library or a defined memory, is refused with a
//! message that names `wasm-ld`. It is not a general replacement for `wasm-ld`.
//!
//! # Status
//!
//! [`command::run`] reads a `wasm-ld` line and links what it asks for, which is how the driver
//! uses the crate. [`link`] does the whole job on bytes: it reads the objects and the archives,
//! resolves the symbols, fetches the archive members that define what is undefined, drops what the
//! roots do not reach, lays out the memory and the table, applies the relocations and writes the
//! module. The readers are public too: [`object`] for one object and [`archive`] for the members
//! of an archive. The driver links with this crate when `-fuse-ld=rucc` is given, when rucc runs
//! as wasm, and when no `wasm-ld` is found.
//!
//! The module is the one `wasm-ld` writes for the same line, byte for byte, for the programs we
//! compared: two small programs, one of which calls `printf`, and the SQLite shell against the
//! wasi-sdk 34 sysroot, each with and without `-g`. The order of the objects, the merged strings,
//! the types, the imports and the names are all LLD's. The DWARF sections of the inputs go in the
//! module as LLD puts them there: the sections of each name one after the other, the strings of
//! `.debug_str` and `.debug_line_str` merged, and the addresses of the functions and the data
//! patched. A function or data that the module does not have gets the address `-1`, or `-2` in
//! `.debug_ranges` and `.debug_loc`.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-wasm-link/0.29.6")]

use core::fmt;

pub mod archive;
mod bytes;
pub mod command;
mod layout;
mod live;
pub mod object;
mod resolve;
#[cfg(test)]
mod testing;
mod write;

/// What the link makes. The defaults are those of `wasm-ld` for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The function the module exports to start it, `_start` for a command. `None` makes a
    /// module with no entry, which a reactor is.
    pub entry: Option<String>,
    /// The names to export, as `--export`. Each must be defined.
    pub exports: Vec<String>,
    /// The names to resolve even when no input refers to them, as `--undefined`.
    pub undefined: Vec<String>,
    /// The size of the stack in bytes, as `-z stack-size`.
    pub stack_size: u32,
    /// The initial memory in bytes, as `--initial-memory`. `None` makes it the pages the data
    /// needs.
    pub initial_memory: Option<u32>,
    /// The maximum memory in bytes, as `--max-memory`. `None` gives the memory no maximum.
    pub max_memory: Option<u32>,
    /// Leave out every custom section, which is the DWARF sections and the `name`, `producers` and
    /// `target_features` sections, as `--strip-all`.
    pub strip: bool,
    /// Leave out the DWARF sections, as `--strip-debug`.
    pub strip_debug: bool,
    /// Make an undefined function an import from `env` and not an error, as `--allow-undefined`.
    pub allow_undefined: bool,
    /// The name of the module in the `name` section. `wasm-ld` gives the file name of the output.
    pub name: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            entry: Some("_start".to_owned()),
            exports: Vec::new(),
            undefined: Vec::new(),
            stack_size: 65_536,
            initial_memory: None,
            max_memory: None,
            strip: false,
            strip_debug: false,
            allow_undefined: false,
            name: None,
        }
    }
}

/// One input to the link: an object or an archive, as its bytes, in command line order.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    /// The name the messages use, which is usually the path.
    pub name: &'a str,
    pub bytes: &'a [u8],
}

/// Links the inputs into one module.
///
/// # Errors
///
/// An input that is not a wasm object or an archive of them, an undefined symbol, a symbol
/// defined twice, a call through the wrong signature, a feature one input uses and another
/// disallows, and an input that needs what this linker does not cover, such as threads.
pub fn link(options: &Options, inputs: &[Input<'_>]) -> Result<Vec<u8>, Error> {
    let mut world = resolve::World::load(options, inputs)?;
    let live = live::Live::mark(&mut world);
    let layout = layout::Layout::new(&world, &live, options)?;
    write::write(&world, &live, &layout, options)
}

/// Why a link stopped. The message names the input when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    pub(crate) fn new(message: String) -> Self {
        Error { message }
    }

    /// The same error, with the name of the input in front.
    #[must_use]
    pub(crate) fn within(self, name: &str) -> Self {
        Error { message: format!("{name}: {}", self.message) }
    }

    /// The message, without a prefix.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{archive, callee, caller, calls, custom, debugged, importer, weak_caller};

    /// The payload of each section of a module, by id, in order.
    fn sections(module: &[u8]) -> Vec<(u8, &[u8])> {
        let mut out = Vec::new();
        let mut at = 8;
        while at < module.len() {
            let id = module[at];
            let (mut size, mut shift) = (0usize, 0);
            at += 1;
            loop {
                let byte = module[at];
                at += 1;
                size |= usize::from(byte & 0x7f) << shift;
                shift += 7;
                if byte < 0x80 {
                    break;
                }
            }
            out.push((id, &module[at..at + size]));
            at += size;
        }
        out
    }

    fn payload(module: &[u8], id: u8) -> Vec<u8> {
        sections(module).into_iter().find(|&(i, _)| i == id).map(|(_, p)| p.to_vec()).unwrap()
    }

    const I32_TO_I32: &[u8] = &[0x60, 1, 0x7f, 1, 0x7f];

    #[test]
    fn an_object_and_an_archive_link_into_a_module() {
        let a = caller("_start");
        let f = callee("f", I32_TO_I32);
        // A member that defines `_start` again must stay out, because nothing needs it.
        let other = callee("_start", &[0x60, 0, 0]);
        let lib = archive(&[("f.o", &f), ("other.o", &other)]);
        let inputs = [Input { name: "a.o", bytes: &a }, Input { name: "lib.a", bytes: &lib }];
        let module = link(&Options::default(), &inputs).unwrap();
        let ids: Vec<u8> = sections(&module).iter().map(|&(id, _)| id).collect();
        // Nothing refers to the stack pointer, so there are no globals and no global section.
        assert_eq!(ids, [1, 3, 4, 5, 7, 10, 11, 0, 0]);
        // `_start` is function 0 and `f` is function 1. The address of `d` is 65536, just above
        // the stack, as a padded LEB128, and the call goes to function 1.
        let start =
            [0, 0x41, 0x80, 0x80, 0x84, 0x80, 0, 0x10, 0x81, 0x80, 0x80, 0x80, 0, 0x1a, 0x0b];
        let mut code = vec![2, 15];
        code.extend(start);
        code.extend([3, 0, 0x00, 0x0b]);
        assert_eq!(payload(&module, 10), code);
        assert_eq!(
            payload(&module, 11),
            [1, 0, 0x41, 0x80, 0x80, 4, 0x0b, 4, b'a', b'b', b'c', b'd']
        );
        let mut exports = vec![2];
        bytes::name(&mut exports, "memory");
        exports.extend([2, 0]);
        bytes::name(&mut exports, "_start");
        exports.extend([0, 0]);
        assert_eq!(payload(&module, 7), exports);
    }

    #[test]
    fn strip_all_leaves_out_every_custom_section() {
        let a = caller("_start");
        let f = callee("f", I32_TO_I32);
        let inputs = [Input { name: "a.o", bytes: &a }, Input { name: "f.o", bytes: &f }];
        let customs = |module: &[u8]| sections(module).iter().filter(|&&(id, _)| id == 0).count();
        // The `name` section and the `target_features` section of `a.o`.
        assert_eq!(customs(&link(&Options::default(), &inputs).unwrap()), 2);
        let options = Options { strip: true, ..Options::default() };
        assert_eq!(customs(&link(&options, &inputs).unwrap()), 0);
    }

    #[test]
    fn the_dwarf_sections_are_kept_with_their_relocations_applied() {
        let a = debugged("_start");
        let b = debugged("g");
        let inputs = [Input { name: "a.o", bytes: &a }, Input { name: "b.o", bytes: &b }];
        let customs = |module: &[u8]| -> Vec<(String, Vec<u8>)> {
            sections(module)
                .into_iter()
                .filter(|&(id, _)| id == 0)
                .map(|(_, p)| {
                    let len = usize::from(p[0]);
                    (String::from_utf8(p[1..=len].to_vec()).unwrap(), p[len + 1..].to_vec())
                })
                .collect()
        };
        let module = customs(&link(&Options::default(), &inputs).unwrap());
        let titles: Vec<&str> = module.iter().map(|(title, _)| title.as_str()).collect();
        assert_eq!(titles, [".debug_str", ".debug_info", "name"]);
        // The strings of both objects, each once, in LLD's order, which sorts them by their bytes
        // from the end.
        assert_eq!(module[0].1, b"_start\0g\0shared\0");
        // `_start` starts at offset 2 of the code section, after the count and the size, and the
        // relocation adds 1. Nothing calls `g`, so it is not in the module, and its address is the
        // value that is not an address. Both offsets of `shared` are the offset of the one copy.
        let info = [3, 0, 0, 0, 9, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 9, 0, 0, 0];
        assert_eq!(module[1].1, info);
        let options = Options { strip_debug: true, ..Options::default() };
        let module = customs(&link(&options, &inputs).unwrap());
        assert!(module.iter().all(|(title, _)| !title.starts_with(".debug")));
    }

    #[test]
    fn the_same_inputs_make_the_same_module() {
        let object = importer();
        let inputs = [Input { name: "a.o", bytes: &object }];
        let module = link(&Options::default(), &inputs).unwrap();
        for _ in 0..8 {
            assert_eq!(link(&Options::default(), &inputs).unwrap(), module);
        }
        // The imports are in the order of the first reference to each.
        let mut imports = vec![3];
        for field in ["a", "b", "c"] {
            bytes::name(&mut imports, "m");
            bytes::name(&mut imports, field);
            imports.extend([0, 0]);
        }
        assert_eq!(payload(&module, 2), imports);
    }

    #[test]
    fn an_undefined_symbol_names_the_object_that_needs_it() {
        let a = caller("_start");
        let error = link(&Options::default(), &[Input { name: "a.o", bytes: &a }]).unwrap_err();
        assert_eq!(error.message(), "undefined symbol: f (referenced by a.o)");
    }

    #[test]
    fn a_call_through_the_wrong_type_is_an_error() {
        let a = caller("_start");
        let f = callee("f", &[0x60, 0, 0]);
        let inputs = [Input { name: "a.o", bytes: &a }, Input { name: "f.o", bytes: &f }];
        let error = link(&Options::default(), &inputs).unwrap_err();
        assert!(
            error.message().starts_with("function signature mismatch: f is (i32) -> (i32)"),
            "{error}"
        );
    }

    #[test]
    fn a_symbol_defined_twice_is_an_error() {
        let a = caller("_start");
        let f = callee("f", I32_TO_I32);
        let inputs = [
            Input { name: "a.o", bytes: &a },
            Input { name: "f.o", bytes: &f },
            Input { name: "g.o", bytes: &f },
        ];
        let error = link(&Options::default(), &inputs).unwrap_err();
        assert_eq!(error.message(), "f is defined in f.o and in g.o");
    }

    #[test]
    fn a_module_with_no_entry_keeps_only_its_exports() {
        let a = caller("g");
        let f = callee("f", I32_TO_I32);
        let options = Options { entry: None, exports: vec!["f".to_owned()], ..Options::default() };
        let inputs = [Input { name: "a.o", bytes: &a }, Input { name: "f.o", bytes: &f }];
        let module = link(&options, &inputs).unwrap();
        // `g` and `d` are not reached, so there is one function and no data.
        assert_eq!(payload(&module, 3), [1, 0]);
        assert!(sections(&module).iter().all(|&(id, _)| id != 11));
    }

    /// A member loads at the first strong reference to it, and goes in the output after the
    /// members that it loads, which is LLD's order. `f.o` loads `g.o`, so `g` is before `f`,
    /// though `f` is the one that `_start` calls. Loading the members in a queue put `f` first.
    #[test]
    fn a_member_goes_after_the_members_that_it_loads() {
        let a = calls("_start", Some("f"));
        let g = calls("g", None);
        let f = calls("f", Some("g"));
        let lib = archive(&[("g.o", &g), ("f.o", &f)]);
        let inputs = [Input { name: "a.o", bytes: &a }, Input { name: "lib.a", bytes: &lib }];
        let module = link(&Options::default(), &inputs).unwrap();
        let call = |index: u8| [8, 0, 0x10, 0x80 | index, 0x80, 0x80, 0x80, 0, 0x0b];
        let mut code = vec![3];
        code.extend(call(2));
        code.extend([2, 0, 0x0b]);
        code.extend(call(1));
        assert_eq!(payload(&module, 10), code);
    }

    /// The `name` section names the module after the output, as `wasm-ld` does, and shows a
    /// `main` that takes arguments as `main`.
    #[test]
    fn the_name_section_has_the_module_and_main() {
        let a = calls("__main_argc_argv", None);
        let options = Options {
            entry: Some("__main_argc_argv".to_owned()),
            name: Some("a.wasm".to_owned()),
            ..Options::default()
        };
        let names = |a: &[u8]| {
            let module = link(&options, &[Input { name: "a.o", bytes: a }]).unwrap();
            sections(&module)
                .into_iter()
                .find(|&(id, payload)| id == 0 && payload.starts_with(b"\x04name"))
                .map(|(_, payload)| payload[5..].to_vec())
                .unwrap()
        };
        let got = names(&a);
        assert!(got.starts_with(b"\x00\x07\x06a.wasm\x01\x07\x01\x00\x04main"), "{got:?}");
        // A name in the object's own `name` section is the one the output gets.
        let mut a = a;
        custom(&mut a, "name", b"\x01\x07\x01\x00\x04real");
        let got = names(&a);
        assert!(got.starts_with(b"\x00\x07\x06a.wasm\x01\x07\x01\x00\x04real"), "{got:?}");
    }

    /// Each undefined weak function that is called gets its own stub, named after it, also when
    /// two of them have one type, as from `wasm-ld`.
    #[test]
    fn each_called_weak_function_gets_a_stub_of_its_own() {
        let a = weak_caller(["a", "b"]);
        let module = link(&Options::default(), &[Input { name: "a.o", bytes: &a }]).unwrap();
        let start = [0, 0x10, 0x80, 0x80, 0x80, 0x80, 0, 0x10, 0x81, 0x80, 0x80, 0x80, 0, 0x0b];
        let mut code = vec![3, 3, 0, 0x00, 0x0b, 3, 0, 0x00, 0x0b, 14];
        code.extend(start);
        assert_eq!(payload(&module, 10), code);
        let names = sections(&module)
            .into_iter()
            .find(|&(id, payload)| id == 0 && payload.starts_with(b"\x04name"))
            .map(|(_, payload)| payload[5..].to_vec())
            .unwrap();
        let mut funcs = vec![3];
        for (index, func) in ["undefined_weak:a", "undefined_weak:b", "_start"].iter().enumerate() {
            funcs.push(index as u8);
            bytes::name(&mut funcs, func);
        }
        let mut want = vec![1, funcs.len() as u8];
        want.extend(funcs);
        assert!(names.starts_with(&want), "{names:?}");
    }

    /// The stub for a `main` that takes arguments and is not there is named after `main`, as
    /// `wasm-ld` shows `__main_argc_argv` in the `name` section. A start file that calls the
    /// `main` that the program defines, by two weak names, has such a stub.
    #[test]
    fn the_stub_for_main_with_arguments_is_named_after_main() {
        let a = weak_caller(["__main_argc_argv", "b"]);
        let module = link(&Options::default(), &[Input { name: "a.o", bytes: &a }]).unwrap();
        let names = sections(&module)
            .into_iter()
            .find(|&(id, payload)| id == 0 && payload.starts_with(b"\x04name"))
            .map(|(_, payload)| payload[5..].to_vec())
            .unwrap();
        let has = |name: &str| names.windows(name.len()).any(|w| w == name.as_bytes());
        assert!(has("undefined_weak:main"), "{names:?}");
        assert!(!has("__main_argc_argv"), "{names:?}");
    }
}
