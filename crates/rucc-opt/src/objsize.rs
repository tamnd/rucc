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
//! The closest member, which is the low bit of the kind, is not something the IR remembers. The
//! whole object is an answer no smaller than the member for the largest, so the first kind's answer
//! stands for the second. For the smallest it could be too big, so the fourth kind is not known
//! here and only the checker ever answers it.

use rucc_ir::{
    AllocSize, AttrSet, Def, Extra, Func, FuncId, Imm, Inst, InstData, IntPred, Module, Opcode,
    Pic, SymbolRef, Type, Value,
};

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
pub fn answer(module: &mut Module, pic: Pic, look: bool) -> usize {
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
            let walk = Walk { module, func, cfg: &cfg, dom: &Dominators::new(&cfg), pic };
            asked
                .iter()
                .map(|&inst| {
                    let Extra::Question(asked) = func[inst].extra else {
                        return (inst, Answer::Known(0));
                    };
                    let address = func[func[inst].args][0];
                    let (kind, dynamic) = (asked & 3, asked & DYNAMIC != 0);
                    let largest = kind & 2 == 0;
                    let known = match (look, kind) {
                        (false, _) | (_, 3) => None,
                        _ => walk.left(address, largest, DEPTH, &mut Vec::new()).ok().flatten(),
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
                            .map_or(Answer::Known(unknown), Answer::Running),
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
            }
            answered += 1;
        }
    }
    answered
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
    let forward: rucc_base::hash::Map<_, _> = [(result, value)].into_iter().collect();
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
    let forward: rucc_base::hash::Map<_, _> = [(result, size)].into_iter().collect();
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
}

/// How many bytes are left in front of an address: `Err` where that is not known, `Ok(None)` where
/// the only way to the address is round a loop back to itself, and otherwise the number.
type Left = Result<Option<u64>, ()>;

impl Walk<'_> {
    /// How many bytes there are from this address to the end of its object.
    ///
    /// `on` is the block parameters whose answer is being worked out, which is how a loop is seen.
    fn left(&self, value: Value, largest: bool, depth: u32, on: &mut Vec<Value>) -> Left {
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
                    let term = self.func.terminator(pred).ok_or(())?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize).ok_or(())?;
                        all = both(all, self.left(arg, largest, depth, on), largest);
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
                        let then = self.left(then, largest, depth, on);
                        both(then, self.left(other, largest, depth, on), largest)
                    }
                    Opcode::PtrAdd => {
                        let base = *args.first().ok_or(())?;
                        let (imm, ty) =
                            crate::fold::evaluated(self.func, *args.get(1).ok_or(())?, 4)
                                .ok_or(())?;
                        let step = u64::try_from(imm.signed(ty)).map_err(|_| ())?;
                        match self.left(base, largest, depth, on)? {
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
                            let (imm, _) = crate::fold::evaluated(self.func, count, 4).ok_or(())?;
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
                        self.left(put, largest, depth, on)
                    }
                    // What an allocator gave back, where the attribute on it says which arguments
                    // are the size and each of them is a constant here.
                    Opcode::Call => {
                        let (alloc, args) = self.allocation(inst).ok_or(())?;
                        let mut size: u64 = 1;
                        for factor in factors(alloc, args).ok_or(())? {
                            let (imm, ty) =
                                crate::fold::evaluated(self.func, factor, 4).ok_or(())?;
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

    /// The one value ever stored in a fixed local this load reads, when that is all the load can
    /// read.
    ///
    /// The slot is only loaded from, stored to and has its lifetime marked, so nothing else can
    /// write it or hand its address on. There is one store, of the type the load reads, and it
    /// comes before the load on every way to it. The load then reads what that store put there
    /// the last time it ran, and a walk of the stored value answers for every time it ran.
    fn only_store(&self, slot: Value, load: Inst) -> Option<Value> {
        let Def::Result { inst: made, .. } = self.func[slot].def else { return None };
        if self.func[made].opcode != Opcode::Alloca || !self.func[self.func[made].args].is_empty() {
            return None;
        }
        let mut store = None;
        for inst in self.func.blocks().flat_map(|block| self.func.insts(block)) {
            let data = &self.func[inst];
            for (at, &arg) in self.func[data.args].iter().enumerate() {
                if arg != slot {
                    continue;
                }
                match (data.opcode, at) {
                    (Opcode::Load, 0) | (Opcode::LifetimeEnd, _) => {}
                    (Opcode::Store, 1) if store.is_none() => store = Some(inst),
                    _ => return None,
                }
            }
        }
        let store = store?;
        let put = self.func[self.func[store].args][0];
        let read = self.func[load].first_result?;
        if self.func[put].ty != self.func[read].ty {
            return None;
        }
        let (from, to) = (self.func.block_of(store)?, self.func.block_of(load)?);
        let before = if from == to {
            self.func.insts(from).find(|&inst| inst == store || inst == load) == Some(store)
        } else {
            self.dom.dominates(from, to)
        };
        before.then_some(put)
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
                let (imm, ty) = crate::fold::evaluated(self.func, *args.get(1)?, 4)?;
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
        answer(&mut module, Pic::Executable, look);
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
        answer(&mut module, Pic::Executable, true);
        let text = rucc_ir::print(&module, &names);
        assert!(text.contains("icmp ugt %0, "), "{text}");
        assert!(text.contains("= sub %0, "), "{text}");
    }

    /// An allocator's result put in a local once and read back is what the allocator gave, which
    /// is the shape a `__free (kfree)` variable has. A second store, or the slot's address going
    /// anywhere but a load or a store, and it is not known.
    #[test]
    fn an_address_read_back_out_of_a_local_it_was_put_in_once_is_followed() {
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
    store %3 -> %1, align 8
{extra}    %4 = load.ptr %1, align 8
    %5 = object_size.i64 %4, kind 0
    call @use(%5) : (i64)
    return
}}
"
            )
        };
        assert_eq!(answers(&body(""), true), [768, 768]);
        assert_eq!(answers(&body("    store %3 -> %1, align 8\n"), true), [768, -1]);
        assert_eq!(answers(&body("    call @keep(%1) : (ptr)\n"), true), [768, -1]);
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
}
