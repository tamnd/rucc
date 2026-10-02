//! Global code motion: a value is worked out where its uses need it, and no deeper in a loop than
//! the place it was written.
//!
//! Design: `spec/optimizer/12-egraph.md` section 12.5, and step 3 of section 12.6, which asks for
//! this standalone before either arm of the experiment exists. Arms B and C place what they build
//! with it, and it is the piece most likely to be kept whichever of them wins.
//!
//! ```c
//! int g(int);
//! int f(int a, int b, int c) {
//!     int t = a * b + 7;
//!     if (c) return g(t);
//!     return 0;
//! }
//! ```
//!
//! The front end computes `t` where it was written, in front of the test, and the path that
//! returns 0 pays for a multiply it throws away. gcc 16 tests `c` first. This moves the multiply
//! and the add down into the arm that calls `g`.
//!
//! # The two walks
//!
//! Click's algorithm (PLDI 1995) has an early schedule, which puts each value in the first block
//! all of its operands are in, and a late schedule, which puts it at the nearest common dominator
//! of the blocks that read it. A read by a block parameter is a read by the branch that passes it,
//! so it counts in the block the branch ends. Anywhere on the dominator tree path between the two
//! is legal, and the block picked is the latest one of the least loop depth.
//!
//! Here the two are two walks that each move what they decide, rather than one decision made from
//! both. The hoisting walk goes in dominator order, so a value's operands have gone wherever they
//! are going before it is looked at, and it goes no higher than they now are. The sinking walk goes
//! the other way, so a value's readers have gone wherever they are going first, and it goes no
//! lower than they now are. Every move is then legal on the program as it stands when it is made,
//! which is what lets fuel or the pressure test stop any one of them without leaving an operand
//! below its reader.
//!
//! Within one loop a block's dominator runs at least as often as it does, so the latest block is
//! also the least frequent one and a frequency would only say the same thing again. Across loops
//! the frequencies are estimates and the depth is not, and a value moved into a loop runs once per
//! iteration whatever the estimate says. A block is a candidate only when every loop it is in also
//! holds the value's block, so a value leaves loops and never enters one, not even a sibling whose
//! header is the exit of the loop the value was in.
//!
//! # What moves
//!
//! Arithmetic, comparisons, conversions and `select`, which are the opcodes whose result is a
//! function of their operands and nothing else. Nothing that touches memory moves, which section
//! 12.7 gives as the cheap and correct rule for loads: load motion is `licm`'s, where the alias
//! analysis is in hand. A constant or the address of a symbol does not move either, because the
//! backend puts those where they are used already and holding one costs a register for nothing.
//!
//! A division that may trap moves down and never up. Going down it runs on fewer paths, and every
//! path it still runs on is one that ran it before. Going up it could run on a path that never
//! divided, with the zero or the minus one that path has.
//!
//! # Pressure
//!
//! Leaving a loop takes a register for the whole of it. When `pressure` says the loop is already
//! at the allocatable count, only what `licm` counts as expensive leaves it, which is the rule
//! section 40.6 gives both passes, and the rest goes no higher than the block it was in.

use rucc_base::hash::Map;
use rucc_cost::heuristics;
use rucc_ir::{Block, Def, Func, Inst, Opcode, Value};

use crate::dom::Dominators;
use crate::loops::Loops;
use crate::machine::Machine;
use crate::phiopt::speculatable;
use crate::pressure::{Pressure, class_of};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats, licm, uses};

/// The name the pipelines and `-fno-gcm` use.
pub const NAME: &str = "gcm";

/// Recorded for each value moved down towards the reads that need it.
const SUNK: &str = "computation moved down to where its reads need it";

/// Recorded for each value moved out of a loop that does not change it.
const HOISTED: &str = "computation moved out of a loop nothing in it changes";

/// Recorded for each value kept in its loop because the loop has no register to spare for it.
const PRESSURE: &str = "left in the loop, the register holding it would cost more than it saves";

/// Recorded for each value that would have moved after the fuel ran out.
const NO_FUEL: &str = "computation left where it was, the pass ran out of fuel";

/// Global code motion.
#[derive(Debug)]
pub struct Gcm;

/// The one instance, which is what the pipelines name.
pub static GCM: Gcm = Gcm;

impl Pass for Gcm {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "moves a computation to the latest place in front of its reads at the least loop depth"
    }

    fn preserves(&self) -> Preserved {
        // No edge moves and no block appears. What changes is where values are live, which is
        // the transformation.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let machine = an.machine();
        let cfg = an.cfg(func);
        let order: Vec<Block> = cfg.reverse_postorder().collect();
        let mut job = Job {
            dom: an.dominators(func),
            loops: an.loops(func),
            pressure: an.pressure(func),
            machine,
            at: Map::default(),
            readers: Map::default(),
        };
        job.survey(func, &order);
        for &block in &order {
            let insts: Vec<Inst> = func.insts(block).collect();
            for inst in insts {
                job.hoist(func, inst, fuel, &mut stats);
            }
        }
        for &block in order.iter().rev() {
            let insts: Vec<Inst> = func.insts(block).collect();
            for &inst in insts.iter().rev() {
                job.sink(func, inst, fuel, &mut stats);
            }
        }
        stats
    }
}

/// What both walks read.
struct Job<'a> {
    dom: &'a Dominators,
    loops: &'a Loops,
    pressure: &'a Pressure,
    machine: Machine,
    /// The block each reachable instruction is in, kept up to date as instructions move.
    at: Map<Inst, Block>,
    /// The instructions that read each value, once each however many times they read it.
    readers: Map<Value, Vec<Inst>>,
}

impl Job<'_> {
    /// Where every reachable instruction is and who reads every value. The readers come from every
    /// block, reachable or not, so a value read somewhere the dominator tree does not reach has a
    /// reader with no place and does not move.
    fn survey(&mut self, func: &Func, order: &[Block]) {
        for &block in order {
            for inst in func.insts(block) {
                self.at.insert(inst, block);
            }
        }
        for block in func.blocks() {
            for inst in func.insts(block) {
                uses::operands(func, inst, |value| {
                    let readers = self.readers.entry(value).or_default();
                    if readers.last() != Some(&inst) {
                        readers.push(inst);
                    }
                });
            }
        }
    }

    /// Where a value is now: its own block for a parameter, the block of what made it for a result.
    fn home(&self, func: &Func, value: Value) -> Option<Block> {
        match func[value].def {
            Def::Param { block, .. } => Some(block),
            Def::Result { inst, .. } => self.at.get(&inst).copied(),
        }
    }

    /// How many loops a block is inside. [`Loops::depth`] counts an outermost loop as zero, so a
    /// block in one is one deep here and a block in none is zero.
    fn depth(&self, block: Block) -> u32 {
        self.loops.innermost(block).map_or(0, |id| self.loops.depth(id) + 1)
    }

    /// Whether a value in `here` may be put in `block`: every loop `block` is in holds `here` too,
    /// and `block` is not in a region with no one header.
    fn may_go(&self, block: Block, here: Block) -> bool {
        !self.loops.is_irreducible(block)
            && self.loops.innermost(block).is_none_or(|id| self.loops.contains(id, here))
    }

    /// The latest block of the least loop depth on the dominator tree path from `low` up to
    /// `high`. `here` is on the path and may always stay where it is, so there is an answer unless
    /// the path leaves the tree.
    fn pick(&self, low: Block, high: Block, here: Block) -> Option<Block> {
        let mut best: Option<Block> = None;
        let mut block = low;
        loop {
            let shallower = best.is_none_or(|best| self.depth(block) < self.depth(best));
            if self.may_go(block, here) && shallower {
                best = Some(block);
            }
            if block == high {
                return best;
            }
            block = self.dom.immediate_dominator(block)?;
        }
    }

    /// Moves an instruction up out of the loops it does not need to be in, as high as its
    /// operands now are.
    fn hoist(&mut self, func: &mut Func, inst: Inst, fuel: &mut Fuel, stats: &mut Stats) {
        if !movable(func, inst) || !speculatable(func, inst) {
            return;
        }
        let Some(&here) = self.at.get(&inst) else { return };
        if self.loops.is_irreducible(here) || self.depth(here) == 0 {
            return;
        }
        let Some(value) = one_result(func, inst) else { return };
        let Some(mut high) = func.entry() else { return };
        for &arg in &func[func[inst].args] {
            let Some(from) = self.home(func, arg) else { return };
            if self.dom.dominates(high, from) {
                high = from;
            }
        }
        let Some(best) = self.pick(here, high, here) else { return };
        if best == here {
            return;
        }
        if self.tight(func, inst, here, value) {
            stats.missed(PRESSURE);
            return;
        }
        let Some(before) = func.terminator(best) else { return };
        if !fuel.take() {
            stats.missed(NO_FUEL);
            return;
        }
        func.remove_inst(inst);
        func.insert_before(inst, before);
        self.at.insert(inst, best);
        stats.optimized(HOISTED);
    }

    /// Moves an instruction down to the latest block in front of all its reads, never into a loop.
    fn sink(&mut self, func: &mut Func, inst: Inst, fuel: &mut Fuel, stats: &mut Stats) {
        if !movable(func, inst) {
            return;
        }
        let Some(&here) = self.at.get(&inst) else { return };
        if self.loops.is_irreducible(here) {
            return;
        }
        let Some(value) = one_result(func, inst) else { return };
        let Some(readers) = self.readers.get(&value) else { return };
        let mut low: Option<Block> = None;
        for reader in readers {
            let Some(&block) = self.at.get(reader) else { return };
            low = match low {
                None => Some(block),
                Some(low) => self.dom.nearest_common_dominator(low, block),
            };
            if low.is_none() {
                return;
            }
        }
        let Some(low) = low else { return };
        let Some(best) = self.pick(low, here, here) else { return };
        if best == here {
            return;
        }
        let before = func
            .insts(best)
            .find(|other| readers.contains(other))
            .or_else(|| func.terminator(best));
        let Some(before) = before else { return };
        if !fuel.take() {
            stats.missed(NO_FUEL);
            return;
        }
        func.remove_inst(inst);
        func.insert_before(inst, before);
        self.at.insert(inst, best);
        stats.optimized(SUNK);
    }

    /// Whether the loop the value would leave has no register to spare, and the value is not
    /// worth one.
    fn tight(&self, func: &Func, inst: Inst, here: Block, value: Value) -> bool {
        if licm::cost(func, inst) >= heuristics::LICM_EXPENSIVE {
            return false;
        }
        let Some(id) = self.loops.innermost(here) else { return false };
        let Some(class) = class_of(func[value].ty) else { return false };
        let room = self.machine.allocatable(class).unwrap_or(0);
        self.pressure.is_tight(self.loops, id, class, room)
    }
}

/// The result of an instruction that has exactly one.
fn one_result(func: &Func, inst: Inst) -> Option<Value> {
    let mut results = func[inst].results();
    let value = results.next()?;
    results.next().is_none().then_some(value)
}

/// Whether this instruction's result is a function of its operands and nothing else, and it costs
/// something to work out, which is what makes where it is worked out a question.
fn movable(func: &Func, inst: Inst) -> bool {
    matches!(
        func[inst].opcode,
        Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::SDiv
            | Opcode::UDiv
            | Opcode::SRem
            | Opcode::URem
            | Opcode::UMulHigh
            | Opcode::SMulHigh
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::FAdd
            | Opcode::FSub
            | Opcode::FMul
            | Opcode::FDiv
            | Opcode::FRem
            | Opcode::FNeg
            | Opcode::Fma
            | Opcode::ICmp
            | Opcode::FCmp
            | Opcode::Select
            | Opcode::Trunc
            | Opcode::SExt
            | Opcode::ZExt
            | Opcode::FPTrunc
            | Opcode::FPExt
            | Opcode::FPToSI
            | Opcode::FPToUI
            | Opcode::SIToFP
            | Opcode::UIToFP
            | Opcode::PtrToInt
            | Opcode::IntToPtr
            | Opcode::Bitcast
            | Opcode::PtrAdd
            | Opcode::Ctlz
            | Opcode::Cttz
            | Opcode::Ctpop
            | Opcode::Bswap
            | Opcode::Bitreverse
    )
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::GCM;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it under this much fuel.
    fn run(body: &str, fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            GCM.run(&mut module[id], &mut an, fuel);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn moved(body: &str) -> String {
        run(body, &mut Fuel::unlimited())
    }

    /// The label of the block the first line holding `what` is in.
    fn block_of(out: &str, what: &str) -> String {
        let mut block = String::new();
        for line in out.lines() {
            let line = line.trim();
            if line.starts_with("block") && line.ends_with(':') {
                block = line.split(['(', ':']).next().unwrap_or_default().to_string();
            } else if line.contains(what) {
                return block;
            }
        }
        panic!("nothing says {what} in\n{out}");
    }

    /// `int t = a * b + 7; if (c) return t; return 0;`, the shape in the module comment.
    const ARM: &str = r#"
func @f(i32, i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32, %2: i32):
    %3 = mul %0, %1
    %4 = iconst.i32 7
    %5 = add %3, %4
    %6 = iconst.i32 0
    %7 = icmp ne %2, %6
    br_if %7, block1, block2

block1:
    return %5

block2:
    return %6
}
"#;

    #[test]
    fn a_value_one_arm_reads_moves_into_that_arm() {
        let out = moved(ARM);
        assert_eq!(block_of(&out, "mul"), "block1", "{out}");
        assert_eq!(block_of(&out, "add"), "block1", "{out}");
        assert_eq!(block_of(&out, "icmp"), "block0", "{out}");
    }

    #[test]
    fn a_value_both_arms_read_stays_in_front_of_the_branch() {
        let out = moved(&ARM.replace("return %6", "return %5"));
        assert_eq!(block_of(&out, "mul"), "block0", "{out}");
        assert_eq!(block_of(&out, "add"), "block0", "{out}");
    }

    #[test]
    fn a_value_an_arm_passes_to_a_parameter_moves_into_that_arm() {
        let out = moved(
            r#"
func @f(i32, i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32, %2: i32):
    %3 = mul %0, %1
    %4 = iconst.i32 0
    %5 = icmp ne %2, %4
    br_if %5, block1, block2

block1:
    jump block3(%3)

block2:
    jump block3(%4)

block3(%6: i32):
    return %6
}
"#,
        );
        assert_eq!(block_of(&out, "mul"), "block1", "{out}");
    }

    #[test]
    fn a_load_stays_where_it_is() {
        let out = moved(
            r#"
func @f(ptr, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = load.i32 %0, align 4
    %3 = iconst.i32 0
    %4 = icmp ne %1, %3
    br_if %4, block1, block2

block1:
    return %2

block2:
    return %3
}
"#,
        );
        assert_eq!(block_of(&out, "load"), "block0", "{out}");
    }

    /// A loop whose body reads `%0` and `%1`, which it does not change, and is given `BODY`.
    const LOOP: &str = r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 3
    %3 = iconst.i32 0
    jump block1(%3)

block1(%4: i32):
    %5 = BODY
    %6 = add %4, %5
    %7 = icmp slt %6, %0
    br_if %7, block1(%6), block2

block2:
    return %6
}
"#;

    #[test]
    fn a_value_the_loop_does_not_change_moves_in_front_of_it() {
        let out = moved(&LOOP.replace("BODY", "mul %0, %1"));
        assert_eq!(block_of(&out, "mul"), "block0", "{out}");
    }

    #[test]
    fn a_division_that_may_trap_stays_in_the_loop() {
        let out = moved(&LOOP.replace("BODY", "sdiv %0, %1"));
        assert_eq!(block_of(&out, "sdiv"), "block1", "{out}");
    }

    #[test]
    fn a_division_by_a_constant_moves_in_front_of_the_loop() {
        let out = moved(&LOOP.replace("BODY", "sdiv %0, %2"));
        assert_eq!(block_of(&out, "sdiv"), "block0", "{out}");
    }

    #[test]
    fn a_value_read_only_in_a_loop_stays_out_of_it() {
        let out = moved(
            r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = mul %0, %1
    %3 = iconst.i32 0
    jump block1(%3)

block1(%4: i32):
    %5 = add %4, %2
    %6 = icmp slt %5, %0
    br_if %6, block1(%5), block2

block2:
    return %5
}
"#,
        );
        assert_eq!(block_of(&out, "mul"), "block0", "{out}");
    }

    #[test]
    fn a_value_read_only_after_the_loop_moves_out_of_it() {
        let out = moved(
            r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 0
    jump block1(%2)

block1(%3: i32):
    %4 = iconst.i32 1
    %5 = add %3, %4
    %6 = mul %5, %1
    %7 = icmp slt %5, %0
    br_if %7, block1(%5), block2

block2:
    return %6
}
"#,
        );
        assert_eq!(block_of(&out, "mul"), "block2", "{out}");
        assert_eq!(block_of(&out, "add"), "block1", "{out}");
    }

    #[test]
    fn fuel_stops_the_moving() {
        let out = run(ARM, &mut Fuel::of(0));
        assert_eq!(block_of(&out, "mul"), "block0", "{out}");
        assert_eq!(block_of(&out, "add"), "block0", "{out}");
        // The walk that sinks goes readers first, so the one move there is fuel for is the add.
        let out = run(ARM, &mut Fuel::of(1));
        assert_eq!(block_of(&out, "add"), "block1", "{out}");
        assert_eq!(block_of(&out, "mul"), "block0", "{out}");
    }
}
