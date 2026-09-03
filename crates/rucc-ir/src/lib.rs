//! The SSA IR with block parameters, and its printer, parser and verifier.
//!
//! Design: `spec/08-ir.md`. Layer rank 8, see `spec/18-package-layout.md`.
//!
//! One IR, target-independent, in SSA form, produced by lowering the typed AST and consumed by
//! the optimizer and the code generator. The MIR is a second, target-dependent representation
//! and lives in its own crate.
//!
//! **Block parameters, not phi nodes.** A phi is a pseudo-instruction whose operands are
//! positionally tied to a predecessor list stored somewhere else, and every IR built that way
//! collects bugs where the two get out of step during a CFG edit. Block parameters put the
//! correspondence in the branch, where it belongs: `br_if %c, block2(%a, %b), block3(%d)`.
//! Removing a predecessor is then a local edit, and there is no such thing as a malformed phi.
//!
//! # What is here so far
//!
//! The vocabulary: [`Type`] and [`Float`] for the type system, [`Opcode`] with [`IntPred`] and
//! [`FloatPred`] for the instruction set, [`Flags`] with [`MemOrder`] and [`RmwOp`] for what
//! rides on an instruction, and [`Attrs`] for what is true of a whole function rather than of
//! one instruction in it. [`Signature`] for what a function takes and returns, with [`Abi`] on
//! each [`Param`] for what a call asks of it beyond its type, which is the one thing about a C
//! function that reading the C cannot answer. The containers: [`Func`], holding [`BlockData`] and
//! [`InstData`] and [`ValueData`] in flat tables, with [`Builder`] to append to it, and
//! [`Module`], holding the functions with the [`Global`]s and the [`Alias`]es and the target
//! they are all for. The textual form: [`print()`], which writes a module out, and [`parse()`],
//! which reads it back byte for byte. And [`verify()`], which says whether a module is one the
//! rest of the compiler may believe.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-ir/0.3.2")]

mod attrs;
#[cfg(test)]
mod fixtures;
mod flags;
mod func;
mod inst;
mod module;
mod opcode;
mod parse;
mod print;
mod ty;
mod verify;

pub use attrs::{AttrSet, Attrs, FpContract};
pub use flags::{Flags, MemOrder, RmwOp};
pub use func::{Builder, Counts, Func};
pub use inst::{
    Abi, AbiList, AsmInfo, Block, BlockCall, BlockCallList, BlockData, CallInfo, Def, Extra, Imm,
    ImmList, Inst, InstData, InstLayout, MemInfo, Meta, MetaNode, Param, Sig, Signature,
    SwitchInfo, Value, ValueData, ValueList, ValueRef,
};
pub use module::{
    Alias, AliasId, AliasKind, Byte, ByteRange, DataLayout, DataList, Datum, FuncId, Global,
    GlobalId, Linkage, Module, ModuleCounts, Reloc, SymbolRef, TlsModel, Visibility,
};
pub use opcode::{ExtraKind, FloatPred, IntPred, Opcode};
pub use parse::{ParseError, parse};
pub use print::{Printer, print, print_func};
pub use ty::{Float, Kind, Type};
pub use verify::{VerifyError, verify, verify_func};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

/// The version of the textual form, written in the module header.
///
/// The IR is not a stable interface before 1.0 and this number does not promise that it is.
/// It exists so that a dump from one build read by another build fails with something a person
/// can act on rather than with a parse error twenty lines in.
pub const FORMAT_VERSION: u32 = 0;

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
