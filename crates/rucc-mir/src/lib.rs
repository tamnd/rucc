//! The machine IR, still in SSA, and its printer and parser.
//!
//! Design: `spec/10-backend.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! MIR is the second representation. It is still a control flow graph of blocks and it is still
//! in SSA form, but the instructions are the target's instructions and the operands are
//! registers drawn from the target's register classes. Instruction selection produces it, the
//! allocator rewrites it, and the encoder reads it.
//!
//! Nothing in this crate knows what any instruction means. An opcode is a name, an operand is a
//! register with a class and a role, and that is all a pass over MIR needs in order to move
//! instructions, split live ranges or insert a spill. What the opcodes mean is in the target's
//! rule set, which is what the selector was compiled from, and in the encoder, which is
//! generated from the same description. That is what `spec/10-backend.md` section 10.8 means
//! when it says no pipeline crate holds target-specific code.
//!
//! # The shape of an instruction
//!
//! An opcode, an operand vector, an optional immediate, an optional memory addressing mode, and
//! the symbol it names. Twenty-four bytes. The operands are in one order, the ones the
//! instruction writes and then the ones it reads, with the registers a memory operand names
//! last, and [`InstBuilder`] is what keeps them that way.
//!
//! Where an instruction goes is on its block rather than on the instruction, in the order the
//! terminator's own arms run, which is regalloc2's arrangement and the one the allocator
//! interface in `spec/10-backend.md` section 10.4 follows. Where an instruction came from is a
//! parallel array, reached by [`Func::span`], for the same reason `rucc-ir` puts it there.
//!
//! # Before and after allocation
//!
//! ```text
//! mfunc @scale {                         mfunc @scale {
//! block0(%0:gpr, %1:gpr):                block0:
//!     %2:gpr = x64.mov_ri 4                  $rcx = x64.mov_ri 4
//!     %3:gpr(reuse 1) = x64.imul_rr ...      $rax = x64.imul_rr $rax, $rcx
//! ```
//!
//! Both are the same text form, printed by [`print()`] and read by [`parse()`], and both round-trip
//! byte for byte. That is what `--emit=mir` and `--emit=mir-final` are, and it is what lets a
//! test of the allocator state its input by writing one down.
//!
//! # Status
//!
//! The representation, the printer and the parser are here. What comes next in `M3` is the
//! lowering that produces MIR, the frame layout and the allocator that rewrite it, and the
//! encoder that reads it. Two things a call needs are deliberately not here yet, because they
//! belong with the ABI lowering that is the next piece rather than with the representation: the
//! set of registers a call clobbers, and the stack slots a frame is made of.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-mir/0.7.3")]

#[cfg(test)]
mod fixtures;
mod func;
mod inst;
mod parse;
mod print;

pub use func::{Binding, Func, InstBuilder, defs};
pub use inst::{
    Amode, Block, BlockCall, BlockData, Imm, ImmRef, Inst, InstData, Mem, MemRef, Opcode, Operand,
    OperandList, Param, Reg,
};
// An operand's role and its constraint are a target's description of an instruction before they
// are anything in the machine IR, so they are written down in `rucc-target` where a target
// description can reach them. They are still part of this crate's vocabulary, because the
// machine IR is where every pass reads them.
pub use parse::{ParseError, parse};
pub use print::{Printer, print, print_func};
pub use rucc_target::{Constraint, Role};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";
