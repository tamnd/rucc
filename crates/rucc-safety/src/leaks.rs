//! Row T9: what a unit built with `-fsafety-leaks` tells the runtime.
//!
//! Design: `spec/safe-memory/08-temporal-safety.md` section 8.7, and `rucc_safe_rt::leak` for the
//! sweep itself, which is where nearly all of this lives. A leak is not an access that went wrong,
//! so no check goes anywhere. What the compiler adds is two things the runtime cannot find out on
//! its own.
//!
//! The first is that a sweep was asked for at all. The unit puts a pointer to `__rucc_leaks_watch`
//! in the section the C library runs constructors from, so the program arms the sweep before `main`
//! is entered. That works whoever compiled `main`, which matters because the program `main` is in
//! is quite often not the one the flag was given to: a test harness, or Juliet's driver, built by
//! some other compiler and linked against code built by this one.
//!
//! The second is where the stack ends when the program is leaving. The sweep runs at exit, and
//! whether the stack is a root then depends on how the program got there. A program that returned
//! from `main` has no frames of its own left, and the bytes those frames held are still there under
//! the C library's, so scanning them would find the pointers of functions that are long gone and
//! call their allocations reachable. A program that called `exit` from three calls deep still has
//! those three frames, and what they hold is live. So every call to `exit` gets a call to
//! `__rucc_leaks_exiting` in front of it, which tells the runtime that the stack from there up is
//! still the program's, and a sweep that was never told that leaves the stack out. That is the same
//! line Valgrind draws, which scans nothing below the stack pointer and so nothing of a frame that
//! has returned.

use rucc_base::Interner;
use rucc_base::hash::Set;
use rucc_ir::{
    Datum, Extra, Func, FuncId, Global, Inst, Linkage, Module, Opcode, Reloc, Signature,
};

use crate::lower::calling;

/// What the runtime calls to arm the sweep, which takes nothing and gives nothing back.
pub const WATCH: &str = "__rucc_leaks_watch";

/// What the runtime calls to be told the program is leaving through `exit`.
pub const EXITING: &str = "__rucc_leaks_exiting";

/// Puts both halves of the module comment into the module, and gives back how many calls to `exit`
/// were marked.
///
/// `section` is where the C library looks for constructors, which is the driver's to say since it
/// depends on the object format. A format with nowhere to put one gets no sweep, and the calls to
/// `exit` are marked anyway, since they cost one call each on a path the program only takes once.
pub fn watch(module: &mut Module, names: &mut Interner, section: Option<&str>) -> usize {
    if let Some(section) = section {
        armed(module, names, section);
    }
    // A unit that defines `exit` is that function's own source, and calls inside it are not a
    // program leaving. `crate::ending` declines the same way.
    let defined: Set<String> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| names.resolve(module[id].name).to_owned())
        .collect();
    if defined.contains("exit") {
        return 0;
    }
    let ids: Vec<FuncId> = module.funcs().collect();
    let mut marked = 0;
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        marked += one(&mut module[id], names);
    }
    marked
}

/// The pointer to `__rucc_leaks_watch` in the constructor section.
///
/// Internal, so every unit built with the flag can have its own without the linker minding, and the
/// runtime arms the sweep once however many of them run.
fn armed(module: &mut Module, names: &mut Interner, section: &str) {
    let watch = names.intern(WATCH);
    if module.lookup(watch).is_none() {
        module.add_func(Func::new(watch, Signature::new()));
    }
    let pointer = module.datalayout.pointer_bits / 8;
    let reloc = module.add_reloc(Reloc { symbol: watch, addend: 0, size: pointer });
    let mut global = Global::new(names.intern("__rucc_leaks.watch"), u64::from(pointer), pointer);
    global.linkage = Linkage::Internal;
    global.section = Some(names.intern(section));
    global.init = Some(module.push_data(&[Datum::Addr(reloc)]));
    module.add_global(global);
}

/// Marks every call to `exit` in one function.
fn one(func: &mut Func, names: &mut Interner) -> usize {
    let leaving: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| exits(func, names, inst))
        .collect();
    for &inst in &leaving {
        let span = func.span(inst);
        let data = calling(func, names, EXITING, &[], &[], &[]);
        let made = func.create_inst(data, &[], span);
        func.insert_before(made, inst);
    }
    leaving.len()
}

/// Whether `inst` is a direct call to `exit`.
///
/// A direct call and no other spelling, as `crate::ending` has it. A call through a pointer that
/// happens to hold `exit` is a program leaving from a frame the sweep then leaves out, which costs
/// a report about something only that frame held and nothing worse.
fn exits(func: &Func, names: &Interner, inst: Inst) -> bool {
    if func[inst].opcode != Opcode::Call {
        return false;
    }
    let Extra::Call(at) = func[inst].extra else { return false };
    func[at].callee.is_some_and(|callee| names.resolve(callee) == "exit")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Builder, CallInfo, Datum, Extra, Func, InstData, Module, Opcode, Signature, Type,
        print_func, verify_func,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{EXITING, WATCH, watch};

    /// A module with nothing in it.
    fn unit(names: &mut Interner) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        Module::new(names.intern("a.c"), &target)
    }

    /// A function called `name` that calls `callee` with an `int` and returns.
    fn calls(names: &mut Interner, module: &mut Module, name: &str, callee: &str) {
        let mut func = Func::new(names.intern(name), Signature::new());
        let entry = func.create_block();
        let sig = func.add_signature(Signature::new().with_params(&[Type::int(32)]));
        let varargs = func.push_abis(&[]);
        let info =
            func.add_call(CallInfo { callee: Some(names.intern(callee)), signature: sig, varargs });
        let mut b = Builder::new(&mut func, entry);
        let status = b.iconst(Type::int(32), 1);
        let args = b.func().push_values(&[status]);
        b.inst(InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) }, &[]);
        b.ret(&[]);
        module.add_func(func);
    }

    #[test]
    fn a_call_to_exit_is_marked_and_a_call_to_anything_else_is_not() {
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        calls(&mut names, &mut module, "leaving", "exit");
        calls(&mut names, &mut module, "staying", "abs");
        assert_eq!(watch(&mut module, &mut names, None), 1);
        for id in module.funcs() {
            let func = &module[id];
            verify_func(&module, func, &names).expect("still valid");
            let printed = print_func(&module, func, &names);
            let name = names.resolve(func.name);
            let marked = printed.find(EXITING);
            match name {
                "leaving" => {
                    let at = marked.expect("the call to exit was marked");
                    assert!(at < printed.find("exit(").expect("still calls exit"), "{printed}");
                }
                _ => assert!(marked.is_none(), "{printed}"),
            }
        }
    }

    #[test]
    fn a_unit_that_defines_exit_marks_nothing() {
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        calls(&mut names, &mut module, "exit", "_exit");
        calls(&mut names, &mut module, "leaving", "exit");
        assert_eq!(watch(&mut module, &mut names, None), 0);
    }

    #[test]
    fn the_sweep_is_armed_by_a_pointer_in_the_constructor_section() {
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        watch(&mut module, &mut names, Some(".init_array"));
        let ids: Vec<_> = module.globals().collect();
        assert_eq!(ids.len(), 1);
        let global = &module[ids[0]];
        assert_eq!(global.section.map(|s| names.resolve(s)), Some(".init_array"));
        assert_eq!(global.size, 8);
        let init = global.init.expect("the pointer is there");
        let &[Datum::Addr(reloc)] = &module[init] else { panic!("one address") };
        assert_eq!(names.resolve(module[reloc].symbol), WATCH);
        // Declared, so the object writer has a symbol to point the relocation at.
        let watch = names.intern(WATCH);
        assert!(module.lookup(watch).is_some());
    }
}
