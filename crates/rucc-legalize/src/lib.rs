//! IR to IR rewrites into the operations that a back end has, shared by the native back end and
//! the wasm back end.
//!
//! Design: the WebAssembly notes, document 06. Layer rank 11, see `spec/18-package-layout.md`.
//!
//! The native back end in `rucc-codegen` and the wasm back end in `rucc-wasm` are both at rank 12,
//! so neither can depend on the other. A rewrite that both of them run is here, below both of them.
//! Each rewrite reads the IR and writes IR, and it knows nothing about registers, machine
//! instructions or the object format.
//!
//! # Status
//!
//! [`switch`], which splits a `switch` into clusters and builds a tree of tests over them, is
//! here. The other rewrites of the lowering group in `rucc-codegen` move here when the wasm back
//! end needs them.

#![doc(html_root_url = "https://docs.rs/rucc-legalize/0.24.5")]

pub mod switch;
