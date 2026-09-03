//! The typed AST to IR walk: SSA construction and ABI-directed lowering.
//!
//! Design: `spec/08-ir.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! # What is here so far
//!
//! [`lower`], the walk from the typed tree to the IR, and [`Ssa`], the SSA construction by
//! Braun's algorithm that it drives: the thing that lets a local variable become a value
//! without ever having been a stack slot.
//!
//! The walk is in four files, one per level of the thing it walks. `unit` is the translation
//! unit: objects with static storage, their images, and the functions. `body` is one function:
//! the statements, the control flow, and the expressions. `repr` is the answer to what a C type
//! is once the IR is the one asking, and `abi` is the answer to how a call travels, which is the
//! target's rather than C's.
//!
//! What it does not build yet is reported rather than mislowered. An `asm` at file scope, a
//! `goto` in a function that has a variable length array in it and a `va_arg` that reads a
//! structure each become a diagnostic, so a program that uses one fails to compile rather than
//! compiling into something that is not what it says.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-lower/0.3.6")]

mod abi;
mod bits;
mod body;
mod repr;
mod ssa;
mod unit;

pub use ssa::{Ssa, Var};
pub use unit::{Context, Lowered, lower};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
