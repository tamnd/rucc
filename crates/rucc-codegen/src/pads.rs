//! Values a landing pad reads, put in the frame for it.
//!
//! A landing pad has no machine edge into it. The unwinder starts it with the registers that the
//! frame rules of the call put back, so the lowering can give a pad only what it can make again
//! from nothing: a constant, the address of a slot of the frame and the address of a name. See
//! `Lower::pad` in [`crate::lower`]. The front end keeps to that, because a cleanup is handed the
//! address of its variable.
//!
//! The optimizer does not keep to it. When the cleanup function is inlined into the pad, the
//! address of the variable is no longer taken, and the variable becomes the value it held. The pad
//! then reads an argument of the function or a sum the function made before the call, and the
//! lowering refused the function. A C file with `-fexceptions`, a `cleanup` attribute and a cleanup
//! function that can be inlined did not compile at `-O1` and above.
//!
//! So each such value is stored to a slot of its own where it is made, and each block a pad leads
//! to loads it back before it reads it. That covers a block after the pad, and a pad that a call
//! in another pad unwinds into. The slot always holds the value wherever the value is defined, so
//! a block that the normal path goes through as well can load it too. This is what gcc does with
//! a value that is live into a landing pad and in no register a call keeps.

use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, Def, Extra, Func, Inst, InstData, MemInfo, MemOrder, Opcode, Restrict, Type, Value,
};

/// Puts each value a landing pad reads, and cannot make again, in a slot of the frame.
///
/// `word` is the size of an address in bytes.
///
/// # Panics
///
/// If a block has no terminator, which a function has none of by the time it is lowered.
pub fn spill(func: &mut Func, word: u32) {
    let pads: Vec<Block> = func
        .blocks()
        .filter(|&block| {
            func.insts(block).next().is_some_and(|first| func[first].opcode == Opcode::Landing)
        })
        .collect();
    if pads.is_empty() {
        return;
    }
    let after = reach(func, &pads);
    let calls: Set<Inst> = pads.iter().flat_map(|&pad| calls_into(func, pad)).collect();
    let mut wanted: Vec<Value> = Vec::new();
    let mut index: Map<Value, usize> = Map::default();
    let mut readers: Vec<(Block, Vec<Value>)> = Vec::new();
    for &block in &after.found {
        let mut read = Vec::new();
        for value in reads(func, block) {
            if read.contains(&value) || !outside(func, value, &after.seen, &calls) {
                continue;
            }
            index.entry(value).or_insert_with(|| {
                wanted.push(value);
                wanted.len() - 1
            });
            read.push(value);
        }
        if !read.is_empty() {
            readers.push((block, read));
        }
    }
    if wanted.is_empty() {
        return;
    }
    // Where each store goes, found before the slots are made so that a store of a parameter of
    // the entry block comes after them.
    let stores: Vec<Inst> = wanted
        .iter()
        .map(|&value| match func[value].def {
            Def::Result { inst, .. } => {
                func.next_inst(inst).expect("a value is not made by a terminator")
            }
            Def::Param { block, .. } => func.insts(block).next().expect("a block has a terminator"),
        })
        .collect();
    let entry = func.entry().expect("a function with a pad has a body");
    let top = func.insts(entry).next().expect("the entry block has a terminator");
    let mut slots = Vec::with_capacity(wanted.len());
    for &value in &wanted {
        let info = slot(func[value].ty, word);
        let span = func.span(top);
        let extra = Extra::Mem(func.add_mem(info));
        let made = func.create_inst(
            InstData { extra, ..InstData::new(Opcode::Alloca) },
            &[Type::PTR],
            span,
        );
        func.insert_before(made, top);
        slots.push((func[made].first_result.expect("an alloca has a result"), info));
    }
    for ((&value, &(address, info)), &before) in wanted.iter().zip(&slots).zip(&stores) {
        let span = func.span(before);
        let extra = Extra::Mem(func.add_mem(info));
        let args = func.push_values(&[value, address]);
        let store =
            func.create_inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[], span);
        func.insert_before(store, before);
    }
    for (block, read) in readers {
        let first = func.insts(block).next().expect("a block has a terminator");
        let mut last = (func[first].opcode == Opcode::Landing).then_some(first);
        let mut loaded: Vec<(Value, Value)> = Vec::with_capacity(read.len());
        for value in read {
            let (address, info) = slots[index[&value]];
            let span = func.span(first);
            let extra = Extra::Mem(func.add_mem(info));
            let args = func.push_values(&[address]);
            let load = func.create_inst(
                InstData { args, extra, ..InstData::new(Opcode::Load) },
                &[func[value].ty],
                span,
            );
            match last {
                Some(at) => func.insert_after(load, at),
                None => func.insert_before(load, first),
            }
            last = Some(load);
            loaded.push((value, func[load].first_result.expect("a load has a result")));
        }
        let with = |value: Value| {
            loaded.iter().find(|&&(from, _)| from == value).map_or(value, |&(_, to)| to)
        };
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            let args = func[inst].args;
            func.rewrite(args, with);
            for call in func.successors(inst).collect::<Vec<_>>() {
                func.rewrite(call.args, with);
            }
        }
    }
}

/// The values an instruction of the block reads, with the arguments it passes to a block.
fn reads(func: &Func, block: Block) -> Vec<Value> {
    let mut read = Vec::new();
    for inst in func.insts(block) {
        read.extend_from_slice(&func[func[inst].args]);
        for call in func.successors(inst) {
            read.extend_from_slice(&func[call.args]);
        }
    }
    read
}

/// The calls whose unwind edge goes to the pad.
///
/// The edge is a `br_if` on the `unwound` straight after the call, with the pad as its first arm,
/// which is the shape the lowering reads.
fn calls_into(func: &Func, pad: Block) -> Vec<Inst> {
    let mut calls = Vec::new();
    for block in func.blocks() {
        let Some(term) = func.terminator(block) else { continue };
        if func[term].opcode != Opcode::BrIf {
            continue;
        }
        let Some(&cond) = func[func[term].args].first() else { continue };
        let Def::Result { inst: unwound, .. } = func[cond].def else { continue };
        if func[unwound].opcode != Opcode::Unwound {
            continue;
        }
        if func.successors(term).next().map(|arm| arm.block) != Some(pad) {
            continue;
        }
        if let Some(call) = func.prev_inst(unwound) {
            calls.push(call);
        }
    }
    calls
}

/// The pads and every block a pad leads to, in the order they are found and as a set.
struct Reach {
    found: Vec<Block>,
    seen: Set<Block>,
}

/// The pads and every block a pad leads to.
fn reach(func: &Func, pads: &[Block]) -> Reach {
    let mut found = pads.to_vec();
    let mut seen: Set<Block> = pads.iter().copied().collect();
    let mut at = 0;
    while at < found.len() {
        let block = found[at];
        at += 1;
        let Some(term) = func.terminator(block) else { continue };
        for call in func.successors(term).collect::<Vec<_>>() {
            if seen.insert(call.block) {
                found.push(call.block);
            }
        }
    }
    Reach { found, seen }
}

/// Whether a pad reads this value from before the call, the lowering cannot make it again in the
/// pad, and a slot can hold it.
///
/// A value made in a block no pad leads to is live into a pad when a block a pad leads to reads it,
/// since no path from the pad goes through where it is made. The result of a call that unwinds
/// into a pad has no value there, and neither has a value made by a terminator or straight before
/// an `unwound`, which a store cannot follow, so these are left as they were.
fn outside(func: &Func, value: Value, after: &Set<Block>, calls: &Set<Inst>) -> bool {
    let block = match func[value].def {
        Def::Result { inst, .. } => {
            match func[inst].opcode {
                Opcode::IConst | Opcode::GlobalAddr | Opcode::Unwound => return false,
                Opcode::Alloca if func[func[inst].args].is_empty() => return false,
                _ => {}
            }
            if calls.contains(&inst) || func.is_terminator(inst) {
                return false;
            }
            let next = func.next_inst(inst);
            if next.is_none_or(|next| func[next].opcode == Opcode::Unwound) {
                return false;
            }
            func.block_of(inst)
        }
        Def::Param { block, .. } => Some(block),
    };
    block.is_some_and(|block| !after.contains(&block)) && fits(func[value].ty)
}

/// Whether a slot of the frame holds a value of this type with one load and one store.
///
/// Not a single bit, which has no size in memory, and not an x87 number, whose slot is wider than
/// its bits. Either one is left as it was, and the lowering refuses it as before.
fn fits(ty: Type) -> bool {
    if ty.is_ptr() {
        return true;
    }
    if !ty.is_int() && !ty.is_float() {
        return false;
    }
    let bits = ty.bits() * ty.lanes();
    ty.bits() >= 8
        && ty.bits() != 80
        && bits % 8 == 0
        && (bits / 8).is_power_of_two()
        && bits <= 512
}

/// The slot for a value of this type, and each access to it.
fn slot(ty: Type, word: u32) -> MemInfo {
    let size = if ty.is_ptr() { word } else { ty.bits() * ty.lanes() / 8 };
    MemInfo {
        size: u64::from(size),
        align: size.min(16),
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    /// `layer` of a C file with a `cleanup` variable once the cleanup is inlined: the pad reads
    /// the sum made before the call, and the block the pad goes on to reads the argument.
    const LAYER: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"

func @deep(i32), linkage(external);

func @f(i32) -> i32, linkage(external);

func @_Unwind_Resume(ptr), linkage(external);

func @layer(i32), linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 100
    %2 = add.nsw %0, %1
    call @deep(%0) : (i32)
    %3 = unwound.i1
    br_if %3, block1, block2

block1:
    %4 = landing.ptr
    %5 = call @f(%2) : (i32) -> i32
    %6 = call @f(%1) : (i32) -> i32
    jump block3

block2:
    return

block3:
    %7 = call @f(%0) : (i32) -> i32
    call @_Unwind_Resume(%4) : (ptr)
    unreachable
}
"#;

    /// `layer` of a C file with two `cleanup` variables once both cleanups are inlined: the
    /// second call unwinds into a pad of its own, and so does the first cleanup in the first pad.
    const NESTED: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"

func @deep(i32), linkage(external);

func @f(i32) -> i32, linkage(external);

func @_Unwind_Resume(ptr), linkage(external);

func @layer(i32), linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 2
    %2 = add.nsw %0, %1
    call @deep(%0) : (i32)
    %3 = unwound.i1
    br_if %3, block3, block1

block1:
    %4 = call @f(%0) : (i32) -> i32
    %5 = unwound.i1
    br_if %5, block4, block2

block2:
    return

block3:
    %6 = landing.ptr
    %7 = call @f(%0) : (i32) -> i32
    %8 = unwound.i1
    br_if %8, block4, block5

block4:
    %9 = landing.ptr
    %10 = call @f(%2) : (i32) -> i32
    call @_Unwind_Resume(%9) : (ptr)
    unreachable

block5:
    %11 = call @f(%2) : (i32) -> i32
    call @_Unwind_Resume(%6) : (ptr)
    unreachable
}
"#;

    /// The fixture after the step, as text, once it is checked.
    fn spilled(source: &str) -> String {
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(source, &mut names).expect("the fixture is IR");
        let id = module.funcs().last().expect("the fixture has a function");
        spill(&mut module[id], 8);
        let func = &module[id];
        rucc_ir::verify_func(&module, func, &names).expect("the rewrite builds valid IR");
        rucc_ir::print_func(&module, func, &names)
    }

    /// The names of the values the lines with `what` in them make.
    ///
    /// The printer numbers the values again, so the names are read from the text.
    fn made<'a>(text: &'a str, what: &str) -> Vec<&'a str> {
        text.lines()
            .filter(|line| line.contains(what))
            .filter_map(|line| line.trim().split(' ').next())
            .collect()
    }

    /// What each call to `f` from the first pad on reads.
    fn read_after_pads(text: &str) -> Vec<&str> {
        text[text.find("landing").expect("the pad is there")..]
            .lines()
            .filter_map(|line| line.split("call @f(").nth(1))
            .filter_map(|rest| rest.split(')').next())
            .collect()
    }

    #[test]
    fn a_pad_reads_what_it_needs_from_the_frame() {
        let text = spilled(LAYER);
        // The sum and the argument, each with a slot, a store where it is made and a load after
        // the landing or at the top of the block after the pad. The constant is made again in the
        // pad, so it has none.
        assert_eq!(text.matches("= alloca").count(), 2, "{text}");
        assert_eq!(text.matches("store").count(), 2, "{text}");
        assert_eq!(text.matches("= load").count(), 2, "{text}");
        let entry = &text[..text.find("landing").expect("the pad is there")];
        assert!(entry.rfind("store").unwrap() < entry.find("call @deep").unwrap(), "{text}");
        let constant = made(&text, "iconst")[0];
        let loads = made(&text, "= load");
        let read = read_after_pads(&text);
        assert_eq!(read.len(), 3, "{text}");
        assert_eq!(read.iter().filter(|&&r| r == constant).count(), 1, "{text}");
        assert!(read.iter().all(|r| *r == constant || loads.contains(r)), "{text}");
    }

    /// A pad that a call in another pad unwinds into reads the slot as well, and the first pad
    /// has nothing new to read.
    #[test]
    fn a_pad_that_a_pad_unwinds_into_reads_the_frame_too() {
        let text = spilled(NESTED);
        assert_eq!(text.matches("= alloca").count(), 2, "{text}");
        assert_eq!(text.matches("store").count(), 2, "{text}");
        assert_eq!(text.matches("= load").count(), 3, "{text}");
        let loads = made(&text, "= load");
        let read = read_after_pads(&text);
        assert_eq!(read.len(), 3, "{text}");
        assert!(read.iter().all(|r| loads.contains(r)), "{text}");
    }

    /// A function with no pad is left as it was.
    #[test]
    fn a_function_with_no_pad_is_left_alone() {
        let text = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"

func @f(i32) -> i32, linkage(external);

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @f(%0) : (i32) -> i32
    return %1
}
"#;
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(text, &mut names).expect("the fixture is IR");
        let id = module.funcs().last().expect("the fixture has a function");
        let before = rucc_ir::print_func(&module, &module[id], &names);
        spill(&mut module[id], 8);
        assert_eq!(rucc_ir::print_func(&module, &module[id], &names), before);
    }
}
