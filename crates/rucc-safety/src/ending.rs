//! The check in front of a call that ends a storage instance.
//!
//! Design: `spec/safe-memory/08-temporal-safety.md` section 8.3, which is the lock and key rule, and
//! tamnd/rucc#492 is the row it closes.
//!
//! A double free is caught while the block is still free, because the allocator reads the header at
//! the address and finds nothing live there. It is not caught once the same block has been handed
//! back out: the header then says a live instance begins at that address, the free looks like an
//! ordinary one, and what it releases is somebody else's object. That is the worse half of the bug.
//! The program keeps running, the storage is handed out a third time, and the report that eventually
//! comes out names an access in code that did nothing wrong.
//!
//! Nothing the allocator can see tells those two apart, because the address is the same in both and
//! the instance is not. What tells them apart is the version the pointer was made at held against the
//! version the plane holds now, which is the same question `check_live` asks at an access and the
//! same one the runtime has been answering since the capability reached it. So the work here is not
//! a new judgement, it is putting the question where the free is.
//!
//! # Why it is a pass of its own
//!
//! Because it is about a call and [`crate::insert`] is about an access. The walk there is over loads,
//! stores, copies and derivations, and it decides everything from the instruction in front of it.
//! Which function a call names is a symbol, and turning a symbol into a name takes the interner,
//! which that pass is not given and should not be: a pass that resolves names is a pass that has an
//! opinion about the library, and `crate::wrap` and `crate::boundary` are the two that already do
//! and are module level for the same reason.
//!
//! # What it does not do
//!
//! The rest of judgement J6, which is the allocator's and stays there. Whether anything was ever
//! allocated at the address, whether it was this allocator that allocated it, and whether the
//! pointer is the base of an instance or the middle of one are all questions the header answers with
//! more to go on than a capability has. `crate::check::freeing` in the runtime is where the division
//! is written down, and the short version is that this check speaks only about the one case the
//! header cannot see.
//!
//! It also says nothing about a free through a pointer this build could not trace. A capability the
//! compiler recovered from the plane carries whatever the plane said at the moment the boundary lost
//! track, which is a fact about the address rather than about the pointer, so the runtime lets it
//! through. That is the same restraint every other check made with a recovered capability shows, and
//! the boxes of tamnd/rucc#1241 that are still open are what move pointers out of that group.

use std::collections::HashSet;

use rucc_base::Interner;
use rucc_ir::{Extra, Func, FuncId, Inst, InstData, Module, Opcode, Value};

use crate::origin;

/// The functions whose call ends the lifetime of the instance one of their arguments names, and
/// which argument that is.
///
/// Three names and not more. `free` and `realloc` are what `rucc_opt::nofree` already reads the same
/// way, and `reallocf` is the BSD spelling of the second that frees on failure as well, so a program
/// that calls it has handed the block over either way.
///
/// `realloc` is here for the pointer that goes in and says nothing about the one that comes out. The
/// instance the argument named is over once the call returns, whether the call moved the bytes or
/// grew them where they were, and a second `realloc` of the old pointer is the same bug a second
/// `free` of it is.
///
/// Sorted, and a test checks that it is sorted and says each name once.
const ENDS: &[(&str, usize)] = &[("free", 0), ("realloc", 0), ("reallocf", 0)];

/// Puts a `check_free` in front of every call in the module that ends an instance.
///
/// Gives back how many went in, which is what the summary counts.
///
/// A module that defines one of the names itself is that function's own source, and what a function
/// called `free` does in there is whatever it was written to do. `rucc_opt::heap::annotate` declines
/// the same way and for the same reason, and the two should agree: a build where one of them
/// believes the library name and the other does not is a build with an inconsistent idea of what its
/// own allocator is.
pub fn checks(module: &mut Module, names: &Interner) -> usize {
    let defined: HashSet<&str> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| names.resolve(module[id].name))
        .collect();
    let ids: Vec<FuncId> = module.funcs().collect();
    let mut done = 0;
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        done += one(&mut module[id], names, &defined);
    }
    done
}

/// The same for one function.
///
/// The calls are found before anything is put in, because taking a capability may put a `cap_of`
/// behind the instruction that made the pointer and that is a block this walk would otherwise be
/// part way through.
///
/// The table of what is already there is taken first and for the reason [`crate::handover`] takes
/// one first: [`origin::existing`] reports the capabilities a function is paying for anyway, and a
/// `check_free` is a reader, so asking afterwards would report capabilities that are only alive
/// because this pass put a check in front of a free. Seeding [`origin::Origins`] with it is what
/// makes a free of a pointer some access already asked about read that access's answer rather than a
/// second walk of the plane over the same object.
fn one(func: &mut Func, names: &Interner, defined: &HashSet<&str>) -> usize {
    let ending: Vec<(Inst, Value)> = func
        .blocks()
        .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
        .filter_map(|inst| handed(func, names, defined, inst).map(|value| (inst, value)))
        .collect();
    if ending.is_empty() {
        return 0;
    }
    let mut origins = origin::Origins::new();
    for (pointer, cap) in origin::existing(func) {
        origins.seed(pointer, cap);
    }
    let mut done = 0;
    for (inst, pointer) in ending {
        let span = func.span(inst);
        let capability = origins.of(func, pointer, inst);
        let args = func.push_values(&[capability, pointer]);
        let data = InstData { args, ..InstData::new(Opcode::CheckFree) };
        let made = func.create_inst(data, &[], span);
        func.insert_before(made, inst);
        done += 1;
    }
    // A free of a pointer that came round a loop asks for the capability at a block parameter,
    // which is a join the edges into it still have to be handed their half of.
    origins.join(func);
    done
}

/// The pointer a call is handing over, or nothing when the call is not one of these.
///
/// A direct call and no other spelling. An indirect one names no function, so there is nothing to
/// look up, and a tail call is not a shape this pass meets: it is the optimizer that makes one and
/// this runs before the optimizer. A tail call the optimizer makes out of a call that has already
/// been given a check keeps the check, since the check is the instruction in front of it.
fn handed(func: &Func, names: &Interner, defined: &HashSet<&str>, inst: Inst) -> Option<Value> {
    if func[inst].opcode != Opcode::Call {
        return None;
    }
    let Extra::Call(at) = func[inst].extra else { return None };
    let name = names.resolve(func[at].callee?);
    if defined.contains(name) {
        return None;
    }
    let (_, which) = ENDS.iter().find(|&&(each, _)| each == name)?;
    // The arguments of a direct call stand for its parameters one for one, there being no operand
    // for the callee, so the index the table holds is an index into these.
    let &value = func[func[inst].args].get(*which)?;
    func[value].ty.is_ptr().then_some(value)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Builder, CallInfo, Extra, Func, FuncId, InstData, IntPred, Module, Opcode, Signature, Type,
        verify_func,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{ENDS, checks};

    /// A module with nothing in it, which is what every test below starts from.
    fn unit(names: &mut Interner) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        Module::new(names.intern("a.c"), &target)
    }

    /// A declaration of `name` taking one pointer and giving back whatever `returns` says.
    fn declares(names: &mut Interner, module: &mut Module, name: &str, returns: bool) -> FuncId {
        let mut signature = Signature::new().with_params(&[Type::PTR]);
        if returns {
            signature = signature.with_returns(&[Type::PTR]);
        }
        module.add_func(Func::new(names.intern(name), signature))
    }

    /// A function called `caller` that takes a pointer and passes it to `callee`.
    fn passes_it_to(names: &mut Interner, module: &mut Module, callee: &str, returns: bool) {
        declares(names, module, callee, returns);
        calls(names, module, callee, returns);
    }

    /// The caller on its own, for the one test where the module has the callee's body rather than a
    /// declaration of it and so cannot have both.
    fn calls(names: &mut Interner, module: &mut Module, callee: &str, returns: bool) {
        let mut signature = Signature::new().with_params(&[Type::PTR]);
        signature = signature.with_returns(&[]);
        let mut func = Func::new(names.intern("caller"), signature);
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let mut called = Signature::new().with_params(&[Type::PTR]);
        if returns {
            called = called.with_returns(&[Type::PTR]);
        }
        let sig = func.add_signature(called);
        let varargs = func.push_abis(&[]);
        let info =
            func.add_call(CallInfo { callee: Some(names.intern(callee)), signature: sig, varargs });
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
        if returns {
            b.value(data, Type::PTR);
        } else {
            b.inst(data, &[]);
        }
        b.ret(&[]);
        module.add_func(func);
    }

    /// How many `check_free` the module holds.
    fn counted(module: &Module) -> usize {
        module
            .funcs()
            .filter(|&id| !module[id].is_declaration())
            .map(|id| &module[id])
            .flat_map(|func| {
                func.blocks()
                    .flat_map(|block| func.insts(block).collect::<Vec<_>>())
                    .filter(|&inst| func[inst].opcode == Opcode::CheckFree)
                    .collect::<Vec<_>>()
            })
            .count()
    }

    #[test]
    fn the_table_is_sorted_and_says_each_name_once() {
        for pair in ENDS.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} then {}", pair[0].0, pair[1].0);
        }
    }

    #[test]
    fn a_free_gets_a_check_in_front_of_it() {
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        passes_it_to(&mut names, &mut module, "free", false);
        assert_eq!(checks(&mut module, &names), 1);
        assert_eq!(counted(&module), 1);
    }

    #[test]
    fn a_realloc_gets_one_too_because_the_pointer_going_in_is_over() {
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        passes_it_to(&mut names, &mut module, "realloc", true);
        assert_eq!(checks(&mut module, &names), 1);
    }

    #[test]
    fn a_call_of_something_else_gets_nothing() {
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        passes_it_to(&mut names, &mut module, "puts", false);
        assert_eq!(checks(&mut module, &names), 0);
    }

    #[test]
    fn a_module_that_writes_its_own_free_keeps_it() {
        // The allocator's own source, where a function called `free` is whatever it was written to
        // be. Believing the name there would put a check in front of a call the library makes to
        // itself and refuse a program for the allocator doing its job.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        let mut own = Func::new(names.intern("free"), Signature::new().with_params(&[Type::PTR]));
        let entry = own.create_block();
        own.append_param(entry, Type::PTR);
        let mut b = Builder::new(&mut own, entry);
        b.ret(&[]);
        module.add_func(own);
        calls(&mut names, &mut module, "free", false);
        assert_eq!(checks(&mut module, &names), 0);
    }

    #[test]
    fn the_check_reads_the_capability_the_pointer_already_had() {
        // The point of seeding the table from what is standing. A free of a pointer some access
        // already asked about is not a second walk of the lifetime plane, it is the same answer
        // read a second time, so the function comes out with one producer rather than two.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        passes_it_to(&mut names, &mut module, "free", false);
        assert_eq!(checks(&mut module, &names), 1);
        let id = module
            .funcs()
            .find(|&id| !module[id].is_declaration())
            .expect("the module defines one function");
        let func = &module[id];
        let made: Vec<_> = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CapOf)
            .collect();
        assert_eq!(made.len(), 1, "one producer for the one pointer");
        let check = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .find(|&inst| func[inst].opcode == Opcode::CheckFree)
            .expect("the check went in");
        let cap = func[made[0]].results().next().expect("a cap_of produces one value");
        assert_eq!(func[func[check].args].first(), Some(&cap));
    }

    #[test]
    fn a_free_of_a_pointer_two_ways_in_hands_each_edge_its_capability() {
        // `free(x)` where x is a block parameter fed a different object down each edge, so the
        // capability is a join and each edge has to pass the one for the pointer it passes. A join
        // nobody finished leaves the block taking an argument its branches do not give it.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        declares(&mut names, &mut module, "free", false);
        let i64_ = Type::int(64);
        let signature = Signature::new().with_params(&[Type::PTR, Type::PTR, i64_]);
        let mut func = Func::new(names.intern("caller"), signature);
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let q = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, i64_);
        let (left, right) = (func.create_block(), func.create_block());
        let join = func.create_block();
        let x = func.append_param(join, Type::PTR);
        let mut b = Builder::new(&mut func, entry);
        let zero = b.iconst(i64_, 0);
        let taken = b.icmp(IntPred::Slt, n, zero);
        b.br_if(taken, left, &[], right, &[]);
        Builder::new(&mut func, left).jump(join, &[p]);
        Builder::new(&mut func, right).jump(join, &[q]);
        let sig = func.add_signature(Signature::new().with_params(&[Type::PTR]));
        let varargs = func.push_abis(&[]);
        let info =
            func.add_call(CallInfo { callee: Some(names.intern("free")), signature: sig, varargs });
        let mut b = Builder::new(&mut func, join);
        let args = b.func().push_values(&[x]);
        b.inst(InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) }, &[]);
        b.ret(&[]);
        module.add_func(func);

        assert_eq!(checks(&mut module, &names), 1);
        let id = module
            .funcs()
            .find(|&id| !module[id].is_declaration())
            .expect("the module defines one function");
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }
}
