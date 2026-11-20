//! The ranges against the values, on functions nobody chose.
//!
//! A range is a claim about what a value can hold, and the way to check that claim is to run the
//! function and look. So this generates random functions over an eight bit parameter, runs every
//! one of them on all two hundred and fifty six inputs, and holds the analysis to what the runs
//! saw. Every value the run put in a register has to be inside the range the query gives for that
//! value at that block, every comparison the query settled has to come out the way it said on
//! every input, and every relation the oracle claims has to hold.
//!
//! The check is one directional and that is deliberate. A range that is wider than the truth is
//! imprecise and a range that is narrower is a miscompilation waiting to happen, so nothing here
//! requires an answer to be tight. What keeps the test from passing on an analysis that gives up
//! everywhere is the counting at the end: the run tallies how often a query at a block came back
//! narrower than the same query at the definition, and fails if that almost never happened.
//!
//! The generated functions are acyclic and have no block parameters. Both are real restrictions
//! and both are covered by the unit tests in `range/query.rs`, which is where the loop case and
//! the parameter case live. What is bought by giving them up is a ground truth with no model in
//! it: the answer here is a list of numbers that a run of the function produced, and there is
//! nothing in it that could be subtly the same mistake as the thing it is checking.
//!
//! Nothing generated relies on undefined behaviour. Arithmetic carries no flags, so every result
//! is the wrapping one, and shifts are only ever by a constant smaller than the width. That
//! matters because a range is allowed to exclude a value that only an undefined program could
//! produce, and a test that produced one would be asking the analysis to agree with a program
//! whose behaviour is not defined.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_ir::{Block, Builder, Extra, Flags, Func, IntPred, Opcode, Signature, Type, Value};
use rucc_opt::range::ops::Truth;
use rucc_opt::{Cfg, Dominators, Range, Ranges};

/// How many functions each property is checked on.
const RUNS: usize = 400;

/// The width of the parameter, and so the number of inputs every function is run on.
const WIDTH: u32 = 8;

/// How many values a block picks from when it is looking for an operand.
const POOL: usize = 12;

#[test]
fn a_value_is_always_inside_the_range_it_was_given() {
    let mut random = Random::new(0x5ce7_c0ff_ee00_0011);
    let mut checked = 0u64;
    let mut narrowed = 0u64;
    for _ in 0..RUNS {
        let plan = random.plan();
        let built = plan.build();
        let cfg = Cfg::new(&built.func);
        let dom = Dominators::new(&cfg);
        let mut ranges = Ranges::new(&built.func, &cfg, &dom);
        let traces = built.run();

        for (block, env) in &traces {
            for (&value, &held) in env {
                let home = built.home[&value];
                if !dom.dominates(home, *block) {
                    continue;
                }
                let at = ranges.at(value, *block);
                assert!(
                    at.contains(held),
                    "{held} was held at {block:?} and the range there was {at:?} in {plan:#?}",
                );
                let of = ranges.of(value);
                assert!(
                    of.contains(held),
                    "{held} was held and the range at its definition was {of:?} in {plan:#?}",
                );
                checked += 1;
                if narrower(at, of) {
                    narrowed += 1;
                }
            }
        }
    }
    assert!(checked > 20_000, "only {checked} values were looked at, which is too few to trust");
    assert!(
        narrowed > checked / 100,
        "only {narrowed} of {checked} queries were narrower at the use than at the definition, \
         so the walk back through the branches is doing nothing",
    );
}

#[test]
fn a_comparison_the_ranges_settled_comes_out_that_way_every_time() {
    const PREDS: [IntPred; 6] =
        [IntPred::Eq, IntPred::Ne, IntPred::Slt, IntPred::Sle, IntPred::Ult, IntPred::Ule];

    let mut random = Random::new(0xc0de_5eed_0000_0012);
    let mut settled = 0u64;
    for _ in 0..RUNS {
        let plan = random.plan();
        let built = plan.build();
        let cfg = Cfg::new(&built.func);
        let dom = Dominators::new(&cfg);
        let mut ranges = Ranges::new(&built.func, &cfg, &dom);
        let traces = built.run();

        for (block, pairs) in built.pairs(&traces, &dom) {
            for (left, right) in pairs {
                for pred in PREDS {
                    let answer = ranges.compare(pred, left, right, block);
                    if answer == Truth::Either {
                        continue;
                    }
                    settled += 1;
                    for (seen, env) in &traces {
                        if *seen != block {
                            continue;
                        }
                        let (a, b) = (env[&left], env[&right]);
                        let truth = holds(pred, a, b, built.width(left));
                        assert_eq!(
                            answer == Truth::Always,
                            truth,
                            "{pred:?} on {a} and {b} at {block:?} was called {answer:?} \
                             in {plan:#?}",
                        );
                    }
                }
            }
        }
    }
    assert!(settled > 200, "only {settled} comparisons came back settled, which is too few");
}

#[test]
fn a_relation_the_oracle_claims_is_a_relation_that_holds() {
    let mut random = Random::new(0x0fee_ddad_0000_0013);
    let mut claimed = 0u64;
    for _ in 0..RUNS {
        let plan = random.plan();
        let built = plan.build();
        let cfg = Cfg::new(&built.func);
        let dom = Dominators::new(&cfg);
        let mut ranges = Ranges::new(&built.func, &cfg, &dom);
        let traces = built.run();

        for (block, pairs) in built.pairs(&traces, &dom) {
            for (left, right) in pairs {
                let Some(pred) = ranges.relation(left, right, block) else { continue };
                claimed += 1;
                for (seen, env) in &traces {
                    if *seen != block {
                        continue;
                    }
                    let (a, b) = (env[&left], env[&right]);
                    assert!(
                        holds(pred, a, b, built.width(left)),
                        "the oracle said {pred:?} at {block:?} and it was {a} and {b} in {plan:#?}",
                    );
                }
            }
        }
    }
    assert!(claimed > 100, "the oracle claimed {claimed} relations, which is too few to trust");
}

/// The value of that width the plan asked for, if the pool has one.
fn take(pool: &[(Value, u32)], width: u32, index: usize) -> Option<Value> {
    let wide: Vec<Value> =
        pool.iter().filter(|(_, held)| *held == width).map(|(value, _)| *value).collect();
    if wide.is_empty() {
        return None;
    }
    Some(wide[index % wide.len()])
}

/// Whether one range is strictly inside another, which is what a walk back through a branch is
/// supposed to buy.
fn narrower(inner: Range, outer: Range) -> bool {
    inner != outer && inner.union(outer) == outer
}

/// Whether a comparison holds on two concrete values of that width.
fn holds(pred: IntPred, left: u128, right: u128, width: u32) -> bool {
    let signed = |value: u128| {
        let shift = 128 - width;
        ((value << shift) as i128) >> shift
    };
    match pred {
        IntPred::Eq => left == right,
        IntPred::Ne => left != right,
        IntPred::Slt => signed(left) < signed(right),
        IntPred::Sle => signed(left) <= signed(right),
        IntPred::Sgt => signed(left) > signed(right),
        IntPred::Sge => signed(left) >= signed(right),
        IntPred::Ult => left < right,
        IntPred::Ule => left <= right,
        IntPred::Ugt => left > right,
        IntPred::Uge => left >= right,
    }
}

/// The bits of a width, as a mask.
fn mask(width: u32) -> u128 {
    if width >= 128 { u128::MAX } else { (1u128 << width) - 1 }
}

/// One operation a generated block can hold.
#[derive(Clone, Copy, Debug)]
enum Step {
    Const(u128),
    Binary(Opcode),
    Shift(Opcode, u32),
    Widen(Opcode),
    Trunc,
}

/// What a generated block ends with.
#[derive(Clone, Debug)]
enum End {
    Return,
    Jump,
    Branch(IntPred),
    Switch(Vec<u128>),
}

/// A function before it is built, which is the thing printed when an assertion fails.
#[derive(Clone, Debug)]
struct Plan {
    /// The successors of each block, by index. Every edge goes forwards, so the graph is
    /// acyclic and every run of it is finite.
    edges: Vec<Vec<usize>>,
    /// The operations in each block, in order.
    body: Vec<Vec<Step>>,
    /// How each block finishes.
    ends: Vec<End>,
    /// Which operand each choice above takes, as an index into the pool the builder has at that
    /// point. Kept here rather than drawn while building so that the plan is the whole of the
    /// function and printing it is enough to see what failed.
    picks: Vec<usize>,
}

/// A built function, with the bookkeeping the checks need.
struct Built {
    func: Func,
    /// The one input, which is the only thing a generated function is a function of.
    param: Value,
    /// The block each value was defined in, which is what says whether asking for it at another
    /// block is a question that means anything.
    home: HashMap<Value, Block>,
    /// The operations, in the order they were emitted, so the interpreter does not have to work
    /// out the semantics from the IR twice.
    order: Vec<Block>,
}

impl Built {
    fn width(&self, value: Value) -> u32 {
        self.func[value].ty.bits()
    }

    /// Every block the function reaches, with the values it held there on one input.
    ///
    /// One entry per input per block entered, so a block inside a branch appears once for each
    /// input that took the branch. That is what makes the pair checks below able to say whether
    /// a relation held, since the two values are read out of the same run.
    fn run(&self) -> Vec<(Block, HashMap<Value, u128>)> {
        let mut traces = Vec::new();
        for input in 0..=mask(WIDTH) {
            let mut env: HashMap<Value, u128> = HashMap::new();
            env.insert(self.param, input);
            let mut block = self.order[0];
            loop {
                for inst in self.func.insts(block) {
                    let data = self.func[inst];
                    let args: Vec<Value> = self.func[data.args].to_vec();
                    let Some(result) = data.first_result else { continue };
                    let width = self.width(result);
                    let read = |index: usize| env[&args[index]];
                    let value = match data.opcode {
                        Opcode::IConst => {
                            let Extra::Imm(at) = data.extra else { unreachable!() };
                            self.func[at].unsigned()
                        }
                        Opcode::Add => read(0).wrapping_add(read(1)),
                        Opcode::Sub => read(0).wrapping_sub(read(1)),
                        Opcode::Mul => read(0).wrapping_mul(read(1)),
                        Opcode::And => read(0) & read(1),
                        Opcode::Or => read(0) | read(1),
                        Opcode::Xor => read(0) ^ read(1),
                        Opcode::Shl => read(0) << read(1),
                        Opcode::LShr => read(0) >> read(1),
                        Opcode::AShr => {
                            let shift = 128 - width;
                            let signed = ((read(0) << shift) as i128) >> shift;
                            (signed >> read(1)) as u128
                        }
                        Opcode::ZExt | Opcode::Trunc => read(0),
                        Opcode::SExt => {
                            let from = self.width(args[0]);
                            let shift = 128 - from;
                            (((read(0) << shift) as i128) >> shift) as u128
                        }
                        Opcode::ICmp => {
                            let Extra::IntPred(pred) = data.extra else { unreachable!() };
                            let from = self.width(args[0]);
                            u128::from(holds(pred, read(0), read(1), from))
                        }
                        other => unreachable!("{other:?} was not generated"),
                    };
                    env.insert(result, value & mask(width));
                }
                traces.push((block, env.clone()));
                let term = self.func.terminator(block).expect("a terminator");
                let data = self.func[term];
                let calls: Vec<_> = self.func.successors(term).collect();
                block = match data.opcode {
                    Opcode::Return => break,
                    Opcode::Jump => calls[0].block,
                    Opcode::BrIf => {
                        let cond = env[&self.func[data.args][0]];
                        if cond == 1 { calls[0].block } else { calls[1].block }
                    }
                    Opcode::Switch => {
                        let Extra::Switch(info) = data.extra else { unreachable!() };
                        let info = self.func[info];
                        let cases: Vec<_> = self.func[info.cases].to_vec();
                        let on = env[&self.func[data.args][0]];
                        let hit = cases.iter().position(|case| case.unsigned() == on);
                        match hit {
                            Some(index) => calls[index + 1].block,
                            None => calls[0].block,
                        }
                    }
                    other => unreachable!("{other:?} was not generated"),
                };
            }
        }
        traces
    }

    /// The pairs of same width values worth asking about, per block.
    ///
    /// Only values that dominate the block are there, since asking about one that does not is a
    /// question with no answer, and only values every run of that block held, since a pair check
    /// reads both out of the same run.
    fn pairs(
        &self,
        traces: &[(Block, HashMap<Value, u128>)],
        dom: &Dominators,
    ) -> Vec<(Block, Vec<(Value, Value)>)> {
        let mut out = Vec::new();
        for &block in &self.order {
            let Some((_, env)) = traces.iter().find(|(seen, _)| *seen == block) else { continue };
            let mut live: Vec<Value> = env
                .keys()
                .copied()
                .filter(|value| dom.dominates(self.home[value], block))
                .filter(|value| self.width(*value) > 1)
                .collect();
            live.sort_unstable();
            let mut pairs = Vec::new();
            for (index, &left) in live.iter().enumerate() {
                for &right in &live[index + 1..] {
                    if self.width(left) == self.width(right) {
                        pairs.push((left, right));
                    }
                }
            }
            out.push((block, pairs));
        }
        out
    }
}

impl Plan {
    /// The blocks that dominate each block, worked out on the plan rather than on the built
    /// function, because the builder needs it before there is a function to analyse.
    ///
    /// Every edge goes forwards, so one pass in index order is a fixed point.
    fn dominators(&self) -> Vec<Vec<bool>> {
        let count = self.edges.len();
        let mut preds = vec![Vec::new(); count];
        for (from, targets) in self.edges.iter().enumerate() {
            for &to in targets {
                preds[to].push(from);
            }
        }
        let mut doms = vec![vec![false; count]; count];
        doms[0][0] = true;
        for block in 1..count {
            let mut set = vec![true; count];
            for &pred in &preds[block] {
                for index in 0..count {
                    set[index] &= doms[pred][index];
                }
            }
            set[block] = true;
            doms[block] = set;
        }
        doms
    }

    fn build(&self) -> Built {
        let count = self.edges.len();
        let doms = self.dominators();
        let mut names = Interner::new();
        let byte = Type::int(WIDTH);
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[byte]));
        let blocks: Vec<Block> = (0..count).map(|_| func.create_block()).collect();
        let param = func.append_param(blocks[0], byte);

        let mut defined: Vec<Vec<(Value, u32)>> = vec![Vec::new(); count];
        defined[0].push((param, WIDTH));
        let mut pick = self.picks.iter().copied().cycle();

        for block in 0..count {
            // What this block can name is what the blocks that dominate it defined, plus what it
            // has defined so far. That is the domination rule SSA is built on and getting it
            // wrong here would produce a function nothing else in the compiler would accept.
            let mut pool: Vec<(Value, u32)> = (0..block)
                .filter(|&other| doms[block][other])
                .flat_map(|other| defined[other].iter().copied())
                .collect();
            pool.extend(defined[block].iter().copied());
            let mut build = Builder::new(&mut func, blocks[block]);

            for &step in &self.body[block] {
                let index = pick.next().expect("the picks cycle");
                let made = match step {
                    Step::Const(value) => Some((build.iconst(byte, value as i128), WIDTH)),
                    Step::Binary(opcode) => {
                        let left = take(&pool, WIDTH, index);
                        let right = take(&pool, WIDTH, index / 3 + 1);
                        match (left, right) {
                            (Some(left), Some(right)) => {
                                Some((build.binary(opcode, left, right, Flags::NONE), WIDTH))
                            }
                            _ => None,
                        }
                    }
                    Step::Shift(opcode, by) => match take(&pool, WIDTH, index) {
                        Some(left) => {
                            let amount = build.iconst(byte, i128::from(by));
                            Some((build.binary(opcode, left, amount, Flags::NONE), WIDTH))
                        }
                        None => None,
                    },
                    Step::Widen(opcode) => take(&pool, WIDTH, index)
                        .map(|arg| (build.unary(opcode, arg, Type::int(WIDTH * 2)), WIDTH * 2)),
                    Step::Trunc => take(&pool, WIDTH * 2, index)
                        .map(|arg| (build.unary(Opcode::Trunc, arg, byte), WIDTH)),
                };
                if let Some((made, width)) = made {
                    pool.push((made, width));
                    defined[block].push((made, width));
                }
            }

            let targets = &self.edges[block];
            match &self.ends[block] {
                End::Return => {
                    build.ret(&[]);
                }
                End::Jump => {
                    build.jump(blocks[targets[0]], &[]);
                }
                End::Branch(pred) => {
                    let index = pick.next().expect("the picks cycle");
                    let left = take(&pool, WIDTH, index).expect("the parameter is always there");
                    let right = take(&pool, WIDTH, index / 2 + 1).expect("and so is it here");
                    let cond = build.icmp(*pred, left, right);
                    defined[block].push((cond, 1));
                    build.br_if(cond, blocks[targets[0]], &[], blocks[targets[1]], &[]);
                }
                End::Switch(cases) => {
                    let index = pick.next().expect("the picks cycle");
                    let on = take(&pool, WIDTH, index).expect("the parameter is always there");
                    let arms: Vec<(i128, Block)> =
                        cases.iter().map(|&case| (case as i128, blocks[targets[1]])).collect();
                    build.switch(on, blocks[targets[0]], &arms);
                }
            }
        }

        // Every value the function holds needs a home, including the constants the shifts and
        // the switches made along the way, so it is read back off the finished function rather
        // than collected while building and one case forgotten.
        let mut home = HashMap::new();
        home.insert(param, blocks[0]);
        for &block in &blocks {
            for inst in func.insts(block) {
                if let Some(result) = func[inst].first_result {
                    home.insert(result, block);
                }
            }
        }
        Built { func, param, home, order: blocks }
    }
}

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

    /// A function whose blocks branch forwards, so the graph is acyclic and every run of it
    /// ends.
    fn plan(&mut self) -> Plan {
        let count = 2 + self.below(5);
        let mut edges = Vec::with_capacity(count);
        let mut ends = Vec::with_capacity(count);
        for index in 0..count {
            let left = count - index - 1;
            if left == 0 {
                edges.push(Vec::new());
                ends.push(End::Return);
                continue;
            }
            let far = index + 1 + self.below(left);
            edges.push(vec![index + 1, far]);
            ends.push(match self.below(4) {
                0 => End::Jump,
                1 => End::Switch((0..1 + self.below(3)).map(|_| self.below(6) as u128).collect()),
                _ => End::Branch(self.pred()),
            });
        }
        // A switch whose cases repeat is a switch with two edges to the same block, which says
        // nothing on either of them, so the duplicates are taken out here rather than left for
        // the analysis to decline.
        for end in &mut ends {
            if let End::Switch(cases) = end {
                cases.sort_unstable();
                cases.dedup();
            }
        }
        let body = (0..count).map(|_| self.steps()).collect();
        let picks = (0..16).map(|_| self.below(7)).collect();
        Plan { edges, body, ends, picks }
    }

    fn steps(&mut self) -> Vec<Step> {
        (0..self.below(4)).map(|_| self.step()).collect()
    }

    fn step(&mut self) -> Step {
        match self.below(10) {
            0 => Step::Const(self.below(1 << WIDTH) as u128),
            1 => Step::Binary(Opcode::Add),
            2 => Step::Binary(Opcode::Sub),
            3 => Step::Binary(Opcode::Mul),
            4 => Step::Binary(Opcode::And),
            5 => Step::Binary(Opcode::Or),
            6 => Step::Binary(Opcode::Xor),
            7 => {
                let by = self.below(WIDTH as usize) as u32;
                Step::Shift([Opcode::Shl, Opcode::LShr, Opcode::AShr][self.below(3)], by)
            }
            8 => Step::Widen([Opcode::ZExt, Opcode::SExt][self.below(2)]),
            _ => Step::Trunc,
        }
    }

    fn pred(&mut self) -> IntPred {
        const PREDS: [IntPred; 10] = [
            IntPred::Eq,
            IntPred::Ne,
            IntPred::Slt,
            IntPred::Sle,
            IntPred::Sgt,
            IntPred::Sge,
            IntPred::Ult,
            IntPred::Ule,
            IntPred::Ugt,
            IntPred::Uge,
        ];
        PREDS[self.below(PREDS.len())]
    }
}

#[test]
fn the_generator_makes_functions_worth_checking() {
    let mut random = Random::new(0xfeed_face_0000_0014);
    let mut blocks = 0;
    let mut steps = 0;
    for _ in 0..RUNS {
        let plan = random.plan();
        blocks += plan.edges.len();
        steps += plan.body.iter().map(Vec::len).sum::<usize>();
        assert!(plan.picks.len() <= POOL + 4, "the picks are meant to be a short cycle");
    }
    assert!(blocks > RUNS * 2, "only {blocks} blocks over {RUNS} functions");
    assert!(steps > RUNS, "only {steps} operations over {RUNS} functions");
}
