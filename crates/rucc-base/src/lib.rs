//! Arenas, interning, index newtypes and the small data structures the rest of the
//! compiler is built out of.
//!
//! Design: `spec/03-architecture.md`. Layer rank 0, see `spec/18-package-layout.md`.
//!
//! Nothing in here knows anything about C. That is deliberate: this is the one crate every
//! other crate depends on, so anything C-specific that leaks in here becomes impossible to
//! avoid later.
//!
//! [`float`] is here for the same reason. Binary floating point is not a C question, and both
//! ends of the compiler need the same answer to it: the lexer converting a constant and the
//! optimizer folding one have to agree to the last bit, and neither may ask the host what a
//! number means, because the same source has to give the same bits whoever compiles it.
//!
//! # Status
//!
//! The index newtypes, the interner and the software float are real and used. The arena is a
//! thin wrapper for now and grows with `M2`. Float arithmetic is not here yet: conversion from
//! text is, which is what a constant needs, and the operations come with the constant
//! evaluator.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-base/0.2.4")]

mod decimal;
pub mod float;
pub mod index;
pub mod intern;
mod scope;

pub use index::{Idx, IdxRange};
pub use intern::{Interner, Symbol};
pub use scope::ScopeMap;
