//! A run of writes to one plane, as one write over the whole range.
//!
//! `spec/safe-memory/07-check-elimination.md` section 7.6 asks for this and gives the shape it
//! takes: "A loop that stores a scalar array element by element performs `n` `meta_init` bit-sets.
//! Coalesced into one range operation before or after the loop ... The same applies to `meta_type`
//! over a `memset`." What is here is the straight line half of that, a run inside one block, and it
//! is the half that fires on ordinary code without any loop analysis at all.
//!
//! ```c
//! struct s { int *a, *b, *c; };
//! void fill(struct s *p, int *x) { p->a = x; p->b = x; p->c = x; }
//! ```
//!
//! Under `-fsafety` each of those three stores gets a `meta_type` and a `meta_init` over its own
//! eight bytes, so six plane writes go out for what is one twenty four byte range in each of two
//! planes. The three type writes name the same node, because the three fields have the same type,
//! and the three ranges sit end to end. So the six become two, and each of the two says exactly
//! what the three it replaced said between them.
//!
//! # Why the merged write sinks rather than rises
//!
//! It goes where the last write of the run was, not where the first was, and the direction is the
//! whole of what makes this safe rather than nearly safe. The two ways to be wrong are not
//! symmetric. A plane that has not yet been told about a store refuses a read of those bytes, which
//! is a false positive: a correct program stopped. A plane told about a store that has not happened
//! yet permits a read of memory nothing has written, which is a use of uninitialized storage that
//! goes unreported. The first is a bug and the second is a hole, so the pass never claims more than
//! has already happened, and the merged write lands at the point where all of it had.
//!
//! # Why the epoch plane is not in this
//!
//! `meta_epoch` looks like the other two and is left alone on purpose. The other two write a fact
//! that does not depend on when it is written: the bytes have this type, the bytes are initialized.
//! `meta_epoch` writes the thread's own clock, and the runtime ticks that clock as it goes, so
//! merging two of them does not write the same thing twice over a wider range. It writes a
//! different stamp. A higher stamp left in the plane is a later `found` for the comparison in
//! `crate::epoch`'s runtime counterpart, and a later `found` makes a race more likely to be
//! reported, which is the false positive direction again. Section 7.6 names `meta_init` and
//! `meta_type` and nothing else, and this is the reason the list stops there.
//!
//! # What the rule proves and what this file decides
//!
//! Section 7.7's split, the same one [`crate::discharge`] is written against. Working out that two
//! plane writes are about one base a constant distance apart, and that nothing between them reads
//! the plane, is ordinary code and it is here. Getting from two adjacent byte counts to one write
//! over both covering the same addresses and no others is arithmetic at sixty four bits, and that
//! is `joined.i64` in `crates/rucc-opt/rules/safety.rules`, which a solver agrees with before the
//! build finishes.

use rucc_ir::{Extra, Func, Inst, InstData, Opcode, Type, Value};

use crate::rules::safety;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// A run of writes to one plane became a single write.
const MERGED: &str = "plane writes merged, a run over one range became one write over all of it";

/// The run was one write long, so there was nothing to merge.
const ALONE: &str = "plane write left alone, nothing next to it writes the same plane";

/// The rules would not join the two ranges.
const NOT_ADJACENT: &str = "plane writes left alone, the rules do not join their two ranges";

/// The write's own operands are not ones the pass can read.
const NOT_A_NUMBER: &str = "plane write left alone, its width is not a constant";

/// Ran out.
const NO_FUEL: &str = "plane writes left alone, the pass ran out of fuel";

/// The widest merged range the rule will discharge, which is the rule file's own bound.
const LIMIT: i128 = 4_294_967_296;

/// The pass.
#[derive(Debug)]
pub struct Coalesce;

impl Pass for Coalesce {
    fn name(&self) -> &'static str {
        "coalesce"
    }

    fn describe(&self) -> &'static str {
        "writes to one plane that sit next to each other become one write over the whole range"
    }

    fn preserves(&self) -> Preserved {
        // The graph does not change and nothing is defined anywhere new, so every analysis about
        // the shape of the function still holds. What does change is where a pointer is last used,
        // which is the one thing liveness is a statement about.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let blocks: Vec<_> = func.blocks().collect();
        for block in blocks {
            let insts: Vec<Inst> = func.insts(block).collect();
            let mut open: Vec<Run> = Vec::new();
            for inst in insts {
                let opcode = func[inst].opcode;
                let Some(kind) = plane(opcode) else {
                    flush(func, &mut open, |run| !crossed(opcode, run.kind), &mut stats, fuel);
                    continue;
                };
                match read(func, inst, kind) {
                    Some(write) => extend(func, &mut open, write, &mut stats, fuel),
                    None => {
                        stats.missed(NOT_A_NUMBER);
                        flush(func, &mut open, |run| run.kind == kind, &mut stats, fuel);
                    }
                }
            }
            // The end of the block, where every run still open has to go somewhere. This is where
            // the common case lands, since a struct filled in field by field is usually the last
            // thing its block does.
            flush(func, &mut open, |_| true, &mut stats, fuel);
        }
        stats
    }
}

/// Which plane a write is to, for the two planes this pass touches.
///
/// `meta_epoch` is deliberately not here, for the reason the module comment gives.
fn plane(opcode: Opcode) -> Option<Opcode> {
    matches!(opcode, Opcode::MetaType | Opcode::MetaInit).then_some(opcode)
}

/// One plane write, taken apart into what a run is made of.
#[derive(Debug)]
struct Write {
    /// Which plane, as the opcode that writes it.
    kind: Opcode,
    /// What the write carries besides its operands, which for `meta_type` is the node.
    extra: Extra,
    /// Where the pointer came from once every constant step has been walked off it.
    base: Value,
    /// How far past that base the write starts.
    at: i128,
    /// How many bytes it covers.
    size: i128,
    /// The pointer as the instruction has it, which is the one to reuse if this write turns out to
    /// be the lowest of its run.
    pointer: Value,
    /// The type the width operand is written in.
    width: Type,
    /// The instruction itself.
    inst: Inst,
}

/// A run of writes to one plane that sit end to end.
#[derive(Debug)]
struct Run {
    /// Which plane, and what its writes carry, which every member of the run agrees on.
    kind: Opcode,
    extra: Extra,
    /// The base every member walked back to.
    base: Value,
    /// The low end of what the run covers, as a distance from that base.
    lo: i128,
    /// The high end, one past the last byte.
    hi: i128,
    /// The pointer operand of whichever member starts at `lo`.
    low: Value,
    /// The type to write the merged width in.
    width: Type,
    /// Every member, in the order they appear in the block.
    parts: Vec<Inst>,
}

impl Run {
    /// Whether a write is to the same plane, about the same base, and says the same thing.
    fn fits(&self, write: &Write) -> bool {
        self.kind == write.kind && self.base == write.base && same(self.extra, write.extra)
    }
}

/// A plane write taken apart, if its operands are ones this pass can read.
fn read(func: &Func, inst: Inst, kind: Opcode) -> Option<Write> {
    let [pointer, length] = func[func[inst].args] else { return None };
    let (imm, width) = crate::fold::constant(func, length)?;
    let size = imm.signed(width);
    // A width of zero would make two writes adjacent that are not next to anything, and a width
    // past the rule's bound is one the rule declines anyway.
    if !(1..=LIMIT).contains(&size) {
        return None;
    }
    let (base, at) = crate::discharge::normal(func, pointer);
    Some(Write { kind, extra: func[inst].extra, base, at, size, pointer, width, inst })
}

/// Puts a write into the run it continues, or starts a new one where it cannot.
fn extend(func: &mut Func, open: &mut Vec<Run>, write: Write, stats: &mut Stats, fuel: &mut Fuel) {
    if let Some(run) = open.iter_mut().find(|run| run.fits(&write)) {
        // Either end, because a structure written field by field runs up and one written
        // backwards runs down, and the rule is asked the same question about the pair either way.
        if write.at == run.hi && joins(run.hi - run.lo, write.size) {
            run.hi = write.at + write.size;
            run.parts.push(write.inst);
            return;
        }
        if write.at + write.size == run.lo && joins(write.size, run.hi - run.lo) {
            run.lo = write.at;
            run.low = write.pointer;
            run.parts.push(write.inst);
            return;
        }
        stats.missed(NOT_ADJACENT);
    }
    // Anything writing this plane that did not continue the run ends it. That is what keeps two
    // writes over bytes that overlap in the order the program put them in, since the second is
    // what those bytes are supposed to say afterwards.
    flush(func, open, |run| run.kind == write.kind, stats, fuel);
    open.push(Run {
        kind: write.kind,
        extra: write.extra,
        base: write.base,
        lo: write.at,
        hi: write.at + write.size,
        low: write.pointer,
        width: write.width,
        parts: vec![write.inst],
    });
}

/// Whether two plane writes carry the same payload.
///
/// For `meta_type` that is the node, and two writes naming different nodes are two different
/// claims about the bytes rather than one claim about more of them. For `meta_init` there is
/// nothing to carry and both sides are [`Extra::None`].
fn same(one: Extra, other: Extra) -> bool {
    match (one, other) {
        (Extra::Node(left), Extra::Node(right)) => left == right,
        (Extra::None, Extra::None) => true,
        _ => false,
    }
}

/// Closes every run the predicate picks out, merging the ones with anything to merge.
fn flush(
    func: &mut Func,
    open: &mut Vec<Run>,
    mut pick: impl FnMut(&Run) -> bool,
    stats: &mut Stats,
    fuel: &mut Fuel,
) {
    let mut kept = Vec::with_capacity(open.len());
    for run in std::mem::take(open) {
        if pick(&run) {
            merge(func, run, stats, fuel);
        } else {
            kept.push(run);
        }
    }
    *open = kept;
}

/// One write in front of the last member of the run, and every member gone.
fn merge(func: &mut Func, run: Run, stats: &mut Stats, fuel: &mut Fuel) {
    let Some(&last) = run.parts.last() else { return };
    if run.parts.len() < 2 {
        stats.note(ALONE);
        return;
    }
    if !fuel.take() {
        stats.missed(NO_FUEL);
        return;
    }
    // In front of the last member rather than of the first, which is the direction the module
    // comment is about. Its pointer is the lowest member's, which is defined earlier in this same
    // block and so is in hand here.
    let width = crate::ivopts::number(func, last, run.width, run.hi - run.lo);
    let args = func.push_values(&[run.low, width]);
    let data = InstData { args, extra: run.extra, ..InstData::new(run.kind) };
    let span = func.span(last);
    let made = func.create_inst(data, &[], span);
    func.insert_before(made, last);
    for part in run.parts {
        func.remove_inst(part);
    }
    stats.optimized(MERGED);
}

/// Whether one range next to another may be written as one, which is a question for the rules.
///
/// This function decides nothing. The pass worked out that two writes are about one base with the
/// second starting exactly where the first ends, and whether one write over both covers the same
/// addresses and no others is `joined.i64` in the rule file. The base is opaque because the answer
/// has to hold wherever it is, and so is the byte the question is about, because the claim is about
/// every address at once rather than about one the pass picked out.
fn joins(span: i128, reach: i128) -> bool {
    let mut question = crate::discharge::Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let width = question.number(span);
    let width = question.app("iconst.i64", &[width]);
    // The distance to the second write is the width of the first, because that is what next to
    // each other means. It goes in as its own operand rather than being left implicit, so that
    // adjacency is something the rule's guard checks rather than something this pass promises.
    let delta = question.number(span);
    let delta = question.app("iconst.i64", &[delta]);
    let reach = question.number(reach);
    let reach = question.app("iconst.i64", &[reach]);
    let byte = question.opaque();
    let byte = question.app("value.i64", &[byte]);
    let term = question.app("joined.i64", &[at, width, delta, reach, byte]);
    match safety::TABLE.find(&question, term) {
        Some(found) => crate::discharge::yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// Whether a run of writes to one plane may be moved past an instruction.
///
/// A default of no and a short list of yes. The list is what a plane write is: it writes one plane
/// over a range of bytes and reads nothing at all, so it may be moved past anything that does not
/// read that plane and does not change what its range of bytes means. Everything not named here,
/// which is every call, every barrier and every lifetime marker, stops the run where it is.
fn crossed(opcode: Opcode, kind: Opcode) -> bool {
    // The other plane's own writes, and the reads of it, which say nothing about this one.
    let other = if kind == Opcode::MetaType {
        matches!(opcode, Opcode::MetaInit | Opcode::MetaInitCopy | Opcode::CheckInit)
    } else {
        matches!(opcode, Opcode::MetaType | Opcode::MetaTypeCopy | Opcode::CheckType)
    };
    other
        // Anything that computes a value and touches nothing, which is most of what sits between
        // two fields of a structure being filled in: the address arithmetic and the constants.
        || !opcode.has_effects()
        || matches!(
            opcode,
            // Moving bytes about. A store is what caused the plane write in the first place, and
            // a plane is not a thing a store reads.
            Opcode::Load
                | Opcode::Store
                | Opcode::Memcpy
                | Opcode::Memmove
                | Opcode::Memset
                // The checks that read some plane other than this one. A check reads and does not
                // write, so crossing one is this pass declining to answer it any earlier than it
                // would have been answered without the merge.
                | Opcode::CheckBounds
                | Opcode::CheckLive
                | Opcode::CheckDeriv
                | Opcode::CheckRace
                | Opcode::CheckRestrictRead
                | Opcode::CheckRestrictWrite
                // The epoch plane, which neither of these two writes and which does not read them.
                | Opcode::MetaEpoch
        )
}

#[cfg(test)]
mod tests {
    use rucc_base::{Idx, Interner};
    use rucc_ir::{
        Block, Builder, Extra, Flags, Func, Inst, InstData, Opcode, Signature, Type, Value,
    };

    use super::{Coalesce, NO_FUEL, NOT_ADJACENT};
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// A function taking one pointer, with plane writes of one kind at the offsets and widths
    /// given, in the order given.
    fn fills(names: &mut Interner, kind: Opcode, at: &[(i64, i64)]) -> Func {
        let (mut func, entry, base) = start(names);
        let mut build = Builder::new(&mut func, entry);
        for &(offset, size) in at {
            let address = walk(&mut build, base, offset);
            plane(&mut build, kind, address, size, 1);
        }
        build.ret(&[]);
        func
    }

    /// An empty function taking one pointer, with its entry block and that pointer.
    fn start(names: &mut Interner) -> (Func, Block, Value) {
        let signature = Signature::new().with_params(&[Type::PTR]);
        let mut func = Func::new(names.intern("fill"), signature);
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

    /// Every write to that plane still in the function, as where it starts and how wide it is.
    fn writes(func: &Func, kind: Opcode) -> Vec<(i128, i128)> {
        every(func)
            .into_iter()
            .filter(|&inst| func[inst].opcode == kind)
            .map(|inst| {
                let [pointer, length] = func[func[inst].args] else { panic!("two operands") };
                let (_, at) = crate::discharge::normal(func, pointer);
                let (imm, ty) = crate::fold::constant(func, length).expect("a constant width");
                (at, imm.signed(ty))
            })
            .collect()
    }

    /// Every instruction in the function, in the order they run.
    fn every(func: &Func) -> Vec<Inst> {
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect()
    }

    fn run(func: &mut Func) -> Stats {
        with(func, &mut Fuel::unlimited())
    }

    fn with(func: &mut Func, fuel: &mut Fuel) -> Stats {
        Coalesce.run(func, &mut crate::machine::fixtures::analyses(), fuel)
    }

    #[test]
    fn three_fields_filled_in_order_are_one_write_over_the_whole_structure() {
        let mut names = Interner::new();
        let mut func = fills(&mut names, Opcode::MetaInit, &[(0, 8), (8, 8), (16, 8)]);
        let stats = run(&mut func);
        assert!(stats.changed());
        assert_eq!(writes(&func, Opcode::MetaInit), [(0, 24)]);
    }

    #[test]
    fn the_one_write_left_is_where_the_last_of_them_was_and_not_where_the_first_was() {
        // The direction the module comment is about. A plane told about a store before the store
        // has happened permits a read of storage nothing has written, so the merged write goes
        // after all the address arithmetic rather than in front of it.
        let mut names = Interner::new();
        let mut func = fills(&mut names, Opcode::MetaInit, &[(0, 8), (8, 8), (16, 8)]);
        run(&mut func);
        let order = every(&func);
        let merged = order
            .iter()
            .position(|&inst| func[inst].opcode == Opcode::MetaInit)
            .expect("the merged write");
        let last = order
            .iter()
            .rposition(|&inst| func[inst].opcode == Opcode::PtrAdd)
            .expect("the last address");
        assert!(merged > last, "the merged write rose above an address it is about");
    }

    #[test]
    fn a_structure_filled_in_backwards_merges_the_same_way() {
        let mut names = Interner::new();
        let mut func = fills(&mut names, Opcode::MetaInit, &[(16, 8), (8, 8), (0, 8)]);
        let stats = run(&mut func);
        assert!(stats.changed());
        assert_eq!(writes(&func, Opcode::MetaInit), [(0, 24)]);
    }

    #[test]
    fn a_gap_between_two_of_them_is_where_the_run_stops() {
        let mut names = Interner::new();
        let mut func = fills(&mut names, Opcode::MetaInit, &[(0, 8), (8, 8), (24, 8)]);
        let stats = run(&mut func);
        assert!(stats.changed());
        assert_eq!(writes(&func, Opcode::MetaInit), [(0, 16), (24, 8)]);
        assert_eq!(stats.count(Kind::Missed, NOT_ADJACENT), 1);
    }

    #[test]
    fn two_writes_over_bytes_that_overlap_stay_in_the_order_the_program_put_them_in() {
        let mut names = Interner::new();
        let mut func = fills(&mut names, Opcode::MetaInit, &[(0, 8), (4, 8)]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(writes(&func, Opcode::MetaInit), [(0, 8), (4, 8)]);
    }

    #[test]
    fn a_run_of_one_write_is_left_exactly_as_it_was() {
        let mut names = Interner::new();
        let mut func = fills(&mut names, Opcode::MetaInit, &[(0, 8)]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(writes(&func, Opcode::MetaInit), [(0, 8)]);
    }

    #[test]
    fn two_fields_of_one_type_merge_and_a_third_of_another_does_not_join_them() {
        // The node is what a `meta_type` write says about the bytes, so two writes naming
        // different nodes are two claims rather than one claim about more bytes.
        let mut names = Interner::new();
        let (mut func, entry, base) = start(&mut names);
        let mut build = Builder::new(&mut func, entry);
        for (offset, node) in [(0, 1), (8, 1), (16, 2)] {
            let address = walk(&mut build, base, offset);
            plane(&mut build, Opcode::MetaType, address, 8, node);
        }
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(stats.changed());
        assert_eq!(writes(&func, Opcode::MetaType), [(0, 16), (16, 8)]);
    }

    #[test]
    fn the_two_planes_do_not_stop_each_other_and_each_ends_up_with_one_write() {
        // The shape the safety pass actually emits, where the two plane writes for one store sit
        // next to each other and the pair repeats. Each run has to be able to step over the
        // other plane's writes or neither of them merges at all.
        let mut names = Interner::new();
        let (mut func, entry, base) = start(&mut names);
        let mut build = Builder::new(&mut func, entry);
        for offset in [0, 8, 16] {
            let address = walk(&mut build, base, offset);
            plane(&mut build, Opcode::MetaType, address, 8, 1);
            plane(&mut build, Opcode::MetaInit, address, 8, 1);
        }
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(stats.changed());
        assert_eq!(writes(&func, Opcode::MetaType), [(0, 24)]);
        assert_eq!(writes(&func, Opcode::MetaInit), [(0, 24)]);
    }

    #[test]
    fn a_call_in_the_middle_of_a_run_is_where_it_stops() {
        let mut names = Interner::new();
        let (mut func, entry, base) = start(&mut names);
        let mut build = Builder::new(&mut func, entry);
        for offset in [0, 8] {
            let address = walk(&mut build, base, offset);
            plane(&mut build, Opcode::MetaInit, address, 8, 1);
        }
        build.inst(InstData::new(Opcode::Call), &[]);
        for offset in [16, 24] {
            let address = walk(&mut build, base, offset);
            plane(&mut build, Opcode::MetaInit, address, 8, 1);
        }
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(stats.changed());
        assert_eq!(writes(&func, Opcode::MetaInit), [(0, 16), (16, 16)]);
    }

    #[test]
    fn a_read_of_the_same_plane_stops_the_run_and_a_read_of_another_one_does_not() {
        for (between, merged) in
            [(Opcode::CheckInit, vec![(0, 8), (8, 8)]), (Opcode::CheckBounds, vec![(0, 16)])]
        {
            let mut names = Interner::new();
            let (mut func, entry, base) = start(&mut names);
            let mut build = Builder::new(&mut func, entry);
            let first = walk(&mut build, base, 0);
            plane(&mut build, Opcode::MetaInit, first, 8, 1);
            build.inst(InstData::new(between), &[]);
            let second = walk(&mut build, base, 8);
            plane(&mut build, Opcode::MetaInit, second, 8, 1);
            build.ret(&[]);
            run(&mut func);
            assert_eq!(writes(&func, Opcode::MetaInit), merged, "across {}", between.name());
        }
    }

    #[test]
    fn the_epoch_plane_is_left_alone_however_its_writes_line_up() {
        let mut names = Interner::new();
        let mut func = fills(&mut names, Opcode::MetaEpoch, &[(0, 8), (8, 8), (16, 8)]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(writes(&func, Opcode::MetaEpoch), [(0, 8), (8, 8), (16, 8)]);
    }

    #[test]
    fn a_width_the_pass_cannot_read_stops_the_run_rather_than_being_guessed_at() {
        let mut names = Interner::new();
        let (mut func, entry, base) = start(&mut names);
        let mut build = Builder::new(&mut func, entry);
        let first = walk(&mut build, base, 0);
        plane(&mut build, Opcode::MetaInit, first, 8, 1);
        // A width off the pointer itself, which is nothing the pass is going to work out.
        let second = walk(&mut build, base, 8);
        let width = build.unary(Opcode::PtrToInt, base, Type::int(64));
        let args = build.func().push_values(&[second, width]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaInit) }, &[]);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
    }

    #[test]
    fn no_fuel_leaves_every_write_where_it_is() {
        let mut names = Interner::new();
        let mut func = fills(&mut names, Opcode::MetaInit, &[(0, 8), (8, 8), (16, 8)]);
        let stats = with(&mut func, &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(writes(&func, Opcode::MetaInit), [(0, 8), (8, 8), (16, 8)]);
        assert_eq!(stats.count(Kind::Missed, NO_FUEL), 1);
    }

    #[test]
    fn a_function_with_no_body_is_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let stats = run(&mut func);
        assert!(!stats.changed());
    }
}
