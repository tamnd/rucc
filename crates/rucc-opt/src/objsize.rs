//! The answer to every `object_size` the front end left for the IR.
//!
//! Design: `spec/optimizer/20-idioms-and-libcalls.md` section 20.2, and `spec/13-gnu-compat.md`
//! section 13.5 for the builtin itself.
//!
//! The checker answers `__builtin_object_size` where it can see the object by looking at the
//! expression, and leaves [`Opcode::ObjectSize`] behind where it cannot. What is left is an address
//! read out of a variable, and inside a function that variable is usually one of a handful of
//! addresses a branch or a loop chose between, as in `r = l == 1 ? &a.buf1[5] : &a.buf2[4]`. The
//! IR as the front end writes it has every one of those in front of it: the variable is a block
//! parameter and each branch to its block passes one address, which is an `alloca` or a global with
//! constant offsets on top. This walks that and writes the answer in as a constant.
//!
//! # When it runs
//!
//! Before every other pass and at every level, because nothing after the front end is allowed to
//! see the instruction. The `_chk` folds in [`crate::libcall`] run next and read the answer as the
//! constant they compare a count against, which is the order gcc's own object size pass and its
//! `_chk` folds run in. At `-O0` nothing is walked and every question gets the answer that says
//! nothing, which is what gcc 16.2.0 gives at that level for an address in a variable.
//!
//! # What the walk believes
//!
//! A fixed `alloca` is its size and a dynamic one of a constant count is that count. A global is
//! its size where `extents::vouched` says the definition in the module is the one that
//! will run. What a call returns is the product of the arguments its callee's `alloc_size` names,
//! where each of them is a constant, which is how `p = malloc(10)` has ten bytes behind it. A `ptr_add` of a constant count takes it off what is left, down to nothing past the
//! end, and one going backwards is not followed. A block parameter is every argument every branch
//! to its block passes and a `select` is both of its arms. Anything else is not known.
//!
//! The kinds asking for the largest answer take the largest of a choice and the kinds asking for
//! the smallest take the smallest, and not knowing any part of a choice is not knowing the whole
//! of it. A loop is the one place that needs thought. A parameter reached again while its own
//! answer is being worked out is the pointer carried round the loop unchanged or moved forward,
//! which can only leave less, so for the largest it adds nothing to the choice. Moved backwards it
//! could leave more, and that is why a backwards `ptr_add` is never followed. For the smallest the
//! same holds where the pointer comes round unchanged, and one moved forward each time round may
//! leave as little as anything, so there the smallest is not known.
//!
//! `__builtin_dynamic_object_size` sets a third bit on the kind, and where the walk finds no
//! constant for one of those, an address that is an allocator's result with constant offsets on it
//! is answered with the size the call asked for, multiplied out and less the offset in front of the
//! question, as gcc 16 answers it from `-O1` up. See `Walk::running` for which shapes.
//!
//! The dynamic spelling of a kind that asks for the largest answer has one more way to an answer.
//! Where every way to the address starts at the same fixed local or the same global, the answer is
//! the size of that object less how far into it the address is, worked out when the program runs
//! and never below zero. On the way any step is followed, whatever its count and its direction, and
//! so is a library call that gives back an address in the object its first argument is in: a copy
//! or a fill, the `p` spellings that give back the end of what they wrote, the `_chk` form of each,
//! and `strchr (s, 0)`, which gcc reads as `s + strlen (s)`. This is how gcc 16 answers it from
//! `-O1` up, and tcc's `c2str`, which reads each line with `fgets (p, sizeof l - (p - l), fp)` at
//! a `p` a loop moves, gets `__fgets_chk` from both compilers this way. See `Walk::within`.
//!
//! The closest member, which is the low bit of the kind, is what a `ptr_add` that lowering wrote
//! for stepping to a member says, as `Extra::Member`. An address the walk finds to be one of
//! those, or one a constant further on, is in that member, which is how `strcpy (inst.buf, s)`
//! asks about the sixteen bytes of `buf` once the fortified `strcpy` is inlined, where gcc asks
//! the same. Where the walk gets to an object without passing one, the whole object is an answer
//! no smaller than the member for the largest, so the first kind's answer stands for the second,
//! and for the smallest it could be too big, so there the fourth kind is not known. A step whose
//! count the program worked out, which lowering marks `counted`, is not taken off the member,
//! since gcc's early pass asks before any constant reaches it, and only what is left of the whole
//! object can make the answer smaller. Once every question is answered the members and the marks
//! are taken off, so no pass after this sees one.

use rucc_ir::{
    AllocSize, AttrSet, Def, Extra, Flags, Func, FuncId, Imm, Inst, InstData, IntPred, Module,
    Opcode, Pic, SymbolRef, Type, Value,
};

use std::cell::RefCell;

use rucc_base::hash::{Map, Set};
use rucc_base::{Interner, Symbol};
use rucc_ir::Block;

use crate::Cfg;
use crate::dom::Dominators;
use crate::extents::vouched;

/// How far a walk goes before it gives up, which is a chain of block parameters and `ptr_add`
/// this long.
const DEPTH: u32 = 16;

/// Answers every `object_size` in the module and says how many there were.
///
/// `look` is false at `-O0`, where every question is answered as not known. A function that
/// `optimize ("O0")` holds to that level is answered the same way whatever `look` is, since gcc
/// reads the level off the function the question is in.
///
/// `library` is the names a call may be read as the standard function under: `None` with
/// `-fno-builtin`, and otherwise every name but the ones `-fno-builtin-<name>` took away.
pub fn answer(
    module: &mut Module,
    names: &Interner,
    library: Option<&[String]>,
    pic: Pic,
    look: bool,
) -> usize {
    let mut answered = 0;
    for id in module.funcs().collect::<Vec<FuncId>>() {
        if module[id].is_declaration() {
            continue;
        }
        let asked = questions(&module[id]);
        if asked.is_empty() {
            continue;
        }
        let look = look && !module[id].attrs.set.contains(AttrSet::OPTNONE);
        let answers: Vec<(Inst, Answer)> = {
            let func = &module[id];
            let cfg = Cfg::new(func);
            let dom = Dominators::new(&cfg);
            let mut walk = Walk {
                module,
                func,
                cfg: &cfg,
                dom: &dom,
                pic,
                names,
                library,
                live: Set::default(),
                numbers: RefCell::default(),
            };
            // Twice, since the first round reads every store and every way into a block, and
            // what it rules out can settle a branch the second round reads.
            for _ in 0..2 {
                walk.live = walk.live_edges();
                walk.numbers.borrow_mut().clear();
            }
            asked
                .iter()
                .map(|&inst| {
                    let Extra::Question(asked) = func[inst].extra else {
                        return (inst, Answer::Known(0));
                    };
                    let address = func[func[inst].args][0];
                    let (kind, dynamic) = (asked & 3, asked & DYNAMIC != 0);
                    let ask = Ask { largest: kind & 2 == 0, closest: kind & 1 == 1 };
                    let largest = ask.largest;
                    let known = match look {
                        false => None,
                        true => walk.left(address, ask, DEPTH, &mut Vec::new()).ok().flatten(),
                    };
                    let unknown = if largest { -1 } else { 0 };
                    let answer = match known {
                        Some(known) => Answer::Known(i128::from(known)),
                        // Only the dynamic spelling may be answered with something worked out
                        // while the program runs, and only where the walk found no constant.
                        None if look && dynamic && kind != SMALLEST_MEMBER => func[inst]
                            .results()
                            .next()
                            .and_then(|result| walk.running(address, func[result].ty, DEPTH))
                            .map(Answer::Running)
                            .or_else(|| largest.then(|| walk.within(address, inst, ask)).flatten())
                            .unwrap_or(Answer::Known(unknown)),
                        None => Answer::Known(unknown),
                    };
                    (inst, answer)
                })
                .collect()
        };
        let func = &mut module[id];
        for (inst, answer) in answers {
            match answer {
                Answer::Known(number) => write(func, inst, number),
                Answer::Running(running) => build(func, inst, running),
                Answer::Within(object, size) => measure(func, inst, object, size),
            }
            answered += 1;
        }
    }
    for id in module.funcs().collect::<Vec<FuncId>>() {
        forget(&mut module[id]);
    }
    answered
}

/// Takes every `Extra::Member` and every `counted` off, which nothing after the questions reads.
fn forget(func: &mut Func) {
    for block in func.blocks().collect::<Vec<_>>() {
        for inst in func.insts(block).collect::<Vec<_>>() {
            if let Extra::Member(_) = func[inst].extra {
                func[inst].extra = Extra::None;
            }
            func[inst].flags = func[inst].flags.without(Flags::COUNTED);
        }
    }
}

/// Which of the four questions a walk answers.
#[derive(Clone, Copy)]
struct Ask {
    /// The largest answer rather than the smallest.
    largest: bool,
    /// The closest member rather than the whole object.
    closest: bool,
}

/// The bit above the two of the kind that says the question was asked with
/// `__builtin_dynamic_object_size`, which may be answered with a value worked out at run time.
const DYNAMIC: u8 = 4;

/// The fourth kind, the smallest answer for the closest member, which the walk cannot give.
const SMALLEST_MEMBER: u8 = 3;

/// What one question is answered with.
enum Answer {
    /// A constant, which is every answer the walk finds and every answer that says nothing.
    Known(i128),
    /// The size an allocator was asked for, worked out where the question is.
    Running(Running),
    /// The size of the object the address is in, less how far in the address is, worked out where
    /// the question is.
    Within(Object, u64),
}

/// The object an address is in, as [`Walk::within`] found it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Object {
    /// A fixed local, which is the `alloca` that made it.
    Local(Value),
    /// A global, by its name.
    Global(Symbol),
}

/// An address that is what an allocator gave back with a constant number of bytes added, as
/// [`Walk::running`] found it.
struct Running {
    /// The arguments that multiply to the size, one or two of them.
    factors: Vec<Value>,
    /// How far into the object the address is.
    offset: u64,
}

/// Every `object_size` in the function.
fn questions(func: &Func) -> Vec<Inst> {
    func.blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::ObjectSize)
        .collect()
}

/// Puts the constant in place of the question and takes the question away.
fn write(func: &mut Func, inst: Inst, number: i128) {
    let result = func[inst].results().next().expect("an object size is one value");
    let ty = func[result].ty;
    let span = func.span(inst);
    let imm = func.add_imm(Imm::int(number, ty.lane()));
    let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    let value = func[made].results().next().expect("a constant is one value");
    let forward: Map<_, _> = [(result, value)].into_iter().collect();
    crate::uses::substitute(func, &forward);
    func.remove_inst(inst);
}

/// Puts the size an allocator was asked for in place of the question, less how far in the address
/// is and never below zero, which is what gcc 16 answers `__builtin_dynamic_object_size(p + 2, 0)`
/// with for a `p` that came out of `malloc(n)`.
fn build(func: &mut Func, inst: Inst, running: Running) {
    let result = func[inst].results().next().expect("an object size is one value");
    let ty = func[result].ty;
    let span = func.span(inst);
    let made = |func: &mut Func, data: InstData, ty: Type| {
        let at = func.create_inst(data, &[ty], span);
        func.insert_before(at, inst);
        func[at].results().next().expect("one result was asked for")
    };
    let mut size = running.factors[0];
    for &factor in &running.factors[1..] {
        let args = func.push_values(&[size, factor]);
        size = made(func, InstData { args, ..InstData::new(Opcode::Mul) }, ty);
    }
    if running.offset != 0 {
        let imm = func.add_imm(Imm::int(i128::from(running.offset), ty.lane()));
        let offset =
            made(func, InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty);
        let imm = func.add_imm(Imm::int(0, ty.lane()));
        let zero =
            made(func, InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty);
        let args = func.push_values(&[size, offset]);
        let test =
            InstData { args, extra: Extra::IntPred(IntPred::Ugt), ..InstData::new(Opcode::ICmp) };
        let room = made(func, test, ty.with_lane(Type::I1));
        let args = func.push_values(&[size, offset]);
        let left = made(func, InstData { args, ..InstData::new(Opcode::Sub) }, ty);
        let args = func.push_values(&[room, left, zero]);
        size = made(func, InstData { args, ..InstData::new(Opcode::Select) }, ty);
    }
    let forward: Map<_, _> = [(result, size)].into_iter().collect();
    crate::uses::substitute(func, &forward);
    func.remove_inst(inst);
}

/// Puts the size of the object less how far into it the address is in place of the question, and
/// never below zero, which is what gcc 16 answers `__builtin_dynamic_object_size (p, 1)` with for
/// a `p` that a loop moves through a local array.
fn measure(func: &mut Func, inst: Inst, object: Object, size: u64) {
    let result = func[inst].results().next().expect("an object size is one value");
    let ty = func[result].ty;
    let address = func[func[inst].args][0];
    let span = func.span(inst);
    let made = |func: &mut Func, data: InstData, ty: Type| {
        let at = func.create_inst(data, &[ty], span);
        func.insert_before(at, inst);
        func[at].results().next().expect("one result was asked for")
    };
    let start = match object {
        Object::Local(start) => start,
        // The address of a global is taken again here, since the one the walk passed may be in a
        // block that does not come before the question.
        Object::Global(name) => made(
            func,
            InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        ),
    };
    let args = func.push_values(&[address]);
    let at = made(func, InstData { args, ..InstData::new(Opcode::PtrToInt) }, ty);
    let args = func.push_values(&[start]);
    let from = made(func, InstData { args, ..InstData::new(Opcode::PtrToInt) }, ty);
    let args = func.push_values(&[at, from]);
    let offset = made(func, InstData { args, ..InstData::new(Opcode::Sub) }, ty);
    let imm = func.add_imm(Imm::int(i128::from(size), ty.lane()));
    let size = made(func, InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty);
    let imm = func.add_imm(Imm::int(0, ty.lane()));
    let zero = made(func, InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty);
    let args = func.push_values(&[size, offset]);
    let test =
        InstData { args, extra: Extra::IntPred(IntPred::Ugt), ..InstData::new(Opcode::ICmp) };
    let room = made(func, test, ty.with_lane(Type::I1));
    let args = func.push_values(&[size, offset]);
    let left = made(func, InstData { args, ..InstData::new(Opcode::Sub) }, ty);
    let args = func.push_values(&[room, left, zero]);
    let answer = made(func, InstData { args, ..InstData::new(Opcode::Select) }, ty);
    let forward: Map<_, _> = [(result, answer)].into_iter().collect();
    crate::uses::substitute(func, &forward);
    func.remove_inst(inst);
}

/// What a walk over one function reads.
struct Walk<'a> {
    module: &'a Module,
    func: &'a Func,
    cfg: &'a Cfg,
    dom: &'a Dominators,
    pic: Pic,
    names: &'a Interner,
    /// The names a call may be read as the standard function under, as [`answer`] says.
    library: Option<&'a [String]>,
    /// The edges a branch on something already a constant here does not rule out, or empty
    /// while that is still being worked out, which counts every edge.
    live: Set<(Block, Block)>,
    /// What [`Walk::number`] found for each value it was asked about, so that a condition many
    /// branches share is worked out once. Emptied whenever `live` changes, since what it found
    /// rests on that.
    numbers: RefCell<Map<Value, Option<(Imm, Type)>>>,
}

/// How many bytes are left in front of an address: `Err` where that is not known, `Ok(None)` where
/// the only way to the address is round a loop back to itself, and otherwise the number.
type Left = Result<Option<u64>, ()>;

impl Walk<'_> {
    /// How many bytes there are from this address to the end of its object.
    ///
    /// `on` is the block parameters whose answer is being worked out, which is how a loop is seen.
    fn left(&self, value: Value, ask: Ask, depth: u32, on: &mut Vec<Value>) -> Left {
        let largest = ask.largest;
        let depth = depth.checked_sub(1).ok_or(())?;
        match self.func[value].def {
            Def::Param { block, index } => {
                if on.contains(&value) {
                    // Round a loop to where the walk started, which adds nothing to the choice for
                    // the reason the module comment gives. The `ptr_add` below is what decides
                    // whether the smallest can say the same.
                    return Ok(None);
                }
                let preds = self.cfg.predecessors(block);
                if preds.is_empty() {
                    return Err(());
                }
                on.push(value);
                let mut all = Ok(None);
                for &pred in preds {
                    // The `return NULL` of an overflow check on a size that does not overflow,
                    // which `kmalloc_array` has, comes in on an edge that cannot be taken.
                    if !self.taken(pred, block) {
                        continue;
                    }
                    let term = self.func.terminator(pred).ok_or(())?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize).ok_or(())?;
                        all = both(all, self.left(arg, ask, depth, on), largest);
                    }
                }
                on.pop();
                all
            }
            Def::Result { inst, .. } => {
                let data = &self.func[inst];
                let args = &self.func[data.args];
                match data.opcode {
                    Opcode::Select => {
                        let (then, other) = (*args.get(1).ok_or(())?, *args.get(2).ok_or(())?);
                        let then = self.left(then, ask, depth, on);
                        both(then, self.left(other, ask, depth, on), largest)
                    }
                    // The start of a member, which is the closest object to an address in it.
                    Opcode::PtrAdd if ask.closest && matches!(data.extra, Extra::Member(_)) => {
                        let Extra::Member(size) = data.extra else { return Err(()) };
                        Ok(Some(u64::from(size)))
                    }
                    // An object reached without passing the start of a member, which for the
                    // smallest answer about the closest member may be too big.
                    Opcode::Alloca | Opcode::GlobalAddr | Opcode::Call
                        if ask.closest && !largest =>
                    {
                        Err(())
                    }
                    Opcode::PtrAdd => {
                        let base = *args.first().ok_or(())?;
                        let count = *args.get(1).ok_or(())?;
                        let (imm, ty) = self.number(count, 4).ok_or(())?;
                        let step = u64::try_from(imm.signed(ty)).map_err(|_| ())?;
                        // A count the program worked out, even one that comes to a constant, is
                        // not known when gcc's early pass asks about the member, so it answers
                        // with all of the member and the step is taken off only what is left of
                        // the whole object. metronomefb clears `args + i` with `i` one and a size
                        // that runs into the `csum` after it, and gcc says nothing.
                        if ask.closest && largest && data.flags.contains(Flags::COUNTED) {
                            let Some(member) = self.left(base, ask, depth, on)? else {
                                return Ok(None);
                            };
                            let whole = Ask { closest: false, ..ask };
                            return Ok(Some(match self.left(value, whole, depth, on) {
                                Ok(Some(whole)) => member.min(whole),
                                _ => member,
                            }));
                        }
                        match self.left(base, ask, depth, on)? {
                            Some(left) => Ok(Some(left.saturating_sub(step))),
                            // A pointer moved forward each time round a loop may end up anywhere
                            // further along, so there is no smallest.
                            None if !largest && step != 0 => Err(()),
                            None => Ok(None),
                        }
                    }
                    Opcode::Alloca => match args.first() {
                        None => {
                            let Extra::Mem(mem) = data.extra else { return Err(()) };
                            Ok(Some(self.func[mem].size))
                        }
                        Some(&count) => {
                            let (imm, _) = self.number(count, 4).ok_or(())?;
                            Ok(Some(u64::try_from(imm.unsigned()).map_err(|_| ())?))
                        }
                    },
                    Opcode::GlobalAddr => {
                        let Extra::Symbol(name) = data.extra else { return Err(()) };
                        let Some(SymbolRef::Global(id)) = self.module.lookup(name) else {
                            return Err(());
                        };
                        let global = &self.module[id];
                        if !vouched(global, self.pic) {
                            return Err(());
                        }
                        Ok(Some(global.size))
                    }
                    // An address read back out of a local it was put in once, which is what a
                    // `__free (kfree)` variable is: the cleanup takes its address, so it stays a
                    // slot rather than becoming a value, and the question is asked of the load.
                    Opcode::Load => {
                        let put = self.only_store(*args.first().ok_or(())?, inst).ok_or(())?;
                        self.left(put, ask, depth, on)
                    }
                    // What an allocator gave back, where the attribute on it says which arguments
                    // are the size and each of them is a constant here.
                    Opcode::Call => {
                        let (alloc, args) = self.allocation(inst).ok_or(())?;
                        let mut size: u64 = 1;
                        for factor in factors(alloc, args).ok_or(())? {
                            let (imm, ty) = self.number(factor, 4).ok_or(())?;
                            let factor = u64::try_from(imm.signed(ty)).map_err(|_| ())?;
                            size = size.checked_mul(factor).ok_or(())?;
                        }
                        Ok(Some(size))
                    }
                    _ => Err(()),
                }
            }
        }
    }

    /// A number this value is whatever way the function runs, worked out the way the fold pass
    /// would once it got there.
    ///
    /// This runs before any of the passes, so it looks further than [`crate::fold::evaluated`]
    /// does: through a local stored once and read back, and through an overflow checking
    /// operation on two numbers. `kmalloc_array (256, 3, ...)` is both, a size put into `bytes`
    /// by `__builtin_mul_overflow` and read back for the call to the allocator.
    fn number(&self, value: Value, depth: u32) -> Option<(Imm, Type)> {
        if let Some(found) = crate::fold::constant(self.func, value) {
            return Some(found);
        }
        if let Some(&found) = self.numbers.borrow().get(&value) {
            return found;
        }
        let found = self.worked_out(value, depth);
        self.numbers.borrow_mut().insert(value, found);
        found
    }

    /// [`Walk::number`] for a value that is not a constant itself and was not asked about yet.
    fn worked_out(&self, value: Value, depth: u32) -> Option<(Imm, Type)> {
        let next = depth.checked_sub(1)?;
        let (inst, index) = match self.func[value].def {
            Def::Result { inst, index } => (inst, index),
            // The same number on every way in that can be taken, which is how an inlined
            // `mem_alloc_profiling_enabled ()` hands back its `false`.
            Def::Param { block, index } => {
                let mut found = None;
                for &pred in self.cfg.predecessors(block) {
                    if !self.taken(pred, block) {
                        continue;
                    }
                    let term = self.func.terminator(pred)?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize)?;
                        let (imm, ty) = self.number(arg, next)?;
                        if found.is_some_and(|(had, _): (Imm, Type)| had != imm) {
                            return None;
                        }
                        found = Some((imm, ty));
                    }
                }
                return found;
            }
        };
        let data = &self.func[inst];
        let args = &self.func[data.args];
        let ty = self.func[value].ty;
        if !ty.is_int() || !ty.is_scalar() {
            return None;
        }
        match data.opcode {
            Opcode::Load => self.number(self.only_store(*args.first()?, inst)?, next),
            Opcode::Expect => self.number(*args.first()?, next),
            _ if data.results == 2 => {
                let (a, _) = self.number(*args.first()?, next)?;
                let (b, _) = self.number(*args.get(1)?, next)?;
                let at = self.func[data.results().next()?].ty;
                let (sum, over) = crate::fold::overflowing(data.opcode, a, b, at)?;
                let found = if index == 0 { sum } else { Imm::from_bits(u128::from(over)) };
                Some((found, ty))
            }
            _ if data.results == 1 => {
                let found = crate::fold::arithmetic(data, args, ty, &|arg| self.number(arg, next))?;
                Some((found, ty))
            }
            _ => None,
        }
    }

    /// Whether the edge can be taken, as far as is known so far.
    fn taken(&self, from: Block, to: Block) -> bool {
        self.live.is_empty() || self.live.contains(&(from, to))
    }

    /// Whether some edge that can be taken comes into the block, or it is the entry.
    fn reached(&self, block: Block) -> bool {
        self.func.entry() == Some(block)
            || self.cfg.predecessors(block).iter().any(|&pred| self.taken(pred, block))
    }

    /// The edges out of each block the entry reaches, leaving out the one a branch on a number
    /// does not take.
    fn live_edges(&self) -> Set<(Block, Block)> {
        let mut live = Set::default();
        let Some(entry) = self.func.entry() else { return live };
        let mut seen: Set<Block> = Set::default();
        let mut work = vec![entry];
        seen.insert(entry);
        while let Some(block) = work.pop() {
            let Some(term) = self.func.terminator(block) else { continue };
            let calls: Vec<Block> = self.func.successors(term).map(|call| call.block).collect();
            let taken = match (self.func[term].opcode, calls.as_slice()) {
                (Opcode::BrIf, &[then, other]) => {
                    let cond = self.func[self.func[term].args].first().copied();
                    match cond.and_then(|cond| self.number(cond, DEPTH)) {
                        Some((imm, _)) if imm.unsigned() != 0 => vec![then],
                        Some(_) => vec![other],
                        None => calls,
                    }
                }
                _ => calls,
            };
            for to in taken {
                live.insert((block, to));
                if seen.insert(to) {
                    work.push(to);
                }
            }
        }
        live
    }

    /// The value the store that reaches this load put in a fixed local, when that is all the load
    /// can read.
    ///
    /// The slot is only loaded from, stored to and has its lifetime marked, so nothing else can
    /// write it or hand its address on. One store, of the type the load reads, comes before the
    /// load on every way to it, and no other store can get to the load without passing through
    /// that one again. The load then reads what that store put there the last time it ran, and a
    /// walk of the stored value answers for every time it ran.
    ///
    /// Other stores are allowed because the inliner gives two inlined bodies one slot when their
    /// lifetimes do not overlap. keyboard.c's `vt_do_diacrit` has two `__free (kfree)` buffers in
    /// one slot that way, one in each of two cases of its switch.
    fn only_store(&self, slot: Value, load: Inst) -> Option<Value> {
        let Def::Result { inst: made, .. } = self.func[slot].def else { return None };
        if self.func[made].opcode != Opcode::Alloca || !self.func[self.func[made].args].is_empty() {
            return None;
        }
        let mut stores = Vec::new();
        for inst in self.func.blocks().flat_map(|block| self.func.insts(block)) {
            let data = &self.func[inst];
            for (at, &arg) in self.func[data.args].iter().enumerate() {
                if arg != slot {
                    continue;
                }
                match (data.opcode, at) {
                    // A comparison of the address hands it to nobody, and the slot shares its
                    // address with other inlined locals that a guard's cleanup compares.
                    (Opcode::Load, 0) | (Opcode::LifetimeEnd | Opcode::ICmp, _) => {}
                    // A store on a way through the function that cannot be taken never runs.
                    (Opcode::Store, 1) | (Opcode::Memset, 0)
                        if !self.reached(self.func.block_of(inst)?) => {}
                    // `-ftrivial-auto-var-init=zero` clears a local before the program writes
                    // it, which the kernel builds with. The clearing is a store as well, one this
                    // walk cannot read, and it is fine for as long as a later one is the one the
                    // load sees.
                    (Opcode::Store, 1) | (Opcode::Memset, 0) => stores.push(inst),
                    _ => return None,
                }
            }
        }
        // The stores before the load on every way to it are one after another, so the last of
        // them is the one each of those ways went through most recently.
        let mut store: Option<Inst> = None;
        for &each in &stores {
            if self.precedes(each, load) && store.is_none_or(|had| self.precedes(had, each)) {
                store = Some(each);
            }
        }
        let store = store?;
        if stores.iter().any(|&other| other != store && self.reaches(other, load, store)) {
            return None;
        }
        if self.func[store].opcode != Opcode::Store {
            return None;
        }
        let put = self.func[self.func[store].args][0];
        let read = self.func[load].first_result?;
        (self.func[put].ty == self.func[read].ty).then_some(put)
    }

    /// Whether one instruction comes before another on every way to the second.
    fn precedes(&self, first: Inst, then: Inst) -> bool {
        let (Some(from), Some(to)) = (self.func.block_of(first), self.func.block_of(then)) else {
            return false;
        };
        if from == to {
            self.func.insts(from).find(|&inst| inst == first || inst == then) == Some(first)
        } else {
            self.dom.dominates(from, to)
        }
    }

    /// Whether running on from `from` can get to `to` without going through `through`.
    fn reaches(&self, from: Inst, to: Inst, through: Inst) -> bool {
        let Some(start) = self.func.block_of(from) else { return true };
        // What the rest of a block decides: `Some` when it meets one of the two, `None` when it
        // runs off the end and the walk goes on into the successors.
        let scan = |mut insts: Box<dyn Iterator<Item = Inst> + '_>| {
            insts.find(|&inst| inst == to || inst == through).map(|inst| inst == to)
        };
        let rest = self.func.insts(start).skip_while(move |&inst| inst != from).skip(1);
        if let Some(found) = scan(Box::new(rest)) {
            return found;
        }
        let mut seen: Set<Block> = Set::default();
        let mut work: Vec<Block> = self.cfg.successors(start).to_vec();
        while let Some(block) = work.pop() {
            if !seen.insert(block) {
                continue;
            }
            match scan(Box::new(self.func.insts(block))) {
                Some(true) => return true,
                Some(false) => {}
                None => work.extend_from_slice(self.cfg.successors(block)),
            }
        }
        false
    }

    /// The `alloc_size` of the function a call names, with the arguments of the call, for a direct
    /// call to a function the module has and that carries one.
    fn allocation(&self, call: Inst) -> Option<(AllocSize, &[Value])> {
        let data = &self.func[call];
        let Extra::Call(info) = data.extra else { return None };
        let name = self.func[info].callee?;
        let Some(SymbolRef::Func(callee)) = self.module.lookup(name) else { return None };
        let alloc = self.module[callee].attrs.alloc_size?;
        Some((alloc, &self.func[data.args]))
    }

    /// The address as what an allocator gave back with a constant number of bytes added, for an
    /// answer worked out at run time from the arguments of the call.
    ///
    /// Only a straight line back to the call is followed, with no choice in it, since a choice
    /// would need the answer built on every path into it. The arguments have to be as wide as the
    /// answer already, which `size_t` is in `malloc` and `kmalloc` and every allocator written
    /// in its terms. A narrower one has a signedness the IR does not remember, and widening it
    /// the wrong way would give a size that is not the one the program asked for, so that is
    /// answered as not known instead.
    fn running(&self, value: Value, size: Type, depth: u32) -> Option<Running> {
        let depth = depth.checked_sub(1)?;
        let Def::Result { inst, .. } = self.func[value].def else { return None };
        let data = &self.func[inst];
        match data.opcode {
            Opcode::PtrAdd => {
                let args = &self.func[data.args];
                let (imm, ty) = self.number(*args.get(1)?, 4)?;
                let step = u64::try_from(imm.signed(ty)).ok()?;
                let mut running = self.running(args[0], size, depth)?;
                running.offset = running.offset.checked_add(step)?;
                Some(running)
            }
            Opcode::Call => {
                let (alloc, args) = self.allocation(inst)?;
                let factors = factors(alloc, args)?;
                if factors.iter().any(|&factor| self.func[factor].ty != size) {
                    return None;
                }
                Some(Running { factors, offset: 0 })
            }
            _ => None,
        }
    }

    /// The object every way to the address starts in, with its size, for an answer worked out
    /// when the program runs from how far into it the address is.
    ///
    /// The local has to be made before the question on every way to it, so that its address is
    /// there to subtract. A global is one whose size `extents::vouched` stands behind. The closest
    /// member is the object found, which is no smaller than any member the address is in, except
    /// where the walk passes the start of a member, which this leaves as not known.
    fn within(&self, value: Value, question: Inst, ask: Ask) -> Option<Answer> {
        let object = self.object(value, ask, DEPTH, &mut Vec::new()).ok()??;
        let size = match object {
            Object::Local(start) => {
                let Def::Result { inst, .. } = self.func[start].def else { return None };
                if !self.precedes(inst, question) {
                    return None;
                }
                let Extra::Mem(mem) = self.func[inst].extra else { return None };
                self.func[mem].size
            }
            Object::Global(name) => {
                let Some(SymbolRef::Global(id)) = self.module.lookup(name) else { return None };
                let global = &self.module[id];
                if !vouched(global, self.pic) {
                    return None;
                }
                global.size
            }
        };
        Some(Answer::Within(object, size))
    }

    /// The object an address is in, as [`Found`] says.
    ///
    /// `on` is the block parameters whose answer is being worked out, as in [`Walk::left`].
    fn object(&self, value: Value, ask: Ask, depth: u32, on: &mut Vec<Value>) -> Found {
        let depth = depth.checked_sub(1).ok_or(())?;
        match self.func[value].def {
            Def::Param { block, index } => {
                if on.contains(&value) {
                    return Ok(None);
                }
                let preds = self.cfg.predecessors(block);
                if preds.is_empty() {
                    return Err(());
                }
                on.push(value);
                let mut all = Ok(None);
                for &pred in preds {
                    if !self.taken(pred, block) {
                        continue;
                    }
                    let term = self.func.terminator(pred).ok_or(())?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize).ok_or(())?;
                        all = same(all, self.object(arg, ask, depth, on));
                    }
                }
                on.pop();
                all
            }
            Def::Result { inst, .. } => {
                let data = &self.func[inst];
                let args = &self.func[data.args];
                match data.opcode {
                    Opcode::Select => {
                        let (then, other) = (*args.get(1).ok_or(())?, *args.get(2).ok_or(())?);
                        let then = self.object(then, ask, depth, on);
                        same(then, self.object(other, ask, depth, on))
                    }
                    Opcode::PtrAdd if ask.closest && matches!(data.extra, Extra::Member(_)) => {
                        Err(())
                    }
                    Opcode::PtrAdd => self.object(*args.first().ok_or(())?, ask, depth, on),
                    Opcode::Alloca if args.is_empty() => Ok(Some(Object::Local(value))),
                    Opcode::GlobalAddr => {
                        let Extra::Symbol(name) = data.extra else { return Err(()) };
                        Ok(Some(Object::Global(name)))
                    }
                    Opcode::Load => {
                        let put = self.only_store(*args.first().ok_or(())?, inst).ok_or(())?;
                        self.object(put, ask, depth, on)
                    }
                    Opcode::Call => self.object(self.into(inst).ok_or(())?, ask, depth, on),
                    _ => Err(()),
                }
            }
        }
    }

    /// The argument a library call gives back an address in the object of, for the calls that do
    /// that whatever they are given.
    ///
    /// A copy or a fill gives back where it wrote, and the `p` spellings give back the end of what
    /// they wrote. `strchr (s, 0)` gives back the end of `s`. A `strchr` for any other character
    /// may give back a null pointer, and gcc does not follow it either.
    fn into(&self, call: Inst) -> Option<Value> {
        let library = self.library?;
        let data = &self.func[call];
        let Extra::Call(info) = data.extra else { return None };
        let callee = self.func[info].callee?;
        let Some(SymbolRef::Func(id)) = self.module.lookup(callee) else { return None };
        let name = self.names.resolve(self.module[id].spelled.unwrap_or(callee));
        if library.iter().any(|it| it == name) {
            return None;
        }
        let args = &self.func[data.args];
        match name {
            "memcpy" | "memmove" | "memset" | "mempcpy" | "strcpy" | "stpcpy" | "strncpy"
            | "stpncpy" | "strcat" | "strncat" | "__memcpy_chk" | "__memmove_chk"
            | "__memset_chk" | "__mempcpy_chk" | "__strcpy_chk" | "__stpcpy_chk"
            | "__strncpy_chk" | "__stpncpy_chk" | "__strcat_chk" | "__strncat_chk" => {
                args.first().copied()
            }
            "strchr" | "index" => {
                let (imm, _) = self.number(*args.get(1)?, 4)?;
                (imm.unsigned() == 0).then_some(args[0])
            }
            _ => None,
        }
    }
}

/// What one walk of [`Walk::object`] found: `Err` where the address may be in more than one object
/// or in one the walk cannot see, `Ok(None)` where the only way to it is round a loop back to
/// itself, and otherwise the object.
type Found = Result<Option<Object>, ()>;

/// Two answers for one choice, which have to be the same object.
fn same(one: Found, other: Found) -> Found {
    match (one?, other?) {
        (Some(one), Some(other)) if one != other => Err(()),
        (one, other) => Ok(one.or(other)),
    }
}

/// The arguments of a call that `alloc_size` says multiply to the size of what it returns, and
/// nothing for a call that has fewer arguments than the attribute counts.
fn factors(alloc: AllocSize, args: &[Value]) -> Option<Vec<Value>> {
    let mut factors = Vec::with_capacity(2);
    for number in [Some(alloc.size), alloc.count].into_iter().flatten() {
        factors.push(*args.get(usize::from(number).checked_sub(1)?)?);
    }
    Some(factors)
}

/// Two answers for one choice, as the kind asks for them to be put together.
fn both(one: Left, other: Left, largest: bool) -> Left {
    Ok(match (one?, other?) {
        (Some(one), Some(other)) if largest => Some(one.max(other)),
        (Some(one), Some(other)) => Some(one.min(other)),
        (Some(one), None) | (None, Some(one)) => Some(one),
        (None, None) => None,
    })
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    /// What every fixture below starts with, which is the target the widths are of.
    const HEAD: &str = "\
; ModuleID = 't.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
";

    /// The numbers each call to `@use` in the module was given once every question is answered,
    /// in the order the calls are written.
    fn answers(body: &str, look: bool) -> Vec<i128> {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, look);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the answers left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        let mut found = Vec::new();
        for id in module.funcs() {
            let func = &module[id];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    assert_ne!(func[inst].opcode, Opcode::ObjectSize, "a question was left");
                    if func[inst].opcode != Opcode::Call {
                        continue;
                    }
                    let &[value] = &func[func[inst].args] else { continue };
                    let Def::Result { inst: def, .. } = func[value].def else { continue };
                    let Extra::Imm(imm) = func[def].extra else { continue };
                    found.push(func[imm].signed(func[value].ty));
                }
            }
        }
        found
    }

    /// What an allocator gave back is the product of the arguments its `alloc_size` names where
    /// they are constants, for both spellings. Where they are not, the dynamic spelling is
    /// answered with the arguments as they are when the program runs, less what was added to the
    /// address, and the other is not known. The numbers are what gcc 16 answers at `-O2`.
    #[test]
    fn what_an_allocator_gave_back_is_what_its_alloc_size_says() {
        let body = "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);
func @my2(i64, i64) -> ptr, linkage(external), attrs(alloc_size=1, alloc_count=2);
func @plain(i64) -> ptr, linkage(external);

func @f(i64), linkage(external) {
block0(%0: i64):
    %1 = iconst.i64 10
    %2 = call @my(%1) : (i64) -> ptr
    %3 = object_size.i64 %2, kind 0
    call @use(%3) : (i64)
    %4 = iconst.i64 4
    %5 = ptr_add %2, %4
    %6 = object_size.i64 %5, kind 2
    call @use(%6) : (i64)
    %7 = iconst.i64 3
    %8 = iconst.i64 5
    %9 = call @my2(%7, %8) : (i64, i64) -> ptr
    %10 = object_size.i64 %9, kind 4
    call @use(%10) : (i64)
    %11 = call @plain(%1) : (i64) -> ptr
    %12 = object_size.i64 %11, kind 0
    call @use(%12) : (i64)
    %13 = call @my(%0) : (i64) -> ptr
    %14 = object_size.i64 %13, kind 0
    call @use(%14) : (i64)
    %15 = ptr_add %13, %4
    %16 = object_size.i64 %15, kind 4
    call @use(%16) : (i64)
    return
}
";
        // Every call with one constant argument is read back, so the two calls given ten are
        // among them. The last question is not a constant when looked at, so it is not.
        assert_eq!(answers(body, true), [10, 10, 6, 15, 10, -1, -1]);
        assert_eq!(answers(body, false), [10, -1, 0, -1, 10, -1, -1, -1]);
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, true);
        let text = rucc_ir::print(&module, &names);
        assert!(text.contains("icmp ugt %0, "), "{text}");
        assert!(text.contains("= sub %0, "), "{text}");
    }

    /// An allocator's result put in a local once and read back is what the allocator gave, which
    /// is the shape a `__free (kfree)` variable has. A second store, or the slot's address going
    /// anywhere but a load or a store, and it is not known.
    #[test]
    fn an_address_read_back_out_of_a_local_is_what_the_last_store_put_there() {
        let body = |extra: &str| {
            format!(
                "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);
func @keep(ptr), linkage(external);

func @f(i64), linkage(external) {{
block0(%0: i64):
    %1 = alloca, size 8, align 8
    %2 = iconst.i64 768
    %3 = call @my(%2) : (i64) -> ptr
    %4 = iconst.i64 16
    %5 = call @my(%4) : (i64) -> ptr
    store %3 -> %1, align 8
{extra}    %6 = load.ptr %1, align 8
    %7 = object_size.i64 %6, kind 0
    call @use(%7) : (i64)
    return
}}
"
            )
        };
        assert_eq!(answers(&body(""), true), [768, 16, 768]);
        // A later store is the one the load reads.
        assert_eq!(answers(&body("    store %5 -> %1, align 8\n"), true), [768, 16, 16]);
        assert_eq!(answers(&body("    call @keep(%1) : (ptr)\n"), true), [768, 16, -1]);
    }

    /// A store on one arm of a branch can be what the load reads, so the store before the branch
    /// does not answer for it, even though that one comes before the load on every way to it.
    #[test]
    fn a_store_that_can_reach_the_load_another_way_leaves_it_unknown() {
        let body = "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);
func @other() -> ptr, linkage(external);

func @f(i1), linkage(external) {
block0(%0: i1):
    %1 = alloca, size 8, align 8
    %2 = iconst.i64 768
    %3 = call @my(%2) : (i64) -> ptr
    store %3 -> %1, align 8
    br_if %0, block1, block2

block1:
    %4 = call @other() : () -> ptr
    store %4 -> %1, align 8
    jump block2

block2:
    %5 = load.ptr %1, align 8
    %6 = object_size.i64 %5, kind 0
    call @use(%6) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [768, -1]);
    }

    /// `kmalloc_array` on two constants, inlined twice into one slot for `bytes` that
    /// `-ftrivial-auto-var-init=zero` clears first. The overflow check cannot fail, so its `NULL`
    /// is never what comes back, and each load reads the store in front of it.
    #[test]
    fn a_size_from_an_overflow_check_through_a_shared_slot_is_followed() {
        let body = |second: &str| {
            format!(
                "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);

func @f(i32), linkage(external) {{
block0(%0: i32):
    %1 = alloca, size 8, align 8
    %2 = iconst.i64 256
    %3 = iconst.i64 3
    %4 = iconst.i8 0
    %5 = iconst.i32 0
    %6 = icmp ne %0, %5
    br_if %6, block1, block4

block1:
    memset %1, %4, size 8, align 8
    %7, %8 = umul_overflow.(i64, i1) %2, %3
    store %7 -> %1, align 8
    br_if %8, block2, block3

block2:
    %9 = iconst.i64 0
    %10 = inttoptr.ptr %9
    jump block5(%10)

block3:
    %11 = load.i64 %1, align 8
    %12 = call @my(%11) : (i64) -> ptr
    jump block5(%12)

block4:
    memset %1, %4, size 8, align 8
    %13 = iconst.i64 {second}
    store %13 -> %1, align 8
    %14 = load.i64 %1, align 8
    %15 = call @my(%14) : (i64) -> ptr
    %16 = object_size.i64 %15, kind 0
    call @use(%16) : (i64)
    return

block5(%17: ptr):
    %18 = object_size.i64 %17, kind 0
    call @use(%18) : (i64)
    return
}}
"
            )
        };
        assert_eq!(answers(&body("16"), true), [16, 768]);
    }

    /// A pointer chosen by a branch between a local and a global has the larger of what the two
    /// have left for the first kind and the smaller for the third.
    #[test]
    fn a_choice_of_two_objects_is_the_larger_or_the_smaller_of_what_each_has_left() {
        let body = "
global @g : bytes 32 = { zero 32 }, align 1, linkage(external)

func @f(i32), linkage(external) {
block0(%0: i32):
    %1 = alloca, size 20, align 16
    %2 = iconst.i32 0
    %3 = icmp ne %0, %2
    br_if %3, block1, block2

block1:
    %4 = iconst.i32 5
    %5 = sext.i64 %4
    %6 = ptr_add %1, %5
    jump block3(%6)

block2:
    %7 = global_addr @g
    %8 = iconst.i64 4
    %9 = ptr_add %7, %8
    jump block3(%9)

block3(%10: ptr):
    %11 = object_size.i64 %10, kind 0
    call @use(%11) : (i64)
    %12 = object_size.i64 %10, kind 1
    call @use(%12) : (i64)
    %13 = object_size.i64 %10, kind 2
    call @use(%13) : (i64)
    %14 = object_size.i64 %10, kind 3
    call @use(%14) : (i64)
    return
}
";
        // The second kind is the whole object's answer, which is no smaller than any member's,
        // and the fourth is not known here at all.
        assert_eq!(answers(body, true), [28, 28, 15, 0]);
        // And at `-O0` none of it is looked at.
        assert_eq!(answers(body, false), [-1, -1, 0, 0]);
    }

    /// A pointer carried round a loop and replaced on some trips is the choice of every address it
    /// was given, and the trip that leaves it alone adds nothing to the choice.
    #[test]
    fn a_pointer_a_loop_leaves_alone_is_what_it_was_given() {
        let body = "
func @f(i32), linkage(external) {
block0(%0: i32):
    %1 = alloca, size 20, align 16
    %2 = iconst.i32 0
    jump block1(%1, %2)

block1(%3: ptr, %4: i32):
    %5 = icmp eq %4, %0
    %6 = iconst.i64 7
    %7 = ptr_add %1, %6
    %8 = select.ptr %5, %7, %3
    %9 = iconst.i32 1
    %10 = add %4, %9
    %11 = icmp slt %10, %0
    br_if %11, block1(%8, %10), block2

block2:
    %12 = object_size.i64 %8, kind 0
    call @use(%12) : (i64)
    %13 = object_size.i64 %8, kind 2
    call @use(%13) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [20, 13]);
    }

    /// A pointer moved forward each time round a loop has less and less left, so the largest is
    /// where it started and there is no smallest. Moved backward there is no largest either, since
    /// it may have more left than anything the walk saw.
    #[test]
    fn a_pointer_a_loop_moves_is_known_only_where_that_can_only_leave_less() {
        let forward = "
func @f(i32), linkage(external) {
block0(%0: i32):
    %1 = alloca, size 20, align 16
    %2 = iconst.i32 0
    jump block1(%1, %2)

block1(%3: ptr, %4: i32):
    %5 = iconst.i64 STEP
    %6 = ptr_add %3, %5
    %7 = iconst.i32 1
    %8 = add %4, %7
    %9 = icmp slt %8, %0
    br_if %9, block1(%6, %8), block2

block2:
    %10 = object_size.i64 %6, kind 0
    call @use(%10) : (i64)
    %11 = object_size.i64 %6, kind 2
    call @use(%11) : (i64)
    return
}
";
        assert_eq!(answers(&forward.replace("STEP", "1"), true), [19, 0]);
        assert_eq!(answers(&forward.replace("STEP", "-1"), true), [-1, 0]);
    }

    /// The module once every question is answered, printed, with `library` as [`answer`] takes it.
    fn printed(body: &str, library: Option<&[String]>) -> String {
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
        answer(&mut module, &names, library, Pic::Executable, true);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the answers left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// The dynamic spelling of a kind that asks for the largest answer, about an address every way
    /// to which starts at one fixed local or one global, is the size of that object less how far
    /// in the address is, worked out when the program runs. The walk goes through a step of any
    /// count, a copy that gives back where it wrote and `strchr (s, 0)`, and round a loop. The
    /// static spelling of the same question is not known. This is what gcc 16 does at `-O2`.
    #[test]
    fn an_address_somewhere_in_one_object_is_measured_when_the_program_runs() {
        let body = "
global @g : bytes 32 = { zero 32 }, align 1, linkage(external)
func @strchr(ptr, i32) -> ptr, linkage(external);
func @strcpy(ptr, ptr) -> ptr, linkage(external);

func @f(ptr, i64), linkage(external) {
block0(%0: ptr, %1: i64):
    %2 = alloca, size 1000, align 16
    jump block1(%2)

block1(%3: ptr):
    %4 = object_size.i64 %3, kind 5
    call @use(%4) : (i64)
    %5 = object_size.i64 %3, kind 1
    call @use(%5) : (i64)
    %6 = iconst.i32 0
    %7 = call @strchr(%3, %6) : (ptr, i32) -> ptr
    %8 = iconst.i64 -1
    %9 = ptr_add %7, %8
    %10 = call @strcpy(%9, %0) : (ptr, ptr) -> ptr
    %11 = ptr_add %10, %1
    %12 = iconst.i64 0
    %13 = icmp ne %1, %12
    br_if %13, block1(%11), block2

block2:
    %14 = global_addr @g
    %15 = ptr_add %14, %1
    %16 = object_size.i64 %15, kind 4
    call @use(%16) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [-1]);
        assert_eq!(answers(body, false), [-1, -1, -1]);
        let text = printed(body, Some(&[]));
        assert_eq!(text.matches("ptrtoint").count(), 4, "{text}");
        assert_eq!(text.matches("icmp ugt").count(), 2, "{text}");
        assert!(text.contains("iconst.i64 1000") && text.contains("iconst.i64 32"), "{text}");
    }

    /// An address that may be in either of two objects, one a `strchr` gave back for a character
    /// that may not be there, and one in a global the linker may take from somewhere else are not
    /// measured. Neither is one a copy gave back under `-fno-builtin`, nor under
    /// `-fno-builtin-strcpy`, since the call may then be to anything.
    #[test]
    fn an_address_that_may_be_in_another_object_is_not_measured() {
        let body = "
global @w : bytes 8 = { zero 8 }, align 1, linkage(weak)
func @strchr(ptr, i32) -> ptr, linkage(external);
func @strcpy(ptr, ptr) -> ptr, linkage(external);

func @f(i1, i64, ptr), linkage(external) {
block0(%0: i1, %1: i64, %2: ptr):
    %3 = alloca, size 10, align 1
    %4 = alloca, size 20, align 1
    %5 = select.ptr %0, %3, %4
    %6 = ptr_add %5, %1
    %7 = object_size.i64 %6, kind 4
    call @use(%7) : (i64)
    %8 = iconst.i32 97
    %9 = call @strchr(%3, %8) : (ptr, i32) -> ptr
    %10 = ptr_add %9, %1
    %11 = object_size.i64 %10, kind 4
    call @use(%11) : (i64)
    %12 = global_addr @w
    %13 = ptr_add %12, %1
    %14 = object_size.i64 %13, kind 4
    call @use(%14) : (i64)
    %15 = call @strcpy(%3, %2) : (ptr, ptr) -> ptr
    %16 = ptr_add %15, %1
    %17 = object_size.i64 %16, kind 4
    call @use(%17) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [-1, -1, -1]);
        assert_eq!(printed(body, Some(&[])).matches("ptrtoint").count(), 2);
        assert_eq!(printed(body, None).matches("ptrtoint").count(), 0);
        assert_eq!(printed(body, Some(&["strcpy".to_owned()])).matches("ptrtoint").count(), 0);
    }

    /// An address past the end of its object has nothing left, and a global the linker may take
    /// from somewhere else is not one whose size this module knows.
    #[test]
    fn past_the_end_is_nothing_and_a_weak_global_is_not_known() {
        let body = "
global @g : bytes 8 = { zero 8 }, align 1, linkage(external)
global @w : bytes 8 = { zero 8 }, align 1, linkage(weak)

func @f(), linkage(external) {
block0:
    %0 = global_addr @g
    %1 = iconst.i64 12
    %2 = ptr_add %0, %1
    %3 = object_size.i64 %2, kind 0
    call @use(%3) : (i64)
    %4 = global_addr @w
    %5 = object_size.i64 %4, kind 0
    call @use(%5) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [0, -1]);
    }

    /// An address lowering marked as the start of a member, or one a constant into it, has what
    /// is left of the member for the closest kinds and what is left of the whole object for the
    /// others. Without a mark on the way the smallest closest answer is not known, and the marks
    /// are gone once the questions are answered. The numbers are what gcc 16 answers at `-O2`.
    #[test]
    fn the_closest_member_is_the_one_lowering_marked() {
        let body = "
global @g : bytes 40 = { zero 40 }, align 4, linkage(external)

func @f(), linkage(external) {
block0:
    %0 = global_addr @g
    %1 = iconst.i64 4
    %2 = ptr_add %0, %1, member 16
    %3 = object_size.i64 %2, kind 1
    call @use(%3) : (i64)
    %4 = object_size.i64 %2, kind 3
    call @use(%4) : (i64)
    %5 = object_size.i64 %2, kind 0
    call @use(%5) : (i64)
    %6 = iconst.i64 2
    %7 = ptr_add %2, %6
    %8 = object_size.i64 %7, kind 1
    call @use(%8) : (i64)
    %9 = ptr_add %0, %1
    %10 = object_size.i64 %9, kind 1
    call @use(%10) : (i64)
    %11 = object_size.i64 %9, kind 3
    call @use(%11) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [16, 16, 36, 14, 36, 0]);
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, true);
        let text = rucc_ir::print(&module, &names);
        assert!(!text.contains("member"), "{text}");
    }

    /// A `ptr_add` whose count the program worked out leaves the closest kinds with all of the
    /// member it moved in, or what is left of the whole object where that is less, and the flag is
    /// gone once the questions are answered. The numbers are what gcc 16 answers at `-O2` for
    /// `s.args + i` and `buf + i` with `i` a local holding one.
    #[test]
    fn a_counted_step_inside_a_member_is_somewhere_in_all_of_it() {
        let body = "
global @g : bytes 66 = { zero 66 }, align 2, linkage(external)
global @b : bytes 16 = { zero 16 }, align 1, linkage(external)

func @f(), linkage(external) {
block0:
    %0 = global_addr @g
    %1 = iconst.i64 2
    %2 = ptr_add %0, %1, member 62
    %3 = ptr_add.counted %2, %1
    %4 = object_size.i64 %3, kind 1
    call @use(%4) : (i64)
    %5 = object_size.i64 %3, kind 0
    call @use(%5) : (i64)
    %6 = ptr_add %2, %1
    %7 = object_size.i64 %6, kind 1
    call @use(%7) : (i64)
    %8 = global_addr @b
    %9 = iconst.i64 1
    %10 = ptr_add.counted %8, %9
    %11 = object_size.i64 %10, kind 1
    call @use(%11) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [62, 62, 60, 15]);
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, true);
        let text = rucc_ir::print(&module, &names);
        assert!(!text.contains("counted"), "{text}");
    }
}
