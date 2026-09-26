//! A division by a constant, as a multiply by its reciprocal and a shift.
//!
//! Design: `spec/optimizer/19-reassociation-and-arithmetic.md` section 19.5, which hands this to
//! the back end.
//!
//! A `div` takes twenty to forty cycles and a multiply takes three, so every compiler turns a
//! division by a constant into a multiply by a number close to its reciprocal, scaled up by a power
//! of two, and a shift that takes the scale back out. The number is what Granlund and Montgomery
//! call the magic number, and [`multiplier`] works it out the way `choose_multiplier` at
//! `gcc/expmed.cc:3728` does.
//!
//! # Why the product is sixty four bits wide
//!
//! gcc writes the multiply as a high multiply at the width of the division, which is an instruction
//! the IR does not have. What it has is an ordinary multiply at sixty four bits, and a dividend of
//! at most thirty two bits times a magic number of at most thirty three fits in one. So the
//! dividend is widened, multiplied, and shifted by the width and the post shift at once, and the
//! answer is the low half of that. It is also one step shorter than gcc's when the magic number
//! needs thirty three bits: gcc cannot hold the sum of the dividend and the high half in thirty two
//! bits and halves the difference first, and here the sum fits.
//!
//! A division at sixty four bits needs the high half of a hundred and twenty eight bit product, so
//! it stays a `div` until the IR has a high multiply (#309). A power of two and an exact division
//! need no product and are done at every width.
//!
//! # What the dividend is known to hold
//!
//! C divides at `int` at least, so an `unsigned short` divided by ten is an `int` division of a
//! value widened from sixteen bits. The widening says the value fits in sixteen bits, a magic
//! number for sixteen bits is smaller and never needs the extra add, and a signed division of a
//! value that cannot be negative is the unsigned one. [`Range`] is that, read off the instruction
//! the dividend comes from.
//!
//! # What is left alone
//!
//! Everything when the goal is size, where the `div` is shorter, which is what gcc does at `-Os`.
//! A divisor of zero, one or minus one, which is undefined or which the optimizer has already
//! answered. A divisor larger than anything the dividend can hold, where the quotient is zero or
//! one and a compare is the right code, which is rare enough to wait.
//!
//! # How it is checked
//!
//! [`program`] writes the rewrite as a short list of [`Step`]s before anything goes into the
//! function, and the tests run that same list on numbers. At eight bits that is every dividend
//! against every divisor. At sixteen it is every divisor, over the dividends either side of each of
//! its multiples, which is a proof rather than a sample: the rewrite and the division it replaces
//! only ever move at those points, so agreeing there is agreeing everywhere. `cargo xtask divide`
//! runs what the compiler makes of the same divisions against gcc.

use rucc_cost::Goal;
use rucc_ir::{Def, Extra, Flags, Func, Imm, Inst, Opcode, Type, Value};

use crate::expand::{ahead, ahead_const, becomes};

/// Rewrites every division and remainder by a constant that [`program`] has an answer for.
pub fn divisions(func: &mut Func, goal: Goal) {
    if matches!(goal, Goal::Size) {
        return;
    }
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        let Some(division) = division(func, inst) else { continue };
        let Some(program) = program(division) else { continue };
        write(func, inst, &program);
    }
}

/// What the dividend is known to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// Something from zero up to, but not including, two to the power of this.
    Unsigned(u32),
    /// Something a signed integer of this many bits holds.
    Signed(u32),
}

/// One division or remainder by a constant, as far as the rewrite needs to know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Division {
    /// Whether it reads its operands as signed.
    pub signed: bool,
    /// Whether the answer is the remainder rather than the quotient.
    pub remainder: bool,
    /// Whether the program promised the division leaves nothing over.
    pub exact: bool,
    /// The width it is done at.
    pub width: u32,
    /// What the dividend holds.
    pub range: Range,
    /// The divisor, read the way the division reads it.
    pub divisor: i128,
}

/// One instruction of a rewrite.
///
/// Value zero is the dividend, and value `i + 1` is what step `i` gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A constant of that many bits.
    Const(i128, u32),
    /// An operation over one value or two, giving that many bits. A conversion reads only the
    /// first.
    Op(Opcode, [usize; 2], u32),
}

/// A rewrite, in the order it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// The width of the dividend and of the answer.
    pub width: u32,
    /// The steps, the last of which is the answer.
    pub steps: Vec<Step>,
}

/// The dividend, which every program starts from.
const DIVIDEND: usize = 0;

impl Program {
    fn new(width: u32) -> Self {
        Self { width, steps: Vec::new() }
    }

    fn push(&mut self, step: Step) -> usize {
        self.steps.push(step);
        self.steps.len()
    }

    fn constant(&mut self, value: i128, bits: u32) -> usize {
        self.push(Step::Const(value, bits))
    }

    /// An operation over two values of `bits` bits, with the second a constant.
    fn by(&mut self, opcode: Opcode, value: usize, constant: i128, bits: u32) -> usize {
        let constant = self.constant(constant, bits);
        self.push(Step::Op(opcode, [value, constant], bits))
    }

    fn op(&mut self, opcode: Opcode, lhs: usize, rhs: usize, bits: u32) -> usize {
        self.push(Step::Op(opcode, [lhs, rhs], bits))
    }

    fn convert(&mut self, opcode: Opcode, value: usize, bits: u32) -> usize {
        self.push(Step::Op(opcode, [value, value], bits))
    }

    /// The dividend at sixty four bits, where the product is taken.
    fn widened(&mut self, extend: Opcode) -> usize {
        if self.width == 64 { DIVIDEND } else { self.convert(extend, DIVIDEND, 64) }
    }

    /// A value of sixty four bits back at the width of the division.
    fn narrowed(&mut self, value: usize) -> usize {
        if self.width == 64 { value } else { self.convert(Opcode::Trunc, value, self.width) }
    }

    /// Zero minus the value.
    fn negated(&mut self, value: usize) -> usize {
        let zero = self.constant(0, self.width);
        self.op(Opcode::Sub, zero, value, self.width)
    }

    /// The dividend less the quotient times the divisor, which is the remainder.
    fn left_over(&mut self, quotient: usize, divisor: i128) -> usize {
        let back = self.by(Opcode::Mul, quotient, divisor, self.width);
        self.op(Opcode::Sub, DIVIDEND, back, self.width)
    }
}

/// The rewrite for one division, or `None` where the `div` is left.
#[must_use]
pub fn program(division: Division) -> Option<Program> {
    let Division { signed, remainder, exact, width, range, divisor } = division;
    if !matches!(width, 8 | 16 | 32 | 64) || matches!(divisor, 0 | 1) || (signed && divisor == -1) {
        return None;
    }
    let mut program = Program::new(width);
    if exact && !remainder {
        exactly(&mut program, signed, divisor);
        return Some(program);
    }
    let size = divisor.unsigned_abs();
    match range {
        Range::Unsigned(bits) => {
            let quotient = unsigned(&mut program, size, bits)?;
            if remainder {
                if size.is_power_of_two() {
                    let mask = i128::try_from(size - 1).ok()?;
                    program.by(Opcode::And, DIVIDEND, mask, width);
                } else {
                    program.left_over(quotient, i128::try_from(size).ok()?);
                }
            } else if divisor < 0 {
                program.negated(quotient);
            }
        }
        Range::Signed(_) if size.is_power_of_two() => biased(&mut program, divisor, remainder),
        Range::Signed(bits) => {
            let quotient = rounded(&mut program, divisor, bits)?;
            if remainder {
                program.left_over(quotient, divisor);
            }
        }
    }
    Some(program)
}

/// The magic number for dividing by `divisor` a value of `bits` bits whose top `bits - precision`
/// bits are known to be clear, and the shift that goes after the multiply.
///
/// `choose_multiplier` at `gcc/expmed.cc:3728`. The quotient is the product shifted right by `bits`
/// and then by the shift. The number is the smallest that is still exact over the whole range,
/// which is found by starting from the largest shift that can work and halving both bounds for as
/// long as they stay apart.
///
/// # Panics
///
/// Panics if the divisor is zero or the working does not fit, which is a width over sixty four.
#[must_use]
pub fn multiplier(divisor: u128, bits: u32, precision: u32) -> (u128, u32) {
    assert!(divisor > 0, "a divisor of zero has no reciprocal");
    let up = 128 - (divisor - 1).leading_zeros();
    assert!(bits + up < 128, "a divisor and a width this can work with");
    let mut shift = up;
    let low = (1u128 << (bits + up)) / divisor;
    let high = ((1u128 << (bits + up)) + (1u128 << (bits + up - precision))) / divisor;
    let (mut low, mut high) = (low, high);
    while shift > 0 && low / 2 < high / 2 {
        low /= 2;
        high /= 2;
        shift -= 1;
    }
    (high, shift)
}

/// The quotient of a dividend that is never negative and fits in `bits` bits, by a positive
/// divisor.
///
/// When the magic number needs one bit more than the dividend has and the dividend has thirty two,
/// the product would need sixty five, so the number goes in without its top bit and the dividend is
/// added back to the high half, which is the same product shifted down by thirty two. The sum has
/// thirty three bits and fits.
fn unsigned(program: &mut Program, divisor: u128, bits: u32) -> Option<usize> {
    let width = program.width;
    if bits >= 128 || divisor >= 1u128 << bits {
        return None;
    }
    if divisor.is_power_of_two() {
        let shift = i128::from(divisor.trailing_zeros());
        return Some(program.by(Opcode::LShr, DIVIDEND, shift, width));
    }
    if bits > 32 {
        return None;
    }
    let (magic, shift) = multiplier(divisor, bits, bits);
    let wide = program.widened(Opcode::ZExt);
    let shifted = if magic < 1u128 << bits || bits < 32 {
        let product = program.by(Opcode::Mul, wide, i128::try_from(magic).ok()?, 64);
        program.by(Opcode::LShr, product, i128::from(bits + shift), 64)
    } else {
        let magic = i128::try_from(magic - (1u128 << 32)).ok()?;
        let product = program.by(Opcode::Mul, wide, magic, 64);
        let high = program.by(Opcode::LShr, product, 32, 64);
        let sum = program.op(Opcode::Add, high, wide, 64);
        if shift == 0 { sum } else { program.by(Opcode::LShr, sum, i128::from(shift), 64) }
    };
    Some(program.narrowed(shifted))
}

/// The quotient of a signed dividend of `bits` bits by a divisor that is not a power of two,
/// rounded towards zero.
///
/// The product shifted down is the quotient rounded towards minus infinity, which for a negative
/// dividend is one less than C's answer, so the dividend's sign, which is minus one or zero, is
/// taken off it. For a negative divisor it is the other way round, and the same subtraction the
/// other way round negates the quotient for nothing.
fn rounded(program: &mut Program, divisor: i128, bits: u32) -> Option<usize> {
    let width = program.width;
    let size = divisor.unsigned_abs();
    if bits > 32 || size > 1u128 << (bits - 1) {
        return None;
    }
    let (magic, shift) = multiplier(size, bits, bits - 1);
    let wide = program.widened(Opcode::SExt);
    let product = program.by(Opcode::Mul, wide, i128::try_from(magic).ok()?, 64);
    let shifted = program.by(Opcode::AShr, product, i128::from(bits + shift), 64);
    let low = program.narrowed(shifted);
    let sign = program.by(Opcode::AShr, DIVIDEND, i128::from(width - 1), width);
    Some(if divisor > 0 {
        program.op(Opcode::Sub, low, sign, width)
    } else {
        program.op(Opcode::Sub, sign, low, width)
    })
}

/// A signed division or remainder by a power of two or the negative of one.
///
/// A shift rounds towards minus infinity and C rounds towards zero, so a negative dividend has the
/// divisor less one added first, which is the bias. The sign shifted down is that bias, or the top
/// bit on its own when the power is one. The remainder is what the mask leaves of the biased
/// dividend with the bias taken back off.
fn biased(program: &mut Program, divisor: i128, remainder: bool) {
    let width = program.width;
    let size = divisor.unsigned_abs();
    let power = size.trailing_zeros();
    let bias = if power == 1 {
        program.by(Opcode::LShr, DIVIDEND, i128::from(width - 1), width)
    } else {
        let sign = program.by(Opcode::AShr, DIVIDEND, i128::from(width - 1), width);
        program.by(Opcode::LShr, sign, i128::from(width - power), width)
    };
    let sum = program.op(Opcode::Add, DIVIDEND, bias, width);
    if remainder {
        let mask = (1i128 << power) - 1;
        let low = program.by(Opcode::And, sum, mask, width);
        program.op(Opcode::Sub, low, bias, width);
        return;
    }
    let quotient = program.by(Opcode::AShr, sum, i128::from(power), width);
    if divisor < 0 {
        program.negated(quotient);
    }
}

/// A division the program promised is exact, as a shift and a multiply by the inverse of what is
/// left of the divisor.
///
/// An odd number has an inverse modulo any power of two, so a multiple of it times that inverse is
/// the other factor, wrapping and all. The power of two in the divisor comes out first as a shift,
/// which loses nothing because the dividend is a multiple of it. This is what a pointer subtraction
/// over a structure whose size is not a power of two becomes, and it needs no product wider than
/// the division, so it is done at sixty four bits too.
fn exactly(program: &mut Program, signed: bool, divisor: i128) {
    let width = program.width;
    let power = divisor.trailing_zeros();
    let mut value = DIVIDEND;
    if power > 0 {
        let shift = if signed { Opcode::AShr } else { Opcode::LShr };
        value = program.by(shift, DIVIDEND, i128::from(power), width);
    }
    let odd = divisor >> power;
    if odd != 1 {
        program.by(Opcode::Mul, value, signed_at(inverse(odd, width), width), width);
    }
}

/// The inverse of an odd number modulo two to the `width`.
///
/// Newton's iteration: an odd number is its own inverse modulo eight, and each step doubles the
/// bits that are right, so six steps are enough for sixty four.
fn inverse(odd: i128, width: u32) -> u128 {
    let odd = odd as u128;
    let mut inverse = odd;
    for _ in 0..6 {
        inverse = inverse.wrapping_mul(2u128.wrapping_sub(odd.wrapping_mul(inverse)));
    }
    inverse & mask(width)
}

/// Those bits read as a signed number of that width.
fn signed_at(bits: u128, width: u32) -> i128 {
    let spare = 128 - width;
    ((bits << spare) as i128) >> spare
}

/// The low `width` bits set.
fn mask(width: u32) -> u128 {
    if width >= 128 { u128::MAX } else { (1u128 << width) - 1 }
}

/// The division an instruction is, if it is one by a constant at a width this knows.
fn division(func: &Func, inst: Inst) -> Option<Division> {
    let data = &func[inst];
    let (signed, remainder) = match data.opcode {
        Opcode::SDiv => (true, false),
        Opcode::UDiv => (false, false),
        Opcode::SRem => (true, true),
        Opcode::URem => (false, true),
        _ => return None,
    };
    let &[dividend, by] = &func[data.args] else { return None };
    let ty = func[dividend].ty;
    if !ty.is_int() || ty.lanes() != 1 {
        return None;
    }
    let imm = constant(func, by)?;
    let divisor = if signed { imm.signed(ty) } else { i128::try_from(imm.unsigned()).ok()? };
    Some(Division {
        signed,
        remainder,
        exact: data.flags.contains(Flags::EXACT),
        width: ty.bits(),
        range: range(func, dividend, ty.bits(), signed),
        divisor,
    })
}

/// The immediate a value is, if it is a constant.
fn constant(func: &Func, value: Value) -> Option<Imm> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(imm) = func[inst].extra else { return None };
    Some(func[imm])
}

/// What the dividend holds, from the widening it came out of if it came out of one.
///
/// A value widened with zeroes is never negative, whichever way the division reads it. One widened
/// with its sign is a narrower signed value to a signed division, and to an unsigned one it is a
/// value of the whole width, since its top bits are the sign's.
fn range(func: &Func, value: Value, width: u32, signed: bool) -> Range {
    let whole = if signed { Range::Signed(width) } else { Range::Unsigned(width) };
    let Def::Result { inst, .. } = func[value].def else { return whole };
    let Some(&from) = func[func[inst].args].first() else { return whole };
    let bits = func[from].ty.bits();
    if bits == 0 || bits >= width {
        return whole;
    }
    match func[inst].opcode {
        Opcode::ZExt => Range::Unsigned(bits),
        Opcode::SExt if signed => Range::Signed(bits),
        _ => whole,
    }
}

/// Puts the program in front of the division and turns the division into its last step.
fn write(func: &mut Func, inst: Inst, program: &Program) {
    let Some((&Step::Op(opcode, args, _), before)) = program.steps.split_last() else { return };
    let mut values = vec![func[func[inst].args][0]];
    for &step in before {
        let value = match step {
            Step::Const(value, bits) => {
                let ty = Type::int(bits);
                ahead_const(func, inst, Imm::int(value, ty), ty)
            }
            Step::Op(opcode, args, bits) => {
                let operands: Vec<Value> =
                    args[..arity(opcode)].iter().map(|&at| values[at]).collect();
                ahead(func, inst, opcode, &operands, Type::int(bits))
            }
        };
        values.push(value);
    }
    let operands: Vec<Value> = args[..arity(opcode)].iter().map(|&at| values[at]).collect();
    becomes(func, inst, opcode, &operands);
}

/// How many operands an opcode of a program reads.
fn arity(opcode: Opcode) -> usize {
    match opcode {
        Opcode::Trunc | Opcode::ZExt | Opcode::SExt => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_cost::Goal;
    use rucc_ir::{Builder, Flags, Func, Opcode, Signature, Type, Value};

    use super::{Division, Program, Range, Step, divisions, mask, multiplier, program, signed_at};

    /// What a program gives for one dividend, worked the way the machine works it, with the width
    /// of every operand checked on the way.
    fn run(program: &Program, dividend: u128, values: &mut Vec<(u128, u32)>) -> u128 {
        values.clear();
        values.push((dividend & mask(program.width), program.width));
        for &step in &program.steps {
            let value = match step {
                Step::Const(value, bits) => (value as u128 & mask(bits), bits),
                Step::Op(opcode, [lhs, rhs], bits) => {
                    let (a, from) = values[lhs];
                    let (b, other) = values[rhs];
                    let answer = match opcode {
                        Opcode::ZExt | Opcode::Trunc | Opcode::SExt => {
                            let right =
                                if opcode == Opcode::Trunc { from > bits } else { from < bits };
                            assert!(right, "{opcode:?} from {from} bits to {bits}");
                            if opcode == Opcode::SExt { signed_at(a, from) as u128 } else { a }
                        }
                        _ => {
                            assert_eq!((from, other), (bits, bits), "{opcode:?} at {bits} bits");
                            match opcode {
                                Opcode::Add => a.wrapping_add(b),
                                Opcode::Sub => a.wrapping_sub(b),
                                Opcode::Mul => a.wrapping_mul(b),
                                Opcode::And => a & b,
                                Opcode::LShr | Opcode::AShr => {
                                    assert!(b < u128::from(bits), "a shift by {b} at {bits} bits");
                                    if opcode == Opcode::LShr {
                                        a >> b
                                    } else {
                                        (signed_at(a, bits) >> b) as u128
                                    }
                                }
                                _ => panic!("{opcode:?} is not something a program writes"),
                            }
                        }
                    };
                    (answer & mask(bits), bits)
                }
            };
            values.push(value);
        }
        let (answer, bits) = *values.last().expect("a program has steps");
        assert_eq!(bits, program.width, "the answer is at the width of the division");
        answer
    }

    /// What C says the division gives.
    fn truth(division: &Division, dividend: u128) -> u128 {
        let answer = if division.signed {
            let x = signed_at(dividend, division.width);
            if division.remainder { x % division.divisor } else { x / division.divisor }
        } else {
            let d = division.divisor as u128;
            (if division.remainder { dividend % d } else { dividend / d }) as i128
        };
        answer as u128 & mask(division.width)
    }

    /// The smallest and the largest value the range holds.
    fn ends(range: Range) -> (i128, i128) {
        match range {
            Range::Unsigned(bits) => (0, (1 << bits) - 1),
            Range::Signed(bits) => (-(1 << (bits - 1)), (1 << (bits - 1)) - 1),
        }
    }

    fn division(
        signed: bool,
        remainder: bool,
        width: u32,
        range: Range,
        divisor: i128,
    ) -> Division {
        Division { signed, remainder, exact: false, width, range, divisor }
    }

    /// Whether the rewrite is meant to leave this one as a `div`, which the tests hold it to, so a
    /// divisor quietly left alone is a failure rather than a pass.
    fn left(division: &Division) -> bool {
        let size = division.divisor.unsigned_abs();
        let (low, high) = ends(division.range);
        let most = low.unsigned_abs().max(high.unsigned_abs());
        let bits = match division.range {
            Range::Unsigned(bits) | Range::Signed(bits) => bits,
        };
        let signed_range = matches!(division.range, Range::Signed(_));
        matches!(division.divisor, 0 | 1)
            || (division.signed && division.divisor == -1)
            || (!signed_range && size > most)
            || (signed_range && !size.is_power_of_two() && (size > most || bits > 32))
            || (!signed_range && !size.is_power_of_two() && bits > 32)
    }

    /// Runs the rewrite of one division over these dividends and holds every answer to C's.
    fn check(
        division: Division,
        dividends: impl IntoIterator<Item = i128>,
        values: &mut Vec<(u128, u32)>,
    ) {
        let Some(program) = program(division) else {
            assert!(left(&division), "{division:?} was left as a div");
            return;
        };
        assert!(!left(&division), "{division:?} was rewritten");
        for x in dividends {
            let x = x as u128 & mask(division.width);
            assert_eq!(run(&program, x, values), truth(&division, x), "{division:?} of {x:#x}");
        }
    }

    /// Every multiple of the divisor in the range and the dividend either side of it, and the ends.
    ///
    /// The rewrite of a quotient only ever steps up as the dividend does, over the values that are
    /// not negative and again over the ones that are, and so does C's quotient. Two functions that
    /// only step up and agree at both ends of every run where one of them is flat agree all the way
    /// along it, so checking these is checking every dividend.
    fn edges(range: Range, divisor: i128) -> Vec<i128> {
        let (low, high) = ends(range);
        let size = divisor.abs().max(1);
        let mut all = vec![low, high, -1, 0, 1];
        let mut at = 0;
        while at <= high + 1 || -at >= low - 1 {
            all.extend([at - 1, at, at + 1, -at - 1, -at, -at + 1]);
            at += size;
        }
        all.retain(|&x| (low..=high).contains(&x));
        all
    }

    /// xorshift64, so the samples are the same every run.
    fn random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn both() -> [(bool, bool); 4] {
        [(false, false), (false, true), (true, false), (true, true)]
    }

    #[test]
    fn the_magic_numbers_are_the_ones_gcc_writes() {
        // `x % 100u` is `imulq $1374389535` and `shrq $37`, thirty two and five.
        assert_eq!(multiplier(100, 32, 32), (1_374_389_535, 5));
        // `x / 7u` needs thirty three bits, and gcc writes the low thirty two, `$613566757`.
        assert_eq!(multiplier(7, 32, 32), ((1 << 32) + 613_566_757, 3));
        // `x / 7` at `int` is `imulq $-1840700269`, the same bits read as signed, and `sarl $2`.
        assert_eq!(multiplier(7, 32, 31), (2_454_267_027, 2));
        // `x / -3` is `imulq $1431655766` and the high half as it is.
        assert_eq!(multiplier(3, 32, 31), (1_431_655_766, 0));
        // An `unsigned short` over ten is `imull $52429` and `shrl $19`.
        assert_eq!(multiplier(10, 16, 16), (52_429, 3));
    }

    /// Every dividend against every divisor at eight bits, at the width the division is done at
    /// and from a `char` widened to each wider one.
    #[test]
    fn every_eight_bit_division_by_every_divisor_is_right() {
        let mut values = Vec::new();
        for (signed, remainder) in both() {
            for width in [8, 16, 32, 64] {
                let mut ranges = vec![Range::Unsigned(8)];
                if signed {
                    ranges.push(Range::Signed(8));
                }
                for range in ranges {
                    if width == 8
                        && range != (if signed { Range::Signed(8) } else { Range::Unsigned(8) })
                    {
                        continue;
                    }
                    let (low, high) = ends(range);
                    let divisors: Vec<i128> =
                        if signed { (-300..=300).collect() } else { (0..=300).collect() };
                    for divisor in divisors {
                        let divisor = match (width, signed) {
                            (8, true) => signed_at(divisor as u128 & 0xff, 8),
                            (8, false) => divisor & 0xff,
                            _ => divisor,
                        };
                        check(
                            division(signed, remainder, width, range, divisor),
                            low..=high,
                            &mut values,
                        );
                    }
                }
            }
        }
    }

    /// Every divisor at sixteen bits, over the dividends that decide the answer, which [`edges`]
    /// says why is all of them.
    #[test]
    fn every_sixteen_bit_quotient_by_every_divisor_is_right() {
        let mut values = Vec::new();
        for signed in [false, true] {
            for width in [16, 32] {
                let range = if signed { Range::Signed(16) } else { Range::Unsigned(16) };
                let divisors: Vec<i128> =
                    if signed { (-32_768..=32_767).collect() } else { (0..=65_535).collect() };
                for divisor in divisors {
                    check(
                        division(signed, false, width, range, divisor),
                        edges(range, divisor),
                        &mut values,
                    );
                }
            }
        }
        // And a signed division of an `unsigned short`, which is the unsigned one.
        let range = Range::Unsigned(16);
        for divisor in -70_000..=70_000 {
            check(division(true, false, 32, range, divisor), edges(range, divisor), &mut values);
        }
    }

    /// The remainder is the quotient multiplied back and taken off, except for a power of two,
    /// where it is a mask of its own and is checked here over every dividend.
    #[test]
    fn every_sixteen_bit_remainder_by_a_power_of_two_is_right() {
        let mut values = Vec::new();
        for (signed, remainder) in both() {
            let range = if signed { Range::Signed(16) } else { Range::Unsigned(16) };
            let (low, high) = ends(range);
            for power in 1..16 {
                for divisor in [1i128 << power, -(1i128 << power)] {
                    if !signed && divisor < 0 {
                        continue;
                    }
                    check(division(signed, remainder, 16, range, divisor), low..=high, &mut values);
                }
            }
        }
    }

    /// Thirty two bits, over the ends, the multiples nearest them and a random sample, for every
    /// small divisor, every power of two and the divisors either side of one, and a random sample
    /// of the rest. Both at thirty two bits and from a thirty two bit value widened to sixty four.
    #[test]
    fn thirty_two_bit_divisions_are_right_at_the_edges_and_on_a_sample() {
        let mut values = Vec::new();
        let mut state = 0x9e37_79b9_7f4a_7c15;
        let mut divisors: Vec<i128> = (-2_000..=2_000).collect();
        for power in 1..=32 {
            let at = 1i128 << power;
            divisors.extend([at - 1, at, at + 1, -at + 1, -at, -at - 1]);
        }
        for _ in 0..2_000 {
            divisors.push(i128::from(random(&mut state) as u32));
            divisors.push(i128::from(random(&mut state) as i32));
        }
        for (signed, remainder) in both() {
            for (width, range) in [
                (32, if signed { Range::Signed(32) } else { Range::Unsigned(32) }),
                (64, Range::Unsigned(32)),
                (64, if signed { Range::Signed(32) } else { Range::Unsigned(32) }),
            ] {
                let (low, high) = ends(range);
                for &divisor in &divisors {
                    let divisor = if width == 32 {
                        let bits = divisor as u128 & mask(32);
                        if signed { signed_at(bits, 32) } else { bits as i128 }
                    } else {
                        divisor
                    };
                    if !signed && divisor < 0 {
                        continue;
                    }
                    let size = divisor.abs().max(1);
                    let mut dividends = vec![low, low + 1, -1, 0, 1, 2, high - 1, high];
                    for end in [low, high] {
                        let near = end / size * size;
                        dividends.extend([near - 1, near, near + 1, near - size, near + size]);
                    }
                    for _ in 0..64 {
                        let x = random(&mut state) as u128 & mask(32);
                        dividends.push(if matches!(range, Range::Signed(_)) {
                            signed_at(x, 32)
                        } else {
                            x as i128
                        });
                    }
                    dividends.retain(|x| (low..=high).contains(x));
                    check(
                        division(signed, remainder, width, range, divisor),
                        dividends,
                        &mut values,
                    );
                }
            }
        }
    }

    /// A sixty four bit dividend that could be anything needs a product of a hundred and twenty
    /// eight bits for any divisor that is not a power of two, and is left as a `div`. A power of
    /// two is not.
    #[test]
    fn sixty_four_bits_rewrite_only_a_power_of_two() {
        let mut values = Vec::new();
        let mut state = 0x2545_f491_4f6c_dd1d;
        for (signed, remainder) in both() {
            let range = if signed { Range::Signed(64) } else { Range::Unsigned(64) };
            let (low, high) = ends(range);
            let mut dividends = vec![low, low + 1, -1, 0, 1, high - 1, high];
            for _ in 0..512 {
                let x = u128::from(random(&mut state));
                dividends.push(if signed { signed_at(x, 64) } else { x as i128 });
            }
            dividends.retain(|x| (low..=high).contains(x));
            for power in 1..64 {
                for divisor in [1i128 << power, -(1i128 << power)] {
                    if !signed && divisor < 0 {
                        continue;
                    }
                    check(
                        division(signed, remainder, 64, range, divisor),
                        dividends.clone(),
                        &mut values,
                    );
                }
            }
            for divisor in [3, 7, 10, 1_000_000_007] {
                check(
                    division(signed, remainder, 64, range, divisor),
                    dividends.clone(),
                    &mut values,
                );
            }
        }
    }

    /// An exact division is a shift and a multiply by an inverse at any width, checked over
    /// multiples of the divisor, which are the only dividends it is promised.
    #[test]
    fn an_exact_division_is_right_over_every_multiple_it_is_given() {
        let mut values = Vec::new();
        let mut state = 0x1234_5678_9abc_def1;
        for signed in [false, true] {
            for width in [8, 16, 32, 64] {
                let range = if signed { Range::Signed(width) } else { Range::Unsigned(width) };
                let (low, high) = ends(range);
                for divisor in (-100i128..=100).chain([12, 24, 40, 56, 1 << 20, 3 << 30]) {
                    if (!signed && divisor < 0) || divisor < low || divisor > high {
                        continue;
                    }
                    let exact =
                        Division { exact: true, ..division(signed, false, width, range, divisor) };
                    let Some(program) = program(exact) else {
                        assert!(
                            matches!(divisor, 0 | 1) || (signed && divisor == -1),
                            "{exact:?} was left"
                        );
                        continue;
                    };
                    let size = divisor.abs();
                    let mut dividends: Vec<i128> = if width <= 16 {
                        (low / size..=high / size).map(|times| times * divisor).collect()
                    } else {
                        (0..256)
                            .map(|_| {
                                let times = signed_at(u128::from(random(&mut state)), 64)
                                    % (high / size).max(1);
                                if signed { times * divisor } else { times.abs() * divisor }
                            })
                            .collect()
                    };
                    dividends.extend([0, divisor, high / size * divisor]);
                    dividends.retain(|x| (low..=high).contains(x));
                    for x in dividends {
                        let x = x as u128 & mask(width);
                        assert_eq!(
                            run(&program, x, &mut values),
                            truth(&exact, x),
                            "{exact:?} of {x:#x}"
                        );
                    }
                }
            }
        }
    }

    /// A function of one parameter that returns what `body` makes of it.
    fn one(param: Type, ret: Type, body: impl FnOnce(&mut Builder<'_>, Value) -> Value) -> Func {
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("f"),
            Signature::new().with_params(&[param]).with_returns(&[ret]),
        );
        let entry = func.create_block();
        let x = func.append_param(entry, param);
        let mut build = Builder::new(&mut func, entry);
        let answer = body(&mut build, x);
        build.ret(&[answer]);
        func
    }

    /// The opcodes the function is left with, less its constants.
    fn opcodes(func: &Func) -> Vec<Opcode> {
        let entry = func.blocks().next().expect("an entry");
        func.insts(entry).map(|inst| func[inst].opcode).filter(|&op| op != Opcode::IConst).collect()
    }

    fn divided(
        opcode: Opcode,
        divisor: i128,
        flags: Flags,
    ) -> impl FnOnce(&mut Builder<'_>, Value) -> Value {
        move |build, x| {
            let ty = build.func()[x].ty;
            let by = build.iconst(ty, divisor);
            build.binary(opcode, x, by, flags)
        }
    }

    #[test]
    fn an_unsigned_division_by_seven_is_a_multiply_an_add_and_two_shifts() {
        let i32 = Type::int(32);
        let mut func = one(i32, i32, divided(Opcode::UDiv, 7, Flags::NONE));
        divisions(&mut func, Goal::Speed);
        let want = [
            Opcode::ZExt,
            Opcode::Mul,
            Opcode::LShr,
            Opcode::Add,
            Opcode::LShr,
            Opcode::Trunc,
            Opcode::Return,
        ];
        assert_eq!(opcodes(&func), want);
    }

    #[test]
    fn a_signed_remainder_is_the_quotient_multiplied_back() {
        let i32 = Type::int(32);
        let mut func = one(i32, i32, divided(Opcode::SRem, 7, Flags::NONE));
        divisions(&mut func, Goal::Speed);
        let want = [
            Opcode::SExt,
            Opcode::Mul,
            Opcode::AShr,
            Opcode::Trunc,
            Opcode::AShr,
            Opcode::Sub,
            Opcode::Mul,
            Opcode::Sub,
            Opcode::Return,
        ];
        assert_eq!(opcodes(&func), want);
    }

    /// `(int)x / 10` for an `unsigned short x`, where the widening says the dividend is sixteen
    /// bits and never negative, so the signed division is the unsigned one with the small number.
    #[test]
    fn a_widened_unsigned_short_takes_the_short_number_and_no_correction() {
        let (i16, i32) = (Type::int(16), Type::int(32));
        let mut func = one(i16, i32, |build, x| {
            let wide = build.unary(Opcode::ZExt, x, i32);
            divided(Opcode::SDiv, 10, Flags::NONE)(build, wide)
        });
        divisions(&mut func, Goal::Speed);
        let want =
            [Opcode::ZExt, Opcode::ZExt, Opcode::Mul, Opcode::LShr, Opcode::Trunc, Opcode::Return];
        assert_eq!(opcodes(&func), want);
    }

    /// `(p - q)` over a twelve byte structure, which the front end marks exact.
    #[test]
    fn an_exact_division_is_a_shift_and_a_multiply() {
        let i64 = Type::int(64);
        let mut func = one(i64, i64, divided(Opcode::SDiv, 12, Flags::EXACT));
        divisions(&mut func, Goal::Speed);
        assert_eq!(opcodes(&func), [Opcode::AShr, Opcode::Mul, Opcode::Return]);
    }

    #[test]
    fn size_a_variable_divisor_and_a_wide_dividend_keep_the_div() {
        let i32 = Type::int(32);
        let mut func = one(i32, i32, divided(Opcode::UDiv, 7, Flags::NONE));
        divisions(&mut func, Goal::Size);
        assert_eq!(opcodes(&func), [Opcode::UDiv, Opcode::Return]);

        let mut func = one(i32, i32, |build, x| build.binary(Opcode::SDiv, x, x, Flags::NONE));
        divisions(&mut func, Goal::Speed);
        assert_eq!(opcodes(&func), [Opcode::SDiv, Opcode::Return]);

        let i64 = Type::int(64);
        let mut func = one(i64, i64, divided(Opcode::UDiv, 10, Flags::NONE));
        divisions(&mut func, Goal::Speed);
        assert_eq!(opcodes(&func), [Opcode::UDiv, Opcode::Return]);
    }
}
