//! Copying a set of blocks, which is what a pass that wants two of something builds on.
//!
//! Unrolling copies a body once per iteration. Splitting a loop, which is section 7.4 of
//! `spec/safe-memory/07-check-elimination.md`, copies it once and takes the checks out of one of the
//! two. Both want the same thing underneath and getting it wrong looks the same in both: an operand
//! that still names the original, a block call that still goes to the original, a value read before
//! anything gave the copy a name for it. That is worth writing once.
//!
//! What this does not do is decide anything. It does not say which blocks are worth copying, it does
//! not look at the terminators it copies beyond sending them to the right blocks, and it leaves the
//! copy's edges pointing wherever the original's pointed. The caller wires the result up, because
//! wiring is the part that differs and it is the part the caller is a pass about.
//!
//! # Reading the substitution
//!
//! The caller hands in a map from values to values and gets it back filled in. Two things go in it.
//!
//! Anything already in it on the way in is what the copy reads in place of the original, and a block
//! parameter that is already in it is given no parameter of its own on the copy. That is how a caller
//! says the copy is reached from exactly one place and should read that place's values directly,
//! which is what unrolling wants for the header of each copy after the first.
//!
//! Everything else the blocks define is added on the way out, old name to new. A parameter with
//! nothing said about it gets a fresh parameter on the copy, and every instruction result gets a
//! fresh result. So after this returns the map answers, for any value the copied blocks defined, what
//! the copy calls it.
//!
//! # Why the operands are settled last
//!
//! A block list says nothing about which block makes a value and which one reads it, so an operand
//! settled while the copy was being made would sometimes have been settled too early and kept a name
//! that does not reach it. Every value the copy makes has a name once the copying is finished, so
//! that is when the operands are walked, once each, over the original names the copies are still
//! holding.

use std::collections::HashMap;

use rucc_ir::{Block, BlockCall, Extra, ExtraKind, Func, Inst, InstData, Type, Value, ValueList};

/// Copies `body` into fresh blocks, filling `map` in with what the copy calls everything it defines.
///
/// The answer maps each block in `body` to its copy. A target that is in `body` becomes the copy's
/// own block and a target outside it stays where it is, so the copy leaves the region in the same
/// places the original does and goes round inside itself rather than round the original.
///
/// The copies come out in the order `body` gives, and a block that appears twice is copied twice,
/// which is a caller passing the same block twice rather than anything this has an opinion about.
pub(crate) fn blocks(
    func: &mut Func,
    body: &[Block],
    map: &mut HashMap<Value, Value>,
) -> HashMap<Block, Block> {
    let mut copies: HashMap<Block, Block> = HashMap::new();
    for &block in body {
        copies.insert(block, func.create_block());
    }
    for &block in body {
        let copy = copies[&block];
        for param in func[block].params.clone() {
            if map.contains_key(&param) {
                continue;
            }
            let fresh = func.append_param(copy, func[param].ty);
            map.insert(param, fresh);
        }
    }
    // A snapshot, because these are blocks this writes to and a round that read one live would copy
    // an instruction a round before it wrote.
    let insts: Vec<(Block, Vec<Inst>)> =
        body.iter().map(|&block| (block, func.insts(block).collect())).collect();
    let mut copied: Vec<Inst> = Vec::new();
    for (block, insts) in &insts {
        let into = copies[block];
        for &inst in insts {
            copied.push(one(func, into, inst, map, &copies));
        }
    }
    for inst in copied {
        let args = func[inst].args;
        func.rewrite(args, |value| map.get(&value).copied().unwrap_or(value));
        let edges: Vec<ValueList> = func.successors(inst).map(|call| call.args).collect();
        for edge in edges {
            func.rewrite(edge, |value| map.get(&value).copied().unwrap_or(value));
        }
    }
    copies
}

/// Copies one instruction to the end of a block, records its results, and remaps where it branches.
///
/// What it reads is left exactly as the original read it, for [`blocks`] to settle once the whole
/// copy is there.
fn one(
    func: &mut Func,
    into: Block,
    inst: Inst,
    map: &mut HashMap<Value, Value>,
    blocks: &HashMap<Block, Block>,
) -> Inst {
    let data = func[inst];
    let args: Vec<Value> = func[data.args].to_vec();
    let edges: Vec<(Block, Vec<Value>)> = func
        .successors(inst)
        .map(|call| {
            let block = blocks.get(&call.block).copied().unwrap_or(call.block);
            (block, func[call.args].to_vec())
        })
        .collect();
    let extra = match data.extra {
        Extra::Targets(_) => {
            let calls: Vec<BlockCall> = edges
                .iter()
                .map(|(block, args)| BlockCall { block: *block, args: func.push_values(args) })
                .collect();
            Extra::Targets(func.push_block_calls(&calls))
        }
        // Anything else names no block, a return and an unreachable among them.
        other => other,
    };
    let types: Vec<Type> = data.results().map(|result| func[result].ty).collect();
    let span = func.span(inst);
    let args = func.push_values(&args);
    let fresh = func.create_inst(InstData { args, extra, ..data }, &types, span);
    func.append_inst(into, fresh);
    for (old, new) in data.results().zip(func[fresh].results()) {
        map.insert(old, new);
    }
    fresh
}

/// Whether this instruction is one [`blocks`] can copy.
///
/// A caller has to ask, because the answer is no for three of them and a copy made anyway would be
/// wrong rather than merely worse. A `switch` and an `inline_asm` with labels on it name their blocks
/// in a side table this does not remap, so the copy would branch into the original. A `va_object`
/// carries a layout that is written once per function and a second one would be a second reading of
/// the same argument list.
pub(crate) fn copyable(func: &Func, inst: Inst) -> bool {
    !matches!(func[inst].extra.kind(), ExtraKind::Switch | ExtraKind::Asm | ExtraKind::VaObject)
}

/// The arguments a terminator hands the target it shares with this block.
///
/// Empty when the two share no edge, which is a caller asking about a block that is not a successor
/// and is worth no more than the empty list a successor with no arguments would give.
pub(crate) fn edge_args(func: &Func, term: Inst, to: Block) -> Vec<Value> {
    for call in func.successors(term) {
        if call.block == to {
            return func[call.args].to_vec();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rucc_base::Interner;
    use rucc_ir::{
        Builder, Flags, Func, IntPred, Module, Opcode, Signature, Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::blocks;

    /// A function of two blocks, the second reading a parameter the first hands it.
    ///
    /// `block0(%0: i32)` jumps to `block1(%0)`, and `block1(%1: i32)` adds one and returns the sum.
    /// Small on purpose: what these tests are about is naming, not shape.
    fn pair(names: &mut Interner) -> Func {
        let i32_ = Type::int(32);
        let sig = Signature::new().with_params(&[i32_]).with_returns(&[i32_]);
        let mut func = Func::new(names.intern("pair"), sig);
        let entry = func.create_block();
        let arg = func.append_param(entry, i32_);
        let body = func.create_block();
        let held = func.append_param(body, i32_);
        Builder::new(&mut func, entry).jump(body, &[arg]);
        let mut build = Builder::new(&mut func, body);
        let one = build.iconst(i32_, 1);
        let sum = build.binary(Opcode::Add, held, one, Flags::NSW);
        build.ret(&[sum]);
        func
    }

    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    #[test]
    fn every_value_the_copy_defines_has_a_fresh_name_for_it() {
        let mut names = Interner::new();
        let mut func = pair(&mut names);
        let body: Vec<_> = func.blocks().collect();
        let defined: Vec<Value> = body
            .iter()
            .flat_map(|&block| {
                let params = func[block].params.clone();
                let results: Vec<Value> =
                    func.insts(block).flat_map(|inst| func[inst].results()).collect();
                params.into_iter().chain(results)
            })
            .collect();
        let mut map = HashMap::new();
        let copies = blocks(&mut func, &body, &mut map);
        assert_eq!(copies.len(), body.len());
        for value in defined {
            let fresh = map.get(&value).copied().expect("a name for everything the copy defines");
            assert_ne!(fresh, value, "the copy is not the original");
        }
    }

    #[test]
    fn the_copy_goes_round_inside_itself_rather_than_back_to_the_original() {
        let mut names = Interner::new();
        let mut func = pair(&mut names);
        let body: Vec<_> = func.blocks().collect();
        let mut map = HashMap::new();
        let copies = blocks(&mut func, &body, &mut map);
        let entry = copies[&body[0]];
        let term = func.terminator(entry).expect("the copied jump");
        let targets: Vec<_> = func.successors(term).map(|call| call.block).collect();
        assert_eq!(targets, vec![copies[&body[1]]]);
    }

    #[test]
    fn a_parameter_the_caller_has_spoken_for_is_not_given_one_on_the_copy() {
        let mut names = Interner::new();
        let mut func = pair(&mut names);
        let body: Vec<_> = func.blocks().collect();
        let held = func[body[1]].params[0];
        let seven = Builder::new(&mut func, body[0]).iconst(Type::int(32), 7);
        let mut map = HashMap::from([(held, seven)]);
        let copies = blocks(&mut func, &body, &mut map);
        assert!(func[copies[&body[1]]].params.is_empty(), "spoken for, so no parameter of its own");
        assert_eq!(map[&held], seven, "and it still reads what it was told to read");
        let copy = copies[&body[1]];
        let add = func.insts(copy).find(|&inst| func[inst].opcode == Opcode::Add).expect("the add");
        assert!(func[func[add].args].contains(&seven), "including in the copy of the add");
    }

    #[test]
    fn what_the_copy_reads_from_outside_it_is_left_alone() {
        let mut names = Interner::new();
        let mut func = pair(&mut names);
        // Only the second block is copied, so nothing outside it is renamed and the copy still has
        // a parameter of its own, since the caller said nothing about that one.
        let body = vec![func.blocks().nth(1).expect("two blocks")];
        let original = func[body[0]].params[0];
        let mut map = HashMap::new();
        let copies = blocks(&mut func, &body, &mut map);
        let copy = copies[&body[0]];
        let held = func[copy].params[0];
        let add = func.insts(copy).find(|&inst| func[inst].opcode == Opcode::Add).expect("the add");
        assert_ne!(held, original, "the copy has its own parameter");
        assert!(func[func[add].args].contains(&held), "and the add in it reads that one");
    }

    #[test]
    fn a_copied_loop_goes_round_itself_and_leaves_where_the_original_left() {
        let mut names = Interner::new();
        let i32_ = Type::int(32);
        let sig = Signature::new().with_params(&[i32_]).with_returns(&[i32_]);
        let mut func = Func::new(names.intern("count"), sig);
        let entry = func.create_block();
        let limit = func.append_param(entry, i32_);
        let header = func.create_block();
        let index = func.append_param(header, i32_);
        let done = func.create_block();
        let out = func.append_param(done, i32_);
        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(i32_, 0);
        build.jump(header, &[zero]);
        let mut build = Builder::new(&mut func, header);
        let one = build.iconst(i32_, 1);
        let next = build.binary(Opcode::Add, index, one, Flags::NSW);
        let test = build.icmp(IntPred::Slt, next, limit);
        build.br_if(test, header, &[next], done, &[next]);
        Builder::new(&mut func, done).ret(&[out]);
        sound(&func, &mut names);

        let body = vec![header];
        let mut map = HashMap::new();
        let copies = blocks(&mut func, &body, &mut map);
        let copy = copies[&header];
        let term = func.terminator(copy).expect("the copied test");
        let targets: Vec<_> = func.successors(term).map(|call| call.block).collect();
        assert_eq!(targets, vec![copy, done], "back to itself, out to where the original went");
        let carried = func.successors(term).next().expect("the back edge").args;
        assert_eq!(func[carried][0], map[&next], "and it carries its own value round");
        // Not verified afterwards on purpose. Nothing reaches the copy yet, and a block nothing
        // reaches is something the verifier refuses, which is section 6.5 putting the tidying on
        // whichever pass stranded the block rather than on a sweeper after it.
    }
}
