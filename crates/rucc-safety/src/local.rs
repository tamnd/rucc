//! The calls that tell the runtime's stack plane where a local begins and ends.
//!
//! Row Y6 of document 03 is a read of a local nothing wrote, and the runtime half of it is
//! `rucc_safe_rt::stack`, which keeps one init bit per byte of the thread's stack. A bit is only
//! worth anything once somebody has said which bytes are a local that has not been written yet, and
//! that is what this pass puts in: `__rucc_local_begin(base, size)` at the top of the function for
//! each local it picks, again at each `meta_begin` of it, and `__rucc_local_end(base, size)` in front
//! of every return.
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
use rucc_ir::{Extra, Func, Imm, Inst, Opcode, Type, Value};

/// The runtime entry that says a local's bytes have not been written yet.
const BEGIN: &str = "__rucc_local_begin";

/// The runtime entry that says a local is gone.
const END: &str = "__rucc_local_end";

/// The runtime entry a `setjmp` is followed by.
const LANDED: &str = "__rucc_local_landed";

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
    let wanted = wanted(func, &insts, &locals);
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
}

/// The address and size of a local of a size known here, or nothing for anything else.
///
/// A variable length array is an `alloca` with an operand, and it is left out because its bytes are
/// handed out again by `stack_restore` with nothing to say so.
fn sized(func: &Func, inst: Inst) -> Option<(Value, u64)> {
    let data = &func[inst];
    let Extra::Mem(mem) = data.extra else { return None };
    if data.opcode != Opcode::Alloca || !func[data.args].is_empty() || func[mem].size == 0 {
        return None;
    }
    Some((data.results().next()?, func[mem].size))
}

/// Which of `locals` a `check_init` reads through and nothing lets go of.
fn wanted(func: &Func, insts: &[Inst], locals: &[(Value, u64)]) -> Set<Value> {
    // Every value that is one of the locals or an address inside one, mapped to the local. The
    // offsets come before their uses in any order the blocks are walked in except round a loop,
    // which a pointer stepped by a block parameter is, and that goes through a block call and
    // counts as letting go.
    let mut root: Map<Value, Value> = locals.iter().map(|&(base, _)| (base, base)).collect();
    loop {
        let before = root.len();
        for &inst in insts {
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
            gone.extend(func[call.args].iter().filter_map(|value| root.get(value).copied()));
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
        let id = module.funcs().next().expect("one function");
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
