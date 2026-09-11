//! The target tuple for rucc.
//!
//! A target is not three strings. It is the set of facts that change the bytes the compiler
//! emits, and there are ten of them: the architecture, the baseline within it, the byte order,
//! the data model, the OS, the OS version, the environment, the environment version, the float
//! ABI, and the object format. `spec/cross-compile/03-target-model.md` argues for each one, and the argument
//! is always the same: if the fact changes how a function is called or how a struct is laid out,
//! then two objects that disagree about it do not link, so it is part of the target's identity
//! and not a flag.
//!
//! That identity property is what the rest of the cross compilation work is built on. The
//! sysroot cache in `spec/cross-compile/13-distribution.md` is keyed on a tuple, the stub libraries in
//! `spec/cross-compile/09-libc-stubs.md` are generated per tuple, and the report that decides a target's tier
//! is indexed by tuple. All three break if two spellings of one target produce two keys, or if
//! one spelling produces two different sets of bytes.
//!
//! # What this replaces
//!
//! The compiler currently has a three field `Triple` with the architecture, the OS and the
//! environment, and two functions on `Arch` that answer the pointer width and the byte order.
//! Both functions are wrong for targets in `spec/cross-compile/04-target-matrix.md`: a 64-bit architecture on
//! Windows has a 32-bit `long`, `x86_64-linux-gnux32` has 32-bit pointers, and five of the ten
//! architectures here have a big endian mode. The old `FromStr` also had a catch-all arm that
//! discarded any component it did not recognize, so `x86_64-linux-gnu.2.28` parsed as
//! `x86_64-linux-gnu` and the pinned glibc version vanished without a word.
//!
//! # Example
//!
//! ```
//! use rucc_tuple::{DataModel, ObjectFormat, TargetTuple};
//!
//! let target: TargetTuple = "x86_64-pc-windows-msvc".parse().unwrap();
//! assert_eq!(target.data_model(), DataModel::Llp64);
//! assert_eq!(target.data_model().long_width(), 32);
//! assert_eq!(target.pointer_width(), 64);
//! assert_eq!(target.object_format(), ObjectFormat::Coff);
//! assert_eq!(target.to_canonical_string(), "x86_64-windows-msvc");
//! assert_eq!(target.to_llvm_string(), "x86_64-pc-windows-msvc");
//! ```

#![doc(html_root_url = "https://docs.rs/rucc-tuple/0.10.19")]
// Every public item here is read by somebody bringing up a target who has the psABI document
// open beside it, so an undocumented one is a question they have to answer by reading the body.
#![deny(missing_docs)]

mod abi;
mod arch;
mod error;
mod os;
mod parse;
mod table;
mod tuple;

pub use abi::{Abi, ObjectFormat};
pub use arch::{Arch, DataModel, Endian, SubArch};
pub use error::Error;
pub use os::{Env, Os, Version};
pub use table::{
    Host, TARGETS, TargetEntry, Tier, counts_by_planned_tier, lookup, planned_working_count,
};
pub use tuple::{TargetTuple, TupleBuilder};
