//! The clobber walk against the truth, on functions nobody chose.
//!
//! The walk in `memssa.rs` claims that a given store is the one a given load sees. That claim is
//! checkable without running anything, because on a function with no loops there are finitely
//! many paths from the entry to the load, and on each of them the store the load sees is just the
//! last one that wrote a byte of what the load reads. So this generates random functions, works
//! the answer out that way, and holds the walk to it.
//!
//! The generated functions are acyclic on purpose, which is what makes the path enumeration
//! finite. It is a real restriction and the unit tests in `memssa.rs` cover the loop cases, which
//! are the ones where the walk meets a memory phi whose argument is defined below it. What is
//! bought by giving that up is a check that has no model of its own to get wrong: the ground
//! truth here is a list of paths and a list of bytes, and there is nothing in it that could be
//! subtly the same mistake as the thing it is checking.
//!
//! Two properties are checked and they are not the same one.
//!
//! The first is that construction produces something the verifier believes, on every function
//! generated. The chain has ten rules in `rucc-ir`'s verifier and they are the specification of
//! what memory SSA is, so a construction that satisfies all of them on a few thousand random
//! control flow graphs is a construction that places its memory phis where they belong.
//!
//! The second is that the walk never lies. Every answer is checked against the paths: a clobber
//! has to be the last write on every path, `NoClobber` has to mean no path wrote anything, and
//! `Unknown` is allowed always because it is the answer that claims nothing. That asymmetry is
//! the point. The walk is allowed to be imprecise and is not allowed to be wrong, and section 9.6
//! of `spec/optimizer/09-memory-ssa.md` says which of those two is the bug.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_ir::{
    Block, Builder, Extra, Flags, Func, Inst, InstData, MemInfo, MemOrder, Module, Opcode,
    Restrict, Signature, Type, Value, verify_func,
};
use rucc_opt::{Clobber, Walk, memssa};
use rucc_target::{TargetInfo, Triple};

/// How many objects a generated function has, which is how often two accesses collide.
const OBJECTS: usize = 3;

/// How many bytes each of them is.
const SIZE: u64 = 8;

/// How many functions each property is checked on.
const RUNS: usize = 2000;

#[test]
fn construction_produces_a_function_the_verifier_believes() {
    let mut random = Random::new(0x5ce7_c0ff_ee00_0009);
    let mut built = 0;
    for _ in 0..RUNS {
        let plan = random.plan();
        let (mut module, names, id) = plan.build();
        if !memssa::build(&mut module[id]) {
            continue;
        }
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("{plan:#?} came out as a function the verifier turns down: {errors:#?}");
        }
        built += 1;
    }
    // Not every plan has memory in it, and the ones that do not are declined rather than given a
    // chain that starts and reaches nothing. Most of them do.
    assert!(built > RUNS / 2, "only {built} of {RUNS} plans had any memory in them");
}

#[test]
fn every_clobber_the_walk_names_is_the_one_every_path_agrees_on() {
    let mut random = Random::new(0x5ce7_c0ff_ee00_000a);
    let mut answered = 0;
    let mut exact = 0;
    for _ in 0..RUNS {
        let plan = random.plan();
        let (mut module, _, id) = plan.build();
        if !memssa::build(&mut module[id]) {
            continue;
        }
        let func = &module[id];
        let truth = Truth::of(func, &plan);
        let mut walk = Walk::new(func, &module);
        for &load in &truth.loads {
            let answer = walk.clobber(load);
            let want = truth.sees(func, load);
            match answer {
                // Allowed always. It is the answer that claims nothing, and a caller that acts
                // on it has read the documentation wrongly rather than been lied to.
                Clobber::Unknown => {}
                Clobber::NoClobber => {
                    assert_eq!(
                        want,
                        Sees::Nothing,
                        "the walk said nothing wrote it and {want:?} did, in {plan:#?}"
                    );
                    answered += 1;
                }
                Clobber::Exact(inst) | Clobber::Partial(inst) => {
                    assert_eq!(
                        want,
                        Sees::This(inst),
                        "the walk named the wrong write, in {plan:#?}"
                    );
                    answered += 1;
                    exact += usize::from(matches!(answer, Clobber::Exact(_)));
                }
                // Every access here is to a local at a known offset of a known width, so an
                // alias query that says the two may touch is always one where both ranges are
                // known and the answer is either exact or partial. `Maybe` on this generator
                // would mean the extent check gave up on something it had the numbers for.
                Clobber::Maybe(inst) => {
                    panic!(
                        "the walk gave up on {inst:?}, which it has the offsets for, in {plan:#?}"
                    )
                }
            }
        }
    }
    // A walk that answered `Unknown` to everything would pass the checks above and be useless,
    // so the run has to have got real answers out of it, and some of them exactly. Exact is the
    // rarer of the two by a long way here, because the generator picks the offset and the width
    // of each access independently and most pairs that overlap do not line up, so most of what
    // comes back is partial. That is the ratio to expect from real code as well.
    assert!(answered > RUNS, "only {answered} walks came back with an answer");
    assert!(exact > RUNS / 20, "only {exact} walks came back exact");
}

/// What the paths say a load sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sees {
    /// Every path to the load has this as its last write to any byte of the reference.
    This(Inst),
    /// No path to the load wrote any byte of it.
    Nothing,
    /// The paths disagree, so there is nothing for the walk to be held to.
    Disagree,
}

/// The paths through a generated function, and what is on them.
struct Truth {
    /// Every path from the entry to each block, as the blocks it goes through.
    paths: HashMap<Block, Vec<Vec<Block>>>,
    /// Every load in the function, in program order.
    loads: Vec<Inst>,
    /// Which object each instruction that touches memory touches, and which bytes of it.
    touches: HashMap<Inst, (usize, u64, u64)>,
    /// The instructions of each block, in order.
    insts: HashMap<Block, Vec<Inst>>,
}

impl Truth {
    fn of(func: &Func, plan: &Plan) -> Self {
        let blocks: Vec<Block> = func.blocks().collect();
        let insts: HashMap<Block, Vec<Inst>> =
            blocks.iter().map(|&block| (block, func.insts(block).collect())).collect();

        // Which object each access is to, recovered by matching the accesses up with the plan in
        // program order. The plan says what was asked for and this says where it ended up.
        let mut touches = HashMap::new();
        let mut loads = Vec::new();
        let mut next = plan.accesses().into_iter();
        for &block in &blocks {
            for &inst in &insts[&block] {
                let opcode = func[inst].opcode;
                if !matches!(opcode, Opcode::Load | Opcode::Store) {
                    continue;
                }
                let access = next.next().expect("the plan has an access for every one built");
                touches.insert(inst, (access.object, access.offset, access.width));
                if opcode == Opcode::Load {
                    loads.push(inst);
                }
            }
        }
        assert!(next.next().is_none(), "the plan had accesses the function does not");

        Self { paths: paths(func, &blocks), loads, touches, insts }
    }

    /// The last write to any byte of what this load reads, if every path agrees on one.
    fn sees(&self, func: &Func, load: Inst) -> Sees {
        let (object, from, width) = self.touches[&load];
        let want = (from, from + width);
        let block = func.block_of(load).expect("the load is in a block");
        let mut answer = None;
        for path in &self.paths[&block] {
            // Everything on the path before the load, in order, which is every block of the path
            // and then the part of the last block above the load itself.
            let mut before: Vec<Inst> = Vec::new();
            for &step in path {
                for &inst in &self.insts[&step] {
                    if inst == load {
                        break;
                    }
                    before.push(inst);
                }
            }
            let last = before.iter().rev().copied().find(|&inst| {
                if func[inst].opcode != Opcode::Store {
                    return false;
                }
                let (to, start, size) = self.touches[&inst];
                to == object && start < want.1 && want.0 < start + size
            });
            let seen = last.map_or(Sees::Nothing, Sees::This);
            if *answer.get_or_insert(seen) != seen {
                return Sees::Disagree;
            }
        }
        answer.unwrap_or(Sees::Nothing)
    }
}

/// Every path from the entry to every block, which is finite because the graph is acyclic.
fn paths(func: &Func, blocks: &[Block]) -> HashMap<Block, Vec<Vec<Block>>> {
    let mut paths: HashMap<Block, Vec<Vec<Block>>> = HashMap::new();
    let entry = func.entry().expect("the function has an entry block");
    paths.insert(entry, vec![vec![entry]]);
    // The generator numbers blocks so that every edge goes forwards, so one pass in that order
    // has every predecessor's paths in hand by the time a block is reached.
    for &block in blocks {
        let Some(here) = paths.get(&block).cloned() else {
            continue;
        };
        let Some(terminator) = func.terminator(block) else {
            continue;
        };
        for call in func.successors(terminator).collect::<Vec<_>>() {
            let at = paths.entry(call.block).or_default();
            for path in &here {
                let mut next = path.clone();
                next.push(call.block);
                at.push(next);
            }
        }
    }
    paths
}

/// One access a generated function makes.
#[derive(Clone, Copy, Debug)]
struct Access {
    object: usize,
    offset: u64,
    width: u64,
    /// Whether it writes. A read otherwise.
    writes: bool,
}

/// A function to generate, as the shape of it rather than the instructions.
#[derive(Debug)]
struct Plan {
    /// What each block branches to. Every target is a higher number, so the graph is acyclic.
    edges: Vec<Vec<usize>>,
    /// The accesses each block makes, in order.
    body: Vec<Vec<Access>>,
}

impl Plan {
    /// Every access the function makes, in the order the blocks are built in.
    fn accesses(&self) -> Vec<Access> {
        self.body.iter().flatten().copied().collect()
    }

    /// The module holding it, the names in it, and which function it is.
    fn build(&self) -> (Module, Interner, rucc_ir::FuncId) {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("gen.c"), &target);
        let mut func = Func::new(names.intern("f"), Signature::new());
        let blocks: Vec<Block> = self.edges.iter().map(|_| func.create_block()).collect();

        // The objects, which are locals in the entry block whose addresses go nowhere. Nothing
        // here takes a parameter, so every access is to one the alias analysis can name.
        let mut build = Builder::new(&mut func, blocks[0]);
        let objects: Vec<Value> = (0..OBJECTS).map(|_| local(&mut build, SIZE)).collect();

        for (index, targets) in self.edges.iter().enumerate() {
            let mut build = Builder::new(&mut func, blocks[index]);
            for access in &self.body[index] {
                let ty = Type::int(u32::try_from(access.width).expect("a small width") * 8);
                let info = MemInfo {
                    size: access.width,
                    align: 1,
                    order: MemOrder::NotAtomic,
                    tbaa: None,
                    restrict: Restrict::NONE,
                };
                let addr = at(&mut build, objects[access.object], access.offset);
                if access.writes {
                    let value = build.iconst(ty, 1);
                    build.store(value, addr, info, Flags::NONE);
                } else {
                    build.load(ty, addr, info, Flags::NONE);
                }
            }
            match targets.as_slice() {
                [] => {
                    build.ret(&[]);
                }
                [only] => {
                    build.jump(blocks[*only], &[]);
                }
                [taken, not_taken] => {
                    let cond = build.iconst(Type::int(1), 1);
                    build.br_if(cond, blocks[*taken], &[], blocks[*not_taken], &[]);
                }
                _ => unreachable!("the generator makes at most two edges out of a block"),
            }
        }

        let id = module.add_func(func);
        (module, names, id)
    }
}

/// An `alloca` of that many bytes.
fn local(build: &mut Builder<'_>, size: u64) -> Value {
    let info = MemInfo {
        size,
        align: 8,
        order: MemOrder::NotAtomic,
        tbaa: None,
        restrict: Restrict::NONE,
    };
    let mem = build.func().add_mem(info);
    build.value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR)
}

/// That address, moved on by a constant number of bytes.
fn at(build: &mut Builder<'_>, base: Value, offset: u64) -> Value {
    if offset == 0 {
        return base;
    }
    let by = build.iconst(Type::int(64), i128::from(offset));
    build.binary(Opcode::PtrAdd, base, by, Flags::NONE)
}

/// The generator, and the numbers behind it.
struct Random(u64);

impl Random {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// xorshift64star, which is four lines and good enough to generate test cases with.
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    /// A function whose blocks branch forwards, so the graph is acyclic and its paths are
    /// finite. Every block that is not the last one branches, and the last one returns, so
    /// nothing is generated that nothing reaches.
    fn plan(&mut self) -> Plan {
        let count = 2 + self.below(5);
        let mut edges: Vec<Vec<usize>> = Vec::with_capacity(count);
        for index in 0..count {
            let left = count - index - 1;
            let targets = match left {
                0 => Vec::new(),
                _ if self.below(3) == 0 => vec![index + 1],
                _ => {
                    let far = index + 1 + self.below(left);
                    vec![index + 1, far]
                }
            };
            edges.push(targets);
        }
        // Every block has to be reachable, so anything nothing branches to is given an edge from
        // the block before it, which the loop above already guarantees by always branching to
        // the next one. That leaves nothing to fix up and it is worth saying why.
        let body = (0..count).map(|_| self.accesses()).collect();
        Plan { edges, body }
    }

    /// A few accesses, at widths and offsets that overlap each other often enough to be worth
    /// generating.
    fn accesses(&mut self) -> Vec<Access> {
        let count = self.below(4);
        (0..count)
            .map(|_| {
                let width = 1 << self.below(3);
                let offset = (self.below(usize::try_from(SIZE / width).unwrap()) as u64) * width;
                Access { object: self.below(OBJECTS), offset, width, writes: self.below(2) == 0 }
            })
            .collect()
    }
}
