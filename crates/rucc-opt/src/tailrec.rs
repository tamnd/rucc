//! A call a function makes to itself in tail position, as a jump back to its top.
//!
//! Design: `spec/optimizer/25-tail-calls.md`, the self recursive case. gcc's `tailr` pass does
//! this from `-O2` up and at `-Os`, under `-foptimize-sibling-calls`, and so does this.
//!
//! ```c
//! static unsigned long rotate(const unsigned char *p, const unsigned char *end, unsigned long a) {
//!     if (p == end) return a;
//!     return rotate(p + 1, end, a + *p);
//! }
//! ```
//!
//! The call hands the function its own arguments again, and what it gives back is what the function
//! gives back. So the call is a jump to the top of the function with the new arguments, and the
//! function is a loop. The code generator makes a jump of such a call on a machine that has one,
//! but the frame is still made and given back on each step. On wasm without the tail call feature
//! there is no such jump at all: each step was a call and a frame, and a long chain ran out of
//! stack where gcc's loop did not. Done here, it is a loop on every target, and the loop passes
//! after it see it as one.
//!
//! # The shape
//!
//! The entry block takes the arguments and nothing may branch to it, so its work moves into a new
//! block that takes them as parameters, the header, and the entry block only jumps there. Each
//! call to the function itself whose results are what the function then returns becomes a jump to
//! the header with the call's arguments. That is a `return` straight after the call, or a jump to a
//! block that only returns what it was handed, which is the two shapes `rucc_codegen::tail` takes
//! too.
//!
//! # What turns a function down
//!
//! Each call had a frame of its own and the loop has one, so anything in the frame turns the
//! function down. That is an `alloca`, which a pointer the next step was handed could point at, a
//! variable argument list, the arguments kept in the frame for `__builtin_apply`, a saved stack
//! pointer, and a question about the frame or the return address. A function that saves a place to
//! come back to is turned down too, the same as in gcc, because a `longjmp` comes back to the step
//! that saved it with that step's arguments. And memory SSA, which a jump would have to carry.
//!
//! An argument passed as the address of a copy, a structure by value or a returned one, turns the
//! function down as well: the call made the copy and the jump would not.

use rucc_base::hash::Map;
use rucc_ir::{Abi, AttrSet, Block, Builder, Extra, Func, Inst, Opcode, Value};

use crate::{Analyses, Fuel, Pass, Preserved, Stats, uses};

/// What `-fno-optimize-sibling-calls` turns off along with the jumps, and the name of the pass.
pub const NAME: &str = "tail-recursion";

/// A call to the function itself is a jump back to its top.
const LOOPED: &str = "a call to the function itself in tail position became a jump to its top";
/// The frame is in the way.
const FRAME: &str = "a call to the function itself stays a call, the frame holds something";
/// Something can come back twice.
const TWICE: &str =
    "a call to the function itself stays a call, the function saves a place to come back to";
/// Ran out.
const NO_FUEL: &str = "a call to the function itself stays a call, the pass ran out of fuel";

/// The pass.
#[derive(Debug)]
pub struct TailRecursion;

impl Pass for TailRecursion {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a call a function makes to itself in tail position becomes a jump back to its top"
    }

    fn preserves(&self) -> Preserved {
        // A block and edges back to it, so nothing about the graph stands.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let calls = calls(func);
        if calls.is_empty() {
            return stats;
        }
        if let Some(why) = refused(func, an) {
            for _ in &calls {
                stats.missed(why);
            }
            return stats;
        }
        let mut taken = Vec::with_capacity(calls.len());
        for (call, ret) in calls {
            if !fuel.take() {
                stats.missed(NO_FUEL);
                break;
            }
            taken.push((call, ret));
        }
        if taken.is_empty() {
            return stats;
        }
        let header = header(func);
        let mut left = Vec::new();
        for (call, ret) in taken {
            let block = func.block_of(call).expect("the call is in a block");
            left.extend(func.successors(ret).map(|target| target.block));
            let args = func[func[call].args].to_vec();
            let span = func.span(call);
            func.remove_inst(ret);
            func.remove_inst(call);
            Builder::new(func, block).at(span).jump(header, &args);
            stats.optimized(LOOPED);
        }
        // A return every arm jumped to and none does now is a block nothing reaches.
        let reached: Vec<Block> = func
            .blocks()
            .filter_map(|block| func.terminator(block))
            .flat_map(|last| func.successors(last).map(|target| target.block))
            .collect();
        left.sort_unstable();
        left.dedup();
        for block in left.into_iter().filter(|block| !reached.contains(block)) {
            func.remove_block(block);
        }
        stats
    }
}

/// Each call the function makes to itself in tail position, with the instruction that ends its
/// block.
fn calls(func: &Func) -> Vec<(Inst, Inst)> {
    let params: Vec<_> = func.signature().params.iter().map(|param| param.ty).collect();
    let mut out = Vec::new();
    for block in func.blocks() {
        let Some(ret) = func.terminator(block) else { continue };
        let Some(call) = func.prev_inst(ret) else { continue };
        if func[call].opcode != Opcode::Call || func.unwinds_to_pad(call) {
            continue;
        }
        let Extra::Call(info) = func[call].extra else { continue };
        let info = func[info];
        if info.callee != Some(func.name) || !func[info.varargs].is_empty() {
            continue;
        }
        // Called with the function's own type, so the arguments are what the parameters take and
        // the results are what the function gives back.
        if func[info.signature] != *func.signature() {
            continue;
        }
        let args = &func[func[call].args];
        if args.len() != params.len()
            || args.iter().zip(&params).any(|(&arg, &ty)| func[arg].ty != ty)
        {
            continue;
        }
        let results: Vec<Value> = func[call].results().collect();
        if returned(func, ret).is_some_and(|given| given == results) {
            out.push((call, ret));
        }
    }
    out
}

/// What the function gives back when that instruction ends its block, or `None` when it does not
/// end the function: a `return`, or a jump to a block that only returns what it was handed, in the
/// order it was handed it.
fn returned(func: &Func, ret: Inst) -> Option<Vec<Value>> {
    match func[ret].opcode {
        Opcode::Return => Some(func[func[ret].args].to_vec()),
        Opcode::Jump => {
            let [target] = func.successors(ret).collect::<Vec<_>>()[..] else { return None };
            let mut insts = func.insts(target.block);
            let (Some(only), None) = (insts.next(), insts.next()) else { return None };
            let same = func[only].opcode == Opcode::Return
                && func[func[only].args] == func[target.block].params[..];
            same.then(|| func[target.args].to_vec())
        }
        _ => None,
    }
}

/// Why no call to the function itself can be a jump to its top, or `None` when one can.
fn refused(func: &Func, an: &Analyses) -> Option<&'static str> {
    if func.attrs.set.contains(AttrSet::NAKED) || func.signature().variadic {
        return Some(FRAME);
    }
    let copied = func.signature().params.iter().any(|param| param.abi.indirect());
    if copied || func.signature().params.iter().any(|param| param.abi == Abi::Chain) {
        return Some(FRAME);
    }
    for block in func.blocks() {
        for inst in func.insts(block) {
            match func[inst].opcode {
                Opcode::Alloca
                | Opcode::VaStart
                | Opcode::ApplyArgs
                | Opcode::StackSave
                | Opcode::FrameAddress
                | Opcode::ReturnAddress
                | Opcode::MemEntry => return Some(FRAME),
                Opcode::SetjmpMarker => return Some(TWICE),
                Opcode::Call => {
                    let Extra::Call(info) = func[inst].extra else { continue };
                    let twice = func[info].callee.is_some_and(|callee| {
                        callee != func.name && an.outside().may_return_twice(callee)
                    });
                    if twice {
                        return Some(TWICE);
                    }
                }
                _ => (),
            }
        }
    }
    None
}

/// Moves the work of the entry block into a new block that takes the arguments, leaves the entry
/// block a jump to it, and gives back the new block.
fn header(func: &mut Func) -> Block {
    let entry = func.entry().expect("a function with a call in it has a body");
    let header = func.create_block();
    let params = func[entry].params.clone();
    let mut forward = Map::default();
    for &param in &params {
        let ty = func[param].ty;
        forward.insert(param, func.append_param(header, ty));
    }
    let insts: Vec<Inst> = func.insts(entry).collect();
    for inst in insts {
        func.remove_inst(inst);
        func.append_inst(header, inst);
    }
    func.carry_starts(entry, header, None);
    uses::substitute(func, &forward);
    Builder::new(func, entry).jump(header, &params);
    header
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rucc_base::Interner;

    use super::TailRecursion;
    use crate::outside::Outside;
    use crate::stats::Kind;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it, and the count of
    /// calls it made jumps of.
    fn run(body: &str) -> (String, u32) {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let outside = Arc::new(Outside::of(&module).knowing_twice(&module, &names));
        let ids: Vec<_> = module.funcs().collect();
        let mut looped = 0;
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses().about(Arc::clone(&outside));
            let stats = TailRecursion.run(&mut module[id], &mut an, &mut Fuel::unlimited());
            looped += stats.count(Kind::Optimized, super::LOOPED);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        (rucc_ir::print(&module, &names), looped)
    }

    /// `rotate` from the corpus, cut down to three arguments.
    const ROTATE: &str = r#"
func @rotate(ptr, ptr, i64) -> i64, linkage(internal) {
block0(%0: ptr, %1: ptr, %2: i64):
    %3 = icmp eq %0, %1
    br_if %3, block1, block2

block1:
    return %2

block2:
    %4 = iconst.i32 1
    %5 = ptr_add %0, %4
    %6 = load.i8 %0, align 1
    %7 = zext.i64 %6
    %8 = add %2, %7
    %9 = call @rotate(%5, %1, %8) : (ptr, ptr, i64) -> i64
    return %9
}
"#;

    #[test]
    fn a_call_to_itself_whose_answer_is_returned_becomes_a_jump_to_the_top() {
        let (out, looped) = run(ROTATE);
        assert_eq!(looped, 1, "{out}");
        assert!(!out.contains("call @rotate"), "{out}");
        assert!(out.contains("jump block3(%0, %1, %2)"), "the entry hands its arguments on\n{out}");
        assert!(out.contains("jump block3(%"), "{out}");
    }

    #[test]
    fn a_call_that_jumps_to_a_return_of_its_answer_becomes_a_jump_to_the_top() {
        let (out, looped) = run(r#"
func @count(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 0
    %3 = icmp eq %0, %2
    br_if %3, block1(%1), block2

block1(%4: i32):
    return %4

block2:
    %5 = iconst.i32 1
    %6 = sub %0, %5
    %7 = add %1, %5
    %8 = call @count(%6, %7) : (i32, i32) -> i32
    jump block1(%8)
}
"#);
        assert_eq!(looped, 1, "{out}");
        assert!(!out.contains("call @count"), "{out}");
    }

    #[test]
    fn a_call_to_itself_with_work_after_it_stays_a_call() {
        let (out, looped) = run(r#"
func @fact(i64) -> i64, linkage(external) {
block0(%0: i64):
    %1 = iconst.i64 1
    %2 = icmp ule %0, %1
    br_if %2, block1, block2

block1:
    return %1

block2:
    %3 = sub %0, %1
    %4 = call @fact(%3) : (i64) -> i64
    %5 = mul %0, %4
    return %5
}
"#);
        assert_eq!(looped, 0, "{out}");
        assert!(out.contains("call @fact"), "{out}");
    }

    #[test]
    fn a_local_in_the_frame_keeps_the_call() {
        let (out, looped) = run(r#"
func @use(ptr), linkage(external);

func @walk(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    call @use(%1) : (ptr)
    %2 = iconst.i32 0
    %3 = icmp eq %0, %2
    br_if %3, block1, block2

block1:
    return %0

block2:
    %4 = iconst.i32 1
    %5 = sub %0, %4
    %6 = call @walk(%5) : (i32) -> i32
    return %6
}
"#);
        assert_eq!(looped, 0, "{out}");
        assert!(out.contains("call @walk"), "{out}");
    }

    #[test]
    fn a_call_to_sigsetjmp_keeps_the_call() {
        let (out, looped) = run(r#"
func @__sigsetjmp(ptr, i32) -> i32, linkage(external);

func @retry(ptr, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = iconst.i32 0
    %3 = call @__sigsetjmp(%0, %2) : (ptr, i32) -> i32
    %4 = icmp eq %1, %2
    br_if %4, block1, block2

block1:
    return %3

block2:
    %5 = iconst.i32 1
    %6 = sub %1, %5
    %7 = call @retry(%0, %6) : (ptr, i32) -> i32
    return %7
}
"#);
        assert_eq!(looped, 0, "{out}");
    }

    #[test]
    fn a_function_that_returns_nothing_loops_too() {
        let (out, looped) = run(r#"
func @clear(ptr, i64), linkage(external) {
block0(%0: ptr, %1: i64):
    %2 = iconst.i64 0
    %3 = icmp eq %1, %2
    br_if %3, block1, block2

block1:
    return

block2:
    %4 = iconst.i8 0
    store %4 -> %0, align 1
    %5 = iconst.i32 1
    %6 = ptr_add %0, %5
    %7 = iconst.i64 1
    %8 = sub %1, %7
    call @clear(%6, %8) : (ptr, i64)
    return
}
"#);
        assert_eq!(looped, 1, "{out}");
        assert!(!out.contains("call @clear"), "{out}");
    }
}
