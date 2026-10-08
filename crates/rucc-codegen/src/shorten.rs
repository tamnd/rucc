//! Writing the same answer in fewer bytes, once the registers are the real ones.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` section 37.4, which calls these the
//! size directed peepholes and puts them after register allocation. tamnd/rucc#741 is the issue
//! about the back end never learning what it is compiling for, and names three of these as free
//! before any of that is settled and one as waiting for it.
//!
//! Six rewrites. A move of zero into a register becomes an exclusive or of the register with
//! itself: `movl $0, %eax` spells the zero out in four bytes of zero bits and is five bytes, `xorl
//! %eax, %eax` says it without spelling it and is two. The processor knows the idiom, so the
//! shorter one is no slower, and this is not a trade of speed for size and does not wait for a size
//! goal to arrive.
//!
//! And a move of a number into a sixty-four bit register becomes the thirty-two bit move where the
//! number is one that fits, because the narrow instruction clears the half of the register it does
//! not write rather than leaving it alone. `movq $7, %rax` is seven bytes and `movl $7, %eax` is
//! five, and for a number above two to the thirty-first it is ten against five, since the wide move
//! cannot reach one by sign extending and writes all eight bytes of it out.
//!
//! The two meet on a zero, and the order they are asked in is the order they are worth: a zero
//! whose condition state is free becomes the exclusive or, and a zero whose state is not becomes
//! the narrow move, which is two bytes off rather than five but costs nothing to say.
//!
//! And a comparison of a register against zero becomes a test of the register against itself.
//! `cmpl $0, %eax` is three bytes and `testl %eax, %eax` is two, the byte being the zero the first
//! one writes out. That one is asked of every instruction whatever the walk has seen, because the
//! test writes the condition state exactly as the comparison does: both leave the sign, the zero
//! and the parity of what is in the register and both clear the carry and the overflow, so every
//! condition this machine jumps on reads the same answer behind either of them.
//!
//! And an addition of one to a register becomes the instruction that adds one and says so in its
//! opcode. `addl $1, %eax` is three bytes, one for the opcode, one saying which register and one
//! for the number, and `incl %eax` is two. A subtraction of one becomes the instruction that takes
//! one away, and each of the two is also what the other one written against minus one becomes.
//!
//! And an address computation whose address is a register becomes a move of that register. `leaq
//! (%rsp), %rax` works out an address that is a base and nothing else, which is what is already in
//! the base, and `movq %rsp, %rax` puts the same number in the same place in three bytes rather than
//! four. The byte is the one an address counted from the stack pointer has to spend saying it has no
//! index, and the stack pointer is the register this turns up on, because what makes it is taking
//! the address of whichever local sits at the bottom of the frame.
//!
//! That fifth one is the only one here worth taking for something other than bytes. A move between
//! registers is a thing the machine can do by renaming, so it is off the critical path, and an
//! address computation is an addition however small the numbers in it are. gcc writes no address
//! computation of that shape anywhere in the SQLite amalgamation and rucc wrote 444 of them.
//!
//! And a copy followed by an addition into the copy becomes one address computation. The addition
//! writes its answer over its first source, so where that source is still wanted the allocator
//! copies it first, and `movq %rcx, %rdi` then `addl %eax, %edi` is what comes out. `leal
//! (%rcx,%rax), %edi` is the same sum in one instruction, which is what gcc writes, and the salsa
//! core of libsodium had 48 of these pairs in one function where gcc had 31 address computations.
//! The address computation writes no condition state, so this waits on the state being dead the way
//! the exclusive or does. tamnd/rucc#2789 is the issue.
//!
//! That fourth one is the only one here that is not free, and it is the only one that reads the
//! goal. An addition writes the carry and an increment leaves the carry as it found it, so the
//! machine has to merge what was left with what the next instruction writes, which costs a little
//! where the code is hot and is worth a byte where the goal is size. gcc writes the addition at
//! `-O2` and the increment at `-Os`, and so does this. The goal arriving here at all is the first
//! half of tamnd/rucc#741: before it, `-Os` was a shorter list of middle end passes and the back
//! end compiled what came out of it exactly as `-O2` would have.
//!
//! The numbers over the corpus at `-Os` before this pass existed: rucc wrote a move of zero into a
//! register 21,304 times and GCC 16 wrote it 9 times, and GCC wrote the exclusive or 23,729 times
//! against rucc's 965. So this is not a case the selector catches most of and misses at the edges.
//! It is one it does not do. Afterwards rucc writes the move 1,232 times and the exclusive or
//! 21,037, and the two moved by the same number, which is what says every one that went became one
//! of these and none of them came from anywhere else.
//!
//! # Why it is not something the encoder does
//!
//! Because the two are not the same instruction. The exclusive or writes the condition state and
//! the move does not, so an encoder that quietly swapped one for the other would change what the
//! instruction behind it reads. Whether anything reads it is a question about the instructions that
//! follow rather than about this one, which is what makes this a pass. [`rucc_target::FlagInsts`]
//! is where the answer comes from, the same description [`crate::compare`] asks, and it answers
//! that a name it does not know writes the state, so an opcode added to a rule set and not to that
//! table makes this find less rather than making it wrong.
//!
//! The narrower move and the test are a different answer to the same question. Those two an encoder
//! could do without asking anything, since the narrower move leaves the same number in the same
//! register and the test leaves the same condition state the comparison left. They are not done
//! there because an encoder handed a sixty-four bit move and writing the bytes of a thirty-two bit
//! one would be writing bytes the listing beside them does not say, and the listing and the bytes
//! agreeing is worth more than the two bytes. Choosing the instruction is this pass and spelling
//! the one it chose is the encoder.
//!
//! # Why it runs last
//!
//! After [`crate::compare`], because that pass takes comparisons out and a comparison that is gone
//! is one whose write of the condition state is gone with it. Running before it would see a state
//! written where the output has none and would refuse rewrites that are allowed. After the layout
//! for the reason `compare` is: the layout writes the jump that reads a comparison into the same
//! block as the comparison, and this is the other pass that has to see that pair whole.
//!
//! Nothing here moves an instruction or changes a block, so running after the freeze costs nothing.
//! Every rewrite is one instruction becoming one instruction in the same place, and the one that
//! takes a copy in also takes the copy out, which leaves every instruction around it where it was.
//!
//! # What a block boundary is
//!
//! The end of everything this knows, which is the same sentence [`crate::compare`] uses and the
//! same reason: the condition state is not a register, nothing in this back end carries one from a
//! block to its successors, and the only place a comparison is read is the block it was made in.
//!
//! That is an invariant of the passes in front rather than of this one, so it is checked instead of
//! believed. `carried` walks every block and asks whether any of them reads the condition state
//! before writing it, which is what a block reading a predecessor's state would look like from
//! here, and one that does turns the whole function down. What it buys is that if some later pass
//! starts writing that shape, this pass stops rather than starts being wrong.
//!
//! Reads it rather than mentions it. A comparison that keeps a byte makes the comparison and reads
//! the answer in the one instruction, so a block opening with one is not a block reading anything a
//! predecessor left, and the description is asked which of the two kinds of read it is rather than
//! being taken at the word. tamnd/rucc#1432 is what that cost before it was asked: 456 functions in
//! the SQLite amalgamation were turned down and every one of them was turned down by this, which is
//! most of the functions in it that have anything for this pass to do.
//!
//! Down for the exclusive or and the increment. The narrower move reads no condition state and
//! writes none, the test writes the same state the comparison it replaces wrote, and neither an
//! address computation nor a move touches any state at all, so where a state is alive is not a
//! question those three have to ask, and a function this turns down still gets all of them.
//!
//! What the walk carries for the increment is a second answer beside the first, which is whether
//! anything behind reads the carry rather than whether anything behind reads the state. The two are
//! not the same question and neither implies the other: a jump on whether a value was zero reads the
//! state and not the carry, and an increment already in the code writes the state and not the carry
//! and so ends the life of neither. That last case is the reason the walk and the check above both
//! ask the target which instructions leave the carry alone rather than stopping at the flag saying
//! the state was written.
//!
//! # A template a program wrote
//!
//! An `asm` statement is not opaque to this. `rucc_target::x86_64::read` turns the text of a
//! template into the opcodes this back end already has, so by the time this runs a template is
//! ordinary instructions carrying ordinary names, and the ones in it that read the condition state
//! are seen the same way any other instruction's read is. A move of zero in front of a template is
//! rewritten when nothing in that template reads a state it did not write itself, which is the same
//! rule as everywhere else and not a rule about templates.
//!
//! Nothing weaker is being assumed there than what a program could already rely on. On this machine
//! GCC has every `asm` clobber the condition state whether the statement said so or not, so a
//! template reading one set before it was never something to hold on to.
//!
//! # What it will not do
//!
//! Turn a move into the exclusive or when anything reads its condition state before anything
//! writes. That is the rule and what it costs is now a small number: the zero going into a register
//! right before a comparison of something else stays a move, and the most the other rewrite can do
//! for it is make it a narrower one. Of the 215 moves of zero left over the corpus at `-Os`, 146
//! are this and the other 69 are the eight bit rule below. Not one of them is sixty-four bits wide.
//!
//! It was 1,232 until the whole function check stopped counting a comparison that keeps a byte as a
//! state read from in front of it, which is tamnd/rucc#1432 and was most of what this pass was
//! leaving alone rather than anything about the instructions it was looking at.
//!
//! Eight bits. `movb $0, %al` and `xorb %al, %al` are both two bytes, so the exchange buys nothing
//! and would spend the condition state on it. The target's table is where that is written down.
//!
//! Write an increment at a level that asked for fast code. That is the goal doing its job rather
//! than a limit, and it is why the same corpus compiled at `-O2` and at `-Os` now differs by
//! something other than which middle end passes ran.
//!
//! Add or take away anything but one. The machine has an opcode for one and for nothing else, so a
//! constant of two is already as short as it is going to be written.
//!
//! Turn an address computation into a move when the address is anything more than a register. An
//! index is a multiplication, a constant is an addition and a symbol is an address the assembler
//! fills in, and a move does none of those. That is what most address computations are for, so this
//! last rewrite is about the ones that were not computing anything rather than about address
//! computation in general.

use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_cost::Goal;
use rucc_mir::{self as mir, Role};
use rucc_target::{FlagInsts, MachineInsts, Reads, ShortInsts};

use crate::changes::{self, Changes, Plan};

/// Rewrites every instruction that has a shorter spelling nothing would notice.
///
/// Gives back how many were rewritten, which the tests read and nothing else does.
///
/// `allocated` says which instructions are moves the allocator wrote, which are the only copies the
/// sixth rewrite takes out. A move a template wrote is one the program asked for by name, and the
/// listing of the template has to keep it whatever it does.
pub fn shorter(
    func: &mut mir::Func,
    short: &ShortInsts,
    flags: &FlagInsts,
    machine: &MachineInsts,
    names: &mut Interner,
    goal: Goal,
    allocated: &dyn Fn(mir::Inst) -> bool,
) -> usize {
    // Every name a rewrite could want, before the walk rather than inside it, because the walk
    // holds a name it read out of the interner while it edits the function and interning a new one
    // there would be the same interner borrowed twice. The same reason [`crate::compare`] has.
    let wanted = short.zeroing.iter().map(|entry| entry.into);
    let wanted = wanted.chain(short.narrowing.iter().map(|entry| entry.into));
    let wanted = wanted.chain(short.testing.iter().map(|entry| entry.into));
    let wanted = wanted.chain(short.stepping.iter().map(|entry| entry.into));
    let wanted = wanted.chain(short.copying.iter().map(|entry| entry.into));
    let wanted = wanted.chain(short.spreading.iter().map(|entry| entry.into));
    let opcodes: Vec<(&'static str, mir::Opcode)> =
        wanted.map(|into| (into, mir::Opcode::new(names.join(short.prefix, into)))).collect();
    let names = &*names;
    let mut counts = changes::Reads::of(func);
    let mut took = 0;
    let mut seen = Map::default();
    // Whether the rewrite that spends the condition state may be asked for at all. The narrower
    // instruction neither reads the state nor writes it, so it is not asked this and a function
    // this turns down still gets that one.
    let free = !carried(func, short, flags, names, &mut seen);
    // Whether the rewrite that trades the carry for a byte may be asked for. It is the one thing
    // here that is not free, so it waits for a level that said it wanted small code.
    let small = free && goal == Goal::Size;
    for block in func.blocks().collect::<Vec<_>>() {
        // Backwards, because the question each instruction asks is about the ones behind it. The
        // state is dead at the end of a block, which is the invariant [`carried`] has just held the
        // function to.
        let mut live = false;
        // The same question about the carry alone, which is the part of the state the shorter
        // addition does not write. It starts false for the reason `live` does and moves separately,
        // because an instruction that writes the whole state ends the life of both and one that
        // writes everything but the carry ends the life of neither.
        let mut carry = false;
        let insts = func.insts(block).collect::<Vec<_>>();
        for (at, &inst) in insts.iter().enumerate().rev() {
            // A copy an addition behind it has taken in, which is gone from the block. It wrote no
            // condition state, so walking past it is what the walk would have done anyway.
            if func.block_of(inst).is_none() {
                continue;
            }
            // Asked again after each rewrite that is taken, since what stands there then is another
            // instruction and the questions below are about that one.
            let mut known = Known::of(&mut seen, func, short, flags, names, inst);
            if free && !live && known.zeroing {
                let into = shorter_form(func, short, names, &opcodes, inst);
                if into.is_some_and(|op| zeroed(func, &mut counts, machine, names, inst, op)) {
                    took += 1;
                    // What stands there now writes the state, and the state was already dead, so
                    // nothing about what the instructions in front of it may do has changed.
                    continue;
                }
            }
            // The zero that could not become an exclusive or can still be written in fewer bytes,
            // which is why this is asked after that one and not instead of it.
            let into = if known.narrowing {
                narrower_form(func, short, names, &opcodes, inst)
            } else {
                None
            };
            if into.is_some_and(|op| narrowed(func, &mut counts, machine, names, inst, op)) {
                took += 1;
                known = Known::of(&mut seen, func, short, flags, names, inst);
                // What stands there now is the same instruction at half the width, which is a
                // move either way, so what it does to the state is what it did before: nothing.
            }
            // A comparison against zero asked of the register alone. Nothing about where the state
            // is live comes into it, because the shorter instruction writes the same five bits of
            // state the comparison wrote, so this is asked of every instruction whatever the walk
            // has seen behind it.
            let into =
                if known.testing { tested_form(func, short, names, &opcodes, inst) } else { None };
            if into.is_some_and(|op| tested(func, &mut counts, machine, names, inst, op)) {
                took += 1;
                known = Known::of(&mut seen, func, short, flags, names, inst);
            }
            // An address that is a register, written as the move it is. Nothing about the condition
            // state comes into it either, since neither instruction writes any, so this is asked of
            // every instruction the same way the narrower move is.
            let into =
                if known.copying { copied_form(func, short, names, &opcodes, inst) } else { None };
            if into.is_some_and(|op| copied(func, &mut counts, machine, names, inst, op)) {
                took += 1;
                known = Known::of(&mut seen, func, short, flags, names, inst);
            }
            // A copy and the addition behind it, written as one address computation. That writes
            // no condition state where the addition wrote all of it, so this waits on the state
            // being dead the way the exclusive or does. It is asked before the step because the one
            // instruction it leaves is shorter than the copy and the increment together.
            if free && !live && known.spreading {
                let before = &insts[..at];
                let into =
                    spread_form(func, short, machine, names, &opcodes, before, inst, allocated);
                if into.is_some_and(|spread| spread.commit(func, &mut counts, names, machine)) {
                    took += 1;
                    known = Known::of(&mut seen, func, short, flags, names, inst);
                    // What stands there now writes no state and the state was already dead, so
                    // the walk past it is the one it would have taken past the addition.
                }
            }
            // Adding one with the one in the opcode, which is the only rewrite here that is a
            // trade. It needs the carry to be dead rather than the whole state, since that is the
            // only part of the state the shorter instruction leaves behind, and it needs the level
            // to have asked for small code.
            if small && !carry && known.stepping {
                let into = stepped_form(func, short, names, &opcodes, inst);
                if into.is_some_and(|op| stepped(func, &mut counts, machine, names, inst, op)) {
                    took += 1;
                    known = Known::of(&mut seen, func, short, flags, names, inst);
                    // What stands there now writes everything but the carry, and the carry was
                    // already dead, so both answers below are the ones they already are and the
                    // walk past it is the walk it would have taken anyway.
                }
            }
            if !known.covered {
                // A name the description does not cover may have read the state and may have
                // written it, and the answer that finds fewer rewrites is that it read it.
                live = true;
                carry = true;
                continue;
            }
            // What it reads before whether it writes, because an instruction can do both and the
            // read it does is a read of what is there now. An add with carry is the one that does,
            // and asking the other way round would call it the end of the state's life and let a
            // rewrite in front of it take the carry away.
            //
            // Unless what it reads is what it wrote itself. A comparison that keeps a byte makes
            // the comparison and reads the answer in the one instruction, so the state it was
            // handed is state it wrote over before anything looked at it, and it ends a life rather
            // than extending one. Asking the description which kind it is rather than stopping at
            // the word read is most of what this pass gets to do in real code, since a C function
            // of any size has one of these in it.
            if known.own {
                live = false;
                carry = false;
            } else if let Some(reads) = known.reads {
                live = true;
                // Which part of the state the condition on it is about. A condition that asks where
                // a value sits as an unsigned number reads the carry, and so does an instruction
                // that is adding a carry on rather than asking a question about one.
                if matches!(reads, Reads::Unsigned | Reads::Carry) {
                    carry = true;
                }
            } else if known.ends {
                // An instruction that writes the state ends the life of everything in it. One that
                // writes all of it but the carry ends the life of none of it, which is the second
                // half of the sentence and is why this asks the description rather than stopping at
                // the flag. That answer is about `live` as much as about `carry`: a rewrite in front
                // that spends the state would be spending a carry this instruction was going to
                // leave for something behind it.
                live = false;
                carry = false;
            }
        }
    }
    took
}

/// Whether any block reads the condition state before writing it, which is what a state carried in
/// from a predecessor would look like from inside this pass.
///
/// The passes in front are the ones that promise this does not happen and the promise is theirs to
/// keep, so what this does is hold them to it rather than restate it. A function where it is broken
/// gets no rewrites at all, which is the answer that is wrong about nothing.
///
/// An instruction that leaves the carry alone does not count as having written the state here, for
/// the same reason it does not count as having written it in the walk. A block opening with one and
/// reading a carry afterwards is reading a carry a predecessor left, which is exactly the shape this
/// is looking for, and stopping at it would be calling that block clean.
///
/// An instruction that reads what it wrote itself does not count as having read the state, for the
/// same reason it does not in the walk. A block opening with a comparison that keeps a byte opens
/// with a comparison, and what the comparison found is not what anything in front of it left.
/// Counting it as a read is the difference between this turning down a few functions and turning
/// down most of them, because a comparison that keeps a byte is what every `!` and every `==` in a
/// value position comes out as.
fn carried(
    func: &mir::Func,
    short: &ShortInsts,
    flags: &FlagInsts,
    names: &Interner,
    seen: &mut Map<mir::Opcode, Known>,
) -> bool {
    func.blocks().any(|block| {
        for inst in func.insts(block) {
            let known = Known::of(seen, func, short, flags, names, inst);
            if !known.covered {
                return true;
            }
            if known.reads.is_some() && !known.own {
                return true;
            }
            if known.ends {
                return false;
            }
        }
        false
    })
}

/// What the description says about one opcode, asked once for each opcode a function has rather
/// than once for each instruction.
///
/// Every answer here is a walk down one of the target's tables comparing names, and the walk above
/// asks most of them of every instruction while nearly every answer is no. A function has a few
/// hundred opcodes at most, so each one is asked about the first time it turns up and read back
/// after that. On jtckdint's `test.c` at `-O2` the comparing of names in this pass was about two and
/// a half percent of the build.
///
/// The forms are only whether the table has an entry for the name. Whether the instruction carries
/// the number or the address the shorter one needs is the instruction's own business and is still
/// asked of it.
#[derive(Debug, Clone, Copy)]
struct Known {
    /// Whether it has a shorter way of writing zero.
    zeroing: bool,
    /// Whether it has a narrower instruction.
    narrowing: bool,
    /// Whether it has a shorter way of comparing against zero.
    testing: bool,
    /// Whether it has a move that says the same thing.
    copying: bool,
    /// Whether it has a shorter addition for some number.
    stepping: bool,
    /// Whether it is an addition a copy in front of it can be folded into.
    spreading: bool,
    /// Whether the description covers the name at all. See [`opcode`].
    covered: bool,
    /// Whether it reads what it wrote itself. See [`FlagInsts::asks_what_it_reads`].
    own: bool,
    /// Which part of the condition state it reads.
    reads: Option<Reads>,
    /// Whether it writes the whole of the condition state, the carry included.
    ends: bool,
}

impl Known {
    fn of(
        seen: &mut Map<mir::Opcode, Self>,
        func: &mir::Func,
        short: &ShortInsts,
        flags: &FlagInsts,
        names: &Interner,
        inst: mir::Inst,
    ) -> Self {
        *seen.entry(func[inst].opcode).or_insert_with(|| {
            let bare = names.resolve(func[inst].opcode.name()).strip_prefix(short.prefix);
            let has =
                |name: fn(&ShortInsts, &str) -> bool| bare.is_some_and(|bare| name(short, bare));
            let name = opcode(func, flags, names, inst);
            Self {
                zeroing: has(|short, bare| short.zeroed(bare).is_some()),
                narrowing: has(|short, bare| short.narrowed(bare).is_some()),
                testing: has(|short, bare| short.tested(bare).is_some()),
                copying: has(|short, bare| short.copied(bare).is_some()),
                stepping: has(|short, bare| short.stepping.iter().any(|entry| entry.name == bare)),
                spreading: has(|short, bare| short.spread(bare).is_some()),
                covered: name.is_some(),
                own: name.is_some_and(|name| flags.asks_what_it_reads(name)),
                reads: name.and_then(|name| flags.reads(name)),
                ends: name.is_some_and(|name| (flags.writes)(name) && !short.steps(name)),
            }
        })
    }
}

/// The shorter instruction this one has, when it has one and the constant it carries is the one
/// that instruction writes.
///
/// The name says which instruction it is and the description says which names have a shorter
/// spelling, and neither of them says what number this one holds. That is the half that decides
/// whether the shorter spelling says the same thing, since the short way of writing zero is only
/// the short way of writing zero.
fn shorter_form(
    func: &mir::Func,
    short: &ShortInsts,
    names: &Interner,
    opcodes: &[(&'static str, mir::Opcode)],
    inst: mir::Inst,
) -> Option<mir::Opcode> {
    let name = names.resolve(func[inst].opcode.name()).strip_prefix(short.prefix)?;
    let into = short.zeroed(name)?;
    if func[inst].imm.map(|at| func[at].0) != Some(0) {
        return None;
    }
    opcodes.iter().find(|&&(at, _)| at == into).map(|&(_, opcode)| opcode)
}

/// The narrower instruction this one has, when it has one and the number it carries is one that
/// instruction holds.
///
/// A number the narrower instruction cannot hold is every negative one and everything above what
/// fits in the bits it writes, since what it does to the rest of the register is clear it. So the
/// question is not whether the number fits in that many bits the way the program meant it, which is
/// a question about a type, but whether the bits the wide instruction would leave in the register
/// are the bits the narrow one leaves there, which is a question about the number.
fn narrower_form(
    func: &mir::Func,
    short: &ShortInsts,
    names: &Interner,
    opcodes: &[(&'static str, mir::Opcode)],
    inst: mir::Inst,
) -> Option<mir::Opcode> {
    let name = names.resolve(func[inst].opcode.name()).strip_prefix(short.prefix)?;
    let narrow = short.narrowed(name)?;
    let held = u64::try_from(func[func[inst].imm?].0).ok()?;
    if narrow.writes >= u64::BITS || held >= 1u64 << narrow.writes {
        return None;
    }
    opcodes.iter().find(|&&(at, _)| at == narrow.into).map(|&(_, opcode)| opcode)
}

/// Rewrites the move into the narrower move, which is the same instruction with a different name.
///
/// So the operands are the ones it had, where the exclusive or below needs its own built: the two
/// moves take a register they write and a number, and the number is the one that was already there.
/// A description where that is not so is one [`Changes`] turns down, and a rewrite it turns down is
/// one this reports as not taken rather than one that goes in anyway.
fn narrowed(
    func: &mut mir::Func,
    counts: &mut changes::Reads,
    machine: &MachineInsts,
    names: &Interner,
    inst: mir::Inst,
    opcode: mir::Opcode,
) -> bool {
    let mut set = Changes::new();
    set.rewrite(inst, Plan { opcode, ..Plan::of(func, inst) });
    set.commit(func, counts, names, machine).is_ok()
}

/// The shorter comparison this one has, when it has one and the constant it carries is zero.
///
/// Zero is the whole of it. A comparison of a register against itself asks whether the register is
/// zero and nothing else, so the description's entry says what to write instead of a comparison
/// against zero and says nothing about a comparison against anything, and an instruction carrying
/// any other number is one this walks past.
fn tested_form(
    func: &mir::Func,
    short: &ShortInsts,
    names: &Interner,
    opcodes: &[(&'static str, mir::Opcode)],
    inst: mir::Inst,
) -> Option<mir::Opcode> {
    let name = names.resolve(func[inst].opcode.name()).strip_prefix(short.prefix)?;
    let into = short.tested(name)?;
    if func[inst].imm.map(|at| func[at].0) != Some(0) {
        return None;
    }
    opcodes.iter().find(|&&(at, _)| at == into).map(|&(_, opcode)| opcode)
}

/// Rewrites the comparison into the test, which reads the register the comparison read and drops
/// the constant.
///
/// The operands are the ones it had, for the reason [`narrowed`] keeps them: both instructions name
/// one register and read it, and what changes is the number, which the shorter one does not carry.
/// So the constant goes and nothing else does. A description where the two are not that shape is one
/// [`Changes`] turns down, and this reports a rewrite it turned down as not taken.
fn tested(
    func: &mut mir::Func,
    counts: &mut changes::Reads,
    machine: &MachineInsts,
    names: &Interner,
    inst: mir::Inst,
    opcode: mir::Opcode,
) -> bool {
    let mut set = Changes::new();
    set.rewrite(inst, Plan { opcode, imm: None, ..Plan::of(func, inst) });
    set.commit(func, counts, names, machine).is_ok()
}

/// The move this address computation is, when the address it works out is a register.
///
/// Which is an addressing mode naming a base and nothing else. An index is a multiplication and an
/// addition, a constant is an addition, and a symbol or a label is an address the assembler fills in
/// later, so any of those is work the move does not do. What is left is a mode that says to take
/// what is in one register, and taking what is in one register is the move.
///
/// The width is not asked about, unlike the narrower move above. An address on this machine is
/// sixty four bits wide whatever is at it, so an address computation that keeps its answer keeps all
/// of it, and the move the description names beside it is the move of that width.
fn copied_form(
    func: &mir::Func,
    short: &ShortInsts,
    names: &Interner,
    opcodes: &[(&'static str, mir::Opcode)],
    inst: mir::Inst,
) -> Option<mir::Opcode> {
    let name = names.resolve(func[inst].opcode.name()).strip_prefix(short.prefix)?;
    let into = short.copied(name)?;
    let amode = func[inst].mem.map(|at| func[at])?;
    if amode.base.is_none() || amode.index.is_some() || amode.disp != 0 {
        return None;
    }
    if amode.symbol.is_some() || amode.block.is_some() || amode.table.is_some() {
        return None;
    }
    if amode.segment.is_some() {
        return None;
    }
    opcodes.iter().find(|&&(at, _)| at == into).map(|&(_, opcode)| opcode)
}

/// Rewrites the address computation into the move, which keeps the operands and drops the mode.
///
/// The operands are already the move's. An instruction with an addressing mode carries the registers
/// that mode names in its operand vector, behind the ones it writes, so an address computation whose
/// mode is one base is an instruction that writes one register and reads one register, in that
/// order, which is the move's shape. What goes is the mode itself, since the move has none.
///
/// A description where those two are not the same shape is one [`Changes`] turns down, and this
/// reports a rewrite it turned down as not taken, which is how an address computation with more in
/// its operand vector than the mode accounted for is left alone rather than guessed at.
fn copied(
    func: &mut mir::Func,
    counts: &mut changes::Reads,
    machine: &MachineInsts,
    names: &Interner,
    inst: mir::Inst,
    opcode: mir::Opcode,
) -> bool {
    let mut set = Changes::new();
    set.rewrite(inst, Plan { opcode, amode: None, ..Plan::of(func, inst) });
    set.commit(func, counts, names, machine).is_ok()
}

/// Rewrites the move into the exclusive or, which names the one register the move wrote in every
/// operand it has.
///
/// The shapes come from the description rather than from the move, since the shorter instruction is
/// not the shape the longer one was: the exclusive or writes a register it also reads, which on this
/// machine is an operand constrained to the same place as one of the reads, and a plan whose
/// operands do not say so is one [`Changes`] turns down. So each operand is built to what the
/// description asks for and the register in it is the one the move wrote, which after allocation is
/// a physical register and so is a register every operand can name without anything being arranged.
/// The constant goes with the move, the shorter instruction being the one that carries none.
///
/// A description whose operands are not all of the register's class, or which writes more than the
/// one register or none, is a description this does not fit, and the answer there is to leave the
/// instruction alone rather than to guess.
fn zeroed(
    func: &mut mir::Func,
    counts: &mut changes::Reads,
    machine: &MachineInsts,
    names: &Interner,
    inst: mir::Inst,
    opcode: mir::Opcode,
) -> bool {
    let written: Vec<mir::Operand> = func[func[inst].operands]
        .iter()
        .filter(|operand| operand.role != Role::Use)
        .copied()
        .collect();
    let [def] = written[..] else { return false };
    let bare = machine.bare(names.resolve(opcode.name()));
    let Some(desc) = (machine.operands)(bare) else { return false };
    if desc.iter().any(|want| want.class != def.class) {
        return false;
    }
    if desc.iter().filter(|want| want.role != Role::Use).count() != 1 {
        return false;
    }
    let operands = desc
        .iter()
        .map(|want| mir::Operand {
            reg: def.reg,
            class: want.class,
            role: want.role,
            constraint: want.constraint,
        })
        .collect();
    let mut set = Changes::new();
    set.rewrite(inst, Plan { opcode, operands, imm: None, ..Plan::of(func, inst) });
    set.commit(func, counts, names, machine).is_ok()
}

/// The shorter addition this one has, when it has one and the number it carries is the number that
/// shorter instruction is about.
///
/// Both halves again, and the second one is doing more work here than anywhere else in this pass.
/// One addition has two shorter instructions, one for each of the two numbers a machine has an
/// opcode for, and a subtraction has the same two the other way round, so the number is what says
/// which of the two is meant rather than only whether either is.
fn stepped_form(
    func: &mir::Func,
    short: &ShortInsts,
    names: &Interner,
    opcodes: &[(&'static str, mir::Opcode)],
    inst: mir::Inst,
) -> Option<mir::Opcode> {
    let name = names.resolve(func[inst].opcode.name()).strip_prefix(short.prefix)?;
    let into = short.stepped(name, func[func[inst].imm?].0)?;
    opcodes.iter().find(|&&(at, _)| at == into).map(|&(_, opcode)| opcode)
}

/// Rewrites the addition into the one that carries its number in its opcode.
///
/// The operands are the ones it had, for the reason [`tested`] keeps them: both instructions write
/// one register and read that same register, and what changes is the number, which the shorter one
/// does not carry. So the constant goes and nothing else does.
fn stepped(
    func: &mut mir::Func,
    counts: &mut changes::Reads,
    machine: &MachineInsts,
    names: &Interner,
    inst: mir::Inst,
    opcode: mir::Opcode,
) -> bool {
    let mut set = Changes::new();
    set.rewrite(inst, Plan { opcode, imm: None, ..Plan::of(func, inst) });
    set.commit(func, counts, names, machine).is_ok()
}

/// How far in front of an addition the copy it takes in may be.
///
/// The allocator writes the copy right in front of the instruction that wanted it and the schedule
/// moves it no further than the instructions it is independent of, which in the salsa core of
/// libsodium is never more than three. A bound keeps the walk linear in a block of additions whose
/// copies are all somewhere else.
const SPREAD: usize = 8;

/// A copy and an addition that are one address computation, found and not yet written.
struct Spread {
    /// The copy, which goes.
    copy: mir::Inst,
    /// The addition, which becomes the address computation.
    inst: mir::Inst,
    /// What it becomes.
    plan: Plan,
}

impl Spread {
    /// Writes the address computation over the addition and takes the copy out.
    ///
    /// The copy goes by hand rather than as a removal in the same set. [`Changes`] keeps a removal
    /// to an instruction whose register nothing reads, which after allocation is a question about a
    /// physical register the whole function reads somewhere, so it would turn every one of these
    /// down. What stands for the question here is the walk in [`spread_form`], which found nothing
    /// between the two reading the register the copy wrote, and the addition was the last reader of
    /// it, since it writes the register over. The count of reads of the register the copy read is
    /// one too many afterwards, which only ever makes a later removal wait.
    fn commit(
        self,
        func: &mut mir::Func,
        counts: &mut changes::Reads,
        names: &Interner,
        machine: &MachineInsts,
    ) -> bool {
        let mut set = Changes::new();
        set.rewrite(self.inst, self.plan);
        if set.commit(func, counts, names, machine).is_err() {
            return false;
        }
        func.remove_inst(self.copy);
        true
    }
}

/// The address computation an addition and the copy in front of it are, when there is a copy and
/// nothing between the two minds it going.
///
/// The addition writes its first source and the copy put what it is adding to there, so the address
/// computation reads what the copy read in place of the first source and reads the second source or
/// carries the constant as it did. The allocator leaves the first source naming the register the
/// copy read rather than the one it wrote, since the two are tied and the copy is what ties them, so
/// either is taken as the addition reading what the copy put there. What has to hold between the two is that nothing reads or writes
/// the register the copy wrote, since that register holds something else once the copy goes, and
/// that nothing writes the register the copy read, since the address computation reads it later than
/// the copy did. The copy has to be one the allocator wrote, which is what `allocated` says. A call or a name the target does not know ends the search, because what either of
/// them writes is not all in its operands.
///
/// A register the addressing mode cannot take as an index goes in the base, and a sum of two of
/// those is left as it is. A constant goes in as the number the address adds, negated for a
/// subtraction, and one that does not fit is left as it is unless the address keeps no more than
/// thirty two bits, where only the low thirty two bits of it were ever going to count.
#[expect(
    clippy::too_many_arguments,
    reason = "the tables, the block so far and the allocator's mark are each read once"
)]
fn spread_form(
    func: &mir::Func,
    short: &ShortInsts,
    machine: &MachineInsts,
    names: &Interner,
    opcodes: &[(&'static str, mir::Opcode)],
    before: &[mir::Inst],
    inst: mir::Inst,
    allocated: &dyn Fn(mir::Inst) -> bool,
) -> Option<Spread> {
    let name = names.resolve(func[inst].opcode.name()).strip_prefix(short.prefix)?;
    let entry = short.spread(name)?;
    let opcode = opcodes.iter().find(|&&(at, _)| at == entry.into).map(|&(_, opcode)| opcode)?;
    if func[inst].mem.is_some() {
        return None;
    }
    let constant = func[inst].imm.map(|at| func[at].0);
    let (def, first, second) = match (&func[func[inst].operands], constant) {
        (&[def, first], Some(_)) => (def, first.reg, None),
        (&[def, first, second], None) => (def, first.reg, Some(second.reg)),
        _ => return None,
    };
    if !def.role.is_def() || second == Some(def.reg) {
        return None;
    }
    let disp = match constant {
        Some(value) => displacement(i128::from(value) * i128::from(entry.sign), entry.bits)?,
        None => 0,
    };
    let touches =
        |operand: &mir::Operand, reg: mir::Reg| operand.reg == reg && operand.class == def.class;
    // What the instructions between write, which is only known to matter once the copy has said
    // which register it read.
    let mut written: Vec<mir::Reg> = Vec::new();
    let mut found = None;
    for &at in before.iter().rev().filter(|&&at| func.block_of(at).is_some()).take(SPREAD) {
        let data = &func[at];
        let full = names.resolve(data.opcode.name());
        if machine.calls(full) || !machine.has(full) {
            return None;
        }
        let operands = &func[data.operands];
        let copy = full.strip_prefix(short.prefix).is_some_and(|bare| entry.copies.contains(&bare));
        if copy && data.imm.is_none() && data.mem.is_none() && allocated(at) {
            if let &[to, from] = operands {
                let shaped = to.role.is_def() && from.role == Role::Use && from.class == def.class;
                let feeds = first == def.reg || first == from.reg;
                if shaped && feeds && touches(&to, def.reg) && from.reg != def.reg {
                    found = Some((at, from.reg));
                    break;
                }
            }
        }
        if operands.iter().any(|operand| touches(operand, def.reg)) {
            return None;
        }
        written.extend(
            operands
                .iter()
                .filter(|operand| operand.role.is_def() && operand.class == def.class)
                .map(|operand| operand.reg),
        );
    }
    let (copy, source) = found?;
    if written.contains(&source) || func.cfi_after(copy).next().is_some() {
        return None;
    }
    let unindexed = |reg: mir::Reg| reg.phys().is_some_and(|reg| short.unindexed.contains(&reg));
    let (base, index) = match second {
        None => (source, None),
        Some(second) if !unindexed(second) => (source, Some(second)),
        Some(_) if unindexed(source) => return None,
        Some(second) => (second, Some(source)),
    };
    let bare = machine.bare(names.resolve(opcode.name()));
    let &[want] = (machine.operands)(bare)? else { return None };
    if want.class != def.class {
        return None;
    }
    let written = mir::Operand {
        reg: def.reg,
        class: want.class,
        role: want.role,
        constraint: want.constraint,
    };
    let mut operands = vec![written, mir::Operand::read(base, def.class)];
    operands.extend(index.map(|index| mir::Operand::read(index, def.class)));
    let amode = mir::Amode {
        base: Some(1),
        index: index.map(|_| 2),
        scale: 1,
        disp,
        ..mir::Amode::NOTHING
    };
    let plan = Plan { opcode, operands, imm: None, amode: Some(amode), symbol: None };
    Some(Spread { copy, inst, plan })
}

/// The number an address adds, for a constant an addition of that many bits carries.
///
/// The constant itself where it fits. Where it does not and the addition keeps no more than thirty
/// two bits, the constant with the same low thirty two bits, which is the same sum in every bit the
/// instruction keeps.
fn displacement(value: i128, bits: u32) -> Option<i32> {
    if let Ok(disp) = i32::try_from(value) {
        return Some(disp);
    }
    if bits > 32 {
        return None;
    }
    let low = value.rem_euclid(1 << 32);
    i32::try_from(if low >= 1 << 31 { low - (1 << 32) } else { low }).ok()
}

/// The name this target knows an instruction by, for an instruction that is one of this target's.
///
/// The opcode in machine IR carries the target's prefix, because a function in the middle of being
/// compiled holds instructions of one machine and the prefix is what says which. Anything without
/// it is not something this description covers, and the walk treats that as knowing nothing rather
/// than as knowing it is safe.
fn opcode<'a>(
    func: &mir::Func,
    flags: &FlagInsts,
    names: &'a Interner,
    inst: mir::Inst,
) -> Option<&'a str> {
    names.resolve(func[inst].opcode.name()).strip_prefix(flags.prefix)
}

#[cfg(test)]
mod tests {
    use rucc_target::x86_64::{FLAGS, GPR, MACHINE, SHORT};

    use super::*;

    /// A function with one block, and the names it was built with.
    fn empty() -> (Interner, mir::Func, mir::Block) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> mir::Opcode {
        mir::Opcode::new(names.intern(&format!("{}{name}", SHORT.prefix)))
    }

    /// The pass, over the machine this crate has a backend for, at a level that wanted fast code.
    fn takes(func: &mut mir::Func, names: &mut Interner) -> usize {
        shorter(func, &SHORT, &FLAGS, &MACHINE, names, Goal::Speed, &|_| true)
    }

    /// The same pass at a level that wanted small code, which is the only one that steps.
    fn small(func: &mut mir::Func, names: &mut Interner) -> usize {
        shorter(func, &SHORT, &FLAGS, &MACHINE, names, Goal::Size, &|_| true)
    }

    /// What every instruction in a block came to, as opcodes with the target's prefix taken off.
    fn shape(func: &mir::Func, names: &Interner, block: mir::Block) -> Vec<String> {
        func.insts(block)
            .map(|inst| {
                names
                    .resolve(func[inst].opcode.name())
                    .strip_prefix(SHORT.prefix)
                    .unwrap_or("")
                    .to_owned()
            })
            .collect()
    }

    /// The destination of a two-address instruction, which the description constrains to the same
    /// register as the first source. The builder's own `def` leaves the constraint off, and the
    /// change framework holds a rewrite to the shape the target asks for, so a test that built one
    /// without it would be a test of a function the allocator could not have produced.
    fn reuse(reg: mir::Reg) -> mir::Operand {
        mir::Operand {
            reg,
            class: GPR,
            role: Role::Def,
            constraint: rucc_mir::Constraint::Reuse(1),
        }
    }

    /// The registers an instruction names, in the order its operands do.
    fn regs(func: &mir::Func, inst: mir::Inst) -> Vec<mir::Reg> {
        func[func[inst].operands].iter().map(|operand| operand.reg).collect()
    }

    /// The number an instruction carries, for an instruction that carries one.
    fn imm(func: &mir::Func, inst: mir::Inst) -> Option<i64> {
        func[inst].imm.map(|at| func[at].0)
    }

    /// The shape the pass is for: a move of zero with nothing reading the condition state after it
    /// becomes the exclusive or, which names the register it writes in all three of its operands and
    /// carries no constant.
    #[test]
    fn a_move_of_zero_becomes_an_exclusive_or() {
        let (mut names, mut func, block) = empty();
        let into = func.new_vreg(GPR);
        let zero = op(&mut names, "mov_ri_32");
        let inst = func.build(block, zero).def(into, GPR).imm(0).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["xor_rr_32"]);
        assert_eq!(regs(&func, inst), [into, into, into]);
        assert!(func[inst].imm.is_none());
    }

    /// Sixty-four bits is the same rewrite and the biggest one, since the long way of writing a zero
    /// there is seven bytes. The instruction it becomes is the thirty-two bit one, which clears the
    /// half of the register it does not write and so leaves the same sixty-four bit zero in one
    /// byte less.
    #[test]
    fn sixty_four_bits_is_the_same_rewrite_at_half_the_width() {
        let (mut names, mut func, block) = empty();
        let into = func.new_vreg(GPR);
        let zero = op(&mut names, "mov_ri_64");
        func.build(block, zero).def(into, GPR).imm(0).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["xor_rr_32"]);
    }

    /// The other rewrite. A number that is not zero has nothing shorter than a move, and the move
    /// that writes half the register is shorter than the one that writes all of it.
    #[test]
    fn a_number_a_narrower_move_holds_is_written_by_the_narrower_move() {
        for value in [1, 7, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff] {
            let (mut names, mut func, block) = empty();
            let into = func.new_vreg(GPR);
            let wide = op(&mut names, "mov_ri_64");
            let inst = func.build(block, wide).def(into, GPR).imm(value).finish();

            assert_eq!(takes(&mut func, &mut names), 1, "{value}");
            assert_eq!(shape(&func, &names, block), ["mov_ri_32"], "{value}");
            assert_eq!(imm(&func, inst), Some(value), "{value}");
            assert_eq!(regs(&func, inst), [into], "{value}");
        }
    }

    /// A number the narrower move does not hold, which is everything above what fits in the bits it
    /// writes and every negative number, since what it does to the rest of the register is clear it
    /// rather than fill it with the sign.
    #[test]
    fn a_number_the_narrower_move_does_not_hold_stays_wide() {
        for value in [-1, -7, 0x1_0000_0000, i64::MIN, i64::MAX] {
            let (mut names, mut func, block) = empty();
            let into = func.new_vreg(GPR);
            let wide = op(&mut names, "mov_ri_64");
            func.build(block, wide).def(into, GPR).imm(value).finish();

            assert_eq!(takes(&mut func, &mut names), 0, "{value}");
            assert_eq!(shape(&func, &names, block), ["mov_ri_64"], "{value}");
        }
    }

    /// A zero the condition state is not free for, which the first rewrite has to leave alone. The
    /// second one has nothing to do with the state and takes it, so the instruction that stays is
    /// five bytes rather than seven.
    #[test]
    fn a_zero_the_state_is_not_free_for_is_narrowed_instead() {
        let (mut names, mut func, block) = empty();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let cmp = op(&mut names, "cmp_rr_32");
        let zero = op(&mut names, "mov_ri_64");
        let set = op(&mut names, "set_e");
        func.build(block, cmp).uses(left, GPR).uses(right, GPR).finish();
        let inst = func.build(block, zero).def(into, GPR).imm(0).finish();
        func.build(block, set).def(byte, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["cmp_rr_32", "mov_ri_32", "set_e"]);
        assert_eq!(imm(&func, inst), Some(0));
    }

    /// A function the state carried across an edge turns down, which is the first rewrite's rule
    /// and not the second one's. The narrower move writes no state and reads none, so a function
    /// that rule turns down still gets it.
    #[test]
    fn a_function_the_carried_state_turns_down_is_still_narrowed() {
        let (mut names, mut func, first) = empty();
        let second = func.create_block();
        let into = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let wide = op(&mut names, "mov_ri_64");
        let set = op(&mut names, "set_e");
        func.build(first, wide).def(into, GPR).imm(7).finish();
        func.build(second, set).def(byte, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, first), ["mov_ri_32"]);
    }

    /// A move of anything else. The shorter instruction writes zero, so it says the same thing only
    /// where the longer one said zero.
    #[test]
    fn a_move_of_a_number_that_is_not_zero_stays() {
        let (mut names, mut func, block) = empty();
        let into = func.new_vreg(GPR);
        let one = op(&mut names, "mov_ri_32");
        func.build(block, one).def(into, GPR).imm(1).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["mov_ri_32"]);
    }

    /// Eight bits, where both spellings are two bytes. The target's table leaves it out and the pass
    /// has nothing to look up, so the move stays and the condition state stays with it.
    #[test]
    fn eight_bits_buys_nothing_and_is_left_alone() {
        let (mut names, mut func, block) = empty();
        let into = func.new_vreg(GPR);
        let zero = op(&mut names, "mov_ri_8");
        func.build(block, zero).def(into, GPR).imm(0).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["mov_ri_8"]);
    }

    /// The cost of the rewrite, which is the zero going into a register in front of something that
    /// reads a comparison of something else. The exclusive or would write over the answer the byte
    /// is about, so the move stays.
    #[test]
    fn a_move_a_condition_reads_the_state_after_stays() {
        let (mut names, mut func, block) = empty();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let cmp = op(&mut names, "cmp_rr_32");
        let zero = op(&mut names, "mov_ri_32");
        let set = op(&mut names, "set_e");
        func.build(block, cmp).uses(left, GPR).uses(right, GPR).finish();
        func.build(block, zero).def(into, GPR).imm(0).finish();
        func.build(block, set).def(byte, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["cmp_rr_32", "mov_ri_32", "set_e"]);
    }

    /// The same three instructions with something writing the condition state in between. What the
    /// byte reads is what the addition left, so the state the move would write is one nothing was
    /// going to read and the rewrite is back on.
    #[test]
    fn a_state_something_else_writes_first_lets_the_rewrite_back_in() {
        let (mut names, mut func, block) = empty();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let zero = op(&mut names, "mov_ri_32");
        let add = op(&mut names, "add_rr_32");
        let set = op(&mut names, "set_e");
        func.build(block, zero).def(into, GPR).imm(0).finish();
        func.build(block, add).def(sum, GPR).uses(left, GPR).uses(right, GPR).finish();
        func.build(block, set).def(byte, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["xor_rr_32", "add_rr_32", "set_e"]);
    }

    /// A function where a block reads the condition state before it writes one, which is what a
    /// state carried across an edge looks like from here. The passes in front say that does not
    /// happen and this is where that is held to rather than believed, so the whole function is
    /// turned down and the move in the other block stays as well.
    #[test]
    fn a_state_carried_into_a_block_turns_the_whole_function_down() {
        let (mut names, mut func, first) = empty();
        let second = func.create_block();
        let into = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let zero = op(&mut names, "mov_ri_32");
        let set = op(&mut names, "set_e");
        func.build(first, zero).def(into, GPR).imm(0).finish();
        func.build(second, set).def(byte, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, first), ["mov_ri_32"]);
    }

    /// The third rewrite. A comparison of a register against zero asks whether the register is
    /// zero, and so does a test of the register against itself, which says it without a number on
    /// the instruction.
    #[test]
    fn a_comparison_against_zero_becomes_a_test_of_the_register_against_itself() {
        for (wide, narrow) in [
            ("cmp_ri_8", "test_rr_8"),
            ("cmp_ri_16", "test_rr_16"),
            ("cmp_ri_32", "test_rr_32"),
            ("cmp_ri_64", "test_rr_64"),
        ] {
            let (mut names, mut func, block) = empty();
            let value = func.new_vreg(GPR);
            let byte = func.new_vreg(GPR);
            let cmp = op(&mut names, wide);
            let set = op(&mut names, "set_e");
            let inst = func.build(block, cmp).uses(value, GPR).imm(0).finish();
            func.build(block, set).def(byte, GPR).finish();

            assert_eq!(takes(&mut func, &mut names), 1, "{wide}");
            assert_eq!(shape(&func, &names, block), [narrow, "set_e"], "{wide}");
            assert_eq!(regs(&func, inst), [value], "{wide}");
            assert_eq!(imm(&func, inst), None, "{wide}");
        }
    }

    /// A comparison against anything else, which the test cannot ask. What a test leaves is the
    /// bits of the register it was given, so it answers one question and the question is zero.
    #[test]
    fn a_comparison_against_a_number_that_is_not_zero_is_left_alone() {
        for value in [1, -1, 7, 255, i64::from(i32::MIN)] {
            let (mut names, mut func, block) = empty();
            let held = func.new_vreg(GPR);
            let byte = func.new_vreg(GPR);
            let cmp = op(&mut names, "cmp_ri_32");
            let set = op(&mut names, "set_e");
            let inst = func.build(block, cmp).uses(held, GPR).imm(value).finish();
            func.build(block, set).def(byte, GPR).finish();

            assert_eq!(takes(&mut func, &mut names), 0, "{value}");
            assert_eq!(shape(&func, &names, block), ["cmp_ri_32", "set_e"], "{value}");
            assert_eq!(imm(&func, inst), Some(value), "{value}");
        }
    }

    /// The condition state is not a question this rewrite asks. The comparison writes the state and
    /// the test writes the same state, so a comparison whose answer something reads right behind it
    /// is rewritten exactly as one whose answer nothing wants is, and a function the carried state
    /// rule turns down gets it too.
    #[test]
    fn a_comparison_is_tested_whatever_the_condition_state_is_doing() {
        let (mut names, mut func, first) = empty();
        let second = func.create_block();
        let value = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let cmp = op(&mut names, "cmp_ri_32");
        let set = op(&mut names, "set_e");
        func.build(first, cmp).uses(value, GPR).imm(0).finish();
        // A block that reads the state before writing it, which is what `carried` turns a function
        // down for and what the first rewrite is the only one to need.
        func.build(second, set).def(byte, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, first), ["test_rr_32"]);
        assert_eq!(shape(&func, &names, second), ["set_e"]);
    }

    /// The fourth rewrite, at every width and in both directions. An addition of one and a
    /// subtraction of minus one are the instruction that adds one, and the other two are the one
    /// that takes one away. The register is the one it had and the number is gone, the shorter
    /// instruction being the one that carries the number in its opcode.
    #[test]
    fn adding_or_taking_away_one_becomes_the_instruction_that_says_so_in_its_opcode() {
        for (name, by, into) in [
            ("add_ri_8", 1, "inc_r_8"),
            ("add_ri_16", 1, "inc_r_16"),
            ("add_ri_32", 1, "inc_r_32"),
            ("add_ri_64", 1, "inc_r_64"),
            ("add_ri_8", -1, "dec_r_8"),
            ("add_ri_16", -1, "dec_r_16"),
            ("add_ri_32", -1, "dec_r_32"),
            ("add_ri_64", -1, "dec_r_64"),
            ("sub_ri_8", 1, "dec_r_8"),
            ("sub_ri_16", 1, "dec_r_16"),
            ("sub_ri_32", 1, "dec_r_32"),
            ("sub_ri_64", 1, "dec_r_64"),
            ("sub_ri_8", -1, "inc_r_8"),
            ("sub_ri_16", -1, "inc_r_16"),
            ("sub_ri_32", -1, "inc_r_32"),
            ("sub_ri_64", -1, "inc_r_64"),
        ] {
            let (mut names, mut func, block) = empty();
            let value = func.new_vreg(GPR);
            let add = op(&mut names, name);
            let inst =
                func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(by).finish();

            assert_eq!(small(&mut func, &mut names), 1, "{name} {by}");
            assert_eq!(shape(&func, &names, block), [into], "{name} {by}");
            assert_eq!(regs(&func, inst), [value, value], "{name} {by}");
            assert_eq!(imm(&func, inst), None, "{name} {by}");
        }
    }

    /// The same function at a level that asked for fast code, which is the goal doing its job. This
    /// is the only rewrite in the pass that asks it, and it is the only one that is a trade.
    #[test]
    fn a_level_that_wanted_fast_code_keeps_the_addition() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let add = op(&mut names, "add_ri_32");
        let inst = func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(1).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["add_ri_32"]);
        assert_eq!(imm(&func, inst), Some(1));
    }

    /// Any other number. The machine has an opcode that means one and none that means anything
    /// else, so the constant is written out either way and the addition is already as short as it
    /// gets.
    #[test]
    fn adding_anything_but_one_stays_an_addition() {
        for by in [0, 2, -2, 7, 255, i64::from(i32::MIN)] {
            let (mut names, mut func, block) = empty();
            let value = func.new_vreg(GPR);
            let add = op(&mut names, "add_ri_32");
            func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(by).finish();

            assert_eq!(small(&mut func, &mut names), 0, "{by}");
            assert_eq!(shape(&func, &names, block), ["add_ri_32"], "{by}");
        }
    }

    /// What the rewrite is really conditional on. Something behind it reading where the value sits
    /// as an unsigned number is something reading the carry, and the carry is the one part of the
    /// condition state the shorter instruction does not write.
    #[test]
    fn an_addition_whose_carry_something_reads_stays_an_addition() {
        for reader in ["set_b", "set_be", "set_a", "set_ae", "adc_ri_32"] {
            let (mut names, mut func, block) = empty();
            let value = func.new_vreg(GPR);
            let byte = func.new_vreg(GPR);
            let add = op(&mut names, "add_ri_32");
            let reads = op(&mut names, reader);
            func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(1).finish();
            func.build(block, reads).def(byte, GPR).finish();

            assert_eq!(small(&mut func, &mut names), 0, "{reader}");
            assert_eq!(shape(&func, &names, block), ["add_ri_32", reader], "{reader}");
        }
    }

    /// A reader of any other part of the state, which the shorter instruction writes exactly as the
    /// addition did. So the rewrite is not about whether the state is read, it is about which part.
    #[test]
    fn an_addition_whose_zero_or_sign_something_reads_still_steps() {
        for reader in ["set_e", "set_ne", "set_l", "set_le", "set_g", "set_ge"] {
            let (mut names, mut func, block) = empty();
            let value = func.new_vreg(GPR);
            let byte = func.new_vreg(GPR);
            let add = op(&mut names, "add_ri_32");
            let reads = op(&mut names, reader);
            func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(1).finish();
            func.build(block, reads).def(byte, GPR).finish();

            assert_eq!(small(&mut func, &mut names), 1, "{reader}");
            assert_eq!(shape(&func, &names, block), ["inc_r_32", reader], "{reader}");
        }
    }

    /// The carry read behind an instruction that writes the rest of the state, which is what the
    /// walk asking the description rather than the flag is for. The addition in front of the
    /// comparison stays, because the comparison writes the carry the reader wants and the addition
    /// would not have to, and the addition behind it goes, because nothing reads a carry after it.
    #[test]
    fn a_write_of_the_state_ends_the_life_of_the_carry_and_a_step_does_not() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let add = op(&mut names, "add_ri_32");
        let cmp = op(&mut names, "cmp_rr_32");
        let below = op(&mut names, "set_b");
        let first = func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(1).finish();
        func.build(block, cmp).uses(value, GPR).uses(value, GPR).finish();
        func.build(block, below).def(byte, GPR).finish();

        assert_eq!(small(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["inc_r_32", "cmp_rr_32", "set_b"]);
        assert_eq!(imm(&func, first), None);
    }

    /// An instruction that leaves the carry alone is not a write of the condition state as far as
    /// the walk is concerned, which is what stops one of them from hiding a carry read behind it.
    /// The increment here is one a program wrote in a template rather than one this put there, and
    /// the addition in front of it is the only thing that sets the carry the reader wants, so the
    /// addition stays.
    #[test]
    fn a_step_does_not_hide_the_carry_read_behind_it() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let add = op(&mut names, "add_ri_32");
        let step = op(&mut names, "inc_r_32");
        let below = op(&mut names, "set_b");
        let first = func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(1).finish();
        func.build(block, step).operand(reuse(value)).uses(value, GPR).finish();
        func.build(block, below).def(byte, GPR).finish();

        assert_eq!(small(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["add_ri_32", "inc_r_32", "set_b"]);
        assert_eq!(imm(&func, first), Some(1));
    }

    /// Two additions in a row with a carry read behind them. The second one sets the carry the
    /// reader wants and stays, and the first one goes, because whatever the first leaves the second
    /// writes over. That is the same walk as the test above arriving at the other answer, and it is
    /// what says the rule is about the carry rather than about the addition.
    #[test]
    fn an_addition_the_next_addition_writes_over_still_steps() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let add = op(&mut names, "add_ri_32");
        let below = op(&mut names, "set_b");
        func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(1).finish();
        let second = func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(1).finish();
        func.build(block, below).def(byte, GPR).finish();

        assert_eq!(small(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["inc_r_32", "add_ri_32", "set_b"]);
        assert_eq!(imm(&func, second), Some(1));
    }

    /// The instruction the whole function check used to stop at. A comparison that keeps a byte
    /// reads the condition state and the state it reads is the one it wrote itself a moment
    /// earlier, so a block opening with one is not a block reading what a predecessor left, and the
    /// move in the other block is rewritten.
    #[test]
    fn a_block_opening_with_a_comparison_that_keeps_a_byte_is_not_a_carried_state() {
        let (mut names, mut func, first) = empty();
        let second = func.create_block();
        let into = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let zero = op(&mut names, "mov_ri_32");
        let fused = op(&mut names, "cmp_set_e_32");
        func.build(first, zero).def(into, GPR).imm(0).finish();
        func.build(second, fused).def(byte, GPR).uses(value, GPR).uses(value, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, first), ["xor_rr_32"]);
    }

    /// The same with the comparison's operand in memory, which is the shape a loop reading an array
    /// and counting with tier six's `setcc` and `movzbl` comes out as. The comparison table leaves
    /// these out, and before the description named them apart this turned the function down.
    #[test]
    fn a_block_opening_with_a_comparison_against_memory_is_not_a_carried_state() {
        let (mut names, mut func, first) = empty();
        let second = func.create_block();
        let into = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let base = func.new_vreg(GPR);
        let zero = op(&mut names, "mov_ri_32");
        let fused = op(&mut names, "cmp_set_g_rm_32");
        func.build(first, zero).def(into, GPR).imm(0).finish();
        func.build(second, fused).def(byte, GPR).uses(value, GPR).uses(base, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, first), ["xor_rr_32"]);
    }

    /// The same sentence inside a block. What the comparison reads is what it wrote, so what it was
    /// handed is written over before anything looks at it, and the move in front of it may spend a
    /// state nothing wants.
    #[test]
    fn a_comparison_that_keeps_a_byte_ends_the_life_of_the_state() {
        let (mut names, mut func, block) = empty();
        let into = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let zero = op(&mut names, "mov_ri_32");
        let fused = op(&mut names, "cmp_set_e_32");
        func.build(block, zero).def(into, GPR).imm(0).finish();
        func.build(block, fused).def(byte, GPR).uses(value, GPR).uses(value, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["xor_rr_32", "cmp_set_e_32"]);
    }

    /// And the carry with it. A comparison that keeps a byte and asks where a value sits as an
    /// unsigned number reads the carry, and it is the carry it set itself, so the addition in front
    /// of it is free to become the instruction that leaves the carry alone.
    #[test]
    fn a_comparison_that_keeps_a_byte_does_not_keep_the_carry_alive() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let add = op(&mut names, "add_ri_32");
        let fused = op(&mut names, "cmp_set_b_32");
        func.build(block, add).operand(reuse(value)).uses(value, GPR).imm(1).finish();
        func.build(block, fused).def(byte, GPR).uses(value, GPR).uses(value, GPR).finish();

        assert_eq!(small(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["inc_r_32", "cmp_set_b_32"]);
    }

    /// The other kind of read, which is the one this must go on stopping at. An add with carry is
    /// reading the bit the instruction in front of it left rather than one it wrote itself, and it
    /// makes no comparison, which is how the description tells the two apart.
    #[test]
    fn an_add_with_carry_opening_a_block_is_still_a_carried_state() {
        let (mut names, mut func, first) = empty();
        let second = func.create_block();
        let into = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let zero = op(&mut names, "mov_ri_32");
        let adc = op(&mut names, "adc_ri_32");
        func.build(first, zero).def(into, GPR).imm(0).finish();
        func.build(second, adc).operand(reuse(value)).uses(value, GPR).imm(1).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, first), ["mov_ri_32"]);
    }

    /// A name the description does not cover, which is anything without this target's prefix. It
    /// may read the condition state and it may write one, and the answer that is wrong about
    /// nothing is that it read it, so the move in front of it stays.
    #[test]
    fn a_name_this_target_does_not_know_stops_the_walk() {
        let (mut names, mut func, block) = empty();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let cmp = op(&mut names, "cmp_rr_32");
        let zero = op(&mut names, "mov_ri_32");
        let strange = mir::Opcode::new(names.intern("nowhere.thing"));
        func.build(block, cmp).uses(left, GPR).uses(right, GPR).finish();
        func.build(block, zero).def(into, GPR).imm(0).finish();
        func.build(block, strange).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["cmp_rr_32", "mov_ri_32", ""]);
    }

    /// The fifth rewrite. An address that is a base register and nothing else is that register, so
    /// the instruction that works it out and keeps it is the move, which keeps both registers in the
    /// order it had them and drops the addressing mode it no longer has a place for.
    #[test]
    fn an_address_that_is_a_register_becomes_a_move() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let lea = op(&mut names, "lea_64");
        let mem = mir::Mem::at(mir::Operand::read(base, GPR));
        let inst = func.build(block, lea).def(into, GPR).mem(mem).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["mov_rr_64"]);
        assert_eq!(regs(&func, inst), [into, base]);
        assert!(func[inst].mem.is_none());
    }

    /// A constant added to the address, which is the shape most address computations have. The move
    /// adds nothing, so there is nothing here for it to say.
    #[test]
    fn an_address_with_a_constant_added_stays() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let lea = op(&mut names, "lea_64");
        let mem = mir::Mem { disp: 8, ..mir::Mem::at(mir::Operand::read(base, GPR)) };
        func.build(block, lea).def(into, GPR).mem(mem).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["lea_64"]);
    }

    /// An index, which is the other half of what an address computation is for. It is a
    /// multiplication and an addition and the move is neither.
    #[test]
    fn an_address_with_an_index_stays() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let lea = op(&mut names, "lea_64");
        let mem = mir::Mem {
            index: Some(mir::Operand::read(index, GPR)),
            scale: 4,
            ..mir::Mem::at(mir::Operand::read(base, GPR))
        };
        func.build(block, lea).def(into, GPR).mem(mem).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["lea_64"]);
    }

    /// The address of a global, which names no register at all. What it works out is a number the
    /// assembler fills in rather than a number that is already somewhere, so there is nothing for a
    /// move to move.
    #[test]
    fn an_address_of_a_symbol_stays() {
        let (mut names, mut func, block) = empty();
        let into = func.new_vreg(GPR);
        let lea = op(&mut names, "lea_64");
        let mem = mir::Mem::of(names.intern("table"));
        func.build(block, lea).def(into, GPR).mem(mem).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["lea_64"]);
    }

    /// And it does not wait on the condition state. Neither the address computation nor the move
    /// writes any, so a byte reading a comparison from in front of it reads the same comparison
    /// afterwards, and the rewrite is taken in the one function the first rewrite has to turn down.
    #[test]
    fn an_address_is_copied_whatever_the_state_behind_it_is() {
        let (mut names, mut func, block) = empty();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let base = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let cmp = op(&mut names, "cmp_rr_32");
        let lea = op(&mut names, "lea_64");
        let set = op(&mut names, "set_e");
        let mem = mir::Mem::at(mir::Operand::read(base, GPR));
        func.build(block, cmp).uses(left, GPR).uses(right, GPR).finish();
        func.build(block, lea).def(into, GPR).mem(mem).finish();
        func.build(block, set).def(byte, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["cmp_rr_32", "mov_rr_64", "set_e"]);
    }

    /// A copy and the addition into it, built the way the allocator leaves them: the copy writes the
    /// register, and the addition reads it back as its first source and writes it over. A second
    /// source is a register when `second` is one and a constant otherwise.
    fn copied_then_added(
        func: &mut mir::Func,
        names: &mut Interner,
        block: mir::Block,
        (copy, add): (&str, &str),
        (into, from): (mir::Reg, mir::Reg),
        second: Result<mir::Reg, i64>,
    ) -> mir::Inst {
        let copy = op(names, copy);
        let add = op(names, add);
        func.build(block, copy).def(into, GPR).uses(from, GPR).finish();
        let build = func.build(block, add).operand(reuse(into)).uses(into, GPR);
        match second {
            Ok(reg) => build.uses(reg, GPR).finish(),
            Err(value) => build.imm(value).finish(),
        }
    }

    /// The sixth rewrite. A copy of one register into another and an addition of a third register
    /// into the copy is one address computation that reads the first two where they are, and the
    /// copy is gone.
    #[test]
    fn a_copy_and_an_addition_become_an_address_computation() {
        for (copy, add, into) in [
            ("mov_rr_64", "add_rr_32", "lea_32"),
            ("mov_rr_32", "add_rr_32", "lea_32"),
            ("mov_rr_64", "add_rr_64", "lea_64"),
        ] {
            let (mut names, mut func, block) = empty();
            let [to, from, second] = [(); 3].map(|()| func.new_vreg(GPR));
            let inst = copied_then_added(
                &mut func,
                &mut names,
                block,
                (copy, add),
                (to, from),
                Ok(second),
            );

            assert_eq!(takes(&mut func, &mut names), 1, "{copy} {add}");
            assert_eq!(shape(&func, &names, block), [into], "{copy} {add}");
            assert_eq!(regs(&func, inst), [to, from, second], "{copy} {add}");
            let amode = func[inst].mem.map(|at| func[at]).expect("an address computation");
            assert_eq!(
                (amode.base, amode.index, amode.scale, amode.disp),
                (Some(1), Some(2), 1, 0)
            );
            assert!(func[inst].imm.is_none(), "{copy} {add}");
        }
    }

    /// The shape the allocator really leaves. The first source of the addition is tied to what it
    /// writes and the copy is what ties them, so after allocation the first source names the register
    /// the copy read. That is the same pair and becomes the same address computation. A first source
    /// naming neither register is an addition of something the copy did not put there.
    #[test]
    fn an_addition_naming_what_the_copy_read_is_the_same_pair() {
        for named in ["copied", "other"] {
            let (mut names, mut func, block) = empty();
            let [to, from, second, other] = [(); 4].map(|()| func.new_vreg(GPR));
            let copy = op(&mut names, "mov_rr_64");
            let add = op(&mut names, "add_rr_32");
            let first = if named == "copied" { from } else { other };
            func.build(block, copy).def(to, GPR).uses(from, GPR).finish();
            let inst = func
                .build(block, add)
                .operand(reuse(to))
                .uses(first, GPR)
                .uses(second, GPR)
                .finish();

            if named == "copied" {
                assert_eq!(takes(&mut func, &mut names), 1);
                assert_eq!(shape(&func, &names, block), ["lea_32"]);
                assert_eq!(regs(&func, inst), [to, from, second]);
            } else {
                assert_eq!(takes(&mut func, &mut names), 0);
                assert_eq!(shape(&func, &names, block), ["mov_rr_64", "add_rr_32"]);
            }
        }
    }

    /// A constant goes in as the number the address adds, and a subtraction of one adds it the other
    /// way round. The thirty two bit one takes a constant the address cannot hold as the number with
    /// the same low thirty two bits, which is the only part of the sum it keeps.
    #[test]
    fn a_constant_is_the_number_the_address_adds() {
        let min = i64::from(i32::MIN);
        for (add, value, into, disp) in [
            ("add_ri_32", 7, "lea_32", 7),
            ("add_ri_32", -7, "lea_32", -7),
            ("sub_ri_32", 7, "lea_32", -7),
            ("sub_ri_32", min, "lea_32", i32::MIN),
            ("add_ri_32", 0xffff_ffff, "lea_32", -1),
            ("add_ri_64", 7, "lea_64", 7),
            ("sub_ri_64", 7, "lea_64", -7),
        ] {
            let (mut names, mut func, block) = empty();
            let [to, from] = [(); 2].map(|()| func.new_vreg(GPR));
            let inst = copied_then_added(
                &mut func,
                &mut names,
                block,
                ("mov_rr_64", add),
                (to, from),
                Err(value),
            );

            assert_eq!(takes(&mut func, &mut names), 1, "{add} {value}");
            assert_eq!(shape(&func, &names, block), [into], "{add} {value}");
            assert_eq!(regs(&func, inst), [to, from], "{add} {value}");
            let amode = func[inst].mem.map(|at| func[at]).expect("an address computation");
            assert_eq!((amode.base, amode.index, amode.disp), (Some(1), None, disp));
        }
    }

    /// A sixty four bit subtraction of the most negative constant adds a number the address cannot
    /// hold, and every bit of the sum is kept, so there is nothing to write it as.
    #[test]
    fn a_constant_a_wide_address_cannot_hold_stays_an_addition() {
        let (mut names, mut func, block) = empty();
        let [to, from] = [(); 2].map(|()| func.new_vreg(GPR));
        let value = i64::from(i32::MIN);
        let pair = ("mov_rr_64", "sub_ri_64");
        copied_then_added(&mut func, &mut names, block, pair, (to, from), Err(value));

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["mov_rr_64", "sub_ri_64"]);
    }

    /// A copy that clears the half of the register a sixty four bit addition reads is not a copy of
    /// what the addition adds to.
    #[test]
    fn a_narrow_copy_is_not_taken_into_a_wide_addition() {
        let (mut names, mut func, block) = empty();
        let [to, from, second] = [(); 3].map(|()| func.new_vreg(GPR));
        let pair = ("mov_rr_32", "add_rr_64");
        copied_then_added(&mut func, &mut names, block, pair, (to, from), Ok(second));

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["mov_rr_32", "add_rr_64"]);
    }

    /// The address computation writes no condition state, so a byte reading what the addition left
    /// keeps the addition and the copy with it.
    #[test]
    fn an_addition_whose_state_something_reads_keeps_its_copy() {
        let (mut names, mut func, block) = empty();
        let [to, from, second, byte] = [(); 4].map(|()| func.new_vreg(GPR));
        let pair = ("mov_rr_64", "add_rr_32");
        copied_then_added(&mut func, &mut names, block, pair, (to, from), Ok(second));
        let set = op(&mut names, "set_e");
        func.build(block, set).def(byte, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["mov_rr_64", "add_rr_32", "set_e"]);
    }

    /// Instructions between the two that leave both registers alone are walked past, which is what
    /// the schedule leaves when it moves the copy up.
    #[test]
    fn a_copy_a_few_instructions_up_is_still_taken() {
        let (mut names, mut func, block) = empty();
        let [to, from, second, other, value] = [(); 5].map(|()| func.new_vreg(GPR));
        let copy = op(&mut names, "mov_rr_64");
        let load = op(&mut names, "mov_rm_64");
        let add = op(&mut names, "add_rr_32");
        func.build(block, copy).def(to, GPR).uses(from, GPR).finish();
        let mem = mir::Mem::at(mir::Operand::read(value, GPR));
        func.build(block, load).def(other, GPR).mem(mem).finish();
        let inst =
            func.build(block, add).operand(reuse(to)).uses(to, GPR).uses(second, GPR).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["mov_rm_64", "lea_32"]);
        assert_eq!(regs(&func, inst), [to, from, second]);
    }

    /// Something between the two that reads the register the copy wrote, or writes the register it
    /// read, needs the copy where it is. So does a call, which writes registers it does not name.
    #[test]
    fn a_copy_something_between_needs_stays() {
        for between in ["reads the copy", "writes the source", "call"] {
            let (mut names, mut func, block) = empty();
            let [to, from, second, other] = [(); 4].map(|()| func.new_vreg(GPR));
            let copy = op(&mut names, "mov_rr_64");
            let mov = op(&mut names, "mov_rr_64");
            let call = op(&mut names, "call");
            let add = op(&mut names, "add_rr_32");
            func.build(block, copy).def(to, GPR).uses(from, GPR).finish();
            match between {
                "reads the copy" => func.build(block, mov).def(other, GPR).uses(to, GPR).finish(),
                "writes the source" => {
                    func.build(block, mov).def(from, GPR).uses(other, GPR).finish()
                }
                _ => func.build(block, call).finish(),
            };
            func.build(block, add).operand(reuse(to)).uses(to, GPR).uses(second, GPR).finish();

            assert_eq!(takes(&mut func, &mut names), 0, "{between}");
            assert_eq!(shape(&func, &names, block)[2], "add_rr_32", "{between}");
        }
    }

    /// A move the allocator did not write is one a template wrote, which the program asked for by
    /// name, so it stays and so does the addition behind it.
    #[test]
    fn a_copy_the_allocator_did_not_write_stays() {
        let (mut names, mut func, block) = empty();
        let [to, from, second] = [(); 3].map(|()| func.new_vreg(GPR));
        let pair = ("mov_rr_64", "add_rr_64");
        copied_then_added(&mut func, &mut names, block, pair, (to, from), Ok(second));

        let took =
            shorter(&mut func, &SHORT, &FLAGS, &MACHINE, &mut names, Goal::Speed, &|_| false);
        assert_eq!(took, 0);
        assert_eq!(shape(&func, &names, block), ["mov_rr_64", "add_rr_64"]);
    }

    /// Every name the table gives is one the target has, and what it becomes takes an address.
    #[test]
    fn every_spread_names_instructions_the_target_has() {
        for entry in SHORT.spreading {
            for name in [entry.name, entry.into].iter().chain(entry.copies) {
                assert!(MACHINE.has(name), "{name}");
            }
            assert!((MACHINE.takes_mem)(entry.into), "{}", entry.into);
        }
    }

    /// The register the addressing mode cannot scale goes in the base, and the copy's source goes in
    /// the index instead.
    #[test]
    fn the_stack_pointer_is_never_the_index() {
        use rucc_target::x86_64::{RAX, RCX, RSP};

        let (mut names, mut func, block) = empty();
        let [to, from, second] = [RAX, RCX, RSP].map(mir::Reg::physical);
        let pair = ("mov_rr_64", "add_rr_64");
        let inst = copied_then_added(&mut func, &mut names, block, pair, (to, from), Ok(second));

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["lea_64"]);
        assert_eq!(regs(&func, inst), [to, second, from]);
    }
}
