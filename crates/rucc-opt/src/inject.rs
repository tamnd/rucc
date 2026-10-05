//! A wrong answer put into a function on purpose, so that a tool which hunts for one has one to
//! find.
//!
//! `rk mixed` in rucc-kernel takes a test the rucc kernel fails and narrows it to an object, then
//! to one rewrite with `-fpass-fuel-global`, then to the pass and the function that rewrite was
//! in. Nothing proves that whole road works short of a failure to walk it on, and a real
//! miscompile is not there when it is wanted. This pass is that failure. No level runs it, and
//! `-fenable-inject-fault=<function>` turns it on for the functions named, so a kernel built with
//! that flag fails exactly the tests that reach them.
//!
//! The fault is the lowest bit of every integer the function stores, flipped. A function that
//! fills in a structure, which is most of what a kernel tests, then fills it in wrong without
//! going anywhere it should not, so the test that checks it fails and the boot around it does
//! not. Each flip takes fuel, which is what lets the search land on one store.

use rucc_ir::{Func, Inst, InstData, Opcode, Value};

use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// What `-fenable-inject-fault=<function>` turns on.
#[derive(Debug, Clone, Copy)]
pub struct InjectFault;

const FLIPPED: &str = "the low bit of a stored integer flipped on purpose";
const NO_FUEL: &str = "a store left as it was, the pass ran out of fuel";

impl Pass for InjectFault {
    fn name(&self) -> &'static str {
        "inject-fault"
    }

    fn describe(&self) -> &'static str {
        "the low bit of every integer the function stores is flipped, to test a bisection"
    }

    fn preserves(&self) -> Preserved {
        // Two new instructions ahead of each store and no edge moved.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let stores: Vec<Inst> = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == Opcode::Store)
            .collect();
        for store in stores {
            let Some(&value) = func[func[store].args].first() else { continue };
            let ty = func[value].ty;
            if !ty.is_int() || ty.is_vector() {
                continue;
            }
            if !fuel.take() {
                stats.missed(NO_FUEL);
                break;
            }
            let one = constant(func, store, value);
            let args = func.push_values(&[value, one]);
            let flipped = emit(func, store, InstData { args, ..InstData::new(Opcode::Xor) });
            // The address is a pointer, so the integer is only ever the value stored.
            let operands = func[store].args;
            func.rewrite(operands, |operand| if operand == value { flipped } else { operand });
            stats.optimized(FLIPPED);
        }
        stats
    }
}

/// A one of the stored value's type, put ahead of the store.
fn constant(func: &mut Func, before: Inst, like: Value) -> Value {
    let ty = func[like].ty;
    let imm = func.add_imm(rucc_ir::Imm::int(1, ty.lane()));
    let data = InstData { extra: rucc_ir::Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

/// An instruction with the type of its first operand, put ahead of `before`.
fn emit(func: &mut Func, before: Inst, data: InstData) -> Value {
    let first = func[data.args].first().copied().expect("an operand");
    let ty = func[first].ty;
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rucc_base::Interner;
    use rucc_ir::{Module, parse, print, verify_func};

    use super::*;
    use crate::outside::Outside;

    const TEXT: &str = "\
; ModuleID = 'inject.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

func @f(ptr, i32, i64), linkage(external) {
block0(%0: ptr, %1: i32, %2: i64):
    store %1 -> %0, align 4
    store %2 -> %0, align 8
    store %0 -> %0, align 8
    return
}
";

    fn with(fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let mut module: Module = parse(TEXT, &mut names).expect("the text parses");
        let id = module.funcs().last().expect("one function");
        let outside = Arc::new(Outside::of(&module));
        let mut an = crate::machine::fixtures::analyses().about(outside);
        InjectFault.run(&mut module[id], &mut an, fuel);
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("{errors:#?}\n{}", print(&module, &names));
        }
        print(&module, &names)
    }

    #[test]
    fn every_integer_store_has_its_low_bit_flipped_and_a_pointer_is_left_alone() {
        let text = with(&mut Fuel::unlimited());
        assert_eq!(text.matches("xor ").count(), 2, "{text}");
        assert!(text.contains("store %0 -> %0, align 8"), "{text}");
        assert!(!text.contains("store %1 -> %0"), "{text}");
        assert!(!text.contains("store %2 -> %0"), "{text}");
    }

    #[test]
    fn each_flip_takes_fuel() {
        let text = with(&mut Fuel::of(1));
        assert_eq!(text.matches("xor ").count(), 1, "{text}");
        assert!(text.contains("store %2 -> %0, align 8"), "{text}");
    }
}
