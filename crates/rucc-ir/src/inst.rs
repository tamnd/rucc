//! What one instruction is, and what a value is.
//!
//! Design: `spec/08-ir.md` sections 8.1 and 8.3.
//!
//! An instruction is an [`Opcode`], a set of [`Flags`], a run of value operands, and whatever
//! else that opcode needs, which is [`Extra`]. Everything that fits in eight bytes is in the
//! [`Extra`] itself and everything larger is an index into a side table, so the instruction
//! stays small enough that walking a function is walking one dense array.
//!
//! A value is the result of an instruction or a parameter of a block, and it is nothing else.
//! There is no constant operand kind: a constant is an [`Opcode::IConst`] with a result like
//! any other instruction. That is what makes the dominance rule in the verifier a single rule
//! with no exceptions, and it costs nothing, because a constant with no uses is deleted by the
//! same pass that deletes anything else with no uses.

use rucc_base::{Idx, IdxRange, Symbol};

use crate::{ExtraKind, Flags, FloatPred, IntPred, MemOrder, Opcode, RmwOp, Type};

/// One value: the result of an instruction, or a parameter of a block.
pub type Value = Idx<ValueData>;
/// One instruction, in the function that owns it.
pub type Inst = Idx<InstData>;
/// One basic block, in the function that owns it.
pub type Block = Idx<BlockData>;

/// The table of references to values, which is what an operand list is a run of.
#[derive(Debug)]
pub struct ValueRef;
/// A run of value operands.
pub type ValueList = IdxRange<ValueRef>;
/// A run of branch targets, which is what a terminator's successors are.
pub type BlockCallList = IdxRange<BlockCall>;
/// A run of immediates, which is what a `switch` holds its case values in.
pub type ImmList = IdxRange<Imm>;

/// A constant, in the immediate table.
///
/// The bits and nothing else. An integer is stored two's complement in as many of the low bits
/// as its type is wide, and a floating point value is stored as its bit pattern, so the same
/// table holds both and the type on the result says how to read it. That keeps a bit-preserving
/// answer for a NaN payload, which a value of a Rust floating type would not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Imm(u128);

impl Imm {
    /// The bits, as they are stored.
    #[must_use]
    pub const fn bits(self) -> u128 {
        self.0
    }

    /// An immediate holding these bits.
    #[must_use]
    pub const fn from_bits(bits: u128) -> Self {
        Self(bits)
    }

    /// An integer, with the bits above `ty` cleared.
    ///
    /// A value is stored in exactly the width its type has, so two immediates are equal when
    /// they are the same value, which is what lets an equality on the table stand in for an
    /// equality on the numbers.
    ///
    /// # Panics
    ///
    /// Panics if `ty` is not an integer type.
    #[must_use]
    pub fn int(value: i128, ty: Type) -> Self {
        assert!(ty.is_int(), "an integer immediate needs an integer type");
        Self(value as u128 & mask(ty.bits()))
    }

    /// The value read as unsigned.
    #[must_use]
    pub const fn unsigned(self) -> u128 {
        self.0
    }

    /// The value read as signed, with the sign bit of `ty` extended.
    ///
    /// # Panics
    ///
    /// Panics if `ty` is not an integer type.
    #[must_use]
    pub fn signed(self, ty: Type) -> i128 {
        assert!(ty.is_int(), "an integer immediate needs an integer type");
        let spare = 128 - ty.bits();
        // Shifting left and then arithmetic right is the branch-free way to sign extend from
        // an arbitrary width, and it is correct for a width of 128 because the shift is zero.
        ((self.0 << spare) as i128) >> spare
    }
}

/// The low `bits` bits set, and a width of 128 meaning all of them.
fn mask(bits: u32) -> u128 {
    if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 }
}

/// A branch target, and the values passed to it.
///
/// This is the whole reason there are no phi nodes. The arguments are here, in the branch,
/// beside the block they go to, so removing a predecessor is one edit in one place and there
/// is no second list anywhere that has to be kept in step with this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockCall {
    /// Where control goes.
    pub block: Block,
    /// What is passed, one for each of the block's parameters.
    pub args: ValueList,
}

/// What defines a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Def {
    /// The result of an instruction, at this position among its results.
    Result {
        /// The instruction.
        inst: Inst,
        /// Which of its results this is.
        index: u8,
    },
    /// A parameter of a block, at this position among its parameters.
    Param {
        /// The block.
        block: Block,
        /// Which of its parameters this is.
        index: u32,
    },
}

/// One value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueData {
    /// Its type.
    pub ty: Type,
    /// Where it comes from.
    pub def: Def,
}

/// What an access does beyond naming an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    /// How many bytes the access covers, for the ones whose size is not their result type.
    ///
    /// A `load` takes its size from the type it produces. An `alloca` and a `memset` do not,
    /// and this is where theirs is.
    pub size: u64,
    /// The alignment the access is known to have, in bytes.
    pub align: u32,
    /// How strongly it is ordered, with [`MemOrder::NotAtomic`] for an ordinary access.
    pub order: MemOrder,
    /// The type-based aliasing node, if the front end knew one.
    pub tbaa: Option<Meta>,
}

/// A metadata node, in the module's table.
pub type Meta = Idx<MetaNode>;

/// A node of the metadata graph, which for now is only what aliasing needs.
///
/// The tree this forms is checked by the verifier, since a cycle in it would make the aliasing
/// query that walks it not terminate, and the place to find that out is here and not there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetaNode {
    /// What this node is called, which is what the printer writes and the parser reads.
    pub name: Symbol,
    /// The node one level up, with the root having none.
    pub parent: Option<Meta>,
    /// The offset within the parent, for a member of a struct type.
    pub offset: u64,
}

/// What a call needs beyond its arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallInfo {
    /// The name, for a direct call. `None` for a call through an address, where the address is
    /// the first operand.
    pub callee: Option<Symbol>,
    /// The signature it is called with, which is where the ABI attributes are.
    pub signature: Sig,
}

/// A signature, in the function's table.
pub type Sig = Idx<Signature>;

/// What a `switch` needs beyond the value it switches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitchInfo {
    /// The targets, with the default first and one for each case after it.
    pub targets: BlockCallList,
    /// The case values, one for each target after the default.
    pub cases: ImmList,
}

/// What inline assembly needs.
///
/// The semantics belong to the inline assembly document. What is here is the shape: a
/// template, the constraints, the clobbers, and the successors that make `asm goto` the one
/// instruction whose being a terminator is a property of the instruction and not the opcode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AsmInfo {
    /// The template string, as written.
    pub template: Symbol,
    /// The constraint list, as written.
    pub constraints: Symbol,
    /// The clobber list, as written.
    pub clobbers: Symbol,
    /// The labels, which are empty for everything except `asm goto`.
    pub targets: BlockCallList,
}

/// Everything an instruction carries that is not a value operand.
///
/// Anything that fits in eight bytes is here and anything larger is an index into a side
/// table, so that the common instructions, which are the arithmetic ones carrying nothing at
/// all, do not pay for the rare ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extra {
    /// Nothing, which is most instructions.
    None,
    /// A constant, for `iconst`, `fconst` and `splat`.
    Imm(Idx<Imm>),
    /// A name, for `global_addr` and for a target-specific intrinsic.
    Symbol(Symbol),
    /// Which comparison, for `icmp`.
    IntPred(IntPred),
    /// Which comparison, for `fcmp`.
    FloatPred(FloatPred),
    /// An access, for the loads, the stores, the copies and `alloca`.
    Mem(Idx<MemInfo>),
    /// An atomic read-modify-write, which is an access and which operation.
    Rmw(RmwOp, Idx<MemInfo>),
    /// A barrier's ordering, for `fence`.
    Order(MemOrder),
    /// The targets of a branch, with the default first for a `switch`.
    Targets(BlockCallList),
    /// A call.
    Call(Idx<CallInfo>),
    /// A `switch`, which is targets and the values that select them.
    Switch(Idx<SwitchInfo>),
    /// Inline assembly.
    Asm(Idx<AsmInfo>),
}

impl Extra {
    /// Which shape this is, without the payload.
    ///
    /// The verifier compares this with [`Opcode::extra_kind`], because an instruction carrying
    /// the payload of some other opcode prints as text the parser cannot read back.
    #[must_use]
    pub const fn kind(self) -> ExtraKind {
        match self {
            Self::None => ExtraKind::None,
            Self::Imm(_) => ExtraKind::Imm,
            Self::Symbol(_) => ExtraKind::Symbol,
            Self::IntPred(_) => ExtraKind::IntPred,
            Self::FloatPred(_) => ExtraKind::FloatPred,
            Self::Mem(_) => ExtraKind::Mem,
            Self::Rmw(..) => ExtraKind::Rmw,
            Self::Order(_) => ExtraKind::Order,
            Self::Targets(_) => ExtraKind::Targets,
            Self::Call(_) => ExtraKind::Call,
            Self::Switch(_) => ExtraKind::Switch,
            Self::Asm(_) => ExtraKind::Asm,
        }
    }
}

/// One instruction.
///
/// There is no result type here. Each result is a value in the function's value table and the
/// type is on the value, which means a reader asking what an instruction produces asks the
/// same question about `add` as about `call`, and there is no second copy of the type to
/// disagree with the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstData {
    /// Which instruction this is.
    pub opcode: Opcode,
    /// What the optimizer is licensed to assume about it.
    pub flags: Flags,
    /// How many values it produces.
    pub results: u8,
    /// The first of them, with the rest following it in the value table.
    pub first_result: Option<Value>,
    /// Its value operands.
    pub args: ValueList,
    /// Everything else it carries.
    pub extra: Extra,
}

impl InstData {
    /// An instruction with no operands, no flags, no results and nothing extra.
    #[must_use]
    pub const fn new(opcode: Opcode) -> Self {
        Self {
            opcode,
            flags: Flags::NONE,
            results: 0,
            first_result: None,
            args: ValueList::EMPTY,
            extra: Extra::None,
        }
    }

    /// The values it produces, in order.
    pub fn results(&self) -> impl Iterator<Item = Value> + use<> {
        let first = self.first_result.map_or(0, Idx::raw);
        (0..u32::from(self.results)).map(move |offset| Value::new(first + offset))
    }

    /// The run of targets it branches to, which is empty when it does not branch.
    ///
    /// A `switch` keeps its targets in a side table, so this reads `Extra::Targets` only and
    /// the function is what answers for the rest.
    #[must_use]
    pub fn targets(&self) -> BlockCallList {
        match self.extra {
            Extra::Targets(targets) => targets,
            _ => BlockCallList::EMPTY,
        }
    }
}

/// What a function takes and returns.
///
/// A signature is not a type. Nothing in the IR has a function type, because a `ptr` has no
/// pointee and there is nothing else a function type could sit on. A `call_indirect` names the
/// signature it is called with, and that is where the ABI attributes are read from.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Signature {
    /// What it takes, in their C-level form before the ABI has been applied.
    pub params: Vec<Type>,
    /// What it returns, which is empty for a `void` function.
    pub returns: Vec<Type>,
    /// Whether it takes arguments beyond the ones named.
    pub variadic: bool,
}

impl Signature {
    /// A signature taking and returning nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same signature with these parameters.
    #[must_use]
    pub fn with_params(mut self, params: &[Type]) -> Self {
        self.params = params.to_vec();
        self
    }

    /// The same signature returning these.
    #[must_use]
    pub fn with_returns(mut self, returns: &[Type]) -> Self {
        self.returns = returns.to_vec();
        self
    }

    /// The same signature, variadic.
    #[must_use]
    pub fn variadic(mut self) -> Self {
        self.variadic = true;
        self
    }
}

/// One basic block: parameters, then instructions, then exactly one terminator.
///
/// The instructions are a doubly linked list rather than a vector, so that inserting one in
/// the middle of a block does not move the ones after it. An optimizer does that constantly,
/// and a move would invalidate every [`Inst`] anybody was holding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockData {
    /// The values arriving here, which is what other IRs spell as phi nodes.
    ///
    /// A `Vec` and not a run in a pool, because SSA construction adds a parameter to a loop
    /// header long after the blocks that come after it have been built, and a run in a pool
    /// cannot grow in the middle.
    pub params: Vec<Value>,
    /// The first instruction, or `None` for a block nothing has been put in yet.
    pub first: Option<Inst>,
    /// The last instruction, which is the terminator once the block is finished.
    pub last: Option<Inst>,
    /// The block before this one in layout order.
    pub prev: Option<Block>,
    /// The block after it.
    pub next: Option<Block>,
}

/// Where one instruction sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstLayout {
    /// The block it is in, or `None` if it has been made and not yet inserted.
    pub block: Option<Block>,
    /// The instruction before it in that block.
    pub prev: Option<Inst>,
    /// The instruction after it.
    pub next: Option<Inst>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_immediate_keeps_only_the_bits_its_type_has() {
        let byte = Type::int(8);
        assert_eq!(Imm::int(-1, byte).unsigned(), 0xff);
        assert_eq!(Imm::int(-1, byte).signed(byte), -1);
        assert_eq!(Imm::int(255, byte), Imm::int(-1, byte));
        assert_eq!(Imm::int(127, byte).signed(byte), 127);
        assert_eq!(Imm::int(128, byte).signed(byte), -128);
    }

    #[test]
    fn a_widest_immediate_is_not_truncated() {
        let word = Type::int(128);
        assert_eq!(Imm::int(i128::MIN, word).signed(word), i128::MIN);
        assert_eq!(Imm::int(i128::MAX, word).signed(word), i128::MAX);
        assert_eq!(Imm::int(-1, word).unsigned(), u128::MAX);
    }

    #[test]
    fn a_one_bit_immediate_is_a_bit() {
        let bit = Type::I1;
        assert_eq!(Imm::int(1, bit).unsigned(), 1);
        assert_eq!(Imm::int(3, bit).unsigned(), 1);
        assert_eq!(Imm::int(2, bit).unsigned(), 0);
        // The one bit is the sign bit, so the only two values are zero and minus one.
        assert_eq!(Imm::int(1, bit).signed(bit), -1);
    }

    #[test]
    fn a_floating_immediate_keeps_its_bits() {
        let bits = f64::NAN.to_bits() | 0x7;
        assert_eq!(Imm::from_bits(u128::from(bits)).bits(), u128::from(bits));
    }

    #[test]
    fn an_instruction_with_no_results_yields_none() {
        let inst = InstData::new(Opcode::Store);
        assert_eq!(inst.results().count(), 0);
    }

    #[test]
    fn results_follow_the_first_one() {
        let mut inst = InstData::new(Opcode::SAddOverflow);
        inst.first_result = Some(Value::new(4));
        inst.results = 2;
        let got: Vec<u32> = inst.results().map(Idx::raw).collect();
        assert_eq!(got, [4, 5]);
    }

    #[test]
    fn a_jump_says_where_it_goes() {
        let mut inst = InstData::new(Opcode::Jump);
        inst.extra = Extra::Targets(BlockCallList::new(Idx::new(0), Idx::new(1)));
        assert_eq!(inst.targets().len(), 1);
    }

    #[test]
    fn a_signature_is_built_by_saying_what_it_takes_and_returns() {
        let sig = Signature::new()
            .with_params(&[Type::int(32), Type::PTR])
            .with_returns(&[Type::int(32)])
            .variadic();
        assert_eq!(sig.params, [Type::int(32), Type::PTR]);
        assert_eq!(sig.returns, [Type::int(32)]);
        assert!(sig.variadic);
        assert_eq!(Signature::new(), Signature::default());
    }

    #[test]
    fn an_instruction_stays_small() {
        // Not a promise, a tripwire. Every function in the program is a run of these, and a
        // change that doubles this should be a change somebody decided to make.
        assert!(size_of::<InstData>() <= 32, "{}", size_of::<InstData>());
        assert_eq!(size_of::<ValueData>(), 16);
        assert_eq!(size_of::<Extra>(), 12);
    }
}
