//! The instruction set.
//!
//! Design: `spec/08-ir.md` section 8.3.
//!
//! The set is small enough to enumerate and it is closed. Adding an opcode is a spec change,
//! because the verifier, the printer, the parser, the rewrite rules and the lowering all have
//! to learn it, and an opcode that only half of them know about is a silent miscompilation
//! waiting for the right input.
//!
//! Two things are deliberately absent. There is no `getelementptr`: pointer arithmetic is
//! [`Opcode::PtrAdd`] over a byte offset the frontend computed, because C never needs the
//! multi-index form and its absence removes a well known source of complexity. And there is no
//! `phi`: values arriving at a block are the block's parameters, passed by the branch, so
//! there is no operand list positionally tied to a predecessor list kept somewhere else.

use std::fmt;

/// One instruction of the IR.
///
/// The names are the textual form exactly, so [`Opcode::name`] and [`Opcode::from_name`] are
/// what the printer and the parser use, and neither carries a table of its own that could
/// drift from this one.
///
/// The enum is not `non_exhaustive`, deliberately. The set is closed, so a pass that matches
/// on every opcode should stop compiling when one is added rather than fall into a wildcard
/// arm that quietly does the wrong thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Opcode {
    // Constants. A constant is an instruction rather than an operand kind, so that every
    // operand is a value and every value has one definition, which is what makes the
    // dominance check in the verifier a single rule rather than a rule with exceptions.
    /// An integer constant, `iconst.i32 7`.
    IConst,
    /// A floating point constant, `fconst.f64 0x1.8p+1`.
    FConst,
    /// A vector constant with every lane the same, `splat.i8x16 0`.
    Splat,
    /// The address of a global or a function, `global_addr @counter`.
    GlobalAddr,
    /// The address of a block in this function, `block_addr block3`.
    ///
    /// The one instruction that names a block without being a branch, which is what GNU's
    /// `&&label` is. Where it goes is [`Opcode::IndirectBr`], and the two are only useful
    /// together: an address on its own is a number that nothing can do anything with.
    BlockAddr,

    // Arithmetic.
    /// Integer addition.
    Add,
    /// Integer subtraction.
    Sub,
    /// Integer multiplication.
    Mul,
    /// Signed division.
    SDiv,
    /// Unsigned division.
    UDiv,
    /// Signed remainder, with the sign of the dividend.
    SRem,
    /// Unsigned remainder.
    URem,
    /// Bitwise and.
    And,
    /// Bitwise or.
    Or,
    /// Bitwise exclusive or.
    Xor,
    /// Shift left.
    Shl,
    /// Logical shift right, shifting in zeroes.
    LShr,
    /// Arithmetic shift right, shifting in the sign bit.
    AShr,
    /// Floating point addition.
    FAdd,
    /// Floating point subtraction.
    FSub,
    /// Floating point multiplication.
    FMul,
    /// Floating point division.
    FDiv,
    /// Floating point remainder.
    FRem,
    /// Floating point negation, which flips the sign bit and is not `0 - x`.
    FNeg,
    /// Fused multiply-add, rounded once.
    Fma,

    // Comparison.
    /// Integer comparison, producing `i1` or a vector of `i1`.
    ICmp,
    /// Floating point comparison, producing `i1` or a vector of `i1`.
    FCmp,

    // Selection.
    /// One of two values, chosen by a bit. `select c, a, b` is `a` when `c` is one.
    ///
    /// This is what control flow becomes when it stops being control flow.
    /// `spec/optimizer/22-phiopt-and-if-conversion.md` section 22.2 makes it the lowering target
    /// for a diamond whose two arms compute a value, and the reason it is an opcode rather than a
    /// pattern is that it is the form the rule set is written against: `select(c, a, a) -> a` and
    /// `select(c, 1, 0) -> zext(c)` are ordinary rules once the shape has a name.
    ///
    /// Both arms are evaluated, which is the whole point and also the whole danger. Whatever
    /// produces one of these owes the argument that evaluating the arm that is not chosen is
    /// harmless, and section 22.6 is the list of ways that argument goes wrong.
    Select,

    // Conversion.
    /// Narrows an integer, discarding the high bits.
    Trunc,
    /// Widens an integer, copying the sign bit.
    SExt,
    /// Widens an integer, filling with zeroes.
    ZExt,
    /// Narrows a floating point value.
    FPTrunc,
    /// Widens a floating point value.
    FPExt,
    /// Floating point to signed integer.
    FPToSI,
    /// Floating point to unsigned integer.
    FPToUI,
    /// Signed integer to floating point.
    SIToFP,
    /// Unsigned integer to floating point.
    UIToFP,
    /// An address to an integer of the same width.
    PtrToInt,
    /// An integer to an address.
    IntToPtr,
    /// A reinterpretation of the same bits at the same width.
    Bitcast,

    // Memory.
    /// Memory as the function found it, which is where a memory SSA chain starts.
    ///
    /// It produces one `mem` and takes nothing, and it belongs at the top of the entry block.
    /// GCC calls the same thing the default definition of `.MEM` and LLVM calls it
    /// `liveOnEntry`. It exists as an instruction rather than as a parameter of the entry block
    /// because the entry block's parameters are the function's parameters and the verifier
    /// checks them against the signature, and memory is not an argument anybody passed.
    MemEntry,
    /// A stack slot. In the entry block, or marked dynamic for a variable length array.
    Alloca,
    /// A read.
    Load,
    /// A write, producing no value.
    Store,
    /// Address arithmetic: an address and a byte offset.
    PtrAdd,
    /// A copy of a known size between addresses that do not overlap.
    Memcpy,
    /// A copy of a known size between addresses that may overlap.
    Memmove,
    /// A fill of a known size with one byte.
    Memset,
    /// An atomic read.
    AtomicLoad,
    /// An atomic write.
    AtomicStore,
    /// An atomic read-modify-write, carrying which operation in [`RmwOp`](crate::RmwOp).
    AtomicRmw,
    /// An atomic compare and exchange, producing the old value and whether it succeeded.
    Cmpxchg,
    /// A memory barrier.
    Fence,

    // Memory safety. Design: `spec/safe-memory/06-instrumentation.md` section 6.2.2. None of
    // these is emitted unless `-fsafety` asked for it, and a function compiled without it
    // contains not one of them.
    /// The capability of a pointer value, taken from the pointer's provenance.
    CapOf,
    /// The capability in the auxiliary slot beside a stored pointer, read back.
    ///
    /// A pointer written to memory and read again has to bring its capability with it, and where
    /// the capability lives is document 05's question rather than this one's. What this says is
    /// that a capability comes back from an address, which is enough for every pass above.
    CapLoad,
    /// The other half of [`Opcode::CapLoad`], writing one into the slot beside a pointer.
    CapStore,
    /// The capability that permits nothing, which is what a null pointer has.
    CapNull,
    /// A capability narrowed to a sub-object of what it covered.
    ///
    /// Only under `-fsafety-subobject`. Narrowing is what catches an overflow from one member of
    /// a struct into the next, and it is separate because C code that walks off the end of a
    /// member on purpose exists and a project has to be able to say so.
    CapNarrow,
    /// The capability for an address that arrived from outside, recovered from the planes.
    CapRecover,
    /// An access is within its capability's bounds, aligned, and permitted.
    ///
    /// The size and the alignment are the access's, and they are in the memory payload rather
    /// than in operands because they are what the front end knew and not what the program
    /// computed.
    CheckBounds,
    /// The capability's provenance is still live.
    CheckLive,
    /// The access agrees with the type plane, which is the effective type rule of C 6.5.
    CheckType,
    /// The bytes the access reads have been written.
    CheckInit,
    /// A pointer derived from another stays inside the capability the first one had.
    ///
    /// Three operands, because the answer is about the new pointer and the question is about
    /// the old one's capability.
    CheckDeriv,
    /// The metadata this access is about to consult has not been changed under it.
    CheckRace,
    /// A storage instance begins here, over a range, with a class.
    ///
    /// Judgement J4. This is the `alloca` for an automatic instance and the allocator's report
    /// for an allocated one, and the range is a pointer and a length in registers rather than a
    /// payload, because the length of a variable length array is not known when the instruction
    /// is written down.
    MetaBegin,
    /// A storage instance ends here, which is judgement J5.
    ///
    /// Every capability for it fails from this point on and keeps failing after the address is
    /// handed out again, which is what makes the check a use after free check rather than a use
    /// after reallocation one.
    MetaEnd,
    /// The effective type of a range is now this one.
    MetaType,
    /// The bytes of a range are now initialized.
    MetaInit,
    /// A range leaves the monitor's authority, or comes back, which is judgement J7.
    MetaTransfer,
    /// A declared exemption starts here, with the reason it was declared.
    ///
    /// Not an optimization hint. Everything between this and its `safe_region_end` is code the
    /// monitor is told not to judge, so the reason it carries is a trust set entry, and
    /// `spec/safe-memory/10-boundaries.md` section 10.2 counts them per build precisely so that
    /// a reviewer can read what a binary's guarantee rests on.
    SafeRegionBegin,
    /// The end of the region the last `safe_region_begin` opened.
    SafeRegionEnd,

    // Control. Every one of these is a terminator.
    /// An unconditional branch, `jump block1(%a, %b)`.
    Jump,
    /// A two-way branch on an `i1`.
    BrIf,
    /// A multi-way branch on an integer, with a default.
    Switch,
    /// A branch to an address, `indirect_br %0, block1, block2`.
    ///
    /// The targets are every block control can arrive at, which is what makes the edges of a
    /// computed `goto` ordinary edges: nothing else in the compiler has to know that the
    /// address decides which one it is. A target that is not listed is a branch that does not
    /// happen, so a frontend that leaves one out has made a promise on the program's behalf.
    IndirectBr,
    /// A return, with the values the signature says.
    Return,
    /// A place control cannot reach, which the frontend emits after a `noreturn` call.
    Unreachable,

    // Calls.
    /// A call to a named function.
    Call,
    /// A call through an address, carrying the signature it is called with.
    CallIndirect,
    /// A call in tail position that reuses the frame, which is a terminator.
    TailCall,

    // Intrinsics, which is the closed part. The open part is `TargetIntrinsic`.
    /// Count leading zeroes.
    Ctlz,
    /// Count trailing zeroes.
    Cttz,
    /// Count set bits.
    Ctpop,
    /// Reverse the bytes.
    Bswap,
    /// Reverse the bits.
    Bitreverse,
    /// Signed addition, producing the result and whether it overflowed.
    SAddOverflow,
    /// Unsigned addition, producing the result and whether it overflowed.
    UAddOverflow,
    /// Signed subtraction, producing the result and whether it overflowed.
    SSubOverflow,
    /// Unsigned subtraction, producing the result and whether it overflowed.
    USubOverflow,
    /// Signed multiplication, producing the result and whether it overflowed.
    SMulOverflow,
    /// Unsigned multiplication, producing the result and whether it overflowed.
    UMulOverflow,
    /// `__builtin_expect`, which is the value with a hint attached.
    Expect,
    /// `__builtin_unreachable` as a hint on a path, distinct from the terminator.
    UnreachableHint,
    /// `__builtin_prefetch`.
    Prefetch,
    /// `__builtin_frame_address`.
    FrameAddress,
    /// `__builtin_return_address`.
    ReturnAddress,
    /// The start of a variable argument list.
    VaStart,
    /// One argument off a variable argument list, which moves the list on as it reads it. Two
    /// of these on one list are two arguments and never one argument read twice, so whatever
    /// decides which instructions may be folded together has to leave these alone.
    VaArg,
    /// One argument off a variable argument list, when that argument is an object rather than a
    /// value, which is what a `struct` or a `union` read out of one is.
    ///
    /// It answers the address of the object rather than the object, because an aggregate is not
    /// a value and there is nothing for one result to be. Where the object arrives in registers
    /// there is no address until something makes one, so what this asks of a target is a place
    /// to put the registers and the address of that place, which is the copy every psABI's own
    /// description of the algorithm makes. It moves the list on for the reason [`Opcode::VaArg`]
    /// does.
    VaObject,
    /// The end of a variable argument list.
    VaEnd,
    /// A copy of a variable argument list.
    VaCopy,
    /// The stack pointer, saved before a variable length array.
    StackSave,
    /// The stack pointer, restored after one.
    StackRestore,
    /// The marker a `setjmp` leaves, which pins everything live across it.
    SetjmpMarker,
    /// The marker a `longjmp` leaves.
    LongjmpMarker,
    /// A target-specific intrinsic, named rather than enumerated, for the vector builtins.
    TargetIntrinsic,

    /// Inline assembly. A terminator when it has labels, which is `asm goto`.
    InlineAsm,
}

impl Opcode {
    /// The textual form, which is also what the parser reads.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::IConst => "iconst",
            Self::FConst => "fconst",
            Self::Splat => "splat",
            Self::GlobalAddr => "global_addr",
            Self::BlockAddr => "block_addr",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::SDiv => "sdiv",
            Self::UDiv => "udiv",
            Self::SRem => "srem",
            Self::URem => "urem",
            Self::And => "and",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::Shl => "shl",
            Self::LShr => "lshr",
            Self::AShr => "ashr",
            Self::FAdd => "fadd",
            Self::FSub => "fsub",
            Self::FMul => "fmul",
            Self::FDiv => "fdiv",
            Self::FRem => "frem",
            Self::FNeg => "fneg",
            Self::Fma => "fma",
            Self::ICmp => "icmp",
            Self::FCmp => "fcmp",
            Self::Select => "select",
            Self::Trunc => "trunc",
            Self::SExt => "sext",
            Self::ZExt => "zext",
            Self::FPTrunc => "fptrunc",
            Self::FPExt => "fpext",
            Self::FPToSI => "fptosi",
            Self::FPToUI => "fptoui",
            Self::SIToFP => "sitofp",
            Self::UIToFP => "uitofp",
            Self::PtrToInt => "ptrtoint",
            Self::IntToPtr => "inttoptr",
            Self::Bitcast => "bitcast",
            Self::MemEntry => "mem_entry",
            Self::Alloca => "alloca",
            Self::Load => "load",
            Self::Store => "store",
            Self::PtrAdd => "ptr_add",
            Self::Memcpy => "memcpy",
            Self::Memmove => "memmove",
            Self::Memset => "memset",
            Self::AtomicLoad => "atomic_load",
            Self::AtomicStore => "atomic_store",
            Self::AtomicRmw => "atomic_rmw",
            Self::Cmpxchg => "cmpxchg",
            Self::Fence => "fence",
            Self::CapOf => "cap_of",
            Self::CapLoad => "cap_load",
            Self::CapStore => "cap_store",
            Self::CapNull => "cap_null",
            Self::CapNarrow => "cap_narrow",
            Self::CapRecover => "cap_recover",
            Self::CheckBounds => "check_bounds",
            Self::CheckLive => "check_live",
            Self::CheckType => "check_type",
            Self::CheckInit => "check_init",
            Self::CheckDeriv => "check_deriv",
            Self::CheckRace => "check_race",
            Self::MetaBegin => "meta_begin",
            Self::MetaEnd => "meta_end",
            Self::MetaType => "meta_type",
            Self::MetaInit => "meta_init",
            Self::MetaTransfer => "meta_transfer",
            Self::SafeRegionBegin => "safe_region_begin",
            Self::SafeRegionEnd => "safe_region_end",
            Self::Jump => "jump",
            Self::BrIf => "br_if",
            Self::Switch => "switch",
            Self::IndirectBr => "indirect_br",
            Self::Return => "return",
            Self::Unreachable => "unreachable",
            Self::Call => "call",
            Self::CallIndirect => "call_indirect",
            Self::TailCall => "tail_call",
            Self::Ctlz => "ctlz",
            Self::Cttz => "cttz",
            Self::Ctpop => "ctpop",
            Self::Bswap => "bswap",
            Self::Bitreverse => "bitreverse",
            Self::SAddOverflow => "sadd_overflow",
            Self::UAddOverflow => "uadd_overflow",
            Self::SSubOverflow => "ssub_overflow",
            Self::USubOverflow => "usub_overflow",
            Self::SMulOverflow => "smul_overflow",
            Self::UMulOverflow => "umul_overflow",
            Self::Expect => "expect",
            Self::UnreachableHint => "unreachable_hint",
            Self::Prefetch => "prefetch",
            Self::FrameAddress => "frame_address",
            Self::ReturnAddress => "return_address",
            Self::VaStart => "va_start",
            Self::VaArg => "va_arg",
            Self::VaObject => "va_object",
            Self::VaEnd => "va_end",
            Self::VaCopy => "va_copy",
            Self::StackSave => "stacksave",
            Self::StackRestore => "stackrestore",
            Self::SetjmpMarker => "setjmp_marker",
            Self::LongjmpMarker => "longjmp_marker",
            Self::TargetIntrinsic => "target_intrinsic",
            Self::InlineAsm => "inline_asm",
        }
    }

    /// Every opcode, in the order they are declared.
    ///
    /// The parser walks this rather than holding a second table, because a second table is a
    /// table that can disagree with the first one.
    pub fn all() -> impl Iterator<Item = Self> {
        ALL.iter().copied()
    }

    /// The opcode with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        ALL.iter().copied().find(|op| op.name() == name)
    }

    /// Whether this ends a block.
    ///
    /// [`Opcode::InlineAsm`] is not here and is the one instruction whose answer depends on
    /// the instruction rather than on the opcode: `asm goto` has successors and everything
    /// else does not. Ask the instruction, not the opcode.
    #[must_use]
    pub const fn is_terminator(self) -> bool {
        matches!(
            self,
            Self::Jump
                | Self::BrIf
                | Self::Switch
                | Self::IndirectBr
                | Self::Return
                | Self::Unreachable
                | Self::TailCall
        )
    }

    /// Whether the operands can be swapped without changing the result.
    ///
    /// The floating point cases are commutative even under the strictest rounding, because
    /// swapping the operands of an addition does not change which of them is a NaN, and the
    /// sign of a NaN result is not something we promise anything about either way.
    #[must_use]
    pub const fn is_commutative(self) -> bool {
        matches!(
            self,
            Self::Add
                | Self::Mul
                | Self::And
                | Self::Or
                | Self::Xor
                | Self::FAdd
                | Self::FMul
                | Self::SAddOverflow
                | Self::UAddOverflow
                | Self::SMulOverflow
                | Self::UMulOverflow
        )
    }

    /// Whether this reads or writes memory, or has an effect the optimizer has to preserve.
    ///
    /// An instruction that answers no can be deleted when nothing uses its result, moved
    /// across a call, and merged with another one computing the same thing. Everything else
    /// has to be argued about individually, so the conservative answer is the true one here
    /// and the list of exceptions is the part that is checked.
    #[must_use]
    pub const fn has_effects(self) -> bool {
        !matches!(
            self,
            Self::IConst
                | Self::FConst
                | Self::Splat
                | Self::GlobalAddr
                | Self::BlockAddr
                | Self::Add
                | Self::Sub
                | Self::Mul
                | Self::SDiv
                | Self::UDiv
                | Self::SRem
                | Self::URem
                | Self::And
                | Self::Or
                | Self::Xor
                | Self::Shl
                | Self::LShr
                | Self::AShr
                | Self::FAdd
                | Self::FSub
                | Self::FMul
                | Self::FDiv
                | Self::FRem
                | Self::FNeg
                | Self::Fma
                | Self::ICmp
                | Self::FCmp
                | Self::Select
                | Self::Trunc
                | Self::SExt
                | Self::ZExt
                | Self::FPTrunc
                | Self::FPExt
                | Self::FPToSI
                | Self::FPToUI
                | Self::SIToFP
                | Self::UIToFP
                | Self::PtrToInt
                | Self::IntToPtr
                | Self::Bitcast
                | Self::PtrAdd
                | Self::Ctlz
                | Self::Cttz
                | Self::Ctpop
                | Self::Bswap
                | Self::Bitreverse
                | Self::SAddOverflow
                | Self::UAddOverflow
                | Self::SSubOverflow
                | Self::USubOverflow
                | Self::SMulOverflow
                | Self::UMulOverflow
                | Self::Expect
                | Self::FrameAddress
                | Self::ReturnAddress
                | Self::MemEntry
                // Three of the capability instructions are arithmetic on a pointer's
                // provenance and touch nothing. The other three do: `cap_load` and
                // `cap_store` are an access, and `cap_recover` reads the planes.
                | Self::CapOf
                | Self::CapNull
                | Self::CapNarrow
        )
    }

    /// Whether an instruction with this opcode touches memory.
    ///
    /// This is what decides whether it takes a memory operand once memory SSA is built, per
    /// document 09 of `spec/optimizer`. It is written as the exceptions to touching memory
    /// rather than as a list of what does, for the reason document 08.6 gives about the escape
    /// analysis: an opcode added later has to end up on the conservative side by default, and a
    /// list of what touches memory would silently leave a new one out.
    ///
    /// `mem_entry` answers no. It produces memory rather than touching it, which is the whole
    /// of what it is for.
    #[must_use]
    pub const fn touches_memory(self) -> bool {
        if !self.has_effects() {
            return false;
        }
        !matches!(
            self,
            // Fresh storage nothing could have been reading, and the pointer that names it.
            Self::Alloca
                // The stack pointer, which is a register and not memory. Putting it back is a
                // different matter and is below, because it takes storage away.
                | Self::StackSave
                // Control, which goes somewhere rather than touching anything. A tail call is
                // not here, because it is a call.
                | Self::Jump
                | Self::BrIf
                | Self::Switch
                | Self::IndirectBr
                | Self::Return
                | Self::Unreachable
                | Self::UnreachableHint
        )
    }

    /// Whether an instruction with this opcode writes memory, and so produces a new version of
    /// it rather than only reading the version it was given.
    ///
    /// Everything that touches memory writes it except the ones that plainly do not. A `fence`
    /// writes nothing and is still a write here, because document 09.5 says an atomic or a
    /// barrier is a definition nothing walks past, and giving it one is how that is expressed
    /// in a representation whose only ordering is the memory chain.
    ///
    /// The checks read the planes and change nothing, which
    /// `spec/safe-memory/06-instrumentation.md` section 6.2.4 states as the word `readonly`. A
    /// check that trapped is a program that stopped and there is no version of memory after it
    /// for anything to observe, so the trap costs nothing here. What it does cost is that a
    /// check may not be moved across a plane write, and that is the memory chain saying so
    /// rather than this.
    #[must_use]
    pub const fn writes_memory(self) -> bool {
        self.touches_memory()
            && !matches!(
                self,
                Self::Load
                    | Self::AtomicLoad
                    | Self::Prefetch
                    | Self::CapLoad
                    | Self::CapRecover
                    | Self::CheckBounds
                    | Self::CheckLive
                    | Self::CheckType
                    | Self::CheckInit
                    | Self::CheckDeriv
                    | Self::CheckRace
            )
    }

    /// How many values this produces, for the opcodes where the count is fixed.
    ///
    /// `None` means the count comes from somewhere else: a call takes it from its signature,
    /// and inline assembly takes it from its output constraints. A tail call is not one of
    /// them, because whatever it returns goes straight out of the function and there is no
    /// instruction after it to use anything.
    #[must_use]
    pub const fn results(self) -> Option<u8> {
        match self {
            Self::Call | Self::CallIndirect | Self::InlineAsm => None,
            Self::Cmpxchg
            | Self::SAddOverflow
            | Self::UAddOverflow
            | Self::SSubOverflow
            | Self::USubOverflow
            | Self::SMulOverflow
            | Self::UMulOverflow => Some(2),
            Self::Store
            | Self::Memcpy
            | Self::Memmove
            | Self::Memset
            | Self::AtomicStore
            | Self::Fence
            | Self::Prefetch
            | Self::VaStart
            | Self::VaEnd
            | Self::VaCopy
            | Self::StackRestore
            | Self::UnreachableHint
            | Self::SetjmpMarker
            | Self::LongjmpMarker
            | Self::CapStore
            | Self::CheckBounds
            | Self::CheckLive
            | Self::CheckType
            | Self::CheckInit
            | Self::CheckDeriv
            | Self::CheckRace
            | Self::MetaBegin
            | Self::MetaEnd
            | Self::MetaType
            | Self::MetaInit
            | Self::MetaTransfer
            | Self::SafeRegionBegin
            | Self::SafeRegionEnd => Some(0),
            _ if self.is_terminator() => Some(0),
            _ => Some(1),
        }
    }

    /// Whether an instruction with this opcode produces a capability.
    ///
    /// Five of the six `cap` instructions, `cap_store` being the one that consumes one instead.
    /// The reason this is a question about the opcode rather than about the
    /// result type is that the verifier asks it the other way round: it walks the results looking
    /// for a `cap` and needs to know whether the instruction under it was entitled to make one.
    #[must_use]
    pub const fn makes_capability(self) -> bool {
        matches!(
            self,
            Self::CapOf | Self::CapLoad | Self::CapNull | Self::CapNarrow | Self::CapRecover
        )
    }

    /// Which payload an instruction with this opcode carries.
    ///
    /// The printer reads the payload it finds and does not need this. The parser has only the
    /// opcode when it reaches the operands, so this is where the two of them agree on what
    /// comes after them. An instruction carrying a payload of some other kind prints as text
    /// the parser cannot read back, which is why the verifier checks it against
    /// [`Extra::kind`](crate::Extra::kind) rather than leaving it to be found later.
    #[must_use]
    pub const fn extra_kind(self) -> ExtraKind {
        match self {
            Self::IConst | Self::FConst | Self::Splat => ExtraKind::Imm,
            Self::GlobalAddr | Self::TargetIntrinsic => ExtraKind::Symbol,
            Self::ICmp => ExtraKind::IntPred,
            Self::FCmp => ExtraKind::FloatPred,
            Self::Alloca
            | Self::Load
            | Self::Store
            | Self::Memcpy
            | Self::Memmove
            | Self::Memset
            | Self::AtomicLoad
            | Self::AtomicStore
            | Self::Cmpxchg
            // Three of the checks are about a run of bytes and the payload is where the size
            // of that run is, along with the alignment `check_bounds` wants and the aliasing
            // node `check_type` compares against. The other three ask a question about a
            // pointer and not about a range, so they carry nothing.
            | Self::CheckBounds
            | Self::CheckType
            | Self::CheckInit => ExtraKind::Mem,
            // The plane writes. What each one needs beyond the range is different, and the range
            // itself is operands, since the length of a variable length array is a value.
            Self::MetaBegin => ExtraKind::Class,
            Self::MetaTransfer => ExtraKind::Owner,
            Self::MetaType => ExtraKind::Node,
            Self::SafeRegionBegin => ExtraKind::Reason,
            Self::VaObject => ExtraKind::VaObject,
            Self::AtomicRmw => ExtraKind::Rmw,
            Self::Fence => ExtraKind::Order,
            Self::Jump | Self::BrIf | Self::BlockAddr | Self::IndirectBr => ExtraKind::Targets,
            Self::Switch => ExtraKind::Switch,
            Self::Call | Self::CallIndirect | Self::TailCall => ExtraKind::Call,
            Self::InlineAsm => ExtraKind::Asm,
            _ => ExtraKind::None,
        }
    }
}

/// Which of [`Extra`](crate::Extra)'s shapes an instruction carries.
///
/// The same list of names, without any of the payloads, so that a question about an opcode can
/// be answered without an instruction to look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExtraKind {
    /// Nothing.
    None,
    /// A constant.
    Imm,
    /// A name.
    Symbol,
    /// An integer comparison predicate.
    IntPred,
    /// A floating point comparison predicate.
    FloatPred,
    /// An access.
    Mem,
    /// An atomic read-modify-write.
    Rmw,
    /// A barrier's ordering.
    Order,
    /// Branch targets.
    Targets,
    /// A call.
    Call,
    /// A `switch`.
    Switch,
    /// Inline assembly.
    Asm,
    /// An object read off a variable argument list.
    VaObject,
    /// What kind of storage an instance is.
    Class,
    /// Who a range of memory went to.
    Owner,
    /// A metadata node.
    Node,
    /// Why a declared exemption is there.
    Reason,
}

impl ExtraKind {
    /// What it is, in words, for a message that names two of them and has to read as English.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "nothing",
            Self::Imm => "a constant",
            Self::Symbol => "a name",
            Self::IntPred => "an integer comparison",
            Self::FloatPred => "a floating point comparison",
            Self::Mem => "an access",
            Self::Rmw => "a read-modify-write",
            Self::Order => "an ordering",
            Self::Targets => "branch targets",
            Self::Call => "a call",
            Self::Switch => "a switch",
            Self::Asm => "inline assembly",
            Self::VaObject => "an object off a variable argument list",
            Self::Class => "a storage class",
            Self::Owner => "an owner",
            Self::Node => "a metadata node",
            Self::Reason => "a reason",
        }
    }
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Every opcode, which is what [`Opcode::all`] hands out.
///
/// This is written out rather than derived, and the test below is what keeps it complete: it
/// checks the count against [`Opcode::InlineAsm`], the last variant, so a new opcode that is
/// not added here fails the build rather than going quietly missing from the parser.
static ALL: &[Opcode] = &[
    Opcode::IConst,
    Opcode::FConst,
    Opcode::Splat,
    Opcode::GlobalAddr,
    Opcode::BlockAddr,
    Opcode::Add,
    Opcode::Sub,
    Opcode::Mul,
    Opcode::SDiv,
    Opcode::UDiv,
    Opcode::SRem,
    Opcode::URem,
    Opcode::And,
    Opcode::Or,
    Opcode::Xor,
    Opcode::Shl,
    Opcode::LShr,
    Opcode::AShr,
    Opcode::FAdd,
    Opcode::FSub,
    Opcode::FMul,
    Opcode::FDiv,
    Opcode::FRem,
    Opcode::FNeg,
    Opcode::Fma,
    Opcode::ICmp,
    Opcode::FCmp,
    Opcode::Select,
    Opcode::Trunc,
    Opcode::SExt,
    Opcode::ZExt,
    Opcode::FPTrunc,
    Opcode::FPExt,
    Opcode::FPToSI,
    Opcode::FPToUI,
    Opcode::SIToFP,
    Opcode::UIToFP,
    Opcode::PtrToInt,
    Opcode::IntToPtr,
    Opcode::Bitcast,
    Opcode::MemEntry,
    Opcode::Alloca,
    Opcode::Load,
    Opcode::Store,
    Opcode::PtrAdd,
    Opcode::Memcpy,
    Opcode::Memmove,
    Opcode::Memset,
    Opcode::AtomicLoad,
    Opcode::AtomicStore,
    Opcode::AtomicRmw,
    Opcode::Cmpxchg,
    Opcode::Fence,
    Opcode::CapOf,
    Opcode::CapLoad,
    Opcode::CapStore,
    Opcode::CapNull,
    Opcode::CapNarrow,
    Opcode::CapRecover,
    Opcode::CheckBounds,
    Opcode::CheckLive,
    Opcode::CheckType,
    Opcode::CheckInit,
    Opcode::CheckDeriv,
    Opcode::CheckRace,
    Opcode::MetaBegin,
    Opcode::MetaEnd,
    Opcode::MetaType,
    Opcode::MetaInit,
    Opcode::MetaTransfer,
    Opcode::SafeRegionBegin,
    Opcode::SafeRegionEnd,
    Opcode::Jump,
    Opcode::BrIf,
    Opcode::Switch,
    Opcode::IndirectBr,
    Opcode::Return,
    Opcode::Unreachable,
    Opcode::Call,
    Opcode::CallIndirect,
    Opcode::TailCall,
    Opcode::Ctlz,
    Opcode::Cttz,
    Opcode::Ctpop,
    Opcode::Bswap,
    Opcode::Bitreverse,
    Opcode::SAddOverflow,
    Opcode::UAddOverflow,
    Opcode::SSubOverflow,
    Opcode::USubOverflow,
    Opcode::SMulOverflow,
    Opcode::UMulOverflow,
    Opcode::Expect,
    Opcode::UnreachableHint,
    Opcode::Prefetch,
    Opcode::FrameAddress,
    Opcode::ReturnAddress,
    Opcode::VaStart,
    Opcode::VaArg,
    Opcode::VaObject,
    Opcode::VaEnd,
    Opcode::VaCopy,
    Opcode::StackSave,
    Opcode::StackRestore,
    Opcode::SetjmpMarker,
    Opcode::LongjmpMarker,
    Opcode::TargetIntrinsic,
    Opcode::InlineAsm,
];

/// The ten integer comparisons.
///
/// Signedness is on the predicate rather than on the type, for the same reason it is on
/// `sdiv` and `udiv`: the type space is halved and the operation says what it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntPred {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Signed less than.
    Slt,
    /// Signed less than or equal.
    Sle,
    /// Signed greater than.
    Sgt,
    /// Signed greater than or equal.
    Sge,
    /// Unsigned less than.
    Ult,
    /// Unsigned less than or equal.
    Ule,
    /// Unsigned greater than.
    Ugt,
    /// Unsigned greater than or equal.
    Uge,
}

impl IntPred {
    /// The textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Slt => "slt",
            Self::Sle => "sle",
            Self::Sgt => "sgt",
            Self::Sge => "sge",
            Self::Ult => "ult",
            Self::Ule => "ule",
            Self::Ugt => "ugt",
            Self::Uge => "uge",
        }
    }

    /// The predicate with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|pred| pred.name() == name)
    }

    /// Every predicate.
    pub fn all() -> impl Iterator<Item = Self> {
        [
            Self::Eq,
            Self::Ne,
            Self::Slt,
            Self::Sle,
            Self::Sgt,
            Self::Sge,
            Self::Ult,
            Self::Ule,
            Self::Ugt,
            Self::Uge,
        ]
        .into_iter()
    }

    /// The predicate that holds exactly when this one does not.
    #[must_use]
    pub const fn inverse(self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ne => Self::Eq,
            Self::Slt => Self::Sge,
            Self::Sge => Self::Slt,
            Self::Sle => Self::Sgt,
            Self::Sgt => Self::Sle,
            Self::Ult => Self::Uge,
            Self::Uge => Self::Ult,
            Self::Ule => Self::Ugt,
            Self::Ugt => Self::Ule,
        }
    }

    /// The predicate that holds when the operands are given the other way round.
    #[must_use]
    pub const fn swapped(self) -> Self {
        match self {
            Self::Eq => Self::Eq,
            Self::Ne => Self::Ne,
            Self::Slt => Self::Sgt,
            Self::Sgt => Self::Slt,
            Self::Sle => Self::Sge,
            Self::Sge => Self::Sle,
            Self::Ult => Self::Ugt,
            Self::Ugt => Self::Ult,
            Self::Ule => Self::Uge,
            Self::Uge => Self::Ule,
        }
    }

    /// Whether this reads its operands as signed. Equality reads them as neither.
    #[must_use]
    pub const fn is_signed(self) -> bool {
        matches!(self, Self::Slt | Self::Sle | Self::Sgt | Self::Sge)
    }
}

impl fmt::Display for IntPred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The floating point comparisons, ordered and unordered.
///
/// An ordered predicate is false if either operand is a NaN, and an unordered one is true. C's
/// `<` is `olt` and C's `!=` is `une`, which is the whole of why both families are here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatPred {
    /// Always false.
    False,
    /// Ordered and equal.
    Oeq,
    /// Ordered and greater than.
    Ogt,
    /// Ordered and greater than or equal.
    Oge,
    /// Ordered and less than.
    Olt,
    /// Ordered and less than or equal.
    Ole,
    /// Ordered and not equal.
    One,
    /// Ordered, which is to say neither operand is a NaN.
    Ord,
    /// Unordered, which is to say one of them is.
    Uno,
    /// Unordered or equal.
    Ueq,
    /// Unordered or greater than.
    Ugt,
    /// Unordered or greater than or equal.
    Uge,
    /// Unordered or less than.
    Ult,
    /// Unordered or less than or equal.
    Ule,
    /// Unordered or not equal.
    Une,
    /// Always true.
    True,
}

impl FloatPred {
    /// The textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::False => "false",
            Self::Oeq => "oeq",
            Self::Ogt => "ogt",
            Self::Oge => "oge",
            Self::Olt => "olt",
            Self::Ole => "ole",
            Self::One => "one",
            Self::Ord => "ord",
            Self::Uno => "uno",
            Self::Ueq => "ueq",
            Self::Ugt => "ugt",
            Self::Uge => "uge",
            Self::Ult => "ult",
            Self::Ule => "ule",
            Self::Une => "une",
            Self::True => "true",
        }
    }

    /// The predicate with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|pred| pred.name() == name)
    }

    /// Every predicate.
    pub fn all() -> impl Iterator<Item = Self> {
        [
            Self::False,
            Self::Oeq,
            Self::Ogt,
            Self::Oge,
            Self::Olt,
            Self::Ole,
            Self::One,
            Self::Ord,
            Self::Uno,
            Self::Ueq,
            Self::Ugt,
            Self::Uge,
            Self::Ult,
            Self::Ule,
            Self::Une,
            Self::True,
        ]
        .into_iter()
    }

    /// The predicate that holds exactly when this one does not.
    #[must_use]
    pub const fn inverse(self) -> Self {
        match self {
            Self::False => Self::True,
            Self::Oeq => Self::Une,
            Self::Ogt => Self::Ule,
            Self::Oge => Self::Ult,
            Self::Olt => Self::Uge,
            Self::Ole => Self::Ugt,
            Self::One => Self::Ueq,
            Self::Ord => Self::Uno,
            Self::Uno => Self::Ord,
            Self::Ueq => Self::One,
            Self::Ugt => Self::Ole,
            Self::Uge => Self::Olt,
            Self::Ult => Self::Oge,
            Self::Ule => Self::Ogt,
            Self::Une => Self::Oeq,
            Self::True => Self::False,
        }
    }

    /// The predicate that holds when the operands are given the other way round.
    #[must_use]
    pub const fn swapped(self) -> Self {
        match self {
            Self::Ogt => Self::Olt,
            Self::Olt => Self::Ogt,
            Self::Oge => Self::Ole,
            Self::Ole => Self::Oge,
            Self::Ugt => Self::Ult,
            Self::Ult => Self::Ugt,
            Self::Uge => Self::Ule,
            Self::Ule => Self::Uge,
            same => same,
        }
    }

    /// Whether this is false when either operand is a NaN.
    ///
    /// [`FloatPred::False`] and [`FloatPred::True`] are neither ordered nor unordered, since
    /// they do not look at their operands at all, and both answer no here.
    #[must_use]
    pub const fn is_ordered(self) -> bool {
        matches!(
            self,
            Self::Oeq | Self::Ogt | Self::Oge | Self::Olt | Self::Ole | Self::One | Self::Ord
        )
    }
}

impl fmt::Display for FloatPred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_opcode_is_in_the_table() {
        // `InlineAsm` is the last variant, so its discriminant plus one is how many there are.
        // A new opcode declared after it moves this number, and a new opcode declared before
        // it and not added to `ALL` moves the length, so either mistake fails here.
        assert_eq!(ALL.len(), Opcode::InlineAsm as usize + 1);
        for (position, &op) in ALL.iter().enumerate() {
            assert_eq!(op as usize, position, "{op} is out of order in ALL");
        }
    }

    #[test]
    fn every_opcode_name_is_one_word_the_reader_can_take() {
        // The textual form keeps the dot for the type suffix and the flags, so an opcode with a
        // dot in it reads back as a shorter opcode with a suffix that is not a type. The safety
        // instructions are spelled `cap_of` and not `cap.of` for this reason, and the
        // specification says so at `spec/safe-memory/06-instrumentation.md` section 6.2.2.
        for opcode in Opcode::all() {
            let name = opcode.name();
            assert!(!name.is_empty(), "an opcode with no name");
            assert!(
                name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{name} is not one word"
            );
        }
    }

    #[test]
    fn every_opcode_has_its_own_name_and_finds_it_again() {
        let mut names: Vec<&str> = Opcode::all().map(Opcode::name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "two opcodes share a name");
        for op in Opcode::all() {
            assert_eq!(Opcode::from_name(op.name()), Some(op));
        }
        assert_eq!(Opcode::from_name("phi"), None);
        assert_eq!(Opcode::from_name("getelementptr"), None);
        assert_eq!(Opcode::from_name(""), None);
    }

    #[test]
    fn the_terminators_are_the_ones_control_leaves_by() {
        let terminators: Vec<&str> =
            Opcode::all().filter(|op| op.is_terminator()).map(Opcode::name).collect();
        assert_eq!(
            terminators,
            ["jump", "br_if", "switch", "indirect_br", "return", "unreachable", "tail_call"]
        );
    }

    #[test]
    fn a_terminator_produces_nothing() {
        for op in Opcode::all().filter(|op| op.is_terminator()) {
            assert_eq!(op.results(), Some(0), "{op}");
        }
    }

    #[test]
    fn the_pair_producing_opcodes_are_the_ones_with_a_flag_beside_the_value() {
        let pairs: Vec<&str> =
            Opcode::all().filter(|op| op.results() == Some(2)).map(Opcode::name).collect();
        assert_eq!(
            pairs,
            [
                "cmpxchg",
                "sadd_overflow",
                "uadd_overflow",
                "ssub_overflow",
                "usub_overflow",
                "smul_overflow",
                "umul_overflow"
            ]
        );
    }

    #[test]
    fn the_capability_instructions_are_the_ones_that_make_a_capability() {
        let makers: Vec<Opcode> = Opcode::all().filter(|op| op.makes_capability()).collect();
        assert_eq!(
            makers,
            vec![
                Opcode::CapOf,
                Opcode::CapLoad,
                Opcode::CapNull,
                Opcode::CapNarrow,
                Opcode::CapRecover
            ]
        );
        // The sixth is the one that writes a capability rather than making one, so it produces
        // nothing at all and is not on the list.
        assert!(!Opcode::CapStore.makes_capability());
        assert_eq!(Opcode::CapStore.results(), Some(0));
        for opcode in makers {
            assert_eq!(opcode.results(), Some(1), "{}", opcode.name());
        }
    }

    #[test]
    fn a_check_reads_the_planes_and_writes_nothing() {
        let checks = [
            Opcode::CheckBounds,
            Opcode::CheckLive,
            Opcode::CheckType,
            Opcode::CheckInit,
            Opcode::CheckDeriv,
            Opcode::CheckRace,
        ];
        for opcode in checks {
            let name = opcode.name();
            // It traps, so it stays where it was put and nothing deletes it for having no
            // result. It reads a plane, so it takes a memory operand. It writes nothing, so
            // the access after it reads the version the check was given.
            assert!(opcode.has_effects(), "{name}");
            assert!(opcode.touches_memory(), "{name}");
            assert!(!opcode.writes_memory(), "{name}");
            assert_eq!(opcode.results(), Some(0), "{name}");
        }
    }

    #[test]
    fn the_capability_instructions_that_touch_memory_are_the_three_that_have_to() {
        // `cap_load` and `cap_store` are an access to the slot beside a pointer and
        // `cap_recover` reads the planes. The other three are arithmetic on a provenance the
        // program already had, so the optimizer may treat them as it treats `ptr_add`.
        assert!(!Opcode::CapOf.has_effects());
        assert!(!Opcode::CapNull.has_effects());
        assert!(!Opcode::CapNarrow.has_effects());
        assert!(Opcode::CapLoad.touches_memory() && !Opcode::CapLoad.writes_memory());
        assert!(Opcode::CapRecover.touches_memory() && !Opcode::CapRecover.writes_memory());
        assert!(Opcode::CapStore.writes_memory());
    }

    #[test]
    fn memory_has_effects_and_arithmetic_does_not() {
        for op in [Opcode::Load, Opcode::Store, Opcode::Call, Opcode::Alloca, Opcode::Fence] {
            assert!(op.has_effects(), "{op}");
        }
        for op in [Opcode::Add, Opcode::FDiv, Opcode::ICmp, Opcode::PtrAdd, Opcode::IConst] {
            assert!(!op.has_effects(), "{op}");
        }
    }

    #[test]
    fn commuting_is_only_claimed_where_it_holds() {
        assert!(Opcode::Add.is_commutative());
        assert!(Opcode::FAdd.is_commutative());
        assert!(!Opcode::Sub.is_commutative());
        assert!(!Opcode::FDiv.is_commutative());
        assert!(!Opcode::Shl.is_commutative());
    }

    #[test]
    fn an_integer_predicate_inverts_and_swaps_back_to_itself() {
        for pred in IntPred::all() {
            assert_eq!(pred.inverse().inverse(), pred);
            assert_eq!(pred.swapped().swapped(), pred);
            assert_eq!(IntPred::from_name(pred.name()), Some(pred));
        }
        assert_eq!(IntPred::Slt.inverse(), IntPred::Sge);
        assert_eq!(IntPred::Slt.swapped(), IntPred::Sgt);
        assert_eq!(IntPred::from_name("lt"), None);
    }

    #[test]
    fn a_floating_predicate_inverts_across_the_ordered_line() {
        for pred in FloatPred::all() {
            assert_eq!(pred.inverse().inverse(), pred);
            assert_eq!(pred.swapped().swapped(), pred);
            assert_eq!(FloatPred::from_name(pred.name()), Some(pred));
        }
        // Inverting has to cross the line, because the negation of an ordered comparison is
        // true when an operand is a NaN. This is where `!(a < b)` stops being `a >= b`. The
        // two constants are outside it: neither of them looks at its operands.
        for pred in FloatPred::all().filter(|p| !matches!(p, FloatPred::False | FloatPred::True)) {
            assert_ne!(pred.is_ordered(), pred.inverse().is_ordered(), "{pred}");
        }
        assert_eq!(FloatPred::Olt.inverse(), FloatPred::Uge);
        assert_eq!(FloatPred::Olt.swapped(), FloatPred::Ogt);
    }

    #[test]
    fn swapping_a_predicate_keeps_it_ordered_or_unordered() {
        for pred in FloatPred::all() {
            assert_eq!(pred.is_ordered(), pred.swapped().is_ordered(), "{pred}");
        }
        for pred in IntPred::all() {
            assert_eq!(pred.is_signed(), pred.swapped().is_signed(), "{pred}");
        }
    }

    #[test]
    fn no_two_predicates_share_a_name_within_their_family() {
        for names in [
            IntPred::all().map(IntPred::name).collect::<Vec<_>>(),
            FloatPred::all().map(FloatPred::name).collect::<Vec<_>>(),
        ] {
            let total = names.len();
            let mut names = names;
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), total);
        }
    }
}
