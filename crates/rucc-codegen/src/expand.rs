//! The IR rewrites the machine needs before a rule can be asked anything.
//!
//! Design: `spec/10-backend.md` section 10.2, which is where the ordering comes from.
//!
//! Everything else in this crate turns an instruction into instructions. There are two things a
//! rule cannot do, and what is done about each of them instead is here.
//!
//! The first is a new shape of control flow. A rule replaces a term with a term and the
//! replacement has nowhere to put a block, so a construct that becomes blocks has to be rewritten
//! before selection rather than during it. There is one such construct today and it is `switch`.
//! Every other terminator leaves a block with one successor or two, which is what the block layout
//! writes jumps for, and a `switch` leaves it with as many as the program had cases.
//!
//! The second is arithmetic on what a rule matched. A rule may name a constant and pass it along,
//! and it may not add to one or read it as something else, because the pattern language is a
//! pattern language and giving it a way to compute would make a rule set a program the solver has
//! to reason about rather than a table it can check a line of at a time. So an instruction whose
//! lowering needs a value worked out from another one is rewritten here into instructions whose
//! lowerings do not. Four of them are floats: a float constant, a negation, and the two conversions
//! between a float and an unsigned integer. The other two move a block of memory, where the
//! arithmetic is the offset of each word from the front of it.
//!
//! # Why a copy is a run of moves and not a call
//!
//! A `memcpy` in the IR is not a call to `memcpy`. It is what the front end writes for a structure
//! assigned, passed or returned by value, and a `memset` is what it writes for the part of an
//! object an initialiser left unnamed, so a program with a `struct` in it reaches one almost at
//! once and the size is a constant every time.
//!
//! A constant size is what makes the moves the right answer. A four byte copy written as a call
//! costs the call and the two arguments and gives back four bytes moved, which is more instructions
//! than the move it replaced and slower than all of them. Every real compiler writes the moves
//! under some threshold for that reason, and above the threshold writes the call, which is where
//! this stops: the call needs a `memcpy` to exist, and a statically linked program has nowhere to
//! get one from until the compiler runtime in tamnd/rucc#277 exists. So a copy larger than the
//! threshold is refused by name rather than written wrong.
//!
//! # Why the chain a `switch` becomes is the backend's and not the front end's
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

use std::cmp::Ordering;
use std::collections::HashMap;

use rucc_base::Interner;
use rucc_ir::{
    BlockCall, Builder, CallInfo, Def, Extra, Flags, FloatPred, Func, Imm, Inst, InstData, IntPred,
    MemInfo, MemOrder, Opcode, Signature, Type, Value,
};

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

/// Rewrites every ordered access into the plain access this machine already makes ordered, and
/// leaves a barrier where the machine needs one.
///
/// This is the one pass here whose reason is a memory model rather than a missing instruction, so
/// it is worth writing down what the model says. x86-64 is total store order. Every load is an
/// acquire, every store is a release, and an aligned access no wider than a word is indivisible
/// whether or not anybody asked for one. So an `atomic_load` at any ordering is the same `mov` a
/// `load` is, and so is an `atomic_store` at every ordering except the strongest, and rewriting
/// them into the plain access is not an approximation: it is the whole of what the machine does.
///
/// The one thing total store order does not give is a store followed by a load of a different
/// address staying in that order, and that is exactly what sequential consistency is missing. So a
/// sequentially consistent store is the same `mov` with an `mfence` behind it, which is the pair
/// gcc 16.2.0 writes. The fence is left in the IR as a `fence` rather than written here, because
/// what a barrier costs is a target question and [`crate::lower`] is where the target answers are.
///
/// A `fence` the program wrote is left alone for the same reason. Every ordering below the
/// strongest is nothing at all on this machine and the strongest is one instruction, and both of
/// those are decided by name in [`crate::lower`] where the instruction lives.
///
/// # Why the width is checked
///
/// An access is only indivisible if the machine can do it in one go, which here means one, two,
/// four or eight bytes at an address aligned to its own width. Anything else is a run of accesses
/// and a run of accesses is not atomic at all, so it is left as the opcode it was and no rule
/// covers it, which is a compile error naming the instruction. That is the right answer: an
/// atomic access the machine cannot make atomic has no correct lowering, and a wrong one that
/// looks right is worse than a refusal. C says the same thing through `__atomic_is_lock_free`.
///
/// `word` is how many bytes the widest indivisible access carries, which is the same number the
/// widest move carries and is read from the machine for the reason [`bulk`] reads it.
///
/// This runs before every other pass here, so that what it produces is an ordinary load or store
/// that the width legalisation and everything after it get to see. An ordered access at a width the
/// machine has no register for would otherwise be a shape nothing later understands, since every
/// pass after this one is written about `load` and `store` by name.
pub fn orderings(func: &mut Func, word: u32) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::AtomicLoad => relaxed(func, inst, Opcode::Load, word),
            Opcode::AtomicStore => relaxed(func, inst, Opcode::Store, word),
            _ => {}
        }
    }
}

/// One ordered access as the plain one, with a barrier behind it when the ordering asked for more
/// than the machine gives for free.
///
/// The ordering is taken off the access rather than left on it, because the IR verifier refuses an
/// ordering on a plain access, and it refuses one for a good reason: a plain load may be moved,
/// duplicated and dropped, and an ordering that survived on one would be a claim nothing downstream
/// honours. What is true after this pass is that the ordering has been discharged, and the way to
/// say that is to stop carrying it.
///
/// A sequentially consistent store becomes the store and then the fence, and it is built that way
/// round: the plain store is put in front of the instruction and the instruction itself becomes the
/// fence. Doing it the other way would need somewhere to insert behind an instruction, and there is
/// nothing to gain from having two ways to insert.
fn relaxed(func: &mut Func, inst: Inst, plain: Opcode, word: u32) {
    let Extra::Mem(mem) = func[inst].extra else { return };
    let info = func[mem];
    let ty = match plain {
        Opcode::Store => match func[func[inst].args].first() {
            Some(&value) => func[value].ty,
            None => return,
        },
        _ => produced(func, inst),
    };
    if !indivisible(ty, info, word) {
        return;
    }
    let unordered = MemInfo { order: MemOrder::NotAtomic, ..info };

    if plain == Opcode::Store && info.order == MemOrder::SeqCst {
        let [value, addr] = func[func[inst].args] else { return };
        write(func, inst, value, addr, unordered);
        let none = func.push_values(&[]);
        let data = &mut func[inst];
        data.opcode = Opcode::Fence;
        data.args = none;
        data.extra = Extra::Order(MemOrder::SeqCst);
        data.flags = data.flags.intersection(Flags::legal_on(Opcode::Fence));
        return;
    }

    let plainly = func.add_mem(unordered);
    let data = &mut func[inst];
    data.opcode = plain;
    data.extra = Extra::Mem(plainly);
    data.flags = data.flags.intersection(Flags::legal_on(plain));
}

/// Whether this machine does an access of this type in one go.
///
/// One, two, four or eight bytes, at an address aligned to at least that many. The alignment is the
/// front end's answer for the type being accessed, which for every type C can spell is its own
/// width, so what this actually refuses is a `long double` and an access the program underaligned
/// on purpose.
///
/// The width is the storage the value takes and not the bits it holds, because that is what the
/// access moves. A `bool` is one bit of value in one byte of memory and one byte is indivisible, so
/// rounding up is what makes an ordered access of one work rather than a refusal nobody wanted. An
/// address is the exception the other way: the IR gives a pointer no width at all, since how wide
/// one is belongs to the target, so the target's number is used for it.
fn indivisible(ty: Type, info: MemInfo, word: u32) -> bool {
    let bytes = if ty.is_ptr() { word } else { ty.bits().div_ceil(8) };
    ty.is_scalar() && bytes.is_power_of_two() && bytes <= word && info.align >= bytes
}

/// Rewrites the float instructions no rule can be written for, and leaves the rest alone.
///
/// Each of them needs a value worked out from one the pattern matched, which is the one thing the
/// rule language deliberately cannot do. A float constant is an integer constant read as a float,
/// and reading it is arithmetic on the immediate. A negation is an exclusive or with a mask that
/// depends on the format. A conversion between a float and an integer is that conversion at a
/// width the machine has, which is a width neither the pattern nor the replacement can work out.
///
/// What is left after this is a function whose float instructions are each one machine
/// instruction, so what a rule is asked stays a table. The conversions between a float and an
/// unsigned sixty four bit integer are the two that are not a widening or a narrowing away from a
/// signed one, because there is no signed width that holds those values, and each gets a rewrite
/// of its own below.
pub fn floats(func: &mut Func) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::FConst => constant(func, inst),
            Opcode::FNeg => negate(func, inst),
            Opcode::SIToFP | Opcode::UIToFP => widen_then_convert(func, inst),
            Opcode::FPToSI | Opcode::FPToUI => convert_then_narrow(func, inst),
            _ => {}
        }
    }
}

/// A float constant, as the integer that spells it and a reading of those bits as the float.
///
/// This is the whole of what a `movsd` from a literal would be if there were a section to put the
/// literal in, and there is not one yet. Two instructions in a register beats a constant pool that
/// nothing else needs, and it is exactly what the bits of the immediate already say, since the IR
/// holds a float constant as its bit pattern rather than as a number.
fn constant(func: &mut Func, inst: Inst) {
    let ty = produced(func, inst);
    let Extra::Imm(imm) = func[inst].extra else { return };
    if !ty.is_float() || !ty.is_scalar() {
        return;
    }
    let int = Type::int(ty.bits());
    let bits = func[imm].bits();
    // The cast is the bits as they are stored, and `Imm::int` keeps the width, so a constant whose
    // top bit is set stays the negative integer that spells it rather than becoming a wider one.
    let spelled = ahead_const(func, inst, Imm::int(bits as i128, int), int);
    becomes(func, inst, Opcode::Bitcast, &[spelled]);
}

/// A negation, as an exclusive or with the sign bit.
///
/// C says negation flips the sign and says nothing else about it, which is not what subtracting
/// from zero does to a zero or to a not a number, so this is the operation the IR already calls
/// out as not being `0 - x`. Flipping the bit is the whole of it, and it is right for every value
/// a float can hold, the payload of a not a number included, because no other bit is touched.
///
/// The bit is flipped in a general purpose register rather than in the one the float is in. The
/// other way is one instruction rather than three and it wants the mask in memory aligned to the
/// register, which is the same section a constant pool would need.
fn negate(func: &mut Func, inst: Inst) {
    let ty = produced(func, inst);
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !ty.is_float() || !ty.is_scalar() {
        return;
    }
    let int = Type::int(ty.bits());
    let bits = ahead(func, inst, Opcode::Bitcast, &[arg], int);
    let mask = ahead_const(func, inst, Imm::int(1i128 << (ty.bits() - 1), int), int);
    let flipped = ahead(func, inst, Opcode::Xor, &[bits, mask], int);
    becomes(func, inst, Opcode::Bitcast, &[flipped]);
}

/// An integer becoming a float, as a widening and the signed conversion at a width there is one at.
///
/// The widening is with the sign for a signed integer and with zeroes for an unsigned one, and
/// after it the value is the same number in a signed integer the machine converts from, so the
/// conversion is the same value and the same rounding. That is the whole of why the machine needs
/// no unsigned conversion and none at a width narrower than an `int`.
fn widen_then_convert(func: &mut Func, inst: Inst) {
    let signed = func[inst].opcode == Opcode::SIToFP;
    let Some(&arg) = func[func[inst].args].first() else { return };
    let from = func[arg].ty;
    if !from.is_int() || !from.is_scalar() {
        return;
    }
    let Some(width) = holder(from.bits(), signed) else {
        from_unsigned_word(func, inst, arg, from);
        return;
    };
    if width == from.bits() {
        return;
    }
    let widen = if signed { Opcode::SExt } else { Opcode::ZExt };
    let wide = ahead(func, inst, widen, &[arg], Type::int(width));
    becomes(func, inst, Opcode::SIToFP, &[wide]);
}

/// A float becoming an integer, as the signed conversion at such a width and a narrowing.
///
/// The same argument the other way round. A float the program says fits in the integer it asked
/// for fits in the signed one that holds every value of it, so converting there and keeping the
/// low bits is that value however it is read, and a float that does not fit is undefined in C and
/// unspecified in the model at either width.
fn convert_then_narrow(func: &mut Func, inst: Inst) {
    let signed = func[inst].opcode == Opcode::FPToSI;
    let ty = produced(func, inst);
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !ty.is_int() || !ty.is_scalar() {
        return;
    }
    let Some(width) = holder(ty.bits(), signed) else {
        to_unsigned_word(func, inst, arg, ty);
        return;
    };
    if width == ty.bits() {
        return;
    }
    let wide = ahead(func, inst, Opcode::FPToSI, &[arg], Type::int(width));
    becomes(func, inst, Opcode::Trunc, &[wide]);
}

/// An unsigned sixty four bit integer becoming a float, without a branch.
///
/// This is the one conversion into a float that is not the signed one at some width, because there
/// is no signed width that holds every value of it. What the machine can do is the signed
/// conversion, so the value has to be brought under half of its range first and put back after.
///
/// Halving it is a shift, and a shift throws away the bit it shifts out, which is the difference
/// between a number that rounds up and one that rounds down. So the bit is put back as the lowest
/// bit of the half: a half that was exact stays exact, and one that was not comes out odd, which is
/// never a value the conversion rounds to and so never a value it rounds the wrong way from. That
/// is round to odd, and rounding to odd and then to nearest is the same answer as rounding to
/// nearest once, at every width a float here has. Doubling afterwards is exact, since a float
/// multiplied by two is the same digits with one more on the exponent and nothing here is near the
/// top of the range.
///
/// A value whose top bit is clear needs none of that and is the signed conversion as it stands, so
/// there are two answers and the machine has to pick one. gcc writes a branch. This writes the
/// choice as arithmetic, because a branch here would mean splitting the block this instruction is
/// in, and every rewrite in this pass stays inside one block. A mask that is every bit or no bit
/// picks the source, and the same mask over the bits of the result picks between doubling it and
/// adding a zero to it. That is more instructions than gcc's and no branch to predict wrong.
fn from_unsigned_word(func: &mut Func, inst: Inst, arg: Value, from: Type) {
    let ty = produced(func, inst);
    if !ty.is_float() || !ty.is_scalar() {
        return;
    }
    let spread = spread_top_bit(func, inst, arg, from);

    // The value halved, with the bit the halving lost put back as the lowest bit of it.
    let one = ahead_const(func, inst, Imm::int(1, from), from);
    let lost = ahead(func, inst, Opcode::And, &[arg, one], from);
    let half = ahead(func, inst, Opcode::LShr, &[arg, one], from);
    let odd = ahead(func, inst, Opcode::Or, &[half, lost], from);

    // The source, as the value with the difference between the two conditionally taken out of it.
    let differ = ahead(func, inst, Opcode::Xor, &[arg, odd], from);
    let taken = ahead(func, inst, Opcode::And, &[differ, spread], from);
    let source = ahead(func, inst, Opcode::Xor, &[arg, taken], from);
    let converted = ahead(func, inst, Opcode::SIToFP, &[source], ty);

    // The doubling, as the result added to itself or to a zero. The mask is the same one narrowed
    // to the width of the float, since the top bit it came from is a fact about the integer.
    let bits = Type::int(ty.bits());
    let narrow = same_width(func, inst, spread, from, bits);
    let raw = ahead(func, inst, Opcode::Bitcast, &[converted], bits);
    let again = ahead(func, inst, Opcode::And, &[raw, narrow], bits);
    let addend = ahead(func, inst, Opcode::Bitcast, &[again], ty);
    becomes(func, inst, Opcode::FAdd, &[converted, addend]);
}

/// A float becoming an unsigned sixty four bit integer, without a branch.
///
/// The same argument the other way round, and the same reason there is no branch. A float below
/// half the range converts as the signed one and is already the answer. One at or above it has half
/// the range subtracted first, which is exact because the two have the same exponent or a smaller
/// one, converts into the signed integer that now holds it, and gets the top bit put back on.
///
/// The subtraction is of a constant that is either half the range or a positive zero, which is the
/// same mask trick as above written over the bits of the float, and subtracting a positive zero
/// leaves every value alone including a negative zero. A float too big for the answer, or a not a
/// number, is undefined in C and unspecified in the model, so the comparison being false for a not
/// a number costs nothing: it takes the path whose answer was never promised either way.
fn to_unsigned_word(func: &mut Func, inst: Inst, arg: Value, ty: Type) {
    let from = func[arg].ty;
    if !from.is_float() || !from.is_scalar() {
        return;
    }
    // Half the range, as the float that spells it and the bits that spell the float.
    let bits = Type::int(from.bits());
    let pattern = Imm::int(half_the_range(from.bits()), bits);
    let spelled = ahead_const(func, inst, pattern, bits);
    let half = ahead(func, inst, Opcode::Bitcast, &[spelled], from);

    let over = ahead_cmp(func, inst, Opcode::FCmp, Extra::FloatPred(FloatPred::Oge), &[arg, half]);
    let wide = ahead(func, inst, Opcode::ZExt, &[over], bits);
    let zero = ahead_const(func, inst, Imm::int(0, bits), bits);
    let spread = ahead(func, inst, Opcode::Sub, &[zero, wide], bits);

    let amount = ahead(func, inst, Opcode::And, &[spread, spelled], bits);
    let taken = ahead(func, inst, Opcode::Bitcast, &[amount], from);
    let under = ahead(func, inst, Opcode::FSub, &[arg, taken], from);
    let low = ahead(func, inst, Opcode::FPToSI, &[under], ty);

    // The top bit back on, from the same comparison at the width of the answer.
    let again = ahead(func, inst, Opcode::ZExt, &[over], ty);
    let up = ahead_const(func, inst, Imm::int(i128::from(ty.bits() - 1), ty), ty);
    let top = ahead(func, inst, Opcode::Shl, &[again, up], ty);
    becomes(func, inst, Opcode::Xor, &[low, top]);
}

/// The top bit of an integer spread over every bit of one, which is every bit or no bit.
///
/// A comparison against zero rather than a shift, because the answer wanted is a mask and the
/// machine writes a mask out of a condition the same way either way, and the comparison says what
/// the question was.
fn spread_top_bit(func: &mut Func, inst: Inst, arg: Value, ty: Type) -> Value {
    let zero = ahead_const(func, inst, Imm::int(0, ty), ty);
    let set = ahead_cmp(func, inst, Opcode::ICmp, Extra::IntPred(IntPred::Slt), &[arg, zero]);
    let wide = ahead(func, inst, Opcode::ZExt, &[set], ty);
    ahead(func, inst, Opcode::Sub, &[zero, wide], ty)
}

/// A value brought to another integer width, and itself when the two are already the same.
fn same_width(func: &mut Func, inst: Inst, value: Value, from: Type, to: Type) -> Value {
    match to.bits().cmp(&from.bits()) {
        Ordering::Equal => value,
        Ordering::Less => ahead(func, inst, Opcode::Trunc, &[value], to),
        Ordering::Greater => ahead(func, inst, Opcode::SExt, &[value], to),
    }
}

/// The bits of the float of this width that is two to the sixty third.
///
/// Half of what an unsigned sixty four bit integer holds, which is the one number both conversions
/// above are written around. The exponent is biased and the significand is zero in both formats,
/// so it is the bias plus sixty three shifted up past the significand.
fn half_the_range(width: u32) -> i128 {
    match width {
        32 => 0x5F00_0000,
        _ => 0x43E0_0000_0000_0000,
    }
}

/// Rewrites every byte swap into the shifts and masks that are one, and leaves the rest alone.
///
/// A byte swap is a rule on a machine that has the instruction and this everywhere else, and until
/// `x64.bswap` is a term the model knows about, this is what x86-64 gets too. That is tamnd/rucc#307
/// and the whole of what is left of it: what is written below is correct at every width and slower
/// than the one instruction, which is the trade `spec/10-backend.md` section 10.3 says the fast path
/// makes everywhere.
///
/// It is here rather than in the front end because the masks are worked out from the width, and
/// arithmetic on a value a pattern matched is the one thing the rule language deliberately cannot
/// do. It is here rather than in the walk to the IR because a byte swap is one instruction in the
/// IR and should stay one for as long as anything is reading the IR, so that the day the rule
/// exists nothing above the backend has to change.
pub fn bytes(func: &mut Func) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        if func[inst].opcode == Opcode::Bswap {
            swap(func, inst);
        }
    }
}

/// One byte swap, as a halving run of swaps of adjacent groups of bits.
///
/// Reversing eight bytes is swapping the two halves, then the two halves of each half, then the two
/// bytes of each of those, and the three steps commute because each is a permutation of positions
/// the others do not touch. So the run goes from the widest group down to a byte, and every step is
/// the same five instructions: keep the even numbered groups, move them up, move the odd numbered
/// ones down, keep those, and put the two together.
///
/// Nine instructions for two bytes, seventeen for four, twenty five for eight, before the constants.
/// Writing it as a shift and a mask per byte instead is fewer steps to read and more instructions at
/// every width above two, since the cost there grows with the number of bytes rather than with the
/// logarithm of it.
///
/// A width that is not a whole number of bytes is left alone. The verifier does not allow one, and
/// silently reversing something else would be worse than the instruction surviving to a selector
/// that has no rule for it and says so.
fn swap(func: &mut Func, inst: Inst) {
    let ty = produced(func, inst);
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !ty.is_int() || !ty.is_scalar() || ty.bits() < 16 || ty.bits() % 8 != 0 {
        return;
    }

    let mut value = arg;
    let mut group = ty.bits() / 2;
    while group >= 8 {
        // The pattern that keeps every other run of `group` bits, counting the run at the bottom as
        // the first one kept. It is what says which half of each pair moves up and which moves down.
        let mask = alternating(ty.bits(), group);
        let keep = ahead_const(func, inst, Imm::int(mask, ty), ty);
        let count = ahead_const(func, inst, Imm::int(i128::from(group), ty), ty);
        let low = ahead(func, inst, Opcode::And, &[value, keep], ty);
        let up = ahead(func, inst, Opcode::Shl, &[low, count], ty);
        let down = ahead(func, inst, Opcode::LShr, &[value, count], ty);
        let high = ahead(func, inst, Opcode::And, &[down, keep], ty);
        // The last step of the last round is the instruction itself, so the value everything
        // downstream already reads is the answer and nothing has to be substituted.
        if group == 8 {
            becomes(func, inst, Opcode::Or, &[up, high]);
            return;
        }
        value = ahead(func, inst, Opcode::Or, &[up, high], ty);
        group /= 2;
    }
}

/// The mask that keeps every other run of `group` bits out of `width` of them, starting with the
/// run at the bottom.
///
/// Sixteen bits in groups of eight is `0x00ff`, thirty two in groups of eight is `0x00ff00ff`, and
/// thirty two in groups of sixteen is `0x0000ffff`. Built rather than written down because there is
/// one of these per width per group and a table of them is a table to get wrong.
///
/// The top group is always one of the dropped ones, since the run at the bottom is kept and the
/// width is an even number of groups, so the answer never has its sign bit set and reads the same
/// as a number as it does as a pattern.
fn alternating(width: u32, group: u32) -> i128 {
    every(width, group * 2, group)
}

/// The pattern with the low `run` bits of every `step` bit group set, out of `width` of them.
///
/// `every(32, 2, 1)` is `0x55555555` and `every(64, 8, 1)` is `0x0101010101010101`. Built rather
/// than written down for the reason the byte swap masks are: there is one of these per width per
/// group and a table of them is a table to get wrong.
///
/// The top group is never a full one when `run` is less than `step`, so the answer never has its
/// sign bit set and reads the same as a number as it does as a pattern.
fn every(width: u32, step: u32, run: u32) -> i128 {
    let ones = (1i128 << run) - 1;
    let mut mask = 0i128;
    let mut at = 0;
    while at < width {
        mask |= ones << at;
        at += step;
    }
    mask
}

/// Rewrites every bit count into the arithmetic that is one, and leaves the rest alone.
///
/// Three instructions and no rules, which is tamnd/rucc#310. `popcnt` is one instruction on a
/// machine that has it and `bsr` and `bsf` are the two searches, and none of the three is a term the
/// model knows about yet, so what runs today is what runs everywhere. The trade is the one
/// `spec/10-backend.md` section 10.3 describes and `expand::bytes` above makes for the same reason:
/// slower than the instruction, right on every target, and built only out of rules the verifier has
/// already discharged.
///
/// The two searches are rewritten first, into a set bit count and a little arithmetic, and then
/// every set bit count is rewritten. That is one pass rather than two because the second sweep picks
/// up what the first one wrote, and it means there is one place that knows how to count bits rather
/// than three.
pub fn counts(func: &mut Func) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::Ctlz => searched(func, inst, true),
            Opcode::Cttz => searched(func, inst, false),
            _ => {}
        }
    }
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        if func[inst].opcode == Opcode::Ctpop {
            counted(func, inst);
        }
    }
}

/// A leading or trailing zero count, as the set bit count of a value with those zeroes turned into
/// the only bits that are set.
///
/// For trailing zeroes that is `~x & (x - 1)`, which is exactly the run of zeroes below the lowest
/// set bit and nothing else, because `x - 1` sets that run and clears the bit above it while `~x`
/// keeps only positions `x` did not have.
///
/// For leading zeroes it is the same idea upside down. Smearing every set bit downwards, by folding
/// the value into itself shifted right by one, two, four and so on, leaves ones everywhere at or
/// below the highest set bit, so the complement is exactly the leading zeroes. That is five extra
/// steps at thirty two bits and six at sixty four, which is why the search instruction is worth
/// having and why #310 stays open for it.
///
/// Both answer the width for a zero argument, which is what they have to answer for `ffs` to be
/// masked correctly and is more than C asks for: `__builtin_clz(0)` and `__builtin_ctz(0)` are
/// undefined, so nothing may rely on this, and the point of writing it down is that it is defined
/// here rather than being whatever a register happened to hold.
fn searched(func: &mut Func, inst: Inst, leading: bool) {
    let ty = produced(func, inst);
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !countable(ty) {
        return;
    }
    let ones = ahead_const(func, inst, Imm::int(-1, ty), ty);
    if leading {
        let mut value = arg;
        let mut by = 1;
        while by < ty.bits() {
            let count = ahead_const(func, inst, Imm::int(i128::from(by), ty), ty);
            let down = ahead(func, inst, Opcode::LShr, &[value, count], ty);
            value = ahead(func, inst, Opcode::Or, &[value, down], ty);
            by *= 2;
        }
        let above = ahead(func, inst, Opcode::Xor, &[value, ones], ty);
        becomes(func, inst, Opcode::Ctpop, &[above]);
        return;
    }
    let missing = ahead(func, inst, Opcode::Xor, &[arg, ones], ty);
    let less = ahead(func, inst, Opcode::Add, &[arg, ones], ty);
    let below = ahead(func, inst, Opcode::And, &[missing, less], ty);
    becomes(func, inst, Opcode::Ctpop, &[below]);
}

/// One set bit count, as the halving sum every bit counting routine is written as.
///
/// Adjacent bits are added into pairs, pairs into nibbles, nibbles into bytes, and then the bytes
/// are added together at once by a multiply whose top byte is their sum. The first step is written
/// as a subtraction rather than as two masks and an add, which is the usual form and is one
/// instruction shorter: a two bit field minus its own high bit is the number of bits set in it.
///
/// Twelve instructions and four constants at sixty four bits, against one `popcnt`, which is the
/// size of what #310 is worth.
///
/// The multiply is the last step only because the byte sums are each at most eight and there are at
/// most eight of them, so the running total in the top byte cannot carry out of it. At eight bits
/// there are no bytes to add and the third step is already the answer.
fn counted(func: &mut Func, inst: Inst) {
    let ty = produced(func, inst);
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !countable(ty) {
        return;
    }
    let width = ty.bits();
    let pairs = ahead_const(func, inst, Imm::int(alternating(width, 1), ty), ty);
    let two = ahead_const(func, inst, Imm::int(2, ty), ty);
    let one = ahead_const(func, inst, Imm::int(1, ty), ty);
    let high = ahead(func, inst, Opcode::LShr, &[arg, one], ty);
    let odd = ahead(func, inst, Opcode::And, &[high, pairs], ty);
    let bits = ahead(func, inst, Opcode::Sub, &[arg, odd], ty);

    let quads = ahead_const(func, inst, Imm::int(alternating(width, 2), ty), ty);
    let low = ahead(func, inst, Opcode::And, &[bits, quads], ty);
    let up = ahead(func, inst, Opcode::LShr, &[bits, two], ty);
    let rest = ahead(func, inst, Opcode::And, &[up, quads], ty);
    let nibbles = ahead(func, inst, Opcode::Add, &[low, rest], ty);

    let four = ahead_const(func, inst, Imm::int(4, ty), ty);
    let bytes = ahead_const(func, inst, Imm::int(alternating(width, 4), ty), ty);
    let folded = ahead(func, inst, Opcode::LShr, &[nibbles, four], ty);
    let summed = ahead(func, inst, Opcode::Add, &[nibbles, folded], ty);
    if width == 8 {
        becomes(func, inst, Opcode::And, &[summed, bytes]);
        return;
    }
    let held = ahead(func, inst, Opcode::And, &[summed, bytes], ty);

    let spread = ahead_const(func, inst, Imm::int(every(width, 8, 1), ty), ty);
    let top = ahead_const(func, inst, Imm::int(i128::from(width - 8), ty), ty);
    let total = ahead(func, inst, Opcode::Mul, &[held, spread], ty);
    becomes(func, inst, Opcode::LShr, &[total, top]);
}

/// Rewrites every overflow checked instruction into the arithmetic and the test that is one.
///
/// Six instructions and no rules, which is tamnd/rucc#309. The trade is the one `expand::bytes` and
/// `expand::counts` above make, with one thing on top of it: these are the only instructions in the
/// IR whose result is two things, a value and a bit, and the rule language has no way to write a
/// term that produces two. So even on a machine whose add sets a carry flag, a rule for one of
/// these could not name both halves of what it answers, and the rewrite would have to happen
/// somewhere. Here is that somewhere.
///
/// Because the instruction goes away rather than becoming another one, the values the rest of the
/// function read have to be pointed at what replaced them. That is what `substitute` below does,
/// once, after every instruction has been rewritten.
pub fn overflows(func: &mut Func) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    let mut forward = HashMap::new();
    for inst in found {
        let checked = match func[inst].opcode {
            Opcode::UAddOverflow => Checked::Add(false),
            Opcode::SAddOverflow => Checked::Add(true),
            Opcode::USubOverflow => Checked::Sub(false),
            Opcode::SSubOverflow => Checked::Sub(true),
            Opcode::UMulOverflow => Checked::Mul(false),
            Opcode::SMulOverflow => Checked::Mul(true),
            _ => continue,
        };
        overflowed(func, inst, checked, &mut forward);
    }
    if !forward.is_empty() {
        substitute(func, &forward);
    }
}

/// Which of the six an instruction is, as the arithmetic and whether the operands are signed.
#[derive(Debug, Clone, Copy)]
enum Checked {
    /// An add, whose answer wraps when the exact sum needed one more bit at the top.
    Add(bool),
    /// A subtract.
    Sub(bool),
    /// A multiply, which is the expensive one because the test needs the high half of the product.
    Mul(bool),
}

/// One overflow checked instruction, as the ordinary arithmetic and a test on the operands.
///
/// The value is always the ordinary instruction, because that is what the wrapped answer is. What
/// differs between the six is how the bit is worked out.
///
/// The two adds and the two subtracts are one comparison each. An unsigned sum wraps exactly when
/// it came out below either operand, and an unsigned difference wraps exactly when the left operand
/// was below the right. A signed sum wraps exactly when both operands had the same sign and the
/// answer had the other one, which `(a ^ v) & (b ^ v)` has the sign bit of, and a signed difference
/// wraps exactly when the operands had different signs and the answer took the right one's, which
/// `(a ^ b) & (a ^ v)` has the sign bit of.
///
/// The multiplies are the high half of the product against what the low half implies it should be.
/// For an unsigned multiply the product fits exactly when the high half is zero, and for a signed
/// one it fits exactly when the high half is the sign extension of the low half, which is the low
/// half shifted right arithmetically by every bit but one.
fn overflowed(func: &mut Func, inst: Inst, checked: Checked, forward: &mut HashMap<Value, Value>) {
    let ty = produced(func, inst);
    let [a, b] = func[func[inst].args] else { return };
    if !countable(ty) {
        return;
    }
    let (value, bit) = match checked {
        Checked::Add(signed) => {
            let value = ahead(func, inst, Opcode::Add, &[a, b], ty);
            let bit = if signed {
                let left = ahead(func, inst, Opcode::Xor, &[a, value], ty);
                let right = ahead(func, inst, Opcode::Xor, &[b, value], ty);
                let both = ahead(func, inst, Opcode::And, &[left, right], ty);
                negative(func, inst, both, ty)
            } else {
                compared(func, inst, IntPred::Ult, value, a)
            };
            (value, bit)
        }
        Checked::Sub(signed) => {
            let value = ahead(func, inst, Opcode::Sub, &[a, b], ty);
            let bit = if signed {
                let apart = ahead(func, inst, Opcode::Xor, &[a, b], ty);
                let moved = ahead(func, inst, Opcode::Xor, &[a, value], ty);
                let both = ahead(func, inst, Opcode::And, &[apart, moved], ty);
                negative(func, inst, both, ty)
            } else {
                compared(func, inst, IntPred::Ult, a, b)
            };
            (value, bit)
        }
        Checked::Mul(signed) => {
            let value = ahead(func, inst, Opcode::Mul, &[a, b], ty);
            let high = high_half(func, inst, a, b, signed, ty);
            let bit = if signed {
                let sign = ahead_const(func, inst, Imm::int(i128::from(ty.bits() - 1), ty), ty);
                let wanted = ahead(func, inst, Opcode::AShr, &[value, sign], ty);
                compared(func, inst, IntPred::Ne, high, wanted)
            } else {
                let zero = ahead_const(func, inst, Imm::int(0, ty), ty);
                compared(func, inst, IntPred::Ne, high, zero)
            };
            (value, bit)
        }
    };
    let mut answers = func[inst].results();
    if let (Some(wrapped), Some(flag)) = (answers.next(), answers.next()) {
        forward.insert(wrapped, value);
        forward.insert(flag, bit);
    }
    func.remove_inst(inst);
}

/// The high half of the product of two values, at the width they are.
///
/// Both operands are split into halves of half the width and multiplied four ways, which is long
/// multiplication in base two to the half width. The three partial products that reach the top are
/// added with the carry out of the bottom ones, and every step of that fits in the width because
/// the total is the high half of the product and that is what a high half is.
///
/// The whole of it is unsigned, and a signed high half is the unsigned one with a correction: a
/// negative operand contributed the width's worth of sign bits to the unsigned product that it
/// should not have, so the other operand is subtracted off once for each negative operand. Spreading
/// the sign bit of each with an arithmetic shift is what turns that into a mask rather than a
/// branch.
///
/// This is the expensive one. Six multiplies and a dozen other instructions at sixty four bits,
/// against one `mul` on a machine whose multiply writes the high half into a second register. That
/// is most of what #309 is worth and it is what makes the multiply the one to give a rule to first.
fn high_half(func: &mut Func, inst: Inst, a: Value, b: Value, signed: bool, ty: Type) -> Value {
    let width = ty.bits();
    let half = width / 2;
    let shift = ahead_const(func, inst, Imm::int(i128::from(half), ty), ty);
    let mask = ahead_const(func, inst, Imm::int((1i128 << half) - 1, ty), ty);

    let al = ahead(func, inst, Opcode::And, &[a, mask], ty);
    let ah = ahead(func, inst, Opcode::LShr, &[a, shift], ty);
    let bl = ahead(func, inst, Opcode::And, &[b, mask], ty);
    let bh = ahead(func, inst, Opcode::LShr, &[b, shift], ty);

    let ll = ahead(func, inst, Opcode::Mul, &[al, bl], ty);
    let lh = ahead(func, inst, Opcode::Mul, &[al, bh], ty);
    let hl = ahead(func, inst, Opcode::Mul, &[ah, bl], ty);
    let hh = ahead(func, inst, Opcode::Mul, &[ah, bh], ty);

    // The carry out of the low half, which is the top of the smallest partial product plus the
    // bottoms of the two middle ones.
    let over = ahead(func, inst, Opcode::LShr, &[ll, shift], ty);
    let lh_low = ahead(func, inst, Opcode::And, &[lh, mask], ty);
    let hl_low = ahead(func, inst, Opcode::And, &[hl, mask], ty);
    let some = ahead(func, inst, Opcode::Add, &[over, lh_low], ty);
    let carry = ahead(func, inst, Opcode::Add, &[some, hl_low], ty);

    let lh_high = ahead(func, inst, Opcode::LShr, &[lh, shift], ty);
    let hl_high = ahead(func, inst, Opcode::LShr, &[hl, shift], ty);
    let up = ahead(func, inst, Opcode::LShr, &[carry, shift], ty);
    let first = ahead(func, inst, Opcode::Add, &[hh, lh_high], ty);
    let second = ahead(func, inst, Opcode::Add, &[first, hl_high], ty);
    let high = ahead(func, inst, Opcode::Add, &[second, up], ty);
    if !signed {
        return high;
    }
    let top = ahead_const(func, inst, Imm::int(i128::from(width - 1), ty), ty);
    let a_sign = ahead(func, inst, Opcode::AShr, &[a, top], ty);
    let b_sign = ahead(func, inst, Opcode::AShr, &[b, top], ty);
    let a_owes = ahead(func, inst, Opcode::And, &[a_sign, b], ty);
    let b_owes = ahead(func, inst, Opcode::And, &[b_sign, a], ty);
    let once = ahead(func, inst, Opcode::Sub, &[high, a_owes], ty);
    ahead(func, inst, Opcode::Sub, &[once, b_owes], ty)
}

/// Whether a value's sign bit is set, as a comparison against zero.
fn negative(func: &mut Func, inst: Inst, value: Value, ty: Type) -> Value {
    let zero = ahead_const(func, inst, Imm::int(0, ty), ty);
    compared(func, inst, IntPred::Slt, value, zero)
}

/// A comparison written in front of an instruction, which [`ahead`] cannot write because a
/// comparison carries its predicate where everything else carries nothing.
fn compared(func: &mut Func, inst: Inst, pred: IntPred, lhs: Value, rhs: Value) -> Value {
    let ty = func[lhs].ty.with_lane(Type::I1);
    let args = func.push_values(&[lhs, rhs]);
    let extra = Extra::IntPred(pred);
    written(func, inst, InstData { args, extra, ..InstData::new(Opcode::ICmp) }, ty)
}

/// Points every reader of a removed instruction's results at what replaced them.
///
/// The arguments of each instruction and the arguments of the blocks it branches to, which between
/// them are everything an instruction can read. Nothing chases here, the way the same walk in
/// `rucc_opt::simplify` does, because every value this map answers with is one written above and so
/// is never itself a key.
fn substitute(func: &mut Func, forward: &HashMap<Value, Value>) {
    let with = |value: Value| forward.get(&value).copied().unwrap_or(value);
    for block in func.blocks().collect::<Vec<_>>() {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            let args = func[inst].args;
            func.rewrite(args, with);
            for call in func.successors(inst).collect::<Vec<_>>() {
                func.rewrite(call.args, with);
            }
        }
    }
}

/// Whether the arithmetic in this file works correctly at this type.
///
/// A whole number of bytes and a power of two of them, which every width the front end can ask about
/// is. Anything else is left as the instruction it was, so a selector with no rule for it says so
/// rather than the program getting a number that was counted, or checked, in the wrong shape.
///
/// The bit counts need it because a halving sum halves, and the overflow checks need it because
/// splitting a value into two halves of equal width needs the width to be even and the halves to be
/// what a shift by half of it separates.
fn countable(ty: Type) -> bool {
    ty.is_int()
        && ty.is_scalar()
        && ty.bits() >= 8
        && ty.bits() <= 64
        && ty.bits().is_power_of_two()
}

/// The most moves a copy or a fill becomes before it is left alone for a call instead.
///
/// Thirty two, which is two hundred and fifty six bytes at a word a time and is a structure larger
/// than almost every one a program writes. What the number is trading is code size against a call,
/// and the exchange rate is a machine's rather than a language's, so the number lives here next to
/// the code it bounds and not in a target description that would have to be right about it for
/// every target at once.
///
/// It is a count of moves and not a count of bytes because that is what the cost is. A copy of
/// sixty four bytes between two addresses aligned to eight is eight moves and a copy of the same
/// sixty four bytes between two addresses aligned to one is sixty four, and the second is the
/// expensive one whatever the size says.
pub const UNROLL: usize = 32;

/// Rewrites every bulk copy and bulk fill, into moves when that is worth it and into a call to the
/// runtime when it is not.
///
/// A copy of more than [`UNROLL`] moves becomes a call, and so does a fill whose byte is not a
/// constant, which the front end does not write today and which would need the byte spread across
/// a word at runtime. A `memmove` is always a call, because the two sides may overlap and a run of
/// moves in one direction is only right for one of the two ways they can.
///
/// `word` is how many bytes the widest move on this machine carries. Nothing here reads a target
/// otherwise, and a copy is the same run of loads and stores everywhere.
pub fn bulk(func: &mut Func, names: &mut Interner, word: u32) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::Memcpy => copy(func, names, inst, word),
            Opcode::Memset => fill(func, names, inst, word),
            Opcode::Memmove => library(func, names, inst, "memmove", word),
            _ => {}
        }
    }
}

/// One `memcpy`, as a load and a store for each word of it.
///
/// Each word is read and then written before the next is read, rather than every read being built
/// before any write the way [`crate::varargs`] copies a list. A `memcpy` is the copy whose two
/// sides the front end promises do not overlap, so what is at the source when the last word is read
/// is what was there when the first was, and reading a word at a time costs one register where
/// reading all of them first would cost as many registers as the copy has words.
fn copy(func: &mut Func, names: &mut Interner, inst: Inst, word: u32) {
    let [into, from] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let info = func[mem];
    let Some(plan) = chunks(info, word) else { return library(func, names, inst, "memcpy", word) };
    for (at, width) in plan {
        let ty = Type::int(width * 8);
        let access = MemInfo { size: u64::from(width), align: width.min(info.align), ..info };
        let there = stepped(func, inst, from, at);
        let word = read(func, inst, there, access, ty);
        let here = stepped(func, inst, into, at);
        write(func, inst, word, here, access);
    }
    func.remove_inst(inst);
}

/// One `memset`, as a store of the byte spread across each word of it.
///
/// The byte is a constant, so the word it spreads into is a constant too and the spreading is done
/// here rather than by the program. The front end writes a `memset` for the part of an object an
/// initialiser did not name, where the byte is always zero, and the general case is written anyway
/// because the arithmetic is the same and being right about `0xff` costs nothing.
fn fill(func: &mut Func, names: &mut Interner, inst: Inst, word: u32) {
    let [into, byte] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let info = func[mem];
    let Some(spelled) = literal(func, byte) else {
        return library(func, names, inst, "memset", word);
    };
    let Some(plan) = chunks(info, word) else { return library(func, names, inst, "memset", word) };
    for (at, width) in plan {
        let ty = Type::int(width * 8);
        let access = MemInfo { size: u64::from(width), align: width.min(info.align), ..info };
        let value = ahead_const(func, inst, Imm::int(spread(spelled, width) as i128, ty), ty);
        let here = stepped(func, inst, into, at);
        write(func, inst, value, here, access);
    }
    func.remove_inst(inst);
}

/// One bulk operation as a call to the routine of that name in the runtime.
///
/// This is what a copy too large to unroll becomes, and what a `memmove` and a fill with a
/// computed byte become whatever their size. The routine is `rucc-builtins`' on a freestanding
/// target and the C library's on a hosted one, and the call is the same either way because the two
/// have the same names and the same signatures on purpose.
///
/// The arguments are the C ones and not the IR ones. The IR holds the size beside the instruction
/// where C passes it, and holds a fill byte as a byte where C passes an `int`, so the size becomes
/// a constant in a register and the byte is widened. The value each returns is its first argument,
/// which nothing reads, so the call is built as returning nothing rather than as returning a
/// pointer nobody looks at.
fn library(func: &mut Func, names: &mut Interner, inst: Inst, routine: &str, word: u32) {
    let [into, second] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let size = func[mem].size;

    // `size_t`, which is as wide as a general purpose register on every target here. Taken from
    // the machine rather than written as sixty four so that a thirty two bit target gets the
    // argument its own C library declares.
    let words = Type::int(word * 8);
    let count = ahead_const(func, inst, Imm::int(i128::from(size), words), words);
    // A fill passes an `int` where the IR passes the byte itself, and the widening is a zero
    // extension because the routine looks at the low eight bits and nothing else.
    let second = match routine {
        "memset" => widened(func, inst, second),
        _ => second,
    };

    let sig = func.add_signature(Signature::new().with_params(&[
        Type::PTR,
        if routine == "memset" { Type::int(32) } else { Type::PTR },
        words,
    ]));
    let callee = names.intern(routine);
    let varargs = func.push_abis(&[]);
    let info = func.add_call(CallInfo { callee: Some(callee), signature: sig, varargs });
    let args = func.push_values(&[into, second, count]);
    let data = &mut func[inst];
    data.opcode = Opcode::Call;
    data.args = args;
    data.extra = Extra::Call(info);
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::Call));
}

/// A value widened to an `int`, or the value itself when it is one already.
fn widened(func: &mut Func, inst: Inst, value: Value) -> Value {
    let int = Type::int(32);
    let ty = func[value].ty;
    if ty == int {
        return value;
    }
    ahead(func, inst, Opcode::ZExt, &[value], int)
}

/// Where each word of a block of memory starts and how wide it is, or nothing for a block that is
/// more words than [`UNROLL`].
///
/// The widest word is the smaller of what the machine moves at once and what the block is known to
/// be aligned to, because a load wider than the alignment is a fault on a machine that checks and
/// this pass does not know whether the one it is compiling for does. That costs a copy of a
/// character array a move per byte, which is exactly the copy the threshold sends to a call.
///
/// The width halves whenever what is left is narrower than it, so a block of thirteen bytes aligned
/// to eight is eight, four and one rather than thirteen ones. Every offset is a multiple of the
/// width at it, since each width divides the sum of the wider ones in front of it, which is what
/// lets the alignment of each access be written down as the width.
fn chunks(info: MemInfo, word: u32) -> Option<Vec<(u64, u32)>> {
    plan(info.size, info.align, word)
}

/// The same, as the two numbers rather than as an access, for the one caller that has no access to
/// ask about.
///
/// [`crate::abi`] copies a structure passed by value into the argument area, and that copy is not a
/// `memcpy` in the IR: it is written straight into the machine IR, because where it goes is an
/// offset the placement walk gives and nothing before this pass knows it. The plan has to be the
/// same plan either way, so it is one function.
pub(crate) fn plan(size: u64, align: u32, word: u32) -> Option<Vec<(u64, u32)>> {
    let widest = word.min(align).max(1);
    if !widest.is_power_of_two() {
        return None;
    }
    let mut plan = Vec::new();
    let mut at = 0;
    let mut width = u64::from(widest);
    while at < size {
        while width > size - at {
            width /= 2;
        }
        plan.push((at, u32::try_from(width).ok()?));
        at += width;
        if plan.len() > UNROLL {
            return None;
        }
    }
    Some(plan)
}

/// The byte a fill writes, when the program said which one rather than working it out.
fn literal(func: &Func, value: Value) -> Option<u8> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(imm) = func[inst].extra else { return None };
    u8::try_from(func[imm].bits() & 0xff).ok()
}

/// One byte repeated across a word of that many bytes, which is what a fill stores.
fn spread(byte: u8, width: u32) -> u64 {
    (0..width).fold(0, |word, at| word | u64::from(byte) << (at * 8))
}

/// The address that far into a block, written in front of an instruction, or the block itself for
/// the word at the front of it.
fn stepped(func: &mut Func, inst: Inst, block: Value, at: u64) -> Value {
    if at == 0 {
        return block;
    }
    let step = ahead_const(func, inst, Imm::int(i128::from(at), Type::int(64)), Type::int(64));
    ahead(func, inst, Opcode::PtrAdd, &[block, step], Type::PTR)
}

/// A load put in front of an instruction, and the value it reads.
fn read(func: &mut Func, inst: Inst, from: Value, info: MemInfo, ty: Type) -> Value {
    let extra = Extra::Mem(func.add_mem(info));
    let args = func.push_values(&[from]);
    written(func, inst, InstData { args, extra, ..InstData::new(Opcode::Load) }, ty)
}

/// A store put in front of an instruction, which produces nothing and is only its effect.
fn write(func: &mut Func, inst: Inst, value: Value, into: Value, info: MemInfo) {
    let span = func.span(inst);
    let extra = Extra::Mem(func.add_mem(info));
    let args = func.push_values(&[value, into]);
    let data = InstData { args, extra, ..InstData::new(Opcode::Store) };
    let made = func.create_inst(data, &[], span);
    func.insert_before(made, inst);
}

/// The width the machine converts at that holds every value of an integer of this one.
///
/// The machine converts between a float and a signed integer at thirty two bits and at sixty four
/// and at no other width, so a conversion anywhere else is one of those two with a widening in
/// front of it or a narrowing behind it. Which of the two it is, is the narrower one the values
/// fit in, and an unsigned integer of `bits` bits needs one more bit than that to be signed in.
///
/// `None` is a width no signed integer here holds, which is only an unsigned sixty four bit one.
fn holder(bits: u32, signed: bool) -> Option<u32> {
    match if signed { bits } else { bits + 1 } {
        ..=32 => Some(32),
        33..=64 => Some(64),
        _ => None,
    }
}

/// The type of the one value an instruction produces.
///
/// Every opcode this pass touches produces exactly one, so an instruction that produces none is
/// one the caller has already gone wrong about and the void type says so without panicking.
fn produced(func: &Func, inst: Inst) -> Type {
    func[inst].first_result.map_or(Type::VOID, |value| func[value].ty)
}

/// Puts an instruction over these operands in front of another one, and gives back its value.
fn ahead(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value], ty: Type) -> Value {
    let args = func.push_values(args);
    written(func, inst, InstData { args, ..InstData::new(opcode) }, ty)
}

/// The same for a comparison, which carries the predicate and produces one bit.
fn ahead_cmp(func: &mut Func, inst: Inst, opcode: Opcode, extra: Extra, args: &[Value]) -> Value {
    let args = func.push_values(args);
    written(func, inst, InstData { args, extra, ..InstData::new(opcode) }, Type::I1)
}

/// The same for a constant, which carries an immediate rather than operands.
fn ahead_const(func: &mut Func, inst: Inst, imm: Imm, ty: Type) -> Value {
    let extra = Extra::Imm(func.add_imm(imm));
    written(func, inst, InstData { extra, ..InstData::new(Opcode::IConst) }, ty)
}

/// Creates the instruction, puts it where those two asked, and reads its value back out.
fn written(func: &mut Func, inst: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(inst);
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    func[made].first_result.expect("an instruction created with one result has one")
}

/// Turns an instruction into a different one over different operands, in place.
///
/// The last instruction of a rewrite is the original rather than a new one, so the value the rest
/// of the function reads is the value it already read and nothing has to be substituted anywhere.
/// The type of that value does not change either, because every rewrite here ends at the type it
/// started at.
fn becomes(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value]) {
    let args = func.push_values(args);
    let data = &mut func[inst];
    data.opcode = opcode;
    data.args = args;
    data.extra = Extra::None;
    // What the program said about rounding and about not a numbers is still true of the
    // instructions it became, and what is no longer meaningful is dropped rather than carried.
    data.flags = data.flags.intersection(Flags::legal_on(opcode));
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
    use rucc_ir::{Builder, Flags, Float, Func, Module, Opcode, Signature, Type};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use rucc_ir::{Extra, InstData, MemInfo, MemOrder, Restrict};

    use super::{
        UNROLL, alternating, blocks_for, bulk, bytes, chunks, counts, every, floats, orderings,
        overflows, spread, switches,
    };

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

    /// A function of one parameter and one result, with a body somebody else writes.
    ///
    /// The float rewrites are each one instruction becoming several in the middle of a block, so
    /// what a test needs is a block with something around the instruction rather than a shape.
    fn one(
        params: &[Type],
        returns: &[Type],
        body: impl FnOnce(&mut Builder<'_>, &[rucc_ir::Value]),
    ) -> (Interner, Func) {
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("f"),
            Signature::new().with_params(params).with_returns(returns),
        );
        let entry = func.create_block();
        let args: Vec<_> = params.iter().map(|&ty| func.append_param(entry, ty)).collect();
        let mut build = Builder::new(&mut func, entry);
        body(&mut build, &args);
        (names, func)
    }

    fn f64() -> Type {
        Type::float(Float::F64)
    }

    fn f32() -> Type {
        Type::float(Float::F32)
    }

    /// The obligation every rewrite here has: nothing after this checks the IR again.
    fn valid(func: &Func, names: &mut Interner) {
        let module = Module::new(names.intern("f.c"), &target());
        rucc_ir::verify_func(&module, func, names).expect("the rewrite builds valid IR");
    }

    /// `double c(void) { return 1.5; }`, which is the constant nothing in the rule set can name.
    #[test]
    fn a_float_constant_becomes_the_integer_that_spells_it_and_a_reading_of_those_bits() {
        let (mut names, mut func) = one(&[], &[f64()], |build, _| {
            let k = build.fconst(f64(), 0x3ff8_0000_0000_0000);
            build.ret(&[k]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("fconst"), "the float constant is gone: {text}");
        assert!(text.contains("iconst.i64 4609434218613702656"), "the bits, as an integer: {text}");
        assert!(text.contains("bitcast"), "read back as the float: {text}");
    }

    /// The width follows the format rather than being the widest one, so a `float` constant is an
    /// `i32` and reaches `movd` rather than `movq`.
    #[test]
    fn a_constant_at_the_narrow_format_is_an_integer_of_the_narrow_width() {
        let (mut names, mut func) = one(&[], &[f32()], |build, _| {
            let k = build.fconst(f32(), 0x4020_0000);
            build.ret(&[k]);
        });
        floats(&mut func);
        assert!(printed(&func, &mut names).contains("iconst.i32"), "an i32, not an i64");
    }

    /// `double n(double x) { return -x; }`. Flipping the sign bit is what C means and subtracting
    /// from zero is not, so what this asserts is the exclusive or and the mask it is given.
    #[test]
    fn a_negation_flips_the_sign_bit_and_touches_no_other() {
        let (mut names, mut func) = one(&[f64()], &[f64()], |build, args| {
            let n = build.unary(Opcode::FNeg, args[0], f64());
            build.ret(&[n]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("fneg"), "the negation is gone: {text}");
        assert!(!text.contains("fsub"), "and it did not become a subtraction: {text}");
        assert!(text.contains("iconst.i64 -9223372036854775808"), "the sign bit alone: {text}");
        assert_eq!(text.matches("xor").count(), 1, "one exclusive or: {text}");
        assert_eq!(text.matches("bitcast").count(), 2, "there and back: {text}");
    }

    /// `double u(unsigned x) { return x; }`, which is a widening and the signed conversion.
    #[test]
    fn an_unsigned_integer_becoming_a_float_widens_first_and_then_converts_as_signed() {
        let (mut names, mut func) = one(&[Type::int(32)], &[f64()], |build, args| {
            let d = build.unary(Opcode::UIToFP, args[0], f64());
            build.ret(&[d]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("uitofp"), "the unsigned conversion is gone: {text}");
        assert!(text.contains("zext.i64"), "widened with zeroes: {text}");
        assert!(text.contains("sitofp.f64"), "converted as signed: {text}");
    }

    /// `unsigned t(double x) { return x; }`, which is the same argument the other way round.
    #[test]
    fn a_float_becoming_an_unsigned_integer_converts_as_signed_first_and_then_narrows() {
        let (mut names, mut func) = one(&[f64()], &[Type::int(32)], |build, args| {
            let n = build.unary(Opcode::FPToUI, args[0], Type::int(32));
            build.ret(&[n]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("fptoui"), "the unsigned conversion is gone: {text}");
        assert!(text.contains("fptosi.i64"), "converted as signed: {text}");
        assert!(text.contains("trunc.i32"), "and narrowed to what was asked: {text}");
    }

    /// `signed char a(double x) { return (signed char)x; }`, which the front end writes as a
    /// conversion straight to eight bits and the machine has no instruction for at that width.
    #[test]
    fn a_conversion_narrower_than_the_machine_has_is_one_it_has_and_a_narrowing() {
        let (mut names, mut func) = one(&[f64()], &[Type::int(8)], |build, args| {
            let n = build.unary(Opcode::FPToSI, args[0], Type::int(8));
            build.ret(&[n]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(text.contains("fptosi.i32"), "converted at a width there is one at: {text}");
        assert!(text.contains("trunc.i8"), "and narrowed to what was asked: {text}");
    }

    /// The same the other way, where the widening carries the sign because the value has one.
    #[test]
    fn a_signed_integer_narrower_than_the_machine_converts_from_is_widened_with_its_sign() {
        let (mut names, mut func) = one(&[Type::int(8)], &[f64()], |build, args| {
            let d = build.unary(Opcode::SIToFP, args[0], f64());
            build.ret(&[d]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(text.contains("sext.i32"), "widened with the sign and not with zeroes: {text}");
        assert!(!text.contains("zext"), "widened with the sign and not with zeroes: {text}");
        assert!(text.contains("sitofp.f64"), "converted at a width there is one at: {text}");
    }

    /// The table the two of them share, which is where the whole argument about widths lives.
    #[test]
    fn the_width_a_conversion_happens_at_is_the_narrowest_one_that_holds_the_values() {
        use super::holder;
        for bits in [1, 8, 16, 32] {
            assert_eq!(holder(bits, true), Some(32), "a signed {bits} bit value fits in an int");
        }
        assert_eq!(holder(64, true), Some(64));
        for bits in [1, 8, 16, 31] {
            assert_eq!(holder(bits, false), Some(32), "an unsigned {bits} bit value does too");
        }
        // The one more bit an unsigned value needs is what makes these two the wider width.
        assert_eq!(holder(32, false), Some(64));
        assert_eq!(holder(64, false), None);
    }

    /// Sixty four bits is where the widening argument runs out, because an unsigned value of that
    /// width is not a signed value of any width the IR has. Each of the two gets a rewrite of its
    /// own, and what both leave is the signed conversion the machine has with arithmetic around it.
    #[test]
    fn the_unsigned_conversions_at_the_widest_width_become_the_signed_one_and_a_correction() {
        for float in [f32(), f64()] {
            let (mut names, mut func) = one(&[Type::int(64)], &[float], |build, args| {
                let d = build.unary(Opcode::UIToFP, args[0], float);
                build.ret(&[d]);
            });
            floats(&mut func);
            let text = printed(&func, &mut names);
            assert!(!text.contains("uitofp"), "the unsigned conversion is gone: {text}");
            assert!(text.contains("sitofp"), "the signed one is what is left: {text}");
            // The halving that brings the value under the range the signed conversion has, and the
            // bit it would have thrown away put back so that the rounding is still the right one.
            assert!(text.contains("lshr"), "the value is halved: {text}");
            assert!(text.contains("fadd"), "and doubled again afterwards: {text}");
            valid(&func, &mut names);
        }

        for float in [f32(), f64()] {
            let (mut names, mut func) = one(&[float], &[Type::int(64)], |build, args| {
                let n = build.unary(Opcode::FPToUI, args[0], Type::int(64));
                build.ret(&[n]);
            });
            floats(&mut func);
            let text = printed(&func, &mut names);
            assert!(!text.contains("fptoui"), "the unsigned conversion is gone: {text}");
            assert!(text.contains("fptosi"), "the signed one is what is left: {text}");
            // Half the range taken off before the conversion and put back on after it.
            assert!(text.contains("fsub"), "the value is brought down: {text}");
            assert!(text.contains("shl"), "and the top bit goes back on: {text}");
            valid(&func, &mut names);
        }
    }

    /// Neither of those two has a branch in it, which is the thing about them worth a test of its
    /// own. Every rewrite in this pass stays inside the block it started in, so a pass that grew a
    /// second block would be one whose callers all have to be looked at again.
    #[test]
    fn the_widest_unsigned_conversions_are_written_without_a_branch() {
        let (_, mut func) = one(&[Type::int(64)], &[f64()], |build, args| {
            let d = build.unary(Opcode::UIToFP, args[0], f64());
            build.ret(&[d]);
        });
        floats(&mut func);
        assert_eq!(func.blocks().count(), 1, "the conversion did not split the block");

        let (_, mut func) = one(&[f64()], &[Type::int(64)], |build, args| {
            let n = build.unary(Opcode::FPToUI, args[0], Type::int(64));
            build.ret(&[n]);
        });
        floats(&mut func);
        assert_eq!(func.blocks().count(), 1, "nor did the other one");
    }

    /// The arithmetic of those two rewrites, done here in the same order the instructions do it.
    ///
    /// This is not the compiler running, it is the sequence written out again in a language that
    /// can be asked what the answer should have been. What it checks is the part that is easy to
    /// get wrong and impossible to see in the assembly, which is whether the halving rounds the way
    /// the conversion would have and whether the subtraction is exact. Every boundary is in the
    /// list, and so are the values either side of the ones where the two paths meet.
    #[test]
    fn the_arithmetic_the_widest_unsigned_conversions_do_is_the_conversion() {
        const CASES: &[u64] = &[
            0,
            1,
            2,
            0x7FFF_FFFF,
            0x8000_0000,
            0xFFFF_FFFF,
            0x0020_0000_0000_0000,
            0x0020_0000_0000_0001,
            0x7FFF_FFFF_FFFF_FFFF,
            0x8000_0000_0000_0000,
            0x8000_0000_0000_0001,
            0x8000_0000_0000_0400,
            0xFFFF_FFFF_FFFF_F800,
            0xFFFF_FFFF_FFFF_FFFF,
        ];
        for &x in CASES {
            // What `from_unsigned_word` writes, at `f64`.
            let mask = if (x as i64) < 0 { u64::MAX } else { 0 };
            let odd = (x >> 1) | (x & 1);
            let source = x ^ ((x ^ odd) & mask);
            let converted = source as i64 as f64;
            let addend = f64::from_bits(converted.to_bits() & mask);
            assert_eq!(converted + addend, x as f64, "converting {x:#x} into a double");
        }

        for &x in CASES {
            // And what `to_unsigned_word` writes, at `f64`, over the same values read back.
            let d = x as f64;
            if d >= 18_446_744_073_709_551_616.0 {
                continue;
            }
            let half = f64::from_bits(0x43E0_0000_0000_0000);
            let mask = if d >= half { u64::MAX } else { 0 };
            let taken = f64::from_bits(half.to_bits() & mask);
            let low = (d - taken) as i64;
            let top = u64::from(d >= half) << 63;
            assert_eq!(low as u64 ^ top, d as u64, "converting {d} into an unsigned word");
        }
    }

    /// The same obligation the `switch` rewrite has, for the same reason: nothing after this
    /// checks the IR again and everything after it assumes what the verifier would have said.
    #[test]
    fn what_the_float_rewrites_leave_is_valid_ir() {
        let (mut names, mut func) = one(&[Type::int(32)], &[f64()], |build, args| {
            let k = build.fconst(f64(), 0x3ff8_0000_0000_0000);
            let d = build.unary(Opcode::UIToFP, args[0], f64());
            let n = build.unary(Opcode::FNeg, d, f64());
            let s = build.binary(Opcode::FAdd, n, k, Flags::NONE);
            build.ret(&[s]);
        });
        floats(&mut func);
        let module = Module::new(names.intern("f.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    /// Nothing else is touched, for the same reason the `switch` pass has that test: this runs
    /// over every function whether or not one has a float in it.
    #[test]
    fn a_function_with_no_floats_in_it_is_left_exactly_as_it_was() {
        let (mut names, mut func) = one(&[Type::int(32)], &[Type::int(32)], |build, args| {
            build.ret(&[args[0]]);
        });
        let before = printed(&func, &mut names);
        floats(&mut func);
        assert_eq!(printed(&func, &mut names), before);
    }
    fn access(size: u64, align: u32) -> MemInfo {
        MemInfo { size, align, order: MemOrder::NotAtomic, tbaa: None, restrict: Restrict::NONE }
    }

    /// `void c(void *to, const void *from) { *(T *)to = *(const T *)from; }` for a `T` of that
    /// size and alignment, which is what the front end writes for a structure assignment.
    fn moving(opcode: Opcode, size: u64, align: u32, byte: Option<i128>) -> (Interner, Func) {
        one(&[Type::PTR, Type::PTR], &[], |build, args| {
            let second = match byte {
                Some(value) => build.iconst(Type::int(8), value),
                None => args[1],
            };
            let mem = build.func().add_mem(access(size, align));
            let operands = build.func().push_values(&[args[0], second]);
            let data = InstData { args: operands, extra: Extra::Mem(mem), ..InstData::new(opcode) };
            build.inst(data, &[]);
            build.ret(&[]);
        })
    }

    fn copying(size: u64, align: u32) -> (Interner, Func) {
        moving(Opcode::Memcpy, size, align, None)
    }

    fn filling(size: u64, align: u32, byte: i128) -> (Interner, Func) {
        moving(Opcode::Memset, size, align, Some(byte))
    }

    /// The plan a copy of that size and alignment becomes, as widths, which is what the offsets
    /// follow from.
    fn widths(size: u64, align: u32) -> Option<Vec<u32>> {
        Some(chunks(access(size, align), 8)?.into_iter().map(|(_, width)| width).collect())
    }

    /// `struct point { int x, y; } a, b; a = b;`, which is sixteen bytes aligned to eight.
    #[test]
    fn a_copy_becomes_a_load_and_a_store_for_each_word_of_it() {
        let (mut names, mut func) = copying(16, 8);
        bulk(&mut func, &mut names, 8);

        let text = printed(&func, &mut names);
        assert!(!text.contains("memcpy"), "the copy is gone: {text}");
        assert_eq!(text.matches("load.i64").count(), 2, "a load per word: {text}");
        assert_eq!(text.matches("store").count(), 2, "a store per word: {text}");
        assert_eq!(
            text.matches("ptr_add").count(),
            2,
            "no offset for the word at the front: {text}"
        );
    }

    /// A word is as wide as the block is known to be aligned to and no wider, because a load
    /// wider than that faults on a machine that checks and this does not know whether the one it
    /// is compiling for does.
    #[test]
    fn a_word_is_as_wide_as_the_block_is_aligned_to() {
        assert_eq!(widths(16, 8), Some(vec![8, 8]));
        assert_eq!(widths(16, 4), Some(vec![4, 4, 4, 4]));
        assert_eq!(widths(4, 1), Some(vec![1, 1, 1, 1]));
    }

    /// What is left over is narrower words rather than a run of bytes, so thirteen bytes aligned
    /// to eight is three moves and not six.
    #[test]
    fn what_is_left_over_is_narrower_words_and_not_a_run_of_bytes() {
        assert_eq!(widths(13, 8), Some(vec![8, 4, 1]));
        assert_eq!(widths(3, 8), Some(vec![2, 1]));
        assert_eq!(widths(1, 8), Some(vec![1]));
    }

    /// Every offset is a multiple of the width at it, which is what lets the alignment of each
    /// access be written down as its width.
    #[test]
    fn every_word_starts_somewhere_it_is_aligned_for() {
        for (at, width) in chunks(access(13, 8), 8).expect("a plan for thirteen bytes") {
            assert_eq!(at % u64::from(width), 0, "{at} is a multiple of {width}");
        }
    }

    /// `struct big b = { 0 };`, where the part the initialiser did not name is zeroed.
    #[test]
    fn a_fill_is_the_byte_spread_across_each_word() {
        let (mut names, mut func) = filling(16, 8, 0);
        bulk(&mut func, &mut names, 8);

        let text = printed(&func, &mut names);
        assert!(!text.contains("memset"), "the fill is gone: {text}");
        assert_eq!(text.matches("store").count(), 2, "a store per word: {text}");
        assert!(!text.contains("load"), "a fill reads nothing: {text}");
    }

    /// The spreading is arithmetic on the byte, which is the thing a rule cannot do and the
    /// reason this pass exists at all.
    #[test]
    fn the_byte_is_repeated_across_the_word_it_is_stored_as() {
        assert_eq!(spread(0, 8), 0);
        assert_eq!(spread(0xff, 1), 0xff);
        assert_eq!(spread(0xff, 4), 0xffff_ffff);
        assert_eq!(spread(0xab, 2), 0xabab);
        assert_eq!(spread(0xab, 8), 0xabab_abab_abab_abab);
    }

    /// A copy larger than the threshold is a call to the runtime rather than a run of moves.
    #[test]
    fn a_copy_too_large_to_unroll_becomes_a_call_to_the_runtime() {
        let size = u64::try_from(UNROLL).expect("a small threshold") + 1;
        let (mut names, mut func) = copying(size, 1);
        bulk(&mut func, &mut names, 8);
        let text = printed(&func, &mut names);
        assert!(text.contains("call @memcpy"), "a call and not a bulk move: {text}");

        // And the one word under it is moves, because the threshold counts moves rather than
        // bytes and the whole point of the threshold is that a small copy does not pay for a call.
        let (mut names, mut func) = copying(size - 1, 1);
        bulk(&mut func, &mut names, 8);
        assert!(!printed(&func, &mut names).contains("memcpy"), "one word under it is unrolled");
    }

    /// The call passes what C passes, which is not what the IR holds. The size lives beside the
    /// instruction in the IR and travels in a register in the call.
    #[test]
    fn the_call_passes_the_size_that_the_instruction_carried_beside_it() {
        let size = u64::try_from(UNROLL).expect("a small threshold") + 1;
        let (mut names, mut func) = copying(size, 1);
        bulk(&mut func, &mut names, 8);
        let text = printed(&func, &mut names);
        assert!(text.contains(&format!("{size}")), "the size is an argument now: {text}");
    }

    /// A `memmove` is a call whatever its size, because the two sides may overlap and a run of
    /// moves in one direction is right for only one of the two ways they can.
    #[test]
    fn a_move_is_a_call_however_small_it_is() {
        let (mut names, mut func) = moving(Opcode::Memmove, 8, 8, None);
        bulk(&mut func, &mut names, 8);
        let text = printed(&func, &mut names);
        assert!(text.contains("call @memmove"), "a call and not a run of moves: {text}");
    }

    /// A fill whose byte the program works out rather than names. Spreading a value across a
    /// word at runtime is a multiply, so this is a call rather than moves however small it is.
    #[test]
    fn a_fill_whose_byte_is_not_a_constant_becomes_a_call() {
        let (mut names, mut func) = one(&[Type::PTR, Type::int(8)], &[], |build, args| {
            let mem = build.func().add_mem(access(8, 8));
            let operands = build.func().push_values(&[args[0], args[1]]);
            let data = InstData {
                args: operands,
                extra: Extra::Mem(mem),
                ..InstData::new(Opcode::Memset)
            };
            build.inst(data, &[]);
            build.ret(&[]);
        });
        bulk(&mut func, &mut names, 8);
        let text = printed(&func, &mut names);
        assert!(text.contains("call @memset"), "a call and not a run of stores: {text}");
        // Widened, because C passes the byte as an `int` and the IR holds it as a byte.
        assert!(text.contains("zext.i32"), "the byte is widened to what C passes: {text}");
    }

    /// A machine whose widest move is four bytes gets four byte words out of an eight byte block,
    /// however well aligned the block is.
    #[test]
    fn no_word_is_wider_than_the_machine_moves_at_once() {
        assert_eq!(chunks(access(8, 8), 4).map(|plan| plan.len()), Some(2));
        assert_eq!(chunks(access(8, 8), 8).map(|plan| plan.len()), Some(1));
    }

    #[test]
    fn what_a_copy_becomes_is_ir_that_verifies() {
        let (mut names, mut func) = copying(13, 8);
        bulk(&mut func, &mut names, 8);
        let module = Module::new(names.intern("c.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    #[test]
    fn what_a_fill_becomes_is_ir_that_verifies() {
        let (mut names, mut func) = filling(13, 8, 0xff);
        bulk(&mut func, &mut names, 8);
        let module = Module::new(names.intern("f.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    #[test]
    fn what_a_copy_too_large_to_unroll_becomes_is_ir_that_verifies() {
        let size = u64::try_from(UNROLL).expect("a small threshold") + 1;
        let (mut names, mut func) = copying(size, 1);
        bulk(&mut func, &mut names, 8);
        let module = Module::new(names.intern("c.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the call is valid IR");
    }

    /// Nothing else is touched, for the same reason the other two passes have that test.
    #[test]
    fn a_function_with_no_bulk_move_in_it_is_left_exactly_as_it_was() {
        let (mut names, mut func) = one(&[Type::int(32)], &[Type::int(32)], |build, args| {
            build.ret(&[args[0]]);
        });
        let before = printed(&func, &mut names);
        bulk(&mut func, &mut names, 8);
        assert_eq!(printed(&func, &mut names), before);
    }

    /// A function whose body is one byte swap of the given width, which is what a call to
    /// `__builtin_bswap16` and its neighbours has become by the time this pass runs.
    fn swapping(width: u32) -> (Interner, Func) {
        let ty = Type::int(width);
        one(&[ty], &[ty], |build, args| {
            let s = build.unary(Opcode::Bswap, args[0], ty);
            build.ret(&[s]);
        })
    }

    /// The masks are the alternating runs the halving needs, and they are the constants a reader
    /// checking this against a byte swap written by hand would expect to see.
    ///
    /// At thirty two bits the first step swaps sixteen bit halves and so keeps the low half of each
    /// pair, which is `0x0000ffff`, and the second swaps bytes within those halves and keeps
    /// `0x00ff00ff`. Written as signed because that is what the IR holds an immediate as.
    #[test]
    fn the_masks_are_the_alternating_runs_of_the_group_being_swapped() {
        assert_eq!(alternating(32, 16), 0x0000_ffff);
        assert_eq!(alternating(32, 8), 0x00ff_00ff);
        assert_eq!(alternating(16, 8), 0x00ff);
        assert_eq!(alternating(64, 32), 0x0000_0000_ffff_ffff);
        assert_eq!(alternating(64, 16), 0x0000_ffff_0000_ffff);
        assert_eq!(alternating(64, 8), 0x00ff_00ff_00ff_00ff);
    }

    /// The two byte swap is the one step there is, so it is one mask and one pair of shifts.
    #[test]
    fn a_two_byte_swap_is_one_exchange_of_neighbouring_bytes() {
        let (mut names, mut func) = swapping(16);
        bytes(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("bswap"), "the instruction is gone: {text}");
        assert!(text.contains("iconst.i16 255"), "the low byte of the pair: {text}");
        assert_eq!(text.matches("shl").count(), 1, "one shift up: {text}");
        assert_eq!(text.matches("lshr").count(), 1, "one shift down: {text}");
        assert_eq!(text.matches(" or ").count(), 1, "and the two put together: {text}");
    }

    /// The wider two are the same step done again at half the group, which is what makes the count
    /// grow by a fixed amount per doubling rather than per byte.
    #[test]
    fn a_wider_swap_is_the_same_exchange_once_per_halving() {
        for (width, steps) in [(16u32, 1usize), (32, 2), (64, 3)] {
            let (mut names, mut func) = swapping(width);
            bytes(&mut func);
            let text = printed(&func, &mut names);
            assert_eq!(text.matches("shl").count(), steps, "at {width}: {text}");
            assert_eq!(text.matches("lshr").count(), steps, "at {width}: {text}");
            assert_eq!(text.matches(" and ").count(), steps * 2, "at {width}: {text}");
            assert_eq!(text.matches(" or ").count(), steps, "at {width}: {text}");
        }
    }

    /// The shift counts are the group being exchanged and nothing else, so a reader can read the
    /// halving straight off the constants.
    #[test]
    fn the_shift_counts_are_the_group_width_halving_as_it_goes() {
        let (mut names, mut func) = swapping(64);
        bytes(&mut func);
        let text = printed(&func, &mut names);
        for count in ["iconst.i64 32", "iconst.i64 16", "iconst.i64 8"] {
            assert!(text.contains(count), "{count} is a step: {text}");
        }
    }

    /// The rewrite has to leave a function the verifier still accepts, for the reason the switch
    /// rewrite has the same test: nothing rechecks it.
    #[test]
    fn what_a_byte_swap_becomes_is_ir_that_verifies() {
        let (mut names, mut func) = swapping(32);
        bytes(&mut func);
        let module = Module::new(names.intern("b.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    /// Nothing else is touched, which matters because this runs over every function in the program
    /// and nearly none of them reverses any bytes.
    #[test]
    fn a_function_with_no_byte_swap_in_it_is_left_exactly_as_it_was() {
        let (mut names, mut func) = one(&[Type::int(32)], &[Type::int(32)], |build, args| {
            build.ret(&[args[0]]);
        });
        let before = printed(&func, &mut names);
        bytes(&mut func);
        assert_eq!(printed(&func, &mut names), before);
    }

    /// A function whose body is one bit count of the given opcode and width.
    fn counting(op: Opcode, width: u32) -> (Interner, Func) {
        let ty = Type::int(width);
        one(&[ty], &[ty], |build, args| {
            let c = build.unary(op, args[0], ty);
            build.ret(&[c]);
        })
    }

    /// The masks the halving sum needs, which are the ones any bit counting routine is written with
    /// and are worth being able to read off against one.
    #[test]
    fn the_counting_masks_are_the_ones_the_halving_sum_is_written_with() {
        assert_eq!(alternating(32, 1), 0x5555_5555);
        assert_eq!(alternating(32, 2), 0x3333_3333);
        assert_eq!(alternating(32, 4), 0x0f0f_0f0f);
        assert_eq!(every(32, 8, 1), 0x0101_0101);
        assert_eq!(every(64, 8, 1), 0x0101_0101_0101_0101);
    }

    /// The set bit count is arithmetic and the multiply is what adds the bytes together, which is
    /// the step a reader is most likely to want to check.
    #[test]
    fn a_set_bit_count_is_the_halving_sum_and_a_multiply_that_adds_the_bytes() {
        let (mut names, mut func) = counting(Opcode::Ctpop, 32);
        counts(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("ctpop"), "the instruction is gone: {text}");
        assert!(text.contains("iconst.i32 1431655765"), "the pairs mask: {text}");
        assert!(text.contains("iconst.i32 858993459"), "the nibbles mask: {text}");
        assert!(text.contains("iconst.i32 252645135"), "the bytes mask: {text}");
        assert_eq!(text.matches(" mul ").count(), 1, "one multiply: {text}");
        assert!(text.contains("iconst.i32 24"), "and the top byte is the answer: {text}");
    }

    /// At eight bits there are no bytes left to add, so the multiply is not written at all.
    #[test]
    fn a_count_of_one_byte_stops_before_the_multiply() {
        let (mut names, mut func) = counting(Opcode::Ctpop, 8);
        counts(&mut func);
        let text = printed(&func, &mut names);
        assert!(!text.contains("ctpop"), "{text}");
        assert!(!text.contains(" mul "), "nothing to add together: {text}");
    }

    /// A leading zero count smears every set bit downwards and counts what is left unset above it,
    /// which is one shift and one or per doubling and then the count.
    #[test]
    fn a_leading_zero_count_smears_the_value_down_and_counts_the_complement() {
        let (mut names, mut func) = counting(Opcode::Ctlz, 32);
        counts(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("ctlz"), "the instruction is gone: {text}");
        assert!(!text.contains("ctpop"), "and so is the count it became: {text}");
        for by in ["iconst.i32 1", "iconst.i32 2", "iconst.i32 4", "iconst.i32 8", "iconst.i32 16"]
        {
            assert!(text.contains(by), "{by} is a smearing step: {text}");
        }
        assert_eq!(text.matches(" xor ").count(), 1, "one complement: {text}");
    }

    /// A trailing zero count is the bits below the lowest set one, which is a mask and no smearing.
    #[test]
    fn a_trailing_zero_count_masks_the_bits_below_the_lowest_set_one() {
        let (mut names, mut func) = counting(Opcode::Cttz, 32);
        counts(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("cttz"), "the instruction is gone: {text}");
        assert!(!text.contains("ctpop"), "and so is the count it became: {text}");
        assert!(text.contains("iconst.i32 -1"), "the complement and the decrement: {text}");
        assert_eq!(text.matches(" xor ").count(), 1, "one complement: {text}");
        // Far fewer instructions than the leading count, because there is no smearing to do.
        assert!(text.matches(" or ").count() <= 1, "no smearing run: {text}");
    }

    /// The rewrites have to leave a function the verifier still accepts, at every width and for all
    /// three, because nothing rechecks what comes out of here.
    #[test]
    fn what_a_bit_count_becomes_is_ir_that_verifies() {
        for op in [Opcode::Ctpop, Opcode::Ctlz, Opcode::Cttz] {
            for width in [8u32, 16, 32, 64] {
                let (mut names, mut func) = counting(op, width);
                counts(&mut func);
                let module = Module::new(names.intern("c.c"), &target());
                rucc_ir::verify_func(&module, &func, &names)
                    .unwrap_or_else(|e| panic!("{op:?} at {width}: {e:?}"));
            }
        }
    }

    /// A width the arithmetic is not written for is left as the instruction it was, so a selector
    /// with no rule for it says so rather than the program getting a number counted in the wrong
    /// shape.
    #[test]
    fn a_width_the_halving_sum_is_not_written_for_is_left_alone() {
        let (mut names, mut func) = counting(Opcode::Ctpop, 24);
        counts(&mut func);
        assert!(printed(&func, &mut names).contains("ctpop"), "left as it was");
    }

    /// Nothing else is touched, for the same reason the other passes have that test.
    #[test]
    fn a_function_with_no_bit_count_in_it_is_left_exactly_as_it_was() {
        let (mut names, mut func) = one(&[Type::int(32)], &[Type::int(32)], |build, args| {
            build.ret(&[args[0]]);
        });
        let before = printed(&func, &mut names);
        counts(&mut func);
        assert_eq!(printed(&func, &mut names), before);
    }

    /// One overflow checked instruction whose value and whose flag are both returned, so that the
    /// substitution has two readers to find rather than none.
    fn checking(op: Opcode, width: u32) -> (Interner, Func) {
        let ty = Type::int(width);
        let bit = ty.with_lane(Type::I1);
        one(&[ty, ty], &[ty, bit], |build, args| {
            let (value, flag) = build.checked(op, args[0], args[1]);
            build.ret(&[value, flag]);
        })
    }

    /// An unsigned add wraps exactly when the sum came out below an operand, which is one
    /// comparison and no arithmetic on the sign bits.
    #[test]
    fn a_checked_unsigned_add_becomes_an_add_and_one_comparison() {
        let (mut names, mut func) = checking(Opcode::UAddOverflow, 32);
        overflows(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("uadd_overflow"), "the instruction is gone: {text}");
        assert_eq!(text.matches(" add ").count(), 1, "one add: {text}");
        assert_eq!(text.matches("icmp ult").count(), 1, "and one comparison: {text}");
        assert!(!text.contains(" xor "), "nothing about sign bits: {text}");
    }

    /// A signed add wraps exactly when the operands agreed in sign and the answer did not, which is
    /// the sign bit of `(a ^ v) & (b ^ v)`.
    #[test]
    fn a_checked_signed_add_becomes_an_add_and_the_sign_bit_of_two_exclusive_ors() {
        let (mut names, mut func) = checking(Opcode::SAddOverflow, 32);
        overflows(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("sadd_overflow"), "the instruction is gone: {text}");
        assert_eq!(text.matches(" add ").count(), 1, "one add: {text}");
        assert_eq!(text.matches(" xor ").count(), 2, "the answer against each operand: {text}");
        assert_eq!(text.matches(" and ").count(), 1, "both at once: {text}");
        assert!(text.contains("icmp slt"), "and its sign bit: {text}");
    }

    /// An unsigned subtract wraps exactly when the left operand was below the right, which does not
    /// need the answer at all.
    #[test]
    fn a_checked_unsigned_subtract_compares_the_operands_and_not_the_answer() {
        let (mut names, mut func) = checking(Opcode::USubOverflow, 64);
        overflows(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("usub_overflow"), "the instruction is gone: {text}");
        assert_eq!(text.matches(" sub ").count(), 1, "one subtract: {text}");
        assert!(text.contains("icmp ult %0, %1"), "the operands, in order: {text}");
    }

    /// A checked multiply is the ordinary multiply and the high half of the product, which is four
    /// multiplies of the halves and the carry between them.
    ///
    /// This is the expensive one and it is what tamnd/rucc#309 is mostly worth: a machine whose
    /// multiply writes the high half into a second register does the whole of it in one
    /// instruction.
    #[test]
    fn a_checked_multiply_becomes_a_multiply_and_the_high_half_of_the_product() {
        let (mut names, mut func) = checking(Opcode::UMulOverflow, 64);
        overflows(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("umul_overflow"), "the instruction is gone: {text}");
        assert_eq!(text.matches(" mul ").count(), 5, "the answer and the four halves: {text}");
        assert!(text.contains("iconst.i64 32"), "split at half the width: {text}");
        assert!(text.contains("iconst.i64 4294967295"), "and masked to it: {text}");
        assert!(text.contains("icmp ne"), "the high half against zero: {text}");
        assert!(!text.contains("ashr"), "and nothing corrected for sign: {text}");
    }

    /// The signed multiply is the unsigned one with the sign correction on top, and the test is
    /// against the sign extension of the low half rather than against zero.
    #[test]
    fn a_checked_signed_multiply_corrects_the_high_half_for_each_negative_operand() {
        let (mut names, mut func) = checking(Opcode::SMulOverflow, 64);
        overflows(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("smul_overflow"), "the instruction is gone: {text}");
        assert_eq!(
            text.matches(" ashr ").count(),
            3,
            "each operand's sign, and the answer: {text}"
        );
        assert!(text.contains("iconst.i64 63"), "spread from the top bit: {text}");
        assert_eq!(text.matches(" sub ").count(), 2, "one correction per operand: {text}");
    }

    /// Both results have to reach their readers, which is the one thing this pass has to do that
    /// the others do not: the instruction goes away rather than becoming another one, so nothing is
    /// left holding the values the rest of the function was reading.
    #[test]
    fn both_results_are_substituted_into_whoever_was_reading_them() {
        let (mut names, mut func) = checking(Opcode::SAddOverflow, 32);
        overflows(&mut func);

        // The whole of it, because what this is checking is that nothing is left pointing at the
        // two values the removed instruction used to define. The return names the add and the
        // comparison, which are what replaced them.
        let text = printed(&func, &mut names);
        assert_eq!(
            text,
            concat!(
                "func @f(i32, i32) -> (i32, i1), linkage(external) {\n",
                "block0(%0: i32, %1: i32):\n",
                "    %2 = add %0, %1\n",
                "    %3 = xor %0, %2\n",
                "    %4 = xor %1, %2\n",
                "    %5 = and %3, %4\n",
                "    %6 = iconst.i32 0\n",
                "    %7 = icmp slt %5, %6\n",
                "    return %2, %7\n",
                "}\n",
            ),
        );
    }

    /// The rewrites have to leave a function the verifier still accepts, for all six and at every
    /// width, because nothing rechecks what comes out of here.
    #[test]
    fn what_an_overflow_check_becomes_is_ir_that_verifies() {
        let all = [
            Opcode::UAddOverflow,
            Opcode::SAddOverflow,
            Opcode::USubOverflow,
            Opcode::SSubOverflow,
            Opcode::UMulOverflow,
            Opcode::SMulOverflow,
        ];
        for op in all {
            for width in [8u32, 16, 32, 64] {
                let (mut names, mut func) = checking(op, width);
                overflows(&mut func);
                let module = Module::new(names.intern("c.c"), &target());
                rucc_ir::verify_func(&module, &func, &names)
                    .unwrap_or_else(|e| panic!("{op:?} at {width}: {e:?}"));
            }
        }
    }

    /// A width the arithmetic is not written for is left as the instruction it was, for the same
    /// reason the bit counts leave one: a selector with no rule for it says so, which is better
    /// than an answer checked in the wrong shape.
    #[test]
    fn a_width_the_split_is_not_written_for_is_left_alone() {
        let (mut names, mut func) = checking(Opcode::UMulOverflow, 24);
        overflows(&mut func);
        assert!(printed(&func, &mut names).contains("umul_overflow"), "left as it was");
    }

    /// Nothing else is touched, for the same reason the other passes have that test.
    #[test]
    fn a_function_with_no_overflow_check_in_it_is_left_exactly_as_it_was() {
        let (mut names, mut func) = one(&[Type::int(32)], &[Type::int(32)], |build, args| {
            build.ret(&[args[0]]);
        });
        let before = printed(&func, &mut names);
        overflows(&mut func);
        assert_eq!(printed(&func, &mut names), before);
    }

    /// `T x = *p;` with an ordering on it, which is what `__atomic_load_n` becomes.
    fn reading(ty: Type, align: u32, order: MemOrder) -> (Interner, Func) {
        one(&[Type::PTR], &[ty], |build, args| {
            let info = MemInfo { order, ..access(0, align) };
            let value = build.atomic_load(ty, args[0], info, Flags::NONE);
            build.ret(&[value]);
        })
    }

    /// `*p = x;` with an ordering on it, which is what `__atomic_store_n` becomes.
    fn writing(ty: Type, align: u32, order: MemOrder) -> (Interner, Func) {
        one(&[Type::PTR, ty], &[], |build, args| {
            let info = MemInfo { order, ..access(0, align) };
            build.atomic_store(args[1], args[0], info, Flags::NONE);
            build.ret(&[]);
        })
    }

    /// Every ordered access below the strongest store is the plain instruction on this machine,
    /// and the ordering comes off it when it is.
    ///
    /// The ordering coming off is not cosmetic: the verifier refuses an ordering on a plain access,
    /// because a plain load may be moved, duplicated and dropped and an ordering left on one would
    /// be a claim nothing downstream honours.
    #[test]
    fn an_ordered_access_becomes_the_plain_one_this_machine_already_orders() {
        for order in [MemOrder::Relaxed, MemOrder::Acquire, MemOrder::SeqCst] {
            let (mut names, mut func) = reading(Type::int(32), 4, order);
            orderings(&mut func, 8);
            let text = printed(&func, &mut names);
            assert!(text.contains("load.i32"), "{order:?}: {text}");
            assert!(!text.contains("atomic_load"), "{order:?}: {text}");
            assert!(!text.contains(order.name()), "the ordering came off: {text}");
        }

        for order in [MemOrder::Relaxed, MemOrder::Release] {
            let (mut names, mut func) = writing(Type::int(32), 4, order);
            orderings(&mut func, 8);
            let text = printed(&func, &mut names);
            assert!(text.contains("store %1 -> %0"), "{order:?}: {text}");
            assert!(!text.contains("atomic_store"), "{order:?}: {text}");
            assert!(!text.contains("fence"), "{order:?} costs nothing here: {text}");
        }
    }

    /// The strongest store is the plain store and a barrier behind it, in that order.
    ///
    /// It is the one thing total store order does not give away: a store followed by a load of
    /// another address may be seen the other way round, and sequential consistency is exactly the
    /// ordering that forbids it.
    #[test]
    fn the_strongest_store_keeps_a_barrier_behind_it() {
        let (mut names, mut func) = writing(Type::int(32), 4, MemOrder::SeqCst);
        orderings(&mut func, 8);
        let text = printed(&func, &mut names);
        let (before, after) = text.split_once("fence seq_cst").expect("a barrier");
        assert!(before.contains("store %1 -> %0"), "the store comes first: {text}");
        assert!(!after.contains("store"), "and nothing is between them: {text}");
        assert!(!text.contains("atomic_store"), "{text}");
    }

    /// A barrier the program wrote is left for `crate::lower`, which is where a target says what
    /// an ordering costs.
    #[test]
    fn a_barrier_is_left_for_the_place_that_knows_what_one_costs() {
        for order in MemOrder::all().filter(|&order| order != MemOrder::NotAtomic) {
            let (mut names, mut func) = one(&[], &[], |build, _| {
                build.fence(order);
                build.ret(&[]);
            });
            let before = printed(&func, &mut names);
            orderings(&mut func, 8);
            assert_eq!(printed(&func, &mut names), before, "{order:?}");
        }
    }

    /// An access the machine cannot do in one go is left as the opcode it was, which is a refusal
    /// naming the instruction rather than an answer that is not atomic at all.
    ///
    /// Two ways it happens: wider than a word, and narrower than a word but at an address the
    /// program said less about than the width. Both are `__atomic_is_lock_free` answering no.
    #[test]
    fn an_access_this_machine_cannot_do_in_one_go_is_left_alone() {
        for (ty, align) in [(Type::int(128), 16), (Type::int(64), 4)] {
            let (mut names, mut func) = reading(ty, align, MemOrder::SeqCst);
            orderings(&mut func, 8);
            assert!(printed(&func, &mut names).contains("atomic_load"), "left as it was");
        }
    }

    /// What comes out is IR the verifier takes, which is the check that matters most here: the
    /// ordering has to be gone from a plain access or this pass has built something illegal.
    #[test]
    fn what_the_ordered_accesses_become_verifies() {
        for order in MemOrder::all().filter(|&order| order != MemOrder::NotAtomic) {
            for (mut names, mut func) in
                [reading(Type::int(32), 4, order), writing(Type::int(32), 4, order)]
            {
                if !order.is_valid_for_load() && !order.is_valid_for_store() {
                    continue;
                }
                orderings(&mut func, 8);
                let module = Module::new(names.intern("a.c"), &target());
                rucc_ir::verify_func(&module, &func, &names)
                    .unwrap_or_else(|e| panic!("{order:?}: {e:?}"));
            }
        }
    }

    /// Nothing else is touched, for the same reason the other passes have that test.
    #[test]
    fn a_function_with_no_ordered_access_in_it_is_left_exactly_as_it_was() {
        let (mut names, mut func) = one(&[Type::int(32)], &[Type::int(32)], |build, args| {
            build.ret(&[args[0]]);
        });
        let before = printed(&func, &mut names);
        orderings(&mut func, 8);
        assert_eq!(printed(&func, &mut names), before);
    }
}
