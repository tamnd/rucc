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
//! So each such value is stored to a slot of its own in front of each call that unwinds into the
//! pad, and the pad loads it back after its `landing`. A value read in a block that only the pad
//! leads to is the same, since that block is reached from the pad alone. This is what gcc does with
//! a value that is live into a landing pad and in no register a call keeps.

use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, Def, Extra, Func, Inst, InstData, MemInfo, MemOrder, Opcode, Restrict, Type, Value,
};

/// Puts each value a landing pad reads, and cannot make again, in a slot of the frame.
///
/// `word` is the size of an address in bytes.
pub fn spill(func: &mut Func, word: u32) {
    let pads: Vec<Block> = func
        .blocks()
        .filter(|&block| {
            func.insts(block).next().is_some_and(|first| func[first].opcode == Opcode::Landing)
        })
        .collect();
    for pad in pads {
        spill_pad(func, pad, word);
    }
}

/// The work of [`spill`] for one pad.
fn spill_pad(func: &mut Func, pad: Block, word: u32) {
    let calls = calls_into(func, pad);
    if calls.is_empty() {
        return;
    }
    let region = region(func, pad);
    let mut wanted: Vec<Value> = Vec::new();
    for &block in &region {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            let mut read: Vec<Value> = func[func[inst].args].to_vec();
            for call in func.successors(inst).collect::<Vec<_>>() {
                read.extend_from_slice(&func[call.args]);
            }
            for value in read {
                if !wanted.contains(&value) && outside(func, value, &region, &calls) {
                    wanted.push(value);
                }
            }
        }
    }
    if wanted.is_empty() {
        return;
    }
    let entry = func.entry().expect("a function with a pad has a body");
    let top = func.insts(entry).next().expect("the entry block has a terminator");
    let landing = func.insts(pad).next().expect("a pad starts with its landing");
    let mut after = landing;
    let mut loaded = Vec::with_capacity(wanted.len());
    for &value in &wanted {
        let ty = func[value].ty;
        let info = slot(ty, word);
        let span = func.span(landing);
        let extra = Extra::Mem(func.add_mem(info));
        let made = func.create_inst(
            InstData { extra, ..InstData::new(Opcode::Alloca) },
            &[Type::PTR],
            span,
        );
        func.insert_before(made, top);
        let address = func[made].first_result.expect("an alloca has a result");
        for &call in &calls {
            let span = func.span(call);
            let extra = Extra::Mem(func.add_mem(info));
            let args = func.push_values(&[value, address]);
            let store = func.create_inst(
                InstData { args, extra, ..InstData::new(Opcode::Store) },
                &[],
                span,
            );
            func.insert_before(store, call);
        }
        let extra = Extra::Mem(func.add_mem(info));
        let args = func.push_values(&[address]);
        let load =
            func.create_inst(InstData { args, extra, ..InstData::new(Opcode::Load) }, &[ty], span);
        func.insert_after(load, after);
        after = load;
        loaded.push(func[load].first_result.expect("a load has a result"));
    }
    let with =
        |value: Value| wanted.iter().position(|&w| w == value).map_or(value, |at| loaded[at]);
    for &block in &region {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            let args = func[inst].args;
            func.rewrite(args, with);
            for call in func.successors(inst).collect::<Vec<_>>() {
                func.rewrite(call.args, with);
            }
        }
    }
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

/// The pad and every block that only the pad leads to.
fn region(func: &Func, pad: Block) -> Vec<Block> {
    let mut preds: Map<Block, Vec<Block>> = Map::default();
    for block in func.blocks() {
        let Some(term) = func.terminator(block) else { continue };
        for call in func.successors(term) {
            preds.entry(call.block).or_default().push(block);
        }
    }
    let mut region = vec![pad];
    let mut inside: Set<Block> = Set::default();
    inside.insert(pad);
    let mut at = 0;
    while at < region.len() {
        let block = region[at];
        at += 1;
        let Some(term) = func.terminator(block) else { continue };
        for call in func.successors(term).collect::<Vec<_>>() {
            let next = call.block;
            if inside.contains(&next) {
                continue;
            }
            let only = preds.get(&next).is_some_and(|from| from.iter().all(|b| inside.contains(b)));
            if only {
                inside.insert(next);
                region.push(next);
            }
        }
    }
    region
}

/// Whether the pad reads this value from before the call, and the lowering cannot make it again
/// inside the pad, and a slot can hold it.
fn outside(func: &Func, value: Value, region: &[Block], calls: &[Inst]) -> bool {
    let block = match func[value].def {
        Def::Result { inst, .. } => {
            match func[inst].opcode {
                Opcode::IConst | Opcode::GlobalAddr | Opcode::Unwound => return false,
                Opcode::Alloca if func[func[inst].args].is_empty() => return false,
                _ => {}
            }
            if calls.contains(&inst) {
                return false;
            }
            func.block_of(inst)
        }
        Def::Param { block, .. } => Some(block),
    };
    block.is_some_and(|block| !region.contains(&block)) && fits(func[value].ty)
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

    #[test]
    fn a_pad_reads_what_it_needs_from_the_frame() {
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(LAYER, &mut names).expect("the fixture is IR");
        let id = module.funcs().last().expect("the fixture has a function");
        spill(&mut module[id], 8);
        let func = &module[id];
        rucc_ir::verify_func(&module, func, &names).expect("the rewrite builds valid IR");
        let text = rucc_ir::print_func(&module, func, &names);
        // The sum and the argument, each with a slot, a store in front of the call and a load
        // after the landing. The constant is made again in the pad, so it has none.
        assert_eq!(text.matches("= alloca").count(), 2, "{text}");
        assert_eq!(text.matches("store").count(), 2, "{text}");
        assert_eq!(text.matches("= load").count(), 2, "{text}");
        let pad = &text[text.find("landing").expect("the pad is there")..];
        let entry = &text[..text.find("landing").expect("the pad is there")];
        assert!(entry.find("store").unwrap() < entry.find("call @deep").unwrap(), "{text}");
        // The printer numbers the values again, so the names are read from the text. Each call
        // in the pad and after it reads a load, except the one that reads the constant.
        let name = |line: &str| line.trim().split(' ').next().unwrap_or_default().to_owned();
        let constant = text.lines().find(|l| l.contains("iconst")).map(name).unwrap();
        let loads: Vec<String> = text.lines().filter(|l| l.contains("= load")).map(name).collect();
        let read: Vec<&str> = pad
            .lines()
            .filter_map(|l| l.split("call @f(").nth(1))
            .map(|rest| rest.split(')').next().unwrap_or_default())
            .collect();
        assert_eq!(read.len(), 3, "{text}");
        assert_eq!(read.iter().filter(|&&r| r == constant).count(), 1, "{text}");
        assert!(read.iter().all(|r| *r == constant || loads.iter().any(|l| l == r)), "{text}");
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
