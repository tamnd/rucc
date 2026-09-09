//! How big the objects a module declares are, and which safety checks that already answers.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.2, which lists four sources of a
//! discharge and puts the frontend first: "The overwhelming majority of accesses in real C are to a
//! local, a global, or a field of an object whose type is known, at a constant offset." The bounds
//! of such an object are not something anybody has to find out, and the section says why it matters
//! more than the other three put together: "This is not an optimization; it is the frontend not
//! being stupid, and it is where most of the win is."
//!
//! The local half of that sentence is read straight off the `alloca` by `crate::discharge`, because
//! an `alloca` of a fixed size says how many bytes it is in its own payload and the pass is looking
//! at it. This is the global half, and it is a separate file for one reason: a global's size lives
//! on the module and a pass is given one function.
//!
//! # Where the answer goes
//!
//! Onto the check, as [`Flags::STATIC`], before the pipeline starts. `crate::nofree` writes a fact
//! about the callee onto the call site for exactly the same reason and the argument is that one
//! repeated. The alternative would be handing every pass the module it is not being given, and the
//! precedent for not doing that is the frontend putting an `unreachable` after a call that never
//! comes back rather than expecting every later pass to look the callee up.
//!
//! One consequence is worth stating plainly. This runs before anything has folded, so an offset the
//! frontend left as arithmetic is an offset this does not read, and the check keeps its price. What
//! is lost that way is a missed optimization and never a wrong answer, since a fact nobody wrote
//! down is a check that stays.
//!
//! # What the flag says and what it does not
//!
//! It says the bytes the check names lie inside one object of static storage duration whose extent
//! this module knows. That is a fact of exactly the shape a passing `check_bounds` establishes, and
//! `crate::discharge` asks the same `covered.i64` rule about it that it asks about every other fact,
//! so section 7.7's separation of the walk from the condition is untouched: nothing here decides
//! that a check may go, and the arithmetic is still somebody's proof rather than this file's
//! opinion.
//!
//! Static storage duration is the other half of it and is what lets a `check_live` go as well as a
//! `check_bounds`. A global lives as long as the program does, so the instance holding an address
//! inside one is alive wherever the question is asked. That is `StorageClass::Static` of
//! `spec/safe-memory/04-safety-model.md` section 4.1, and it is why the flag is named for the
//! storage duration rather than for the size.
//!
//! # Which globals are believed
//!
//! A definition, because a module that only says the variable exists somewhere has not been told
//! how big it is, and the size field on a declaration is whatever the declaration implied.
//!
//! Not `weak`, `linkonce` or `common`, because the linker is allowed to take a definition from
//! another object instead and this analysis read the one that will not run. That is the bar
//! `crate::nofree` sets for a function body and the reason is the same one.
//!
//! Not thread-local, because the address of a thread-local is not the `global_addr` itself on every
//! model and a fact that holds on some of them is not one to write down.
//!
//! An ordinary external definition is believed, and that is the stated assumption `crate::nofree`
//! makes about a function body. A data symbol in a shared library can be interposed, and closing
//! that means deciding what this compiler does about interposition generally. Until it has an
//! answer, a build that cares can say `-fvisibility=hidden`.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_ir::{Def, Extra, Flags, Func, FuncId, Inst, Linkage, Module, Opcode, Value};

use crate::discharge::{Fact, about, alive, covers, derives};

/// Marks every check whose bytes are inside a global this module defines, saying how many.
///
/// Only sets the flag, never clears one, for the reason [`crate::nofree::annotate`] gives: the flag
/// is an assertion, so a caller that put one there meant it.
pub fn annotate(module: &mut Module) -> usize {
    let sizes = extents(module);
    if sizes.is_empty() {
        return 0;
    }
    let mut marked = 0;
    let ids: Vec<FuncId> = module.funcs().collect();
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        let func = &mut module[id];
        let insts: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
        for inst in insts {
            if func[inst].flags.contains(Flags::STATIC) || !inside(func, inst, &sizes) {
                continue;
            }
            func[inst].flags |= Flags::STATIC;
            marked += 1;
        }
    }
    marked
}

/// Whether that instruction is a check every byte of which is inside one global.
///
/// The three kinds are asked in the shape `crate::discharge` asks them in, since the point of the
/// flag is that the pass finds the answer already there rather than a second opinion about it.
fn inside(func: &Func, inst: Inst, sizes: &HashMap<Symbol, u64>) -> bool {
    match func[inst].opcode {
        Opcode::CheckBounds => {
            // The hoisted form of section 7.4 covers as many bytes as its loop runs times, which is
            // a number only the program has, so there is nothing here to compare against an extent.
            if func[func[inst].args].len() > 2 {
                return false;
            }
            let Some(asked) = about(func, inst) else { return false };
            object(func, asked.base, sizes).is_some_and(|whole| covers(&whole, &asked))
        }
        Opcode::CheckLive => {
            let Some(asked) = alive(func, inst) else { return false };
            object(func, asked.base, sizes).is_some_and(|whole| covers(&whole, &asked))
        }
        // Both ends inside one object, which is the whole of what a derivation check asks. Two
        // objects each holding one end would answer nothing, and one of these cannot be two objects
        // because both ends normalized to the same base.
        Opcode::CheckDeriv => {
            let Some((from, to)) = derives(func, inst) else { return false };
            object(func, from.base, sizes)
                .is_some_and(|whole| covers(&whole, &from) && covers(&whole, &to))
        }
        _ => false,
    }
}

/// The object a global is, when the address a check is about was computed from one.
fn object(func: &Func, base: Value, sizes: &HashMap<Symbol, u64>) -> Option<Fact> {
    let Def::Result { inst, .. } = func[base].def else { return None };
    if func[inst].opcode != Opcode::GlobalAddr {
        return None;
    }
    let Extra::Symbol(name) = func[inst].extra else { return None };
    let size = *sizes.get(&name)?;
    Some(Fact::whole(base, i128::from(size)))
}

/// How big each global this module both defines and can vouch for is.
///
/// A name that somehow arrives twice keeps the smaller of the two sizes. That cannot happen in a
/// module the frontend built, and writing it this way means the failure if it ever does is a check
/// that stays rather than one that goes.
pub(crate) fn extents(module: &Module) -> HashMap<Symbol, u64> {
    let mut sizes: HashMap<Symbol, u64> = HashMap::new();
    for id in module.globals() {
        let global = &module[id];
        if global.is_declaration() || global.size == 0 || global.tls.is_some() {
            continue;
        }
        if !matches!(global.linkage, Linkage::External | Linkage::Internal) {
            continue;
        }
        let at = sizes.entry(global.name).or_insert(global.size);
        *at = (*at).min(global.size);
    }
    sizes
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Builder, Extra, Flags, Func, Global, InstData, Linkage, MemInfo, MemOrder, Module, Opcode,
        Restrict, Signature, TlsModel, Type, Value,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::annotate;

    /// A module with one global named `g`, of that size, linkage and definedness.
    fn module(size: u64, linkage: Linkage, defined: bool) -> (Interner, Module) {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let mut global = Global::new(names.intern("g"), size, 8);
        global.linkage = linkage;
        if defined {
            global.init = Some(module.push_data(&[]));
        }
        module.add_global(global);
        (names, module)
    }

    /// The plain case, which is a sixty four byte global this module defines.
    fn defined() -> (Interner, Module) {
        module(64, Linkage::External, true)
    }

    /// Puts a function `f` into the module, whose body starts from the address of the global.
    fn func(names: &mut Interner, module: &mut Module, body: impl FnOnce(&mut Builder<'_>, Value)) {
        let name = names.intern("f");
        let at = names.intern("g");
        let mut func = Func::new(name, Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let extra = Extra::Symbol(at);
        let at = build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        body(&mut build, at);
        build.ret(&[]);
        module.add_func(func);
    }

    /// Puts `cap_of` and a `check_bounds` over `size` bytes at `pointer` into a block.
    ///
    /// The shape `rucc-safety` emits, written out here rather than reached for, because `rucc-opt`
    /// is rank 9 alongside `rucc-safety` and cannot depend on it.
    fn check(build: &mut Builder<'_>, pointer: Value, size: u64) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let info = MemInfo {
            size,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let args = build.func().push_values(&[capability, pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
    }

    /// Puts `cap_of` and a `check_live` at `pointer` into a block.
    fn live(build: &mut Builder<'_>, pointer: Value) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = build.func().push_values(&[capability, pointer]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckLive) }, &[]);
    }

    /// Puts a `check_deriv` over a walk from `from` to `to` into a block.
    fn deriv(build: &mut Builder<'_>, from: Value, to: Value) {
        let args = build.func().push_values(&[from]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let width = build.iconst(Type::int(64), 1);
        let args = build.func().push_values(&[capability, from, to, width]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckDeriv) }, &[]);
    }

    /// A pointer `bytes` past another one.
    fn past(build: &mut Builder<'_>, pointer: Value, bytes: i128) -> Value {
        let offset = build.iconst(Type::int(64), bytes);
        let args = build.func().push_values(&[pointer, offset]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    /// How many instructions in the module carry the flag.
    fn flagged(module: &Module) -> usize {
        module
            .funcs()
            .map(|id| {
                let func = &module[id];
                func.blocks()
                    .flat_map(|block| func.insts(block).collect::<Vec<_>>())
                    .filter(|&inst| func[inst].flags.contains(Flags::STATIC))
                    .count()
            })
            .sum()
    }

    #[test]
    fn every_check_inside_a_global_this_module_defines_is_marked() {
        let (mut names, mut module) = defined();
        func(&mut names, &mut module, |build, at| {
            let field = past(build, at, 32);
            deriv(build, at, field);
            check(build, field, 8);
            live(build, field);
        });
        assert_eq!(annotate(&mut module), 3);
        assert_eq!(flagged(&module), 3);
        // Running it again finds nothing left to say, which is what makes it safe to run over a
        // module that has already been through it.
        assert_eq!(annotate(&mut module), 0);
        assert_eq!(flagged(&module), 3);
    }

    #[test]
    fn a_check_that_runs_off_the_end_of_a_global_is_left_alone() {
        // Four bytes short of the end and eight bytes wide, so the object holds the address and
        // not the access. The rule is what says so, and it says so about the same term the pass
        // would have built.
        let (mut names, mut module) = defined();
        func(&mut names, &mut module, |build, at| {
            let field = past(build, at, 60);
            check(build, field, 8);
        });
        assert_eq!(annotate(&mut module), 0);
    }

    #[test]
    fn a_walk_that_leaves_the_global_is_left_alone() {
        let (mut names, mut module) = defined();
        func(&mut names, &mut module, |build, at| {
            let away = past(build, at, 128);
            deriv(build, at, away);
        });
        assert_eq!(annotate(&mut module), 0);
    }

    #[test]
    fn a_check_before_the_start_of_a_global_is_left_alone() {
        let (mut names, mut module) = defined();
        func(&mut names, &mut module, |build, at| {
            let before = past(build, at, -8);
            check(build, before, 8);
            live(build, before);
        });
        assert_eq!(annotate(&mut module), 0);
    }

    #[test]
    fn a_global_this_module_only_declares_says_nothing_about_how_big_it_is() {
        let (mut names, mut module) = module(64, Linkage::External, false);
        func(&mut names, &mut module, |build, at| check(build, at, 8));
        assert_eq!(annotate(&mut module), 0);
    }

    #[test]
    fn a_definition_the_linker_may_replace_is_not_believed() {
        for linkage in [Linkage::Weak, Linkage::LinkOnce, Linkage::Common] {
            let (mut names, mut module) = module(64, linkage, true);
            func(&mut names, &mut module, |build, at| check(build, at, 8));
            assert_eq!(annotate(&mut module), 0, "{}", linkage.name());
        }
    }

    #[test]
    fn a_thread_local_is_not_believed() {
        let (mut names, mut module) = module(64, Linkage::Internal, true);
        let id = module.globals().next().unwrap();
        module[id].tls = Some(TlsModel::GlobalDynamic);
        func(&mut names, &mut module, |build, at| check(build, at, 8));
        assert_eq!(annotate(&mut module), 0);
    }

    #[test]
    fn a_check_over_a_length_the_program_worked_out_is_left_alone() {
        // Section 7.4's hoisted check covers as many bytes as its loop runs times, so there is no
        // number here to compare with the size of the object.
        let (mut names, mut module) = defined();
        func(&mut names, &mut module, |build, at| {
            let args = build.func().push_values(&[at]);
            let capability =
                build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
            let bytes = build.iconst(Type::int(64), 8);
            let info = MemInfo {
                size: 8,
                align: 1,
                order: MemOrder::NotAtomic,
                tbaa: None,
                restrict: Restrict::NONE,
            };
            let extra = Extra::Mem(build.func().add_mem(info));
            let args = build.func().push_values(&[capability, at, bytes]);
            build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
        });
        assert_eq!(annotate(&mut module), 0);
    }

    #[test]
    fn a_check_on_a_name_this_module_never_heard_of_is_left_alone() {
        let (mut names, mut module) = defined();
        let name = names.intern("f");
        let elsewhere = names.intern("elsewhere");
        let mut body = Func::new(name, Signature::new());
        let block = body.create_block();
        let mut build = Builder::new(&mut body, block);
        let extra = Extra::Symbol(elsewhere);
        let at = build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        check(&mut build, at, 8);
        build.ret(&[]);
        module.add_func(body);
        assert_eq!(annotate(&mut module), 0);
    }

    #[test]
    fn a_module_with_no_globals_is_nothing_to_work_out() {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        func(&mut names, &mut module, |build, at| check(build, at, 8));
        assert_eq!(annotate(&mut module), 0);
    }
}
