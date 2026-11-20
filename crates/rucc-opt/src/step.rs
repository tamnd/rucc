//! A step of a pointer or a count, put after the last read of the value it steps.
//!
//! ```c
//! void copy(char *d, const char *s, int n) {
//!     while (n--)
//!         *d++ = *s++;
//! }
//! ```
//!
//! The front end works out `d + 1` and `s + 1` where the `++` is written, which is in front of the
//! load and the store that read the old `d` and `s`. So in the loop body the old pointer and the
//! new one are both live from the step to the access, and the allocator cannot give the two one
//! register. Each new pointer then goes to the loop parameter through a copy on the back edge, and
//! FreeBSD's `boot2` had three copies and a jump on the back edge of its `memcpy` where gcc has
//! none. With the step after the access the old value is dead where the new one is made, the two
//! share a register, the step is one `inc`, and the back edge has nothing to copy.
//!
//! # What moves
//!
//! An `add`, a `sub` or a `ptr_add` of one value and a constant. It goes down in its block to just
//! after the last instruction that reads the value it steps, when nothing between reads the step
//! itself. The value then dies where the step is made, which is the whole point, so a move that
//! leaves a reader of the old value after the step is not made.
//!
//! The step reads no memory and cannot trap, so it can go past a load, a store or a call. It does
//! not go past a comparison the block's branch reads, because the branch and its comparison are one
//! instruction pair on the machine, and a step between them is a step that has to keep the flags.
//! A branch that reads the old value keeps it live to the end of the block anyway, so a reader in
//! the terminator is not a reason to move.
//!
//! The pass runs at `-Os` and `-Oz`. The step was in front of the access at `-O2` as well, and the
//! copies are there too, but code at `-O2` is laid out by passes that weigh other things, and this
//! is the change that `boot2` needed.

use rucc_ir::{Block, Def, Func, Inst, Opcode, Value};

use crate::{Analyses, Analysis, Fuel, Pass, Phase, Preserved, Stats};

/// The name the pipelines and `-fno-step-sink` use.
const NAME: &str = "step-sink";

/// A step went down to the last read of the value it steps.
const MOVED: &str = "step moved after the last read of the value it steps";

/// A block this long is left as it is, since each step looks at the rest of the block.
const LONGEST: usize = 4096;

/// The pass.
#[derive(Debug)]
pub struct StepSink;

impl Pass for StepSink {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a step of a value goes after the last read of the value, so the two share a register"
    }

    fn phase(&self) -> Phase {
        Phase::Scalar
    }

    fn preserves(&self) -> Preserved {
        // Instructions move inside their blocks and no edge moves.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        for block in func.blocks().collect::<Vec<Block>>() {
            let mut order: Vec<Inst> = func.insts(block).collect();
            if order.len() > LONGEST {
                continue;
            }
            let steps: Vec<Inst> =
                order.iter().copied().filter(|&inst| step(func, inst).is_some()).collect();
            for inst in steps {
                let Some(stepped) = step(func, inst) else { continue };
                let Some(at) = order.iter().position(|&other| other == inst) else { continue };
                let Some(after) = destination(func, &order, at, stepped) else { continue };
                if !fuel.take() {
                    break;
                }
                func.remove_inst(inst);
                func.insert_after(inst, order[after]);
                order.remove(at);
                order.insert(after, inst);
                stats.optimized(MOVED);
            }
        }
        stats
    }
}

/// The value a step steps, when this instruction is one: an `add`, a `sub` or a `ptr_add` of a
/// value and a constant, with one result.
fn step(func: &Func, inst: Inst) -> Option<Value> {
    if !matches!(func[inst].opcode, Opcode::Add | Opcode::Sub | Opcode::PtrAdd) {
        return None;
    }
    let args = &func[func[inst].args];
    let [base, by] = args else { return None };
    (!constant(func, *base) && constant(func, *by)).then_some(*base)
}

/// Whether a value is an integer constant.
fn constant(func: &Func, value: Value) -> bool {
    match func[value].def {
        Def::Result { inst, .. } => func[inst].opcode == Opcode::IConst,
        Def::Param { .. } => false,
    }
}

/// Where in `order` the step at `at` should go after, if anywhere: the last instruction that reads
/// `stepped`, when no instruction up to it reads the step and it is not a comparison the branch
/// reads.
fn destination(func: &Func, order: &[Inst], at: usize, stepped: Value) -> Option<usize> {
    let result = func[order[at]].results().next()?;
    let mut last = None;
    for (index, &other) in order.iter().enumerate().skip(at + 1) {
        if func.is_terminator(other) {
            break;
        }
        let args = &func[func[other].args];
        if args.contains(&result) {
            break;
        }
        if args.contains(&stepped) {
            last = Some(index);
        }
    }
    let last = last?;
    // Every read of the old value has to be before the step for it to die there, and one in the
    // instructions after the last that was looked at is one the step could not pass.
    let rest = order.iter().skip(last + 1).filter(|&&other| !func.is_terminator(other));
    if rest.clone().any(|&other| func[func[other].args].contains(&stepped)) {
        return None;
    }
    if branched(func, order, order[last]) {
        return None;
    }
    Some(last)
}

/// Whether this is a comparison the block's terminator reads.
fn branched(func: &Func, order: &[Inst], inst: Inst) -> bool {
    if func[inst].opcode != Opcode::ICmp {
        return false;
    }
    let Some(compared) = func[inst].results().next() else { return false };
    let Some(&end) = order.last() else { return false };
    func.is_terminator(end) && func[func[end].args].contains(&compared)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::StepSink;
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
            StepSink.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// Where in the printed function the first line that holds `text` is. The printer numbers the
    /// values in the order they are laid out, so a line is found by what it reads, not by its name.
    fn line(out: &str, text: &str) -> usize {
        out.lines().position(|line| line.contains(text)).unwrap_or_else(|| panic!("{text}\n{out}"))
    }

    /// The loop of `while (n--) *d++ = *s++;` as the front end leaves it at `-Os`.
    const COPY: &str = r#"
func @copy(ptr, ptr, i32), linkage(external) {
block0(%0: ptr, %1: ptr, %2: i32):
    %3 = iconst.i32 1
    %4 = sub.nsw %2, %3
    %5 = iconst.i32 0
    %6 = icmp ne %2, %5
    br_if %6, block3, block2

block1(%7: ptr, %8: ptr, %9: i32):
    %10 = iconst.i32 1
    %11 = ptr_add.nuw %7, %10
    %12 = ptr_add.nuw %8, %10
    %13 = load.i8 %8, align 1
    store %13 -> %7, align 1
    %14 = sub.nsw %9, %10
    %15 = iconst.i32 0
    %16 = icmp ne %9, %15
    br_if %16, block4, block2

block2:
    return

block3:
    jump block1(%0, %1, %4)

block4:
    jump block1(%11, %12, %14)
}
"#;

    /// Both pointers step after the access that reads them, so each dies where it is stepped.
    #[test]
    fn the_steps_of_a_copy_loop_go_after_the_load_and_the_store() {
        let out = run(COPY);
        assert!(line(&out, "ptr_add.nuw %8,") > line(&out, "load.i8 %8,"), "{out}");
        assert!(line(&out, "ptr_add.nuw %7,") > line(&out, "-> %7,"), "{out}");
    }

    /// The count is read by the comparison the branch reads, and the step does not go between
    /// the two.
    #[test]
    fn a_step_does_not_go_between_a_branch_and_its_comparison() {
        let out = run(COPY);
        assert!(line(&out, "sub.nsw %9,") < line(&out, "icmp ne %9,"), "{out}");
    }

    /// A step that something reads before the last read of the old value stays where it is,
    /// since the two values are live together either way.
    #[test]
    fn a_step_read_before_the_old_value_is_done_with_stays() {
        let out = run(r#"
func @f(ptr, ptr) -> ptr, linkage(external) {
block0(%0: ptr, %1: ptr):
    %2 = iconst.i32 4
    %3 = ptr_add %0, %2
    store %0 -> %3, align 4
    %4 = load.i32 %0, align 4
    store %4 -> %1, align 4
    return %3
}
"#);
        assert!(line(&out, "ptr_add %0,") < line(&out, "store %0 ->"), "{out}");
    }

    /// A step past a call is a value no longer held across it.
    #[test]
    fn a_step_goes_past_a_call_that_reads_the_old_value() {
        let out = run(r#"
func @g(i32), linkage(external);

func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = add %0, %1
    call @g(%0) : (i32)
    return %2
}
"#);
        assert!(line(&out, "add %0,") > line(&out, "call @g"), "{out}");
    }
}
