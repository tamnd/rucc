//! Counts the loop nests documents 30 and 31 are gated on, and changes nothing at all.
//!
//! Design: `spec/optimizer/30-loop-restructuring.md` section 30.8 and
//! `spec/optimizer/31-dependence-analysis.md` section 31.8.
//!
//! Both of those documents decline to build anything in M4, and both of them decline on the same
//! number. Section 30.8 asks for the fraction of the corpus spent in loops that are perfectly
//! nested at depth two or more with affine subscripts, and says it is the one measurement that
//! would overturn the decision. Section 31.8 says everything in documents 30, 31 and 32 is
//! downstream of it and to collect it first. This pass is the collecting.
//!
//! It is also section 31.3's instrumentation-first approach applied one step earlier than that
//! section applies it. Document 31 wants the subscript tests instrumented so that where to add
//! power is decided by counts rather than by intuition. Before any of those tests exist there is a
//! prior count, which is how many nests there are for them to run on at all, and the pass that
//! takes it is a fiftieth of the size of the ones the answer would authorize.
//!
//! # It is not in any pipeline
//!
//! Nothing here is worth a walk over every function of every build, so no optimization level names
//! it. What reaches it is `-fenable-nests`, which section 41.6 already makes pull a pass into the
//! pipeline that the level did not choose, together with `-fopt-info-all` to hear what it found.
//! Surveying the corpus is then one run of the corpus with two flags on, which is section 30.8's
//! claim that the number costs one instrumented run to obtain.
//!
//! # What it counts
//!
//! One remark per nest, and the nests are counted from the outside in. Starting at a loop with
//! nothing around it, the chain goes down for as long as the loop it is at has exactly one loop
//! inside it and nothing between the two of them, and it stops at a loop with no loop inside it.
//! A chain that stops anywhere else is not perfectly nested and is counted as that.
//!
//! **Nothing between the two of them** is read as no instruction that touches memory in the blocks
//! of the outer loop that are not blocks of the inner one. That is narrower than perfect nesting
//! strictly means, since scalar arithmetic between the loops breaks the perfect nesting too, and it
//! is the right width for what the number is for. A subscript computation hoisted out of the inner
//! loop is arithmetic between the loops that every transformation in document 30 sinks back before
//! it does anything else, so counting those nests out would undercount the population that the
//! transformations serve. A store between the loops is a statement, and that is the shape those
//! transformations genuinely cannot have.
//!
//! **A straight line in the counters** is what document 31.1 calls an affine access function. The
//! address of every read and write in the innermost loop is asked of `crate::scev` once per loop of
//! the chain, and it has to come back as something that either does not move or moves by a fixed
//! step. Anything else, and a call is the common anything else, means the equation document 31.1
//! states is not a linear one and none of the tests in section 31.2 apply.
//!
//! The count of references is reported too, one remark each, because section 31.7's cost is
//! quadratic in it: a nest with fifty references has 1,225 subscript pairs, and how large that
//! number gets on real code is the second thing worth knowing before writing the tests.
//!
//! # What it does not do
//!
//! It does not weight anything by run time, and section 30.8 asks for a fraction of run time rather
//! than a count of nests. Static counts are the half of the answer a compiler can give on its own.
//! The other half is which of those nests the corpus actually spends its time in, which is a
//! profile, and it belongs to the corpus rather than here.

use rucc_ir::{Func, Inst, Opcode, Value};

use crate::loops::{LoopId, Loops};
use crate::scev::{Evolution, Invariant, Scev};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

const POPULATION: &str =
    "loop nest two or more deep, perfectly nested, every address in it a straight line";
const NOT_AFFINE: &str =
    "loop nest two or more deep, perfectly nested, an address in it is not a straight line";
const NOT_PERFECT: &str = "loop nest, but not perfectly nested, something sits between the loops";
const ALONE: &str = "loop with no loop inside it";
const REFERENCE: &str = "read or write in the innermost loop of a perfect nest";

/// The survey section 30.8 asks for.
#[derive(Debug)]
pub struct Nests;

impl Pass for Nests {
    fn name(&self) -> &'static str {
        "nests"
    }

    fn describe(&self) -> &'static str {
        "counts the loop nests, and changes nothing"
    }

    fn preserves(&self) -> Preserved {
        // It writes nothing, so everything worked out about the function is still true.
        Preserved::ALL
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, _fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let cfg = an.cfg(func).clone();
        let loops = an.loops(func).clone();
        let mut scev = Scev::new(func, &cfg, &loops);
        for id in loops.all() {
            if loops.parent(id).is_some() {
                continue;
            }
            match chain(func, &loops, id) {
                Chain::Broken => stats.note(NOT_PERFECT),
                Chain::Perfect(nest) => report(func, &loops, &mut scev, &nest, &mut stats),
            }
        }
        stats
    }
}

/// How far the loops go down before something stops them being one nest.
enum Chain {
    /// The loops from the outside in, ending at one with no loop inside it.
    Perfect(Vec<LoopId>),
    /// A loop that holds more than one loop, or holds one with a statement beside it.
    Broken,
}

/// The chain of loops starting at this one, going in for as long as it stays a nest.
fn chain(func: &Func, loops: &Loops, outer: LoopId) -> Chain {
    let mut nest = vec![outer];
    let mut at = outer;
    loop {
        let inside = loops.children(at);
        let [only] = inside else {
            return match inside.is_empty() {
                true => Chain::Perfect(nest),
                false => Chain::Broken,
            };
        };
        if between(func, loops, at, *only) {
            return Chain::Broken;
        }
        nest.push(*only);
        at = *only;
    }
}

/// Whether anything touching memory sits in the outer loop and not in the inner one.
fn between(func: &Func, loops: &Loops, outer: LoopId, inner: LoopId) -> bool {
    loops
        .blocks(outer)
        .iter()
        .filter(|&&block| !loops.contains(inner, block))
        .flat_map(|&block| func.insts(block))
        .any(|inst| func[inst].opcode.touches_memory())
}

/// Says which kind of nest this one is, and counts what its innermost loop reads and writes.
fn report(func: &Func, loops: &Loops, scev: &mut Scev<'_>, nest: &[LoopId], stats: &mut Stats) {
    let Some(&innermost) = nest.last() else { return };
    if nest.len() < 2 {
        stats.note(ALONE);
        return;
    }
    let mut affine = true;
    let touching: Vec<Inst> = loops
        .blocks(innermost)
        .iter()
        .flat_map(|&block| func.insts(block))
        .filter(|&inst| func[inst].opcode.touches_memory())
        .collect();
    for inst in touching {
        stats.note(REFERENCE);
        affine &= match address(func, inst) {
            // A call is the usual one here. It touches memory at an address nothing named, so
            // there is no access function to be affine and document 31.1's equation has no terms.
            None => false,
            Some(addr) => straight(scev, nest, addr),
        };
    }
    stats.note(if affine { POPULATION } else { NOT_AFFINE });
}

/// The address a read or a write names, when it names one.
fn address(func: &Func, inst: Inst) -> Option<Value> {
    let data = func[inst];
    let args = &func[data.args];
    match data.opcode {
        Opcode::Load => args.first().copied(),
        Opcode::Store => args.get(1).copied(),
        _ => None,
    }
}

/// Whether that value is a straight line in the counters of this nest.
///
/// Asked innermost first, because that is how a nest of them is built. A value that moves by a
/// fixed step in the innermost loop is a straight line there if what it starts at and what it
/// steps by are themselves straight lines in the loops outside, which is the same recursion
/// document 31.1's access function is written by.
fn straight(scev: &mut Scev<'_>, nest: &[LoopId], value: Value) -> bool {
    let Some((&innermost, outer)) = nest.split_last() else { return true };
    match scev.evolution(innermost, value) {
        Evolution::Unknown => false,
        Evolution::Invariant(inv) => part(scev, outer, inv),
        Evolution::Affine(chrec) => part(scev, outer, chrec.base) && part(scev, outer, chrec.step),
    }
}

/// The same question about one end of a chrec, which is a number, or a value and a scale, or one
/// value with another scaled beside it.
fn part(scev: &mut Scev<'_>, outer: &[LoopId], inv: Invariant) -> bool {
    let rest = match inv.on() {
        // Two values in the expression is two values that have to be straight lines, because the
        // access function is the sum of them and a sum is only as straight as both its sides.
        Some((on, rest)) => {
            if !straight(scev, outer, on) {
                return false;
            }
            rest
        }
        None => inv.plain().expect("an invariant not measured from a value is a plain one"),
    };
    match rest.value {
        None => true,
        Some(value) => straight(scev, outer, value),
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Flags, Func, IntPred, MemInfo, MemOrder, Opcode, Restrict, Signature, Type,
        Value,
    };

    use super::{ALONE, NOT_AFFINE, NOT_PERFECT, Nests, POPULATION, REFERENCE};
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// Runs the survey over the function as it stands.
    fn survey(func: &mut Func) -> Stats {
        Nests.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// An access that says as little about itself as one may.
    fn plain() -> MemInfo {
        MemInfo {
            size: 0,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        }
    }

    /// A counted loop with a body for the caller to fill and a block for it to leave to.
    struct Counted {
        head: Block,
        body: Block,
        out: Block,
        counter: Value,
    }

    /// Opens a loop, entered from `into`.
    ///
    /// ```text
    /// into:      jump head(0)
    /// head(i):   t = i < limit; br t -> body(i), out
    /// ```
    ///
    /// The counter is handed back because a body that indexes with it is the whole of what this
    /// pass asks about. Closing the loop is [`close`], and it is separate so that the caller can
    /// put another loop in the body first.
    fn counted(func: &mut Func, into: Block, limit: i128) -> Counted {
        let head = func.create_block();
        let body = func.create_block();
        let out = func.create_block();
        let i = func.append_param(head, Type::int(32));
        let carried = func.append_param(body, Type::int(32));

        let mut build = Builder::new(func, into);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(head, &[zero]);

        let mut build = Builder::new(func, head);
        let stop = build.iconst(Type::int(32), limit);
        let test = build.icmp(IntPred::Slt, i, stop);
        build.br_if(test, body, &[i], out, &[]);

        Counted { head, body, out, counter: carried }
    }

    /// Closes a loop, with `at` as the block the counter is moved along in.
    ///
    /// That is the body for a loop with nothing inside it, and the block the inner loop leaves to
    /// for a loop with one inside it, which is what makes the nest have nothing between its levels.
    fn close(func: &mut Func, it: &Counted, at: Block) {
        let mut build = Builder::new(func, at);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, it.counter, one, Flags::NSW);
        build.jump(it.head, &[next]);
    }

    /// A function taking one pointer, with an entry block for a loop to go in.
    fn shell(names: &mut Interner) -> (Func, Block, Value) {
        let signature = Signature::new().with_params(&[Type::PTR]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let base = func.append_param(entry, Type::PTR);
        (func, entry, base)
    }

    #[test]
    fn a_loop_with_nothing_inside_it_is_not_a_nest() {
        let mut names = Interner::new();
        let (mut func, entry, _) = shell(&mut names);
        let it = counted(&mut func, entry, 8);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);

        let stats = survey(&mut func);
        assert_eq!(stats.count(Kind::Note, ALONE), 1);
        assert_eq!(stats.count(Kind::Note, POPULATION), 0);
        assert!(!stats.changed(), "the survey rewrites nothing");
    }

    #[test]
    fn two_loops_walking_a_row_at_a_time_are_the_population() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let outer = counted(&mut func, entry, 4);

        // row = base + i * 256, worked out once per turn of the outer loop, which is the shape a
        // two dimensional array walk arrives in.
        let mut build = Builder::new(&mut func, outer.body);
        let wide = build.unary(Opcode::SExt, outer.counter, Type::int(64));
        let stride = build.iconst(Type::int(64), 256);
        let along = build.binary(Opcode::Mul, wide, stride, Flags::NSW);
        let row = build.binary(Opcode::PtrAdd, base, along, Flags::NONE);

        let inner = counted(&mut func, outer.body, 3);
        // row[j] = j.
        let mut build = Builder::new(&mut func, inner.body);
        let step = build.unary(Opcode::SExt, inner.counter, Type::int(64));
        let four = build.iconst(Type::int(64), 4);
        let by = build.binary(Opcode::Mul, step, four, Flags::NSW);
        let addr = build.binary(Opcode::PtrAdd, row, by, Flags::NONE);
        build.store(inner.counter, addr, plain(), Flags::NONE);
        close(&mut func, &inner, inner.body);
        close(&mut func, &outer, inner.out);
        Builder::new(&mut func, outer.out).ret(&[]);

        let stats = survey(&mut func);
        assert_eq!(stats.count(Kind::Note, POPULATION), 1);
        assert_eq!(stats.count(Kind::Note, NOT_AFFINE), 0);
        assert_eq!(stats.count(Kind::Note, REFERENCE), 1);
    }

    /// The case this pass exists to count, and the day the analysis grew.
    ///
    /// `base[i + j]` is affine in both counters by any account of what affine means, and this used
    /// to come out on the wrong side of the line. Widening the sum to pointer width goes through
    /// `Scev`'s extension, which took a chrec only when what it starts at was a plain number, and
    /// what this one starts at is the outer counter. The extension now takes one of a value as
    /// well, describing rather than naming the widened value, so the sum widens to
    /// `{sext(i), +, 1}` and both ends of it are straight lines in the loops outside. The test is
    /// kept the way round it is now because the number the survey reports is a number about rucc's
    /// analysis and not only about the corpus, and it should move again if the analysis moves.
    #[test]
    fn an_address_added_from_both_counters_is_one_this_compiler_can_describe() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let outer = counted(&mut func, entry, 4);
        let inner = counted(&mut func, outer.body, 3);

        let mut build = Builder::new(&mut func, inner.body);
        let sum = build.binary(Opcode::Add, outer.counter, inner.counter, Flags::NSW);
        let wide = build.unary(Opcode::SExt, sum, Type::int(64));
        let addr = build.binary(Opcode::PtrAdd, base, wide, Flags::NONE);
        build.store(outer.counter, addr, plain(), Flags::NONE);
        close(&mut func, &inner, inner.body);
        close(&mut func, &outer, inner.out);
        Builder::new(&mut func, outer.out).ret(&[]);

        let stats = survey(&mut func);
        assert_eq!(stats.count(Kind::Note, NOT_AFFINE), 0);
        assert_eq!(stats.count(Kind::Note, POPULATION), 1);
    }

    #[test]
    fn a_write_between_the_two_loops_stops_it_being_a_nest() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let outer = counted(&mut func, entry, 4);
        let inner = counted(&mut func, outer.body, 3);

        Builder::new(&mut func, inner.body).store(inner.counter, base, plain(), Flags::NONE);
        close(&mut func, &inner, inner.body);
        // The write the outer loop does itself, which is the statement beside the inner loop.
        Builder::new(&mut func, inner.out).store(outer.counter, base, plain(), Flags::NONE);
        close(&mut func, &outer, inner.out);
        Builder::new(&mut func, outer.out).ret(&[]);

        let stats = survey(&mut func);
        assert_eq!(stats.count(Kind::Note, NOT_PERFECT), 1);
        assert_eq!(stats.count(Kind::Note, POPULATION), 0);
    }

    #[test]
    fn an_address_that_came_out_of_memory_is_not_a_straight_line() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let outer = counted(&mut func, entry, 4);
        let inner = counted(&mut func, outer.body, 3);

        // p = *base; *p = j, which is the list walk no subscript test describes.
        let mut build = Builder::new(&mut func, inner.body);
        let addr = build.load(Type::PTR, base, plain(), Flags::NONE);
        build.store(inner.counter, addr, plain(), Flags::NONE);
        close(&mut func, &inner, inner.body);
        close(&mut func, &outer, inner.out);
        Builder::new(&mut func, outer.out).ret(&[]);

        let stats = survey(&mut func);
        assert_eq!(stats.count(Kind::Note, NOT_AFFINE), 1);
        assert_eq!(stats.count(Kind::Note, POPULATION), 0);
        assert_eq!(stats.count(Kind::Note, REFERENCE), 2, "the load and the write both count");
    }
}
