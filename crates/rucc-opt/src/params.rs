//! How big the object behind a pointer parameter is, worked out from the calls that pass it.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.5, which asks for a summary per
//! function recording "which pointer parameters are dereferenced and over what range, which are
//! freed, which escape, and whether the function can free memory at all". `crate::nofree` is the
//! last of those four. This is the first, read the other way round.
//!
//! Section 7.5 writes the dereferenced range as something the callee tells its callers, which is
//! what makes a call site cheaper. What is here is the callers telling the callee, which is what
//! makes the callee's own checks cheaper, and the callee is where the checks are. On the SQLite
//! amalgamation 13284 of the bounds checks the discharge pass keeps are on a pointer that arrived
//! as a parameter, which is more than the next two sources put together, and a parameter is
//! exactly the value a function-at-a-time pass can say nothing about.
//!
//! # What is claimed
//!
//! A function only this module can call, every call to which passes an object with at least so
//! many bytes left in it, has a parameter with at least so many bytes wherever it is used. The
//! objects believed are the two whose extent is already written down: a frame slot of the caller,
//! read off a fixed size `alloca`, and a global this module defines and vouches for, which is
//! `crate::extents`' table. Both are alive for as long as the call runs, so the answer says a
//! lifetime as well as an extent and [`Flags::HANDED`] licenses both, in the way
//! [`Flags::STATIC`] does.
//!
//! Only this module can call it means internal linkage and an address this module never takes.
//! An address is taken by a `global_addr` naming it anywhere in any body, by a relocation in any
//! global's initial image, and by an alias resolving to it. Any of those and the function is left
//! alone, because a call through an address is a call site this cannot see and the argument it
//! passes is one nobody counted.
//!
//! # Where the answer goes
//!
//! Onto the check, as [`Flags::HANDED`], before the pipeline starts. The reason is
//! `crate::extents`' reason: what is being said is worked out across functions and a pass is given
//! one. Writing it on the instruction is also what keeps the claim in one place. A pass reading a
//! flag cannot accidentally believe half of it.
//!
//! # Which way the fixed point goes
//!
//! Every parameter starts unknown and becomes known only when every call site has an answer, and
//! the round is repeated until nothing changes. That is the least fixed point, and it is the one
//! that has to be taken here, because the opposite start would let a fact hold itself up: two
//! functions that pass each other the parameter they were given would agree on any number at all,
//! and a self-recursive function would agree with itself. Starting from unknown, neither of them
//! ever gets an answer, which is a check that stays rather than a check that should not have gone.
//!
//! One argument reaching an answer through the caller's own parameter is the case that makes this
//! worth iterating rather than reading once. A static helper is usually passed what its caller was
//! passed, and the chain only bottoms out at a frame slot several calls up.
//!
//! # What is not here
//!
//! Nothing is said about a pointer that arrived from a `load`, from an allocator or from a call,
//! and nothing is said about a function this module does not define or that anything can reach.
//! Those are the other rows of the measurement and they need their own work.
//!
//! The summary is spent on the checks and thrown away, in the way `crate::nofree`'s is, and for
//! the same reason: a record that survives the file it was worked out in is what link time
//! optimization will want and there is no link time optimization yet.

use std::collections::{HashMap, HashSet};

use rucc_base::Symbol;
use rucc_ir::{
    Datum, Def, Extra, Flags, Func, FuncId, Inst, Linkage, Module, Opcode, Pic, Type, Value,
};

use crate::discharge::{Fact, about, alive, covers, derives, normal};
use crate::extents::extents;

/// Writes [`Flags::HANDED`] onto every check whose bytes are inside an object its callers hand in.
///
/// Gives back how many checks were marked, which is what the pipeline reports.
pub fn annotate(module: &mut Module, pic: Pic) -> usize {
    let reachable = reachable(module);
    let closed: Vec<FuncId> = module
        .funcs()
        .filter(|&id| {
            let func = &module[id];
            !func.is_declaration()
                && func.linkage == Linkage::Internal
                && !reachable.contains(&func.name)
        })
        .collect();
    if closed.is_empty() {
        return 0;
    }
    let globals = extents(module, pic);
    let handed = handed(module, &closed, &globals);
    if handed.is_empty() {
        return 0;
    }
    let mut marked = 0;
    for id in closed {
        let Some(sizes) = handed.get(&id) else { continue };
        let func = &module[id];
        let Some(entry) = func.entry() else { continue };
        let object = |base: Value| -> Option<Fact> {
            let Def::Param { block, index } = func[base].def else { return None };
            if block != entry {
                return None;
            }
            Some(Fact::whole(base, i128::from(*sizes.get(&index)?)))
        };
        let marks: Vec<Inst> = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| !func[inst].flags.contains(Flags::HANDED))
            .filter(|&inst| inside(func, inst, &object))
            .collect();
        marked += marks.len();
        let func = &mut module[id];
        for inst in marks {
            func[inst].flags |= Flags::HANDED;
        }
    }
    marked
}

/// How many bytes each closed function's pointer parameters are known to have.
///
/// Keyed by the function and then by the position of the parameter in the entry block, which is
/// the position of the argument at every call to it. A parameter with no entry is one nothing is
/// known about, and a function with no entry is one where that is true of all of them.
fn handed(
    module: &Module,
    closed: &[FuncId],
    globals: &HashMap<Symbol, u64>,
) -> HashMap<FuncId, HashMap<u32, u64>> {
    let mut where_defined: HashMap<_, FuncId> = HashMap::new();
    for &id in closed {
        where_defined.insert(module[id].name, id);
    }
    let sites = sites(module, &where_defined);
    let mut known: HashMap<FuncId, HashMap<u32, u64>> = HashMap::new();
    loop {
        let mut settled = true;
        for &id in closed {
            let Some(calls) = sites.get(&id) else { continue };
            let count = module[id].signature().params.len();
            let mut sizes = HashMap::new();
            for index in 0..count {
                if module[id].signature().params[index].ty != Type::PTR {
                    continue;
                }
                let Some(least) = least(module, calls, index, globals, &known) else { continue };
                sizes.insert(u32::try_from(index).unwrap_or(u32::MAX), least);
            }
            if known.get(&id) != Some(&sizes) {
                known.insert(id, sizes);
                settled = false;
            }
        }
        if settled {
            known.retain(|_, sizes| !sizes.is_empty());
            return known;
        }
    }
}

/// The fewest bytes any call leaves in the object it passes at that position.
///
/// `None` the moment one call cannot be read, because what is wanted holds at every call or it
/// holds nowhere. A callee nothing in this module calls also answers `None`, since the fewest of
/// no numbers is not a number and pretending otherwise would say anything at all about a function
/// that is only reached from outside.
fn least(
    module: &Module,
    calls: &[(FuncId, Inst)],
    index: usize,
    globals: &HashMap<Symbol, u64>,
    known: &HashMap<FuncId, HashMap<u32, u64>>,
) -> Option<u64> {
    let mut least = None;
    for &(caller, inst) in calls {
        let func = &module[caller];
        let &value = func[func[inst].args].get(index)?;
        let left = passed(caller, func, value, globals, known)?;
        least = Some(least.map_or(left, |so_far: u64| so_far.min(left)));
    }
    least
}

/// How many bytes are left in the object this argument points into.
///
/// The walk to a base and a constant is the discharge pass's, so a call passing `&thing.field`
/// says what is left of `thing` from that field rather than nothing. An offset outside the object
/// is not an object with a negative amount left, it is a pointer this says nothing about.
fn passed(
    caller: FuncId,
    func: &Func,
    value: Value,
    globals: &HashMap<Symbol, u64>,
    known: &HashMap<FuncId, HashMap<u32, u64>>,
) -> Option<u64> {
    let (base, offset) = normal(func, value);
    let whole = i128::from(object(caller, func, base, globals, known)?);
    if offset < 0 || offset > whole {
        return None;
    }
    u64::try_from(whole - offset).ok()
}

/// How big the object a value is, when it is one of the three this believes.
fn object(
    caller: FuncId,
    func: &Func,
    base: Value,
    globals: &HashMap<Symbol, u64>,
    known: &HashMap<FuncId, HashMap<u32, u64>>,
) -> Option<u64> {
    match func[base].def {
        // The caller's own parameter, which is what makes a chain of static helpers worth
        // following. Empty until a round settles it, so the first round reaches only the calls
        // that pass an object outright.
        Def::Param { block, index } => {
            if func.entry() != Some(block) {
                return None;
            }
            known.get(&caller)?.get(&index).copied()
        }
        Def::Result { inst, .. } => match func[inst].opcode {
            Opcode::Alloca if func[func[inst].args].is_empty() => {
                let Extra::Mem(info) = func[inst].extra else { return None };
                Some(func[info].size)
            }
            Opcode::GlobalAddr => {
                let Extra::Symbol(name) = func[inst].extra else { return None };
                globals.get(&name).copied()
            }
            _ => None,
        },
    }
}

/// Every direct call in the module to one of the closed functions, by the function called.
///
/// A call whose argument count does not match what the callee takes is left out rather than
/// counted, since the positions would not line up and a prototype disagreeing with a definition is
/// something a translation unit can contain. A variadic callee is left out for the same reason
/// read the other way: a position past the named parameters is not a parameter.
fn sites(
    module: &Module,
    where_defined: &HashMap<Symbol, FuncId>,
) -> HashMap<FuncId, Vec<(FuncId, Inst)>> {
    let mut sites: HashMap<FuncId, Vec<(FuncId, Inst)>> = HashMap::new();
    for id in module.funcs() {
        let func = &module[id];
        if func.is_declaration() {
            continue;
        }
        for block in func.blocks() {
            for inst in func.insts(block) {
                if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
                    continue;
                }
                let Extra::Call(at) = func[inst].extra else { continue };
                let Some(callee) = func[at].callee else { continue };
                let Some(&target) = where_defined.get(&callee) else { continue };
                let signature = module[target].signature();
                if signature.variadic || signature.params.len() != func[func[inst].args].len() {
                    continue;
                }
                sites.entry(target).or_default().push((id, inst));
            }
        }
    }
    sites
}

/// Every function symbol whose address this module hands out.
///
/// A `global_addr` in any body, a relocation in any global's initial image, and the target of any
/// alias. What each of them has in common is that something other than a direct call can reach the
/// function afterwards, and a call this cannot see is an argument nobody counted.
fn reachable(module: &Module) -> HashSet<Symbol> {
    let mut taken = HashSet::new();
    for id in module.funcs() {
        let func = &module[id];
        if func.is_declaration() {
            continue;
        }
        for block in func.blocks() {
            for inst in func.insts(block) {
                if func[inst].opcode != Opcode::GlobalAddr {
                    continue;
                }
                if let Extra::Symbol(name) = func[inst].extra {
                    taken.insert(name);
                }
            }
        }
    }
    for id in module.globals() {
        let Some(init) = module[id].init else { continue };
        for &datum in &module[init] {
            if let Datum::Addr(at) = datum {
                taken.insert(module[at].symbol);
            }
        }
    }
    for id in module.aliases() {
        taken.insert(module[id].target);
    }
    taken
}

/// Whether that instruction is a check every byte of which is inside one object handed in.
///
/// The three kinds asked the way `crate::discharge` asks them, which is `crate::extents`' shape
/// with the object reader passed in rather than fixed.
fn inside(func: &Func, inst: Inst, object: &impl Fn(Value) -> Option<Fact>) -> bool {
    match func[inst].opcode {
        Opcode::CheckBounds => {
            if func[func[inst].args].len() > 2 {
                return false;
            }
            let Some(asked) = about(func, inst) else { return false };
            object(asked.base).is_some_and(|whole| covers(&whole, &asked))
        }
        Opcode::CheckLive => {
            let Some(asked) = alive(func, inst) else { return false };
            object(asked.base).is_some_and(|whole| covers(&whole, &asked))
        }
        Opcode::CheckDeriv => {
            let Some((from, to)) = derives(func, inst) else { return false };
            object(from.base).is_some_and(|whole| covers(&whole, &from) && covers(&whole, &to))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Builder, Extra, Func, Global, InstData, Linkage, MemInfo, MemOrder, Module, Opcode, Pic,
        Restrict, Signature, Type, Value,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::annotate;

    /// An empty module for a sixty four bit Linux.
    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        Module::new(names.intern("t.c"), &target)
    }

    /// Puts a static function taking one pointer into the module, with a check over `size` bytes
    /// at the pointer it was handed.
    fn callee(names: &mut Interner, module: &mut Module, size: u64) {
        let name = names.intern("g");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR]));
        func.linkage = Linkage::Internal;
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, size);
        live(&mut build, pointer);
        build.ret(&[]);
        module.add_func(func);
    }

    /// Puts a function `f` into the module whose body calls `g` with whatever the closure builds.
    fn caller(
        names: &mut Interner,
        module: &mut Module,
        name: &str,
        argument: impl FnOnce(&mut Builder<'_>) -> Value,
    ) {
        let at = names.intern(name);
        let called = names.intern("g");
        let mut func = Func::new(at, Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        let value = argument(&mut build);
        build.call(called, signature, &[value]);
        build.ret(&[]);
        module.add_func(func);
    }

    /// A stack slot of `size` bytes.
    fn local(build: &mut Builder<'_>, size: u64) -> Value {
        let info = MemInfo {
            size,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        build.value(InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// Puts `cap_of` and a `check_bounds` over `size` bytes at `pointer` into a block.
    fn check(build: &mut Builder<'_>, pointer: Value, size: u64) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let info = MemInfo {
            size,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
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

    /// A pointer `bytes` past another one.
    fn past(build: &mut Builder<'_>, pointer: Value, bytes: i128) -> Value {
        let offset = build.iconst(Type::int(64), bytes);
        let args = build.func().push_values(&[pointer, offset]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    #[test]
    fn a_check_on_a_parameter_every_call_hands_a_slot_big_enough_is_marked() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        caller(&mut names, &mut module, "f", |build| local(build, 32));
        assert_eq!(
            annotate(&mut module, Pic::Executable),
            2,
            "the bounds check and the lifetime one"
        );
    }

    #[test]
    fn a_call_handing_a_slot_too_small_marks_only_the_lifetime_check() {
        // Eight bytes are not the sixteen the bounds check reads, and they are the one byte the
        // lifetime check is about. The extent and the lifetime are separate claims and a slot too
        // small for the first still settles the second.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        caller(&mut names, &mut module, "f", |build| local(build, 8));
        assert_eq!(annotate(&mut module, Pic::Executable), 1);
    }

    #[test]
    fn the_fewest_bytes_any_call_hands_is_what_the_parameter_gets() {
        // Two calls, one generous and one not. What holds at the parameter is what holds at every
        // call, so the eight byte slot decides and the bounds check is not marked. The generous
        // call does not get it either, because there is one parameter and not one per call site.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        caller(&mut names, &mut module, "f", |build| local(build, 32));
        caller(&mut names, &mut module, "h", |build| local(build, 8));
        assert_eq!(
            annotate(&mut module, Pic::Executable),
            1,
            "the lifetime check, which eight bytes settle"
        );
    }

    #[test]
    fn a_call_handing_a_field_of_a_slot_leaves_what_is_past_the_field() {
        // Sixteen bytes past the start of a thirty two byte slot is sixteen bytes left, which is
        // exactly what the check asks for.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        caller(&mut names, &mut module, "f", |build| {
            let slot = local(build, 32);
            past(build, slot, 16)
        });
        assert_eq!(annotate(&mut module, Pic::Executable), 2);
    }

    #[test]
    fn a_call_handing_a_field_that_leaves_too_little_marks_only_the_lifetime_check() {
        // Twelve bytes left of the thirty two, which is less than the sixteen the bounds check
        // reads and more than the one the lifetime check is about.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        caller(&mut names, &mut module, "f", |build| {
            let slot = local(build, 32);
            past(build, slot, 20)
        });
        assert_eq!(annotate(&mut module, Pic::Executable), 1);
    }

    #[test]
    fn a_callee_anything_can_reach_is_left_alone() {
        // The same module with `g` external rather than static. A call this module cannot see
        // passes an argument nobody counted, so nothing is claimed about the parameter.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        let id = module.funcs().next().expect("the callee");
        module[id].linkage = Linkage::External;
        caller(&mut names, &mut module, "f", |build| local(build, 32));
        assert_eq!(annotate(&mut module, Pic::Executable), 0);
    }

    #[test]
    fn a_callee_whose_address_is_taken_is_left_alone() {
        // The `global_addr` is what taking the address of a function looks like, and after it the
        // call this counted is no longer the only way in.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        caller(&mut names, &mut module, "f", |build| local(build, 32));
        let called = names.intern("g");
        caller(&mut names, &mut module, "h", |build| {
            let extra = Extra::Symbol(called);
            build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
            local(build, 32)
        });
        assert_eq!(annotate(&mut module, Pic::Executable), 0);
    }

    #[test]
    fn a_callee_named_by_a_globals_image_is_left_alone() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        caller(&mut names, &mut module, "f", |build| local(build, 32));
        let at = module.add_reloc(rucc_ir::Reloc { symbol: names.intern("g"), addend: 0, size: 8 });
        let init = module.push_data(&[rucc_ir::Datum::Addr(at)]);
        let mut global = Global::new(names.intern("table"), 8, 8);
        global.init = Some(init);
        module.add_global(global);
        assert_eq!(annotate(&mut module, Pic::Executable), 0);
    }

    #[test]
    fn a_callee_nothing_in_the_module_calls_is_left_alone() {
        // The fewest of no numbers is not a number, and saying otherwise would claim anything at
        // all about a function only reached from outside.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        assert_eq!(annotate(&mut module, Pic::Executable), 0);
    }

    #[test]
    fn a_chain_of_static_helpers_reaches_the_slot_at_the_top() {
        // `f` has the slot, `h` is handed it, `g` is handed what `h` was handed. The middle link
        // is what makes the fixed point worth iterating: `g` gets an answer only after `h` has
        // one, which is the round after.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        callee(&mut names, &mut module, 16);
        let name = names.intern("h");
        let called = names.intern("g");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR]));
        func.linkage = Linkage::Internal;
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        build.call(called, signature, &[pointer]);
        build.ret(&[]);
        module.add_func(func);
        let at = names.intern("f");
        let called = names.intern("h");
        let mut func = Func::new(at, Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        let slot = local(&mut build, 32);
        build.call(called, signature, &[slot]);
        build.ret(&[]);
        module.add_func(func);
        assert_eq!(annotate(&mut module, Pic::Executable), 2);
    }

    #[test]
    fn two_functions_handing_each_other_their_own_parameter_hold_nothing_up() {
        // The reason the fixed point starts from unknown. Neither of these has a call site with an
        // object in it, and starting from the other end they would agree on any number at all.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        relay(&mut names, &mut module, "g", "h", 16);
        relay(&mut names, &mut module, "h", "g", 16);
        assert_eq!(annotate(&mut module, Pic::Executable), 0);
    }

    /// A static function taking one pointer, checking `size` bytes at it and handing it on.
    fn relay(names: &mut Interner, module: &mut Module, name: &str, on: &str, size: u64) {
        let at = names.intern(name);
        let called = names.intern(on);
        let mut func = Func::new(at, Signature::new().with_params(&[Type::PTR]));
        func.linkage = Linkage::Internal;
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, size);
        let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        build.call(called, signature, &[pointer]);
        build.ret(&[]);
        module.add_func(func);
    }
}
