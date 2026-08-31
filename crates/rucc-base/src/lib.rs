//! Arenas, interning, index newtypes and the small data structures the rest of the
//! compiler is built out of.
//!
//! Design: `spec/03-architecture.md`. Layer rank 0, see `spec/18-package-layout.md`.
//!
//! Nothing in here knows anything about C. That is deliberate: this is the one crate every
//! other crate depends on, so anything C-specific that leaks in here becomes impossible to
//! avoid later.
//!
//! # Status
//!
//! The index newtypes and the interner are real and used. The arena is a thin wrapper for
//! now and grows with `M2`.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-base/0.1.0")]

pub mod index;
pub mod intern;

pub use index::{Idx, IdxRange};
pub use intern::{Interner, Symbol};
