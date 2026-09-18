//! Putting a run of machine instructions together into the shorter run the machine has for it.
//!
//! Design: `spec/10-backend.md` section 10.9, and `spec/optimizer/37-machine-level-optimization.md`
//! sections 37.3 and 37.4.
//!
//! Section 37.4 names this pass first of the ten it says are genuinely machine level, and says what
//! shape it should be: a match over machine instructions in SSA form, inside one block, over a
//! window of a few instructions, which is `gcc/late-combine.cc` rather than `gcc/combine.cc`. The
//! reason for the smaller of the two is in the same section. Combine is fifteen thousand lines
//! because it was written without def-use chains and had to find them again each time, and every
//! RTL pass GCC has written since is on the SSA form it added later for exactly that.
//!
//! Section 37.3 says what the pass does once it has found a run: substitute the earlier instruction
//! into the later one, and ask the machine description whether what came out is an instruction this
//! target has. That is [`crate::changes`] and this pass does not repeat any of it.
//!
//! # The runs it puts together
//!
//! Two of them. A value read out of memory and then used once, by arithmetic that this machine
//! could have read it out of memory itself, which is [`loads`]:
//!
//! ```text
//!   movq 16(%rax), %rcx
//!   addq %rcx, %rdx        ->    addq 16(%rax), %rdx
//! ```
//!
//! Two instructions become one. The register the load wrote is not written at all, which is one
//! fewer value for the allocator to find a place for, and the bytes come down because an addressing
//! mode costs what it costs whichever instruction carries it and the load's own opcode byte goes.
//!
//! It is the commonest pair in the machine IR this compiler writes. Counting adjacent instructions
//! over the corpus at `-O2`, where the first writes what the second reads, the largest family by a
//! long way is a move into arithmetic, and an addition at eight bytes is the largest single entry
//! in it. What the pass gets over that corpus is 865 of these at `-O2` and 845 fewer instructions
//! once the allocator has had its say, with the difference between the two explained below.
//!
//! And the same value written back where it came from, which is [`stores`]:
//!
//! ```text
//!   movq 16(%rax), %rcx
//!   addq %rdx, %rcx        ->    addq %rdx, 16(%rax)
//!   movq %rcx, 16(%rax)
//! ```
//!
//! Three instructions become one, and this is what a C program writes as `*p += x`. The register in
//! the middle goes the way the load's register goes above, and so does the second addressing mode,
//! which was the same address written down twice.
//!
//! [`stores`] takes the same run with a constant in it, which is what a C program writes as
//! `*p += 1` and is the commoner of the two:
//!
//! ```text
//!   movq 16(%rax), %rcx
//!   addq $1, %rcx          ->    addq $1, 16(%rax)
//!   movq %rcx, 16(%rax)
//! ```
//!
//! Nothing is left holding a register here at all. The instruction that comes out reads the place,
//! adds the constant the instruction carries and writes the place, so the whole run costs the
//! addressing mode and the constant and no operand the allocator has to answer for.
//!
//! [`stores`] runs first. Its run is three instructions as the selector wrote them, and folding the
//! load into the middle one first would leave the same run written a second way that the walk would
//! then have to know about. Whatever it does not take is still a pair for [`loads`].
//!
//! # Why no rule does it
//!
//! The selector matches a term, and a term is one value. A load is a term and an addition is a
//! term, and the pattern that would cover both is an addition with a load under it, which the
//! selector does offer: it shows a rule the operands of its operands. What it cannot offer is the
//! rest of the condition. Whether the load may move down to where the addition is depends on what
//! is written between the two, and whether the load's value is wanted anywhere else depends on the
//! whole function. Neither is a fact about the term, so neither can be in a pattern.
//!
//! # When the load may move
//!
//! The load stops being where it was and starts being part of an instruction further down the
//! block, so everything between the two has to be something the load can pass. Two things are not.
//!
//! Anything that touches memory, whether it reads or writes. A write is the obvious half: whether
//! it writes the bytes this load reads is a question about two addresses, and telling two addresses
//! apart is an analysis nothing below selection has, so the walk below stops at a store rather than
//! guessing. [`MachineInsts::touches_mem`] is the target's answer and [`MachineInsts::calls`] is the
//! rest of it, since what a call does to memory is not in the instruction at all.
//!
//! A read is the half that is easy to argue away and is the one that matters. Moving a read past a
//! read changes the order two accesses happen in, and the machine IR does not say which accesses
//! the program insisted on: a `volatile` read and an ordinary one are the same instruction with the
//! same operands here, as [`crate::copies`] says at more length about the same problem. So
//! `volatile int a, b; return b - a;` is two loads and a subtract, and folding the first of them
//! into the subtract would read `b` before `a` when the program said otherwise. Stopping at any
//! access at all is what rules that out, and it costs almost nothing: the load that the arithmetic
//! reads is nearly always the last access before it, so it is still the one that folds.
//!
//! What follows from that is the shape of the walk. There is one load in hand rather than a list of
//! them, and it is always the last memory access there was.
//!
//! Anything that writes a register the address reads. Machine IR is in SSA form until the
//! allocator has run, so a virtual register cannot be written twice, but the stack pointer and the
//! frame pointer are physical here and an address into the frame reads one of them.
//!
//! # When the load is wanted elsewhere
//!
//! Exactly one instruction may read what the load wrote, and it has to be the one taking the load
//! in. [`Reads`] is that count, kept across the commits of the pass the way [`crate::fold`] keeps
//! it, and a count of one is the whole of the test because a virtual register is written once. Two
//! readers and the load has to stay where it is, so putting it into one of them buys nothing and
//! costs a second read of memory.
//!
//! An argument an edge carries is a read like any other and is in no operand vector, which is the
//! one place a count of this shape is easy to get wrong. [`Reads::of`] counts those, which is what
//! keeps a load whose value leaves the block out of this.
//!
//! # Which arithmetic
//!
//! [`FOLDS`] is the list, and it is a list rather than a rule about names because the two ends of
//! each entry are instructions the target describes separately and the widths have to agree. A
//! sixty four bit addition takes a sixty four bit load and nothing else: reading four bytes where
//! the program asked for eight is a different instruction, and reading eight where it asked for
//! four is three bytes nobody said were there.
//!
//! The eight bit multiply is the one member of the family with no entry. This machine has no
//! two-operand multiply narrower than sixteen bits, so an eight bit one is written as a thirty two
//! bit `imul` and reads a register whose upper bits nothing looks at. A memory operand has no
//! upper bits to not look at, so there is nothing to read there and the entry is left out.
//!
//! # Either source, when the operation does not care
//!
//! An addition reads two registers and it is the second of them the memory operand replaces,
//! because the first is the one the destination is tied to. Where the load feeds the first instead,
//! the two sources are swapped first, which is a change to the instruction and not to what it
//! computes as long as the operation commutes. Five of the six here do and subtraction does not,
//! which is what [`Fold::commutes`] says.
//!
//! # What a `volatile` access gets
//!
//! The same thing an ordinary one does, and that is worth writing down rather than leaving to be
//! noticed. `volatile int *p; *p += x;` comes out of here as one instruction that reads the place
//! and writes it back, where GCC writes the load, the arithmetic and the store. Both do one read
//! and one write of that address, which is what the abstract machine says has to happen, so the
//! program is the program either way. What they differ on is whether the two are one instruction,
//! and a machine whose memory does something when it is read cares about that.
//!
//! The reason it happens is the one this module already gives twice: the machine IR does not carry
//! the flag. `rucc_ir::Flags::VOLATILE` says an access happens exactly once and is never moved or
//! merged, every pass above selection reads it, and the instruction that reaches this pass is the
//! same instruction whether it was set or not. [`loads`] has the same hole and has had it since it
//! landed: a `volatile` load folds into the arithmetic that reads it, which is still one read and
//! is still not what GCC writes. Carrying the flag down is what closes both, and it is
//! tamnd/rucc#1302 rather than something this pass can decide on its own.
//!
//! # What makes the three one
//!
//! The same three questions as the pair, and one more. The word the load read is read by the
//! arithmetic and by nothing else, the answer the arithmetic wrote is read by the store and by
//! nothing else, and nothing between the load and the store touches memory or writes a register the
//! instruction that is left still reads. The run collapses onto the store, so the read of memory
//! moves down the block to where the write already was, which is the move the memory rule is about.
//!
//! The one more is that the two addressing modes have to name the same place. The same registers,
//! the same scale, the same displacement and the same symbol is most of it, and the frame is the
//! rest: the displacement of a local is a number [`crate::finish`] has still to add the frame's own
//! offset to, so two locals can be the same three registers and the same zero here and be two
//! different places. The list itself is what tells those apart, and the entry the load was
//! waiting on comes off the list when the run is joined, since the store is already waiting on the
//! same one.
//!
//! # The window
//!
//! A load is carried forward at most [`WINDOW`] instructions and then dropped. The bound is what
//! makes the pass cost a fixed amount per instruction rather than an amount that grows with the
//! block, which section 37.3 records as GCC's own answer: `max-combine-insns` is four and has been
//! for decades.
//!
//! It is also nearly all of it already at one. The measurement in [`WINDOW`] is that a bound of one
//! finds 852 folds over the corpus and a bound of thirty two finds 865, which follows from the rule
//! above about memory rather than from anything about how the selector writes code: the load that
//! folds is the last access to memory before the arithmetic, and the last access before it is
//! usually the instruction in front of it. The window is there to bound the walk and it earns
//! thirteen folds along the way.
//!
//! # Where it costs something
//!
//! A fold takes out exactly one instruction, so the number of folds and the number of instructions
//! saved should be the same number, and they are not: 865 folds against 845 instructions over the
//! corpus at `-O2`, and 1609 against 1444 over the SQLite amalgamation. The gap is the allocator.
//!
//! Taking the load out changes which values are live where, so the allocator makes different
//! choices, and a few of them are worse. Two programs in the corpus come out two instructions
//! longer at every level above `-O0`, both for the same reason: the folded addition is given a
//! callee saved register while a caller saved one was free, which buys a push, a pop and a copy for
//! a value that dies before the next call. That is the allocator preferring the wrong end of its
//! own list rather than anything this pass did, and it is worth fixing where it is rather than
//! worth not folding over.
//!
//! The trade is the other thing the gap is, and it is a real one rather than an accounting error.
//! Two instructions become one and the one that is left both reads memory and computes, so it is
//! two operations in one slot rather than one, which a machine that issues several instructions at
//! once may not want. The measurement that settles it is run time rather than instruction count,
//! and section 38.6's scheduler is where that argument belongs, since a scheduler is the pass that
//! can see whether the slot was going to be used.
//!
//! # What it does not do yet
//!
//! A comparison. This machine compares against memory as readily as it adds to it, and the reason
//! there is no entry for one is that a comparison here is one opcode holding a compare and the byte
//! behind it, so the memory form is a third instruction rather than a second and the target has to
//! describe it before this can write it.
//!
//! Arithmetic against a constant, in either run. `addq $1, 16(%rcx)` is `*p += 1`, which is at
//! least as common as `*p += x`, and the target has no form that carries an addressing mode and an
//! immediate together. That is a third instruction description rather than a rule, the way the
//! comparison above is.
//!
//! Anything longer than the two runs above. Section 37.3 says GCC goes to four instructions, and
//! the longer of the two here is three. What makes a fourth worth having is a rule set that has
//! something to say about four, and the rule set here grows one measured entry at a time.

use rucc_base::Interner;
use rucc_mir::{Amode, Func, Inst, Opcode, Operand, Reg};
use rucc_target::MachineInsts;

use crate::changes::{Changes, Plan, Reads};
use crate::fold::Pending;

/// How far a load is carried looking for the instruction that takes it in.
///
/// Measured over the corpus at `-O2`, which folds this many loads at each bound:
///
/// ```text
///   1     2     4     8    16    32
/// 852   858   863   864   865   865
/// ```
///
/// Sixteen, because that is where the curve stops. Doubling it again finds nothing, and the pass
/// still costs a fixed amount per instruction, which is what the bound is for.
///
/// The curve is that flat because of the rule about memory rather than because of anything the
/// selector does. The load that folds is the last access to memory before the arithmetic, and
/// almost always that is the instruction immediately in front of it. What the room past one buys is
/// the thirteen where a register was written or a constant made in between.
pub const WINDOW: usize = 16;

/// One arithmetic instruction that could read its second source out of memory, and the load that
/// would fill it.
///
/// A table rather than a rule about spellings, because the three names in each row are three things
/// the target describes on their own and nothing about `add_rr_64` says that `mov_rm_64` is the
/// load of the same width. Writing the three together is what makes a mismatched width a line
/// somebody can see rather than a string that was built at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    /// The arithmetic as the selector wrote it, reading both its sources from registers.
    pub from: &'static str,
    /// The same arithmetic reading its second source out of memory.
    pub into: &'static str,
    /// The load that would have filled that register, which has to be of the same width.
    pub load: &'static str,
    /// Whether the two sources may be swapped, which is what lets the load feed either of them.
    pub commutes: bool,
}

/// The arithmetic a load can move into on this machine.
///
/// Every two-address integer operation the target has, at every width it has one, except the eight
/// bit multiply the module documentation gives the reason for. Subtraction is the one that does not
/// commute.
pub static FOLDS: &[Fold] = &[
    Fold { from: "add_rr_8", into: "add_rm_8", load: "mov_rm_8", commutes: true },
    Fold { from: "add_rr_16", into: "add_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "add_rr_32", into: "add_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "add_rr_64", into: "add_rm_64", load: "mov_rm_64", commutes: true },
    Fold { from: "sub_rr_8", into: "sub_rm_8", load: "mov_rm_8", commutes: false },
    Fold { from: "sub_rr_16", into: "sub_rm_16", load: "mov_rm_16", commutes: false },
    Fold { from: "sub_rr_32", into: "sub_rm_32", load: "mov_rm_32", commutes: false },
    Fold { from: "sub_rr_64", into: "sub_rm_64", load: "mov_rm_64", commutes: false },
    Fold { from: "and_rr_8", into: "and_rm_8", load: "mov_rm_8", commutes: true },
    Fold { from: "and_rr_16", into: "and_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "and_rr_32", into: "and_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "and_rr_64", into: "and_rm_64", load: "mov_rm_64", commutes: true },
    Fold { from: "or_rr_8", into: "or_rm_8", load: "mov_rm_8", commutes: true },
    Fold { from: "or_rr_16", into: "or_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "or_rr_32", into: "or_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "or_rr_64", into: "or_rm_64", load: "mov_rm_64", commutes: true },
    Fold { from: "xor_rr_8", into: "xor_rm_8", load: "mov_rm_8", commutes: true },
    Fold { from: "xor_rr_16", into: "xor_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "xor_rr_32", into: "xor_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "xor_rr_64", into: "xor_rm_64", load: "mov_rm_64", commutes: true },
    Fold { from: "imul_rr_16", into: "imul_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "imul_rr_32", into: "imul_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "imul_rr_64", into: "imul_rm_64", load: "mov_rm_64", commutes: true },
];

/// One arithmetic instruction that could work on memory rather than on a register, and the load
/// and the store that would be the rest of the run.
///
/// A table for the reason [`Fold`] is one, and four names in a row rather than three because the
/// run is three instructions rather than two. The widths of all four have to agree, and writing
/// them out is what makes a row that got one wrong something a reader can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Update {
    /// The arithmetic as the selector wrote it, on two registers.
    pub from: &'static str,
    /// The same arithmetic reading one source out of memory and leaving its answer there.
    pub into: &'static str,
    /// The load that put the memory's word in a register.
    pub load: &'static str,
    /// The store that put the answer back.
    pub store: &'static str,
    /// Whether the two sources may be swapped, which is what lets the load feed either of them.
    pub commutes: bool,
}

/// The arithmetic that can work on memory in place on this machine.
///
/// The five operations that share an opcode column, at every width. The multiply is not one of
/// them: `imul` writes a register and there is no encoding of it that leaves the product where it
/// read one of its sources, so there is no instruction for a row to name.
///
/// Subtraction is here and does not commute, and the two facts are related. `subq %rax, (%rcx)`
/// takes the register away from the memory, so the run it matches is the one where the load feeds
/// the left source, which is the one arrangement [`FOLDS`] cannot use. The other four take either
/// source, because the answer does not depend on which of the two came out of memory.
pub static UPDATES: &[Update] = &[
    Update {
        from: "add_rr_8",
        into: "add_mr_8",
        load: "mov_rm_8",
        store: "mov_mr_8",
        commutes: true,
    },
    Update {
        from: "add_rr_16",
        into: "add_mr_16",
        load: "mov_rm_16",
        store: "mov_mr_16",
        commutes: true,
    },
    Update {
        from: "add_rr_32",
        into: "add_mr_32",
        load: "mov_rm_32",
        store: "mov_mr_32",
        commutes: true,
    },
    Update {
        from: "add_rr_64",
        into: "add_mr_64",
        load: "mov_rm_64",
        store: "mov_mr_64",
        commutes: true,
    },
    Update {
        from: "sub_rr_8",
        into: "sub_mr_8",
        load: "mov_rm_8",
        store: "mov_mr_8",
        commutes: false,
    },
    Update {
        from: "sub_rr_16",
        into: "sub_mr_16",
        load: "mov_rm_16",
        store: "mov_mr_16",
        commutes: false,
    },
    Update {
        from: "sub_rr_32",
        into: "sub_mr_32",
        load: "mov_rm_32",
        store: "mov_mr_32",
        commutes: false,
    },
    Update {
        from: "sub_rr_64",
        into: "sub_mr_64",
        load: "mov_rm_64",
        store: "mov_mr_64",
        commutes: false,
    },
    Update {
        from: "and_rr_8",
        into: "and_mr_8",
        load: "mov_rm_8",
        store: "mov_mr_8",
        commutes: true,
    },
    Update {
        from: "and_rr_16",
        into: "and_mr_16",
        load: "mov_rm_16",
        store: "mov_mr_16",
        commutes: true,
    },
    Update {
        from: "and_rr_32",
        into: "and_mr_32",
        load: "mov_rm_32",
        store: "mov_mr_32",
        commutes: true,
    },
    Update {
        from: "and_rr_64",
        into: "and_mr_64",
        load: "mov_rm_64",
        store: "mov_mr_64",
        commutes: true,
    },
    Update {
        from: "or_rr_8",
        into: "or_mr_8",
        load: "mov_rm_8",
        store: "mov_mr_8",
        commutes: true,
    },
    Update {
        from: "or_rr_16",
        into: "or_mr_16",
        load: "mov_rm_16",
        store: "mov_mr_16",
        commutes: true,
    },
    Update {
        from: "or_rr_32",
        into: "or_mr_32",
        load: "mov_rm_32",
        store: "mov_mr_32",
        commutes: true,
    },
    Update {
        from: "or_rr_64",
        into: "or_mr_64",
        load: "mov_rm_64",
        store: "mov_mr_64",
        commutes: true,
    },
    Update {
        from: "xor_rr_8",
        into: "xor_mr_8",
        load: "mov_rm_8",
        store: "mov_mr_8",
        commutes: true,
    },
    Update {
        from: "xor_rr_16",
        into: "xor_mr_16",
        load: "mov_rm_16",
        store: "mov_mr_16",
        commutes: true,
    },
    Update {
        from: "xor_rr_32",
        into: "xor_mr_32",
        load: "mov_rm_32",
        store: "mov_mr_32",
        commutes: true,
    },
    Update {
        from: "xor_rr_64",
        into: "xor_mr_64",
        load: "mov_rm_64",
        store: "mov_mr_64",
        commutes: true,
    },
];

/// One arithmetic instruction against a constant that could work on memory, and the load and the
/// store that would be the rest of the run.
///
/// [`Update`] with the register source replaced by an immediate, and a field shorter for it. There
/// is no `commutes`, because there is nothing to swap: the constant is on the instruction and
/// cannot be anywhere else, so the memory is always the left source and every row reads the same
/// way. Subtraction is in the table without a note attached for the same reason. `subl $1, (%rax)`
/// takes one away from the place, which is the run this matches and the only one it could be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bump {
    /// The arithmetic as the selector wrote it, on a register and a constant.
    pub from: &'static str,
    /// The same arithmetic reading memory and leaving its answer there.
    pub into: &'static str,
    /// The load that put the memory's word in a register.
    pub load: &'static str,
    /// The store that put the answer back.
    pub store: &'static str,
}

/// The arithmetic against a constant that can work on memory in place on this machine.
///
/// The same five operations [`UPDATES`] has, at the same four widths, and the multiply is missing
/// for the same reason. The eight bit inclusive or is also the instruction a probing prologue
/// writes, which is one instruction described once rather than two things that happen to encode
/// alike.
///
/// Four rows take nothing today. The narrow inclusive or and exclusive or against a constant are on
/// `crate::select::x86_64`'s list of instructions no rule selects yet, which went out under
/// tamnd/rucc#368 and come back with the width narrowing in tamnd/rucc#375, so a program that writes
/// `*p |= 4` through a `char` gets a constant in a register and a run this cannot match. The rows
/// are here for the reason the descriptions of those instructions stayed: what the machine can do
/// is true whether or not anything asks for it today, and the rows would otherwise be a second
/// thing to remember when #375 lands.
pub static BUMPS: &[Bump] = &[
    Bump { from: "add_ri_8", into: "add_mi_8", load: "mov_rm_8", store: "mov_mr_8" },
    Bump { from: "add_ri_16", into: "add_mi_16", load: "mov_rm_16", store: "mov_mr_16" },
    Bump { from: "add_ri_32", into: "add_mi_32", load: "mov_rm_32", store: "mov_mr_32" },
    Bump { from: "add_ri_64", into: "add_mi_64", load: "mov_rm_64", store: "mov_mr_64" },
    Bump { from: "sub_ri_8", into: "sub_mi_8", load: "mov_rm_8", store: "mov_mr_8" },
    Bump { from: "sub_ri_16", into: "sub_mi_16", load: "mov_rm_16", store: "mov_mr_16" },
    Bump { from: "sub_ri_32", into: "sub_mi_32", load: "mov_rm_32", store: "mov_mr_32" },
    Bump { from: "sub_ri_64", into: "sub_mi_64", load: "mov_rm_64", store: "mov_mr_64" },
    Bump { from: "and_ri_8", into: "and_mi_8", load: "mov_rm_8", store: "mov_mr_8" },
    Bump { from: "and_ri_16", into: "and_mi_16", load: "mov_rm_16", store: "mov_mr_16" },
    Bump { from: "and_ri_32", into: "and_mi_32", load: "mov_rm_32", store: "mov_mr_32" },
    Bump { from: "and_ri_64", into: "and_mi_64", load: "mov_rm_64", store: "mov_mr_64" },
    Bump { from: "or_ri_8", into: "or_mi_8", load: "mov_rm_8", store: "mov_mr_8" },
    Bump { from: "or_ri_16", into: "or_mi_16", load: "mov_rm_16", store: "mov_mr_16" },
    Bump { from: "or_ri_32", into: "or_mi_32", load: "mov_rm_32", store: "mov_mr_32" },
    Bump { from: "or_ri_64", into: "or_mi_64", load: "mov_rm_64", store: "mov_mr_64" },
    Bump { from: "xor_ri_8", into: "xor_mi_8", load: "mov_rm_8", store: "mov_mr_8" },
    Bump { from: "xor_ri_16", into: "xor_mi_16", load: "mov_rm_16", store: "mov_mr_16" },
    Bump { from: "xor_ri_32", into: "xor_mi_32", load: "mov_rm_32", store: "mov_mr_32" },
    Bump { from: "xor_ri_64", into: "xor_mi_64", load: "mov_rm_64", store: "mov_mr_64" },
];

/// The load this block has passed that could still end up inside something.
///
/// One rather than a list of them, because anything that touches memory ends the one being carried,
/// so the one being carried is always the last memory access there was.
#[derive(Debug, Clone, Copy)]
struct Waiting {
    /// The load.
    inst: Inst,
    /// The register it wrote, which is what the arithmetic has to be reading.
    reg: Reg,
    /// Which load it is, so that the width can be held against the arithmetic's.
    load: &'static str,
    /// How far along the block it is, which is what [`WINDOW`] is counted in.
    at: usize,
}

/// Puts every load that can move into the arithmetic that reads it, and gives back how many.
///
/// `pending` is the addresses [`crate::finish`] has still to write a displacement into, and a load
/// that moves takes its entry with it, the same way one folded into a reader does. An address into
/// the frame arrives here already inside the load, because [`crate::fold`] has run.
///
/// Run after selection and after the addresses are folded, and before allocation. Before the
/// allocator because what makes the pair safe to put together is that a virtual register is written
/// once, and after the addresses because a load whose address is still a `lea` in front of it has
/// nothing in its own memory operand worth carrying.
pub fn loads(
    func: &mut Func,
    machine: &MachineInsts,
    names: &mut Interner,
    pending: &mut Pending<'_>,
) -> usize {
    let mut reads = Reads::of(func);
    let mut done = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        let mut waiting: Option<Waiting> = None;
        for (at, inst) in func.insts(block).collect::<Vec<_>>().into_iter().enumerate() {
            let name = names.resolve(func[inst].opcode.name()).to_owned();
            let bare = machine.bare(&name).to_owned();
            // Asked before the rewrite below rather than after it, because the rewrite turns an
            // instruction that touched no memory into one that does, and asking afterwards would
            // throw away the load that had just gone into it over the load that had just gone into
            // it. Nothing else about the answer moves: the other end of a row of the fold table is
            // arithmetic this target describes and is not a call.
            let barrier = machine.calls(&name) || !machine.has(&name) || machine.touches_mem(&name);
            if let Some(carried) = waiting {
                if let Some(plan) = joined(func, &reads, carried, machine, names, inst, &bare) {
                    let mut set = Changes::new();
                    set.rewrite(inst, plan);
                    set.remove(carried.inst);
                    if set.commit(func, &mut reads, names, machine).is_ok() {
                        pending.moved(carried.inst, &[inst]);
                        waiting = None;
                        done += 1;
                    }
                }
            }
            if barrier {
                waiting = None;
            }
            if let Some(carried) = waiting {
                if at - carried.at >= WINDOW || writes_what_it_reads(func, inst, &carried) {
                    waiting = None;
                }
            }
            if let Some(load) = FOLDS.iter().find(|fold| fold.load == bare).map(|fold| fold.load) {
                let operands = &func[func[inst].operands];
                if let Some(first) = operands.first().filter(|operand| operand.role.is_def()) {
                    waiting = Some(Waiting { inst, reg: first.reg, load, at });
                }
            }
        }
    }
    done
}

/// The three instructions that read a place, compute on what was there and write it back.
#[derive(Debug, Clone, Copy)]
struct Run {
    /// The load that read the place.
    load: Inst,
    /// The arithmetic that read what the load put in a register.
    alu: Inst,
    /// The store that put the answer back where the load got it.
    store: Inst,
    /// Which row of [`UPDATES`] the run is.
    update: &'static Update,
    /// The source the arithmetic is left reading, which is the one the memory is not.
    kept: Operand,
}

/// The same three instructions with a constant where the other source was.
///
/// A separate shape from [`Run`] rather than the same one with an option in it, because the two
/// differ in what they carry and in nothing else. This one holds the constant the instruction that
/// comes out will carry, and has no `kept`, since the arithmetic is left reading nothing at all.
#[derive(Debug, Clone, Copy)]
struct Bumped {
    /// The load that read the place.
    load: Inst,
    /// The arithmetic that read what the load put in a register.
    alu: Inst,
    /// The store that put the answer back where the load got it.
    store: Inst,
    /// Which row of [`BUMPS`] the run is.
    bump: &'static Bump,
    /// The constant the arithmetic was against.
    imm: i64,
}

/// Puts every run that reads a place, computes on it and writes it back into the one instruction
/// this machine has for all three, and gives back how many.
///
/// `pending` is the addresses [`crate::finish`] has still to write a displacement into. The store
/// is the instruction that survives and it is already waiting on the entry the load was waiting on,
/// since the two name the same place, so the load's entry is taken off rather than moved.
///
/// Run before [`loads`] rather than after it. The run this looks for is three instructions the
/// selector wrote, and folding the load into the arithmetic first would leave two instructions that
/// are the same thing written differently, so the walk would have to know both spellings. Whatever
/// this does not take is still there for [`loads`] to take the load out of.
///
/// The run whose arithmetic is against a constant is looked for after the one whose arithmetic is
/// against a register, and the order between those two does not matter: the middle instruction
/// decides which of them a run is, and no instruction is both an [`UPDATES`] row and a [`BUMPS`]
/// row.
pub fn stores(
    func: &mut Func,
    machine: &MachineInsts,
    names: &mut Interner,
    pending: &mut Pending<'_>,
) -> usize {
    let mut reads = Reads::of(func);
    let mut done = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        let insts: Vec<Inst> = func.insts(block).collect();
        for at in 0..insts.len() {
            let found = match run(func, &reads, machine, names, &insts, at) {
                Some(found) => Some((
                    found.load,
                    found.alu,
                    found.store,
                    updated(func, machine, names, &found),
                )),
                None => constant(func, &reads, machine, names, &insts, at).map(|found| {
                    (found.load, found.alu, found.store, bumped(func, machine, names, &found))
                }),
            };
            let Some((load, alu, store, plan)) = found else { continue };
            if !pending.alike(load, store) {
                continue;
            }
            let mut set = Changes::new();
            set.rewrite(store, plan);
            set.remove(alu);
            set.remove(load);
            if set.commit(func, &mut reads, names, machine).is_ok() {
                pending.moved(load, &[]);
                done += 1;
            }
        }
    }
    done
}

/// The run ending in the instruction at that position, or `None`.
///
/// Walked backwards from the store, because the store is the end of the run and is the instruction
/// that is left when the run is joined. Everything the walk needs is behind it: which register it
/// is storing says which arithmetic to look for, and which source that arithmetic reads says which
/// load.
///
/// An instruction an earlier fold took out is still in `insts` and is read here as though it were
/// where it was. That costs a fold and never takes one: a removed instruction is one more thing in
/// the way, and it cannot be the arithmetic or the load this is looking for, because each of those
/// is the one writer of a register something still reads.
fn run(
    func: &Func,
    reads: &Reads,
    machine: &MachineInsts,
    names: &Interner,
    insts: &[Inst],
    at: usize,
) -> Option<Run> {
    let store = insts[at];
    let stored = machine.bare(names.resolve(func[store].opcode.name())).to_owned();
    let value = *func[func[store].operands].first()?;
    if value.role.is_def() || reads.count(value.reg) != 1 {
        return None;
    }
    // One bound over the whole run rather than one per pair, so that what the window means is how
    // far apart the first and the last of the three may be.
    let earliest = at.saturating_sub(WINDOW);
    let alu = (earliest..at).rev().find(|&k| writes(func, insts[k], value.reg))?;
    let bare = machine.bare(names.resolve(func[insts[alu]].opcode.name())).to_owned();
    let update = UPDATES.iter().find(|row| row.from == bare && row.store == stored)?;
    let operands = func[func[insts[alu]].operands].to_vec();
    let [_, first, second] = operands[..] else { return None };
    // The left source is the one the memory takes the place of, because the answer is left where
    // the memory operand points and the answer is tied to the left source. Where the load feeds the
    // right one instead and the operation commutes, the two swap, which leaves the instruction
    // computing what it computed.
    let both = [(first, second), (second, first)];
    let tried = if update.commutes { &both[..] } else { &both[..1] };
    for &(source, kept) in tried {
        if reads.count(source.reg) != 1 {
            continue;
        }
        let Some(from) = (earliest..alu).rev().find(|&k| writes(func, insts[k], source.reg)) else {
            continue;
        };
        let load = insts[from];
        if machine.bare(names.resolve(func[load].opcode.name())) != update.load {
            continue;
        }
        if !same_place(func, load, store) {
            continue;
        }
        // The registers the one instruction left is reading, which are the ones nothing between the
        // load and the store may write. The arithmetic itself passes this without being left out of
        // it: what it writes is the value the store is storing, and that register is not one of
        // these.
        let mut wanted: Vec<Reg> =
            func[func[store].operands][1..].iter().map(|operand| operand.reg).collect();
        wanted.push(kept.reg);
        if !clear(func, machine, names, insts, (from, at), &wanted) {
            continue;
        }
        return Some(Run { load, alu: insts[alu], store, update, kept });
    }
    None
}

/// The run against a constant ending in the instruction at that position, or `None`.
///
/// [`run`] with the arithmetic's second source gone. Walked backwards from the store for the same
/// reason, and asking the same four questions: the stored register is read once, the source the
/// arithmetic reads is written once by a load of the right width, that load names the same place as
/// the store, and nothing between the two is in the way. There is no arrangement to choose between,
/// because the constant is on the instruction and only the left source can be the memory.
///
/// One question [`run`] does not ask is here: where the addressing mode's registers are. The
/// instruction that comes out has no operand in front of them, so each of them moves one place
/// towards the front of the vector, and a mode that already pointed at the front would have to move
/// to nowhere. That cannot happen, since the front is the value the store is storing, and refusing
/// the run is what it costs to say so rather than to assume it.
fn constant(
    func: &Func,
    reads: &Reads,
    machine: &MachineInsts,
    names: &Interner,
    insts: &[Inst],
    at: usize,
) -> Option<Bumped> {
    let store = insts[at];
    let stored = machine.bare(names.resolve(func[store].opcode.name())).to_owned();
    let value = *func[func[store].operands].first()?;
    if value.role.is_def() || reads.count(value.reg) != 1 {
        return None;
    }
    let mem = func[func[store].mem?];
    if mem.base == Some(0) || mem.index == Some(0) {
        return None;
    }
    let earliest = at.saturating_sub(WINDOW);
    let alu = (earliest..at).rev().find(|&k| writes(func, insts[k], value.reg))?;
    let bare = machine.bare(names.resolve(func[insts[alu]].opcode.name())).to_owned();
    let bump = BUMPS.iter().find(|row| row.from == bare && row.store == stored)?;
    let operands = func[func[insts[alu]].operands].to_vec();
    let [_, source] = operands[..] else { return None };
    let imm = func[func[insts[alu]].imm?].0;
    if reads.count(source.reg) != 1 {
        return None;
    }
    let from = (earliest..alu).rev().find(|&k| writes(func, insts[k], source.reg))?;
    let load = insts[from];
    if machine.bare(names.resolve(func[load].opcode.name())) != bump.load {
        return None;
    }
    if !same_place(func, load, store) {
        return None;
    }
    // The registers the one instruction left is reading, which are the ones in its address and no
    // others, since the constant is not in a register and the arithmetic is left reading nothing.
    let wanted: Vec<Reg> =
        func[func[store].operands][1..].iter().map(|operand| operand.reg).collect();
    if !clear(func, machine, names, insts, (from, at), &wanted) {
        return None;
    }
    Some(Bumped { load, alu: insts[alu], store, bump, imm })
}

/// Whether this instruction writes that register.
fn writes(func: &Func, inst: Inst, reg: Reg) -> bool {
    func[func[inst].operands].iter().any(|operand| operand.role.is_def() && operand.reg == reg)
}

/// Whether the two instructions name the same place in memory.
///
/// The same addressing mode, the same symbol, and the same registers where the mode holds operand
/// positions. Both instructions here write their value down first and their address behind it, so
/// the positions line up, and the registers are compared anyway rather than the positions, because
/// what makes two addresses one place is which registers they read.
fn same_place(func: &Func, one: Inst, other: Inst) -> bool {
    let (Some(here), Some(there)) = (func[one].mem, func[other].mem) else { return false };
    let (here, there) = (func[here], func[there]);
    if func[one].symbol != func[other].symbol {
        return false;
    }
    let bare = |amode: Amode| Amode { base: None, index: None, ..amode };
    if bare(here) != bare(there) {
        return false;
    }
    let same = |left: Option<u8>, right: Option<u8>| match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            func[func[one].operands][usize::from(left)].reg
                == func[func[other].operands][usize::from(right)].reg
        }
        _ => false,
    };
    same(here.base, there.base) && same(here.index, there.index)
}

/// Whether everything between the two positions may be passed.
///
/// The run becomes one instruction where the store is, so the read of memory the load was doing
/// moves down the block to there. Nothing that touches memory may be passed, for the reason the
/// module documentation gives about [`loads`], and nothing may write a register the instruction
/// that is left still reads.
fn clear(
    func: &Func,
    machine: &MachineInsts,
    names: &Interner,
    insts: &[Inst],
    span: (usize, usize),
    wanted: &[Reg],
) -> bool {
    let (from, to) = span;
    insts[from + 1..to].iter().all(|&inst| {
        let name = names.resolve(func[inst].opcode.name());
        if machine.calls(name) || !machine.has(name) || machine.touches_mem(name) {
            return false;
        }
        !func[func[inst].operands]
            .iter()
            .any(|operand| operand.role.is_def() && wanted.contains(&operand.reg))
    })
}

/// What the store becomes with the rest of the run inside it.
///
/// The store's own addressing mode and the source the arithmetic kept, which is the whole of it.
/// The mode is left exactly as it was, because the operand it was written against is the value the
/// store was storing and what takes that operand's place is one operand as well.
fn updated(func: &Func, machine: &MachineInsts, names: &mut Interner, run: &Run) -> Plan {
    let operands = func[func[run.store].operands].to_vec();
    let into = names.intern(&format!("{}{}", machine.prefix, run.update.into));
    Plan {
        opcode: Opcode::new(into),
        operands: [run.kept].into_iter().chain(operands[1..].iter().copied()).collect(),
        imm: None,
        amode: func[run.store].mem.map(|mem| func[mem]),
        symbol: func[run.store].symbol,
    }
}

/// What the store becomes with the rest of a constant run inside it.
///
/// The store's own addressing mode again, and the constant the arithmetic carried. The mode does
/// not come through untouched this time. The value the store was storing has nothing taking its
/// place, so the registers behind it each move one place towards the front of the operand vector,
/// and the positions the mode holds are positions in that vector and move with them. [`constant`]
/// is what makes sure there is a place for each of them to move to.
fn bumped(func: &Func, machine: &MachineInsts, names: &mut Interner, run: &Bumped) -> Plan {
    let operands = func[func[run.store].operands][1..].to_vec();
    let into = names.intern(&format!("{}{}", machine.prefix, run.bump.into));
    let back = |at: Option<u8>| at.map(|at| at - 1);
    Plan {
        opcode: Opcode::new(into),
        operands,
        imm: Some(run.imm),
        amode: func[run.store].mem.map(|mem| {
            let mem = func[mem];
            Amode { base: back(mem.base), index: back(mem.index), ..mem }
        }),
        symbol: func[run.store].symbol,
    }
}

/// Whether this instruction writes a register the carried load needs left alone.
///
/// The registers its address reads, and the register it wrote. The second is there for the same
/// reason the first is: a virtual register cannot be written twice while the IR is in SSA form, and
/// these are the physical ones a function has before the allocator runs.
fn writes_what_it_reads(func: &Func, inst: Inst, carried: &Waiting) -> bool {
    let written: Vec<Reg> = func[func[inst].operands]
        .iter()
        .filter(|operand| operand.role.is_def())
        .map(|operand| operand.reg)
        .collect();
    func[func[carried.inst].operands].iter().any(|operand| written.contains(&operand.reg))
}

/// What this instruction becomes with the carried load inside it, or `None`.
///
/// Nothing here changes anything. What comes back is a proposal, and whether the target has the
/// instruction it describes is [`Changes`]'s answer rather than this one.
fn joined(
    func: &Func,
    reads: &Reads,
    carried: Waiting,
    machine: &MachineInsts,
    names: &mut Interner,
    inst: Inst,
    bare: &str,
) -> Option<Plan> {
    let fold = FOLDS.iter().find(|fold| fold.from == bare)?;
    if carried.load != fold.load || reads.count(carried.reg) != 1 {
        return None;
    }
    let operands = func[func[inst].operands].to_vec();
    let [answer, first, second] = operands[..] else { return None };
    // The second source is the one the memory operand replaces, because the answer is tied to the
    // first. Where the load feeds the first source instead and the operation commutes, the two are
    // swapped, which leaves the instruction computing what it computed.
    let kept = if second.reg == carried.reg {
        first
    } else if fold.commutes && first.reg == carried.reg {
        second
    } else {
        return None;
    };
    let load = carried.inst;
    let address = func[func[load].operands][1..].to_vec();
    let mut amode = func[func[load].mem?];
    // The registers an address names are operands behind the ones the instruction writes down, and
    // there is one of those in front of them here where there was none in front of them in the
    // load, so every position the mode holds moves along by one.
    amode.base = amode.base.map(|at| at + 1);
    amode.index = amode.index.map(|at| at + 1);
    let into = names.intern(&format!("{}{}", machine.prefix, fold.into));
    Some(Plan {
        opcode: Opcode::new(into),
        operands: [answer, kept].into_iter().chain(address).collect(),
        imm: None,
        amode: Some(amode),
        symbol: func[load].symbol,
    })
}

#[cfg(test)]
mod tests {
    use rucc_mir::{self as mir, Constraint, Mem, Operand};
    use rucc_target::x86_64::{GPR, MACHINE};

    use super::*;

    /// A function with one block, and the names it was built with.
    fn empty() -> (Interner, Func, mir::Block) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> Opcode {
        Opcode::new(names.intern(&format!("{}{name}", MACHINE.prefix)))
    }

    /// A load of eight bytes off that register.
    fn load(func: &mut Func, names: &mut Interner, block: mir::Block, base: Reg) -> Reg {
        let into = func.new_vreg(GPR);
        let mov = op(names, "mov_rm_64");
        func.build(block, mov)
            .def(into, GPR)
            .mem(Mem { disp: 16, ..Mem::at(Operand::read(base, GPR)) })
            .finish();
        into
    }

    /// Two-address arithmetic of that name on those two registers, in that order.
    fn alu(
        func: &mut Func,
        names: &mut Interner,
        block: mir::Block,
        name: &str,
        first: Reg,
        second: Reg,
    ) -> Reg {
        let answer = func.new_vreg(GPR);
        let opcode = op(names, name);
        func.build(block, opcode)
            .operand(Operand::write(answer, GPR).with(Constraint::Reuse(1)))
            .uses(first, GPR)
            .uses(second, GPR)
            .finish();
        answer
    }

    /// What every instruction in a block came to, as opcodes.
    fn shape(func: &Func, names: &Interner, block: mir::Block) -> Vec<String> {
        func.insts(block).map(|inst| names.resolve(func[inst].opcode.name()).to_owned()).collect()
    }

    /// The pass, with lists nothing is on.
    fn combine(func: &mut Func, names: &mut Interner) -> usize {
        let mut addresses = Vec::new();
        let mut arguments = Vec::new();
        let mut dynamic = Vec::new();
        let mut pending =
            Pending { addresses: &mut addresses, arguments: &mut arguments, dynamic: &mut dynamic };
        loads(func, &MACHINE, names, &mut pending)
    }

    /// A store of that register to sixteen off that base, which is the address `load` reads.
    fn store(func: &mut Func, names: &mut Interner, block: mir::Block, base: Reg, value: Reg) {
        let mov = op(names, "mov_mr_64");
        func.build(block, mov)
            .uses(value, GPR)
            .mem(Mem { disp: 16, ..Mem::at(Operand::read(base, GPR)) })
            .finish();
    }

    /// The other walk, with lists nothing is on.
    fn update(func: &mut Func, names: &mut Interner) -> usize {
        let mut addresses = Vec::new();
        let mut arguments = Vec::new();
        let mut dynamic = Vec::new();
        let mut pending =
            Pending { addresses: &mut addresses, arguments: &mut arguments, dynamic: &mut dynamic };
        stores(func, &MACHINE, names, &mut pending)
    }

    /// The shape the second walk is for, which is what `*p += x` is.
    #[test]
    fn a_word_read_changed_and_written_back_becomes_one_instruction() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu(&mut func, &mut names, block, "add_rr_64", word, other);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.add_mr_64"]);
        let inst = func.insts(block).next().expect("the addition");
        let mem = func[inst].mem.expect("it writes memory");
        assert_eq!(func[mem].disp, 16, "the address came from the store");
        assert_eq!(func[mem].base, Some(1), "and names the operand behind the source");
        assert_eq!(func[func[inst].operands].len(), 2, "one source and the base of the address");
        assert_eq!(func[func[inst].operands][0].reg, other, "the source it kept");
        assert_eq!(func[func[inst].operands][1].reg, base, "the address");
    }

    /// The same run with the load feeding the right source instead, which an addition does not
    /// mind. What `subq %rax, (%rcx)` computes is memory minus register, so the subtraction below
    /// is the one that has to care.
    #[test]
    fn a_word_read_into_the_right_source_of_an_addition_is_still_one_instruction() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu(&mut func, &mut names, block, "add_rr_64", other, word);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.add_mr_64"]);
        assert_eq!(func[func[func.insts(block).next().expect("it")].operands][0].reg, other);
    }

    /// A subtraction with the memory on the left, which is `*p -= x` and is what the machine
    /// instruction computes.
    #[test]
    fn a_subtraction_taking_a_register_away_from_memory_becomes_one_instruction() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let left = alu(&mut func, &mut names, block, "sub_rr_64", word, other);
        store(&mut func, &mut names, block, base, left);

        assert_eq!(update(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.sub_mr_64"]);
    }

    /// And the same subtraction the other way round, which is `*p = x - *p`. The machine
    /// instruction would compute the other answer, so the run stays three instructions.
    #[test]
    fn a_subtraction_taking_memory_away_from_a_register_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let left = alu(&mut func, &mut names, block, "sub_rr_64", other, word);
        store(&mut func, &mut names, block, base, left);

        assert_eq!(update(&mut func, &mut names), 0);
        assert_eq!(
            shape(&func, &names, block),
            ["x64.mov_rm_64", "x64.sub_rr_64", "x64.mov_mr_64"]
        );
    }

    /// A store to somewhere else. The answer is not going back where it came from, so what is left
    /// is a load and an arithmetic and a store of three different addresses.
    #[test]
    fn a_store_to_another_address_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let elsewhere = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu(&mut func, &mut names, block, "add_rr_64", word, other);
        store(&mut func, &mut names, block, elsewhere, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// The same address at a different displacement, which is the near miss the comparison has to
    /// catch rather than the obvious one above.
    #[test]
    fn a_store_at_another_displacement_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu(&mut func, &mut names, block, "add_rr_64", word, other);
        let mov = op(&mut names, "mov_mr_64");
        func.build(block, mov)
            .uses(sum, GPR)
            .mem(Mem { disp: 24, ..Mem::at(Operand::read(base, GPR)) })
            .finish();

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// The word read again by something else. The load has to stay for the second reader, so the
    /// run is not a run.
    #[test]
    fn a_word_two_instructions_read_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu(&mut func, &mut names, block, "add_rr_64", word, other);
        alu(&mut func, &mut names, block, "xor_rr_64", word, other);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// The answer read by something else as well as by the store, which is `x = *p += 1` and
    /// leaves the answer wanted in a register the joined instruction never writes.
    #[test]
    fn an_answer_something_else_reads_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu(&mut func, &mut names, block, "add_rr_64", word, other);
        store(&mut func, &mut names, block, base, sum);
        alu(&mut func, &mut names, block, "xor_rr_64", sum, other);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// Another access to memory in the middle. The read the run does moves down the block to where
    /// the write was, so it would be moving past this one.
    #[test]
    fn a_run_with_another_access_in_the_middle_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        load(&mut func, &mut names, block, other);
        let sum = alu(&mut func, &mut names, block, "add_rr_64", word, other);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// Something writing the address register in the middle. A physical register is the only one
    /// this can happen to before the allocator runs, and the frame is addressed through two.
    #[test]
    fn a_run_whose_address_register_is_written_in_the_middle_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RSP);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sub = op(&mut names, "sub_ri_64");
        func.build(block, sub)
            .operand(Operand::write(base, GPR).with(Constraint::Reuse(1)))
            .uses(base, GPR)
            .imm(32)
            .finish();
        let sum = alu(&mut func, &mut names, block, "add_rr_64", word, other);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// Two locals whose displacements are both nothing so far. They are the same registers and the
    /// same number here and are two different places, and what says so is the list the frame layout
    /// has still to write an offset into.
    #[test]
    fn two_locals_the_layout_has_not_placed_yet_are_not_the_same_place() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RSP);
        let other = func.new_vreg(GPR);
        let mov = op(&mut names, "mov_rm_64");
        let word = func.new_vreg(GPR);
        func.build(block, mov).def(word, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        let read = func.insts(block).next().expect("the load");
        let sum = alu(&mut func, &mut names, block, "add_rr_64", word, other);
        let put = op(&mut names, "mov_mr_64");
        func.build(block, put).uses(sum, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        let written = func.insts(block).nth(2).expect("the store");

        let mut addresses = vec![(read, 3usize), (written, 4usize)];
        let mut arguments = Vec::new();
        let mut dynamic = Vec::new();
        let mut pending =
            Pending { addresses: &mut addresses, arguments: &mut arguments, dynamic: &mut dynamic };
        assert_eq!(stores(&mut func, &MACHINE, &mut names, &mut pending), 0);
    }

    /// The one local, which is the same place twice and folds. The entry the load was waiting on
    /// comes off the list, because the store is already waiting on the same one and adding the
    /// frame's offset twice would put the local at twice its distance.
    #[test]
    fn the_frame_entry_of_a_load_that_goes_comes_off_the_list() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RSP);
        let other = func.new_vreg(GPR);
        let mov = op(&mut names, "mov_rm_64");
        let word = func.new_vreg(GPR);
        func.build(block, mov).def(word, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        let read = func.insts(block).next().expect("the load");
        let sum = alu(&mut func, &mut names, block, "add_rr_64", word, other);
        let put = op(&mut names, "mov_mr_64");
        func.build(block, put).uses(sum, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        let written = func.insts(block).nth(2).expect("the store");

        let mut addresses = vec![(read, 3usize), (written, 3usize)];
        let mut arguments = Vec::new();
        let mut dynamic = Vec::new();
        let mut pending =
            Pending { addresses: &mut addresses, arguments: &mut arguments, dynamic: &mut dynamic };
        assert_eq!(stores(&mut func, &MACHINE, &mut names, &mut pending), 1);

        let inst = func.insts(block).next().expect("the addition");
        assert_eq!(addresses, [(inst, 3usize)], "one entry, on the instruction that is left");
    }

    /// A run of the wrong width, which is a load of four bytes under an addition of eight.
    #[test]
    fn a_run_whose_widths_disagree_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let narrow = op(&mut names, "mov_rm_32");
        func.build(block, narrow)
            .def(into, GPR)
            .mem(Mem { disp: 16, ..Mem::at(Operand::read(base, GPR)) })
            .finish();
        let sum = alu(&mut func, &mut names, block, "add_rr_64", into, other);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// Every row of the table names four instructions this target has, all of one width.
    #[test]
    fn every_row_of_the_update_table_is_four_instructions_this_target_has() {
        for update in UPDATES {
            for name in [update.from, update.into, update.load, update.store] {
                assert!(MACHINE.has(name), "{name} is not an instruction");
            }
            let width = |name: &str| name.rsplit_once('_').map(|(_, width)| width.to_owned());
            assert_eq!(width(update.from), width(update.into), "{} changes width", update.from);
            assert_eq!(
                width(update.from),
                width(update.load),
                "{} loads another width",
                update.from
            );
            assert_eq!(
                width(update.from),
                width(update.store),
                "{} stores another width",
                update.from
            );
            assert!((MACHINE.takes_mem)(update.into), "{} reaches no memory", update.into);
            assert!(!(MACHINE.takes_mem)(update.from), "{} already reaches memory", update.from);
        }
    }

    /// One row per arithmetic instruction this machine can do in place, for the reason the count
    /// over the fold table is there.
    #[test]
    fn the_update_table_covers_the_arithmetic_this_target_can_do_in_place() {
        assert_eq!(UPDATES.len(), 20, "five operations at four widths, and no multiply");
        let commuting = UPDATES.iter().filter(|update| update.commutes).count();
        assert_eq!(commuting, 16, "everything but the four subtractions");
    }

    /// Two-address arithmetic of that name against a constant.
    fn alu_imm(
        func: &mut Func,
        names: &mut Interner,
        block: mir::Block,
        name: &str,
        source: Reg,
        value: i64,
    ) -> Reg {
        let answer = func.new_vreg(GPR);
        let opcode = op(names, name);
        func.build(block, opcode)
            .operand(Operand::write(answer, GPR).with(Constraint::Reuse(1)))
            .uses(source, GPR)
            .imm(value)
            .finish();
        answer
    }

    /// The shape the constant run is for, which is what `*p += 1` is.
    #[test]
    fn a_word_read_changed_by_a_constant_and_written_back_becomes_one_instruction() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu_imm(&mut func, &mut names, block, "add_ri_64", word, 1);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.add_mi_64"]);
        let inst = func.insts(block).next().expect("the addition");
        let mem = func[inst].mem.expect("it writes memory");
        assert_eq!(func[mem].disp, 16, "the address came from the store");
        assert_eq!(func[mem].base, Some(0), "which is now the first operand and not the second");
        assert_eq!(func[func[inst].operands].len(), 1, "the base of the address and nothing else");
        assert_eq!(func[func[inst].operands][0].reg, base, "the address");
        assert_eq!(func[func[inst].imm.expect("the constant")].0, 1);
    }

    /// The subtraction, which needs no arrangement chosen for it. A constant cannot be the left
    /// source, so the run that exists is the one the instruction computes.
    #[test]
    fn a_constant_taken_away_from_a_place_becomes_one_instruction() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let left = alu_imm(&mut func, &mut names, block, "sub_ri_64", word, 7);
        store(&mut func, &mut names, block, base, left);

        assert_eq!(update(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.sub_mi_64"]);
        assert_eq!(func[func[func.insts(block).next().expect("it")].imm.expect("it")].0, 7);
    }

    /// The narrow one, so that a width that is carried through wrong is a test that fails rather
    /// than a program that is wrong.
    #[test]
    fn a_byte_read_changed_by_a_constant_and_written_back_becomes_one_instruction() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let word = func.new_vreg(GPR);
        let mov = op(&mut names, "mov_rm_8");
        func.build(block, mov)
            .def(word, GPR)
            .mem(Mem { disp: 16, ..Mem::at(Operand::read(base, GPR)) })
            .finish();
        let sum = alu_imm(&mut func, &mut names, block, "or_ri_8", word, 4);
        let put = op(&mut names, "mov_mr_8");
        func.build(block, put)
            .uses(sum, GPR)
            .mem(Mem { disp: 16, ..Mem::at(Operand::read(base, GPR)) })
            .finish();

        assert_eq!(update(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.or_mi_8"]);
    }

    /// The word read again by something else, which is the first of the four conditions and is
    /// asked here the way it is asked of the register run.
    #[test]
    fn a_word_a_constant_changes_and_something_else_reads_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu_imm(&mut func, &mut names, block, "add_ri_64", word, 1);
        alu(&mut func, &mut names, block, "xor_rr_64", word, other);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// Something else in the middle that touches memory, which the one instruction left would be
    /// passing if the run were joined.
    #[test]
    fn a_constant_run_with_another_access_in_the_middle_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let elsewhere = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu_imm(&mut func, &mut names, block, "add_ri_64", word, 1);
        load(&mut func, &mut names, block, elsewhere);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// The address register written between the load and the store, which would leave the one
    /// instruction naming a different place from the one the run read.
    #[test]
    fn a_constant_run_whose_address_register_is_written_in_the_middle_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RAX);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu_imm(&mut func, &mut names, block, "add_ri_64", word, 1);
        let mov = op(&mut names, "mov_ri_64");
        func.build(block, mov).def(base, GPR).imm(0).finish();
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// A store somewhere else, which is the run that is not a run.
    #[test]
    fn a_constant_written_to_another_address_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let elsewhere = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sum = alu_imm(&mut func, &mut names, block, "add_ri_64", word, 1);
        store(&mut func, &mut names, block, elsewhere, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// A run of the wrong width, which is a load of four bytes under an addition of eight.
    #[test]
    fn a_constant_run_whose_widths_disagree_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let narrow = op(&mut names, "mov_rm_32");
        func.build(block, narrow)
            .def(into, GPR)
            .mem(Mem { disp: 16, ..Mem::at(Operand::read(base, GPR)) })
            .finish();
        let sum = alu_imm(&mut func, &mut names, block, "add_ri_64", into, 1);
        store(&mut func, &mut names, block, base, sum);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// The multiply, which has a two-address form against a constant and no form that leaves the
    /// product in memory, so the run stays three instructions.
    #[test]
    fn a_place_multiplied_by_a_constant_stays_three_instructions() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let product = alu_imm(&mut func, &mut names, block, "imul_ri_64", word, 3);
        store(&mut func, &mut names, block, base, product);

        assert_eq!(update(&mut func, &mut names), 0);
    }

    /// The local, which is the same place twice and folds, and whose frame entry comes off the
    /// list for the reason the register run's does.
    #[test]
    fn the_frame_entry_of_a_load_a_constant_run_takes_comes_off_the_list() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RSP);
        let mov = op(&mut names, "mov_rm_64");
        let word = func.new_vreg(GPR);
        func.build(block, mov).def(word, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        let read = func.insts(block).next().expect("the load");
        let sum = alu_imm(&mut func, &mut names, block, "add_ri_64", word, 1);
        let put = op(&mut names, "mov_mr_64");
        func.build(block, put).uses(sum, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        let written = func.insts(block).nth(2).expect("the store");

        let mut addresses = vec![(read, 3usize), (written, 3usize)];
        let mut arguments = Vec::new();
        let mut dynamic = Vec::new();
        let mut pending =
            Pending { addresses: &mut addresses, arguments: &mut arguments, dynamic: &mut dynamic };
        assert_eq!(stores(&mut func, &MACHINE, &mut names, &mut pending), 1);

        let inst = func.insts(block).next().expect("the addition");
        assert_eq!(addresses, [(inst, 3usize)], "one entry, on the instruction that is left");
    }

    /// Two locals the layout has not placed yet, which are the same addressing mode and not the
    /// same place, the way they are for the register run.
    #[test]
    fn two_locals_a_constant_run_would_join_are_not_the_same_place() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RSP);
        let mov = op(&mut names, "mov_rm_64");
        let word = func.new_vreg(GPR);
        func.build(block, mov).def(word, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        let read = func.insts(block).next().expect("the load");
        let sum = alu_imm(&mut func, &mut names, block, "add_ri_64", word, 1);
        let put = op(&mut names, "mov_mr_64");
        func.build(block, put).uses(sum, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        let written = func.insts(block).nth(2).expect("the store");

        let mut addresses = vec![(read, 3usize), (written, 4usize)];
        let mut arguments = Vec::new();
        let mut dynamic = Vec::new();
        let mut pending =
            Pending { addresses: &mut addresses, arguments: &mut arguments, dynamic: &mut dynamic };
        assert_eq!(stores(&mut func, &MACHINE, &mut names, &mut pending), 0);
    }

    /// Every row of the constant table names four instructions this target has, all of one width.
    #[test]
    fn every_row_of_the_bump_table_is_four_instructions_this_target_has() {
        for bump in BUMPS {
            for name in [bump.from, bump.into, bump.load, bump.store] {
                assert!(MACHINE.has(name), "{name} is not an instruction");
            }
            let width = |name: &str| name.rsplit_once('_').map(|(_, width)| width.to_owned());
            assert_eq!(width(bump.from), width(bump.into), "{} changes width", bump.from);
            assert_eq!(width(bump.from), width(bump.load), "{} loads another width", bump.from);
            assert_eq!(width(bump.from), width(bump.store), "{} stores another width", bump.from);
            assert!((MACHINE.takes_mem)(bump.into), "{} reaches no memory", bump.into);
            assert!(!(MACHINE.takes_mem)(bump.from), "{} already reaches memory", bump.from);
            assert!((MACHINE.takes_imm)(bump.into), "{} carries no constant", bump.into);
        }
    }

    /// One row per arithmetic instruction this machine can do in place against a constant, which is
    /// the same five operations at the same four widths the register table has.
    #[test]
    fn the_bump_table_covers_the_arithmetic_this_target_can_do_in_place_against_a_constant() {
        assert_eq!(BUMPS.len(), 20, "five operations at four widths, and no multiply");
        let register: Vec<&str> = UPDATES.iter().map(|update| update.from).collect();
        for bump in BUMPS {
            let same = bump.from.replace("_ri_", "_rr_");
            assert!(register.contains(&same.as_str()), "{} has no register row", bump.from);
        }
    }

    /// No instruction is in both tables, which is what lets the two walks be tried one after the
    /// other without either having to know what the other took.
    #[test]
    fn nothing_is_both_a_register_run_and_a_constant_run() {
        for bump in BUMPS {
            assert!(
                !UPDATES.iter().any(|update| update.from == bump.from),
                "{} starts both kinds of run",
                bump.from
            );
        }
    }

    /// The shape the whole pass is for.
    #[test]
    fn a_load_read_once_by_an_addition_becomes_its_memory_operand() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.add_rm_64"]);
        let inst = func.insts(block).next().expect("the addition");
        let mem = func[inst].mem.expect("the addition reads memory now");
        assert_eq!(func[mem].disp, 16, "the load's displacement came with it");
        assert_eq!(func[mem].base, Some(2), "and names the operand behind the source it kept");
        assert_eq!(func[func[inst].operands][1].reg, other, "the source it kept");
        assert_eq!(func[func[inst].operands][2].reg, base, "the address it took on");
    }

    /// The same load feeding the source the answer is tied to. The two sources are swapped, which
    /// an addition does not mind and is what lets this fold at all.
    #[test]
    fn a_load_feeding_the_first_source_of_an_addition_is_swapped_and_folded() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "add_rr_64", word, other);

        assert_eq!(combine(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.add_rm_64"]);
        let inst = func.insts(block).next().expect("the addition");
        assert_eq!(func[func[inst].operands][1].reg, other);
    }

    /// A subtraction with the load on the left, which is the one place the swap above would change
    /// the answer.
    #[test]
    fn a_load_feeding_the_left_of_a_subtraction_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "sub_rr_64", word, other);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64", "x64.sub_rr_64"]);
    }

    /// And the same subtraction the other way round, which is the one that folds.
    #[test]
    fn a_load_feeding_the_right_of_a_subtraction_folds() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "sub_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.sub_rm_64"]);
    }

    /// Two readers. The load has to stay where it is for the second of them, so putting it into the
    /// first buys nothing and reads the memory twice.
    #[test]
    fn a_load_two_instructions_read_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "add_rr_64", other, word);
        alu(&mut func, &mut names, block, "xor_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(
            shape(&func, &names, block),
            ["x64.mov_rm_64", "x64.add_rr_64", "x64.xor_rr_64"]
        );
    }

    /// A store between the two. Whether it writes what the load reads is a question about two
    /// addresses, and the answer to not being able to tell is to leave the load where it is.
    #[test]
    fn a_load_with_a_store_between_it_and_its_reader_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let store = op(&mut names, "mov_mr_64");
        func.build(block, store).uses(other, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(
            shape(&func, &names, block),
            ["x64.mov_rm_64", "x64.mov_mr_64", "x64.add_rr_64"]
        );
    }

    /// Another load between the two, which writes nothing and is still not passed.
    ///
    /// This is the one that would be wrong if the walk asked only about writes. Where both reads
    /// are `volatile` the program said which of them happens first, and nothing here can tell that
    /// program from the one that did not say it, so neither may be reordered.
    #[test]
    fn a_load_with_another_load_between_it_and_its_reader_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        load(&mut func, &mut names, block, other);
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(
            shape(&func, &names, block),
            ["x64.mov_rm_64", "x64.mov_rm_64", "x64.add_rr_64"]
        );
    }

    /// The second of two loads, read by arithmetic that reads the first as well. Nothing moves past
    /// anything, which is what makes this one the shape the pass is allowed to take.
    #[test]
    fn the_later_of_two_loads_is_the_one_that_folds() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let first = load(&mut func, &mut names, block, base);
        let second = load(&mut func, &mut names, block, other);
        alu(&mut func, &mut names, block, "add_rr_64", first, second);

        assert_eq!(combine(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64", "x64.add_rm_64"]);
        let addition = func.insts(block).nth(1).expect("the addition");
        assert_eq!(func[func[addition].operands][1].reg, first, "the earlier load is still read");
        assert_eq!(func[func[addition].operands][2].reg, other, "and the later one is the address");
    }

    /// A call between the two. What a call does to memory is not in the instruction, so it is the
    /// same answer as the store and reached without asking about the address.
    #[test]
    fn a_load_with_a_call_between_it_and_its_reader_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let call = op(&mut names, "call");
        func.build(block, call).finish();
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64", "x64.call", "x64.add_rr_64"]);
    }

    /// Something writing the register the address reads. A physical register is the only one this
    /// can happen to while the IR is in SSA form, and the frame is addressed through two of them.
    #[test]
    fn a_load_whose_address_register_is_written_between_the_two_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RSP);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sub = op(&mut names, "sub_ri_64");
        func.build(block, sub)
            .operand(Operand::write(base, GPR).with(Constraint::Reuse(1)))
            .uses(base, GPR)
            .imm(32)
            .finish();
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
    }

    /// A load of four bytes under an addition of eight. The register held what the load put in it
    /// and a memory operand holds what is at the address, which is a different number of bytes.
    #[test]
    fn a_load_of_the_wrong_width_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let narrow = op(&mut names, "mov_rm_32");
        func.build(block, narrow).def(into, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        alu(&mut func, &mut names, block, "add_rr_64", other, into);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_32", "x64.add_rr_64"]);
    }

    /// A load whose value leaves the block on an edge. It is read by nothing in any operand vector
    /// and is read all the same, which is the count that is easy to get wrong.
    #[test]
    fn a_load_whose_value_an_edge_carries_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "add_rr_64", other, word);
        let arrived = func.new_vreg(GPR);
        func.params_mut(next).push(mir::Param { reg: arrived, class: GPR });
        *func.succs_mut(block) = vec![mir::BlockCall::with(next, vec![word])];

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64", "x64.add_rr_64"]);
    }

    /// A reader in another block, which is the whole of what block local means here.
    #[test]
    fn a_reader_in_another_block_stays_where_it_is() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, next, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64"]);
        assert_eq!(shape(&func, &names, next), ["x64.add_rr_64"]);
    }

    /// A reader further down the block than the window reaches.
    #[test]
    fn a_reader_past_the_window_stays_where_it_is() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let nop = op(&mut names, "nop");
        for _ in 0..WINDOW {
            func.build(block, nop).finish();
        }
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
    }

    /// And one instruction closer, which is the last place it still folds.
    #[test]
    fn a_reader_at_the_edge_of_the_window_folds() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let nop = op(&mut names, "nop");
        for _ in 0..WINDOW - 1 {
            func.build(block, nop).finish();
        }
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 1);
    }

    /// The entry a frame layout is waiting on moves with the load. Without this the displacement
    /// of a local would be written into an instruction that has gone.
    #[test]
    fn the_frame_entry_of_a_load_that_moves_goes_with_it() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RSP);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let reader = func.insts(block).nth(1);
        assert!(reader.is_none(), "the block holds the load alone so far");
        alu(&mut func, &mut names, block, "add_rr_64", other, word);
        let held = func.insts(block).next().expect("the load");

        let mut addresses = vec![(held, 3usize)];
        let mut arguments = Vec::new();
        let mut dynamic = Vec::new();
        let mut pending =
            Pending { addresses: &mut addresses, arguments: &mut arguments, dynamic: &mut dynamic };
        assert_eq!(loads(&mut func, &MACHINE, &mut names, &mut pending), 1);

        let inst = func.insts(block).next().expect("the addition");
        assert_eq!(addresses, [(inst, 3usize)], "the entry names the instruction that took it");
    }

    /// Every row of the table names instructions this target has, and names a load and an
    /// arithmetic whose widths agree. A row that got one of the three wrong would propose an
    /// instruction the change framework turns down, which is a fold that silently never happens.
    #[test]
    fn every_row_of_the_table_is_three_instructions_this_target_has() {
        for fold in FOLDS {
            assert!(MACHINE.has(fold.from), "{} is not an instruction", fold.from);
            assert!(MACHINE.has(fold.into), "{} is not an instruction", fold.into);
            assert!(MACHINE.has(fold.load), "{} is not an instruction", fold.load);
            let width = |name: &str| name.rsplit_once('_').map(|(_, width)| width.to_owned());
            assert_eq!(width(fold.from), width(fold.into), "{} changes width", fold.from);
            assert_eq!(width(fold.from), width(fold.load), "{} loads another width", fold.from);
            assert!((MACHINE.takes_mem)(fold.into), "{} reads no memory", fold.into);
            assert!(!(MACHINE.takes_mem)(fold.from), "{} already reads memory", fold.from);
        }
    }

    /// One row per arithmetic instruction the target has that could take one. The count is here so
    /// that an instruction added to the target without a row shows up as a number rather than as a
    /// fold nobody noticed was missing.
    #[test]
    fn the_table_covers_the_arithmetic_this_target_has() {
        assert_eq!(FOLDS.len(), 23, "six operations at four widths, less the eight bit multiply");
        let commuting = FOLDS.iter().filter(|fold| fold.commutes).count();
        assert_eq!(commuting, 19, "everything but the four subtractions");
    }
}
