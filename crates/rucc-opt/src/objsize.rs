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
//! its size where [`crate::extents::vouched`] says the definition in the module is the one that
//! will run. A `ptr_add` of a constant count takes it off what is left, down to nothing past the
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
//! The closest member, which is the low bit of the kind, is not something the IR remembers. The
//! whole object is an answer no smaller than the member for the largest, so the first kind's answer
//! stands for the second. For the smallest it could be too big, so the fourth kind is not known
//! here and only the checker ever answers it.

use rucc_ir::{
    Def, Extra, Func, FuncId, Imm, Inst, InstData, Module, Opcode, Pic, SymbolRef, Value,
};

use crate::Cfg;
use crate::extents::vouched;

/// How far a walk goes before it gives up, which is a chain of block parameters and `ptr_add`
/// this long.
const DEPTH: u32 = 16;

/// Answers every `object_size` in the module and says how many there were.
///
/// `look` is false at `-O0`, where every question is answered as not known.
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
        let answers: Vec<(Inst, i128)> = {
            let func = &module[id];
            let walk = Walk { module, func, cfg: &Cfg::new(func), pic };
            asked
                .iter()
                .map(|&inst| {
                    let Extra::Question(kind) = func[inst].extra else { return (inst, 0) };
                    let address = func[func[inst].args][0];
                    let largest = kind & 2 == 0;
                    let known = match (look, kind) {
                        (false, _) | (_, 3) => None,
                        _ => walk.left(address, largest, DEPTH, &mut Vec::new()).ok().flatten(),
                    };
                    (inst, known.map_or(if largest { -1 } else { 0 }, i128::from))
                })
                .collect()
        };
        let func = &mut module[id];
        for (inst, number) in answers {
            write(func, inst, number);
            answered += 1;
        }
    }
    answered
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
    let forward = [(result, value)].into_iter().collect();
    crate::uses::substitute(func, &forward);
    func.remove_inst(inst);
}

/// What a walk over one function reads.
struct Walk<'a> {
    module: &'a Module,
    func: &'a Func,
    cfg: &'a Cfg,
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
                    _ => Err(()),
                }
            }
        }
    }
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
