//! The calls that tell the runtime's stack plane where a local begins and ends.
//!
//! Row Y6 of document 03 is a read of a local nothing wrote, and the runtime half of it is
//! `rucc_safe_rt::stack`, which keeps one init bit per byte of the thread's stack. A bit is only
//! worth anything once somebody has said which bytes are a local that has not been written yet, and
//! that is what this pass puts in: `__rucc_local_begin(base, size)` at the top of the function for
//! each local it picks, again at each `meta_begin` of it, and `__rucc_local_end(base, size)` in front
//! of every return.
//!
//! The bytes an `alloca` or a variable length array takes off the stack are begun where they are
//! taken, with the size the program asked for. Nothing keeps that size until they go back, and they
//! go back all at once, so what ends them is `__rucc_local_left(top)`, which says every byte below
//! `top` is gone: behind each `stackrestore` with the pointer it restored, and in front of every
//! return with the frame's own address, which is above everything the function took.
//!
//! A `meta_begin` is where the front end reached the declaration of a local in a block, and C11
//! 6.2.4p6 says the value of one is indeterminate each time its declaration is reached. So a local
//! that loses its value at the bottom of a loop and is read at the top of the next time round is a
//! read of nothing, and saying so takes beginning it again there.
//!
//! # Which locals
//!
//! A local is begun only when two things hold. Some `check_init` reads through it, because a local
//! nobody asks about gains nothing from being tracked. And its address goes nowhere this pass
//! cannot see, because a write by code that was not built with the flag, a library routine handed
//! the address or a pointer stored into memory and written through later, would leave the plane
//! saying the bytes were never written when they were, and that is a refusal of a correct program.
//! A `lifetime_end` does not count, since a build with this pass in it never gives two locals the
//! same bytes.
//!
//! Every way of using the address that is not on the list in `kept` counts as it going
//! somewhere. That is the conservative way round: a local wrongly left out is a read the plane does
//! not ask about, which is what happened before this pass existed.
//!
//! # `setjmp`
//!
//! A `longjmp` leaves every frame below the one it lands in without running their ends. The
//! runtime forgets those frames on the next begin, from how deep the stack has been, but the frame
//! a `longjmp` lands in can go on to call functions that begin nothing and whose locals sit on
//! those bytes. So a `setjmp` gets `__rucc_local_landed()` behind it, which forgets everything below
//! the frame it is called from. It runs on both returns, and on the first one there is nothing below
//! to forget.

use rucc_base::Interner;
use rucc_base::hash::{Map, Set};
use rucc_ir::{Extra, Func, Imm, Inst, InstData, Opcode, Type, Value};

/// The runtime entry that says a local's bytes have not been written yet.
const BEGIN: &str = "__rucc_local_begin";

/// The runtime entry that says a local is gone.
const END: &str = "__rucc_local_end";

/// The runtime entry a `setjmp` is followed by.
const LANDED: &str = "__rucc_local_landed";

/// The runtime entry that says every byte below an address on the stack is gone.
const LEFT: &str = "__rucc_local_left";

/// Puts the begins, the ends and the landings into `func`.
///
/// Called before the checks are lowered, because which locals are wanted is read off the
/// `check_init` instructions and those are calls once the lowering has run.
pub(crate) fn begin(func: &mut Func, names: &mut Interner, word: Type) {
    let insts: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for &inst in &insts {
        if func[inst].opcode != Opcode::SetjmpMarker {
            continue;
        }
        let data = crate::lower::calling(func, names, LANDED, &[], &[], &[]);
        let landed = func.create_inst(data, &[], func.span(inst));
        func.insert_after(landed, inst);
    }
    let Some(entry) = func.entry() else { return };
    // The first thing in the entry block that is not an `alloca`, which is where every local in
    // front of it already has its address and nothing has used it yet.
    let Some(first) = func.insts(entry).find(|&inst| func[inst].opcode != Opcode::Alloca) else {
        return;
    };
    let locals: Vec<(Value, u64)> = func
        .insts(entry)
        .take_while(|&inst| inst != first)
        .filter_map(|inst| sized(func, inst))
        .collect();
    let grown: Vec<(Inst, Value, Value)> =
        insts.iter().filter_map(|&inst| grown(func, inst, word)).collect();
    let bases: Vec<Value> = locals
        .iter()
        .map(|&(base, _)| base)
        .chain(grown.iter().map(|&(_, base, _)| base))
        .collect();
    let wanted = wanted(func, &insts, &bases);
    if wanted.is_empty() {
        return;
    }
    let ends: Vec<Inst> = func
        .blocks()
        .filter_map(|block| func.terminator(block))
        .filter(|&end| matches!(func[end].opcode, Opcode::Return | Opcode::TailCall))
        .collect();
    for (base, size) in locals {
        if !wanted.contains(&base) {
            continue;
        }
        let params = [Type::PTR, word];
        let len = crate::lower::konst(func, first, Imm::int(i128::from(size), word), word);
        let data = crate::lower::calling(func, names, BEGIN, &params, &[], &[base, len]);
        let made = func.create_inst(data, &[], func.span(first));
        func.insert_before(made, first);
        for &inst in &insts {
            if func[inst].opcode != Opcode::MetaBegin
                || func[func[inst].args].first() != Some(&base)
            {
                continue;
            }
            let len = crate::lower::konst(func, inst, Imm::int(i128::from(size), word), word);
            let data = crate::lower::calling(func, names, BEGIN, &params, &[], &[base, len]);
            let made = func.create_inst(data, &[], func.span(inst));
            func.insert_before(made, inst);
        }
        for &end in &ends {
            let len = crate::lower::konst(func, end, Imm::int(i128::from(size), word), word);
            let data = crate::lower::calling(func, names, END, &params, &[], &[base, len]);
            let made = func.create_inst(data, &[], func.span(end));
            func.insert_before(made, end);
        }
    }
    let mut took = false;
    for (inst, base, size) in grown {
        if !wanted.contains(&base) {
            continue;
        }
        let params = [Type::PTR, word];
        let data = crate::lower::calling(func, names, BEGIN, &params, &[], &[base, size]);
        let made = func.create_inst(data, &[], func.span(inst));
        func.insert_after(made, inst);
        took = true;
    }
    if took {
        given_back(func, names, &insts, &ends);
    }
}

/// The ends of what an `alloca` took: everything below the pointer each `stackrestore` puts back,
/// behind it, and everything below the frame in front of each return.
fn given_back(func: &mut Func, names: &mut Interner, insts: &[Inst], ends: &[Inst]) {
    for &inst in insts {
        if func[inst].opcode != Opcode::StackRestore {
            continue;
        }
        let Some(&saved) = func[func[inst].args].first() else { continue };
        let data = crate::lower::calling(func, names, LEFT, &[Type::PTR], &[], &[saved]);
        let made = func.create_inst(data, &[], func.span(inst));
        func.insert_after(made, inst);
    }
    for &end in ends {
        let span = func.span(end);
        let data = InstData { extra: Extra::Depth(0), ..InstData::new(Opcode::FrameAddress) };
        let frame = func.create_inst(data, &[Type::PTR], span);
        func.insert_before(frame, end);
        let Some(top) = func[frame].results().next() else { continue };
        let data = crate::lower::calling(func, names, LEFT, &[Type::PTR], &[], &[top]);
        let made = func.create_inst(data, &[], span);
        func.insert_before(made, end);
    }
}

/// The address and size of a local of a size known here, or nothing for anything else.
///
/// A variable length array is an `alloca` with an operand, and [`grown`] is the one for that.
fn sized(func: &Func, inst: Inst) -> Option<(Value, u64)> {
    let data = &func[inst];
    let Extra::Mem(mem) = data.extra else { return None };
    if data.opcode != Opcode::Alloca || !func[data.args].is_empty() || func[mem].size == 0 {
        return None;
    }
    Some((data.results().next()?, func[mem].size))
}

/// An `alloca` with an operand, which is `__builtin_alloca` or a variable length array: where it is,
/// its address, and the number of bytes it took when that is a number as wide as `word`.
fn grown(func: &Func, inst: Inst, word: Type) -> Option<(Inst, Value, Value)> {
    let data = &func[inst];
    if data.opcode != Opcode::Alloca {
        return None;
    }
    let &[size] = &func[data.args] else { return None };
    if func[size].ty != word {
        return None;
    }
    Some((inst, data.results().next()?, size))
}

/// Which of `locals` a `check_init` reads through and nothing lets go of.
fn wanted(func: &Func, insts: &[Inst], locals: &[Value]) -> Set<Value> {
    // Every value that is one of the locals or an address inside one, mapped to the local. A
    // block parameter is one of them when something passed to it is, which is how a pointer
    // stepped round a loop looks once the optimizer has had it, and a parameter that is also
    // passed something else lets its local go further down.
    let mut root: Map<Value, Value> = locals.iter().map(|&base| (base, base)).collect();
    loop {
        let before = root.len();
        for &inst in insts {
            for call in func.successors(inst) {
                for (value, &param) in func[call.args].iter().zip(&func[call.block].params) {
                    if let Some(&local) = root.get(value) {
                        root.entry(param).or_insert(local);
                    }
                }
            }
            if func[inst].opcode != Opcode::PtrAdd {
                continue;
            }
            let Some(&base) = func[func[inst].args].first() else { continue };
            let Some(&local) = root.get(&base) else { continue };
            for result in func[inst].results() {
                root.insert(result, local);
            }
        }
        if root.len() == before {
            break;
        }
    }
    let mut asked: Set<Value> = Set::default();
    let mut gone: Set<Value> = Set::default();
    for &inst in insts {
        let opcode = func[inst].opcode;
        for (index, value) in func[func[inst].args].iter().enumerate() {
            let Some(&local) = root.get(value) else { continue };
            if opcode == Opcode::CheckInit && index == 1 {
                asked.insert(local);
            }
            if !kept(opcode, index) {
                gone.insert(local);
            }
        }
        for call in func.successors(inst) {
            for (value, param) in func[call.args].iter().zip(&func[call.block].params) {
                let (from, to) = (root.get(value), root.get(param));
                if from != to {
                    gone.extend(from.into_iter().chain(to).copied());
                }
            }
        }
    }
    asked.retain(|local| !gone.contains(local));
    asked
}

/// Whether an address in operand `index` of an `opcode` stays where this pass can see it.
///
/// A `lifetime_end` is here, because nothing shares a local's bytes in a build that has this pass.
fn kept(opcode: Opcode, index: usize) -> bool {
    match opcode {
        Opcode::Load | Opcode::PtrAdd | Opcode::Memset | Opcode::LifetimeEnd => index == 0,
        Opcode::Store => index == 1,
        Opcode::Memcpy | Opcode::Memmove => index < 2,
        Opcode::ICmp => true,
        _ => {
            opcode.touches_only_planes() || opcode.makes_capability() || opcode == Opcode::CapStore
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The calls the pass put into the one function in `body`, in the order they come.
    fn calls(body: &str) -> Vec<String> {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let id = module.funcs().find(|&id| !module[id].is_declaration()).expect("one function");
        begin(&mut module[id], &mut names, Type::int(64));
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        let func = &module[id];
        func.blocks()
            .flat_map(|block| func.insts(block))
            .filter_map(|inst| match func[inst].extra {
                Extra::Call(info) => func[info].callee.map(|name| names.resolve(name).to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_local_an_init_check_reads_is_begun_at_the_top_and_ended_at_each_return() {
        let got = calls(
            r#"
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = alloca, size 32, align 16
    %2 = cap_of %1
    %3 = iconst.i64 12
    %4 = ptr_add.nuw %1, %3
    br_if %0, block1, block2

block1:
    check_init %2, %4, size 4, align 4
    %5 = load.i32 %4, align 4
    return %5

block2:
    %6 = iconst.i32 7
    return %6
}
"#,
        );
        assert_eq!(got, [BEGIN, END, END]);
    }

    #[test]
    fn a_local_whose_address_is_stored_somewhere_is_left_alone() {
        let got = calls(
            r#"
func @f(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = alloca, size 4, align 4
    %2 = cap_of %1
    store %1 -> %0, align 8
    check_init %2, %1, size 4, align 4
    %3 = load.i32 %1, align 4
    return %3
}
"#,
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn a_local_in_a_block_is_begun_again_where_its_declaration_is_reached() {
        let got = calls(
            r#"
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = alloca, size 4, align 4
    %2 = cap_of %1
    %3 = iconst.i64 4
    jump block1

block1:
    meta_begin %1, %3, class automatic
    check_init %2, %1, size 4, align 4
    %4 = load.i32 %1, align 4
    lifetime_end %1
    br_if %0, block1, block2

block2:
    return %4
}
"#,
        );
        assert_eq!(got, [BEGIN, BEGIN, END]);
    }

    #[test]
    fn a_local_nothing_asks_about_is_left_alone() {
        let got = calls(
            r#"
func @f() -> i32, linkage(external) {
block0:
    %0 = alloca, size 4, align 4
    %1 = iconst.i32 3
    store %1 -> %0, align 4
    %2 = load.i32 %0, align 4
    return %2
}
"#,
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn what_an_alloca_took_is_begun_where_it_is_taken_and_left_at_each_return() {
        let got = calls(
            r#"
func @f(i64, i1) -> i32, linkage(external) {
block0(%0: i64, %1: i1):
    %2 = alloca %0, align 16
    %3 = cap_of %2
    br_if %1, block1, block2

block1:
    check_init %3, %2, size 4, align 4
    %4 = load.i32 %2, align 4
    return %4

block2:
    %5 = iconst.i32 7
    return %5
}
"#,
        );
        assert_eq!(got, [BEGIN, LEFT, LEFT]);
    }

    #[test]
    fn what_a_scope_gives_back_is_left_behind_the_restore() {
        let got = calls(
            r#"
func @f(i64) -> i32, linkage(external) {
block0(%0: i64):
    %1 = stacksave.ptr
    %2 = alloca %0, align 16
    %3 = cap_of %2
    check_init %3, %2, size 4, align 4
    %4 = load.i32 %2, align 4
    stackrestore %1
    return %4
}
"#,
        );
        assert_eq!(got, [BEGIN, LEFT, LEFT]);
    }

    #[test]
    fn what_an_alloca_took_and_handed_to_a_call_is_left_alone() {
        let got = calls(
            r#"
func @fill(ptr), linkage(external);

func @f(i64) -> i32, linkage(external) {
block0(%0: i64):
    %1 = alloca %0, align 16
    %2 = cap_of %1
    call @fill(%1) : (ptr)
    check_init %2, %1, size 4, align 4
    %3 = load.i32 %1, align 4
    return %3
}
"#,
        );
        assert_eq!(got, ["fill"]);
    }

    #[test]
    fn a_pointer_stepped_round_a_loop_through_a_local_keeps_it() {
        let got = calls(
            r#"
func @f(i64) -> i32, linkage(external) {
block0(%0: i64):
    %1 = alloca %0, align 16
    %2 = cap_of %1
    %3 = iconst.i32 7
    jump block1(%1)

block1(%4: ptr):
    store %3 -> %4, align 4
    %5 = iconst.i64 4
    %6 = ptr_add %4, %5
    %7 = icmp ult %6, %1
    br_if %7, block1(%6), block2

block2:
    check_init %2, %1, size 4, align 4
    %8 = load.i32 %1, align 4
    return %8
}
"#,
        );
        assert_eq!(got, [BEGIN, LEFT]);
    }

    #[test]
    fn a_parameter_that_is_also_passed_some_other_pointer_lets_the_local_go() {
        let got = calls(
            r#"
func @f(i64, ptr, i1) -> i32, linkage(external) {
block0(%0: i64, %1: ptr, %2: i1):
    %3 = alloca %0, align 16
    %4 = cap_of %3
    br_if %2, block1(%3), block1(%1)

block1(%5: ptr):
    %6 = iconst.i32 7
    store %6 -> %5, align 4
    check_init %4, %3, size 4, align 4
    %7 = load.i32 %3, align 4
    return %7
}
"#,
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn a_setjmp_is_followed_by_a_landing() {
        let got = calls(
            r#"
func @f(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = setjmp_marker.i32 %0
    return %1
}
"#,
        );
        assert_eq!(got, [LANDED]);
    }
}
