//! Arenas, interning, index newtypes and the small data structures the rest of the
//! compiler is built out of.
//!
//! Design: `spec/03-architecture.md`. Layer rank 0, see `spec/18-package-layout.md`.
//!
//! Nothing in here knows anything about C. That is deliberate: this is the one crate every
//! other crate depends on, so anything C-specific that leaks in here becomes impossible to
//! avoid later.
//!
//! [`rules`] is here for the same reason, one level up. It is the walk over the automaton a
//! rule file compiles into, and both `rucc-opt` rewriting IR to IR and `rucc-codegen` lowering
//! IR to machine terms match with it. A rewrite and a lowering are the same claim about two
//! terms, so writing the walk twice would be writing two chances to disagree.
//!
//! [`float`] is here for the same reason. Binary floating point is not a C question, and both
//! ends of the compiler need the same answer to it: the lexer converting a constant and the
//! optimizer folding one have to agree to the last bit, and neither may ask the host what a
//! number means, because the same source has to give the same bits whoever compiles it.
//!
//! # Status
//!
//! The index newtypes, the interner and the software float are real and used. The arena is a
//! thin wrapper for now and grows with `M2`. The rule matcher is real and used by the x86-64
//! selector, and gains its second caller with the rewrite rules of `M4.2`. The float has both
//! halves of what a constant needs: conversion from text, and the arithmetic the constant
//! evaluator folds with, each of them correctly rounded and neither of them asking the host
//! anything.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-base/0.5.2")]

mod decimal;
pub mod float;
pub mod index;
pub mod intern;
pub mod rules;
mod scope;

pub use index::{Idx, IdxRange};
pub use intern::{Interner, Symbol, sym};
pub use scope::ScopeMap;
