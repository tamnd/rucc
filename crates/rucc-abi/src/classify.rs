//! The one classifier every ABI is run through, and the four mechanisms it is built from.
//!
//! Design: `spec/cross-compile/06-abis.md` section 6.7.
//!
//! [`crate::describe`] argues for the split between mechanism and policy. This is the mechanism
//! half. There are four things in here that look inside an aggregate, and every ABI in
//! [`crate::abis`] is one of them plus a size rule plus an order.
//!
//! # Ask about the return value first
//!
//! On three of the five ABIs described here, a return value that comes back in memory takes an
//! argument register with it on the way past, so a function returning a large structure has one
//! argument register fewer than the same function returning `int`. Classifying the arguments
//! before the return value gives a different answer for the last argument, and it is a different
//! answer rather than an error, which is the worst kind.
//!
//! [`Call::returns`] therefore comes first and [`Call::argument`] is asked once per argument in
//! source order. Asking out of order answers for a different program.

use crate::describe::{
    AbiDescription, Banks, ReturnPointer, Rule, Scalars, Short, Test, Travel, Variadic,
};
use crate::shape::{Arg, Format, Kind, Pass, Scalar, Shape, Slot};

/// The registers one call has left.
///
/// Made by [`AbiDescription::call`], asked about the return value first and then about each
/// argument in order.
#[derive(Debug, Clone)]
pub struct Call {
    /// The ABI being followed.
    abi: &'static AbiDescription,
    /// General purpose argument registers left. On an ABI whose banks are shared this is the
    /// argument positions left, since there both kinds of register share them.
    integer: u32,
    /// Floating point argument registers left.
    float: u32,
}

impl AbiDescription {
    /// The start of one call, with every argument register still to spend.
    #[must_use]
    pub const fn call(&'static self) -> Call {
        Call { abi: self, integer: self.banks.integer, float: self.banks.float }
    }
}

impl Call {
    /// The ABI this call follows.
    #[must_use]
    pub const fn abi(&self) -> &'static AbiDescription {
        self.abi
    }

    /// General purpose argument registers left, which is what a test asserts about draining.
    #[must_use]
    pub const fn integer_left(&self) -> u32 {
        self.integer
    }

    /// Floating point argument registers left.
    #[must_use]
    pub const fn float_left(&self) -> u32 {
        self.float
    }

    /// How the return value comes back, which is asked before anything else.
    #[must_use]
    pub fn returns(&mut self, arg: &Arg<'_>) -> Pass {
        let shape = match arg {
            // A returned scalar comes back in the first register of its bank and spends nothing,
            // because the registers a return value uses are not the ones arguments use.
            Arg::Void => return Pass::Ignore,
            Arg::Scalar(_) => return Pass::Direct,
            Arg::Aggregate(shape) => *shape,
        };
        self.apply(self.abi.returns, &shape, true)
    }

    /// How the next fixed argument travels, which spends whatever registers it takes.
    #[must_use]
    pub fn argument(&mut self, arg: &Arg<'_>) -> Pass {
        let shape = match arg {
            Arg::Void => return Pass::Ignore,
            Arg::Scalar(scalar) => return self.scalar(*scalar),
            Arg::Aggregate(shape) => *shape,
        };
        self.apply(self.abi.arguments, &shape, false)
    }

    /// How the next argument past the `...` travels.
    ///
    /// Only one of the three policies changes the answer this crate gives. Under
    /// [`Variadic::AlwaysMemory`] the argument is classified as though no argument registers were
    /// left, which is Darwin arm64's rule stated in the one form that needs no new mechanism.
    /// [`Variadic::BothBanks`] is a fact about which registers the backend has to write, not
    /// about the form the value travels in, so the answer here is the same as for a fixed
    /// argument and the description carries the flag for the backend to read.
    #[must_use]
    pub fn variadic_argument(&mut self, arg: &Arg<'_>) -> Pass {
        match self.abi.variadic {
            Variadic::SameAsFixed | Variadic::BothBanks => self.argument(arg),
            Variadic::AlwaysMemory => {
                let shape = match arg {
                    Arg::Void => return Pass::Ignore,
                    // On the stack, in the same form, spending nothing.
                    Arg::Scalar(_) => return Pass::Direct,
                    Arg::Aggregate(shape) => *shape,
                };
                // A scratch call with nothing left. Every rule that wanted a register runs
                // short, which is exactly what "always on the stack" means, and the real banks
                // are untouched because a variadic argument does not spend one.
                let mut empty = Self { abi: self.abi, integer: 0, float: 0 };
                empty.apply(self.abi.arguments, &shape, false)
            }
        }
    }

    /// How a scalar argument travels, which is always as itself, and what it costs.
    fn scalar(&mut self, scalar: Scalar) -> Pass {
        let Banks { shared, integer_width, float_width, .. } = self.abi.banks;
        let Scalars { in_memory, wide_integer_is_all_or_nothing } = self.abi.scalars;
        // A `long double` argument on SysV is in the argument area and there is no register file
        // it could have gone in, so it costs nothing and leaves the banks alone.
        if matches!(scalar.kind, Kind::Float(format) if Some(format) == in_memory) {
            return Pass::Direct;
        }
        let want = registers(scalar.size, integer_width);
        match scalar.kind {
            // Shared banks mean there is one sequence of positions and every value takes the
            // next one, whichever kind of register it ends up in.
            _ if shared => self.integer = self.integer.saturating_sub(1),
            Kind::Float(_) if scalar.size <= float_width => {
                self.float = self.float.saturating_sub(1);
            }
            // Wider than a vector register holds, which is a `long double` on RISC-V LP64D. It
            // travels in general purpose registers like an integer of the same size.
            Kind::Float(_) => self.integer = self.integer.saturating_sub(want),
            Kind::Integer if wide_integer_is_all_or_nothing => {
                if want <= self.integer {
                    self.integer -= want;
                }
            }
            Kind::Integer => self.integer = self.integer.saturating_sub(want),
        }
        Pass::Direct
    }

    /// The first rule whose test matches, with what it costs applied.
    fn apply(&mut self, rules: &'static [Rule], shape: &Shape<'_>, returning: bool) -> Pass {
        for rule in rules {
            let Some(found) = self.matches(rule.when, shape) else { continue };
            match self.travel(rule, &found, shape, returning) {
                Some(pass) => return pass,
                // The rule ran short of registers and said to try the next one.
                None => continue,
            }
        }
        // A description whose last rule is not `Test::Anything` has a hole in it, and the test
        // in `abis.rs` is what stops one being written. Reaching here means that test is gone.
        unreachable!("every rule list ends with a rule that matches anything")
    }

    /// Whether a test matches, and the slots it found if it is one of the tests that looks
    /// inside.
    fn matches(&self, test: Test, shape: &Shape<'_>) -> Option<Vec<Slot>> {
        let Banks { integer_width, float_width, .. } = self.abi.banks;
        match test {
            Test::Anything => Some(Vec::new()),
            Test::Empty => (shape.size == 0).then(Vec::new),
            Test::SizeOneOf(sizes) => sizes.contains(&shape.size).then(Vec::new),
            Test::SizeAtMost(limit) => (shape.size <= limit).then(Vec::new),
            Test::Homogeneous { limit } => homogeneous(shape, limit),
            Test::FloatPair => float_pair(shape, integer_width, float_width),
            Test::X87Stack => x87_stack(shape),
            Test::Eightbytes { limit } => eightbytes(shape, limit),
        }
    }

    /// The pass a matched rule produces, and [`None`] if it ran short and said to try the next
    /// rule.
    fn travel(
        &mut self,
        rule: &Rule,
        found: &[Slot],
        shape: &Shape<'_>,
        returning: bool,
    ) -> Option<Pass> {
        let width = self.abi.banks.integer_width;
        let slots = match rule.then {
            Travel::Ignore => return Some(Pass::Ignore),
            Travel::InMemory => return Some(Pass::Memory),
            Travel::ByReference => {
                // As an argument the address is one more argument. As a return value it is
                // whichever register this ABI reserves for the purpose, and on AAPCS64 that is
                // not an argument register at all.
                if !returning || self.abi.return_pointer == ReturnPointer::FirstArgument {
                    self.integer = self.integer.saturating_sub(1);
                }
                return Some(Pass::Reference);
            }
            Travel::AsFound => found.to_vec(),
            Travel::AsIntegers => integer_slots(shape.size, width),
            Travel::AsOneInteger => {
                vec![Slot::Integer { offset: 0, size: u32::try_from(shape.size).unwrap_or(8) }]
            }
        };
        // A return value in registers spends nothing: the registers a value comes back in are
        // not the ones arguments go out in.
        if returning {
            return Some(Pass::Pieces(slots));
        }
        let (integer, float) = self.cost(&slots);
        if integer <= self.integer && float <= self.float {
            self.integer -= integer;
            self.float -= float;
            return Some(Pass::Pieces(slots));
        }
        match rule.short {
            Short::Unchanged => {
                self.integer = self.integer.saturating_sub(integer);
                self.float = self.float.saturating_sub(float);
                Some(Pass::Pieces(slots))
            }
            Short::Memory => Some(Pass::Memory),
            Short::MemoryAndDrain => {
                // Whichever bank it could not be served from is spent, so that nothing after it
                // gets a register the ABI would have had to skip over.
                if integer > self.integer {
                    self.integer = 0;
                }
                if float > self.float {
                    self.float = 0;
                }
                Some(Pass::Memory)
            }
            Short::TryNextRule => None,
        }
    }

    /// What a run of slots costs, as general purpose registers and then vector registers.
    fn cost(&self, slots: &[Slot]) -> (u32, u32) {
        let count = u32::try_from(slots.len()).unwrap_or(u32::MAX);
        if self.abi.banks.shared {
            // One position per register's worth, whichever bank it lands in.
            return (count, 0);
        }
        let float =
            u32::try_from(slots.iter().filter(|slot| slot.is_float()).count()).unwrap_or(u32::MAX);
        (count - float, float)
    }
}

/// How many registers of this width a value of this size takes, which is at least one.
fn registers(size: u64, width: u64) -> u32 {
    u32::try_from(size.div_ceil(width.max(1))).unwrap_or(1).max(1)
}

/// An object of this size as a run of integer registers, the last one holding only what is left.
///
/// The last slot being narrow is not tidiness. A twelve byte structure at the end of a page is
/// twelve readable bytes followed by four that are not, and a load of the full register width
/// there faults on a program that is correct.
fn integer_slots(size: u64, width: u64) -> Vec<Slot> {
    let width = width.max(1);
    (0..size.div_ceil(width))
        .map(|index| Slot::Integer {
            offset: index * width,
            size: u32::try_from((size - index * width).min(width)).unwrap_or(8),
        })
        .collect()
}

/// The vector registers of a homogeneous floating point aggregate, and [`None`] for anything
/// else.
///
/// Homogeneous means every scalar is the same floating point type once arrays and nested records
/// are flattened out, and that they fill the aggregate. The second half is what rules out
/// `struct { float a; char pad[8]; }`, which has one floating point member and is not an HFA,
/// and anything a zero width bit-field has stretched.
fn homogeneous(shape: &Shape<'_>, limit: usize) -> Option<Vec<Slot>> {
    let first = shape.pieces.first()?;
    let Kind::Float(format) = first.scalar.kind else { return None };
    let count = shape.pieces.len();
    if count > limit || shape.pieces.iter().any(|piece| piece.scalar != first.scalar) {
        return None;
    }
    let fills = first.scalar.size.checked_mul(count as u64) == Some(shape.size);
    fills.then(|| {
        shape.pieces.iter().map(|piece| Slot::Float { offset: piece.offset, format }).collect()
    })
}

/// The registers a one or two member aggregate travels in under the RISC-V floating point rule,
/// and [`None`] for one the rule does not reach.
///
/// A member wider than a floating point register is not a floating point member for this
/// purpose, which is why a `long double` on LP64D makes the aggregate holding it an ordinary
/// integer pair.
fn float_pair(shape: &Shape<'_>, integer_width: u64, float_width: u64) -> Option<Vec<Slot>> {
    let slot = |piece: &crate::shape::Piece| match piece.scalar.kind {
        Kind::Float(format) if piece.scalar.size <= float_width => {
            Some(Slot::Float { offset: piece.offset, format })
        }
        Kind::Integer if piece.scalar.size <= integer_width => Some(Slot::Integer {
            offset: piece.offset,
            size: u32::try_from(piece.scalar.size).ok()?,
        }),
        _ => None,
    };
    let floats = shape.pieces.iter().filter(|piece| piece.scalar.is_float()).count();
    match shape.pieces {
        // One floating point member, in the register the member itself would have used.
        [only] if floats == 1 => Some(vec![slot(only)?]),
        // Two members with at least one floating point member between them. Two integers are not
        // this: they are the ordinary size rule, and the ordinary size rule gives them the same
        // two registers anyway.
        [first, second] if floats > 0 => Some(vec![slot(first)?, slot(second)?]),
        _ => None,
    }
}

/// The x87 stack registers a `long double` or a `_Complex long double` comes back in, and
/// [`None`] for anything else.
fn x87_stack(shape: &Shape<'_>) -> Option<Vec<Slot>> {
    let one_value = shape.pieces.len() == 1 || (shape.pieces.len() == 2 && shape.complex);
    let all_x87 = shape.is_all_of(Format::X87Extended);
    (all_x87 && one_value).then(|| {
        shape
            .pieces
            .iter()
            .map(|piece| Slot::Float { offset: piece.offset, format: Format::X87Extended })
            .collect()
    })
}

/// The class of one eightbyte, section 3.2.3 of the SysV psABI.
///
/// SSEUP and X87UP are not here. Both mean "the continuation of the eightbyte before this one",
/// and the only two things that produce them are a vector wider than eight bytes, which is not
/// an aggregate and does not come through here, and a `long double`, whose two eightbytes are
/// treated as the one value they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Nothing reaches into it, which takes padding or an empty member.
    None,
    /// A general purpose register.
    Integer,
    /// A vector register.
    Sse,
    /// The x87 stack.
    X87,
    /// Memory, which takes the whole argument with it.
    Memory,
}

/// Two classes over one eightbyte, section 3.2.3's merge rule.
fn merge(left: Class, right: Class) -> Class {
    match (left, right) {
        (a, b) if a == b => a,
        (Class::None, other) | (other, Class::None) => other,
        (Class::Memory, _) | (_, Class::Memory) => Class::Memory,
        // An x87 value shares an eightbyte with something else only in a packed record, and
        // there is no way to pass the two of them together.
        (Class::X87, _) | (_, Class::X87) => Class::Memory,
        // The rule that surprises people: one `int` in an eightbyte sends the `float` beside it
        // into a general purpose register.
        (Class::Integer, _) | (_, Class::Integer) => Class::Integer,
        _ => Class::Sse,
    }
}

/// The slots the SysV classification produces, and [`None`] when the answer is memory.
///
/// x87 counts as memory here. As an argument that is the right answer directly, and as a return
/// value the x87 stack rule is a separate rule earlier in the list, so by the time this runs an
/// x87 class means the value goes back in memory either way.
fn eightbytes(shape: &Shape<'_>, limit: u64) -> Option<Vec<Slot>> {
    if shape.size > limit {
        return None;
    }
    let mut classes = vec![Class::None; usize::try_from(shape.size.div_ceil(8)).ok()?];
    for piece in shape.pieces {
        // A member away from its natural alignment is what `packed` makes, and it is the second
        // of the two things section 3.2.3 sends straight to memory.
        if piece.scalar.align > 1 && piece.offset % piece.scalar.align != 0 {
            return None;
        }
        let class = match piece.scalar.kind {
            Kind::Integer => Class::Integer,
            Kind::Float(Format::X87Extended) => Class::X87,
            Kind::Float(_) => Class::Sse,
        };
        for at in piece.offset / 8..=(piece.end() - 1) / 8 {
            let slot = classes.get_mut(usize::try_from(at).ok()?)?;
            *slot = merge(*slot, class);
        }
    }
    if classes.iter().any(|class| matches!(class, Class::Memory | Class::X87)) {
        return None;
    }
    Some(
        classes
            .iter()
            .enumerate()
            .map(|(index, class)| {
                let offset = index as u64 * 8;
                let bytes = (shape.size - offset).min(8);
                match class {
                    // Four bytes or fewer of floating point is one `float`. More than that is a
                    // `double` or two `float`s, which arrive in the same register either way.
                    Class::Sse if bytes <= 4 => Slot::Float { offset, format: Format::Single },
                    Class::Sse => Slot::Float { offset, format: Format::Double },
                    // An eightbyte nothing reaches into still travels, and it travels in a
                    // general purpose register, because an ABI does not leave a hole in the
                    // middle of an argument.
                    _ => Slot::Integer { offset, size: u32::try_from(bytes).unwrap_or(8) },
                }
            })
            .collect(),
    )
}
