//! Which calls hand back an object of a size the caller asked for, and where it is not null.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.2, whose first source of a
//! discharge is "a local, a global, or a field of an object whose type is known, at a constant
//! offset", because the extent of such an object is not something anybody has to find out. A frame
//! slot says its extent on its `alloca` and a global says it on the module, and `crate::extents`
//! and `crate::discharge` read those two. The section's sentence does not name an allocation and
//! the argument is the same one: `malloc(n)` hands back either null or one storage instance of at
//! least `n` bytes, so a request for a fixed number of bytes says an extent as plainly as an
//! `alloca` does, with a different instruction saying it.
//!
//! On the SQLite amalgamation 1800 of the bounds checks the discharge pass keeps are on a pointer
//! a call returned, which is the fourth row of the measurement in `tamnd/rucc#693`. None of those
//! 1800 are this: SQLite allocates through `sqlite3Malloc` and friends, which is a wrapper around
//! an indirect call, and the amalgamation contains exactly one direct call to `malloc`, whose size
//! it works out. What this is worth is on ordinary C that calls the allocator itself, and the row
//! is measured again under "What is not here" below.
//!
//! # Two halves, because they want different things
//!
//! [`annotate`] runs before the pipeline and does the one thing that has to happen there: it reads
//! the name of the function each call names, which takes the interner, and a pass is handed a
//! function and no names. What it writes is [`Flags::NOFREE`]'s neighbour, a fact about a call
//! worked out from the name it calls.
//!
//! Everything else is read by `crate::discharge` out of the IR in front of it, and it has to be,
//! because before the pipeline runs `malloc(16)` is a call to `malloc` of a sign extension of a
//! thirty two bit sixteen. The size is a constant the way the program wrote it rather than the way
//! a fact is read, and so are the offsets the checks are at. Waiting until the folding has happened
//! is the difference between reading them and writing a second constant folder here.
//!
//! # Why the null test is part of it
//!
//! Because a null pointer is inside no object at all, so a bounds check on one is a check that is
//! supposed to fail, and a program that reads through what `malloc` gave it without looking is a
//! program with the bug this compiler is for. Taking the check out on the strength of the size
//! alone would be losing exactly the case worth catching.
//!
//! So the extent is only believed where the program has already found out the pointer is not null,
//! and finding that out is a comparison against null and a branch. What is asked of every block is
//! that every way into it either comes over the arm of such a branch or comes from a block where
//! the same was already true. That is more than asking which block the branch dominates, and the
//! difference is the shape a program that checks its allocation and then does two things with it
//! has: an `if` inside the tested arm joins back, and the block after the join has two ways in that
//! have both been through the test.
//!
//! # What is claimed and what is not
//!
//! An extent, and not a lifetime. A global and a caller's frame slot are alive for as long as the
//! call runs, which is why [`Flags::STATIC`] and [`Flags::HANDED`] say both things at once. An
//! object on the heap is alive until something frees it and the something can be a `free` three
//! lines further down this same function. So nothing here answers a lifetime check, that check
//! stays, and it is the one that reports the use after free. The bounds check going does not hide
//! that: the two run one after the other on the same access, and what a program gets told is the
//! lifetime refusal instead of whatever the bounds check would have said about a dangling address.
//!
//! # What is not here
//!
//! A pointer that came from a call to anything not in the table below, which includes every
//! program's own allocation wrapper, and a size that is not a constant, which is what most of those
//! wrappers are handed. Both of those want `crate::params` read in the other direction, so that a
//! wrapper every call to which asks for at least so many bytes is a wrapper whose result has them.
//!
//! That is where the rest of the row is, and it is nearly all of it. Grouping SQLite's kept bounds
//! checks by what the base came from puts 309 on `sqlite3DbMallocZero`, 217 on
//! `sqlite3DbMallocRawNN`, 109 on `sqlite3MallocZero`, 52 on `sqlite3_malloc64`, 48 on
//! `sqlite3DbMallocRaw` and 30 on `sqlite3Malloc`, and none at all on `malloc`.

use std::collections::HashSet;

use rucc_base::Interner;
use rucc_ir::{Block, Def, Extra, Flags, Func, FuncId, IntPred, Module, Opcode, Value};

use crate::cfg::Cfg;
use crate::discharge::{Fact, constant, operand_of};

/// The functions whose result is either null or a fresh storage instance of at least as many bytes
/// as the call's last argument says.
///
/// The last argument rather than a reading per function, because for every name here that is a
/// true lower bound and one reading is easier to be sure of than six. It is exact for `malloc`,
/// `realloc`, `reallocf` and `valloc`, and it understates `calloc(count, size)`, whose instance is
/// the product, and `aligned_alloc(align, size)`, whose instance is rounded up. Understating is the
/// direction that costs a check rather than hides a bug, and it costs nothing at all on the
/// accesses these are about, which are to the object the program asked for and not to the padding.
///
/// `realloc` is here even though it may free what it was given, because what is claimed is about
/// the pointer that comes out and not about the one that went in.
///
/// Sorted, and a test checks that it is sorted and says each name once.
const MAKES: &[&str] = &["aligned_alloc", "calloc", "malloc", "realloc", "reallocf", "valloc"];

/// Nothing this believes is anywhere near this many bytes, and a size past it is a program doing
/// something other than making an object.
///
/// The same four gigabytes `crates/rucc-opt/rules/safety.rules` bounds its numbers at, and for the
/// reason that file gives: past it the arithmetic the pass does and the arithmetic a rule is proved
/// in stop agreeing.
const LARGEST: i128 = 4 * 1024 * 1024 * 1024;

/// Writes [`Flags::HEAP`] onto every call this module can vouch for.
///
/// Gives back how many calls were marked, which is what the pipeline reports.
///
/// Only sets the flag, never clears one, for the reason [`crate::nofree::annotate`] gives: the flag
/// is an assertion, so a caller that put one there meant it.
pub fn annotate(module: &mut Module, names: &Interner) -> usize {
    // A module that defines one of these names itself is the allocator's own source, and what a
    // function called `malloc` does in there is whatever it was written to do.
    let defined: HashSet<&str> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| names.resolve(module[id].name))
        .collect();
    let mut marked = 0;
    let ids: Vec<FuncId> = module.funcs().collect();
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        let func = &module[id];
        let marks: Vec<_> = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::Call)
            .filter(|&inst| !func[inst].flags.contains(Flags::HEAP))
            .filter(|&inst| {
                let Extra::Call(at) = func[inst].extra else { return false };
                let Some(callee) = func[at].callee else { return false };
                let name = names.resolve(callee);
                !defined.contains(name) && MAKES.binary_search(&name).is_ok()
            })
            .collect();
        marked += marks.len();
        let func = &mut module[id];
        for inst in marks {
            func[inst].flags |= Flags::HEAP;
        }
    }
    marked
}

/// Whether anything in this function hands back an object of a size it was asked for.
///
/// What `crate::discharge` reads to find out whether the graph is worth building.
pub(crate) fn allocates(func: &Func) -> bool {
    func.blocks().any(|block| {
        func.insts(block)
            .any(|inst| func[inst].opcode == Opcode::Call && func[inst].flags.contains(Flags::HEAP))
    })
}

/// The whole of the object a value is, when the value came out of a call this vouched for.
///
/// The shape `crate::discharge::declared` has for a frame slot, with a call saying the size instead
/// of an `alloca`. `None` covers everything from the value being something else to the size not
/// being a number by the time this is asked.
pub(crate) fn made(func: &Func, base: Value) -> Option<Fact> {
    let Def::Result { inst, index: 0 } = func[base].def else { return None };
    if !func[inst].flags.contains(Flags::HEAP) {
        return None;
    }
    let &last = func[func[inst].args].last()?;
    let size = constant(func, last)?;
    (size > 0 && size <= LARGEST).then(|| Fact::whole(base, size))
}

/// The blocks where the program has already found out this pointer is not null.
///
/// A block is in when every way into it either comes over the arm of a branch on a comparison of
/// this pointer against null, or comes from a block already in.
///
/// The round starts by believing it of every block and takes it away, which is what a claim about
/// every path has to do to say anything about a loop. The entry block is the one it never believes:
/// nothing has been tested before the function starts, and that is what makes the answer travel
/// forward from there rather than hold itself up. A block the entry does not reach is left out
/// altogether, so nothing is claimed about code nothing runs.
pub(crate) fn tested(func: &Func, cfg: &Cfg, pointer: Value) -> HashSet<Block> {
    let Some(entry) = cfg.entry() else { return HashSet::new() };
    let order: Vec<Block> = cfg.reverse_postorder().collect();
    let mut known: HashSet<Block> = order.iter().copied().filter(|&block| block != entry).collect();
    loop {
        let mut settled = true;
        for &block in &order {
            if !known.contains(&block) {
                continue;
            }
            let preds = cfg.predecessors(block);
            let holds = !preds.is_empty()
                && preds
                    .iter()
                    .all(|&pred| known.contains(&pred) || proves(func, pred, block, pointer));
            if !holds {
                known.remove(&block);
                settled = false;
            }
        }
        if settled {
            return known;
        }
    }
}

/// Whether taking this edge is what finding out the pointer is not null looks like.
///
/// Both arms going to the same block answers no rather than yes. One of the two is the arm where
/// the pointer is null, and an edge that is both arms at once has been through neither test.
fn proves(func: &Func, pred: Block, into: Block, pointer: Value) -> bool {
    let Some(term) = func.terminator(pred) else { return false };
    if func[term].opcode != Opcode::BrIf {
        return false;
    }
    let Extra::Targets(targets) = func[term].extra else { return false };
    let [first, second] = func[targets] else { return false };
    if first.block == second.block {
        return false;
    }
    let Some(&condition) = func[func[term].args].first() else { return false };
    // The first target is the one taken when the condition is one, which is what `Builder::br_if`
    // writes, so a test for inequality reaches the pointer that is not null down the first arm and
    // a test for equality reaches it down the second.
    match against_null(func, condition, pointer) {
        Some(IntPred::Ne) => first.block == into,
        Some(IntPred::Eq) => second.block == into,
        _ => false,
    }
}

/// Which way a value compares that pointer against null, when that is what it does.
fn against_null(func: &Func, condition: Value, pointer: Value) -> Option<IntPred> {
    let Def::Result { inst, .. } = func[condition].def else { return None };
    if func[inst].opcode != Opcode::ICmp {
        return None;
    }
    let Extra::IntPred(pred) = func[inst].extra else { return None };
    if !matches!(pred, IntPred::Eq | IntPred::Ne) {
        return None;
    }
    let args = &func[func[inst].args];
    let (&left, &right) = (args.first()?, args.get(1)?);
    let matched = (left == pointer && null(func, right)) || (right == pointer && null(func, left));
    matched.then_some(pred)
}

/// Whether that value is the null pointer.
///
/// Two spellings, because the frontend writes the constant as an integer and turns it into a
/// pointer, and by the time this is asked the pair may have been folded into one.
fn null(func: &Func, value: Value) -> bool {
    if constant(func, value) == Some(0) {
        return true;
    }
    operand_of(func, value, Opcode::IntToPtr, 0)
        .is_some_and(|inner| constant(func, inner) == Some(0))
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Func, Linkage, Signature, Type};
    use rucc_target::{TargetInfo, Triple};

    use super::{
        Fact, Flags, IntPred, LARGEST, MAKES, Module, Opcode, Value, allocates, made, tested,
    };
    use crate::cfg::Cfg;

    /// An empty module for a sixty four bit Linux.
    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        Module::new(names.intern("t.c"), &target)
    }

    /// What an allocator's declaration says, which is one size in and one pointer out.
    fn shape() -> Signature {
        Signature::new().with_params(&[Type::int(64)]).with_returns(&[Type::PTR])
    }

    /// Puts a function called `at` into the module whose body calls `name` with one constant.
    fn calls(names: &mut Interner, module: &mut Module, at: &str, name: &str, size: i128) {
        let at = names.intern(at);
        let called = names.intern(name);
        let mut func = Func::new(at, Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(shape());
        let bytes = build.iconst(Type::int(64), size);
        build.call(called, signature, &[bytes]);
        build.ret(&[]);
        module.add_func(func);
    }

    /// Whether the one call in a function came out marked.
    fn marked(func: &Func) -> bool {
        allocates(func)
    }

    /// The call every one of these tests is about, already marked the way [`super::annotate`]
    /// marks it, since which calls deserve the flag is a question about a module.
    fn allocation(names: &mut Interner, build: &mut Builder<'_>, size: i128) -> Value {
        let called = names.intern("malloc");
        let signature = build.func().add_signature(shape());
        let bytes = build.iconst(Type::int(64), size);
        let call = build.call(called, signature, &[bytes]);
        let func = build.func();
        func[call].flags |= Flags::HEAP;
        func[call].results().next().expect("a call that gives back a pointer")
    }

    #[test]
    fn the_names_are_sorted_and_each_one_is_written_once() {
        // `binary_search` is what reads the table, so an unsorted entry would be an entry nothing
        // ever finds, and it would go on being nothing that ever fires rather than a failure.
        let mut sorted = MAKES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, MAKES);
    }

    #[test]
    fn a_call_to_an_allocator_is_marked_and_a_call_to_anything_else_is_not() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        calls(&mut names, &mut module, "f", "malloc", 16);
        calls(&mut names, &mut module, "g", "reallocf", 16);
        calls(&mut names, &mut module, "h", "memcpy", 16);
        assert_eq!(super::annotate(&mut module, &names), 2);
        let found: Vec<bool> = module.funcs().map(|id| marked(&module[id])).collect();
        assert_eq!(found, [true, true, false]);
    }

    #[test]
    fn a_module_that_writes_its_own_allocator_is_left_alone() {
        // Inside the allocator's own source a function called `malloc` is whatever it was written
        // to be, and the one place that matters is the runtime in `runtime/rucc-safe-rt`.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        calls(&mut names, &mut module, "f", "malloc", 16);
        let name = names.intern("malloc");
        let mut mine = Func::new(name, shape());
        mine.linkage = Linkage::External;
        let block = mine.create_block();
        let mut build = Builder::new(&mut mine, block);
        build.ret(&[]);
        module.add_func(mine);
        assert_eq!(super::annotate(&mut module, &names), 0);
    }

    #[test]
    fn a_declaration_is_walked_over_rather_than_into() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let name = names.intern("malloc");
        module.add_func(Func::new(name, shape()));
        calls(&mut names, &mut module, "f", "malloc", 16);
        assert_eq!(super::annotate(&mut module, &names), 1);
    }

    #[test]
    fn the_size_is_read_off_the_last_argument_and_has_to_be_a_number_in_range() {
        for (size, works) in
            [(16, true), (LARGEST, true), (0, false), (-1, false), (LARGEST + 1, false)]
        {
            let mut names = Interner::new();
            let name = names.intern("f");
            let mut func = Func::new(name, Signature::new());
            let block = func.create_block();
            let mut build = Builder::new(&mut func, block);
            let pointer = allocation(&mut names, &mut build, size);
            build.ret(&[]);
            let wanted = works.then(|| Fact::whole(pointer, size));
            assert_eq!(made(&func, pointer), wanted, "{size}");
        }
    }

    #[test]
    fn a_pointer_from_a_call_nobody_marked_is_not_an_allocation() {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let called = names.intern("mine");
        let signature = build.func().add_signature(shape());
        let bytes = build.iconst(Type::int(64), 16);
        let call = build.call(called, signature, &[bytes]);
        let pointer = build.func()[call].results().next().expect("a pointer");
        build.ret(&[]);
        assert_eq!(made(&func, pointer), None);
    }

    #[test]
    fn the_tested_arm_is_the_one_the_comparison_says_it_is() {
        // The same function twice, once written `if (p) ...` and once `if (!p) ...`, and the block
        // that is in the answer swaps over with it.
        for pred in [IntPred::Ne, IntPred::Eq] {
            let mut names = Interner::new();
            let name = names.intern("f");
            let mut func = Func::new(name, Signature::new());
            let entry = func.create_block();
            let first = func.create_block();
            let second = func.create_block();
            let mut build = Builder::new(&mut func, entry);
            let pointer = allocation(&mut names, &mut build, 16);
            let zero = build.iconst(Type::int(64), 0);
            let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
            let condition = build.icmp(pred, pointer, null);
            build.br_if(condition, first, &[], second, &[]);
            for block in [first, second] {
                let mut build = Builder::new(&mut func, block);
                build.ret(&[]);
            }

            let cfg = Cfg::new(&func);
            let known = tested(&func, &cfg, pointer);
            let good = if pred == IntPred::Ne { first } else { second };
            let bad = if pred == IntPred::Ne { second } else { first };
            assert!(known.contains(&good), "{pred:?}");
            assert!(!known.contains(&bad), "{pred:?}");
            assert!(!known.contains(&entry), "nothing is known before the test runs");
        }
    }

    #[test]
    fn a_join_of_two_paths_that_have_both_been_through_the_test_is_still_tested() {
        // This is the whole reason the answer is a round over the graph rather than a question for
        // the dominator tree. `if (p) { if (v) ...; use(p); }` puts two ways into the block that
        // uses it, neither of them the branch on null, and both of them past it.
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::int(32)]));
        let entry = func.create_block();
        let inside = func.create_block();
        let arm = func.create_block();
        let join = func.create_block();
        let outside = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let value = build.func().append_param(entry, Type::int(32));
        let pointer = allocation(&mut names, &mut build, 16);
        let zero = build.iconst(Type::int(64), 0);
        let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
        let condition = build.icmp(IntPred::Ne, pointer, null);
        build.br_if(condition, inside, &[], outside, &[]);
        let mut build = Builder::new(&mut func, inside);
        let none = build.iconst(Type::int(32), 0);
        let again = build.icmp(IntPred::Ne, value, none);
        build.br_if(again, arm, &[], join, &[]);
        let mut build = Builder::new(&mut func, arm);
        build.jump(join, &[]);
        for block in [join, outside] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        let cfg = Cfg::new(&func);
        let known = tested(&func, &cfg, pointer);
        assert!(known.contains(&inside));
        assert!(known.contains(&arm));
        assert!(known.contains(&join), "both ways in have been through the test");
        assert!(!known.contains(&outside));
        assert!(!known.contains(&entry));
    }

    #[test]
    fn a_block_a_path_reaches_without_the_test_is_not_tested() {
        // The shape `if (!p) abort();` has, since nothing here knows that a call does not come
        // back. One of the two ways into the block after it is the arm where the pointer is null,
        // and refusing it is the right answer rather than a missed one.
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new());
        let entry = func.create_block();
        let sorry = func.create_block();
        let after = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let pointer = allocation(&mut names, &mut build, 16);
        let zero = build.iconst(Type::int(64), 0);
        let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
        let condition = build.icmp(IntPred::Eq, pointer, null);
        build.br_if(condition, sorry, &[], after, &[]);
        let mut build = Builder::new(&mut func, sorry);
        build.jump(after, &[]);
        let mut build = Builder::new(&mut func, after);
        build.ret(&[]);

        let cfg = Cfg::new(&func);
        let known = tested(&func, &cfg, pointer);
        assert!(!known.contains(&after));
        assert!(!known.contains(&sorry));
    }

    #[test]
    fn a_loop_the_test_is_outside_of_is_tested_all_the_way_round() {
        // What the optimistic start is for. The header has two ways in, one from the branch on
        // null and one from the latch, and reading the latch pessimistically would take the answer
        // away from the header and then from the latch, which is the fact holding itself up in
        // reverse.
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::int(32)]));
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let value = build.func().append_param(entry, Type::int(32));
        let pointer = allocation(&mut names, &mut build, 16);
        let zero = build.iconst(Type::int(64), 0);
        let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
        let condition = build.icmp(IntPred::Ne, pointer, null);
        build.br_if(condition, header, &[], done, &[]);
        let mut build = Builder::new(&mut func, header);
        let none = build.iconst(Type::int(32), 0);
        let again = build.icmp(IntPred::Ne, value, none);
        build.br_if(again, body, &[], done, &[]);
        let mut build = Builder::new(&mut func, body);
        build.jump(header, &[]);
        let mut build = Builder::new(&mut func, done);
        build.ret(&[]);

        let cfg = Cfg::new(&func);
        let known = tested(&func, &cfg, pointer);
        assert!(known.contains(&header));
        assert!(known.contains(&body));
        // Both ways into it have been through the branch, one down each arm, and one of those is
        // the arm where the pointer is null.
        assert!(!known.contains(&done));
    }
}
