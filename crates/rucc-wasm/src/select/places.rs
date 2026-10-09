//! Where the declarations of the source are in the code of a function, for `-g`.
//!
//! A declaration in the frame is at its offset from the frame pointer over all of the function, at
//! each optimization level. [`Lower::plan`] gives each fixed `alloca` a slot of its own, and the
//! frame pointer does not change after the prologue.
//!
//! At `-O0`, [`Lower::assign`] gives each value a local of its own, and nothing writes the local but
//! the code of the value, or the edges into the block for a block parameter. So the local holds the
//! value from where it is written to the end of the function, and the question is only which of the
//! values of the declaration it is at each place. A declaration with one value, which it is given
//! where the value is made, is in the local of that value over all of the function. Before the
//! value is made, the local holds zero, which is what a local that the program did not initialize
//! can hold.
//!
//! Above `-O0`, [`Lower::color`] gives one local to values that are not live at the same time, so a
//! local holds a value only over a part of the code. The selector then keeps each `local.set` and
//! `local.tee` with the value it writes, and the coloring keeps the values that are live at the top
//! of each block. A declaration with one value is in its local over all of the function when no
//! other value is written to that local, as at `-O0`. For the others, each stretch that the walk
//! below finds is cut to the code where the local holds the value. See [`Lower::held`].
//!
//! For the others, the code of each block is walked from its top. The declaration holds what
//! [`rucc_ir::holding::on_entry`] says at the top, and each assignment in the block changes it at
//! the end of the code of its instruction. The walk stops where the terminator starts, because the
//! code of the terminator holds the blocks that the structure puts inside it, and those blocks have
//! walks of their own.

use rucc_ir::holding;
use rucc_ir::{Block, Extra, Inst, Value};

use super::Lower;
use crate::{Kept, Spot, is_pair};

/// A place in the code that the walk measures by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    /// The top of the code of a block.
    Top(Block),
    /// The end of the code of an instruction.
    After(Inst),
    /// The start of the code of the terminator of a block.
    Term(Block),
}

impl Lower<'_, '_> {
    /// Where each declaration is, with the offsets from the start of the code. `marks` are the
    /// offsets of the marks in the code, when the selector kept them.
    pub(super) fn places(&self, marks: Option<&[(u32, Mark)]>) -> Vec<Kept> {
        let func = self.func;
        let mut out = Vec::new();
        for (&inst, &offset) in &self.frame.slots {
            let Extra::Mem(mem) = func[inst].extra else { continue };
            let at = Spot::Frame(offset);
            out.extend(func.mem_decls(mem).map(|decl| Kept { decl, at, over: None }));
        }
        let Some(marks) = marks else { return out };

        let count = func.counts();
        let mut top = vec![None; count.blocks];
        let mut term = vec![None; count.blocks];
        let mut after = vec![None; count.insts];
        for &(at, mark) in marks {
            match mark {
                Mark::Top(block) => top[block.index()] = Some(at),
                Mark::Term(block) => term[block.index()] = Some(at),
                Mark::After(inst) => after[inst.index()] = Some(at),
            }
        }
        let assigned = holding::assignments(func);
        let mut decls: Vec<u32> = assigned.keys().copied().collect();
        decls.sort_unstable();
        let mut entries = None;
        for decl in decls {
            let all = &assigned[&decl];
            // One value, given to the declaration where the value is made, in a local that no
            // other value is written to.
            let first = all[0].2;
            if self.alone(first)
                && all.iter().all(|&(_, _, value)| value == first)
                && func.value_decls(first).any(|have| have == decl)
            {
                if let Some(at) = self.place(first) {
                    out.push(Kept { decl, at, over: None });
                }
                continue;
            }
            let entries = entries.get_or_insert_with(|| holding::on_entry(func));
            let start = entries.partition_point(|&(have, _, _)| have < decl);
            let end = entries.partition_point(|&(have, _, _)| have <= decl);
            let entries = &entries[start..end];
            let mut blocks: Vec<Block> = entries.iter().map(|&(_, block, _)| block).collect();
            blocks.extend(all.iter().map(|&(block, _, _)| block));
            blocks.sort_unstable();
            blocks.dedup();
            let mut spans: Vec<(u32, u32, Spot)> = Vec::new();
            for block in blocks {
                let (Some(start), Some(to)) = (top[block.index()], term[block.index()]) else {
                    continue;
                };
                let found = entries.binary_search_by_key(&block, |&(_, have, _)| have);
                let mut held = found.ok().map(|at| entries[at].2);
                // The assignments at the top of the block, which are its parameters and the starts
                // before its first instruction. Two of them that disagree say nothing.
                let tops: Vec<Value> = all
                    .iter()
                    .filter(|&&(have, after, _)| have == block && after.is_none())
                    .map(|&(_, _, value)| value)
                    .collect();
                if let Some(&value) = tops.first() {
                    held = tops.iter().all(|&other| other == value).then_some(value);
                }
                // The assignments after an instruction, in the order of the code. An instruction
                // with no mark is one that is written somewhere else, and its assignment is left
                // out.
                let mut changes: Vec<(u32, usize, Value)> = all
                    .iter()
                    .filter(|&&(have, _, _)| have == block)
                    .filter_map(|&(_, after_inst, value)| {
                        let inst = after_inst?;
                        let at = after[inst.index()]?;
                        let position = func.insts(block).position(|have| have == inst)?;
                        Some((at, position, value))
                    })
                    .collect();
                changes.sort_unstable_by_key(|&(at, position, _)| (at, position));
                let mut from = start;
                for (at, _, value) in changes {
                    let at = at.min(to);
                    if let Some(held) = held {
                        self.held(block, held, (start, to), (from, at), &mut spans);
                    }
                    held = Some(value);
                    from = at;
                }
                if let Some(held) = held {
                    self.held(block, held, (start, to), (from, to), &mut spans);
                }
            }
            spans.retain(|&(from, to, _)| to > from);
            spans.sort_unstable_by_key(|&(from, _, _)| from);
            // Two stretches that meet in one place are one stretch.
            let mut joined: Vec<(u32, u32, Spot)> = Vec::with_capacity(spans.len());
            for span in spans {
                match joined.last_mut() {
                    Some(last) if last.1 == span.0 && last.2 == span.2 => last.1 = span.1,
                    _ => joined.push(span),
                }
            }
            out.extend(joined.into_iter().map(|(from, to, at)| Kept {
                decl,
                at,
                over: Some((from, to - from)),
            }));
        }
        out
    }

    /// Add the stretches of `over` where the place of `value` holds it, in `block`, whose code
    /// before its terminator is `code`.
    ///
    /// At `-O0` that is all of `over`. Above it, the local of the value holds it at the top of the
    /// block when the value is live there or is a parameter of the block, because the coloring
    /// gives the local to no other value that is live at the same time, and the edges write the
    /// parameters. After that, the local holds the value from the end of each write of the value to
    /// the start of the next write of something else.
    fn held(
        &self,
        block: Block,
        value: Value,
        code: (u32, u32),
        over: (u32, u32),
        spans: &mut Vec<(u32, u32, Spot)>,
    ) {
        let Some(place) = self.place(value) else { return };
        let mut push = |from: u32, to: u32| {
            let (from, to) = (from.max(over.0), to.min(over.1));
            if to > from {
                spans.push((from, to, place));
            }
        };
        let (Spot::Local(local), Some(writes)) = (place, self.code.writes.as_deref()) else {
            push(over.0, over.1);
            return;
        };
        let live =
            self.entering.contains(&(block, value)) || self.func[block].params.contains(&value);
        let mut since = live.then_some(code.0);
        let first = writes.partition_point(|write| write.from < code.0);
        for write in writes[first..].iter().take_while(|write| write.from < code.1) {
            if write.local != local {
                continue;
            }
            match (since, write.value == Some(value)) {
                (Some(from), false) => {
                    push(from, write.from);
                    since = None;
                }
                (None, true) => since = Some(write.to),
                _ => {}
            }
        }
        if let Some(from) = since {
            push(from, code.1);
        }
    }

    /// Whether nothing but `value` is written to the local of `value`. At `-O0` that is true of
    /// each value.
    fn alone(&self, value: Value) -> bool {
        let (Some(Spot::Local(local)), Some(writes)) = (self.place(value), &self.code.writes)
        else {
            return true;
        };
        writes.iter().all(|write| write.local != local || write.value == Some(value))
    }

    /// Where a value is: in its local, or nowhere for a constant, which is its number.
    /// A pair is in two locals, and DWARF needs pieces to say that, so it has no place.
    fn place(&self, value: Value) -> Option<Spot> {
        if is_pair(self.func[value].ty) {
            return None;
        }
        if let Some(&local) = self.local.get(&value) {
            return Some(Spot::Local(local));
        }
        self.constant(value).map(|bits| Spot::Constant(bits as u64))
    }
}
