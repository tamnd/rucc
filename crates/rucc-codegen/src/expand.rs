//! The IR rewrites the machine needs before a rule can be asked anything.
//!
//! Design: `spec/10-backend.md` section 10.2, which is where the ordering comes from.
//!
//! Everything else in this crate turns an instruction into instructions. This turns a block into
//! blocks, which is the one thing a rule cannot do: a rule replaces a term with a term, and the
//! replacement has nowhere to put a block, so a construct whose lowering is a new shape of control
//! flow has to be rewritten before selection rather than during it.
//!
//! There is one such construct today and it is `switch`. Every other terminator leaves a block
//! with one successor or two, which is what the block layout writes jumps for, and a `switch`
//! leaves it with as many as the program had cases.
//!
//! # Why this is the backend's and not the front end's
//!
//! What a `switch` should become is a target decision and not a language one. A chain of compares
//! is right for three cases and wrong for two hundred, where the answer is a jump table, and wrong
//! again for twenty spread over a million, where it is a binary search on the value. A front end
//! that picked one would be picking for every target at once, and the IR would no longer hold what
//! the program said. So the `switch` survives as far as here, and here is where it is given up.
//!
//! What is written today is the chain, which `spec/10-backend.md` calls the version every compiler
//! starts with. It is correct for any number of cases and it is slow for a large one. A jump table
//! wants a read only section to put the table in and a relocation to reach it, and neither exists
//! yet, so the chain is also the only one that could be written today.

use rucc_ir::{BlockCall, Builder, Extra, Func, Imm, Inst, IntPred, Opcode, Value};

/// Rewrites every `switch` in the function into branches, and leaves everything else alone.
///
/// The function is changed in place, which is what makes this the last thing that reads the IR as
/// the front end built it. `--emit=ir` prints before this runs, and nothing after this asks what
/// the program said, only what the machine has to do.
pub fn switches(func: &mut Func) {
    let found: Vec<Inst> = func
        .blocks()
        .filter_map(|block| func.terminator(block))
        .filter(|&inst| func[inst].opcode == Opcode::Switch)
        .collect();
    for inst in found {
        chain(func, inst);
    }
}

/// One `switch`, as a compare and a branch for each case in the order they were written.
///
/// The block the `switch` was in gets the first compare, and each case after the first gets a
/// block of its own that the one before it falls to when its compare failed. The last of them
/// falls to the default, so the default is not a block anything is created for and the chain costs
/// one block per case less one.
///
/// The order is the order the cases are in, which is the order the program wrote them and not a
/// sorted one. Sorting would be the first half of a binary search and the second half is not here,
/// so it would cost a reader the ability to look at the assembly and see their own `switch`, and
/// buy nothing.
fn chain(func: &mut Func, inst: Inst) {
    let block = func.block_of(inst).expect("a terminator is in a block");
    let span = func.span(inst);
    let Extra::Switch(info) = func[inst].extra else { return };
    let info = func[info];
    let value = func[func[inst].args][0];
    // The lane, because a `switch` on a vector is not a thing C can write and the immediates are
    // an integer's either way.
    let ty = func[value].ty.lane();
    let calls: Vec<BlockCall> = func[info.targets].to_vec();
    let cases: Vec<Imm> = func[info.cases].to_vec();
    let Some((default, arms)) = calls.split_first() else { return };

    // Before anything is written, because the builder appends and the `switch` is where the
    // appending has to happen.
    func.remove_inst(inst);

    // A `switch` with nothing but a default is a jump, which is worth writing down rather than
    // refusing: it is what a `switch` whose only label is `default` is, and it is also what one
    // whose cases were all folded away by a later pass would be.
    let Some((first, rest)) = arms.split_first() else {
        let args: Vec<Value> = func[default.args].to_vec();
        Builder::new(func, block).at(span).jump(default.block, &args);
        return;
    };

    let mut at = block;
    for (index, arm) in std::iter::once(first).chain(rest).enumerate() {
        let last = index + 1 == arms.len();
        let next = if last { default.block } else { func.create_block() };
        let onward: Vec<Value> = if last { func[default.args].to_vec() } else { Vec::new() };
        let taken: Vec<Value> = func[arm.args].to_vec();
        let case = cases[index].signed(ty);

        let mut build = Builder::new(func, at).at(span);
        let want = build.iconst(ty, case);
        let same = build.icmp(IntPred::Eq, value, want);
        build.br_if(same, arm.block, &taken, next, &onward);
        at = next;
    }
}

/// The blocks a chain of `n` cases needs beyond the ones the program already had.
///
/// Here so that a test can say the number rather than count it, and so that whoever writes the
/// jump table has one place to compare against.
#[must_use]
pub fn blocks_for(cases: usize) -> usize {
    cases.saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Func, Module, Opcode, Signature, Type};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{blocks_for, switches};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// `int sw(int x) { switch (x) { case 1: return 10; case 2: return 20; default: return 30; } }`
    /// as the walk builds it, which is the program in issue 275.
    fn built(cases: &[i128]) -> (Interner, Func) {
        let mut names = Interner::new();
        let int = Type::int(32);
        let mut func = Func::new(
            names.intern("sw"),
            Signature::new().with_params(&[int]).with_returns(&[int]),
        );
        let entry = func.create_block();
        let x = func.append_param(entry, int);

        let default = func.create_block();
        let arms: Vec<_> = cases.iter().map(|_| func.create_block()).collect();
        let table: Vec<(i128, rucc_ir::Block)> =
            cases.iter().copied().zip(arms.iter().copied()).collect();
        Builder::new(&mut func, entry).switch(x, default, &table);

        for (index, &arm) in arms.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            let what = i128::try_from(index).expect("a small number of cases");
            let v = build.iconst(int, (what + 1) * 10);
            build.ret(&[v]);
        }
        let mut build = Builder::new(&mut func, default);
        let v = build.iconst(int, 30);
        build.ret(&[v]);
        (names, func)
    }

    fn count(func: &Func) -> usize {
        func.blocks().count()
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("sw.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    #[test]
    fn a_switch_becomes_a_compare_and_a_branch_for_each_case() {
        let (mut names, mut func) = built(&[1, 2]);
        let before = count(&func);
        switches(&mut func);
        assert_eq!(count(&func), before + blocks_for(2));

        let text = printed(&func, &mut names);
        assert!(!text.contains("switch"), "the switch is gone: {text}");
        assert_eq!(text.matches("icmp eq").count(), 2, "one compare per case: {text}");
        assert_eq!(text.matches("br_if").count(), 2, "one branch per case: {text}");
    }

    #[test]
    fn the_last_case_falls_to_the_default_rather_than_to_a_block_of_its_own() {
        let (_, mut func) = built(&[7]);
        let before = count(&func);
        switches(&mut func);
        // One case needs no chain block at all: the one compare goes to the arm or to the default.
        assert_eq!(count(&func), before);
        assert_eq!(blocks_for(1), 0);
    }

    #[test]
    fn a_switch_with_only_a_default_is_a_jump() {
        let (_, mut func) = built(&[]);
        switches(&mut func);
        let entry = func.entry().expect("an entry block");
        let term = func.terminator(entry).expect("a terminator");
        assert_eq!(func[term].opcode, Opcode::Jump);
    }

    /// The rewrite has to leave a function the verifier still accepts, since every check it makes
    /// is one the rest of the back end assumes and none of them is rechecked after this runs.
    #[test]
    fn what_comes_out_is_valid_ir() {
        let (mut names, mut func) = built(&[1, 2, 3, 4]);
        switches(&mut func);
        let module = Module::new(names.intern("sw.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    /// Nothing else is touched, which matters because this runs over every function whether or not
    /// one has a `switch` in it.
    #[test]
    fn a_function_with_no_switch_is_left_exactly_as_it_was() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[int]).with_returns(&[int]));
        let entry = func.create_block();
        let x = func.append_param(entry, int);
        Builder::new(&mut func, entry).ret(&[x]);

        let before = printed(&func, &mut names);
        switches(&mut func);
        assert_eq!(printed(&func, &mut names), before);
    }
}
