//! How often each block runs, carried from the IR down to the machine IR.
//!
//! Design: `spec/optimizer/38-scheduling-and-layout.md` sections 38.4 and 38.6, and
//! tamnd/rucc#364, which is the observation that a branch weight is worked out and then thrown
//! away because nothing downstream could read one.
//!
//! [`rucc_opt::Frequencies`] answers, for an IR function, how often every block runs next to the
//! once the function is entered, and how likely each arm of each branch is to be the one taken.
//! Every one of its consumers so far has been a pass in the middle end, and the consumer this is
//! for is [`crate::layout`], which is at the far end of selection, allocation and the prologue.
//! None of those could work the numbers out for itself: by then a loop is a backward branch and
//! the loop forest the frequency was summed over is gone.
//!
//! So the numbers are copied onto the blocks and the arms as soon as there are blocks and arms to
//! copy them onto, which is the moment selection finishes. [`mir::Weight`] is the same scale the
//! frequency is in, so the copy is a copy.
//!
//! # Why it is a pass over what selection left rather than part of selection
//!
//! Selection makes one machine block per IR block, in the same order, with the arms in the same
//! order, and says so by handing back [`crate::lower::Lowered::blocks`]. That correspondence is
//! the whole of what this needs, and it is a fact worth spending rather than a reason to thread a
//! second table of numbers through four thousand lines of instruction selection.
//!
//! # What is not carried
//!
//! Nothing keeps a weight in step with the graph after this. A pass that makes a block says how
//! often it runs if it knows, and [`crate::split::critical`] does, and everything else leaves the
//! new block running as often as the function does. That is a heuristic going slightly stale, not
//! a fact going wrong, and [`mir::Weight`] says so where it is defined.

use rucc_ir as ir;
use rucc_mir as mir;
use rucc_opt::{Callees, Cfg, Dominators, Frequencies, Loops};

/// Writes onto a machine function how often each of its blocks runs and each of its arms is
/// taken, worked out from the IR function it was selected from.
///
/// `blocks` is [`crate::lower::Lowered::blocks`], which is the machine block each IR block
/// became. A function nothing was lowered from is left alone, and so is every machine block that
/// came from no IR block, which is what a block some later pass added looks like from here.
pub fn carry(source: &ir::Func, blocks: &[Option<mir::Block>], func: &mut mir::Func) {
    let cfg = Cfg::new(source);
    if cfg.entry().is_none() {
        return;
    }
    let doms = Dominators::new(&cfg);
    let loops = Loops::new(&cfg, &doms);
    // The call graph's answer to whether a callee comes back is a fact about the module and this
    // only ever sees the one function, so the predictor is left with the answer the IR gives
    // directly, which is an `unreachable` after a call. That is where most of it comes from
    // anyway, since C error handling is a call to `abort` and the front end writes the
    // `unreachable` behind it.
    let freqs = Frequencies::of(source, &cfg, &loops, &Callees::nothing());
    for block in source.blocks() {
        let Some(&Some(out)) = blocks.get(block.index()) else { continue };
        let weight = freqs.get(block);
        func.set_weight(out, mir::Weight::parts(weight.raw()));
        let Some(term) = source.terminator(block) else { continue };
        let arms: Vec<ir::Block> = source.successors(term).map(|call| call.block).collect();
        if arms.len() != func[out].succs.len() {
            continue;
        }
        for (index, arm) in arms.iter().enumerate() {
            // The probabilities are indexed by the graph's successors and not by the
            // terminator's arms, and the two differ on a branch whose arms agree and on a switch
            // with two labels on one case: the graph names such a block once. So the arm is
            // looked up by where it goes rather than by where it is, and two arms that go to one
            // block each carry the whole of that block's share. Nothing is lost by that, because
            // the layout's question is which block to put next and both arms answer the same.
            let Some(at) = cfg.successors(block).iter().position(|succ| succ == arm) else {
                continue;
            };
            func.succs_mut(out)[index].weight =
                mir::Weight::parts(weight.along(freqs.taken(block, at)).raw());
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Func, Opcode, Signature, Type};
    use rucc_target::x86_64::SYSV;

    use super::*;
    use crate::elsewhere::Elsewhere;
    use crate::lower;

    /// Lowers a function and carries its weights down, and gives back the machine function.
    fn lowered(source: &mut Func, names: &mut Interner) -> mir::Func {
        let out = lower::func(source, names, &SYSV, &Elsewhere::default()).expect("it lowers");
        let lower::Lowered { mut func, blocks, .. } = out;
        carry(source, &blocks, &mut func);
        func
    }

    /// The weights of every block, in layout order, which at this point is the order they were
    /// made in.
    fn weights(func: &mir::Func) -> Vec<u64> {
        func.blocks().map(|block| func[block].weight.raw()).collect()
    }

    #[test]
    fn a_function_with_no_branch_in_it_runs_every_block_once() {
        let mut names = Interner::new();
        let mut source = Func::new(names.intern("f"), Signature::new());
        let entry = source.create_block();
        Builder::new(&mut source, entry).ret(&[]);

        let func = lowered(&mut source, &mut names);

        assert_eq!(weights(&func), [mir::Weight::ONCE.raw()]);
    }

    #[test]
    fn the_arms_of_a_branch_add_up_to_the_block_they_leave() {
        let int = Type::int(32);
        let mut names = Interner::new();
        let mut source =
            Func::new(names.intern("f"), Signature::new().with_params(&[Type::int(1)]));
        let entry = source.create_block();
        let cond = source.append_param(entry, Type::int(1));
        let yes = source.create_block();
        let no = source.create_block();
        Builder::new(&mut source, entry).br_if(cond, yes, &[], no, &[]);
        let mut build = Builder::new(&mut source, yes);
        let one = build.iconst(int, 1);
        build.ret(&[one]);
        let mut build = Builder::new(&mut source, no);
        let two = build.iconst(int, 2);
        build.ret(&[two]);

        let func = lowered(&mut source, &mut names);

        let head = func.blocks().next().expect("an entry");
        let arms: Vec<u64> = func[head].succs.iter().map(|call| call.weight.raw()).collect();
        assert_eq!(arms.len(), 2);
        assert_eq!(arms.iter().sum::<u64>(), mir::Weight::ONCE.raw());
    }

    #[test]
    fn a_loop_body_runs_more_often_than_the_block_that_follows_it() {
        let int = Type::int(32);
        let mut names = Interner::new();
        let mut source =
            Func::new(names.intern("f"), Signature::new().with_params(&[Type::int(1)]));
        let entry = source.create_block();
        let cond = source.append_param(entry, Type::int(1));
        let head = source.create_block();
        let body = source.create_block();
        let out = source.create_block();
        Builder::new(&mut source, entry).jump(head, &[]);
        Builder::new(&mut source, head).br_if(cond, body, &[], out, &[]);
        Builder::new(&mut source, body).jump(head, &[]);
        let mut build = Builder::new(&mut source, out);
        let zero = build.iconst(int, 0);
        build.ret(&[zero]);

        let func = lowered(&mut source, &mut names);

        let made: Vec<mir::Block> = func.blocks().collect();
        let weight = |at: usize| func[made[at]].weight.raw();
        assert!(weight(2) > weight(3), "the body {} the exit {}", weight(2), weight(3));
        // The exit runs once per call, give or take the one part in ten thousand the geometric
        // series loses to integer division on the way round the loop.
        assert!(weight(3).abs_diff(mir::Weight::ONCE.raw()) <= 1, "the exit {}", weight(3));
    }

    #[test]
    fn the_arm_control_does_not_come_back_from_is_the_colder_one() {
        let int = Type::int(32);
        let mut names = Interner::new();
        let mut source =
            Func::new(names.intern("f"), Signature::new().with_params(&[Type::int(1)]));
        let entry = source.create_block();
        let cond = source.append_param(entry, Type::int(1));
        let yes = source.create_block();
        let no = source.create_block();
        Builder::new(&mut source, entry).br_if(cond, yes, &[], no, &[]);
        // The arm that calls nothing and returns, against the arm control does not come back
        // from, which is the predictor that needs no source and is the one C error handling
        // shows up as.
        Builder::new(&mut source, yes).inst(ir::InstData::new(Opcode::Unreachable), &[]);
        let mut build = Builder::new(&mut source, no);
        let zero = build.iconst(int, 0);
        build.ret(&[zero]);

        let func = lowered(&mut source, &mut names);

        let head = func.blocks().next().expect("an entry");
        let arms: Vec<u64> = func[head].succs.iter().map(|call| call.weight.raw()).collect();
        assert!(arms[0] < arms[1], "{arms:?}");
    }
}
