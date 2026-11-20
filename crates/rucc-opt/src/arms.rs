//! What both arms of a branch compute first, computed once above the branch.
//!
//! ```c
//! #define cgbase(fs, c) ((daddr_t)(fs)->fs_fpg * (c))
//! #define cgstart(fs, c) \
//!     ((fs)->fs_magic == FS_UFS2_MAGIC ? cgbase(fs, c) : \
//!      (cgbase(fs, c) + (fs)->fs_old_cgoffset * ((c) & ~((fs)->fs_old_cgmask))))
//! ```
//!
//! Both arms of the conditional load `fs_fpg` and `fs_ipg`, divide and multiply, and one of them
//! goes on to add a term. FreeBSD's `boot2` has this in `fsread`, where it is two loads, a
//! division and a multiplication written out twice, and gcc keeps one copy of them with its code
//! hoisting. This is that: an instruction at the top of one arm that the other arm computes too
//! goes into the block that branches, and the other arm reads it from there.
//!
//! # What goes up
//!
//! The branch is a `br_if` to two different blocks, each of which has the branching block as its
//! only predecessor, so each arm is entered from there and nowhere else. An instruction of an arm
//! is one that may go when it is pure, or a load that is not volatile or atomic, and when it comes
//! before the first instruction in its arm that is neither. Everything up to that point runs every
//! time the arm is entered, so an instruction that both arms have there runs every time the branch
//! is reached, and computing it before the branch computes nothing that was not computed already.
//! That is why a division may go up, though it can trap: both arms would have trapped. And a load
//! may go up because nothing in front of it in its arm writes memory, and the branch does not.
//!
//! Two instructions are the same when the opcode, the flags, the extra, the types and the operands
//! are. An operand is the same when it is the same value, when the other arm's is a result that
//! already went up and is read as this one, or when both are the same constant. A constant of the
//! arm that went up is copied above the branch for the instruction that reads it, since the arm
//! keeps its own for whatever else reads it there.
//!
//! The pass runs at `-Os` and `-Oz`, where one copy of the code is what is asked for. It goes up in
//! front of the comparison the branch reads, when the comparison is just in front of the branch,
//! so the comparison and the branch stay one instruction pair on the machine.

use rucc_base::hash::Map;
use rucc_ir::{Block, Def, Extra, Facts, Flags, Func, Inst, InstData, MemOrder, Opcode, Value};

use crate::uses::{chase, substitute};
use crate::{Analyses, Analysis, Fuel, Pass, Phase, Preserved, Stats};

/// The name the pipelines and `-fno-arm-hoist` use.
const NAME: &str = "arm-hoist";

/// An instruction of both arms went up into the block that branches.
const HOISTED: &str = "instruction both arms compute moved above the branch";

/// An arm this long is left as it is, since each instruction of one arm is looked for in the other.
const LONGEST: usize = 512;

/// The pass.
#[derive(Debug)]
pub struct ArmHoist;

impl Pass for ArmHoist {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "what both arms of a branch compute first is computed once above the branch"
    }

    fn phase(&self) -> Phase {
        Phase::Scalar
    }

    fn preserves(&self) -> Preserved {
        // Instructions move between blocks and go, and no edge moves.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let mut diamonds = Vec::new();
        {
            let cfg = an.cfg(func);
            for block in func.blocks() {
                let Some(end) = func.terminator(block) else { continue };
                if func[end].opcode != Opcode::BrIf {
                    continue;
                }
                let arms: Vec<Block> = func.successors(end).map(|call| call.block).collect();
                let [then, other] = arms[..] else { continue };
                if then == other || then == block || other == block {
                    continue;
                }
                if cfg.predecessors(then) != [block] || cfg.predecessors(other) != [block] {
                    continue;
                }
                diamonds.push((block, then, other));
            }
        }
        let mut same: Map<Value, Value> = Map::default();
        let mut gone: Vec<Inst> = Vec::new();
        for (block, then, other) in diamonds {
            if !hoist(func, fuel, &mut stats, &mut same, &mut gone, block, then, other) {
                break;
            }
        }
        for inst in gone {
            func.remove_inst(inst);
        }
        if !same.is_empty() {
            substitute(func, &same);
        }
        stats
    }
}

/// Moves what the two arms of the branch at the end of `block` both compute first above it.
/// False when the fuel ran out.
#[allow(clippy::too_many_arguments)]
fn hoist(
    func: &mut Func,
    fuel: &mut Fuel,
    stats: &mut Stats,
    same: &mut Map<Value, Value>,
    gone: &mut Vec<Inst>,
    block: Block,
    then: Block,
    other: Block,
) -> bool {
    let first = head(func, then);
    let mut second = head(func, other);
    if first.is_empty() || second.is_empty() || first.len() > LONGEST || second.len() > LONGEST {
        return true;
    }
    let Some(end) = func.terminator(block) else { return true };
    // The copies of the arm's constants made above the branch, so that two instructions that read
    // one constant read one copy.
    let mut copies: Map<Value, Value> = Map::default();
    // Once one instruction had to go after the comparison, the ones after it go there too, since
    // they may read it.
    let mut late = false;
    for inst in first {
        if leaf(func, inst) || indexed_name(func, inst) {
            continue;
        }
        let args: Vec<Value> = func[func[inst].args].to_vec();
        if args.iter().any(|&arg| inside(func, arg, then) && !leaf_value(func, arg)) {
            continue;
        }
        let Some(at) = second.iter().position(|&twin| alike(func, same, inst, twin)) else {
            continue;
        };
        if !fuel.take() {
            return false;
        }
        let twin = second.remove(at);
        let (Some(result), Some(dropped)) = (func[inst].first_result, func[twin].first_result)
        else {
            continue;
        };
        let place = if late { end } else { place(func, end, &args) };
        late = place == end;
        func.remove_inst(inst);
        func.insert_before(inst, place);
        let mut copied = false;
        for arg in args {
            if !inside(func, arg, then) {
                continue;
            }
            copied = true;
            if copies.contains_key(&arg) {
                continue;
            }
            let Def::Result { inst: made, .. } = func[arg].def else { continue };
            let data = &func[made];
            let data =
                InstData { flags: data.flags, extra: data.extra, ..InstData::new(data.opcode) };
            let ty = func[arg].ty;
            let span = func.span(made);
            let copy = func.create_inst(data, &[ty], span);
            func.insert_before(copy, inst);
            copies.insert(arg, func[copy].first_result.expect("one result was asked for"));
        }
        if copied {
            let list = func[inst].args;
            func.rewrite(list, |value| copies.get(&value).copied().unwrap_or(value));
        }
        if func.facts(result) != func.facts(dropped) {
            func.set_facts(result, Facts::NONE);
        }
        same.insert(dropped, result);
        gone.push(twin);
        stats.optimized(HOISTED);
    }
    true
}

/// The instructions at the top of an arm that may go up, which are the ones in front of the first
/// that may not.
fn head(func: &Func, arm: Block) -> Vec<Inst> {
    func.insts(arm).take_while(|&inst| !func.is_terminator(inst) && movable(func, inst)).collect()
}

/// Whether an instruction may be computed above the branch when both arms compute it.
fn movable(func: &Func, inst: Inst) -> bool {
    let data = &func[inst];
    if data.results != 1 {
        return false;
    }
    if data.opcode == Opcode::Load {
        let Extra::Mem(mem) = data.extra else { return false };
        return !data.flags.contains(Flags::VOLATILE)
            && func[mem].order == MemOrder::NotAtomic
            && !func.carries_mem(inst);
    }
    !data.opcode.has_effects()
        && data.opcode != Opcode::MemEntry
        && matches!(
            data.extra,
            Extra::None
                | Extra::Imm(_)
                | Extra::Symbol(_)
                | Extra::IntPred(_)
                | Extra::FloatPred(_)
        )
}

/// Whether this is a constant or an address, which reads no operand and is copied rather than
/// moved.
fn leaf(func: &Func, inst: Inst) -> bool {
    matches!(func[inst].opcode, Opcode::IConst | Opcode::FConst | Opcode::GlobalAddr)
        && func[func[inst].args].is_empty()
}

/// Whether this is the address of a name plus an index that is not a constant.
///
/// Such a sum stays in its arm. Each access in the arm puts the name and the index into its own
/// address, as `movl types+8(%eax), %ecx` on i386. If the sum goes above the branch, a register
/// holds it across the branch and each access reads it, which is no fewer bytes and costs a
/// register. The index still goes up when both arms compute it.
fn indexed_name(func: &Func, inst: Inst) -> bool {
    let data = &func[inst];
    if data.opcode != Opcode::PtrAdd {
        return false;
    }
    let args = &func[data.args];
    let name = |value: Value| {
        matches!(func[value].def, Def::Result { inst, .. }
            if func[inst].opcode == Opcode::GlobalAddr && leaf(func, inst))
    };
    args.len() == 2 && name(args[0]) && !leaf_value(func, args[1])
}

/// Whether a value is a result of a [`leaf`].
fn leaf_value(func: &Func, value: Value) -> bool {
    matches!(func[value].def, Def::Result { inst, .. } if leaf(func, inst))
}

/// Whether a value is made in this block, as a result or as a parameter.
fn inside(func: &Func, value: Value, block: Block) -> bool {
    match func[value].def {
        Def::Result { inst, .. } => func.block_of(inst) == Some(block),
        Def::Param { block: of, .. } => of == block,
    }
}

/// Whether two instructions compute the same thing, reading `twin`'s operands through `same`.
fn alike(func: &Func, same: &Map<Value, Value>, inst: Inst, twin: Inst) -> bool {
    let (one, two) = (&func[inst], &func[twin]);
    if one.opcode != two.opcode
        || one.flags != two.flags
        || !extra_alike(func, one.extra, two.extra)
    {
        return false;
    }
    let (Some(a), Some(b)) = (one.first_result, two.first_result) else { return false };
    if func[a].ty != func[b].ty {
        return false;
    }
    let (left, right) = (&func[one.args], &func[two.args]);
    left.len() == right.len()
        && left.iter().zip(right).all(|(&a, &b)| operand_alike(func, a, chase(same, b)))
}

/// Whether two extras say the same thing, comparing what a constant or an access holds rather than
/// where it is stored.
fn extra_alike(func: &Func, one: Extra, two: Extra) -> bool {
    match (one, two) {
        (Extra::Imm(a), Extra::Imm(b)) => func[a].bits() == func[b].bits(),
        (Extra::Mem(a), Extra::Mem(b)) => func[a] == func[b],
        _ => one == two,
    }
}

/// Whether two operands are the same value or the same constant.
fn operand_alike(func: &Func, a: Value, b: Value) -> bool {
    if a == b {
        return true;
    }
    let (Def::Result { inst: one, .. }, Def::Result { inst: two, .. }) = (func[a].def, func[b].def)
    else {
        return false;
    };
    leaf(func, one)
        && leaf(func, two)
        && func[one].opcode == func[two].opcode
        && func[one].flags == func[two].flags
        && func[a].ty == func[b].ty
        && extra_alike(func, func[one].extra, func[two].extra)
}

/// Where in the branching block an instruction reading `args` goes: in front of the comparison the
/// branch reads when that is just in front of the branch and is not read here, and in front of the
/// branch otherwise.
fn place(func: &Func, end: Inst, args: &[Value]) -> Inst {
    let Some(before) = func.prev_inst(end) else { return end };
    let Some(compared) = func[before].first_result else { return end };
    let reads = func[func[end].args].first() == Some(&compared);
    if reads && !args.contains(&compared) { before } else { end }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::ArmHoist;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "i686-unknown-freebsd15"
target datalayout = "e-p:32:32-i64:32-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it.
    fn run(body: &str) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            ArmHoist.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// How many lines of the printed module hold `text`.
    fn count(out: &str, text: &str) -> usize {
        out.lines().filter(|line| line.contains(text)).count()
    }

    /// The part of the printed module from the line holding `from` to the line holding `to`.
    fn between<'a>(out: &'a str, from: &str, to: &str) -> &'a str {
        let start = out.find(from).unwrap_or_else(|| panic!("{from}\n{out}"));
        let end = out[start..].find(to).map_or(out.len(), |at| start + at);
        &out[start..end]
    }

    /// The UFS1 and UFS2 start of a group, as `fsread` has it once the macros are expanded.
    const GROUP: &str = r#"
global @fs : bytes 256, align 4, linkage(external)

func @group(i32, i32) -> i64, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 5
    %3 = icmp eq %1, %2
    br_if %3, block1, block2

block1:
    %4 = global_addr @fs
    %5 = iconst.i32 188
    %6 = ptr_add.nuw %4, %5
    %7 = load.i32 %6, align 4
    %8 = sext.i64 %7
    %9 = zext.i64 %0
    %10 = iconst.i32 184
    %11 = ptr_add.nuw %4, %10
    %12 = load.i32 %11, align 8
    %13 = zext.i64 %12
    %14 = udiv %9, %13
    %15 = mul %8, %14
    jump block3(%15)

block2:
    %16 = global_addr @fs
    %17 = iconst.i32 188
    %18 = ptr_add.nuw %16, %17
    %19 = load.i32 %18, align 4
    %20 = sext.i64 %19
    %21 = zext.i64 %0
    %22 = iconst.i32 184
    %23 = ptr_add.nuw %16, %22
    %24 = load.i32 %23, align 8
    %25 = zext.i64 %24
    %26 = udiv %21, %25
    %27 = mul %20, %26
    %28 = iconst.i32 24
    %29 = ptr_add.nuw %16, %28
    %30 = load.i32 %29, align 8
    %31 = sext.i64 %30
    %32 = and %26, %31
    %33 = mul %31, %32
    %34 = add %27, %33
    jump block3(%34)

block3(%35: i64):
    return %35
}
"#;

    #[test]
    fn the_division_both_arms_make_is_made_once_above_the_branch() {
        let out = run(GROUP);
        assert_eq!(count(&out, "udiv"), 1, "{out}");
        assert_eq!(count(&out, "load.i32"), 3, "{out}");
        let top = between(&out, "block0(", "block1:");
        assert!(top.contains("udiv"), "{out}");
        assert_eq!(count(top, "load.i32"), 2, "{out}");
        // The comparison stays next to the branch.
        let lines: Vec<&str> = top.lines().filter(|line| !line.trim().is_empty()).collect();
        assert!(lines[lines.len() - 2].contains("icmp eq"), "{out}");
        // The load only the second arm makes stays there.
        assert!(between(&out, "block2:", "block3(").contains("load.i32"), "{out}");
    }

    #[test]
    fn nothing_goes_up_past_a_store_in_an_arm() {
        let out = run(r#"
func @f(ptr, i32, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: i32, %2: i32):
    %3 = iconst.i32 0
    %4 = icmp eq %1, %3
    br_if %4, block1, block2

block1:
    store %3 -> %0, align 4
    %5 = udiv %1, %2
    jump block3(%5)

block2:
    %6 = udiv %1, %2
    jump block3(%6)

block3(%7: i32):
    return %7
}
"#);
        assert_eq!(count(&out, "udiv"), 2, "{out}");
    }

    #[test]
    fn an_arm_with_another_way_in_is_left_alone() {
        let out = run(r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 0
    %3 = icmp eq %0, %2
    br_if %3, block1, block2

block1:
    %4 = mul %0, %1
    jump block2

block2:
    %5 = mul %0, %1
    return %5
}
"#);
        assert_eq!(count(&out, "mul"), 2, "{out}");
    }

    #[test]
    fn a_volatile_load_stays_in_its_arm() {
        let out = run(r#"
func @f(ptr, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = iconst.i32 0
    %3 = icmp eq %1, %2
    br_if %3, block1, block2

block1:
    %4 = load.i32.volatile %0, align 4
    jump block3(%4)

block2:
    %5 = load.i32.volatile %0, align 4
    jump block3(%5)

block3(%6: i32):
    return %6
}
"#);
        assert_eq!(count(&out, "load.i32"), 2, "{out}");
    }

    #[test]
    fn the_index_goes_up_and_the_name_plus_the_index_stays_in_each_arm() {
        let out = run(r#"
global @types : bytes 1, align 4, linkage(external)

func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 0
    %3 = icmp ne %1, %2
    br_if %3, block1, block2

block1:
    %4 = iconst.i32 28
    %5 = mul %0, %4
    %6 = global_addr @types
    %7 = ptr_add %6, %5
    %8 = iconst.i32 8
    %9 = ptr_add.nuw %7, %8
    %10 = load.i32 %9, align 4
    jump block3(%10)

block2:
    %11 = iconst.i32 28
    %12 = mul %0, %11
    %13 = global_addr @types
    %14 = ptr_add %13, %12
    %15 = iconst.i32 12
    %16 = ptr_add.nuw %14, %15
    %17 = load.i32 %16, align 4
    jump block3(%17)

block3(%18: i32):
    return %18
}
"#);
        assert_eq!(count(&out, "mul"), 1, "{out}");
        assert!(between(&out, "block0(", "block1:").contains("mul"), "{out}");
        assert_eq!(count(&out, "ptr_add %"), 2, "{out}");
        assert!(!between(&out, "block0(", "block1:").contains("ptr_add"), "{out}");
    }
}
