//! The integer that is wider than a register, as the two registers it is held in.
//!
//! `__int128` is the one integer a C program on this machine writes that no register holds.
//! Everything else the front end produces is a width the machine has, or is a width
//! [`crate::widths`] rounds up into one, and neither of those is true here: there is nothing to
//! round up into above sixty four bits. What there is, is two registers, and the convention already
//! says so. System V classifies a `__int128` as two eightbytes of class INTEGER, so it travels in a
//! pair of general purpose registers, comes back in the pair a return comes back in, and sits in
//! memory as two words with the low one first. That is what this pass writes down.
//!
//! Every value a hundred and twenty eight bits wide becomes two values of sixty four, a low half
//! and a high half, and every instruction over such a value becomes instructions over the halves.
//! After it there is no value of that width left anywhere in the function, which is what lets the
//! rest of the back end stay written about widths the machine has. Nothing below this knows the
//! type existed.
//!
//! # Why a pass and not a rule
//!
//! A rule matches a term and rewrites it into instructions of the machine, and the selector works a
//! value at a time. There is no register a value this wide can be selected into, so there is
//! nothing for a rule to produce, and a rule that produced a pair would have to say which register
//! each half landed in, which is the allocator's answer and not a rule's. So the splitting happens
//! before selection, in the IR, where a value is still something a pass may make two of. That is
//! the same reasoning [`crate::widths`] follows from the other end, and the two are the two halves
//! of one sentence: nothing reaching the selector is at a width the machine has no register for.
//!
//! # What crosses the boundary
//!
//! A parameter and a return value are agreed with something this compilation is not looking at, so
//! splitting one is a claim about where the two halves are. The claim is true when both halves land
//! in registers, because the convention hands out argument registers in order and two halves in a
//! row take the two registers the whole value would have taken. It is not true when they do not: a
//! value the convention could not fit in registers travels in the argument area as sixteen bytes
//! aligned to sixteen, and two independent words travel as two words each aligned to eight, which
//! is a different place as soon as an odd number of words went before them. So a function whose
//! wide parameter would run out of registers is left exactly as it was and refused by name, the
//! same as a function this pass does not understand. `tamnd/rucc#351` carries what passing one in
//! memory would take, which is a form of parameter the IR has no way to spell today.
//!
//! # What it does not do yet
//!
//! Dividing, which at this width is a call into the compiler runtime rather than arithmetic at all
//! and waits on the runtime having the four entry points to call. A function that divides one of
//! these is left alone here and refused by the selector, which is the same answer it got before
//! this pass existed.

use std::collections::{HashMap, HashSet};

use rucc_ir::{
    Abi, Block, BlockCall, CallInfo, Def, Extra, Flags, Func, Imm, Inst, InstData, IntPred,
    MemInfo, Opcode, Param, Signature, Type, Value,
};
use rucc_target::{CallRegs, Places, Where};

use crate::expand;

/// The width this pass is about, which is the one width a C program writes that no register holds.
const WIDE: u32 = 128;

/// The width each half is, which is a register on every target this pass runs for.
const HALF: u32 = 64;

/// How many bytes one half takes in memory, which is how far the high one sits above the low one.
const STEP: u64 = 8;

/// Whether a type is the width this pass splits.
fn is_wide(ty: Type) -> bool {
    ty.is_int() && ty.is_scalar() && ty.bits() == WIDE
}

/// The type each half has.
fn half() -> Type {
    Type::int(HALF)
}

/// Splits every integer the machine holds in two registers into the two halves it holds it in.
///
/// Gives back whether it changed anything, which is what a test asks and what tells a reader of a
/// dump that the function the selector saw is not the one the middle end produced.
///
/// The function is left exactly as it was when there is nothing at that width, when something at
/// that width is reached by an instruction this does not understand, and when a half would cross
/// the function's boundary somewhere the convention has no register for it. All three leave the
/// refusal to the passes below, which name the construct they could not lower, rather than
/// rewriting into something that guessed.
pub fn halves(func: &mut Func, conv: &CallRegs) -> bool {
    if !func.values().any(|value| is_wide(func[value].ty)) {
        return false;
    }
    let insts: Vec<Inst> =
        walk(func).into_iter().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    let order: HashMap<Inst, usize> =
        insts.iter().enumerate().map(|(at, &inst)| (inst, at)).collect();
    if !insts.iter().enumerate().all(|(at, &inst)| can_split(func, &order, at, inst)) {
        return false;
    }
    if !func.signatures().all(|signature| fits(signature, conv)) {
        return false;
    }

    let mut halves: Halves = HashMap::new();
    let mut forward: HashMap<Value, Value> = HashMap::new();
    for block in func.blocks().collect::<Vec<_>>() {
        params(func, block, &mut halves, &mut forward);
    }
    for &inst in &insts {
        rewrite(func, &mut halves, &mut forward, inst);
    }
    substitute(func, &forward);
    let signature = split_signature(func.signature());
    func.set_signature(signature);
    true
}

/// Every block, in an order where a block comes after everything that dominates it.
///
/// Reverse postorder from the entry, then whatever the walk did not reach, in the order the
/// function holds them. The order is what the rule below about a use and its definition is read
/// against, and the two together are the whole of why this is not simply the order the function
/// holds the blocks in: a value is defined in a block that dominates every block reading it, a
/// dominator is on every path from the entry, so a depth first walk finishes it last and reverse
/// postorder puts it first. The order the function holds blocks in says nothing of the kind. It is
/// the order they were made in, and every pass in the optimizer that makes a block, which is every
/// pass that gives a loop a preheader or copies a header in front of one, puts a block that runs
/// early at the end of that list. So the same program compiled at `-O0` and at `-O1` gave two
/// different answers to whether this pass understood it, and above `-O0` the answer was often no.
/// tamnd/rucc#1054.
///
/// A block nothing reaches cannot be walked to and is put at the end rather than dropped, because
/// deciding a block is unreachable is not this pass's business. Two of them in the wrong order
/// refuse the function the way they always did.
fn walk(func: &Func) -> Vec<Block> {
    let Some(entry) = func.entry() else { return func.blocks().collect() };
    let mut seen: HashSet<Block> = HashSet::new();
    let mut order: Vec<Block> = Vec::new();
    // A postorder without recursion: the second time a block comes off the stack every block below
    // it has been finished, so that is where it belongs in the postorder.
    let mut stack: Vec<(Block, bool)> = vec![(entry, false)];
    seen.insert(entry);
    while let Some((block, done)) = stack.pop() {
        if done {
            order.push(block);
            continue;
        }
        stack.push((block, true));
        let Some(term) = func.terminator(block) else { continue };
        for call in func.successors(term) {
            if seen.insert(call.block) {
                stack.push((call.block, false));
            }
        }
    }
    order.reverse();
    order.extend(func.blocks().filter(|block| !seen.contains(block)));
    order
}

/// The two halves each wide value became, low first.
type Halves = HashMap<Value, (Value, Value)>;

/// The opcodes this pass knows how to split.
///
/// An instruction that touches a value of this width and is not one of these is why the whole
/// function is left alone, so this list is the pass's own statement of what it has thought about.
/// Adding to it is adding an arm to [`rewrite`] as well.
///
/// The divisions are deliberately not here, and the module documentation says what they wait on. A
/// conversion between one of these and a floating point value is missing for a different reason:
/// the conversion the machine has stops at sixty four bits, so what is needed there is arithmetic
/// rather than a split, and it belongs beside the other conversions in [`crate::expand`].
fn understood(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::IConst
            | Opcode::Load
            | Opcode::Store
            | Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::ICmp
            | Opcode::Select
            | Opcode::Trunc
            | Opcode::SExt
            | Opcode::ZExt
            | Opcode::Call
            | Opcode::CallIndirect
            | Opcode::Return
            | Opcode::Jump
            | Opcode::BrIf
    )
}

/// Whether one instruction is one this pass can split, given where it is in the walk.
///
/// Asked of every instruction, and answered yes at once for the ones that never see a value this
/// wide, which in a function that has one at all is still most of them.
fn can_split(func: &Func, order: &HashMap<Inst, usize>, at: usize, inst: Inst) -> bool {
    let data = func[inst];
    let reads = operands(func, inst);
    let wide = |&value: &Value| is_wide(func[value].ty);
    if !reads.iter().any(wide) && !data.results().any(|value| is_wide(func[value].ty)) {
        return true;
    }
    if !understood(data.opcode) {
        return false;
    }
    // Memory SSA threads a version of memory through each access, and splitting one access into two
    // makes a version this pass would have to name. Nothing hands this crate a function carrying it
    // today, and leaving one alone costs less than being wrong about it later.
    if func.carries_mem(inst) {
        return false;
    }
    // The machine sign extends from a byte and no narrower, so a truth value widened into the high
    // half would become an instruction with no rule behind it. Zero extending one is fine, which is
    // why only the signed side is asked about.
    if data.opcode == Opcode::SExt && reads.iter().any(|&value| func[value].ty.bits() < 8) {
        return false;
    }
    // Splitting an argument makes two of them, and which parameter an argument stands for is how a
    // variadic call knows what the ABI asks of the ones its signature does not name. Two values
    // where that list has one entry is a call laid out against the wrong list.
    if matches!(data.opcode, Opcode::Call | Opcode::CallIndirect) {
        let Extra::Call(info) = data.extra else { return false };
        if func[func[info].signature].variadic {
            return false;
        }
    }
    // The halves of a value are written where the value was, so a use this pass reaches before the
    // definition is a use whose halves do not exist yet. A value arriving as a block parameter is
    // always ready, since every block's parameters are split before any instruction is.
    reads.iter().filter(|value| wide(value)).all(|&value| match func[value].def {
        Def::Result { inst, .. } => order.get(&inst).is_some_and(|&def| def < at),
        Def::Param { .. } => true,
    })
}

/// Everything an instruction reads: its own operands, and the arguments it passes along its edges.
///
/// The arguments of a `jump` and of a `br_if` hang on the block call rather than on the
/// instruction, so an instruction whose own operands are all narrow may still be handing a wide one
/// to the block it branches to.
fn operands(func: &Func, inst: Inst) -> Vec<Value> {
    let mut reads = func[func[inst].args].to_vec();
    for call in func.successors(inst).collect::<Vec<_>>() {
        reads.extend_from_slice(&func[call.args]);
    }
    reads
}

/// Whether both halves of every wide parameter of one signature land in registers.
///
/// The walk is the one [`crate::abi::entry`] makes, because the answer has to be the one that walk
/// will give: it hands out places in the order the signature holds the parameters, and a wide
/// parameter is about to become two halves in a row in that order. Both have to be registers. One
/// register and one word of the argument area is where two independent words go and is not where
/// the convention puts a sixteen byte value.
///
/// A return value is not asked about. What comes back comes back in the registers a return uses,
/// which is a sequence of its own with two in it on this convention, and a signature wanting more
/// than it has is refused by name in [`crate::lower`] already.
fn fits(signature: &Signature, conv: &CallRegs) -> bool {
    let mut places = Places::new(conv);
    for param in &signature.params {
        // A structure the classification put in the argument area, which is the one parameter whose
        // place is bytes rather than a register. Everything else is a value, the pointer an `sret`
        // hands over included, and a value takes the next register of its own kind.
        if let Abi::ByVal { size, align } = param.abi {
            places.on_stack(u32::try_from(size).unwrap_or(u32::MAX), align);
        } else if crate::abi::on_the_stack(param.ty) {
            let (size, align) = crate::abi::X87_AREA;
            places.on_stack(size, align);
        } else if is_wide(param.ty) {
            let low = places.integer();
            let high = places.integer();
            if !matches!((low, high), (Where::Reg(_), Where::Reg(_))) {
                return false;
            }
        } else if param.ty.is_float() {
            places.float();
        } else {
            places.integer();
        }
    }
    true
}

/// One block's parameters, with each wide one replaced by its two halves in the same position.
///
/// Every parameter of such a block is made again rather than only the wide ones, because a
/// parameter's position is its identity to the branches that feed it and appending is the only way
/// to add one. The narrow ones are made again as themselves and pointed at the copy, which costs
/// nothing once the substitution below has run.
fn params(func: &mut Func, block: Block, halves: &mut Halves, forward: &mut HashMap<Value, Value>) {
    let old: Vec<Value> = func[block].params.clone();
    if !old.iter().any(|&value| is_wide(func[value].ty)) {
        return;
    }
    for &value in &old {
        if is_wide(func[value].ty) {
            let low = func.append_param(block, half());
            let high = func.append_param(block, half());
            halves.insert(value, (low, high));
        } else {
            let again = func.append_param(block, func[value].ty);
            forward.insert(value, again);
        }
    }
    func.retain_params(block, |value| !old.contains(&value));
}

/// One instruction, as instructions over halves.
fn rewrite(func: &mut Func, halves: &mut Halves, forward: &mut HashMap<Value, Value>, inst: Inst) {
    let data = func[inst];
    let produces = data.results().any(|value| is_wide(func[value].ty));
    let takes = func[data.args].iter().any(|&value| is_wide(func[value].ty));
    match data.opcode {
        Opcode::IConst if produces => constant(func, halves, inst),
        Opcode::Load if produces => load(func, halves, inst),
        Opcode::Store if takes => store(func, halves, inst),
        Opcode::Add | Opcode::Sub if produces => carried(func, halves, inst, data.opcode),
        Opcode::Mul if produces => multiply(func, halves, inst),
        Opcode::Shl | Opcode::LShr | Opcode::AShr if produces => {
            shifted(func, halves, inst, data.opcode);
        }
        Opcode::And | Opcode::Or | Opcode::Xor if produces => {
            bitwise(func, halves, inst, data.opcode);
        }
        Opcode::ICmp if takes => compare(func, halves, forward, inst),
        Opcode::Select if produces => choose(func, halves, inst),
        Opcode::Trunc if takes => truncate(func, halves, forward, inst),
        Opcode::SExt | Opcode::ZExt if produces => {
            extend(func, halves, inst, data.opcode == Opcode::SExt);
        }
        Opcode::Call | Opcode::CallIndirect if produces || takes => {
            call(func, halves, forward, inst);
        }
        Opcode::Return if takes => flatten(func, halves, inst),
        Opcode::Jump | Opcode::BrIf => edges(func, halves, inst),
        _ => {}
    }
}

/// A constant, as the two halves of its bits with the low one first.
fn constant(func: &mut Func, halves: &mut Halves, inst: Inst) {
    let Extra::Imm(imm) = func[inst].extra else { return };
    let bits = func[imm].unsigned();
    #[expect(clippy::cast_possible_truncation, reason = "the halves are what this is taking")]
    let (low, high) = (bits as u64, (bits >> HALF) as u64);
    let low = ahead_const(func, inst, i128::from(low));
    let high = ahead_const(func, inst, i128::from(high));
    replace(func, halves, inst, low, high);
}

/// A read, as the two words of it with the low one first.
///
/// Little endian is the order, which is what every target this back end has is. The high word knows
/// less about its alignment than the low one when the low one knew more than a word, since a
/// sixteen byte object aligned to sixteen has its high word aligned to eight.
fn load(func: &mut Func, halves: &mut Halves, inst: Inst) {
    let data = func[inst];
    let Extra::Mem(mem) = data.extra else { return };
    let info = func[mem];
    let Some(&from) = func[data.args].first() else { return };
    let low = read(func, inst, from, word(info, 0), data.flags);
    let up = stepped(func, inst, from);
    let high = read(func, inst, up, word(info, STEP), data.flags);
    replace(func, halves, inst, low, high);
}

/// A write, as the two words of it.
fn store(func: &mut Func, halves: &mut Halves, inst: Inst) {
    let data = func[inst];
    let Extra::Mem(mem) = data.extra else { return };
    let info = func[mem];
    let args = func[data.args].to_vec();
    let [value, into] = args[..] else { return };
    let Some(&(low, high)) = halves.get(&value) else { return };
    write(func, inst, low, into, word(info, 0), data.flags);
    let up = stepped(func, inst, into);
    write(func, inst, high, up, word(info, STEP), data.flags);
    func.remove_inst(inst);
}

/// An add or a subtract, as the same over the low halves and the same again over the high ones with
/// what the low halves carried between them.
///
/// The carry is a comparison and not a flag. An unsigned sum comes out below either operand exactly
/// when it wrapped, and an unsigned difference wrapped exactly when the left operand was below the
/// right, which are the two tests [`crate::expand`] writes for the overflow builtins and are what
/// the machine's own carry flag stands for. Whether the pair is put back together into an `adc` and
/// an `sbb` is a question for what reads flags rather than for this, and the answer here is correct
/// either way.
fn carried(func: &mut Func, halves: &mut Halves, inst: Inst, opcode: Opcode) {
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    let (Some(&(a_low, a_high)), Some(&(b_low, b_high))) = (halves.get(&a), halves.get(&b)) else {
        return;
    };
    let low = ahead(func, inst, opcode, &[a_low, b_low]);
    let carried = if opcode == Opcode::Add {
        compared(func, inst, IntPred::Ult, low, a_low)
    } else {
        compared(func, inst, IntPred::Ult, a_low, b_low)
    };
    let carry = ahead(func, inst, Opcode::ZExt, &[carried]);
    let high = ahead(func, inst, opcode, &[a_high, b_high]);
    let high = ahead(func, inst, opcode, &[high, carry]);
    replace(func, halves, inst, low, high);
}

/// A multiply, which is long multiplication in base two to the sixty fourth with everything that
/// lands above the width thrown away.
///
/// The low half of the answer is the low halves multiplied together. The high half is what that
/// multiply carried out of its own top, plus the two cross products, each of which starts at bit
/// sixty four. The fourth partial product is the two high halves against each other and it starts
/// at bit one hundred and twenty eight, so the whole of it is above the width and it is never
/// worked out, which is why a wide multiply is three multiplies and not four.
///
/// Nothing here asks whether the operands are signed, because the low hundred and twenty eight bits
/// of a product are the same bits either way. The sign only matters to the bits that are being
/// thrown away.
///
/// The carry out of the low halves is the high half of a sixty four bit product, which this machine
/// has an instruction for and this compiler has no way to ask for. [`crate::expand`] already writes
/// that out as long multiplication one level further down, for the overflow builtins, so this calls
/// it rather than keeping a second copy of the same arithmetic. It is the expensive part of a wide
/// multiply by a long way, and `tamnd/rucc#309` is the rule that would make it one instruction for
/// both callers at once.
fn multiply(func: &mut Func, halves: &mut Halves, inst: Inst) {
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    let (Some(&(a_low, a_high)), Some(&(b_low, b_high))) = (halves.get(&a), halves.get(&b)) else {
        return;
    };
    let low = ahead(func, inst, Opcode::Mul, &[a_low, b_low]);
    let carried = expand::high_half(func, inst, a_low, b_low, false, half());
    let cross = ahead(func, inst, Opcode::Mul, &[a_low, b_high]);
    let other = ahead(func, inst, Opcode::Mul, &[a_high, b_low]);
    let high = ahead(func, inst, Opcode::Add, &[carried, cross]);
    let high = ahead(func, inst, Opcode::Add, &[high, other]);
    replace(func, halves, inst, low, high);
}

/// A shift, as each half shifted by the count with the bits that crossed between them put back, and
/// a second answer for a count that reached a whole half.
///
/// A count below sixty four moves each half by the count, and the bits that left one half are the
/// ones that arrive in the other. A count of sixty four or more empties one half completely, and
/// what lands in the other is the first half moved by the count less sixty four. Taking the sixty
/// four bit off a count in range is the same as subtracting sixty four from it, so both cases shift
/// by the same number of places and differ only in which value ends up where, which means one shift
/// each and a choice rather than two of everything. The choice is a `select` and not a branch, for
/// the reason [`choose`] gives.
///
/// The bits that cross move the other way by sixty four less the count. That is a shift of sixty
/// four places when the count is zero, which is not a distance this width has. Moving one place and
/// then sixty three less the count is the same distance for every count from one to sixty three,
/// and for a count of zero it shifts a value whose top bit is already gone all the way down to
/// nothing, which is the right answer: a half that did not move carries nothing into the other one.
///
/// A count of a hundred and twenty eight or more is undefined in C and nothing here goes out of its
/// way about it, the same as at every other width.
fn shifted(func: &mut Func, halves: &mut Halves, inst: Inst, opcode: Opcode) {
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    let (Some(&(a_low, a_high)), Some(&(count, _))) = (halves.get(&a), halves.get(&b)) else {
        return;
    };
    let top = ahead_const(func, inst, i128::from(HALF - 1));
    let places = ahead(func, inst, Opcode::And, &[count, top]);
    let back = ahead(func, inst, Opcode::Sub, &[top, places]);
    let one = ahead_const(func, inst, 1);
    let zero = ahead_const(func, inst, 0);
    let bit = ahead_const(func, inst, i128::from(HALF));
    let reach = ahead(func, inst, Opcode::And, &[count, bit]);
    let whole = compared(func, inst, IntPred::Ne, reach, zero);

    let (low, high) = if opcode == Opcode::Shl {
        let moved = ahead(func, inst, Opcode::Shl, &[a_low, places]);
        let edge = ahead(func, inst, Opcode::LShr, &[a_low, one]);
        let across = ahead(func, inst, Opcode::LShr, &[edge, back]);
        let above = ahead(func, inst, Opcode::Shl, &[a_high, places]);
        let joined = ahead(func, inst, Opcode::Or, &[above, across]);
        let low = ahead(func, inst, Opcode::Select, &[whole, zero, moved]);
        let high = ahead(func, inst, Opcode::Select, &[whole, moved, joined]);
        (low, high)
    } else {
        let moved = ahead(func, inst, opcode, &[a_high, places]);
        let edge = ahead(func, inst, Opcode::Shl, &[a_high, one]);
        let across = ahead(func, inst, Opcode::Shl, &[edge, back]);
        let below = ahead(func, inst, Opcode::LShr, &[a_low, places]);
        let joined = ahead(func, inst, Opcode::Or, &[below, across]);
        // What is left behind when the whole low half is gone: zeroes for a logical shift, and for
        // an arithmetic one the sign bit spread over the half it came from.
        let spent = if opcode == Opcode::AShr {
            ahead(func, inst, Opcode::AShr, &[a_high, top])
        } else {
            zero
        };
        let low = ahead(func, inst, Opcode::Select, &[whole, moved, joined]);
        let high = ahead(func, inst, Opcode::Select, &[whole, spent, moved]);
        (low, high)
    };
    replace(func, halves, inst, low, high);
}

/// An `and`, an `or` or an `xor`, which is the same operation on each half and nothing between
/// them.
fn bitwise(func: &mut Func, halves: &mut Halves, inst: Inst, opcode: Opcode) {
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    let (Some(&(a_low, a_high)), Some(&(b_low, b_high))) = (halves.get(&a), halves.get(&b)) else {
        return;
    };
    let low = ahead(func, inst, opcode, &[a_low, b_low]);
    let high = ahead(func, inst, opcode, &[a_high, b_high]);
    replace(func, halves, inst, low, high);
}

/// A comparison, which produces one bit and so is pointed at its answer rather than halved.
///
/// An equality is the two halves differing in neither place, which is one `or` over two `xor`s
/// against zero and is shorter than comparing twice and combining. An ordering is the high halves
/// settling it outright, or the low halves settling it when the high halves are equal, and the low
/// halves are compared without a sign because the low half of a signed number is unsigned whatever
/// the number is.
///
/// The high halves are asked a strict question even when the predicate is not strict. A predicate
/// that lets the two be equal is true of two equal high halves whatever the low halves say, and
/// what decides it there is the low halves, so `a >= b` is `a.hi > b.hi` or the high halves being
/// equal and `a.lo >= b.lo` unsigned. Asking `a.hi >= b.hi` instead makes every value with a high
/// half of its own greater than or equal to every other, which is the shape of this that a
/// differential run against GCC caught.
fn compare(func: &mut Func, halves: &Halves, forward: &mut HashMap<Value, Value>, inst: Inst) {
    let Extra::IntPred(pred) = func[inst].extra else { return };
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    let (Some(&(a_low, a_high)), Some(&(b_low, b_high))) = (halves.get(&a), halves.get(&b)) else {
        return;
    };
    let answer = if matches!(pred, IntPred::Eq | IntPred::Ne) {
        let low = ahead(func, inst, Opcode::Xor, &[a_low, b_low]);
        let high = ahead(func, inst, Opcode::Xor, &[a_high, b_high]);
        let both = ahead(func, inst, Opcode::Or, &[low, high]);
        let zero = ahead_const(func, inst, 0);
        compared(func, inst, pred, both, zero)
    } else {
        let above = compared(func, inst, strict(pred), a_high, b_high);
        let below = compared(func, inst, unsigned(pred), a_low, b_low);
        let same = compared(func, inst, IntPred::Eq, a_high, b_high);
        let tail = bit(func, inst, Opcode::And, same, below);
        bit(func, inst, Opcode::Or, above, tail)
    };
    if let Some(result) = func[inst].first_result {
        forward.insert(result, answer);
    }
    func.remove_inst(inst);
}

/// The same ordering with the equal case taken out of it, which is what the high halves are asked.
fn strict(pred: IntPred) -> IntPred {
    match pred {
        IntPred::Sle => IntPred::Slt,
        IntPred::Sge => IntPred::Sgt,
        IntPred::Ule => IntPred::Ult,
        IntPred::Uge => IntPred::Ugt,
        other => other,
    }
}

/// The same ordering with no sign in it, which is how the low halves of two signed numbers compare.
fn unsigned(pred: IntPred) -> IntPred {
    match pred {
        IntPred::Slt => IntPred::Ult,
        IntPred::Sle => IntPred::Ule,
        IntPred::Sgt => IntPred::Ugt,
        IntPred::Sge => IntPred::Uge,
        other => other,
    }
}

/// A choice between two wide values, which is the same choice made on each half.
///
/// Two of them rather than one, with the condition read twice. What that costs is one more
/// conditional move, and what the alternative costs is a branch, which is the more expensive of the
/// two on anything that predicts.
fn choose(func: &mut Func, halves: &mut Halves, inst: Inst) {
    let args = func[func[inst].args].to_vec();
    let [cond, then, other] = args[..] else { return };
    let (Some(&(then_low, then_high)), Some(&(other_low, other_high))) =
        (halves.get(&then), halves.get(&other))
    else {
        return;
    };
    let low = ahead(func, inst, Opcode::Select, &[cond, then_low, other_low]);
    let high = ahead(func, inst, Opcode::Select, &[cond, then_high, other_high]);
    replace(func, halves, inst, low, high);
}

/// Keeping the low bits of a wide value, which is the low half and then whatever is left to do.
///
/// Down to sixty four there is nothing left to do and the low half is the answer, so the truncation
/// goes and its readers read the half. Down to anything narrower the machine's own truncation still
/// happens, out of the half rather than out of the value that is no longer there.
fn truncate(func: &mut Func, halves: &Halves, forward: &mut HashMap<Value, Value>, inst: Inst) {
    let Some(&arg) = func[func[inst].args].first() else { return };
    let Some(&(low, _)) = halves.get(&arg) else { return };
    let Some(result) = func[inst].first_result else { return };
    if func[result].ty.bits() == HALF {
        forward.insert(result, low);
        func.remove_inst(inst);
        return;
    }
    becomes(func, inst, Opcode::Trunc, &[low]);
}

/// Widening into a wide value, which is the value in the low half and its own sign or zero above.
fn extend(func: &mut Func, halves: &mut Halves, inst: Inst, signed: bool) {
    let Some(&arg) = func[func[inst].args].first() else { return };
    let low = if func[arg].ty.bits() == HALF {
        arg
    } else {
        let opcode = if signed { Opcode::SExt } else { Opcode::ZExt };
        ahead(func, inst, opcode, &[arg])
    };
    let high = if signed {
        let top = ahead_const(func, inst, i128::from(HALF - 1));
        ahead(func, inst, Opcode::AShr, &[low, top])
    } else {
        ahead_const(func, inst, 0)
    };
    replace(func, halves, inst, low, high);
}

/// A call, as a call passing and receiving halves.
///
/// The instruction is made again rather than edited, because how many values a call gives back is
/// settled when it is created and a wide return value is two where it was one. Its signature is
/// made again for the same reason, since the signature is what each end of the call lays itself out
/// against and both ends are split the same way.
fn call(func: &mut Func, halves: &mut Halves, forward: &mut HashMap<Value, Value>, inst: Inst) {
    let data = func[inst];
    let Extra::Call(info) = data.extra else { return };
    let info = func[info];
    let args = spread(&func[data.args], halves);
    let results: Vec<Type> = data
        .results()
        .map(|value| func[value].ty)
        .flat_map(|ty| if is_wide(ty) { vec![half(), half()] } else { vec![ty] })
        .collect();
    let signature = func.add_signature(split_signature(&func[info.signature]));
    let extra = Extra::Call(func.add_call(CallInfo { signature, ..info }));
    let args = func.push_values(&args);
    let span = func.span(inst);
    let made = func.create_inst(InstData { args, extra, ..data }, &results, span);
    func.insert_before(made, inst);
    let mut fresh = func[made].results();
    for old in data.results() {
        if is_wide(func[old].ty) {
            let (Some(low), Some(high)) = (fresh.next(), fresh.next()) else { return };
            halves.insert(old, (low, high));
        } else if let Some(again) = fresh.next() {
            forward.insert(old, again);
        }
    }
    func.remove_inst(inst);
}

/// A `return`, whose operands are the values the signature says and so are halves now.
fn flatten(func: &mut Func, halves: &Halves, inst: Inst) {
    let args = spread(&func[func[inst].args], halves);
    func[inst].args = func.push_values(&args);
}

/// A branch, whose arguments hang on the edge rather than on the instruction.
fn edges(func: &mut Func, halves: &Halves, inst: Inst) {
    for at in func.target_list(inst).iter() {
        let call = func[at];
        let args = func[call.args].to_vec();
        if !args.iter().any(|value| halves.contains_key(value)) {
            continue;
        }
        let args = func.push_values(&spread(&args, halves));
        func.set_block_call(at, BlockCall { block: call.block, args });
    }
}

/// A list of values with each wide one replaced by its two halves in the same position.
fn spread(args: &[Value], halves: &Halves) -> Vec<Value> {
    args.iter()
        .flat_map(|value| match halves.get(value) {
            Some(&(low, high)) => vec![low, high],
            None => vec![*value],
        })
        .collect()
}

/// One signature with every wide parameter and return value as two halves in its place.
///
/// Each half is plain. What the ABI asks beyond a type is about the bits above a narrow value and
/// about an object whose address travels, and a half is neither: it is exactly a register wide and
/// it is the value itself.
fn split_signature(signature: &Signature) -> Signature {
    let split = |params: &[Param]| -> Vec<Param> {
        params
            .iter()
            .flat_map(|param| {
                if is_wide(param.ty) {
                    vec![Param::new(half()), Param::new(half())]
                } else {
                    vec![*param]
                }
            })
            .collect()
    };
    Signature {
        params: split(&signature.params),
        returns: split(&signature.returns),
        variadic: signature.variadic,
    }
}

/// Records the two halves an instruction became and takes the instruction out.
fn replace(func: &mut Func, halves: &mut Halves, inst: Inst, low: Value, high: Value) {
    if let Some(result) = func[inst].first_result {
        halves.insert(result, (low, high));
    }
    func.remove_inst(inst);
}

/// Points every reader of a value this pass replaced at what replaced it.
///
/// The arguments of each instruction and the arguments of the blocks it branches to, which between
/// them are everywhere a value can be read. Nothing chases, because every value this map answers
/// with is one made here and so is never itself a key.
fn substitute(func: &mut Func, forward: &HashMap<Value, Value>) {
    if forward.is_empty() {
        return;
    }
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

/// The access one word of a wide access is, that many bytes into it.
fn word(info: MemInfo, at: u64) -> MemInfo {
    let align = if at == 0 { info.align } else { info.align.min(8) };
    MemInfo { size: STEP, align, ..info }
}

/// The address one word past another, written in front of an instruction.
fn stepped(func: &mut Func, inst: Inst, from: Value) -> Value {
    let step = ahead_const(func, inst, i128::from(STEP));
    let args = func.push_values(&[from, step]);
    written(func, inst, InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
}

/// A load put in front of an instruction, and the half it reads.
fn read(func: &mut Func, inst: Inst, from: Value, info: MemInfo, flags: Flags) -> Value {
    let extra = Extra::Mem(func.add_mem(info));
    let args = func.push_values(&[from]);
    let data = InstData { args, flags, extra, ..InstData::new(Opcode::Load) };
    written(func, inst, data, half())
}

/// A store put in front of an instruction, which produces nothing and is only its effect.
fn write(func: &mut Func, inst: Inst, value: Value, into: Value, info: MemInfo, flags: Flags) {
    let span = func.span(inst);
    let extra = Extra::Mem(func.add_mem(info));
    let args = func.push_values(&[value, into]);
    let data = InstData { args, flags, extra, ..InstData::new(Opcode::Store) };
    let made = func.create_inst(data, &[], span);
    func.insert_before(made, inst);
}

/// A comparison written in front of an instruction, which carries its predicate where everything
/// else carries nothing.
fn compared(func: &mut Func, inst: Inst, pred: IntPred, lhs: Value, rhs: Value) -> Value {
    let args = func.push_values(&[lhs, rhs]);
    let extra = Extra::IntPred(pred);
    written(func, inst, InstData { args, extra, ..InstData::new(Opcode::ICmp) }, Type::I1)
}

/// An `and` or an `or` over two truth values, which is the same instruction at the width of one.
fn bit(func: &mut Func, inst: Inst, opcode: Opcode, lhs: Value, rhs: Value) -> Value {
    let args = func.push_values(&[lhs, rhs]);
    written(func, inst, InstData { args, ..InstData::new(opcode) }, Type::I1)
}

/// An instruction over these operands put in front of another one, producing a half.
fn ahead(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value]) -> Value {
    let args = func.push_values(args);
    written(func, inst, InstData { args, ..InstData::new(opcode) }, half())
}

/// A constant half put in front of an instruction.
fn ahead_const(func: &mut Func, inst: Inst, value: i128) -> Value {
    let extra = Extra::Imm(func.add_imm(Imm::int(value, half())));
    written(func, inst, InstData { extra, ..InstData::new(Opcode::IConst) }, half())
}

/// Creates the instruction, puts it in front of another, and reads its value back out.
fn written(func: &mut Func, inst: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(inst);
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    func[made].first_result.expect("an instruction created with one result has one")
}

/// Turns an instruction into a different one over different operands, in place.
fn becomes(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value]) {
    let args = func.push_values(args);
    let data = &mut func[inst];
    data.opcode = opcode;
    data.args = args;
    data.extra = Extra::None;
    data.flags = data.flags.intersection(Flags::legal_on(opcode));
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Flags, Func, MemOrder, Module, Restrict, Signature, Type, Value,
    };
    use rucc_target::x86_64::SYSV;
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{HALF, IntPred, MemInfo, Opcode, halves};

    /// The width the pass is about, as a type, which is what every test builds with.
    fn wide() -> Type {
        Type::int(super::WIDE)
    }

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("w.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    /// A function of those parameters returning that, with its entry block and its parameters.
    fn shell(names: &mut Interner, params: &[Type], returns: &[Type]) -> (Func, Block, Vec<Value>) {
        let signature = Signature::new().with_params(params).with_returns(returns);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let values = params.iter().map(|&ty| func.append_param(entry, ty)).collect();
        (func, entry, values)
    }

    /// An ordinary access of that many bytes, aligned that far.
    fn info(size: u64, align: u32) -> MemInfo {
        MemInfo {
            size,
            align,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        }
    }

    #[test]
    fn an_add_carries_from_the_low_half_into_the_high_one() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), wide()], &[wide()]);
        let mut build = Builder::new(&mut func, entry);
        let sum = build.binary(Opcode::Add, params[0], params[1], Flags::NONE);
        build.ret(&[sum]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i128"), "nothing that wide is left: {text}");
        // Two adds for the halves, one more for the carry, and the carry itself is the unsigned
        // comparison that says the low half wrapped.
        assert_eq!(text.matches(" = add ").count(), 3, "three adds: {text}");
        assert_eq!(text.matches("icmp ult").count(), 1, "one carry: {text}");
        assert_eq!(text.matches(" = zext.i64 ").count(), 1, "the carry as a number: {text}");
    }

    #[test]
    fn a_subtract_borrows_the_other_way_round() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), wide()], &[wide()]);
        let mut build = Builder::new(&mut func, entry);
        let difference = build.binary(Opcode::Sub, params[0], params[1], Flags::NONE);
        build.ret(&[difference]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert_eq!(text.matches(" = sub ").count(), 3, "three subtracts: {text}");
        // The borrow is the operands compared, not the answer, which is what tells a reader the
        // two directions were thought about separately.
        assert!(text.contains("icmp ult %0, %2"), "the operands are compared: {text}");
    }

    #[test]
    fn the_signature_and_the_entry_block_say_the_same_thing() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[Type::int(32), wide()], &[wide()]);
        let mut build = Builder::new(&mut func, entry);
        build.ret(&[params[1]]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        assert_eq!(
            func.signature().param_types().collect::<Vec<_>>(),
            [Type::int(32), Type::int(HALF), Type::int(HALF)],
            "the wide parameter became two where it stood"
        );
        assert_eq!(
            func.signature().return_types().collect::<Vec<_>>(),
            [Type::int(HALF), Type::int(HALF)],
            "and so did what comes back"
        );
        let text = printed(&func, &mut names);
        assert!(text.contains("block0(%0: i32, %1: i64, %2: i64)"), "the block agrees: {text}");
        assert!(text.contains("return %1, %2"), "both halves go back: {text}");
        let _ = entry;
    }

    #[test]
    fn a_read_takes_the_high_word_a_word_above_the_low_one() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[Type::PTR], &[wide()]);
        let mut build = Builder::new(&mut func, entry);
        let value = build.load(wide(), params[0], info(16, 16), Flags::NONE);
        build.ret(&[value]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert_eq!(text.matches(" = load.i64 ").count(), 2, "two reads: {text}");
        assert!(text.contains("ptr_add"), "the high word is a word up: {text}");
        // The object is aligned to sixteen and its high word is not, which is the one thing
        // splitting an access can get wrong quietly.
        assert!(text.contains("align 16"), "the low word keeps what the object had: {text}");
        assert!(text.contains("align 8"), "the high word knows less: {text}");
    }

    #[test]
    fn an_equality_asks_once_about_both_halves() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), wide()], &[Type::int(32)]);
        let mut build = Builder::new(&mut func, entry);
        let same = build.icmp(IntPred::Eq, params[0], params[1]);
        let answer = build.unary(Opcode::ZExt, same, Type::int(32));
        build.ret(&[answer]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert_eq!(text.matches("icmp").count(), 1, "one comparison: {text}");
        assert_eq!(text.matches(" = xor ").count(), 2, "the halves differ or they do not: {text}");
    }

    #[test]
    fn an_ordering_reads_the_low_halves_without_a_sign() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), wide()], &[Type::int(32)]);
        let mut build = Builder::new(&mut func, entry);
        let below = build.icmp(IntPred::Slt, params[0], params[1]);
        let answer = build.unary(Opcode::ZExt, below, Type::int(32));
        build.ret(&[answer]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert!(text.contains("icmp slt"), "the high halves keep the sign: {text}");
        assert!(text.contains("icmp ult"), "the low halves have none: {text}");
        assert!(
            text.contains("icmp eq"),
            "and the low halves only matter when the high tie: {text}"
        );
    }

    /// An ordering that allows the two to be equal still asks the high halves a strict question.
    ///
    /// Two values whose high halves are equal are ordered by their low halves alone, and a high
    /// half that is greater than or equal to the other says nothing about that. Asking the high
    /// halves the predicate as it stands makes every ordering that is not strict answer yes on a
    /// tie, which is the mistake a run against GCC caught.
    #[test]
    fn an_ordering_that_allows_equality_asks_the_high_halves_a_strict_question() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), wide()], &[Type::int(32)]);
        let mut build = Builder::new(&mut func, entry);
        let at_least = build.icmp(IntPred::Sge, params[0], params[1]);
        let answer = build.unary(Opcode::ZExt, at_least, Type::int(32));
        build.ret(&[answer]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert!(text.contains("icmp sgt"), "the high halves settle it outright: {text}");
        assert!(!text.contains("icmp sge"), "a tie in the high halves settles nothing: {text}");
        assert!(text.contains("icmp uge"), "the low halves are the ones allowed to tie: {text}");
    }

    #[test]
    fn a_widening_puts_the_sign_of_the_value_in_the_high_half() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[Type::int(32)], &[wide()]);
        let mut build = Builder::new(&mut func, entry);
        let value = build.unary(Opcode::SExt, params[0], wide());
        build.ret(&[value]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert!(text.contains("sext.i64"), "the value fills the low half: {text}");
        assert!(text.contains("ashr"), "and its sign fills the high one: {text}");
    }

    #[test]
    fn a_block_parameter_becomes_two_and_every_branch_passes_two() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), Type::int(32)], &[wide()]);
        let tail = func.create_block();
        let carried = func.append_param(tail, wide());
        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        let taken = build.icmp(IntPred::Ne, params[1], zero);
        let other = build.iconst(wide(), 7);
        build.br_if(taken, tail, &[params[0]], tail, &[other]);
        let mut build = Builder::new(&mut func, tail);
        build.ret(&[carried]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i128"), "nothing that wide is left: {text}");
        assert!(text.contains("block1(%7: i64, %8: i64)"), "the block takes two: {text}");
        assert_eq!(text.matches("block1(").count(), 3, "and both edges pass two: {text}");
    }

    /// A multiply is the low halves, the two cross products, and nothing for the fourth corner.
    ///
    /// Three at the top, and four more inside the carry out of the low halves, which is a product
    /// at half the width again worked out the same way. What the count is really saying is that the
    /// two high halves are never multiplied together, because the whole of that partial product
    /// lands above the width.
    #[test]
    fn a_multiply_is_three_multiplies_and_the_carry_out_of_the_low_ones() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), wide()], &[wide()]);
        let mut build = Builder::new(&mut func, entry);
        let product = build.binary(Opcode::Mul, params[0], params[1], Flags::NONE);
        build.ret(&[product]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i128"), "nothing that wide is left: {text}");
        assert_eq!(text.matches(" = mul ").count(), 7, "three and the carry's four: {text}");
    }

    /// A shift left moves each half and chooses between the count having crossed a half and not.
    ///
    /// Two shifts left, one per half, and the low one does for both cases: a count that reached a
    /// whole half puts exactly that value in the high half and nothing in the low one, so the only
    /// thing the far case needs is the shift the near case already did. Two right shifts carry the
    /// crossing bits, two selects pick a half each, and there is no branch anywhere.
    #[test]
    fn a_shift_left_chooses_between_a_count_that_crossed_a_half_and_one_that_did_not() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), wide()], &[wide()]);
        let mut build = Builder::new(&mut func, entry);
        let moved = build.binary(Opcode::Shl, params[0], params[1], Flags::NONE);
        build.ret(&[moved]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i128"), "nothing that wide is left: {text}");
        assert_eq!(
            text.matches(" = shl ").count(),
            2,
            "one per half, and the far case reuses one: {text}"
        );
        assert_eq!(text.matches(" = select.i64 ").count(), 2, "one choice per half: {text}");
        assert_eq!(text.matches(" = lshr ").count(), 2, "the crossing bits, in two steps: {text}");
    }

    /// The bits that cross move one place and then the rest, so a count of zero carries nothing.
    ///
    /// Sixty four less a count of zero is sixty four, which is not a distance a sixty four bit shift
    /// has. One place first and sixty three less the count after is the same distance everywhere the
    /// question is asked, and for a count of zero it moves a value whose top bit has already gone
    /// all the way out, which leaves the zero a half that did not move should carry.
    #[test]
    fn the_bits_that_cross_move_one_place_and_then_the_rest_of_the_way() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), wide()], &[wide()]);
        let mut build = Builder::new(&mut func, entry);
        let moved = build.binary(Opcode::LShr, params[0], params[1], Flags::NONE);
        build.ret(&[moved]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert!(text.contains("iconst.i64 63"), "sixty three is the distance left: {text}");
        assert!(text.contains("iconst.i64 1"), "after the one place that comes first: {text}");
        assert!(text.contains(" = sub "), "the rest of the way is worked out: {text}");
        assert!(
            !text.contains("iconst.i64 127"),
            "and the count is not masked to the width: {text}"
        );
    }

    /// An arithmetic shift right leaves the sign bit behind where a logical one leaves zeroes.
    ///
    /// What the two differ in is only the half the count moved out of entirely. A logical shift puts
    /// zeroes there, which is a constant already in hand, and an arithmetic one puts the sign bit
    /// spread across the half it came from, which is one more shift.
    #[test]
    fn an_arithmetic_shift_right_leaves_the_sign_bit_where_a_logical_one_leaves_zeroes() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide(), wide()], &[wide()]);
        let mut build = Builder::new(&mut func, entry);
        let moved = build.binary(Opcode::AShr, params[0], params[1], Flags::NONE);
        build.ret(&[moved]);

        assert!(halves(&mut func, &SYSV), "there is a width to split");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i128"), "nothing that wide is left: {text}");
        // The high half by the count, and the high half by sixty three for the half left empty.
        assert_eq!(text.matches(" = ashr ").count(), 2, "the count and the sign: {text}");
        assert_eq!(text.matches(" = lshr ").count(), 1, "the low half is not signed: {text}");
        assert_eq!(text.matches(" = select.i64 ").count(), 2, "one choice per half: {text}");
    }

    #[test]
    fn a_parameter_with_one_register_left_leaves_the_function_alone() {
        let mut names = Interner::new();
        let word = Type::int(HALF);
        // Five words take five of the six argument registers, so the halves of the sixth
        // parameter would be one register and one word of the caller's stack, which is not where
        // the convention puts a value this wide.
        let params = [word, word, word, word, word, wide()];
        let (mut func, entry, values) = shell(&mut names, &params, &[word]);
        let mut build = Builder::new(&mut func, entry);
        let low = build.unary(Opcode::Trunc, values[5], word);
        build.ret(&[low]);
        let before = printed(&func, &mut names);

        assert!(!halves(&mut func, &SYSV), "one of the halves has no register");
        assert_eq!(printed(&func, &mut names), before, "so nothing moved");
    }

    /// The order the function holds its blocks in is not the order they run in.
    ///
    /// This is what the optimizer produced and `-O0` did not. A block that runs early is made late
    /// by whichever pass needed it, so the list the function keeps had a use of a wide value in it
    /// before the instruction defining that value, and the walk that asks whether every definition
    /// comes first said no and left the whole function alone. Then the selector met an instruction
    /// at a width it has no register for and refused the program. The blocks here are made in the
    /// order that produces, which is the tail before the middle. tamnd/rucc#1054.
    #[test]
    fn a_block_made_after_the_one_it_runs_before_is_still_split() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[wide()], &[wide()]);
        let tail = func.create_block();
        let middle = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        build.jump(middle, &[]);
        let mut build = Builder::new(&mut func, middle);
        let doubled = build.binary(Opcode::Add, params[0], params[0], Flags::NONE);
        build.jump(tail, &[]);
        let mut build = Builder::new(&mut func, tail);
        let again = build.binary(Opcode::Add, doubled, doubled, Flags::NONE);
        build.ret(&[again]);

        assert!(
            halves(&mut func, &SYSV),
            "the definition runs before the use whatever the list says"
        );
        let text = printed(&func, &mut names);
        assert!(!text.contains("i128"), "nothing that wide is left: {text}");
    }

    #[test]
    fn a_function_with_nothing_that_wide_is_not_touched() {
        let mut names = Interner::new();
        let word = Type::int(HALF);
        let (mut func, entry, params) = shell(&mut names, &[word, word], &[word]);
        let mut build = Builder::new(&mut func, entry);
        let sum = build.binary(Opcode::Add, params[0], params[1], Flags::NONE);
        build.ret(&[sum]);

        assert!(!halves(&mut func, &SYSV), "there is nothing to split");
    }
}
