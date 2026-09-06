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

use rucc_target::Slot;

use crate::{
    ExtraKind, Flags, FloatPred, IntPred, MemOrder, Opcode, Owner, RmwOp, StorageClass, Type,
};

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
/// A run of ABI attributes, which is what a call says about the arguments its signature does
/// not name.
pub type AbiList = IdxRange<Abi>;
/// A run of eightbytes, which is how an object read off a variable argument list travelled.
pub type SlotList = IdxRange<Slot>;

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
    /// Which `restrict` scope the access is in and which pointer it went through.
    pub restrict: Restrict,
}

/// Which `restrict` scope an access is in, and which pointer inside that scope it went through.
///
/// Two small numbers, which is the whole of the mechanism. GCC spells them
/// `MR_DEPENDENCE_CLIQUE` and `MR_DEPENDENCE_BASE` at `gcc/tree-ssa-alias.cc:2503` and the rule
/// is one line: same clique and different base means the two accesses cannot touch the same
/// byte, because that is exactly what `restrict` promises. A clique is one scope, numbered as
/// lowering enters it, and a base is one `restrict` pointer declared inside it. Clique zero
/// means nothing is known, which is what every access that is not under a `restrict` gets.
///
/// This is spec 9.4's scope tree rather than a blanket assumption, and it costs four bytes that
/// were padding in [`MemInfo`] already.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restrict {
    /// The scope, with zero meaning no information.
    pub clique: u16,
    /// The pointer within that scope, which only means anything when the clique is not zero.
    pub base: u16,
}

impl Restrict {
    /// No information, which is what an access outside any `restrict` scope carries.
    pub const NONE: Self = Self { clique: 0, base: 0 };

    /// Whether `restrict` says these two accesses cannot touch the same byte.
    ///
    /// Only accesses. GCC's PR71062 is what happens when this answer is used to fold a
    /// comparison of the two pointers: `restrict` constrains what is read and written through a
    /// pointer and says nothing about what the pointer's value is, so two pointers that may not
    /// be used to reach the same object can still compare equal. A rule that folds `p == q` to
    /// false on the strength of this is wrong.
    #[must_use]
    pub const fn disjoint(self, other: Self) -> bool {
        self.clique != 0 && self.clique == other.clique && self.base != other.base
    }
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
    /// What the ABI asks of the arguments the signature does not name, one entry for each of
    /// them.
    ///
    /// Only a variadic call has any, because only a variadic call passes an argument no
    /// parameter stands for, and it is empty when every one of them travels as the value in
    /// hand, which is nearly always. A structure the classification puts in the argument area
    /// is the case it exists for: the bytes travel and there is no parameter to hang the
    /// [`Abi::ByVal`] on, so it hangs here instead.
    pub varargs: AbiList,
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

/// What an object read off a variable argument list is.
///
/// The access says how many bytes it is and what it is aligned to, which is the whole of what an
/// object the convention put in the caller's argument area needs: it is there, and those two say
/// where the argument behind it starts. An object that travelled in registers is not there at all.
/// It is in the callee's own register save area, in as many places as it has eightbytes, and which
/// register file each of those came from is not something the size and the alignment say. So the
/// slots say it, and they are empty for the object that went in memory.
///
/// The classification is the front end's, because it is the one that still has the type. By the
/// time an instruction reaches a backend the type is a size and an alignment, and the algorithm in
/// section 3.5.7 of the psABI wants more than that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaInfo {
    /// The object, as any other access describes one.
    pub mem: Idx<MemInfo>,
    /// Where each of its eightbytes travelled, or nothing at all for one that travelled whole in
    /// the caller's memory.
    pub slots: SlotList,
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
    /// An object read off a variable argument list, which is an access and how it travelled.
    VaObject(Idx<VaInfo>),
    /// What kind of storage an instance is, for `meta_begin`.
    Class(StorageClass),
    /// Who a range went to, for `meta_transfer`.
    Owner(Owner),
    /// A metadata node, for `meta_type`, which is the one plane write that names a type.
    Node(Meta),
    /// Why an exemption was declared, for `safe_region_begin`.
    Reason(Symbol),
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
            Self::VaObject(_) => ExtraKind::VaObject,
            Self::Class(_) => ExtraKind::Class,
            Self::Owner(_) => ExtraKind::Owner,
            Self::Node(_) => ExtraKind::Node,
            Self::Reason(_) => ExtraKind::Reason,
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

/// How one parameter or one return value travels, beyond what its type says.
///
/// The IR's types are the machine's and not C's, so a `ptr` parameter says nothing about
/// whether the pointer is the argument or whether the object it points at is, and an `i8` says
/// nothing about which half of the register above it the callee may read. Both are the ABI's
/// answer rather than the type's, which is why they are here and not on [`Type`].
///
/// A signature carrying one of these has already had the ABI applied to it. What the walk to
/// the IR builds first is the C-level form, where every parameter is [`Abi::Plain`], and the
/// classification in `rucc-target` is what turns one into the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Abi {
    /// The value itself, in the type it is written as.
    #[default]
    Plain,
    /// An integer narrower than a register, with the bits above it its own sign.
    ///
    /// Which of these an ABI asks for is not a property of the value: `unsigned char` is
    /// [`Abi::Sext`] on the Darwin ABIs and [`Abi::Zext`] elsewhere, and on SysV neither the
    /// caller nor the callee may assume anything about those bits at all.
    Sext,
    /// An integer narrower than a register, with zeroes above it.
    Zext,
    /// The bytes of the object the pointer points at, in the argument area, with no address
    /// travelling anywhere.
    ///
    /// The caller makes the copy the callee is free to write to, which is what makes this a C
    /// call by value rather than a pointer the callee must not keep.
    ByVal {
        /// How many bytes travel.
        size: u64,
        /// What the copy is aligned to, which is the C alignment of the type and not the
        /// pointer's.
        align: u32,
    },
    /// Somewhere for the return value to go, whose address the caller passes as the first
    /// argument because the value does not fit in the registers a return comes back in.
    Sret {
        /// How many bytes the callee writes.
        size: u64,
        /// What the space is aligned to.
        align: u32,
    },
}

impl Abi {
    /// Whether this describes an object behind a pointer rather than the value in hand.
    #[must_use]
    pub const fn indirect(self) -> bool {
        matches!(self, Self::ByVal { .. } | Self::Sret { .. })
    }

    /// The size and alignment of that object, for the two that have one.
    #[must_use]
    pub const fn object(self) -> Option<(u64, u32)> {
        match self {
            Self::ByVal { size, align } | Self::Sret { size, align } => Some((size, align)),
            _ => None,
        }
    }
}

/// One parameter, or one return value: a type and how it travels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    /// The type the IR sees, which for the indirect forms is `ptr`.
    pub ty: Type,
    /// What the ABI asks of it.
    pub abi: Abi,
}

impl Param {
    /// A parameter of this type, in its C-level form.
    #[must_use]
    pub const fn new(ty: Type) -> Self {
        Self { ty, abi: Abi::Plain }
    }

    /// A parameter of this type travelling this way.
    #[must_use]
    pub const fn with_abi(ty: Type, abi: Abi) -> Self {
        Self { ty, abi }
    }
}

/// What a function takes and returns.
///
/// A signature is not a type. Nothing in the IR has a function type, because a `ptr` has no
/// pointee and there is nothing else a function type could sit on. A `call_indirect` names the
/// signature it is called with, and that is where the ABI attributes are read from.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Signature {
    /// What it takes, in their C-level form until the ABI has been applied.
    pub params: Vec<Param>,
    /// What it returns, which is empty for a `void` function and for one whose return value
    /// comes back through an [`Abi::Sret`] parameter.
    pub returns: Vec<Param>,
    /// Whether it takes arguments beyond the ones named.
    pub variadic: bool,
}

impl Signature {
    /// A signature taking and returning nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same signature with these parameters, each in its C-level form.
    #[must_use]
    pub fn with_params(mut self, params: &[Type]) -> Self {
        self.params = params.iter().copied().map(Param::new).collect();
        self
    }

    /// The same signature returning these, each in its C-level form.
    #[must_use]
    pub fn with_returns(mut self, returns: &[Type]) -> Self {
        self.returns = returns.iter().copied().map(Param::new).collect();
        self
    }

    /// The same signature with one more parameter, travelling the way the ABI said.
    #[must_use]
    pub fn and_param(mut self, param: Param) -> Self {
        self.params.push(param);
        self
    }

    /// The same signature with one more return value, travelling the way the ABI said.
    #[must_use]
    pub fn and_return(mut self, param: Param) -> Self {
        self.returns.push(param);
        self
    }

    /// The types it takes, without what the ABI asks of them.
    pub fn param_types(&self) -> impl Iterator<Item = Type> + use<'_> {
        self.params.iter().map(|param| param.ty)
    }

    /// The types it returns.
    pub fn return_types(&self) -> impl Iterator<Item = Type> + use<'_> {
        self.returns.iter().map(|param| param.ty)
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
        assert_eq!(sig.param_types().collect::<Vec<_>>(), [Type::int(32), Type::PTR]);
        assert_eq!(sig.return_types().collect::<Vec<_>>(), [Type::int(32)]);
        assert!(sig.variadic);
        assert_eq!(Signature::new(), Signature::default());
    }

    #[test]
    fn a_parameter_says_how_it_travels_and_not_only_what_it_is() {
        let object = Abi::ByVal { size: 24, align: 8 };
        let sig = Signature::new()
            .and_param(Param::with_abi(Type::PTR, Abi::Sret { size: 32, align: 16 }))
            .and_param(Param::with_abi(Type::PTR, object))
            .and_param(Param::with_abi(Type::int(8), Abi::Zext));
        // The types alone say `ptr, ptr, i8`, which is three of the calls in any C program and
        // none of them the same call.
        assert_eq!(sig.param_types().collect::<Vec<_>>(), [Type::PTR, Type::PTR, Type::int(8)]);
        assert_eq!(sig.params[1].abi.object(), Some((24, 8)));
        assert!(sig.params[0].abi.indirect() && !sig.params[2].abi.indirect());
        assert_eq!(Param::new(Type::PTR).abi, Abi::Plain);
        assert_eq!(Abi::Plain.object(), None);
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
