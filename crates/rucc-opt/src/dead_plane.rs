//! A write to a plane that a later write covers, with nothing reading it in between.
//!
//! `spec/safe-memory/07-check-elimination.md` section 7.6, the first of its three rules: "A
//! `meta_type` or `meta_init` whose range is entirely overwritten by a later one on all paths, with
//! no intervening check that reads it, is dead. This is dead-store elimination over the planes and
//! it uses the same machinery." What is here is the straight line half of that, a pair inside one
//! block, where on all paths is one path and there is nothing to prove about the shape of the graph.
//!
//! ```c
//! void twice(int **p, int *x, int *y) { *p = x; *p = y; }
//! ```
//!
//! Under `-fsafety` each of those two stores gets a `meta_type` and a `meta_init` over the same
//! eight bytes. The second pair says everything the first pair said, nothing between them asks the
//! planes anything, and so the first pair is two writes whose result nobody ever sees.
//!
//! # What makes a write dead and what does not
//!
//! The later write has to cover the earlier one whole. Covering part of it leaves the rest saying
//! what the earlier write said, which is a thing a later check can still read. The node a
//! `meta_type` carries is not part of it, in either direction: a write that covers the bytes
//! replaces whatever was there with its own node, so an earlier write of a different node is as
//! dead as an earlier write of the same one. That is the difference from [`crate::coalesce`], which
//! joins two writes into one and so does need them to agree.
//!
//! Nothing between the two may read the plane being written. A check that reads it is the whole
//! reason the earlier write was there, and a call is anything at all. The list of what may sit
//! between them is the one [`crate::coalesce`] moves a run past, which is the same list for the
//! same reason: a write that may be moved past an instruction is a write that instruction cannot
//! be reading.
//!
//! # What may never go
//!
//! Section 7.6 names it: `meta_begin` and `meta_end` for a storage instance whose address escapes,
//! and `meta_transfer`. None of the three is a plane write and none of them is reachable from here,
//! since what this pass calls a plane write is `meta_type` and `meta_init` and nothing else.
//! `meta_epoch` is not here either, for the reason [`crate::coalesce`] gives about it.
//!
//! # What the rule proves and what this file decides
//!
//! Section 7.7's split again. Working out that two plane writes are about one base a constant
//! distance apart, and that nothing between them reads the plane, is ordinary code and it is here.
//! Getting from two byte counts to one range of addresses lying inside another is arithmetic at
//! sixty four bits, and that is `covered.i64` in `crates/rucc-opt/rules/safety.rules`. It is the
//! rule the discharge pass asks about two accesses, asked here about two writes, because the
//! containment of one range of bytes in another is one question however it came up.

use rucc_ir::{Func, Inst};

use crate::coalesce::{Write, crossed, plane, read};
use crate::discharge::{Fact, covers};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// A write nobody could have read went.
const DEAD: &str = "plane write removed, a later write covers it and nothing in between reads it";

/// The two writes are to one plane and neither covers the other.
const NOT_COVERED: &str = "plane write kept, no later write to that plane covers all of its bytes";

/// Ran out.
const NO_FUEL: &str = "plane write kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug)]
pub struct DeadPlane;

impl Pass for DeadPlane {
    fn name(&self) -> &'static str {
        "dead-plane"
    }

    fn describe(&self) -> &'static str {
        "a write to a plane that a later write covers, with nothing reading it in between, goes"
    }

    fn preserves(&self) -> Preserved {
        // The graph does not change and nothing is defined anywhere new. What does change is that
        // a pointer stops being used where it was, which is what liveness is a statement about.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let blocks: Vec<_> = func.blocks().collect();
        for block in blocks {
            let insts: Vec<Inst> = func.insts(block).collect();
            let mut standing: Vec<Write> = Vec::new();
            for inst in insts {
                let opcode = func[inst].opcode;
                let Some(kind) = plane(opcode) else {
                    // Anything that might read a plane takes every write to it off the list,
                    // because a write something reads is a write that did its job.
                    standing.retain(|write| crossed(opcode, write.kind));
                    continue;
                };
                let Some(write) = read(func, inst, kind) else {
                    // A write whose own range the pass cannot read could be covering anything, so
                    // nothing is decided about the writes in front of it either way.
                    standing.retain(|standing| standing.kind != kind);
                    continue;
                };
                bury(func, &mut standing, &write, &mut stats, fuel);
                standing.push(write);
            }
        }
        stats
    }
}

/// Takes out every write on the list the new one covers whole.
fn bury(
    func: &mut Func,
    standing: &mut Vec<Write>,
    write: &Write,
    stats: &mut Stats,
    fuel: &mut Fuel,
) {
    let over = Fact::range(write.base, write.at, write.size);
    let mut gone = Vec::new();
    for (index, earlier) in standing.iter().enumerate() {
        if earlier.kind != write.kind {
            continue;
        }
        if !covers(&over, &Fact::range(earlier.base, earlier.at, earlier.size)) {
            stats.missed(NOT_COVERED);
            continue;
        }
        if !fuel.take() {
            stats.missed(NO_FUEL);
            continue;
        }
        gone.push(index);
    }
    // Backwards, so that removing one does not move the next.
    for &index in gone.iter().rev() {
        func.remove_inst(standing.remove(index).inst);
        stats.optimized(DEAD);
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::{Idx, Interner};
    use rucc_ir::{Block, Builder, Extra, Flags, Func, InstData, Opcode, Signature, Type, Value};

    use super::DeadPlane;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// An empty function taking one pointer, with its entry block and that pointer.
    fn start(names: &mut Interner) -> (Func, Block, Value) {
        let signature = Signature::new().with_params(&[Type::PTR]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let base = func.append_param(entry, Type::PTR);
        (func, entry, base)
    }

    /// The address that many bytes along from a pointer.
    fn walk(build: &mut Builder<'_>, base: Value, offset: i64) -> Value {
        let step = build.iconst(Type::int(64), i128::from(offset));
        build.binary(Opcode::PtrAdd, base, step, Flags::NONE)
    }

    /// One plane write of that many bytes at that address, naming that node.
    fn plane(build: &mut Builder<'_>, kind: Opcode, address: Value, size: i64, node: u32) {
        let width = build.iconst(Type::int(64), i128::from(size));
        let extra = match kind {
            Opcode::MetaType => Extra::Node(Idx::new(node)),
            _ => Extra::None,
        };
        let args = build.func().push_values(&[address, width]);
        build.inst(InstData { args, extra, ..InstData::new(kind) }, &[]);
    }

    /// A function with plane writes of one kind at the offsets and widths given, in that order.
    fn writes_of(names: &mut Interner, kind: Opcode, at: &[(i64, i64)]) -> Func {
        let (mut func, entry, base) = start(names);
        let mut build = Builder::new(&mut func, entry);
        for &(offset, size) in at {
            let address = walk(&mut build, base, offset);
            plane(&mut build, kind, address, size, 1);
        }
        build.ret(&[]);
        func
    }

    /// Every write to that plane still in the function, as where it starts and how wide it is.
    fn left(func: &Func, kind: Opcode) -> Vec<(i128, i128)> {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == kind)
            .map(|inst| {
                let [pointer, length] = func[func[inst].args] else { panic!("two operands") };
                let (_, at) = crate::discharge::normal(func, pointer);
                let (imm, ty) = crate::fold::constant(func, length).expect("a constant width");
                (at, imm.signed(ty))
            })
            .collect()
    }

    fn run(func: &mut Func) -> Stats {
        with(func, &mut Fuel::unlimited())
    }

    fn with(func: &mut Func, fuel: &mut Fuel) -> Stats {
        DeadPlane.run(func, &mut crate::machine::fixtures::analyses(), fuel)
    }

    #[test]
    fn a_field_written_twice_keeps_only_the_second_pair_of_plane_writes() {
        let mut names = Interner::new();
        let mut func = writes_of(&mut names, Opcode::MetaInit, &[(0, 8), (0, 8)]);
        let stats = run(&mut func);
        assert!(stats.changed());
        assert_eq!(left(&func, Opcode::MetaInit), [(0, 8)]);
    }

    #[test]
    fn a_wider_write_buries_every_narrower_one_it_covers() {
        let mut names = Interner::new();
        let mut func = writes_of(&mut names, Opcode::MetaInit, &[(0, 8), (8, 8), (16, 8), (0, 24)]);
        let stats = run(&mut func);
        assert!(stats.changed());
        assert_eq!(left(&func, Opcode::MetaInit), [(0, 24)]);
    }

    #[test]
    fn a_write_that_covers_only_part_of_an_earlier_one_leaves_it_alone() {
        let mut names = Interner::new();
        let mut func = writes_of(&mut names, Opcode::MetaInit, &[(0, 8), (0, 4)]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(left(&func, Opcode::MetaInit), [(0, 8), (0, 4)]);
        assert_eq!(stats.count(Kind::Missed, super::NOT_COVERED), 1);
    }

    #[test]
    fn two_writes_over_bytes_that_do_not_meet_are_both_kept() {
        let mut names = Interner::new();
        let mut func = writes_of(&mut names, Opcode::MetaInit, &[(0, 8), (8, 8)]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(left(&func, Opcode::MetaInit), [(0, 8), (8, 8)]);
    }

    #[test]
    fn the_node_an_earlier_write_names_does_not_save_it() {
        // What a later write does to the bytes it covers is replace whatever they said, so an
        // earlier write naming a different node is as dead as one naming the same node. This is
        // where this pass and the coalescing one part company, since that one joins two writes
        // into one and does need them to agree.
        let mut names = Interner::new();
        let (mut func, entry, base) = start(&mut names);
        let mut build = Builder::new(&mut func, entry);
        for node in [1, 2] {
            let address = walk(&mut build, base, 0);
            plane(&mut build, Opcode::MetaType, address, 8, node);
        }
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(stats.changed());
        assert_eq!(left(&func, Opcode::MetaType), [(0, 8)]);
    }

    #[test]
    fn a_read_of_the_same_plane_between_them_is_what_the_earlier_write_was_for() {
        for (between, kept) in
            [(Opcode::CheckInit, vec![(0, 8), (0, 8)]), (Opcode::CheckBounds, vec![(0, 8)])]
        {
            let mut names = Interner::new();
            let (mut func, entry, base) = start(&mut names);
            let mut build = Builder::new(&mut func, entry);
            let first = walk(&mut build, base, 0);
            plane(&mut build, Opcode::MetaInit, first, 8, 1);
            build.inst(InstData::new(between), &[]);
            let second = walk(&mut build, base, 0);
            plane(&mut build, Opcode::MetaInit, second, 8, 1);
            build.ret(&[]);
            run(&mut func);
            assert_eq!(left(&func, Opcode::MetaInit), kept, "across {}", between.name());
        }
    }

    #[test]
    fn a_call_between_them_keeps_the_earlier_write() {
        let mut names = Interner::new();
        let (mut func, entry, base) = start(&mut names);
        let mut build = Builder::new(&mut func, entry);
        let first = walk(&mut build, base, 0);
        plane(&mut build, Opcode::MetaInit, first, 8, 1);
        build.inst(InstData::new(Opcode::Call), &[]);
        let second = walk(&mut build, base, 0);
        plane(&mut build, Opcode::MetaInit, second, 8, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(left(&func, Opcode::MetaInit), [(0, 8), (0, 8)]);
    }

    #[test]
    fn a_write_to_one_plane_does_not_bury_a_write_to_the_other() {
        let mut names = Interner::new();
        let (mut func, entry, base) = start(&mut names);
        let mut build = Builder::new(&mut func, entry);
        let address = walk(&mut build, base, 0);
        plane(&mut build, Opcode::MetaType, address, 8, 1);
        plane(&mut build, Opcode::MetaInit, address, 8, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(left(&func, Opcode::MetaType), [(0, 8)]);
        assert_eq!(left(&func, Opcode::MetaInit), [(0, 8)]);
    }

    #[test]
    fn the_epoch_plane_is_left_alone_however_its_writes_line_up() {
        let mut names = Interner::new();
        let mut func = writes_of(&mut names, Opcode::MetaEpoch, &[(0, 8), (0, 8)]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(left(&func, Opcode::MetaEpoch), [(0, 8), (0, 8)]);
    }

    #[test]
    fn a_write_whose_width_the_pass_cannot_read_decides_nothing_about_the_ones_in_front_of_it() {
        let mut names = Interner::new();
        let (mut func, entry, base) = start(&mut names);
        let mut build = Builder::new(&mut func, entry);
        let first = walk(&mut build, base, 0);
        plane(&mut build, Opcode::MetaInit, first, 8, 1);
        let width = build.unary(Opcode::PtrToInt, base, Type::int(64));
        let args = build.func().push_values(&[first, width]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaInit) }, &[]);
        let third = walk(&mut build, base, 0);
        plane(&mut build, Opcode::MetaInit, third, 8, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
    }

    #[test]
    fn a_write_in_another_block_does_not_reach_back_into_this_one() {
        // On all paths is what section 7.6 asks for and one block is the half of it that is here.
        // A write in a block further on covers this one only on the paths that get there.
        let mut names = Interner::new();
        let (mut func, entry, base) = start(&mut names);
        let next = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let address = walk(&mut build, base, 0);
        plane(&mut build, Opcode::MetaInit, address, 8, 1);
        build.jump(next, &[]);
        let mut build = Builder::new(&mut func, next);
        let again = walk(&mut build, base, 0);
        plane(&mut build, Opcode::MetaInit, again, 8, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(left(&func, Opcode::MetaInit), [(0, 8), (0, 8)]);
    }

    #[test]
    fn no_fuel_leaves_every_write_where_it_is() {
        let mut names = Interner::new();
        let mut func = writes_of(&mut names, Opcode::MetaInit, &[(0, 8), (0, 8)]);
        let stats = with(&mut func, &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(left(&func, Opcode::MetaInit), [(0, 8), (0, 8)]);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
    }

    #[test]
    fn a_function_with_no_body_is_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let stats = run(&mut func);
        assert!(!stats.changed());
    }
}
