//! Scalar evolution: how a value changes across the iterations of a loop, and how many
//! iterations there are.
//!
//! Design: `spec/optimizer/07-loops-and-scev.md` sections 7.4 through 7.7. This is the second
//! half of document 07 and it answers the last two of the four questions section 7.6 says loop
//! analysis exists for. The first two are in [`crate::loops`].
//!
//! # Chains of recurrences, and how much of one
//!
//! GCC writes how a value changes as a chain of recurrences, `{base, +, step}`, meaning a value
//! that is `base` on the first iteration and `step` more on each one after. The representation is
//! good because it is closed under the operations anyone wants: adding two chrecs of the same
//! loop adds componentwise, multiplying by something invariant scales both parts, and evaluating
//! one at a given iteration is arithmetic rather than a special case. That closure is why
//! `j = 2 * i + 3` is as easy as `i = i + 1`, and pattern matching the second would run out of
//! road on the first.
//!
//! Section 7.4 says what rucc builds and it is a subset: affine chrecs only. A value is
//! invariant, or `{base, +, step}` with both parts invariant, or unknown. Addition, subtraction,
//! multiplication by an invariant, shifting by a constant, and extension where the extension
//! provably does not wrap. Nothing polynomial and nothing mutually recursive. That covers every
//! induction variable a C programmer writes and every array subscript document 31 could use, and
//! what it leaves out of GCC's four thousand lines is the part serving Fortran and the polyhedral
//! framework.
//!
//! The one extension past affine is pointer chrecs, because C loops walk pointers and `p = p + 1`
//! is `i = i + 1` with a scale. A `ptr_add` is addition with the byte offset as the step, which
//! is the difference between analysing half of real C loops and analysing nearly all of them.
//!
//! # Trip counts, and the part that is uncomfortable
//!
//! Given an exit that compares an affine chrec against something invariant, solving for the
//! iteration at which the comparison first fails is arithmetic. What makes it hard is that the
//! answer is almost always conditional: on the loop being entered at all, and on the induction
//! variable not wrapping before it gets there. Section 7.5 says a trip count returned without its
//! assumptions is a miscompilation generator, and that the temptation to return one is strong
//! because the assumptions are usually true.
//!
//! So [`Bound`] carries them and there is no way to read the count without seeing them.
//! [`Bound::parts`] hands back both, and [`Bound::proven`] hands back the count only when there
//! is nothing left to prove. A caller that means to emit a runtime check reads the assumptions
//! and emits it, and a caller that forgets cannot get at the number.
//!
//! [`Bound`] and [`Estimate`] are different types on purpose. A bound is used for correctness, an
//! estimate is used to decide whether a transformation is worth doing, and section 7.5 calls
//! conflating them a category error that costs correctness. GCC keeps them apart as
//! `max_loop_iterations` and `estimate_numbers_of_iterations` and the names do not stop anyone.
//! Different structs do.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_ir::{Block, Def, Extra, Flags, Func, Imm, Inst, IntPred, Opcode, Type, Value};

use crate::cfg::Cfg;
use crate::loops::{LoopId, Loops};

/// How deep the search for a step walks back through arithmetic.
///
/// The chain from a header parameter to the value fed back to it is two or three instructions in
/// anything a person writes, and the walk terminates on its own because SSA has no cycles except
/// through block parameters. The limit is here so a generated function with a thousand additions
/// in the increment costs a bounded amount rather than a stack.
const STEP_LIMIT: u32 = 16;

/// How many blocks that do nothing but pass a value on the walk reads through.
///
/// One is what a canonicalized loop has. The limit is here for the same reason the one above is,
/// which is that a generated function can have a chain of them and the cost of following it should
/// not depend on how long somebody made it.
const FORWARD_LIMIT: u32 = 8;

/// How many times a loop is assumed to run when nothing better is known.
///
/// GCC's `--param avg-loop-niter`, whose default is the same number. It is a guess, so nothing may
/// rest on it. It reaches [`Estimate`], which is only ever used to decide whether something is
/// worth doing, and [`crate::split`], which spends it on how far to ask the runtime to look and is
/// answered with a true count of bytes whatever it asked for.
pub(crate) const ASSUMED_ITERATIONS: u64 = 10;

/// A value that does not change inside the loop, read as `on + scale * value + offset`.
///
/// The `value` is a value defined outside the loop, or `None` when the linear part is a plain
/// number. Keeping the shape rather than a bare [`Value`] is what lets `j = 2 * i + 3` come out
/// as `{3, +, 2}` instead of unknown: the base and the step of that chrec are expressions nothing
/// in the function computes, so a representation that could only name existing values would have
/// to give up.
///
/// The `on` is a second symbol, and it is there for one shape: a pointer plus an index the loop
/// did not start at zero. `a[i]` with `i` starting at a parameter has a first address of
/// `a + start * 4`, which is two symbols, and a representation with room for one has to answer
/// unknown to it. Nothing scales `on` and nothing negates it, because the thing it was added for
/// is a pointer and a pointer is not something a loop multiplies. It is an [`Anchor`] rather than
/// a value so that the address of a global can be one of them.
///
/// The `read` is the other half of the same shape, since in C that index is an `int` and what
/// reaches the address is `sext(start)`. It is described rather than named, for the reason on
/// [`Widening`]. The two together are what let `a[start + i]` be followed, and on the SQLite
/// amalgamation they take 158 checks and 12 sites off the largest row of loop splitting's census.
/// See tamnd/rucc#810.
///
/// Arithmetic on two of these is refused once the sum would need a third symbol, because
/// `x + y + z` is not of this shape. That is the boundary of the subset and it is where the answer
/// becomes unknown rather than wrong.
///
/// The fields are private on purpose. Every reader has to go through [`Invariant::plain`], which
/// hands back the one symbol reading and refuses when there is a pointer in it, or through
/// [`Invariant::on`], which hands back both halves. A reader that helped itself to `value` and
/// `scale` would quietly drop the `on` and build an address off the wrong object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invariant {
    /// A second symbol the whole expression is measured from, or `None`. Always one of it.
    on: Option<Anchor>,
    /// What the linear part is built on, or `None` for a plain number.
    value: Option<Value>,
    /// How that value is read, when it is read at a width that is not its own.
    read: Option<Widening>,
    /// How many of it.
    scale: i128,
    /// What is added.
    offset: i128,
}

/// What an expression is measured from.
///
/// Usually a value the function computed somewhere outside the loop, which whoever reads the
/// invariant can name. Sometimes the address of a global, which nothing has to compute because it
/// is settled at link time and is the same number everywhere in the program.
///
/// The second one is here because of where a `global_addr` sits. [`crate::licm`] gives it a cost of
/// zero and so never moves it out of a loop, which is the right call: working the address out again
/// is one instruction and holding it in a register across a loop is a register. But that leaves the
/// instruction inside the loop, and [`Loops::is_invariant`] answers by where a value is defined, so
/// `a[i]` on a file scope `a` came out unknown. Describing the address rather than naming a value
/// is the same move [`Widening`] makes, and it means a reader that wants the address in front of
/// the loop writes another `global_addr` there for the one instruction it costs. On the SQLite
/// amalgamation that is 178 checks at 47 sites of loop splitting's largest census row.
/// See tamnd/rucc#810.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// A value, which is defined outside the loop and so can be named where it is wanted.
    Value(Value),
    /// The address of a global, which is written again wherever it is wanted.
    Address(Symbol),
}

impl Anchor {
    /// The value, when it is one. `None` for an address, which no value names.
    #[must_use]
    pub fn value(self) -> Option<Value> {
        match self {
            Self::Value(value) => Some(value),
            Self::Address(_) => None,
        }
    }
}

/// A value read at a type wider than its own.
///
/// Widening `{start, +, 1}` in `int` gives `{sext(start), +, 1}` in `long`, and `sext(start)` is an
/// expression nothing in the function computes. A representation that could only name values had
/// to refuse the whole widening on that account, which is what shut the door on a walk from an
/// index the caller handed in, because in C that index is an `int`. So the extension is described
/// rather than named and whoever builds code from the invariant emits it.
///
/// The value stays the narrow one. Reading it at a third width later is an extension of an
/// extension, and the two collapse into one wherever they mean the same thing, which is everywhere
/// except a zero extension read as signed afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Widening {
    /// Sign extended or zero extended.
    pub reading: Reading,
    /// The type it is read at, which is wider than the value's own.
    pub to: Type,
}

/// An invariant with no second symbol in it, read as `scale * value + offset`.
///
/// What every reader but [`crate::split`] wants, and what every reader wanted before there was an
/// `on` at all. [`Invariant::plain`] is the only way to one, so a reader that does not know about
/// the second symbol cannot get an expression that has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plain {
    /// What it is built on, or `None` for a plain number.
    pub value: Option<Value>,
    /// How that value is read, when it is read at a width that is not its own.
    pub read: Option<Widening>,
    /// How many of it.
    pub scale: i128,
    /// What is added to it.
    pub offset: i128,
}

impl Invariant {
    /// A plain number.
    #[must_use]
    pub fn number(offset: i128) -> Self {
        Self { on: None, value: None, read: None, scale: 0, offset }
    }

    /// One of a value.
    #[must_use]
    pub fn of(value: Value) -> Self {
        Self { on: None, value: Some(value), read: None, scale: 1, offset: 0 }
    }

    /// So many of a value, plus a number.
    #[must_use]
    pub fn scaled(value: Value, scale: i128, offset: i128) -> Self {
        Self { on: None, value: Some(value), read: None, scale, offset }
    }

    /// The address of a global.
    ///
    /// It goes straight into the `on` slot rather than into `value`, because that slot is the one
    /// for the thing an address is measured from and an address is the only thing this ever is.
    /// Nothing scales it and nothing negates it, which the rest of the arithmetic here already
    /// refuses for whatever is in that slot.
    #[must_use]
    pub fn address(symbol: Symbol) -> Self {
        Self { on: Some(Anchor::Address(symbol)), value: None, read: None, scale: 0, offset: 0 }
    }

    /// The one symbol reading, and `None` when there is a second symbol in it.
    #[must_use]
    pub fn plain(self) -> Option<Plain> {
        self.on.is_none().then_some(Plain {
            value: self.value,
            read: self.read,
            scale: self.scale,
            offset: self.offset,
        })
    }

    /// What it is measured from and how far past that, when there is a second symbol in it.
    ///
    /// Exactly one of this and [`Invariant::plain`] answers, so a reader that handles both has
    /// handled every invariant there is.
    #[must_use]
    pub fn on(self) -> Option<(Anchor, Plain)> {
        let on = self.on?;
        Some((
            on,
            Plain { value: self.value, read: self.read, scale: self.scale, offset: self.offset },
        ))
    }

    /// Whether the two are the same expression apart from the number added to them.
    #[must_use]
    pub fn alike(self, other: Self) -> bool {
        self.on == other.on
            && self.value == other.value
            && self.read == other.read
            && self.scale == other.scale
    }

    /// The number added to it, whatever else it has in it.
    #[must_use]
    pub fn offset(self) -> i128 {
        self.offset
    }

    /// The number this is, when it is one.
    #[must_use]
    pub fn as_number(self) -> Option<i128> {
        (self.on.is_none() && self.symbol().is_none()).then_some(self.offset)
    }

    /// Whether this is the number zero.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.as_number() == Some(0)
    }

    /// The value the linear part is built on, when the linear part has one.
    fn symbol(self) -> Option<Value> {
        if self.scale == 0 { None } else { self.value }
    }

    /// This as something to measure from, when it is one of a value and a number.
    ///
    /// Never a widened one. What an expression is measured from is a pointer, and a pointer is not
    /// something anything here extends.
    fn measure(self) -> Option<Anchor> {
        (self.on.is_none() && self.read.is_none() && self.scale == 1)
            .then_some(self.value)
            .flatten()
            .map(Anchor::Value)
    }

    /// The symbol both linear parts are built on and how it is read, when they agree on one or one
    /// of them has none.
    ///
    /// The same value read two ways is two different numbers, so agreeing on the value is not
    /// enough. `sext(x)` and `zext(x)` are the same bits and not the same quantity.
    fn shared(self, other: Self) -> Option<(Option<Value>, Option<Widening>)> {
        match (self.symbol(), other.symbol()) {
            (None, _) => Some((other.value, other.read)),
            (_, None) => Some((self.value, self.read)),
            (left, right) => {
                (left == right && self.read == other.read).then_some((left, self.read))
            }
        }
    }

    /// This same value read at a wider type, when the widening has a form here.
    ///
    /// A number means the same thing at both widths under a sign extension, and under a zero
    /// extension once it is not negative. One of a value becomes that value read through the
    /// extension. Anything else is refused, because the narrow arithmetic may already have wrapped
    /// and `sext(2 * x + 3)` is not `2 * sext(x) + 3`.
    fn widened(self, reading: Reading, to: Type) -> Option<Self> {
        if let Some(number) = self.as_number() {
            return (reading == Reading::Signed || number >= 0).then_some(Self::number(number));
        }
        if self.on.is_some() || self.scale != 1 || self.offset != 0 {
            return None;
        }
        let value = self.value?;
        // An extension of an extension. A zero extension is never negative, so reading its result
        // as signed afterwards is the same numbers and the pair collapses into the zero extension
        // at the outer width. The other way round it does not: a sign extension of a negative
        // number read as unsigned afterwards is a different number entirely.
        let reading = match (self.read.map(|read| read.reading), reading) {
            (None, outer) => outer,
            (Some(Reading::Unsigned), _) => Reading::Unsigned,
            (Some(Reading::Signed), Reading::Signed) => Reading::Signed,
            (Some(Reading::Signed), Reading::Unsigned) => return None,
        };
        Some(Self {
            on: None,
            value: Some(value),
            read: Some(Widening { reading, to }),
            scale: 1,
            offset: 0,
        })
    }

    /// The two added, when the sum is of this shape.
    #[must_use]
    pub fn plus(self, other: Self) -> Option<Self> {
        let offset = self.offset.checked_add(other.offset)?;
        // At most one of the two brought something to measure from, since a sum measured from two
        // pointers is not an address.
        let on = match (self.on, other.on) {
            (None, on) | (on, None) => on,
            (Some(_), Some(_)) => return None,
        };
        // The linear parts are about the same symbol, or one of them is a number, so they add.
        if let Some((value, read)) = self.shared(other) {
            let scale = self.scale.checked_add(other.scale)?;
            return Some(Self { on, value, read, scale, offset });
        }
        // Two different symbols, which is what a pointer plus an index the loop did not start at
        // zero is. Nothing may already be measured from anything, and one of the two has to be one
        // of a value and a number, and that one becomes what the sum is measured from.
        if on.is_some() {
            return None;
        }
        let (on, rest) = match (self.measure(), other.measure()) {
            (Some(on), _) => (on, other),
            (_, Some(on)) => (on, self),
            _ => return None,
        };
        Some(Self { on: Some(on), value: rest.value, read: rest.read, scale: rest.scale, offset })
    }

    /// The second subtracted from the first, when the difference is of this shape.
    #[must_use]
    pub fn minus(self, other: Self) -> Option<Self> {
        self.plus(other.negated()?)
    }

    /// This with its sign flipped, which needs nothing to measure from.
    ///
    /// A pointer is not a thing to negate, and the second symbol is only ever there because a
    /// pointer put it there.
    #[must_use]
    pub fn negated(self) -> Option<Self> {
        if self.on.is_some() {
            return None;
        }
        Some(Self {
            on: None,
            value: self.value,
            read: self.read,
            scale: self.scale.checked_neg()?,
            offset: self.offset.checked_neg()?,
        })
    }

    /// The two multiplied, which needs one of them to be a plain number and neither to be measured
    /// from anything.
    #[must_use]
    pub fn times(self, other: Self) -> Option<Self> {
        if self.on.is_some() || other.on.is_some() {
            return None;
        }
        let (symbol, by) = match (self.as_number(), other.as_number()) {
            (Some(by), _) => (other, by),
            (_, Some(by)) => (self, by),
            _ => return None,
        };
        Some(Self {
            on: None,
            value: symbol.value,
            read: symbol.read,
            scale: symbol.scale.checked_mul(by)?,
            offset: symbol.offset.checked_mul(by)?,
        })
    }
}

/// How a value changes from one iteration of a loop to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evolution {
    /// The same on every iteration.
    Invariant(Invariant),
    /// `{base, +, step}`: `base` the first time round and `step` more each time after.
    Affine(Chrec),
    /// Not something this analysis describes. Never a claim that the value does not evolve.
    Unknown,
}

impl Evolution {
    /// The chrec, when this is one.
    #[must_use]
    pub fn chrec(self) -> Option<Chrec> {
        match self {
            Self::Affine(chrec) => Some(chrec),
            _ => None,
        }
    }

    /// The invariant expression, when this is one.
    #[must_use]
    pub fn invariant(self) -> Option<Invariant> {
        match self {
            Self::Invariant(inv) => Some(inv),
            _ => None,
        }
    }
}

/// An affine chain of recurrences, `{base, +, step}`, evolving in a named type.
///
/// The type is not decoration. `{0, +, 1}` in `unsigned char` is not the sequence `0, 1, 2, ...`,
/// it is that sequence modulo two hundred and fifty six, and section 7.7 says this is where a
/// naive implementation is wrong constantly and in ways that pass every test written by someone
/// thinking in `int`. Every operation here checks the type and every one that cannot stay right
/// in it answers unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrec {
    /// What the value is on the first iteration.
    pub base: Invariant,
    /// What is added each time round.
    pub step: Invariant,
    /// The type it evolves in, which is what says when it wraps.
    pub ty: Type,
    /// What the instruction that increments it promised. `nsw` means the sequence does not wrap
    /// when read as signed and `nuw` means it does not when read as unsigned, and both come from
    /// the increment rather than from anything this analysis proved.
    pub flags: Flags,
}

impl Chrec {
    /// Whether the sequence is known not to wrap under the reading this predicate takes.
    #[must_use]
    pub fn does_not_wrap(self, signed: bool) -> bool {
        self.flags.contains(if signed { Flags::NSW } else { Flags::NUW })
    }
}

/// Something that has to be true for a trip count to be the right answer.
///
/// Section 7.5 asks for exactly this: not a trip count but a trip count plus a predicate under
/// which it holds, so the consumer either proves the predicate, emits a runtime check for it, or
/// gives up. These are the predicates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assumption {
    /// The counter starts on the near side of its limit, so the distance between them is a
    /// number that is not negative.
    ///
    /// For a loop ending on an ordering this is the loop being entered at all.
    /// `for (i = 0; i < n; i++)` with `n` of zero runs no times and the distance is zero, but `n`
    /// of minus one also runs no times and the distance is minus one, so a count taken from the
    /// distance has to be told which case it is in. For a loop ending on `!=` it is the limit
    /// being somewhere the counter is heading, because one stepping away from its limit never
    /// arrives.
    ///
    /// Only ever present on a symbolic count. When the distance is a number the sign of it is
    /// there to be read, so this is settled rather than assumed.
    Approaching,
    /// The induction variable does not wrap in its own type before the exit is taken.
    ///
    /// Present whenever the increment did not carry the matching `nsw` or `nuw` flag. With the
    /// flag there is nothing to assume, because the flag is the promise.
    NoWrap(Chrec),
    /// Signed overflow is undefined here, which is what makes `for (int i = 0; i <= n; i++)`
    /// finite.
    ///
    /// GCC infers loop bounds from this in `infer_loop_bounds_from_signedness`, and it is the
    /// single most common source of a report that the compiler broke a working program. It is
    /// recorded rather than assumed silently so that `-fwrapv` can withdraw the count and so that
    /// a dump can name it.
    StrictOverflow,
}

impl Assumption {
    /// What it says, in a line, for a dump to print.
    ///
    /// Section 7.5 asks that every inference of this kind be dumpable and say what it rests on,
    /// because a user who has been bitten by one deserves a command that tells them which line
    /// the compiler used against them. This is the sentence that command prints.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Approaching => "the counter starts on the near side of its limit".to_string(),
            Self::NoWrap(chrec) => {
                format!("the induction variable does not wrap in i{}", chrec.ty.bits())
            }
            Self::StrictOverflow => {
                "signed overflow is undefined, so -fwrapv withdraws this count".to_string()
            }
        }
    }
}

/// How many iterations, as a number or as an expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// Exactly this many.
    Exact(u128),
    /// This many, worked out from something the loop does not change.
    Symbolic(Invariant),
}

/// Which reading of its operands the test the count came from took.
///
/// It matters to anybody widening the value a symbolic count is built out of. The count is the
/// distance to the limit of the exit test, the limit is a value of the counter's own type, and
/// what that value means is the reading its test took. A limit past the middle of a thirty two bit
/// type is a large number to an unsigned test and a negative one to a signed test, and a consumer
/// that sign extends what an unsigned test compared has turned a loop over three billion elements
/// into a loop that runs no times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// The test read its operands as signed, so widening the count means sign extending it.
    Signed,
    /// The test read them as unsigned, so widening the count means zero extending it.
    Unsigned,
}

/// How many times a loop runs at most, and what that rests on.
///
/// For correctness. A pass that deletes an iteration, peels one off, or decides a memory access
/// is in bounds needs one of these. The count cannot be read without the assumptions, which is
/// section 7.7's defence against a caller proving two of three and forgetting the third.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    count: Count,
    assumptions: Vec<Assumption>,
    reading: Reading,
}

impl Bound {
    /// The count and everything it rests on, together, because they cannot be asked for apart.
    #[must_use]
    pub fn parts(&self) -> (Count, &[Assumption]) {
        (self.count, &self.assumptions)
    }

    /// How the value a symbolic count is built out of has to be read.
    ///
    /// Meaningless on a count that is a number, since a number has already been read.
    #[must_use]
    pub fn reading(&self) -> Reading {
        self.reading
    }

    /// What has to be proved before the count means anything.
    #[must_use]
    pub fn assumptions(&self) -> &[Assumption] {
        &self.assumptions
    }

    /// The count, for a caller with nothing left to prove.
    ///
    /// `None` does not mean the count is unknown. It means there are assumptions and this is not
    /// the accessor for reading a count that has them.
    #[must_use]
    pub fn proven(&self) -> Option<Count> {
        self.assumptions.is_empty().then_some(self.count)
    }

    /// The count, for a caller compiling a language where signed overflow is undefined.
    ///
    /// [`Bound::proven`] answers nothing for any `for (int i = 0; i < n; i++)` in any C program,
    /// because `solve` puts [`Assumption::StrictOverflow`] on every count taken from a signed
    /// test, and a pass built on `proven` alone is a pass that never fires. What that assumption
    /// says is that the count rests on signed overflow being undefined, and `-fwrapv` is
    /// implemented in `rucc-lower` by not setting `nsw` rather than by a flag anything down here
    /// reads. So an increment that still carries `nsw` under `-fwrapv` does not exist, and a bound
    /// with `StrictOverflow` and nothing else on it is a bound whose counter the front end
    /// promised does not wrap. That promise is exactly what the assumption wanted.
    ///
    /// [`Assumption::NoWrap`] is the case where there is no such promise, and it is refused here.
    /// So is [`Assumption::Approaching`], though only in passing, because it never appears on a
    /// count that is a number.
    #[must_use]
    pub fn under_undefined_overflow(&self) -> Option<Count> {
        self.assumptions
            .iter()
            .all(|rests_on| matches!(rests_on, Assumption::StrictOverflow))
            .then_some(self.count)
    }
}

/// How many times a loop probably runs.
///
/// For cost decisions and never for correctness. A pass asking whether unrolling pays for itself
/// wants one of these, and it is fine for the answer to be a guess, because being wrong makes the
/// code slower rather than wrong. Nothing here can be turned into a [`Bound`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Estimate {
    iterations: u64,
    guessed: bool,
}

impl Estimate {
    /// The number to do arithmetic with.
    #[must_use]
    pub fn iterations(self) -> u64 {
        self.iterations
    }

    /// Whether nothing was known and this is the default.
    #[must_use]
    pub fn is_guess(self) -> bool {
        self.guessed
    }
}

/// An exit test, read so that the loop keeps going while it holds.
///
/// Not public. It is the shape [`Scev::bound_at`] and [`Scev::holds`] both want out of the same
/// branch, and what either of them says about it is what the outside sees.
#[derive(Clone, Copy, Debug)]
struct Test {
    /// The side that moves, with the predicate already turned round to put it on the left.
    chrec: Chrec,
    /// The side that does not.
    limit: Invariant,
    /// The comparison that has to hold for the loop to go round again.
    pred: IntPred,
    /// Whether every iteration that goes round asks it.
    each: bool,
}

/// The analysis, which works out an answer when asked and remembers it.
///
/// Demand driven and memoized, per section 7.8, because the cost of scalar evolution is a
/// function of how many distinct values get asked about rather than of the size of the function.
/// The cache holds one loop's worth of answers per loop and the whole thing is thrown away when
/// anything about the loops changes, which per document 04.4 is any pass that touches one.
#[derive(Debug)]
pub struct Scev<'a> {
    func: &'a Func,
    cfg: &'a Cfg,
    loops: &'a Loops,
    known: HashMap<(LoopId, Value), Evolution>,
    held: HashMap<LoopId, Option<Chrec>>,
}

impl<'a> Scev<'a> {
    /// A fresh analysis over these loops, knowing nothing yet.
    #[must_use]
    pub fn new(func: &'a Func, cfg: &'a Cfg, loops: &'a Loops) -> Self {
        Self { func, cfg, loops, known: HashMap::new(), held: HashMap::new() }
    }

    /// How this value changes across the iterations of this loop.
    ///
    /// The way in, and what it does before answering is settle `Scev::holds` for the loop. That has
    /// to happen out here rather than at the point `Scev::extend` wants it, because settling it
    /// means asking about other values and `Scev::at` parks a marker on the value it is working on.
    /// Asked from in there, the answer would depend on what was already in flight.
    pub fn evolution(&mut self, id: LoopId, value: Value) -> Evolution {
        self.holds(id);
        self.at(id, value)
    }

    /// How this value changes, with the loop's own facts already settled.
    fn at(&mut self, id: LoopId, value: Value) -> Evolution {
        if let Some(&known) = self.known.get(&(id, value)) {
            return known;
        }
        // Unknown while the answer is being worked out, so the cycle from a header parameter back
        // to itself terminates instead of asking the same question forever. Anything that reaches
        // the parameter again gets unknown and the shape it was matching fails, which is the
        // right answer for a value defined in terms of itself through arithmetic this does not
        // describe.
        self.known.insert((id, value), Evolution::Unknown);
        let found = self.compute(id, value);
        self.known.insert((id, value), found);
        found
    }

    /// How many times this loop runs at most, and what that rests on.
    ///
    /// Any one exit gives a valid upper bound, because a loop cannot run more times than the
    /// first exit that fires, so this takes the first exit it can solve rather than the smallest.
    /// That is `max_loop_iterations` and not `estimate_numbers_of_iterations`, which is why the
    /// answer is a [`Bound`].
    pub fn bound(&mut self, id: LoopId) -> Option<Bound> {
        self.holds(id);
        let exits: Vec<Block> = self.loops.exits(id).iter().map(|exit| exit.from).collect();
        exits.into_iter().find_map(|from| self.bound_at(id, from))
    }

    /// The counter an exit test of this loop keeps inside its own type, when there is one.
    ///
    /// [`bounded_by_its_test`] is the argument and this is where its answer is written down as a
    /// fact about the loop rather than spent on one trip count. What it buys is [`Scev::extend`]:
    /// an unsigned counter carries no `nuw`, so widening anything built out of one used to be
    /// refused, and the test that holds the counter holds everything walking beside it.
    ///
    /// Settled once per loop and then read. It is settled from [`Scev::evolution`] and
    /// [`Scev::bound`], which are the two ways in, so that it is worked out with nothing in flight.
    /// The cache for the loop is emptied afterwards, because the answers already in it were worked
    /// out while this was still unknown and a conservative answer that stayed would make what the
    /// analysis says depend on which question was asked first.
    fn holds(&mut self, id: LoopId) -> Option<Chrec> {
        if let Some(&known) = self.held.get(&id) {
            return known;
        }
        // Unknown while it is being worked out, which is what stops the recursion below from
        // asking the same question forever, and which is why the cache is emptied after.
        self.held.insert(id, None);
        let exits: Vec<Block> = self.loops.exits(id).iter().map(|exit| exit.from).collect();
        let found = exits.into_iter().find_map(|from| {
            let test = self.test_at(id, from)?;
            let step = test.chrec.step.as_number()?;
            (test.each && bounded_by_its_test(test.pred, step)).then_some(test.chrec)
        });
        self.held.insert(id, found);
        self.known.retain(|&(of, _), _| of != id);
        found
    }

    /// How many times this loop probably runs.
    pub fn estimate(&mut self, id: LoopId) -> Estimate {
        match self.bound(id).map(|bound| bound.count) {
            Some(Count::Exact(exact)) => {
                Estimate { iterations: u64::try_from(exact).unwrap_or(u64::MAX), guessed: false }
            }
            _ => Estimate { iterations: ASSUMED_ITERATIONS, guessed: true },
        }
    }

    /// The evolution of a value nothing is known about yet.
    fn compute(&mut self, id: LoopId, value: Value) -> Evolution {
        if let Some(invariant) = self.invariant(id, value) {
            return Evolution::Invariant(invariant);
        }
        match self.func[value].def {
            Def::Param { block, index } if block == self.loops.header(id) => {
                self.at_header(id, value, index as usize)
            }
            // A parameter of a block inside the loop that is not the header takes a different
            // value depending on which way control came, and describing that is a job for the
            // value range work of document 10 rather than for a chrec. Unless there is only one
            // way in, in which case it does not.
            Def::Param { .. } => match self.forwarded(value) {
                same if same == value => Evolution::Unknown,
                through => self.at(id, through),
            },
            Def::Result { inst, .. } => self.at_inst(id, inst, value),
        }
    }

    /// The value as an expression that does not change inside the loop, if it is one.
    fn invariant(&self, id: LoopId, value: Value) -> Option<Invariant> {
        if let Some((imm, ty)) = constant(self.func, value) {
            return Some(Invariant::number(imm.signed(ty)));
        }
        // A constant is invariant wherever it sits, which is why it is asked about first. Anything
        // else has to be defined outside the loop.
        if self.loops.is_invariant(self.func, id, value) {
            return Some(Invariant::of(value));
        }
        // Except the address of a global, which is a link time constant and so does not change
        // inside a loop wherever it is written. Asked after the question above and not instead of
        // it, so that a `global_addr` already sitting outside the loop stays a value every reader
        // can name, and this arm is only the case that used to come out unknown. See [`Anchor`].
        symbol(self.func, value).map(Invariant::address)
    }

    /// The evolution of a parameter of the loop header, which is where an induction variable is.
    ///
    /// The parameter takes one value on the way in and another on the way round, which is what
    /// other IRs spell as a phi node. If the way round is the parameter plus something invariant,
    /// the parameter is an affine chrec and that something is its step.
    fn at_header(&mut self, id: LoopId, value: Value, index: usize) -> Evolution {
        let (func, cfg, loops) = (self.func, self.cfg, self.loops);
        let header = loops.header(id);
        // Section 7.3 wants exactly one latch and the canonicalizer makes one. Two of them means
        // two ways round with two different increments, and picking one would be a guess.
        let [latch] = loops.latches(id) else { return Evolution::Unknown };
        let mut entering = None;
        let mut around = None;
        for &pred in cfg.predecessors(header) {
            let Some(arg) = argument(func, pred, header, index) else { return Evolution::Unknown };
            let arg = self.forwarded(arg);
            let slot = if pred == *latch { &mut around } else { &mut entering };
            if slot.replace(arg).is_some_and(|old| old != arg) {
                return Evolution::Unknown;
            }
        }
        let (Some(entering), Some(around)) = (entering, around) else { return Evolution::Unknown };
        let Some(base) = self.invariant(id, entering) else { return Evolution::Unknown };
        let Some((step, flags)) = self.step(id, around, value, 0) else {
            return Evolution::Unknown;
        };
        affine(base, step, func[value].ty, flags)
    }

    /// The value a block parameter stands for, when there is only one way into its block.
    ///
    /// This is not an analysis, it is undoing a rename. A block with one predecessor has one value
    /// for each of its parameters and it is the argument that predecessor passes, so reading
    /// through it loses nothing and assumes nothing.
    ///
    /// It is here because of what canonicalization does. `crate::canon` splits the back edge of a
    /// loop to give it a latch of its own, and after that the value going round the loop is not the
    /// increment the loop computed, it is a parameter of a block that does nothing but pass the
    /// increment on. Without this, every counted loop the pipeline actually produces looks like a
    /// loop whose counter comes from somewhere unknown, and the trip count of a `for` loop in a
    /// real function comes back as nothing.
    fn forwarded(&self, value: Value) -> Value {
        let mut value = value;
        for _ in 0..FORWARD_LIMIT {
            let Def::Param { block, index } = self.func[value].def else { return value };
            let [pred] = self.cfg.predecessors(block) else { return value };
            let Some(arg) = argument(self.func, *pred, block, index as usize) else { return value };
            if arg == value {
                return value;
            }
            value = arg;
        }
        value
    }

    /// What is added to `of` to get `value`, and what the additions promised.
    ///
    /// Written as its own walk rather than as the general combination below, because at the point
    /// this runs the parameter's own evolution is not known yet and the general walk would ask
    /// for it and get unknown.
    fn step(&self, id: LoopId, value: Value, of: Value, depth: u32) -> Option<(Invariant, Flags)> {
        let value = self.forwarded(value);
        if value == of {
            // Nothing added yet, and nothing has had a chance to overflow either.
            return Some((Invariant::number(0), Flags::NSW.union(Flags::NUW)));
        }
        if depth >= STEP_LIMIT {
            return None;
        }
        let Def::Result { inst, .. } = self.func[value].def else { return None };
        let data = &self.func[inst];
        let args = &self.func[data.args];
        let (&lhs, &rhs) = (args.first()?, args.get(1)?);
        let combine = |carried: (Invariant, Flags), other: Invariant, subtract: bool| {
            let (delta, flags) = carried;
            let moved = if subtract { delta.minus(other)? } else { delta.plus(other)? };
            Some((moved, flags.intersection(data.flags)))
        };
        match data.opcode {
            Opcode::Add => {
                if let Some(carried) = self.step(id, lhs, of, depth + 1) {
                    return combine(carried, self.invariant(id, rhs)?, false);
                }
                combine(self.step(id, rhs, of, depth + 1)?, self.invariant(id, lhs)?, false)
            }
            Opcode::Sub => {
                combine(self.step(id, lhs, of, depth + 1)?, self.invariant(id, rhs)?, true)
            }
            // A pointer walks by bytes, and only the pointer side can be the one carrying the
            // induction variable. The offset is the step, which is the element size the front end
            // already multiplied in.
            Opcode::PtrAdd => {
                combine(self.step(id, lhs, of, depth + 1)?, self.invariant(id, rhs)?, false)
            }
            _ => None,
        }
    }

    /// The evolution of an instruction's result, from the evolutions of its operands.
    fn at_inst(&mut self, id: LoopId, inst: Inst, value: Value) -> Evolution {
        let func = self.func;
        let data = &func[inst];
        let (opcode, flags) = (data.opcode, data.flags);
        let args = &func[data.args];
        let ty = func[value].ty;
        let Some(&lhs) = args.first() else { return Evolution::Unknown };
        match opcode {
            Opcode::Add | Opcode::PtrAdd => {
                let Some(&rhs) = args.get(1) else { return Evolution::Unknown };
                let (left, right) = (self.at(id, lhs), self.at(id, rhs));
                combine(left, right, ty, flags, false)
            }
            Opcode::Sub => {
                let Some(&rhs) = args.get(1) else { return Evolution::Unknown };
                let (left, right) = (self.at(id, lhs), self.at(id, rhs));
                combine(left, right, ty, flags, true)
            }
            Opcode::Mul => {
                let Some(&rhs) = args.get(1) else { return Evolution::Unknown };
                let (left, right) = (self.at(id, lhs), self.at(id, rhs));
                scale(left, right, ty, flags)
            }
            // A shift by a constant is a multiplication by a power of two, and only by a constant:
            // a variable count is invariant in the loop and still not a number this can multiply
            // by. A count at or above the width is poison rather than a shift to zero, so the
            // range is checked here rather than assumed.
            Opcode::Shl => {
                let Some(&rhs) = args.get(1) else { return Evolution::Unknown };
                let Some((count, count_ty)) = constant(func, rhs) else {
                    return Evolution::Unknown;
                };
                let count = count.unsigned();
                if count >= u128::from(ty.bits()) || !count_ty.is_int() {
                    return Evolution::Unknown;
                }
                let by = Evolution::Invariant(Invariant::number(1i128 << count));
                scale(self.at(id, lhs), by, ty, flags)
            }
            Opcode::SExt | Opcode::ZExt => self.extend(id, opcode, lhs, ty),
            // A truncation is a wrap by construction, so a chrec through one describes a sequence
            // that restarts, and this does not have a representation for that.
            _ => Evolution::Unknown,
        }
    }

    /// A chrec widened, which needs the sequence not to wrap at the narrow width.
    ///
    /// Section 7.4 allows extension only where the extension provably does not wrap, and the first
    /// proof here is the flag the increment carries. `nsw` on the increment is the promise that the
    /// signed sequence does not wrap, which is exactly what makes the wide sequence the same
    /// numbers as the narrow one.
    ///
    /// The second proof is the loop's own exit test, through [`Scev::holds`] and [`trails`], and it
    /// is here because of what an unsigned counter looks like. `for (unsigned i = 0; i < n; i++)`
    /// carries no `nuw`, because C says unsigned arithmetic wraps, so `a[i]` on that counter used
    /// to come back unwidened and every bounds check in the loop stayed where it was. The test that
    /// keeps the counter inside its type keeps everything walking beside it inside too.
    ///
    /// Each part is either a plain number or one of a value, and nothing else. A number means the
    /// same thing at both widths, and one of a value becomes that value read through the extension,
    /// which is what [`Widening`] is for. Anything with arithmetic in it is refused, because the
    /// narrow arithmetic may already have wrapped and `sext(2 * x + 3)` is not `2 * sext(x) + 3`.
    /// What that leaves out is a base like `start + 1`, and what it lets in is `start`, which is
    /// the shape a walk from an index the caller handed in is in. See #810.
    fn extend(&mut self, id: LoopId, opcode: Opcode, from: Value, to: Type) -> Evolution {
        let narrow = self.func[from].ty;
        let signed = opcode == Opcode::SExt;
        let held = self.held.get(&id).copied().flatten();
        let settled = |chrec: Chrec| {
            chrec.does_not_wrap(signed) || (!signed && held.is_some_and(|held| trails(chrec, held)))
        };
        match self.at(id, from) {
            Evolution::Invariant(inv) => match inv.as_number() {
                // A number read at the narrow width means the same thing at the wide one under
                // sign extension, and under zero extension once it is not negative.
                Some(number) if signed || number >= 0 => Evolution::Invariant(inv),
                _ => Evolution::Unknown,
            },
            Evolution::Affine(chrec) if chrec.ty == narrow && settled(chrec) => {
                let reading = if signed { Reading::Signed } else { Reading::Unsigned };
                let (Some(base), Some(step)) =
                    (chrec.base.widened(reading, to), chrec.step.widened(reading, to))
                else {
                    return Evolution::Unknown;
                };
                Evolution::Affine(Chrec { base, step, ty: to, flags: chrec.flags })
            }
            _ => Evolution::Unknown,
        }
    }

    /// The trip count from the exit leaving this block, if this exit can be solved.
    fn bound_at(&mut self, id: LoopId, from: Block) -> Option<Bound> {
        let test = self.test_at(id, from)?;
        solve(test.chrec, test.limit, test.pred, test.each)
    }

    /// The exit test leaving this block, read into the pieces its two readers want.
    ///
    /// [`Scev::bound_at`] spends it on a trip count and [`Scev::holds`] spends it on whether the
    /// counter can wrap, and both want the same reading of the same branch, so the reading is
    /// written once.
    fn test_at(&mut self, id: LoopId, from: Block) -> Option<Test> {
        let func = self.func;
        let term = func.terminator(from)?;
        if func[term].opcode != Opcode::BrIf {
            return None;
        }
        let args = &func[func[term].args];
        let &cond = args.first()?;
        let calls = &func[func.target_list(term)];
        let (&taken, &not_taken) = (calls.first()?, calls.get(1)?);
        // Which arm keeps going. If both stay in or both leave, the branch is not the test that
        // ends the loop and there is nothing here to solve.
        let stays = match (
            self.loops.contains(id, taken.block),
            self.loops.contains(id, not_taken.block),
        ) {
            (true, false) => true,
            (false, true) => false,
            _ => return None,
        };

        let Def::Result { inst, .. } = func[cond].def else { return None };
        if func[inst].opcode != Opcode::ICmp {
            return None;
        }
        let Extra::IntPred(pred) = func[inst].extra else { return None };
        // The loop keeps going while the test says so, so an exit taken when the test is true is
        // an exit whose continuing condition is the opposite one.
        let pred = if stays { pred } else { invert(pred) };
        let operands = &func[func[inst].args];
        let (&lhs, &rhs) = (operands.first()?, operands.get(1)?);

        // One side evolves and the other does not. Swapping puts the one that evolves on the left
        // and turns the predicate round with it, so only one direction has to be solved.
        let (chrec, limit, pred) = match (self.at(id, lhs), self.at(id, rhs)) {
            (Evolution::Affine(chrec), other) => (chrec, other.invariant()?, pred),
            (other, Evolution::Affine(chrec)) => (chrec, other.invariant()?, swap(pred)),
            _ => return None,
        };

        // Whether every iteration that goes round asks this test. The header runs on all of them by
        // being the header. A latch runs on all of them only when it is the loop's one latch, since
        // with two of them an iteration can go round the other and never reach the test. Anywhere
        // else is a test under a condition, which [`bounded_by_its_test`] must not be given.
        //
        // The one latch is written out rather than taken for granted. `at_header` refuses a loop
        // with two of them already, so nothing reaching here has two, but the two conditions are
        // about different things and a later loosening of that one should not quietly loosen this.
        let each = from == self.loops.header(id) || self.loops.latches(id) == [from];
        Some(Test { chrec, limit, pred, each })
    }
}

/// Two evolutions added, or subtracted when asked.
fn combine(left: Evolution, right: Evolution, ty: Type, flags: Flags, subtract: bool) -> Evolution {
    let apply = |a: Invariant, b: Invariant| if subtract { a.minus(b) } else { a.plus(b) };
    match (left, right) {
        (Evolution::Invariant(a), Evolution::Invariant(b)) => {
            apply(a, b).map_or(Evolution::Unknown, Evolution::Invariant)
        }
        (Evolution::Affine(chrec), Evolution::Invariant(b)) => {
            // Adding something that does not move only moves the base.
            let Some(base) = apply(chrec.base, b) else { return Evolution::Unknown };
            affine(base, chrec.step, ty, flags.intersection(chrec.flags))
        }
        (Evolution::Invariant(a), Evolution::Affine(chrec)) => {
            let (Some(base), Some(step)) = (
                apply(a, chrec.base),
                if subtract { chrec.step.negated() } else { Some(chrec.step) },
            ) else {
                return Evolution::Unknown;
            };
            affine(base, step, ty, flags.intersection(chrec.flags))
        }
        (Evolution::Affine(a), Evolution::Affine(b)) => {
            // Two chrecs of the same loop add componentwise, which is the closure property that
            // makes the representation worth having. Of different types they do not, because the
            // two sequences wrap at different widths.
            if a.ty != b.ty {
                return Evolution::Unknown;
            }
            let (Some(base), Some(step)) = (apply(a.base, b.base), apply(a.step, b.step)) else {
                return Evolution::Unknown;
            };
            affine(base, step, ty, flags.intersection(a.flags).intersection(b.flags))
        }
        _ => Evolution::Unknown,
    }
}

/// One evolution multiplied by another, which needs one of them to stand still.
fn scale(left: Evolution, right: Evolution, ty: Type, flags: Flags) -> Evolution {
    let (chrec, by) = match (left, right) {
        (Evolution::Invariant(a), Evolution::Invariant(b)) => {
            return a.times(b).map_or(Evolution::Unknown, Evolution::Invariant);
        }
        (Evolution::Affine(chrec), Evolution::Invariant(by))
        | (Evolution::Invariant(by), Evolution::Affine(chrec)) => (chrec, by),
        // Two chrecs multiplied give a quadratic, which is a chain of recurrences with a second
        // step and is outside the subset section 7.4 chose.
        _ => return Evolution::Unknown,
    };
    let (Some(base), Some(step)) = (chrec.base.times(by), chrec.step.times(by)) else {
        return Evolution::Unknown;
    };
    affine(base, step, ty, flags.intersection(chrec.flags))
}

/// A chrec, or invariant when the step turns out to be nothing.
///
/// A step of zero is a valid affine chrec describing a value that does not move, and section 7.7
/// warns that code dividing by the step to get a trip count divides by zero. Reporting it as
/// invariant here means the shape is right for every reader rather than only for the careful
/// ones, and the trip count solver still checks, because a step can also come out zero from a
/// header parameter incremented by an invariant that happens to be zero.
fn affine(base: Invariant, step: Invariant, ty: Type, flags: Flags) -> Evolution {
    if step.is_zero() {
        return Evolution::Invariant(base);
    }
    Evolution::Affine(Chrec { base, step, ty, flags })
}

/// The iteration at which `chrec pred limit` first fails, with what that rests on.
///
/// `each` says the test runs on every iteration that goes round, which is what lets the test itself
/// stand in for a promise the counter does not carry. See [`bounded_by_its_test`].
fn solve(chrec: Chrec, limit: Invariant, pred: IntPred, each: bool) -> Option<Bound> {
    // Section 7.7's first way of being wrong. A step of zero is a loop that never leaves through
    // this exit, and dividing the distance by it is a crash rather than an answer.
    let step = chrec.step.as_number()?;
    if step == 0 {
        return None;
    }
    let signed = matches!(pred, IntPred::Slt | IntPred::Sle | IntPred::Sgt | IntPred::Sge);

    let mut assumptions = Vec::new();
    if !chrec.does_not_wrap(signed) && !(each && bounded_by_its_test(pred, step)) {
        assumptions.push(Assumption::NoWrap(chrec));
    }
    if signed {
        assumptions.push(Assumption::StrictOverflow);
    }

    // A test that does not read its operands as signed does not read the constants in them that
    // way either, and every constant reaching here was read as signed on the way in.
    let (base, limit) = if signed {
        (chrec.base, limit)
    } else {
        (as_unsigned(chrec.base, chrec.ty)?, as_unsigned(limit, chrec.ty)?)
    };

    // The distance the counter has to travel, always counting up. A loop going down is the same
    // problem with the ends swapped, which is why the step is used by size below and its sign is
    // spent here.
    let apart = step.unsigned_abs();
    let found = match (pred, step > 0) {
        (IntPred::Slt | IntPred::Ult, true) => {
            ordered(limit.minus(base)?, apart, false, assumptions)
        }
        (IntPred::Sle | IntPred::Ule, true) => {
            ordered(limit.minus(base)?, apart, true, assumptions)
        }
        (IntPred::Sgt | IntPred::Ugt, false) => {
            ordered(base.minus(limit)?, apart, false, assumptions)
        }
        (IntPred::Sge | IntPred::Uge, false) => {
            ordered(base.minus(limit)?, apart, true, assumptions)
        }
        (IntPred::Ne, _) => {
            let distance = if step > 0 { limit.minus(base)? } else { base.minus(limit)? };
            landing(distance, apart, assumptions)
        }
        // Either the counter steps away from the limit, in which case the loop is endless rather
        // than long, or the test is one this does not solve. Silence is the answer to both.
        _ => None,
    };
    // Written once here rather than threaded through the two solvers, because it is a fact about
    // the test and neither of them looks at the test. A count taken from a test with no sign to it,
    // which is `!=`, is read unsigned, because that is the reading `as_unsigned` above already put
    // its operands through.
    let reading = if signed { Reading::Signed } else { Reading::Unsigned };
    found.map(|(count, assumptions)| Bound { count, assumptions, reading })
}

/// Whether the exit test by itself rules out the counter wrapping before the loop ends.
///
/// An unsigned counter carries no `nuw`, because C says unsigned arithmetic wraps, so without this
/// every `for (unsigned i = 0; i < n; i++)` comes back resting on an assumption nothing downstream
/// can discharge. What discharges it is the test. A counter stepping up by exactly one is at the
/// limit before it is anywhere past it, and the test ends the loop there, so it never reaches the
/// top of its type. GCC works the same thing out in `scev_probably_wraps_p`.
///
/// Every part of that is load bearing. The step has to be one: `i += 2` can go from one below the
/// limit to one above the top of the type and come back round at the bottom, which is a loop that
/// runs forever rather than one that runs twice as fast. The test has to be the strict one: `<=`
/// lets the counter reach the limit and step once more, and a limit that is the largest number of
/// its type makes that last step the one that wraps. And the test has to run on every iteration
/// that goes round, or the counter can be stepped by a path that never asks it anything.
///
/// Nothing is claimed here about a signed counter, which needs no help: a signed counter that would
/// wrap is a program with undefined behaviour in it and [`Assumption::StrictOverflow`] is where
/// that is recorded.
fn bounded_by_its_test(pred: IntPred, step: i128) -> bool {
    matches!((pred, step), (IntPred::Ult, 1) | (IntPred::Ugt, -1))
}

/// Whether this sequence stays behind one the exit test already keeps inside its type.
///
/// [`bounded_by_its_test`] says the counter the test compares never reaches the top of its type.
/// Everything else the loop counts with is that counter plus a fixed distance, because two affine
/// chrecs of the same loop with the same step differ by a constant, so a sequence starting no
/// further along than the counter is a sequence that gets to the top no sooner than the counter
/// does, which is never.
///
/// Same base is the case that matters most and the easiest to see: the test compares `i + 1` and
/// the subscript reads `i`, which is one loop written two ways, and the two chrecs differ only in
/// where they start.
///
/// Going up only. A counter going down wraps at the bottom rather than the top, so the sequence
/// that is safe is the one that starts further along rather than the one that starts behind, and
/// nothing measured so far walks an array downwards. Doing it would be turning the comparison
/// round, and it should come with the program that wants it.
fn trails(chrec: Chrec, held: Chrec) -> bool {
    if chrec.ty != held.ty || chrec.step != held.step {
        return false;
    }
    if chrec.base == held.base {
        return true;
    }
    let (Some(step), Some(mine), Some(theirs)) =
        (chrec.step.as_number(), chrec.base.as_number(), held.base.as_number())
    else {
        return false;
    };
    // Read as unsigned, which is the reading the test took, so a base that came in negative is a
    // large number rather than a small one and starting behind is not what it is doing.
    step > 0 && mine >= 0 && theirs >= 0 && mine <= theirs
}

/// The same expression, read the way a test without a sign reads it.
///
/// Constants arrive here as the number their bits are when the sign bit is taken seriously,
/// because that is the only reading available before anybody knows what will be done with them.
/// An unsigned test disagrees about half of them. `for (unsigned char i = 0; i < 200; i++)` holds
/// its limit as minus fifty six, and a distance worked out from that is negative, which reads as
/// a loop that runs no times rather than one that runs two hundred.
///
/// The step is not put through this, because a step is a difference rather than a value and its
/// signed reading is the one that says which way the counter goes.
fn as_unsigned(inv: Invariant, ty: Type) -> Option<Invariant> {
    match inv.as_number() {
        Some(number) if number >= 0 => Some(inv),
        Some(number) => {
            // Only an integer constant was read as signed in the first place. A pointer never
            // was, so a negative number sitting in one is an expression this cannot reinterpret.
            let bits = ty.is_int().then(|| ty.bits()).filter(|&bits| bits < 127)?;
            Some(Invariant::number(number & ((1i128 << bits) - 1)))
        }
        // A symbolic operand is whatever it is at run time, and the subtraction below cancels it
        // rather than reading it, so long as nothing signed has been folded in beside it. Two
        // symbols is two things to cancel and the subtraction only ever cancels one.
        None => (inv.on.is_none() && inv.scale == 1 && inv.offset == 0).then_some(inv),
    }
}

/// The count for an exit tested with an ordering, where overshooting the limit still ends it.
fn ordered(
    distance: Invariant,
    step: u128,
    inclusive: bool,
    mut assumptions: Vec<Assumption>,
) -> Option<(Count, Vec<Assumption>)> {
    match distance.as_number() {
        Some(exact) => {
            if exact < 0 {
                // The counter starts past the limit, so the test fails the first time it runs.
                // That is a count of zero and it rests on nothing at all, not even on the counter
                // behaving, because the counter never moves.
                return Some((Count::Exact(0), Vec::new()));
            }
            // Rounding up, because a step that overshoots still took the iteration that overshot.
            let count = (exact.unsigned_abs() + u128::from(inclusive)).div_ceil(step);
            Some((Count::Exact(count), assumptions))
        }
        // Symbolic, and only for a step of one, because dividing an expression by anything else
        // needs a representation for a division and there is not one here.
        None if step == 1 => {
            assumptions.push(Assumption::Approaching);
            let count = distance.plus(Invariant::number(i128::from(inclusive)))?;
            Some((Count::Symbolic(count), assumptions))
        }
        None => None,
    }
}

/// The count for an exit tested with `!=`, where the counter has to land on the limit exactly.
///
/// This is a different problem from the one above and not a special case of it. An ordering test
/// ends the loop the moment the counter is past the limit, so a step that overshoots still stops.
/// `!=` only ends the loop on the one iteration where the counter is the limit, so a counter that
/// steps over the limit, or that starts on the far side of it, keeps going until it wraps. Both
/// of those are endless loops rather than short ones, and answering zero for either was the bug
/// this function exists to not have.
fn landing(
    distance: Invariant,
    step: u128,
    mut assumptions: Vec<Assumption>,
) -> Option<(Count, Vec<Assumption>)> {
    match distance.as_number() {
        Some(exact) => {
            let travel = u128::try_from(exact).ok()?;
            // Checked outright rather than assumed, which is why nothing here needs an assumption
            // about the step dividing anything.
            (travel % step == 0).then(|| (Count::Exact(travel / step), assumptions))
        }
        // A step of one lands on everything ahead of it, so the only thing left to establish is
        // that the limit is ahead. `while (p != end)` is this case, and a step of anything else
        // would need the division a symbolic distance has no room for.
        None if step == 1 => {
            assumptions.push(Assumption::Approaching);
            Some((Count::Symbolic(distance), assumptions))
        }
        None => None,
    }
}

/// The predicate that is true exactly when this one is not.
fn invert(pred: IntPred) -> IntPred {
    match pred {
        IntPred::Eq => IntPred::Ne,
        IntPred::Ne => IntPred::Eq,
        IntPred::Slt => IntPred::Sge,
        IntPred::Sle => IntPred::Sgt,
        IntPred::Sgt => IntPred::Sle,
        IntPred::Sge => IntPred::Slt,
        IntPred::Ult => IntPred::Uge,
        IntPred::Ule => IntPred::Ugt,
        IntPred::Ugt => IntPred::Ule,
        IntPred::Uge => IntPred::Ult,
    }
}

/// The predicate that says the same thing with the operands the other way round.
fn swap(pred: IntPred) -> IntPred {
    match pred {
        IntPred::Eq => IntPred::Eq,
        IntPred::Ne => IntPred::Ne,
        IntPred::Slt => IntPred::Sgt,
        IntPred::Sle => IntPred::Sge,
        IntPred::Sgt => IntPred::Slt,
        IntPred::Sge => IntPred::Sle,
        IntPred::Ult => IntPred::Ugt,
        IntPred::Ule => IntPred::Uge,
        IntPred::Ugt => IntPred::Ult,
        IntPred::Uge => IntPred::Ule,
    }
}

/// The constant a value is, if it is one.
fn constant(func: &Func, value: Value) -> Option<(Imm, Type)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(at) = func[inst].extra else { return None };
    let ty = func[value].ty;
    ty.is_int().then(|| (func[at], ty))
}

/// The global whose address a value is, if it is one.
fn symbol(func: &Func, value: Value) -> Option<Symbol> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::GlobalAddr {
        return None;
    }
    let Extra::Symbol(symbol) = func[inst].extra else { return None };
    Some(symbol)
}

/// What this predecessor passes to the block's parameter at this position.
///
/// `None` when the predecessor branches to the block more than once with different arguments,
/// which a `br_if` with both arms on the same block can do and which means the parameter takes a
/// value that depends on the test rather than on the edge.
fn argument(func: &Func, pred: Block, block: Block, index: usize) -> Option<Value> {
    let term = func.terminator(pred)?;
    let mut found = None;
    for call in func.successors(term) {
        if call.block != block {
            continue;
        }
        let arg = *func[call.args].get(index)?;
        if found.replace(arg).is_some_and(|old| old != arg) {
            return None;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Extra, Flags, Func, InstData, IntPred, Opcode, Signature, Type, Value};

    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::loops::{LoopId, Loops};
    use crate::scev::{
        Anchor, Assumption, Bound, Count, Evolution, Invariant, Reading, Scev, Widening,
    };

    /// A loop counting in `ty` from `from` by `step` while the counter is below `to`.
    ///
    /// ```text
    /// entry:  jump header(from)
    /// header(i): test = icmp pred i, to ; br_if test, body, exit
    /// body:   next = add i, step ; jump header(next)
    /// exit:   ret
    /// ```
    ///
    /// The counter is the header's only parameter, which is what the tests ask about.
    struct Counted {
        func: Func,
        counter: Value,
        next: Value,
    }

    fn counted(ty: Type, from: i128, to: i128, step: i128, pred: IntPred, flags: Flags) -> Counted {
        let (it, ()) = counted_with(ty, from, to, step, pred, flags, |_, _| ());
        it
    }

    /// The same loop, with `extra` run in the body on the counter before the counter steps.
    ///
    /// The builder appends, and the body's `jump` back to the header has to stay the last
    /// instruction in it or the block has no terminator and the loop stops being one. So anything
    /// a test wants derived from the counter goes in here rather than being tacked on afterwards.
    fn counted_with<T>(
        ty: Type,
        from: i128,
        to: i128,
        step: i128,
        pred: IntPred,
        flags: Flags,
        extra: impl FnOnce(&mut Builder<'_>, Value) -> T,
    ) -> (Counted, T) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();
        let counter = func.append_param(header, ty);

        let mut build = Builder::new(&mut func, entry);
        let start = build.iconst(ty, from);
        build.jump(header, &[start]);

        let mut build = Builder::new(&mut func, header);
        let limit = build.iconst(ty, to);
        let test = build.icmp(pred, counter, limit);
        build.br_if(test, body, &[], exit, &[]);

        let mut build = Builder::new(&mut func, body);
        let derived = extra(&mut build, counter);
        let by = build.iconst(ty, step);
        let next = build.binary(Opcode::Add, counter, by, flags);
        build.jump(header, &[next]);

        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);

        (Counted { func, counter, next }, derived)
    }

    /// The analysis over a function, along with the one loop it has.
    fn analyse(func: &Func) -> (Cfg, Loops) {
        let cfg = Cfg::new(func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        (cfg, loops)
    }

    /// The chrec of a value in the one loop of a function.
    fn evolution(func: &Func, value: Value) -> Evolution {
        let (cfg, loops) = analyse(func);
        let id = loops.roots()[0];
        Scev::new(func, &cfg, &loops).evolution(id, value)
    }

    /// The trip count of the one loop of a function.
    fn bound(func: &Func) -> Option<Bound> {
        let (cfg, loops) = analyse(func);
        let id: LoopId = loops.roots()[0];
        Scev::new(func, &cfg, &loops).bound(id)
    }

    #[test]
    fn a_counter_from_zero_by_one_is_the_chrec_everyone_expects() {
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Slt, Flags::NSW);
        let chrec = evolution(&it.func, it.counter).chrec().expect("the counter evolves");
        assert_eq!(chrec.base, Invariant::number(0));
        assert_eq!(chrec.step, Invariant::number(1));
        assert_eq!(chrec.ty, Type::int(32));
        assert!(chrec.does_not_wrap(true));
    }

    #[test]
    fn a_walk_over_a_file_scope_array_is_a_chrec_measured_from_the_symbol() {
        // The `global_addr` is inside the loop, which is where the compiler leaves one: working
        // the address out again is a single instruction and `crate::licm` would rather do that
        // than hold it in a register the whole way round. Answering by where a value is defined
        // meant `a[i]` on a file scope `a` was an address with nothing to say about it.
        let mut names = Interner::new();
        let tab = names.intern("tab");
        let (it, address) =
            counted_with(Type::int(64), 0, 100, 1, IntPred::Slt, Flags::NSW, |build, counter| {
                let four = build.iconst(Type::int(64), 4);
                let by = build.binary(Opcode::Mul, counter, four, Flags::NSW);
                let extra = Extra::Symbol(tab);
                let at =
                    build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
                let args = build.func().push_values(&[at, by]);
                build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
            });

        let chrec = evolution(&it.func, address).chrec().expect("the address evolves");
        assert_eq!(chrec.step, Invariant::number(4));
        // Described rather than named, so there is nothing for `plain` to hand back and a reader
        // of the base has to go through `on` and see what it is measured from.
        assert!(chrec.base.plain().is_none());
        let (base, rest) = chrec.base.on().expect("the base is measured from the symbol");
        assert_eq!(base, Anchor::Address(tab));
        assert_eq!(base.value(), None);
        assert_eq!(rest.value, None);
        assert_eq!(rest.offset, 0);
    }

    #[test]
    fn the_value_fed_back_is_the_chrec_one_step_along() {
        let it = counted(Type::int(32), 5, 100, 3, IntPred::Slt, Flags::NSW);
        let chrec = evolution(&it.func, it.next).chrec().expect("the increment evolves");
        assert_eq!(chrec.base, Invariant::number(8));
        assert_eq!(chrec.step, Invariant::number(3));
    }

    #[test]
    fn a_multiple_of_the_counter_plus_a_number_is_a_chrec_of_its_own() {
        // `j = 2 * i + 3` where `i = {0, +, 1}`, which is the shape section 7.4 says pattern
        // matching runs out of road on and chains of recurrences do not.
        let (it, shifted) =
            counted_with(Type::int(32), 0, 100, 1, IntPred::Slt, Flags::NSW, |build, counter| {
                let two = build.iconst(Type::int(32), 2);
                let three = build.iconst(Type::int(32), 3);
                let doubled = build.binary(Opcode::Mul, counter, two, Flags::NSW);
                build.binary(Opcode::Add, doubled, three, Flags::NSW)
            });

        let chrec = evolution(&it.func, shifted).chrec().expect("it evolves");
        assert_eq!(chrec.base, Invariant::number(3));
        assert_eq!(chrec.step, Invariant::number(2));
    }

    #[test]
    fn a_shift_by_a_constant_scales_the_chrec_and_a_shift_past_the_width_does_not() {
        let (it, (scaled, poison)) =
            counted_with(Type::int(32), 1, 100, 1, IntPred::Slt, Flags::NSW, |build, counter| {
                let three = build.iconst(Type::int(32), 3);
                let wide = build.iconst(Type::int(32), 32);
                (
                    build.binary(Opcode::Shl, counter, three, Flags::NSW),
                    build.binary(Opcode::Shl, counter, wide, Flags::NSW),
                )
            });

        let chrec = evolution(&it.func, scaled).chrec().expect("it evolves");
        assert_eq!(chrec.base, Invariant::number(8));
        assert_eq!(chrec.step, Invariant::number(8));
        // A count at the width is poison rather than a shift to zero, so there is no sequence to
        // describe.
        assert_eq!(evolution(&it.func, poison), Evolution::Unknown);
    }

    #[test]
    fn a_pointer_walked_by_the_element_size_is_a_chrec_in_bytes() {
        // What `for (p = a; p != end; p++)` lowers to on an array of four byte elements. Section
        // 7.4 calls this the one deliberate extension past affine and the difference between
        // analysing half of real C loops and nearly all of them.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();
        let start = func.append_param(entry, Type::PTR);
        let cursor = func.append_param(header, Type::PTR);

        let mut build = Builder::new(&mut func, entry);
        build.jump(header, &[start]);
        let mut build = Builder::new(&mut func, header);
        let done = build.icmp(IntPred::Eq, cursor, start);
        build.br_if(done, exit, &[], body, &[]);
        let mut build = Builder::new(&mut func, body);
        let four = build.iconst(Type::int(64), 4);
        let next = build.binary(Opcode::PtrAdd, cursor, four, Flags::NONE);
        build.jump(header, &[next]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);

        let chrec = evolution(&func, cursor).chrec().expect("the cursor evolves");
        assert_eq!(chrec.base, Invariant::of(start));
        assert_eq!(chrec.step, Invariant::number(4));
        assert_eq!(chrec.ty, Type::PTR);
    }

    /// A value to hang an invariant on, which these never look inside.
    fn some_value() -> Value {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        func.append_param(entry, Type::int(8))
    }

    #[test]
    fn one_of_a_value_widens_and_arithmetic_on_it_does_not() {
        // What `Scev::extend` may take. A value is widened by describing the extension rather than
        // by naming a value nothing computes, which is what lets `for (i = start; i < n; i++)`
        // have a chrec at pointer width. `2 * x + 3` is refused, because the narrow arithmetic may
        // already have wrapped and `sext(2 * x + 3)` is not `2 * sext(x) + 3`.
        let value = some_value();
        let word = Type::int(64);
        assert_eq!(
            Invariant::of(value).widened(Reading::Signed, word),
            Some(Invariant {
                on: None,
                value: Some(value),
                read: Some(Widening { reading: Reading::Signed, to: word }),
                scale: 1,
                offset: 0,
            }),
        );
        assert_eq!(Invariant::scaled(value, 2, 3).widened(Reading::Signed, word), None);
        assert_eq!(Invariant::scaled(value, 1, 3).widened(Reading::Signed, word), None);
        // A number is the same number at both widths under a sign extension, and under a zero
        // extension once it is not negative.
        assert_eq!(
            Invariant::number(-1).widened(Reading::Signed, word),
            Some(Invariant::number(-1)),
        );
        assert_eq!(Invariant::number(-1).widened(Reading::Unsigned, word), None);
    }

    #[test]
    fn an_extension_of_an_extension_collapses_only_where_it_means_the_same_thing() {
        // A zero extension is never negative, so reading its result as signed afterwards is the
        // same numbers and the pair is one zero extension at the outer width. The other way round
        // it is not: a sign extended negative number read as unsigned is a different quantity, and
        // there is nothing to collapse to.
        let value = some_value();
        let (half, word) = (Type::int(32), Type::int(64));
        let read = |inv: Invariant| inv.read.expect("a widened value carries how it is read");

        let zeroed = Invariant::of(value).widened(Reading::Unsigned, half).expect("it widens");
        let again = zeroed.widened(Reading::Signed, word).expect("and it widens again");
        assert_eq!(read(again), Widening { reading: Reading::Unsigned, to: word });

        let signed = Invariant::of(value).widened(Reading::Signed, half).expect("it widens");
        assert_eq!(signed.widened(Reading::Unsigned, word), None);
        let again = signed.widened(Reading::Signed, word).expect("and it widens again");
        assert_eq!(read(again), Widening { reading: Reading::Signed, to: word });
    }

    #[test]
    fn two_invariants_on_the_same_value_read_two_ways_do_not_add() {
        // `sext(x)` and `zext(x)` are the same bits and not the same quantity, so a sum of them is
        // not two of anything and there is no shape here for it.
        let value = some_value();
        let word = Type::int(64);
        let signed = Invariant::of(value).widened(Reading::Signed, word).expect("it widens");
        let zeroed = Invariant::of(value).widened(Reading::Unsigned, word).expect("it widens");
        assert_eq!(signed.plus(zeroed), None);
        assert_eq!(
            signed.plus(signed),
            Some(Invariant {
                on: None,
                value: Some(value),
                read: Some(Widening { reading: Reading::Signed, to: word }),
                scale: 2,
                offset: 0,
            }),
            "the same value read the same way adds to two of it",
        );
    }

    #[test]
    fn a_counter_in_unsigned_char_wraps_and_does_not_widen_without_a_promise() {
        // Section 7.7's second way of being wrong. `{0, +, 1}` in `unsigned char` is not
        // `0, 1, 2, ...`, it is that modulo two hundred and fifty six, and widening it is only
        // the same sequence if it does not get that far.
        //
        // An inclusive test, because a strict one is a proof of its own and the case below is
        // about what happens when there is no proof at all. This loop does not in fact wrap, and
        // the point is that nothing here can say so.
        let (it, wide) =
            counted_with(Type::int(8), 0, 100, 1, IntPred::Ule, Flags::NONE, |build, counter| {
                build.unary(Opcode::ZExt, counter, Type::int(32))
            });
        let chrec = evolution(&it.func, it.counter).chrec().expect("the counter evolves");
        assert_eq!(chrec.ty, Type::int(8));
        assert!(!chrec.does_not_wrap(false));
        assert_eq!(evolution(&it.func, wide), Evolution::Unknown);
    }

    #[test]
    fn a_counter_its_own_test_holds_widens_without_a_promise() {
        // The same counter under the strict test, which is the shape `for (unsigned i = 0; i < n;
        // i++)` has. Nothing promised anything, and the test is the proof: the counter is at the
        // limit before it is anywhere past it, and the loop ends there.
        let (it, wide) =
            counted_with(Type::int(8), 0, 100, 1, IntPred::Ult, Flags::NONE, |build, counter| {
                build.unary(Opcode::ZExt, counter, Type::int(32))
            });
        let narrow = evolution(&it.func, it.counter).chrec().expect("the counter evolves");
        assert!(!narrow.does_not_wrap(false), "nothing was promised, so nothing carries a flag");
        let chrec = evolution(&it.func, wide).chrec().expect("its own test holds it");
        assert_eq!(chrec.ty, Type::int(32));
        assert_eq!(chrec.base, Invariant::number(0));
        assert_eq!(chrec.step, Invariant::number(1));
    }

    #[test]
    fn a_sequence_that_starts_further_along_than_the_counter_does_not_widen() {
        // `trails` in the direction it refuses. The test holds `i`, which starts at zero, and this
        // asks about `i + 1`, which starts one further along. One further along is where the
        // counter would be if it had gone round once more, and going round once more is the step
        // nothing here rules out.
        let (it, wide) =
            counted_with(Type::int(8), 0, 100, 1, IntPred::Ult, Flags::NONE, |build, counter| {
                let one = build.iconst(Type::int(8), 1);
                let ahead = build.binary(Opcode::Add, counter, one, Flags::NONE);
                build.unary(Opcode::ZExt, ahead, Type::int(32))
            });
        assert_eq!(evolution(&it.func, wide), Evolution::Unknown);
    }

    #[test]
    fn a_counter_in_short_widens_when_the_increment_promised_it_would_not_wrap() {
        let (it, (wide, zero_extended)) =
            counted_with(Type::int(16), 0, 100, 1, IntPred::Slt, Flags::NSW, |build, counter| {
                (
                    build.unary(Opcode::SExt, counter, Type::int(32)),
                    build.unary(Opcode::ZExt, counter, Type::int(32)),
                )
            });

        let chrec = evolution(&it.func, wide).chrec().expect("it widens");
        assert_eq!(chrec.ty, Type::int(32));
        assert_eq!(chrec.base, Invariant::number(0));
        assert_eq!(chrec.step, Invariant::number(1));
        // `nsw` is a promise about the signed reading and says nothing about the unsigned one.
        assert_eq!(evolution(&it.func, zero_extended), Evolution::Unknown);
    }

    #[test]
    fn a_step_of_zero_is_invariant_and_has_no_trip_count() {
        // Section 7.7's first way of being wrong. `i += k` with `k` of zero is a valid affine
        // chrec of a loop that never leaves through this exit, and code dividing the distance by
        // the step divides by zero.
        let it = counted(Type::int(32), 0, 100, 0, IntPred::Slt, Flags::NSW);
        assert!(matches!(evolution(&it.func, it.counter), Evolution::Invariant(_)));
        assert_eq!(bound(&it.func), None);
    }

    #[test]
    fn a_counted_loop_has_the_count_anyone_would_work_out_by_hand() {
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Slt, Flags::NSW);
        let found = bound(&it.func).expect("it is counted");
        let (count, assumptions) = found.parts();
        assert_eq!(count, Count::Exact(100));
        // The distance is a number and it is not negative, so being entered is not in question.
        // Signed overflow being undefined still is, which is what `-fwrapv` would withdraw.
        assert_eq!(assumptions, [Assumption::StrictOverflow]);
        assert_eq!(found.proven(), None);
    }

    #[test]
    fn a_step_that_overshoots_still_takes_the_iteration_that_overshot() {
        // Zero, three, six, nine, and the test fails at twelve, so four iterations rather than
        // three and a third. Rounding the other way is an off by one in every unroller.
        let it = counted(Type::int(32), 0, 10, 3, IntPred::Slt, Flags::NSW);
        let (count, _) = bound(&it.func).expect("it is counted").parts();
        assert_eq!(count, Count::Exact(4));
    }

    #[test]
    fn an_inclusive_test_runs_one_more_time() {
        let it = counted(Type::int(32), 0, 10, 1, IntPred::Sle, Flags::NSW);
        let (count, _) = bound(&it.func).expect("it is counted").parts();
        assert_eq!(count, Count::Exact(11));
    }

    #[test]
    fn a_loop_whose_test_fails_first_time_runs_no_times_and_rests_on_nothing() {
        let it = counted(Type::int(32), 10, 0, 1, IntPred::Slt, Flags::NSW);
        let found = bound(&it.func).expect("it is counted");
        assert_eq!(found.proven(), Some(Count::Exact(0)));
        assert!(found.assumptions().is_empty());
    }

    #[test]
    fn counting_down_is_the_same_problem_with_the_ends_swapped() {
        let it = counted(Type::int(32), 10, 0, -1, IntPred::Sgt, Flags::NSW);
        let (count, _) = bound(&it.func).expect("it is counted").parts();
        assert_eq!(count, Count::Exact(10));
    }

    #[test]
    fn an_unsigned_test_does_not_drag_in_the_signed_overflow_assumption() {
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Ult, Flags::NUW);
        let found = bound(&it.func).expect("it is counted");
        assert_eq!(found.proven(), Some(Count::Exact(100)));
    }

    #[test]
    fn a_test_against_something_the_loop_does_not_change_gives_a_symbolic_count() {
        // `for (i = 0; i < n; i++)`, where the answer is `n` and is only `n` if the loop is
        // entered, because `n` of minus one runs no times and the distance is minus one.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();
        let limit = func.append_param(entry, Type::int(32));
        let counter = func.append_param(header, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(header, &[zero]);
        let mut build = Builder::new(&mut func, header);
        let test = build.icmp(IntPred::Slt, counter, limit);
        build.br_if(test, body, &[], exit, &[]);
        let mut build = Builder::new(&mut func, body);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        build.jump(header, &[next]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);

        let found = bound(&func).expect("it is counted");
        let (count, assumptions) = found.parts();
        assert_eq!(count, Count::Symbolic(Invariant::of(limit)));
        assert!(assumptions.contains(&Assumption::Approaching), "{assumptions:?}");
        assert!(assumptions.contains(&Assumption::StrictOverflow), "{assumptions:?}");
        assert_eq!(found.proven(), None);
    }

    #[test]
    fn the_count_records_which_reading_its_test_took() {
        // What a consumer widening a symbolic count has to know. The limit is a value of the
        // counter's type and which number that value is depends on how its test read it.
        let signed = counted(Type::int(32), 0, 100, 1, IntPred::Slt, Flags::NSW);
        assert_eq!(bound(&signed.func).expect("it is counted").reading(), Reading::Signed);
        let unsigned = counted(Type::int(32), 0, 100, 1, IntPred::Ult, Flags::NUW);
        assert_eq!(bound(&unsigned.func).expect("it is counted").reading(), Reading::Unsigned);
    }

    #[test]
    fn a_counter_without_a_no_wrap_promise_carries_the_assumption_instead() {
        // An inclusive test, because the strict one is the case the test itself answers. Under
        // `<=` the counter reaches the limit and is stepped once more, so a limit at the top of
        // the type makes that last step the one that wraps and nothing here rules it out.
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Ule, Flags::NONE);
        let found = bound(&it.func).expect("it is counted");
        let (_, assumptions) = found.parts();
        assert!(assumptions.iter().any(|a| matches!(a, Assumption::NoWrap(_))), "{assumptions:?}");
    }

    #[test]
    fn an_unsigned_counter_stepping_by_one_is_held_by_its_own_test() {
        // `for (unsigned i = 0; i < n; i++)` written out. Unsigned arithmetic wraps in C so the
        // increment carries no `nuw`, and without reading the test this would rest on an
        // assumption nothing downstream can discharge.
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Ult, Flags::NONE);
        let found = bound(&it.func).expect("it is counted");
        assert_eq!(found.assumptions(), &[]);
        assert_eq!(found.proven(), Some(Count::Exact(100)));
    }

    #[test]
    fn counting_down_by_one_is_held_the_same_way() {
        let it = counted(Type::int(32), 100, 0, -1, IntPred::Ugt, Flags::NONE);
        let found = bound(&it.func).expect("it is counted");
        assert_eq!(found.assumptions(), &[]);
        assert_eq!(found.proven(), Some(Count::Exact(100)));
    }

    #[test]
    fn a_step_of_two_can_jump_the_limit_so_the_test_holds_nothing() {
        // The counter is never at the limit, so the loop can be left by a step that goes from one
        // below the limit to one past the top of the type and comes back round at the bottom.
        let it = counted(Type::int(32), 0, 100, 2, IntPred::Ult, Flags::NONE);
        let found = bound(&it.func).expect("it is counted");
        let (_, assumptions) = found.parts();
        assert!(assumptions.iter().any(|a| matches!(a, Assumption::NoWrap(_))), "{assumptions:?}");
    }

    #[test]
    fn a_test_the_counter_can_be_stepped_without_being_asked_holds_nothing_either() {
        // ```text
        // header(i): br_if flag, check, latch
        // check:     br_if i <u 100, latch, exit
        // latch:     jump header(i + 1)
        // ```
        // The counter goes round by a path that never reaches the test, so the test says nothing
        // about how far the counter got.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let check = func.create_block();
        let latch = func.create_block();
        let exit = func.create_block();
        let flag = func.append_param(entry, Type::int(1));
        let counter = func.append_param(header, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(header, &[zero]);
        let mut build = Builder::new(&mut func, header);
        build.br_if(flag, check, &[], latch, &[]);
        let mut build = Builder::new(&mut func, check);
        let limit = build.iconst(Type::int(32), 100);
        let test = build.icmp(IntPred::Ult, counter, limit);
        build.br_if(test, latch, &[], exit, &[]);
        let mut build = Builder::new(&mut func, latch);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NONE);
        build.jump(header, &[next]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);

        let found = bound(&func).expect("it is counted");
        let (_, assumptions) = found.parts();
        assert!(assumptions.iter().any(|a| matches!(a, Assumption::NoWrap(_))), "{assumptions:?}");
    }

    #[test]
    fn a_test_that_ends_the_loop_when_it_succeeds_is_read_the_other_way_round() {
        // `for (i = 0; ; i++) if (i >= 100) break;`, which is the same loop with the arms of the
        // branch swapped. The test that keeps the loop going is the opposite of the one written.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();
        let counter = func.append_param(header, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(header, &[zero]);
        let mut build = Builder::new(&mut func, header);
        let limit = build.iconst(Type::int(32), 100);
        let done = build.icmp(IntPred::Sge, counter, limit);
        build.br_if(done, exit, &[], body, &[]);
        let mut build = Builder::new(&mut func, body);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        build.jump(header, &[next]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);

        let (count, _) = bound(&func).expect("it is counted").parts();
        assert_eq!(count, Count::Exact(100));
    }

    #[test]
    fn an_unsigned_limit_past_the_middle_of_its_type_is_not_a_negative_one() {
        // `for (unsigned char i = 0; i < 200; i++)`. Two hundred does not fit in a signed byte
        // and the constant is held as minus fifty six, so a distance taken at face value is
        // negative and reads as a loop that runs no times.
        let it = counted(Type::int(8), 0, 200, 1, IntPred::Ult, Flags::NUW);
        let found = bound(&it.func).expect("it is counted");
        assert_eq!(found.proven(), Some(Count::Exact(200)));
    }

    #[test]
    fn a_walk_that_lands_on_a_not_equal_limit_exactly_is_counted() {
        // `while (i != 10)` counting by one, which is `while (p != end)` over an array once the
        // element size has been divided out. `!=` says nothing about how its operands are read,
        // so the promise it wants is the unsigned one and an `nsw` on its own is not enough.
        let it = counted(Type::int(32), 0, 10, 1, IntPred::Ne, Flags::NSW.union(Flags::NUW));
        let found = bound(&it.func).expect("it lands on its limit");
        // The step divides the distance and both are numbers, so it was checked rather than
        // assumed and there is nothing left over.
        assert_eq!(found.proven(), Some(Count::Exact(10)));
    }

    #[test]
    fn a_counter_stepping_away_from_a_not_equal_limit_is_not_a_loop_that_runs_no_times() {
        // The distance is negative and an ordering test would read that as the loop never being
        // entered. `!=` reads it as the counter never arriving, which is an endless loop, and
        // answering zero for it was a real bug that the property test in `tests/scev.rs` found.
        let it = counted(Type::int(32), 48, 15, 1, IntPred::Ne, Flags::NSW);
        assert_eq!(bound(&it.func), None);
    }

    #[test]
    fn a_counter_stepping_over_a_not_equal_limit_never_arrives_either() {
        // Zero, three, six, nine, twelve, and ten is never one of them. An ordering test would
        // have stopped at twelve.
        let it = counted(Type::int(32), 0, 10, 3, IntPred::Ne, Flags::NSW);
        assert_eq!(bound(&it.func), None);
    }

    #[test]
    fn an_estimate_is_the_count_when_there_is_one_and_a_guess_when_there_is_not() {
        let counted_loop = counted(Type::int(32), 0, 7, 1, IntPred::Slt, Flags::NSW);
        let (cfg, loops) = analyse(&counted_loop.func);
        let id = loops.roots()[0];
        let estimate = Scev::new(&counted_loop.func, &cfg, &loops).estimate(id);
        assert_eq!(estimate.iterations(), 7);
        assert!(!estimate.is_guess());

        // A loop this cannot count still has to answer, because the caller is deciding whether
        // something is worth doing rather than whether it is legal.
        let uncounted = counted(Type::int(32), 0, 100, 0, IntPred::Slt, Flags::NSW);
        let (cfg, loops) = analyse(&uncounted.func);
        let id = loops.roots()[0];
        let estimate = Scev::new(&uncounted.func, &cfg, &loops).estimate(id);
        assert!(estimate.is_guess());
        assert_eq!(estimate.iterations(), super::ASSUMED_ITERATIONS);
    }

    #[test]
    fn a_value_the_loop_does_not_touch_is_invariant_rather_than_unknown() {
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Slt, Flags::NSW);
        let (cfg, loops) = analyse(&it.func);
        let id = loops.roots()[0];
        let mut scev = Scev::new(&it.func, &cfg, &loops);
        // The counter's start is an `iconst` in the entry block, which is both.
        assert_eq!(
            scev.evolution(id, it.counter).chrec().expect("it evolves").base,
            Invariant::number(0)
        );
    }

    #[test]
    fn a_back_edge_of_its_own_does_not_hide_the_counter() {
        // What canonicalization leaves behind. The back edge goes through a block that does nothing
        // but pass the increment on, so the value arriving at the header is a parameter of that
        // block rather than the increment itself. Reading through it is undoing a rename and not an
        // analysis, and without it the trip count of every loop the pipeline produces is nothing.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let latch = func.create_block();
        let exit = func.create_block();
        let counter = func.append_param(header, Type::int(32));
        let carried = func.append_param(latch, Type::int(32));

        let start = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(header, &[start]);

        let mut build = Builder::new(&mut func, header);
        let limit = build.iconst(Type::int(32), 100);
        let test = build.icmp(IntPred::Slt, counter, limit);
        build.br_if(test, body, &[], exit, &[]);

        let mut build = Builder::new(&mut func, body);
        let by = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, counter, by, Flags::NSW);
        build.jump(latch, &[next]);

        Builder::new(&mut func, latch).jump(header, &[carried]);
        Builder::new(&mut func, exit).ret(&[]);

        let chrec = evolution(&func, counter).chrec().expect("the counter still evolves");
        assert_eq!(chrec.base, Invariant::number(0));
        assert_eq!(chrec.step, Invariant::number(1));
        let (count, _) = bound(&func).expect("it is still counted").parts();
        assert_eq!(count, Count::Exact(100));
    }

    #[test]
    fn every_assumption_says_what_it_is_in_a_line() {
        let it = counted(Type::int(8), 0, 100, 1, IntPred::Ult, Flags::NONE);
        let found = bound(&it.func).expect("it is counted");
        for assumption in found.assumptions() {
            let line = assumption.describe();
            assert!(!line.is_empty());
            assert!(!line.contains('\n'), "an assumption is one line: {line}");
        }
    }
}
