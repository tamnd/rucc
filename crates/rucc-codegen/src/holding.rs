//! Which of its values a declaration holds on the way into each block.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! A local the program writes more than once is behind a value for each write, and two of them can
//! both be live into one block: the value from the last trip round a loop is still read after the
//! one computed on this trip has been written, and both of them are the local. Their stretches then
//! start at the same address and say different things, and nothing in the stretches says which of
//! the two the local is. What does is the order the assignments ran in, which is a question about
//! the program rather than about the registers, so it is asked of the IR before selection.
//!
//! The answer is the value the last assignment on every path into the block gave the declaration.
//! An assignment is where a named value was computed, a block parameter a declaration is the value
//! of, or a start [`rucc_ir::Func::start_place`] says is still somewhere. A block whose paths in
//! disagree, or one no path reaches, has no answer, and the back end reads that as nothing to
//! choose by rather than as a choice.

use std::collections::HashMap;

use rucc_ir::{Block, Def, Func, Inst, Value};

/// The value each declaration that has more than one of them holds at the top of each block, where
/// every path in agrees, as the declaration, the block and the value, sorted.
///
/// Only a declaration with more than one value is asked about, since a declaration with one cannot
/// be holding the wrong one of them, and the list would otherwise be every local times every block.
#[must_use]
pub fn on_entry(func: &Func) -> Vec<(u32, Block, Value)> {
    let mut assigned: HashMap<u32, Vec<(Block, Option<Inst>, Value)>> = HashMap::new();
    for value in func.values() {
        let place = match func[value].def {
            Def::Result { inst, .. } => func.block_of(inst).map(|block| (block, Some(inst))),
            Def::Param { block, index } => {
                let param = usize::try_from(index)
                    .ok()
                    .and_then(|index| func[block].params.get(index).copied());
                (func.is_placed(block) && param == Some(value)).then_some((block, None))
            }
        };
        if let Some((block, after)) = place {
            for decl in func.value_decls(value) {
                assigned.entry(decl).or_default().push((block, after, value));
            }
        }
        for start in func.value_starts(value) {
            if let Some((block, after)) = func.start_place(start) {
                assigned.entry(start.decl).or_default().push((block, after, value));
            }
        }
    }
    assigned.retain(|_, all| all.iter().any(|&(_, _, value)| value != all[0].2));
    if assigned.is_empty() {
        return Vec::new();
    }

    let blocks: Vec<Block> = func.blocks().collect();
    let count = func.counts().blocks;
    // Where each instruction is in its block, one past the top so that the top itself is nought.
    let mut position = vec![0usize; func.counts().insts];
    let mut preds: Vec<Vec<Block>> = vec![Vec::new(); count];
    for &block in &blocks {
        for (at, inst) in func.insts(block).enumerate() {
            position[inst.index()] = at + 1;
        }
        if let Some(terminator) = func.terminator(block) {
            for call in func.successors(terminator) {
                preds[call.block.index()].push(block);
            }
        }
    }
    let entry = func.entry();

    let mut decls: Vec<u32> = assigned.keys().copied().collect();
    decls.sort_unstable();
    let mut out = Vec::new();
    for decl in decls {
        // What the last assignment in each block gave the declaration, and the ones at the top of
        // it, the parameters and a start before everything in it. Two at the same place giving it
        // different values are two assignments this cannot put in order, and the answer is none.
        let mut last: Vec<Option<(usize, Held)>> = vec![None; count];
        let mut top: Vec<Option<Held>> = vec![None; count];
        for &(block, after, value) in &assigned[&decl] {
            let at = after.map_or(0, |inst| position[inst.index()]);
            let slot = &mut last[block.index()];
            *slot = match *slot {
                Some((have, _)) if have > at => *slot,
                Some((have, held)) if have == at => Some((at, held.meet(Held::Known(value)))),
                _ => Some((at, Held::Known(value))),
            };
            if at == 0 {
                let slot = &mut top[block.index()];
                *slot = Some(slot.map_or(Held::Known(value), |held| held.meet(Held::Known(value))));
            }
        }
        // Forward to a fixed point. Every block starts unvisited, the entry starts with nothing
        // said, and a block's way in is what all of its predecessors agree on at their way out.
        let mut into: Vec<Held> = vec![Held::Unvisited; count];
        let mut changed = true;
        while changed {
            changed = false;
            for &block in &blocks {
                let now = if Some(block) == entry {
                    Held::Unknown
                } else {
                    preds[block.index()].iter().fold(Held::Unvisited, |held, &pred| {
                        let out = last[pred.index()].map_or(into[pred.index()], |(_, held)| held);
                        held.meet(out)
                    })
                };
                if now != into[block.index()] {
                    into[block.index()] = now;
                    changed = true;
                }
            }
        }
        for &block in &blocks {
            let held = top[block.index()].unwrap_or(into[block.index()]);
            if let Held::Known(value) = held {
                out.push((decl, block, value));
            }
        }
    }
    out
}

/// What a declaration is known to hold at one place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// Nothing has reached here yet.
    Unvisited,
    /// This value, on every path that has.
    Known(Value),
    /// Different values on different paths, or nothing said at all.
    Unknown,
}

impl Held {
    /// What two paths that meet agree on.
    fn meet(self, other: Held) -> Held {
        match (self, other) {
            (Held::Unvisited, held) | (held, Held::Unvisited) => held,
            (Held::Known(one), Held::Known(two)) if one == two => self,
            _ => Held::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Symbol;
    use rucc_ir::{Builder, Signature, Start, Type};

    use super::*;

    /// A loop the way the lowering leaves one: the header takes the declaration's value as a
    /// parameter, the body computes the next one, and a block after the body reads both.
    ///
    /// `entry -> head(i) -> body -> after -> head`, with `after` also leaving. The body is where
    /// `next` is computed and named, so in `after` the declaration is `next` even though `i` is
    /// still live there, and in `head` it is the parameter.
    #[test]
    fn a_value_computed_on_this_trip_is_what_the_blocks_after_it_hold() {
        let mut func = Func::new(Symbol::from_raw(0), Signature::new());
        let entry = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let after = func.create_block();
        let exit = func.create_block();
        let i = func.append_param(head, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(head, &[zero]);
        Builder::new(&mut func, head).jump(body, &[]);
        let mut build = Builder::new(&mut func, body);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(rucc_ir::Opcode::Add, i, one, rucc_ir::Flags::NONE);
        build.jump(after, &[]);
        let mut build = Builder::new(&mut func, after);
        let done = build.icmp(rucc_ir::IntPred::Eq, i, next);
        build.br_if(done, exit, &[], head, &[next]);
        Builder::new(&mut func, exit).ret(&[]);
        for value in [zero, i, next] {
            func.declare_value(value, 3);
        }

        let held = on_entry(&func);
        assert!(held.contains(&(3, head, i)), "{held:?}");
        assert!(held.contains(&(3, body, i)), "{held:?}");
        assert!(held.contains(&(3, after, next)), "{held:?}");
        assert!(held.contains(&(3, exit, next)), "{held:?}");
        assert!(!held.iter().any(|&(_, block, _)| block == entry), "{held:?}");
    }

    /// Two arms that give a declaration different values leave the block they meet in with no
    /// answer, and a start that gives it one of them again on the way in is what it holds.
    #[test]
    fn arms_that_disagree_say_nothing_and_a_start_says_what_it_gave() {
        let mut func = Func::new(Symbol::from_raw(0), Signature::new());
        let entry = func.create_block();
        let left = func.create_block();
        let right = func.create_block();
        let join = func.create_block();
        let flag = func.append_param(entry, Type::I1);
        let mut build = Builder::new(&mut func, entry);
        let first = build.iconst(Type::int(32), 1);
        build.br_if(flag, left, &[], right, &[]);
        let mut build = Builder::new(&mut func, left);
        let second = build.iconst(Type::int(32), 2);
        let saved = build.iconst(Type::int(32), 3);
        build.jump(join, &[]);
        Builder::new(&mut func, right).jump(join, &[]);
        Builder::new(&mut func, join).ret(&[]);
        func.declare_value(first, 5);
        func.declare_value(second, 5);

        let held = on_entry(&func);
        assert!(held.contains(&(5, left, first)), "{held:?}");
        assert!(held.contains(&(5, right, first)), "{held:?}");
        assert!(!held.iter().any(|&(_, block, _)| block == join), "{held:?}");

        // `x = saved;` at the end of the left arm, where saved is the first value again, so both
        // arms hand the join the first. A declaration with the one value is not asked about.
        let Def::Result { inst: loaded, .. } = func[saved].def else { unreachable!() };
        func.declare_value_from(first, Start { decl: 5, block: left, after: Some(loaded) });
        func.declare_value_from(second, Start { decl: 6, block: left, after: None });
        let held = on_entry(&func);
        assert!(held.contains(&(5, join, first)), "{held:?}");
        assert!(held.contains(&(5, left, first)), "{held:?}");
        assert!(!held.iter().any(|&(decl, _, _)| decl == 6), "{held:?}");
    }
}
