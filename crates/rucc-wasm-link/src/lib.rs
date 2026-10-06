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
//! [`link`] does the whole job: it reads the objects and the archives, resolves the symbols,
//! fetches the archive members that define what is undefined, drops what the roots do not reach,
//! lays out the memory and the table, applies the relocations and writes the module. The readers
//! are public too: [`object`] for one object and [`archive`] for the members of an archive. The
//! driver does not use this crate yet.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-wasm-link/0.25.0")]

use core::fmt;

pub mod archive;
mod bytes;
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
    /// Leave out the `name` section, as `--strip-all`.
    pub strip: bool,
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
    use crate::testing::{archive, callee, caller};

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
        assert_eq!(ids, [1, 3, 4, 5, 6, 7, 10, 11, 0, 0]);
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
        // The stack pointer starts at the top of the stack.
        assert_eq!(payload(&module, 6), [1, 0x7f, 1, 0x41, 0x80, 0x80, 4, 0x0b]);
        let mut exports = vec![2];
        bytes::name(&mut exports, "memory");
        exports.extend([2, 0]);
        bytes::name(&mut exports, "_start");
        exports.extend([0, 0]);
        assert_eq!(payload(&module, 7), exports);
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
}
