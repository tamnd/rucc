//! Scalar evolution against loops that were actually run, on parameters nobody chose.
//!
//! The analysis in `scev.rs` says what a counter will be on every iteration and how many
//! iterations there are. Both claims are checkable by running the loop, so that is what this
//! does: it builds a counted loop out of random parameters, simulates it with the wrapping
//! arithmetic of the type it counts in, and holds the analysis to what the simulation saw.
//!
//! The interesting half is the assumptions. Section 7.5 of `spec/optimizer/07-loops-and-scev.md`
//! says a trip count is only right under a predicate, and the way to check that claim is to run
//! loops where the predicate is false and confirm the count is not asserted there. So every run
//! works out whether the assumptions held, and the count is only required to be the truth when
//! they did. A count that came back with no assumptions at all is required to be the truth
//! always, which is what makes [`Bound::proven`](rucc_opt::Bound::proven) worth having.

use rucc_base::Interner;
use rucc_ir::{Builder, Flags, Func, IntPred, Opcode, Signature, Type, Value};
use rucc_opt::{Assumption, Cfg, Count, Dominators, Evolution, Invariant, Loops, Scev};

/// How long a simulated loop is allowed to run before it is called uncounted.
///
/// A loop that steps away from its limit runs until the counter wraps, which at sixty four bits
/// is longer than anyone will wait. The generator produces those on purpose, because the analysis
/// has to decline them rather than answer, and the simulation only has to get far enough to say
/// the run did not stop.
const CAP: u128 = 4096;

#[test]
fn a_chrec_predicts_every_value_a_counter_actually_took() {
    let mut random = Random::new(0x5ce7_c0ff_ee00_0001);
    let mut checked = 0;
    for _ in 0..2000 {
        let it = random.loop_case();
        let run = it.run();
        let (func, counter) = it.build();
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        let id = loops.roots()[0];
        let mut scev = Scev::new(&func, &cfg, &loops);

        match scev.evolution(id, counter) {
            Evolution::Affine(chrec) => {
                assert_ne!(it.step, 0, "a step of zero came back as a chrec in {it:?}");
                assert_eq!(chrec.ty, it.ty, "in {it:?}");
                // Held as the signed reading, whatever the test that ends the loop makes of it.
                assert_eq!(chrec.base, Invariant::number(it.as_signed(it.from)), "in {it:?}");
                assert_eq!(chrec.step, Invariant::number(it.step), "in {it:?}");
                // The sequence the chrec describes, worked out by hand at the width it counts
                // in, is the sequence the loop went through.
                for (iteration, &seen) in run.values.iter().enumerate() {
                    let predicted = it.narrow(it.from.wrapping_add(it.step * iteration as i128));
                    assert_eq!(predicted, seen, "iteration {iteration} of {it:?}");
                }
                checked += 1;
            }
            // A counter that does not move is invariant rather than a chrec with a step of
            // nothing, which is what keeps a trip count from dividing by zero.
            Evolution::Invariant(_) => assert_eq!(it.step, 0, "in {it:?}"),
            Evolution::Unknown => panic!("a plain counter came back unknown in {it:?}"),
        }
    }
    assert!(checked > 1000, "only {checked} of two thousand loops had a counter that moved");
}

#[test]
fn a_trip_count_is_the_truth_whenever_the_things_it_rests_on_are() {
    let mut random = Random::new(0xc0de_5eed_0000_0002);
    let mut counted = 0;
    let mut proven = 0;
    for _ in 0..2000 {
        let it = random.loop_case();
        let run = it.run();
        let (func, _) = it.build();
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        let id = loops.roots()[0];
        let mut scev = Scev::new(&func, &cfg, &loops);

        let Some(bound) = scev.bound(id) else { continue };
        let (count, assumptions) = bound.parts();
        let Count::Exact(count) = count else {
            panic!("a loop with constant limits gave a symbolic count in {it:?}");
        };
        counted += 1;

        // A count with nothing left to prove is a promise with no escape clause in it, so the
        // run has to have ended and ended there.
        if bound.proven().is_some() {
            proven += 1;
            assert!(
                run.finished,
                "a proven count was given for a loop that did not stop in {it:?}"
            );
            assert_eq!(count, run.iterations, "proven count in {it:?}");
            continue;
        }

        if !assumptions.iter().all(|assumption| it.holds(assumption, &run)) {
            continue;
        }
        assert!(run.finished, "a count was given for a loop that did not stop in {it:?}");
        assert_eq!(count, run.iterations, "count in {it:?} under {assumptions:?}");
    }
    assert!(counted > 500, "only {counted} of two thousand loops got a count at all");
    assert!(proven > 20, "only {proven} counts came back with nothing left to prove");
}

#[test]
fn an_estimate_answers_even_where_a_bound_does_not() {
    let mut random = Random::new(0x0e57_5eed_0000_0003);
    let mut guessed = 0;
    for _ in 0..500 {
        let it = random.loop_case();
        let (func, _) = it.build();
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        let id = loops.roots()[0];
        let mut scev = Scev::new(&func, &cfg, &loops);

        let estimate = scev.estimate(id);
        // An estimate is for deciding whether a transformation pays for itself, and a caller
        // asking that question always gets a number back.
        if estimate.is_guess() {
            guessed += 1;
        } else {
            let bound = scev.bound(id).expect("an estimate that is not a guess came from a bound");
            assert_eq!(bound.parts().0, Count::Exact(u128::from(estimate.iterations())));
        }
    }
    assert!(guessed > 10, "only {guessed} of five hundred loops needed the default");
}

/// One counted loop, described by the numbers that make it.
///
/// ```text
/// entry:     jump header(from)
/// header(i): test = icmp pred i, to ; br_if test, body, exit
/// body:      next = add i, step ; jump header(next)
/// exit:      ret
/// ```
#[derive(Clone, Copy, Debug)]
struct Case {
    ty: Type,
    bits: u32,
    from: i128,
    to: i128,
    step: i128,
    pred: IntPred,
    /// What the increment promises, which is worked out from the run rather than guessed, so
    /// the IR handed to the analysis is never lying about overflow.
    flags: Flags,
}

/// What a loop did when it was run.
struct Run {
    /// The counter at the start of each iteration, in order.
    values: Vec<i128>,
    iterations: u128,
    /// Whether the loop stopped rather than being cut off at [`CAP`].
    finished: bool,
    /// Whether the counter ever went past the end of its type, read as signed.
    wrapped_signed: bool,
    /// The same read as unsigned.
    wrapped_unsigned: bool,
}

impl Case {
    /// The value read back at this width the way the test reads it, which is where a counter in
    /// `unsigned char` stops counting the way an `int` would.
    fn narrow(self, value: i128) -> i128 {
        let shift = 128 - self.bits;
        if self.signed() {
            self.as_signed(value)
        } else {
            ((value as u128) << shift >> shift) as i128
        }
    }

    /// The same bits with the sign taken seriously, which is how a constant is held before
    /// anybody knows what is going to read it.
    fn as_signed(self, value: i128) -> i128 {
        let shift = 128 - self.bits;
        (value << shift) >> shift
    }

    /// Whether the test reads its operands as signed.
    fn signed(self) -> bool {
        matches!(self.pred, IntPred::Slt | IntPred::Sle | IntPred::Sgt | IntPred::Sge)
    }

    /// Whether the loop keeps going with the counter here.
    fn keeps_going(self, counter: i128) -> bool {
        let (a, b) = (counter, self.narrow(self.to));
        match self.pred {
            IntPred::Eq => a == b,
            IntPred::Ne => a != b,
            IntPred::Slt | IntPred::Ult => a < b,
            IntPred::Sle | IntPred::Ule => a <= b,
            IntPred::Sgt | IntPred::Ugt => a > b,
            IntPred::Sge | IntPred::Uge => a >= b,
        }
    }

    /// The loop, run.
    fn run(self) -> Run {
        let mut values = Vec::new();
        let mut counter = self.narrow(self.from);
        let (mut wrapped_signed, mut wrapped_unsigned) = (false, false);
        let mut iterations = 0;
        let finished = loop {
            if !self.keeps_going(counter) {
                break true;
            }
            if iterations >= CAP {
                break false;
            }
            values.push(counter);
            iterations += 1;
            // The two readings wrap at different places, and a step that is fine in one is not
            // in the other, which is why the analysis records which promise it needs.
            let plain = counter + self.step;
            wrapped_signed |= signed_wraps(counter, self.step, self.bits);
            wrapped_unsigned |= unsigned_wraps(counter, self.step, self.bits);
            counter = self.narrow(plain);
        };
        Run { values, iterations, finished, wrapped_signed, wrapped_unsigned }
    }

    /// Whether what the analysis said it needed was true of the run.
    fn holds(self, assumption: &Assumption, run: &Run) -> bool {
        match assumption {
            Assumption::Approaching => self.narrow(self.to) >= self.narrow(self.from),
            // Asked for under the reading the test takes, which for `!=` is the unsigned one
            // because `!=` compares bit patterns and has no opinion about signs.
            Assumption::NoWrap(_) => {
                !if self.signed() { run.wrapped_signed } else { run.wrapped_unsigned }
            }
            // Signed overflow being undefined is a fact about the language rather than about the
            // run, and the run it is being used on is only a legal one if it does not overflow.
            Assumption::StrictOverflow => !run.wrapped_signed,
        }
    }

    /// The function this case describes, along with the counter to ask about.
    fn build(self) -> (Func, Value) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();
        let counter = func.append_param(header, self.ty);

        let mut build = Builder::new(&mut func, entry);
        let start = build.iconst(self.ty, self.from);
        build.jump(header, &[start]);

        let mut build = Builder::new(&mut func, header);
        let limit = build.iconst(self.ty, self.to);
        let test = build.icmp(self.pred, counter, limit);
        build.br_if(test, body, &[], exit, &[]);

        let mut build = Builder::new(&mut func, body);
        let by = build.iconst(self.ty, self.step);
        let next = build.binary(Opcode::Add, counter, by, self.flags);
        build.jump(header, &[next]);

        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);
        (func, counter)
    }
}

/// Whether adding this to that goes past the end of the signed range at this width.
fn signed_wraps(counter: i128, step: i128, bits: u32) -> bool {
    let (low, high) = (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1);
    let sum = counter + step;
    sum < low || sum > high
}

/// The same for the unsigned range, where the counter is read as a bit pattern.
fn unsigned_wraps(counter: i128, step: i128, bits: u32) -> bool {
    let mask = (1i128 << bits) - 1;
    let sum = (counter & mask) + step;
    sum < 0 || sum > mask
}

/// The generator, which is here rather than in a dependency because this crate has none.
struct Random(u64);

impl Random {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn bits(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.bits() % bound
    }

    /// A number in the range, ends included.
    fn between(&mut self, low: i128, high: i128) -> i128 {
        low + i128::from(self.below((high - low + 1) as u64))
    }

    /// One counted loop.
    ///
    /// Narrow types are as likely as wide ones, because section 7.7 says a suite written by
    /// somebody thinking in `int` is a suite that passes while the analysis is wrong. Steps go
    /// both ways and are sometimes nothing, limits are sometimes behind the start, and the
    /// predicate is picked without reference to any of it, so the run contains loops that never
    /// start, loops that never stop and loops that step past their limit.
    fn loop_case(&mut self) -> Case {
        let bits = [8u32, 16, 32, 64][self.below(4) as usize];
        // A byte gets the whole of its range, which puts starts and limits on both sides of the
        // sign bit. That is where the two readings of the same constant disagree, and a run that
        // wraps only turns up at all when the counter has somewhere near to wrap to.
        let span: i128 = if bits == 8 { 250 } else { 64 };
        let pred = [
            IntPred::Slt,
            IntPred::Sle,
            IntPred::Sgt,
            IntPred::Sge,
            IntPred::Ult,
            IntPred::Ule,
            IntPred::Ugt,
            IntPred::Uge,
            IntPred::Ne,
        ][self.below(9) as usize];
        let mut case = Case {
            ty: Type::int(bits),
            bits,
            from: self.between(0, span),
            to: self.between(0, span),
            step: self.between(-3, 3),
            pred,
            flags: Flags::NONE,
        };
        // The promise is written down only where the run bears it out. An `nsw` on an increment
        // that does overflow is undefined behaviour, and handing the analysis one would be
        // testing it against a program that has no meaning.
        let run = case.run();
        if !run.wrapped_signed {
            case.flags = case.flags.union(Flags::NSW);
        }
        if !run.wrapped_unsigned {
            case.flags = case.flags.union(Flags::NUW);
        }
        case
    }
}
