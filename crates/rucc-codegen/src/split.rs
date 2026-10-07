//! Splitting critical edges, so that every edge that carries values has somewhere to put them.
//!
//! Design: `spec/10-backend.md` section 10.4.
//!
//! An edge carries values when the block it goes to takes parameters, and giving a parameter its
//! value is a move. The move has to happen on the edge and not before it or after it, because
//! before it is a block that goes somewhere else too and after it is a block that is arrived at
//! from somewhere else too, and in either case the move would run on a path it was not written
//! for. An edge out of a block with one successor can put its moves at the end of that block,
//! since every path through it takes the edge. An edge into a block with one predecessor can put
//! them at the start of that block, for the same reason the other way round. An edge that is
//! neither, which is what a critical edge is, has neither place, and the allocator says so:
//! `rucc_regalloc` asserts that it never sees one.
//!
//! So one is turned into two. A block with nothing in it goes on the edge, the arguments move on
//! to the second half, and both halves are now uncritical: the first goes to a block with one
//! predecessor and the second leaves a block with one successor. Which of the two the moves end
//! up in is the allocator's answer and not this one's, and either is correct.
//!
//! # What it leaves behind
//!
//! An empty block, which is a jump to the next thing unless the layout puts it where it falls
//! through. That is a cost, and it is why an edge with nothing to carry is left alone: there are
//! no moves to find a place for, so splitting it would buy a jump and nothing else. When the
//! allocator puts the moves somewhere else after all, the block is empty again, and from `-O1` up
//! [`crate::layout::forward`] sends the branch past it.
//!
//! # The other edge with nowhere to put a move
//!
//! A computed `goto` leaves its block through a register, and the moves an edge out of it carries
//! would have to be written somewhere the jump has already gone past. So there is a second pass
//! here, [`indirect`], which takes the values off those edges and puts them in a block of their
//! own in front of each label. It runs first, and what it leaves behind is edges the splitting
//! below then has nothing to do about.
//!
//! [`pads`] is here for the same reason and not for a reason of its own: the blocks those labels
//! begin at are addresses an indirect branch arrives at, and a machine that checks the forward edge
//! wants a landing pad at every one of them. Which block an address names is settled by the pass
//! above, so the pad is written after it and not where the prologue's own pad is written.

use rucc_base::Interner;
use rucc_base::hash::{Map, Set};
use rucc_mir as mir;
use rucc_target::{BranchInsts, FlagInsts, FrameInsts, RegClass, ShortInsts};

/// Splits every critical edge that carries values, and every critical edge out of an `asm goto`
/// that writes any, and gives back how many it split.
///
/// Run after lowering and before allocation. Running it twice is running it once, because the
/// blocks it adds have one successor each and are never the source of a critical edge.
pub fn critical(func: &mut mir::Func) -> usize {
    let preds = preds(func);
    let blocks: Vec<mir::Block> = func.blocks().collect();
    let mut split = 0;
    for block in blocks {
        if func[block].succs.len() < 2 {
            continue;
        }
        // An `asm goto` that writes its outputs is the other thing with moves on its edges even
        // when they carry nothing. What it wrote is valid on every edge out of it, and a value the
        // allocator keeps in the frame is stored there behind the instruction, which for the last
        // instruction of a block that leaves several ways is the start of each block it goes to.
        let writes = func.insts(block).last().is_some_and(|last| {
            func[func[last].operands].iter().any(|operand| operand.role.is_def())
        });
        for index in 0..func[block].succs.len() {
            let call = func[block].succs[index].clone();
            if (call.args.is_empty() && !writes) || preds[call.block.index()] < 2 {
                continue;
            }
            // The new block is at the end of the layout, which is where a block that is a jump
            // and nothing else does the least harm before the layout pass has an opinion.
            //
            // It runs exactly as often as the edge it sits on is taken, and both halves of that
            // edge are now that edge, which is why the weight is copied onto all three rather
            // than left at what a block nobody told anything runs. A block on a cold edge that
            // claimed to run once per call would be one the layout put in the middle of the hot
            // path.
            let weight = call.weight;
            let half = func.create_block();
            func.set_weight(half, weight);
            *func.succs_mut(half) = vec![call];
            func.succs_mut(block)[index] = mir::BlockCall::to(half).taken(weight);
            split += 1;
        }
    }
    split
}

/// Takes the values off every edge out of a computed `goto`, and gives back how many blocks it
/// made to hold them.
///
/// Run after lowering and before [`critical`], which then sees edges with nothing on them and
/// leaves them alone. Running it twice is running it once, for the reason the splitting above is:
/// the blocks it adds end in a jump rather than in a branch through a register.
///
/// # What is wrong with the edge it takes the values off
///
/// Every other edge in the function is out of a block whose last instruction the layout writes, so
/// an edge that is the only way out of its block can put its moves at the end of that block and
/// they land in front of the jump. A block that leaves through a register already ends in the jump
/// when the allocator runs, because where it goes is a value and a value is something selection
/// reads rather than something the layout knows. Moves at the end of that block would be written
/// after the jump, where nothing runs them, and moves in front of it would be written across the
/// register the jump reads, which the allocator believes is dead from the jump onwards and is free
/// to hand to one of the moves.
///
/// So the moves go somewhere else. Each label an indirect branch reaches gets a block in front of
/// it that carries the values, the branch goes to that block with nothing on the edge, and the
/// address the `&&label` produces is the address of that block rather than of the label's own. The
/// new block is arrived at one way and leaves one way, so its own edge has both of the places the
/// splitting above talks about and the allocator is content.
///
/// # One label, one address, and two branches that disagree
///
/// A label has one address, so two computed `goto`s that reach it both arrive at whatever block
/// that address names, and the values they carry are not the same values. One block in front of
/// the label cannot move two different sets of registers.
///
/// So they are made to agree first. Each parameter of the label gets a register of its own, every
/// branch writes that register in front of its jump, and the block in front of the label carries
/// those registers and nothing else. That is what gcc does about the same problem, which it calls
/// coalescing across an abnormal edge, done here rather than while the values are still the
/// optimizer's.
///
/// Writing them in front of the jump is safe, which is not obvious, since a branch that goes five
/// ways writes the registers of one of those ways on the path to all five. What makes it safe is
/// that nothing reads those registers except the block in front of the label, and the only way to
/// reach that block is an edge out of a branch, which writes them on the way. So a value written
/// here and not used is a value overwritten before anything looks, whichever way the jump went.
///
/// # One register for one value, and not one for every place it is given to
///
/// That safety is also what makes the cost of it worth watching. A branch writes the registers of
/// every label it can reach, so a register for every parameter of every label is a whole table's
/// worth of moves in front of every jump in the function, and a dispatch table is a branch that
/// reaches hundreds of labels. An interpreter hands each of them whatever its loop had in hand at
/// the jump, which is the same few values over and over, so a register for each place one of them
/// lands means those values written a hundred times over before every instruction the interpreter
/// runs. That is not a small constant. It is what makes an interpreter built this way ten times
/// slower than the same interpreter built with a `switch` instead of the computed `goto`.
///
/// So the register belongs to the value rather than to the place. Two parameters are given one
/// register when they are drawn from the same class and every branch in the function gives them
/// the same register, which is exactly when one register can stand for both, and a branch writes
/// each register it has to write once however many labels asked for it. A dispatch table where
/// every label wants the instruction pointer writes the instruction pointer once. A label
/// something else reaches, or a label given something no other label is given, keeps a register of
/// its own, and a branch that gives one label nothing shares nothing with it, since a register
/// that branch never wrote is not one the label can be given.
///
/// # Panics
///
/// Panics on a class of register the machine named no move for, which is a function carrying a
/// value of a kind the target never said how to copy, and on a branch that has lost the terminator
/// it was found by, which nothing between the finding and the use of it can do. Both are a target
/// description or a function that was built wrongly, and both are worth finding here rather than as
/// a value that arrives somewhere it was never written.
pub fn indirect(
    func: &mut mir::Func,
    branch: &BranchInsts,
    frame: &FrameInsts,
    short: &ShortInsts,
    flags: &FlagInsts,
    names: &mut Interner,
) -> usize {
    let jump = mir::Opcode::new(names.intern(&format!("{}{}", branch.prefix, branch.indirect)));
    let branches: Vec<mir::Block> = func
        .blocks()
        .filter(|&block| func.terminator(block).is_some_and(|last| func[last].opcode == jump))
        .collect();
    // Nothing at all in almost every function, and the walk at the bottom is over every instruction
    // in it, so the answer is arrived at here rather than paid for everywhere.
    if branches.is_empty() {
        return 0;
    }
    // In the order the branches name them rather than in whatever order a hash gives, so that two
    // runs of the compiler over one program write the same blocks.
    let mut targets: Vec<mir::Block> = Vec::new();
    for &block in &branches {
        for call in &func[block].succs {
            if !call.args.is_empty() && !targets.contains(&call.block) {
                targets.push(call.block);
            }
        }
    }

    // One register per thing a branch has to give, rather than one per place it is given to. The
    // key is what every branch gives that parameter, so two parameters given the same register by
    // the same branches are given it in one register and a branch writes that register once.
    let mut homes: Map<Given, mir::Reg> = Map::default();
    // What each branch writes in front of its jump, in the order it was first asked for, and never
    // the same register twice. Two parameters that share a register are given it by the one move.
    let mut writes: Vec<Vec<(mir::Reg, mir::Reg, RegClass)>> = vec![Vec::new(); branches.len()];
    let mut entries: Map<mir::Block, mir::Block> = Map::default();
    // Each label with the block in front of it and the registers that block carries to it, in the
    // order the labels were found.
    let mut placed: Vec<(mir::Block, mir::Block, Vec<mir::Reg>)> = Vec::new();

    for target in targets {
        let params = func[target].params.clone();
        let given = given(func, &branches, target);
        let mut carried: Vec<mir::Reg> = Vec::new();
        for (index, param) in params.iter().enumerate() {
            let key: Given = (
                param.class,
                given.iter().map(|edges| edges.iter().map(|args| args[index]).collect()).collect(),
            );
            let home = match homes.get(&key) {
                Some(&home) => home,
                None => {
                    // As narrow as the widest thing it is given, since what it holds is one of
                    // them, and the whole register when any of them is.
                    let widths = key.1.iter().flatten().map(|&arg| func.width(arg));
                    let width =
                        widths.collect::<Option<Vec<u8>>>().and_then(|all| all.into_iter().max());
                    let home = func.new_vreg(param.class);
                    func.set_width(home, width.map_or(0, u32::from));
                    homes.insert(key, home);
                    home
                }
            };
            carried.push(home);
            for (branch, edges) in given.iter().enumerate() {
                for args in edges {
                    if !writes[branch].iter().any(|&(written, _, _)| written == home) {
                        writes[branch].push((home, args[index], param.class));
                    }
                }
            }
        }
        let entry = func.create_block();
        let mut total = mir::Weight::NEVER;
        for &block in &branches {
            for index in 0..func[block].succs.len() {
                if func[block].succs[index].block != target {
                    continue;
                }
                // The block in front of the label runs as often as every branch that reaches it,
                // which is the same sum the weight of a block with that many edges into it would
                // be.
                let weight = func[block].succs[index].weight;
                total = mir::Weight::parts(total.raw().saturating_add(weight.raw()));
                func.succs_mut(block)[index] = mir::BlockCall::to(entry).taken(weight);
            }
        }
        func.set_weight(entry, total);
        placed.push((target, entry, carried.clone()));
        *func.succs_mut(entry) = vec![mir::BlockCall::with(target, carried).taken(total)];
        entries.insert(target, entry);
    }

    // And the labels that can do without a parameter at all, because what they would be given is
    // already in a register they can keep using. See [`in_place`].
    let renamed = in_place(func, &branches, &placed, &writes);

    // And the moves themselves, once every label has asked for what it wants, since what one label
    // asks for is what another may already have asked the same branch for.
    let mut made: Vec<(mir::Block, mir::Inst)> = Vec::new();
    for (branch, moves) in branches.iter().zip(&writes) {
        let last = func.terminator(*branch).expect("a block that ends in a jump");
        for &(home, arg, class) in moves {
            // A label handing on what it was given, which is in the register already.
            if renamed.get(&arg) == Some(&home) {
                continue;
            }
            let name = frame.moves(class).expect("a class this machine can move").mov;
            let opcode = mir::Opcode::new(names.intern(&format!("{}{name}", frame.prefix)));
            let inst = func.build_loose(opcode).def(home, class).uses(arg, class).finish();
            func.insert_before(last, inst);
            made.push((*branch, inst));
        }
    }
    rename(func, &renamed);
    let lea = mir::Opcode::new(names.intern(&format!("{}{}", frame.prefix, frame.lea)));
    let mut opcode =
        |name: &str| mir::Opcode::new(names.intern(&format!("{}{name}", short.prefix)));
    let spreads = short
        .spreading
        .iter()
        .map(|entry| (opcode(entry.name), opcode(entry.into), entry.sign))
        .collect();
    at_source(func, &made, &Steps { lea, spreads, flags, names });

    // And the addresses, which is the half of this that is not about edges. Every `&&label` in the
    // function names a block, and a label with a block in front of it now begins at that block, so
    // an address left pointing at the label's own block would be a jump past the moves.
    let mut addresses: Vec<mir::MemRef> = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if let Some(mem) = func[inst].mem {
                addresses.push(mem);
            }
        }
    }
    for mem in addresses {
        if let Some(named) = func[mem].block {
            if let Some(&entry) = entries.get(&named) {
                func[mem].block = Some(entry);
            }
        }
    }
    // And the names, for the same reason. A block an image points at is one a `goto *p` arrives at,
    // so a name left on the label's own block would be an address in a table that skips the moves,
    // which is the one way into the block that would not have made them.
    for (block, _) in &mut func.labels {
        if let Some(&entry) = entries.get(block) {
            *block = entry;
        }
    }
    entries.len()
}

/// Puts a landing pad right after every call that says it can come back by a jump, with
/// [`mir::Flags::LANDS`], and gives back how many it wrote. Nothing without a pad to write, which
/// is the same option [`pads`] reads.
pub fn after_calls(
    func: &mut mir::Func,
    frame: &FrameInsts,
    landing: Option<&'static str>,
    names: &mut Interner,
) -> usize {
    let Some(name) = landing else { return 0 };
    let opcode = mir::Opcode::new(names.intern(&format!("{}{name}", frame.prefix)));
    let calls: Vec<mir::Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].flags.contains(mir::Flags::LANDS))
        .collect();
    for &call in &calls {
        let pad = func.build_loose(opcode).finish();
        func.insert_after(call, pad);
    }
    calls.len()
}

/// Puts a landing pad at the front of every block whose address is taken, and gives back how many
/// it wrote.
///
/// Run after [`indirect`], because the block an address names is not settled until that has moved
/// the addresses on to the blocks it made, and only when the command line asked for the forward
/// edge to be checked. Nothing is written otherwise, which is why the name comes in as an option
/// and why a target with no such instruction is a target this does nothing on.
///
/// The pad a prologue opens with is written elsewhere, in `crate::finish`, because the address it
/// makes reachable is the address of the function rather than a place inside it. These are the
/// other addresses an indirect branch may arrive at, and a machine that checks the forward edge
/// faults on one that has no pad, so a computed `goto` compiled without this would be a program
/// that ran everywhere except on the hardware the flag was turned on for.
pub fn pads(
    func: &mut mir::Func,
    frame: &FrameInsts,
    landing: Option<&'static str>,
    names: &mut Interner,
) -> usize {
    let Some(name) = landing else { return 0 };
    let opcode = mir::Opcode::new(names.intern(&format!("{}{name}", frame.prefix)));
    let mut addressed: Vec<mir::Block> = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if let Some(mem) = func[inst].mem {
                if let Some(named) = func[mem].block {
                    if !addressed.contains(&named) {
                        addressed.push(named);
                    }
                }
            }
        }
    }
    // And every arm of a jump table, which an indirect jump arrives at the same way.
    for table in &func.tables {
        let Some(jump) = func.block_of(table.jump) else { continue };
        for &cell in &table.cells {
            let named = func[jump].succs[cell as usize].block;
            if !addressed.contains(&named) {
                addressed.push(named);
            }
        }
    }
    // And every label an image points at, such as the kernel's BPF interpreter's table of
    // `&&label` in a static array, which a `goto *` arrives at without any instruction in the
    // function naming it.
    for &(named, _) in &func.labels {
        if !addressed.contains(&named) {
            addressed.push(named);
        }
    }
    for &block in &addressed {
        let inst = func.build_loose(opcode).finish();
        func.prepend_inst(block, inst);
    }
    addressed.len()
}

/// What decides whether two parameters can be given their value in one register: the class the
/// parameter is drawn from, and the register every branch in the function gives it, in the order
/// the branches are in and with one entry per edge inside that. A branch that does not reach the
/// label gives nothing, which is a length of zero and is as much a part of the answer as a
/// register is, since sharing with a parameter a branch never gives anything to would be reading a
/// register that branch never wrote.
type Given = (RegClass, Vec<Vec<mir::Reg>>);

/// Takes the parameters off the labels that can be given their value in the register a branch
/// writes it to, and gives back each of those parameters with that register.
///
/// A label's parameter is given its value by a move out of that register at the top of the label,
/// and an interpreter's handler hands most of what it was given on to the next handler unchanged,
/// which is a move back into the same register in front of its jump. Neither move does anything
/// the allocator could not have done by putting both in one place, but it cannot see that they
/// belong in one place: the register lives from a jump to the label and no further, and the
/// parameter lives from the label to the jump and no further, so a call in the handler is a reason
/// to put the parameter somewhere a call keeps and no reason at all to do the same for the register.
/// Every handler that makes a call then copies every value it carries out of one and back in.
///
/// So where it is safe the label reads the register itself and the parameter goes. It is safe when
/// nothing else could have written the register by the time the label reads it, which is when all
/// of these hold:
///
/// - The label is arrived at only from the block in front of it, so the register is the only way
///   the value gets there.
/// - Nothing defines the parameter but the label, which is true of every parameter in a function
///   that has not been allocated and is checked rather than trusted.
/// - No branch writes the parameter into a register other than its own, since the moves in front
///   of a jump are written one after another and one that read a register an earlier one wrote
///   would read the wrong value.
/// - No jump reads it, since the moves are written in front of the jump.
/// - It is not wanted on the far side of any jump, which is the only place the register is written.
///   That one is a walk back from every place the parameter is read until it reaches the label,
///   and a walk that reaches any block a jump goes to is a value a jump went past.
fn in_place(
    func: &mut mir::Func,
    branches: &[mir::Block],
    placed: &[(mir::Block, mir::Block, Vec<mir::Reg>)],
    writes: &[Vec<(mir::Reg, mir::Reg, RegClass)>],
) -> Map<mir::Reg, mir::Reg> {
    let mut preds: Vec<Vec<mir::Block>> = vec![Vec::new(); func.block_count()];
    for block in func.blocks() {
        for call in &func[block].succs {
            preds[call.block.index()].push(block);
        }
    }
    // Every block a jump through a register goes to, which is the block in front of a label that
    // takes something and the label itself for one that takes nothing.
    let mut beyond = vec![false; func.block_count()];
    for &branch in branches {
        for call in &func[branch].succs {
            beyond[call.block.index()] = true;
        }
    }
    // Each parameter that may be its register, with its label and that register.
    let mut candidates: Map<mir::Reg, (mir::Block, mir::Reg)> = Map::default();
    for (target, front, homes) in placed {
        let (target, front) = (*target, *front);
        if preds[target.index()] != [front] {
            continue;
        }
        for (param, &home) in func[target].params.iter().zip(homes) {
            candidates.insert(param.reg, (target, home));
        }
    }
    if candidates.is_empty() {
        return Map::default();
    }

    let mut out: Vec<mir::Reg> = Vec::new();
    let mut reads: Map<mir::Reg, Vec<mir::Block>> = Map::default();
    for (&branch, moves) in branches.iter().zip(writes) {
        for &(home, arg, _) in moves {
            if let Some(&(_, own)) = candidates.get(&arg) {
                if own == home {
                    reads.entry(arg).or_default().push(branch);
                } else {
                    out.push(arg);
                }
            }
        }
        let last = func.terminator(branch).expect("a block that ends in a jump");
        out.extend(func[func[last].operands].iter().map(|operand| operand.reg));
    }
    for block in func.blocks() {
        for param in &func[block].params {
            if candidates.get(&param.reg).is_some_and(|&(target, _)| target != block) {
                out.push(param.reg);
            }
        }
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                if !candidates.contains_key(&operand.reg) {
                    continue;
                }
                if operand.role.is_def() {
                    out.push(operand.reg);
                } else {
                    reads.entry(operand.reg).or_default().push(block);
                }
            }
        }
        for call in &func[block].succs {
            for arg in &call.args {
                if candidates.contains_key(arg) {
                    reads.entry(*arg).or_default().push(block);
                }
            }
        }
    }
    for reg in out {
        candidates.remove(&reg);
    }

    // The walk back, one parameter at a time and stamped rather than cleared between them.
    let mut seen = vec![0_usize; func.block_count()];
    let mut stamp = 0;
    let mut wanted: Vec<mir::Block> = Vec::new();
    candidates.retain(|reg, &mut (target, _)| {
        stamp += 1;
        wanted.clear();
        let Some(blocks) = reads.get(reg) else { return true };
        wanted.extend(blocks.iter().copied().filter(|&block| block != target));
        while let Some(block) = wanted.pop() {
            if seen[block.index()] == stamp {
                continue;
            }
            seen[block.index()] = stamp;
            if beyond[block.index()] {
                return false;
            }
            wanted.extend(preds[block.index()].iter().copied().filter(|&pred| pred != target));
        }
        true
    });

    // And the parameters themselves, label by label and from the last so that taking one off does
    // not move the ones still to look at.
    let mut renamed: Map<mir::Reg, mir::Reg> = Map::default();
    for &(target, front, _) in placed {
        let params = func[target].params.clone();
        for (index, param) in params.iter().enumerate().rev() {
            let Some(&(_, home)) = candidates.get(&param.reg) else { continue };
            // The register holds what the parameter did now, so it is as wide as the wider of them.
            let width = match (func.width(param.reg), func.width(home)) {
                (Some(one), Some(other)) => u32::from(one.max(other)),
                _ => 0,
            };
            func.set_width(home, width);
            func.params_mut(target).remove(index);
            func.succs_mut(front)[0].args.remove(index);
            renamed.insert(param.reg, home);
        }
    }
    renamed
}

/// Writes a value a branch works out for a label into the register the label is given it in, where
/// it is worked out, and takes out the move that copied it there. Gives back how many moves went.
///
/// An interpreter's handler steps on to the next instruction and jumps to it, which is `op++` and
/// `goto *op->opcode`. The step is a new value, and the move in front of the jump copies it into
/// the register the next handler reads the step pointer in. The allocator does not see that the
/// two belong in one register, since a copy is not something it is asked to keep together, so every
/// handler of Postgres' `ExecInterpExpr` ended in `leaq 24(%rcx), %rdi`, the load through `%rdi`
/// and `movq %rdi, %rcx`, where gcc writes `addq $24, %rcx`. tamnd/rucc#1994.
///
/// That register is written by the moves in front of the jump and read only by the labels, so it
/// may be written earlier in the same block as long as nothing in between reads what it held or
/// writes it, which is what is checked:
///
/// - The value is written once in the function, by an instruction in the branch's own block in
///   front of the move, that writes it as any register at all and names no register outright.
/// - Every read of it is in that block, between that instruction and the move, so renaming those
///   reads is renaming all of them.
/// - Nothing in between, and nothing in that instruction but a read, names the register the move
///   writes. A read in the instruction itself is fine, since an instruction reads its sources
///   before it writes an answer that is not written early.
/// - It is not itself one of those registers, which another move may have written there first.
///
/// A handler that steps on before it is done with the old pointer, such as `op++` above a read of
/// what `op` pointed at, reads the register in between. When the step is an address worked out
/// with the target's `lea`, which reads no memory and writes no flags, it is moved down to just
/// after the last of those reads first, as long as nothing it is moved past writes what it reads
/// or reads what it writes. A step that is an addition of a constant, which is how `op++` comes
/// out of selection, is written as the `lea` that is the same sum before it is moved, when nothing
/// reads the flags the addition wrote.
fn at_source(func: &mut mir::Func, made: &[(mir::Block, mir::Inst)], steps: &Steps<'_>) -> usize {
    let homes: Set<mir::Reg> =
        made.iter().map(|&(_, copy)| func[func[copy].operands][0].reg).collect();
    let mut writes: Map<mir::Reg, usize> = Map::default();
    let mut reads: Map<mir::Reg, usize> = Map::default();
    for block in func.blocks() {
        for param in &func[block].params {
            *writes.entry(param.reg).or_default() += 1;
        }
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                let count = if operand.role.is_def() { &mut writes } else { &mut reads };
                *count.entry(operand.reg).or_default() += 1;
            }
        }
        for call in &func[block].succs {
            for &arg in &call.args {
                *reads.entry(arg).or_default() += 1;
            }
        }
    }
    let mut gone = 0;
    for &(block, copy) in made {
        let &[to, from] = &func[func[copy].operands] else { continue };
        let (home, value) = (to.reg, from.reg);
        if !value.is_virtual() || homes.contains(&value) || writes.get(&value) != Some(&1) {
            continue;
        }
        let mut insts: Vec<mir::Inst> = func.insts(block).collect();
        let Some(end) = insts.iter().position(|&inst| inst == copy) else { continue };
        let Some(start) = insts[..end].iter().position(|&inst| {
            func[func[inst].operands]
                .iter()
                .any(|operand| operand.reg == value && operand.role.is_def())
        }) else {
            continue;
        };
        let plain = func[func[insts[start]].operands].iter().all(|operand| {
            let answer = operand.role == mir::Role::Def
                && operand.class == to.class
                && matches!(operand.constraint, mir::Constraint::Reg | mir::Constraint::Reuse(_));
            operand.reg.is_virtual()
                && (operand.reg != value || answer)
                && (operand.reg != home || !operand.role.is_def())
        });
        if !plain {
            continue;
        }
        if insts[start + 1..end].iter().any(|&inst| naming(func, inst, home) > 0) {
            if let Some(address) = as_address(func, steps, &insts, start) {
                insts[start] = address;
            }
        }
        let maker = insts[start];
        let lea = steps.lea;
        let mut between = &insts[start + 1..end];
        if let Some(last) = between.iter().rposition(|&inst| naming(func, inst, home) > 0) {
            let read: Vec<mir::Reg> = func[func[maker].operands]
                .iter()
                .filter(|operand| !operand.role.is_def())
                .map(|operand| operand.reg)
                .collect();
            let past = &between[..=last];
            let movable = func[maker].opcode == lea
                && past.iter().all(|&inst| {
                    func[func[inst].operands].iter().all(|operand| {
                        operand.reg != value
                            && !(operand.role.is_def()
                                && (operand.reg == home || read.contains(&operand.reg)))
                    })
                });
            if !movable {
                continue;
            }
            func.remove_inst(maker);
            func.insert_after(past[last], maker);
            between = &between[last + 1..];
        }
        let local: usize = between.iter().map(|&inst| naming(func, inst, value)).sum();
        if reads.get(&value) != Some(&(local + 1)) {
            continue;
        }
        for &inst in between.iter().chain([&maker]) {
            let operands = func[inst].operands;
            for operand in &mut func[operands] {
                if operand.reg == value {
                    operand.reg = home;
                }
            }
        }
        // A step worked out from the register it is written into, such as `leaq 24(%rcx), %rcx`,
        // is tied to it the way a two address instruction is. The allocator asks no more of a
        // `lea` than that its answer is in some register, and a register of its own and a copy at
        // the end of it is an answer.
        if func[maker].opcode == lea {
            let base = func[maker].mem.and_then(|mem| func[mem].base);
            let operands = func[maker].operands;
            let reads = base.filter(|&at| {
                func[operands].get(usize::from(at)).is_some_and(|operand| {
                    operand.reg == home && operand.class == to.class && !operand.role.is_def()
                })
            });
            if let Some(at) = reads {
                for operand in &mut func[operands] {
                    if operand.reg == home
                        && operand.role == mir::Role::Def
                        && operand.constraint == mir::Constraint::Reg
                    {
                        operand.constraint = mir::Constraint::Reuse(at);
                    }
                }
            }
        }
        // A variable the value was is the register now, for the debugger's sake.
        for named in &mut func.named {
            if named.1 == value {
                named.1 = home;
            }
        }
        func.remove_inst(copy);
        gone += 1;
    }
    gone
}

/// What [`at_source`] needs to know to move a step down: the target's `lea`, each addition of a
/// constant with the `lea` that is the same sum and what the constant is multiplied by on its way
/// into the address, and which instructions read and write the flags.
struct Steps<'a> {
    lea: mir::Opcode,
    spreads: Vec<(mir::Opcode, mir::Opcode, i64)>,
    flags: &'a FlagInsts,
    names: &'a Interner,
}

/// Writes the step at that place in the block as the `lea` that is the same sum, when it is an
/// addition of a constant into a register and nothing after it reads the flags it wrote before
/// something else writes them. Gives back the `lea`, which is where the addition was.
///
/// Only an addition whose `lea` is the target's own is taken, since that is the one [`at_source`]
/// knows how to move, and a 32 bit step is left as it was.
fn as_address(
    func: &mut mir::Func,
    steps: &Steps<'_>,
    insts: &[mir::Inst],
    at: usize,
) -> Option<mir::Inst> {
    let step = insts[at];
    let opcode = func[step].opcode;
    let &(_, into, sign) = steps.spreads.iter().find(|&&(from, _, _)| from == opcode)?;
    if into != steps.lea || func[step].mem.is_some() {
        return None;
    }
    let constant = func[func[step].imm?].0;
    let &[answer, first] = &func[func[step].operands] else { return None };
    let plain = answer.role.is_def() && !first.role.is_def() && first.class == answer.class;
    let disp = i32::try_from(i128::from(constant) * i128::from(sign)).ok()?;
    if !plain || !first.reg.is_virtual() {
        return None;
    }
    // A name the target does not know may read them.
    for &inst in &insts[at + 1..] {
        let name = steps.names.resolve(func[inst].opcode.name());
        let bare = name.strip_prefix(steps.flags.prefix)?;
        if steps.flags.reads(bare).is_some() {
            return None;
        }
        if (steps.flags.writes)(bare) {
            break;
        }
    }
    let span = func.span(step);
    let mem = mir::Mem { disp, ..mir::Mem::at(mir::Operand::read(first.reg, first.class)) };
    let address = func.build_loose(into).def(answer.reg, answer.class).mem(mem).at(span).finish();
    func.insert_before(step, address);
    func.remove_inst(step);
    Some(address)
}

/// How many of the instruction's operands name the register.
fn naming(func: &mir::Func, inst: mir::Inst, reg: mir::Reg) -> usize {
    func[func[inst].operands].iter().filter(|operand| operand.reg == reg).count()
}

/// Reads each register [`in_place`] took off a label as the register it was given in instead,
/// everywhere in the function.
fn rename(func: &mut mir::Func, renamed: &Map<mir::Reg, mir::Reg>) {
    if renamed.is_empty() {
        return;
    }
    let blocks: Vec<mir::Block> = func.blocks().collect();
    for block in blocks {
        let insts: Vec<mir::Inst> = func.insts(block).collect();
        for inst in insts {
            let operands = func[inst].operands;
            for operand in &mut func[operands] {
                if let Some(&home) = renamed.get(&operand.reg) {
                    operand.reg = home;
                }
            }
        }
        for call in func.succs_mut(block) {
            for arg in &mut call.args {
                if let Some(&home) = renamed.get(arg) {
                    *arg = home;
                }
            }
        }
    }
}

/// What each branch gives that label, edge by edge.
///
/// One entry per branch and in the branches' own order, since a label two branches reach and a
/// label one branch reaches twice are not given the same thing. A branch is allowed to reach one
/// label twice, which a table with the same label in two of its cells is, so what a branch gives
/// is a list of what it gives rather than one set of registers.
fn given(func: &mir::Func, branches: &[mir::Block], target: mir::Block) -> Vec<Vec<Vec<mir::Reg>>> {
    branches
        .iter()
        .map(|&block| {
            func[block]
                .succs
                .iter()
                .filter(|call| call.block == target)
                .map(|call| call.args.clone())
                .collect()
        })
        .collect()
}

/// How many edges arrive at each block, counted by index rather than in layout order so that a
/// block added while splitting can be looked up in the same table.
fn preds(func: &mir::Func) -> Vec<usize> {
    let mut counts = vec![0; func.block_count()];
    for block in func.blocks() {
        for call in &func[block].succs {
            counts[call.block.index()] += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_target::x86_64::{BRANCH, FLAGS, FRAME, GPR, REGS, SHORT};

    use super::*;

    /// A diamond: one block that goes two ways and one block both ways arrive at, with as many
    /// parameters on the block they arrive at as the test asks for.
    fn diamond(params: usize) -> (Interner, mir::Func, [mir::Block; 4]) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let head = func.create_block();
        let left = func.create_block();
        let right = func.create_block();
        let join = func.create_block();
        // The values arrive in the head, so that they have somewhere to be defined and the
        // printer has a name for them. Nothing here runs an allocator, which is the one thing
        // that would object to a first block with parameters.
        let args: Vec<mir::Reg> = (0..params).map(|_| func.append_param(head, GPR)).collect();
        for _ in 0..params {
            func.append_param(join, GPR);
        }
        *func.succs_mut(head) = vec![mir::BlockCall::to(left), mir::BlockCall::to(right)];
        *func.succs_mut(left) = vec![mir::BlockCall::with(join, args.clone())];
        *func.succs_mut(right) = vec![mir::BlockCall::with(join, args)];
        (names, func, [head, left, right, join])
    }

    /// Where each block goes, which is the whole of what this changes.
    fn edges(func: &mir::Func) -> Vec<Vec<usize>> {
        func.blocks()
            .map(|block| func[block].succs.iter().map(|call| call.block.index()).collect())
            .collect()
    }

    #[test]
    fn an_edge_that_is_the_only_way_out_is_left_alone() {
        let (_, mut func, _) = diamond(1);
        // The two edges into the join carry a value each and neither is critical, because the
        // block each leaves goes nowhere else.
        assert_eq!(critical(&mut func), 0);
        assert_eq!(edges(&func), vec![vec![1, 2], vec![3], vec![3], vec![]]);
    }

    #[test]
    fn a_critical_edge_carrying_a_value_is_split_in_two() {
        let (_, mut func, [head, _, _, join]) = diamond(1);
        // Now the head goes straight to the join as well, so both of its arms are critical: it
        // has two ways out and the join has three ways in.
        let arg = func.append_param(head, GPR);
        func.succs_mut(head).push(mir::BlockCall::with(join, vec![arg]));
        func.succs_mut(head).swap(1, 2);

        assert_eq!(critical(&mut func), 1);
        assert_eq!(
            edges(&func),
            // The head's second arm is the new block and the new block goes to the join. The
            // other two arms are untouched, because each goes to a block with one way in.
            vec![vec![1, 4, 2], vec![3], vec![3], vec![], vec![3]]
        );
    }

    #[test]
    fn a_critical_edge_carrying_nothing_is_left_alone() {
        let (_, mut func, [head, _, _, join]) = diamond(0);
        func.succs_mut(head).push(mir::BlockCall::to(join));

        // Critical and not split, because there is no move to find a place for and a block that
        // is a jump and nothing else is worth more than nothing.
        assert_eq!(critical(&mut func), 0);
    }

    #[test]
    fn the_arguments_move_on_to_the_half_that_arrives() {
        let (names, mut func, [head, _, _, join]) = diamond(1);
        let arg = func.append_param(head, GPR);
        func.succs_mut(head).push(mir::BlockCall::with(join, vec![arg]));

        assert_eq!(critical(&mut func), 1);
        // What the first half carries is nothing, since the block it goes to asks for nothing,
        // and what the second half carries is what the whole edge used to.
        let half = func.blocks().last().expect("the block the split added");
        assert_eq!(func[head].succs[2].args, Vec::new());
        assert_eq!(func[half].succs[0].args, vec![arg]);
        assert_eq!(
            mir::print_func(&func, &names, &REGS),
            "mfunc @f {\nblock0(%0:gpr, %1:gpr):\n    block1, block2, block4\n\n\
             block1:\n    block3(%0)\n\nblock2:\n    block3(%0)\n\n\
             block3(%2:gpr):\n\nblock4:\n    block3(%1)\n}\n"
        );
    }

    #[test]
    fn splitting_twice_is_splitting_once() {
        let (_, mut func, [head, _, _, join]) = diamond(1);
        let arg = func.append_param(head, GPR);
        func.succs_mut(head).push(mir::BlockCall::with(join, vec![arg]));

        assert_eq!(critical(&mut func), 1);
        assert_eq!(critical(&mut func), 0);
    }

    /// A function with one label whose address is taken and as many blocks leaving through that
    /// address as the test asks for, each carrying as many values to the label as it asks for.
    fn computed(branches: usize, params: usize) -> (Interner, mir::Func) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let head = func.create_block();
        let label = func.create_block();
        for _ in 0..params {
            func.append_param(label, GPR);
        }
        let lea = mir::Opcode::new(names.intern("x64.lea_64"));
        let jump = mir::Opcode::new(names.intern("x64.jmp_reg"));
        for _ in 0..branches {
            // Every branch works the address out for itself, which is what a program that takes
            // the address of a label twice looks like once the values are in registers.
            let args: Vec<mir::Reg> = (0..params).map(|_| func.append_param(head, GPR)).collect();
            let address = func.new_vreg(GPR);
            let at = if branches == 1 { head } else { func.create_block() };
            func.build(at, lea).def(address, GPR).mem(mir::Mem::block(label)).finish();
            func.build(at, jump).operand(mir::Operand::read(address, GPR)).finish();
            *func.succs_mut(at) = vec![mir::BlockCall::with(label, args)];
        }
        (names, func)
    }

    /// Which block each address in the function names, in the order the instructions are in.
    fn addressed(func: &mir::Func) -> Vec<usize> {
        func.blocks()
            .flat_map(|block| func.insts(block))
            .filter_map(|inst| func[inst].mem)
            .filter_map(|mem| func[mem].block)
            .map(mir::Block::index)
            .collect()
    }

    #[test]
    fn the_values_a_computed_goto_carries_move_into_a_block_in_front_of_the_label() {
        let (mut names, mut func) = computed(1, 1);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 1);

        // The branch goes to the new block carrying nothing, and the new block carries the value
        // the branch used to. The address the `lea` works out is the new block's as well, since
        // arriving at the label without going through the new block is arriving without the value.
        assert_eq!(edges(&func), vec![vec![2], vec![], vec![1]]);
        assert_eq!(func[mir::Block::new(0)].succs[0].args, Vec::new());
        assert_eq!(addressed(&func), vec![2]);
    }

    #[test]
    fn two_computed_gotos_that_reach_one_label_are_made_to_agree() {
        let (mut names, mut func) = computed(2, 1);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 1);

        // One block in front of the label and not two, because the label has one address and both
        // branches arrive at it. What makes that sound is the move each branch writes in front of
        // its own jump, which puts its value in the register that block carries.
        assert_eq!(edges(&func), vec![vec![], vec![], vec![4], vec![4], vec![1]]);
        let text = mir::print_func(&func, &names, &REGS);
        assert_eq!(text.matches("x64.mov_rr_64").count(), 2, "{text}");
        // In front of the jump rather than behind it, since nothing behind a jump runs.
        for line in text.lines().collect::<Vec<_>>().windows(2) {
            if line[1].contains("x64.jmp_reg") {
                assert!(line[0].contains("x64.mov_rr_64"), "{text}");
            }
        }
        assert_eq!(addressed(&func), vec![4, 4]);
    }

    /// A function with one computed `goto` that reaches as many labels as the test asks for, each
    /// given the one value the branch has in hand, which is the shape of a dispatch table.
    fn table(labels: usize) -> (Interner, mir::Func, Vec<mir::Block>) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let head = func.create_block();
        let arg = func.append_param(head, GPR);
        let lea = mir::Opcode::new(names.intern("x64.lea_64"));
        let jump = mir::Opcode::new(names.intern("x64.jmp_reg"));
        let mut targets = Vec::new();
        for _ in 0..labels {
            let label = func.create_block();
            func.append_param(label, GPR);
            targets.push(label);
            func.succs_mut(head).push(mir::BlockCall::with(label, vec![arg]));
        }
        let address = func.new_vreg(GPR);
        func.build(head, lea).def(address, GPR).mem(mir::Mem::block(targets[0])).finish();
        func.build(head, jump).operand(mir::Operand::read(address, GPR)).finish();
        (names, func, targets)
    }

    #[test]
    fn labels_a_branch_gives_the_same_value_are_given_it_in_one_register() {
        let (mut names, mut func, _) = table(8);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 8);

        // One move in front of the jump and not eight, because the eight labels are given the one
        // value and it is now in the one register. Eight blocks were still made, since each label
        // needs the block that moves that register on to its own parameter.
        let text = mir::print_func(&func, &names, &REGS);
        assert_eq!(text.matches("x64.mov_rr_64").count(), 1, "{text}");
    }

    #[test]
    fn a_label_given_something_else_keeps_a_register_of_its_own() {
        let (mut names, mut func, targets) = table(8);
        let head = mir::Block::new(0);
        let other = func.append_param(head, GPR);
        let last = func[head].succs.len() - 1;
        func.succs_mut(head)[last] = mir::BlockCall::with(targets[7], vec![other]);

        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 8);
        // Two moves: one register for the seven labels given the same value, and one for the label
        // given the other. Sharing is about what a label is given and not about how many there are.
        let text = mir::print_func(&func, &names, &REGS);
        assert_eq!(text.matches("x64.mov_rr_64").count(), 2, "{text}");
    }

    #[test]
    fn labels_that_take_different_numbers_of_values_still_share_the_ones_they_agree_on() {
        let (mut names, mut func, targets) = table(8);
        let head = mir::Block::new(0);
        let arg = func[head].params[0].reg;
        let other = func.append_param(head, GPR);
        // The last label takes a second value, which is what an interpreter looks like: each of
        // its labels uses what it needs and no two of them need quite the same list.
        func.append_param(targets[7], GPR);
        let last = func[head].succs.len() - 1;
        func.succs_mut(head)[last] = mir::BlockCall::with(targets[7], vec![arg, other]);

        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 8);
        // Two moves, not nine. The first parameter of the long label is given what the other seven
        // are given, so it takes the same register, and only the value nothing else is given needs
        // one of its own.
        let text = mir::print_func(&func, &names, &REGS);
        assert_eq!(text.matches("x64.mov_rr_64").count(), 2, "{text}");
    }

    /// Whether any instruction or edge in the function still reads or writes that register.
    fn mentions(func: &mir::Func, reg: mir::Reg) -> bool {
        func.blocks().any(|block| {
            func.insts(block).any(|inst| func[func[inst].operands].iter().any(|op| op.reg == reg))
                || func[block].succs.iter().any(|call| call.args.contains(&reg))
        })
    }

    /// A jump through a register at the end of that block, to the label `to`, carrying `args`.
    fn dispatch(
        func: &mut mir::Func,
        names: &mut Interner,
        from: mir::Block,
        to: mir::Block,
        args: Vec<mir::Reg>,
    ) {
        let lea = mir::Opcode::new(names.intern("x64.lea_64"));
        let jump = mir::Opcode::new(names.intern("x64.jmp_reg"));
        let address = func.new_vreg(GPR);
        func.build(from, lea).def(address, GPR).mem(mir::Mem::block(to)).finish();
        func.build(from, jump).operand(mir::Operand::read(address, GPR)).finish();
        func.succs_mut(from).push(mir::BlockCall::with(to, args));
    }

    #[test]
    fn a_label_that_hands_on_what_it_was_given_reads_it_where_it_arrives() {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let head = func.create_block();
        let label = func.create_block();
        let state = func.append_param(head, GPR);
        let given = func.append_param(label, GPR);
        // The label reads what it was given and then hands it on to itself, which is what every
        // handler of an interpreter does with the state it is not changing.
        let test = mir::Opcode::new(names.intern("x64.test_rr_64"));
        func.build(label, test).uses(given, GPR).uses(given, GPR).finish();
        dispatch(&mut func, &mut names, head, label, vec![state]);
        dispatch(&mut func, &mut names, label, label, vec![given]);

        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 1);
        // The label takes nothing now and reads the register the head writes, so the one move is
        // the head's and the label's own jump writes nothing.
        assert!(func[label].params.is_empty());
        assert!(!mentions(&func, given));
        let text = mir::print_func(&func, &names, &REGS);
        assert_eq!(text.matches("x64.mov_rr_64").count(), 1, "{text}");
    }

    /// What a [`stepping`] label reads between making the step and loading through it: nothing,
    /// the old pointer, or the old pointer and the step both.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Then {
        Nothing,
        Old,
        Both,
        Carry,
    }

    /// A label given a step pointer that steps it on and jumps where the next step says, which is
    /// `op++` and `goto *op->opcode`, reading what `then` says in between.
    fn stepping(then: Then) -> (Interner, mir::Func, mir::Reg) {
        stepping_by(then, false)
    }

    /// The same, with the step the `add` selection makes of `op++` when `added` says so.
    fn stepping_by(then: Then, added: bool) -> (Interner, mir::Func, mir::Reg) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let head = func.create_block();
        let label = func.create_block();
        let state = func.append_param(head, GPR);
        let given = func.append_param(label, GPR);
        dispatch(&mut func, &mut names, head, label, vec![state]);
        let lea = mir::Opcode::new(names.intern("x64.lea_64"));
        let load = mir::Opcode::new(names.intern("x64.mov_rm_64"));
        let test = mir::Opcode::new(names.intern("x64.test_rr_64"));
        let jump = mir::Opcode::new(names.intern("x64.jmp_reg"));
        let next = func.new_vreg(GPR);
        let to = func.new_vreg(GPR);
        if added {
            let add = mir::Opcode::new(names.intern("x64.add_ri_64"));
            let answer = mir::Operand::write(next, GPR).with(mir::Constraint::Reuse(1));
            func.build(label, add).operand(answer).uses(given, GPR).imm(24).finish();
        } else {
            let stepped = mir::Mem { disp: 24, ..mir::Mem::at(mir::Operand::read(given, GPR)) };
            func.build(label, lea).def(next, GPR).mem(stepped).finish();
        }
        match then {
            Then::Nothing => {}
            Then::Old => {
                func.build(label, test).uses(given, GPR).uses(given, GPR).finish();
            }
            Then::Both => {
                func.build(label, test).uses(given, GPR).uses(next, GPR).finish();
            }
            Then::Carry => {
                let carry = mir::Opcode::new(names.intern("x64.adc_ri_64"));
                let (old, sum) = (func.new_vreg(GPR), func.new_vreg(GPR));
                let answer = mir::Operand::write(sum, GPR).with(mir::Constraint::Reuse(1));
                func.build(label, carry).operand(answer).uses(old, GPR).imm(0).finish();
                func.build(label, test).uses(given, GPR).uses(given, GPR).finish();
            }
        }
        let at = mir::Mem::at(mir::Operand::read(next, GPR));
        func.build(label, load).def(to, GPR).mem(at).finish();
        func.build(label, jump).operand(mir::Operand::read(to, GPR)).finish();
        func.succs_mut(label).push(mir::BlockCall::with(label, vec![next]));
        (names, func, next)
    }

    #[test]
    fn a_step_a_label_makes_for_the_next_is_made_in_the_register_the_next_reads() {
        let (mut names, mut func, next) = stepping(Then::Nothing);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 1);
        // The `lea` writes the register the label reads the pointer in, so the label's jump has no
        // move in front of it and the head's is the only one.
        assert!(!mentions(&func, next));
        let text = mir::print_func(&func, &names, &REGS);
        assert_eq!(text.matches("x64.mov_rr_64").count(), 1, "{text}");
        // And tied to the register it steps on from, as `addq $24, %rcx` would be.
        assert!(text.contains("(reuse 1) = x64.lea_64"), "{text}");
    }

    #[test]
    fn a_step_made_while_the_old_pointer_is_still_wanted_is_made_after_it() {
        let (mut names, mut func, next) = stepping(Then::Old);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 1);
        // The `test` reads the old pointer after the step is made and nothing reads the step
        // before it, so the `lea` goes after the `test` and writes the register itself.
        assert!(!mentions(&func, next));
        let text = mir::print_func(&func, &names, &REGS);
        assert_eq!(text.matches("x64.mov_rr_64").count(), 1, "{text}");
        // The last `lea`, since the head has one of its own for the address it jumps to.
        let (test, lea) = (text.find("x64.test_rr_64"), text.rfind("x64.lea_64"));
        assert!(test.zip(lea).is_some_and(|(test, lea)| test < lea), "{text}");
    }

    #[test]
    fn a_step_added_while_the_old_pointer_is_still_wanted_is_made_after_it_as_a_lea() {
        let (mut names, mut func, next) = stepping_by(Then::Old, true);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 1);
        // The `add` cannot go past the `test`, which writes the flags it wrote, but the `lea` that
        // is the same sum can, and then it writes the register the label reads the pointer in.
        assert!(!mentions(&func, next));
        let text = mir::print_func(&func, &names, &REGS);
        assert!(!text.contains("x64.add_ri_64"), "{text}");
        assert_eq!(text.matches("x64.mov_rr_64").count(), 1, "{text}");
        assert!(text.contains("(reuse 1) = x64.lea_64"), "{text}");
        let (test, lea) = (text.find("x64.test_rr_64"), text.rfind("x64.lea_64"));
        assert!(test.zip(lea).is_some_and(|(test, lea)| test < lea), "{text}");
    }

    #[test]
    fn a_step_added_where_the_next_instruction_reads_its_carry_is_left_an_add() {
        let (mut names, mut func, next) = stepping_by(Then::Carry, true);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 1);
        // The `adc` reads the carry the `add` wrote, so a `lea` would hand it another one, and the
        // `add` stays where it is with the move in front of the jump.
        assert!(mentions(&func, next));
        let text = mir::print_func(&func, &names, &REGS);
        assert!(text.contains("x64.add_ri_64"), "{text}");
        assert_eq!(text.matches("x64.mov_rr_64").count(), 2, "{text}");
    }

    #[test]
    fn a_step_read_while_the_old_pointer_is_still_wanted_is_moved_across() {
        let (mut names, mut func, next) = stepping(Then::Both);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 1);
        // The `test` reads the old pointer and the step both, so the step cannot go into the
        // register the old pointer is in until the move in front of the jump.
        assert!(mentions(&func, next));
        let text = mir::print_func(&func, &names, &REGS);
        assert_eq!(text.matches("x64.mov_rr_64").count(), 2, "{text}");
    }

    #[test]
    fn a_value_wanted_past_a_jump_keeps_a_parameter_of_its_own() {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let head = func.create_block();
        let first = func.create_block();
        let second = func.create_block();
        let state = func.append_param(head, GPR);
        let kept = func.append_param(first, GPR);
        let other = func.new_vreg(GPR);
        let given = func.append_param(second, GPR);
        dispatch(&mut func, &mut names, head, first, vec![state]);
        let lea = mir::Opcode::new(names.intern("x64.lea_64"));
        func.build(first, lea).def(other, GPR).mem(mir::Mem::block(second)).finish();
        dispatch(&mut func, &mut names, first, second, vec![other]);
        // The second label reads what the first was given, so that value is still wanted after the
        // first label's jump, and the jump writes the register the second label is given in.
        let test = mir::Opcode::new(names.intern("x64.test_rr_64"));
        func.build(second, test).uses(kept, GPR).uses(given, GPR).finish();

        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 2);
        assert_eq!(func[first].params.len(), 1);
        assert!(func[second].params.is_empty());
    }

    #[test]
    fn a_label_arrived_at_some_other_way_keeps_its_parameter() {
        let (mut names, mut func) = computed(1, 1);
        let label = mir::Block::new(1);
        let other = func.create_block();
        let arg = func.append_param(other, GPR);
        *func.succs_mut(other) = vec![mir::BlockCall::with(label, vec![arg])];

        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 1);
        // An ordinary edge gives the label its value too, and that edge writes the parameter and
        // not the register the branch writes.
        assert_eq!(func[label].params.len(), 1);
    }

    #[test]
    fn an_edge_out_of_a_computed_goto_that_carries_nothing_is_left_alone() {
        let (mut names, mut func) = computed(1, 0);

        // No values to carry, so no block to carry them, and the address stays the label's own.
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 0);
        assert_eq!(addressed(&func), vec![1]);
    }

    #[test]
    fn a_function_with_no_computed_goto_in_it_is_left_alone() {
        let (mut names, mut func, _) = diamond(1);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names), 0);
        assert_eq!(edges(&func), vec![vec![1, 2], vec![3], vec![3], vec![]]);
    }

    #[test]
    fn what_it_leaves_is_nothing_for_the_splitting_below_to_do() {
        let (mut names, mut func) = computed(2, 1);
        indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names);
        // The edges out of the branches carry nothing now, and the edges out of the blocks it
        // added are the only way out of those blocks, so neither kind is critical.
        assert_eq!(critical(&mut func), 0);
    }

    /// The first instruction of each block, by opcode, and an empty string for a block with
    /// nothing in it.
    fn opens(func: &mir::Func, names: &Interner) -> Vec<String> {
        func.blocks()
            .map(|block| match func.insts(block).next() {
                Some(inst) => names.resolve(func[inst].opcode.name()).to_owned(),
                None => String::new(),
            })
            .collect()
    }

    #[test]
    fn the_block_a_label_begins_at_gets_a_landing_pad_when_the_forward_edge_is_checked() {
        let (mut names, mut func) = computed(2, 1);
        indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names);

        // One pad, at the block in front of the label, because that is the block both addresses
        // name once the values have been moved on to it. The label's own block is arrived at by an
        // ordinary edge from there and wants nothing.
        assert_eq!(pads(&mut func, &FRAME, FRAME.landing, &mut names), 1);
        assert_eq!(opens(&func, &names), ["", "", "x64.lea_64", "x64.lea_64", "x64.endbr64"]);
    }

    #[test]
    fn a_label_with_no_block_in_front_of_it_gets_the_pad_itself() {
        let (mut names, mut func) = computed(1, 0);
        indirect(&mut func, &BRANCH, &FRAME, &SHORT, &FLAGS, &mut names);

        // Nothing was moved on to anything, so the address still names the label and the pad goes
        // where the address goes.
        assert_eq!(pads(&mut func, &FRAME, FRAME.landing, &mut names), 1);
        assert_eq!(opens(&func, &names), ["x64.lea_64", "x64.endbr64"]);
    }

    #[test]
    fn a_label_only_an_image_points_at_gets_a_landing_pad() {
        let (mut names, mut func) = computed(1, 0);
        let (head, label) = {
            let mut blocks = func.blocks();
            (blocks.next().expect("a head"), blocks.next().expect("a label"))
        };
        // The image names the head too, which no instruction does, and the label once more, which
        // is still one pad.
        let name = names.intern("f.label");
        func.labels.push((head, name));
        func.labels.push((label, name));
        assert_eq!(pads(&mut func, &FRAME, FRAME.landing, &mut names), 2);
        assert_eq!(opens(&func, &names), ["x64.endbr64", "x64.endbr64"]);
    }

    #[test]
    fn nothing_is_written_when_the_forward_edge_is_not_checked() {
        let (mut names, mut func) = computed(1, 0);
        assert_eq!(pads(&mut func, &FRAME, None, &mut names), 0);
        assert_eq!(opens(&func, &names), ["x64.lea_64", ""]);
    }
}
