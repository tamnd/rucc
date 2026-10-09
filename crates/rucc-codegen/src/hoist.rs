//! Which addresses of names are written once in front of a loop rather than in every block of the
//! loop that reads them, which is tamnd/rucc#3185.
//!
//! [`crate::lower`] writes the address of a name again in each block that reads it, and that is the
//! right answer nearly everywhere. It is one instruction that reads nothing, and when one load or
//! store is all that reads it [`crate::fold`] puts the symbol into that instruction and there is no
//! `lea` left at all. Inside a loop that reads an array with an index it is the wrong answer. An
//! address relative to a symbol has no room for an index on any machine this compiler writes, so
//! the `lea` stays, and it runs on every trip for a value that is the same on every trip. gcc
//! takes it once in front of the loop and keeps it in a register. On the `division` cases of
//! rucc-corpus that `lea` was one instruction in six of the inner loop.
//!
//! So an address is held across a loop when three things are true. Something in the loop reads it
//! with an index. Nothing in the loop is a call, since a register held across a call is one the
//! callee saves, and the save and the restore cost more than the `lea` they would save. And the
//! loop is not full already: the most values live at any point in it, counting the addresses held
//! across it so far, is below the allocatable count less `LOOP_RESERVED_REGS`, which is the test
//! loop invariant motion makes in [`rucc_opt::Pressure::is_tight`]. Of the loops around the block
//! the address is taken in, it is the outermost one that passes, walking out from the innermost,
//! so the address of an array read in a nest is taken once in front of the whole nest, as gcc
//! writes it. Two addresses of one name held across one loop share a register.
//!
//! The middle end is not where this is decided, though hoisting is its job. Loop invariant motion
//! leaves the address of a name where it is on purpose, because whether holding one is worth a
//! register depends on whether [`crate::fold`] would have taken it into its reader, and that is a
//! question about the machine's addressing modes.

use rucc_base::Symbol;
use rucc_base::hash::Map;
use rucc_cost::{RegClass, heuristics};
use rucc_ir::{Block, Def, Extra, Func, Opcode, Value};
use rucc_opt::{Cfg, Dominators, Liveness, LoopId, Loops, Pressure};

/// The addresses of names one function holds across its loops.
#[derive(Debug)]
pub struct Hoisted {
    loops: Loops,
    /// Each address held, with the loop it is held across and the address of the same name whose
    /// register it shares, which is itself for the first of them.
    across: Map<Value, (LoopId, Value)>,
    /// The addresses taken at the end of each block in front of a loop, one for each name.
    ahead: Map<Block, Vec<Value>>,
}

impl Hoisted {
    /// Which of `names` to hold across a loop of `source`, on a machine that hands out `room`
    /// general purpose registers, or nothing when none of them is.
    ///
    /// `names` are the addresses of names [`crate::lower`] would write again in each block, in the
    /// order the walk meets them, which is the order they are given registers in when there is
    /// room for some and not all.
    #[must_use]
    pub fn of(source: &Func, names: &[Value], room: u32) -> Option<Self> {
        let indexed = indexed(source, names);
        if indexed.is_empty() {
            return None;
        }
        let cfg = Cfg::new(source);
        cfg.entry()?;
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        let calls: Vec<bool> = loops
            .all()
            .map(|id| {
                loops.blocks(id).iter().any(|&block| {
                    source.insts(block).any(|inst| {
                        matches!(
                            source[inst].opcode,
                            Opcode::Call
                                | Opcode::CallIndirect
                                | Opcode::TailCall
                                | Opcode::Apply
                                | Opcode::InlineAsm
                        )
                    })
                })
            })
            .collect();
        let live = Liveness::of(source, &cfg);
        let pressure = Pressure::of(source, &cfg, &live);
        let limit = room.saturating_sub(rucc_cost::param!(heuristics::LOOP_RESERVED_REGS));
        // How many addresses are held across each block so far, which the pressure in it does not
        // count, since the IR has each of them live only from where it is taken to where it is
        // last read.
        let mut extra = vec![0_u32; cfg.capacity()];
        let mut held: Map<Symbol, Vec<(LoopId, Value)>> = Map::default();
        let mut across = Map::default();
        let mut ahead: Map<Block, Vec<Value>> = Map::default();
        for &value in names {
            let Some(readers) = indexed.get(&value) else { continue };
            let Def::Result { inst, .. } = source[value].def else { continue };
            let Extra::Symbol(symbol) = source[inst].extra else { continue };
            let Some(taken) = source.block_of(inst) else { continue };
            let sharing = held.get(&symbol).and_then(|loops_held| {
                loops_held.iter().find(|&&(id, _)| loops.contains(id, taken)).copied()
            });
            if let Some(shared) = sharing {
                across.insert(value, shared);
                continue;
            }
            let mut chosen = None;
            let mut walk = loops.innermost(taken);
            while let Some(id) = walk {
                let most = loops
                    .blocks(id)
                    .iter()
                    .map(|&block| {
                        pressure.most_in_block(block, RegClass::Integer) + extra[block.index()]
                    })
                    .max()
                    .unwrap_or(0);
                if calls[id.index()] || loops.preheader(&cfg, id).is_none() || most >= limit {
                    break;
                }
                chosen = Some(id);
                walk = loops.parent(id);
            }
            let Some(id) = chosen else { continue };
            if !readers.iter().any(|&block| loops.contains(id, block)) {
                continue;
            }
            let Some(front) = loops.preheader(&cfg, id) else { continue };
            for &block in loops.blocks(id) {
                extra[block.index()] += 1;
            }
            held.entry(symbol).or_default().push((id, value));
            across.insert(value, (id, value));
            ahead.entry(front).or_default().push(value);
        }
        if across.is_empty() {
            return None;
        }
        Some(Self { loops, across, ahead })
    }

    /// The addresses to take at the end of that block, which is in front of the loops they are held
    /// across.
    #[must_use]
    pub fn ahead(&self, block: Block) -> &[Value] {
        self.ahead.get(&block).map_or(&[], Vec::as_slice)
    }

    /// The address whose register a read of `value` in `block` is, when `value` is held across a
    /// loop `block` is in.
    #[must_use]
    pub fn shared(&self, value: Value, block: Block) -> Option<Value> {
        let &(id, first) = self.across.get(&value)?;
        self.loops.contains(id, block).then_some(first)
    }
}

/// The blocks each of `names` is read in with an index, which is as the base of an address whose
/// offset is not a constant.
fn indexed(source: &Func, names: &[Value]) -> Map<Value, Vec<Block>> {
    let mut found: Map<Value, Vec<Block>> =
        names.iter().map(|&value| (value, Vec::new())).collect();
    for block in source.blocks() {
        for inst in source.insts(block) {
            if source[inst].opcode != Opcode::PtrAdd {
                continue;
            }
            let &[base, offset] = &source[source[inst].args] else { continue };
            let constant = matches!(
                source[offset].def,
                Def::Result { inst: made, .. } if source[made].opcode == Opcode::IConst
            );
            if constant {
                continue;
            }
            if let Some(blocks) = found.get_mut(&base) {
                blocks.push(block);
            }
        }
    }
    found.retain(|_, blocks| !blocks.is_empty());
    found
}
