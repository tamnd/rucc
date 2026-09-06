//! Witnessing the pointers that cross between instrumented code and code that is not.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.3 and
//! `spec/safe-memory/10-boundaries.md` sections 10.2 and 10.7.
//!
//! [`mod@crate::wrap`] handles one direction of the boundary and only for the functions there is a
//! row for: a call to `memcpy` becomes a call to a wrapper that judges both ranges. Everything else
//! that crosses is what this module is about, and there are two of those.
//!
//! A pointer *arrives* when uninstrumented code calls a function this compiler built. There is no
//! frame beside the call, because the caller did not know to write one, so the callee's pointer
//! parameters have no capability and the runtime has to reconstruct what it can from the address.
//! That is [`crate::wrap`]'s opposite number in the runtime, `recover`, and section 10.8 says the
//! callback handed to `dlopen`ed code is the case with no other answer.
//!
//! A pointer *leaves* and comes back when instrumented code calls a function this build did not
//! instrument and gets a pointer out of it. `fopen` hands back a `FILE *`, an uninstrumented
//! library hands back whatever it allocated, and neither address means anything to the planes until
//! somebody has looked.
//!
//! Both are the same question, which is how much is known about an address that came from outside,
//! and section 10.2's answer is that the honest thing to do with a question you cannot answer is to
//! count it. So each of those places gets a call to `__rucc_cap_witness`, and what a run leaves
//! behind is the four counts of `recover`: how many crossings landed on an instance the runtime
//! knows the bounds of, how many landed in an arena whose allocator has said nothing, how many
//! landed on storage nobody owns, and how many landed somewhere nothing watches at all. The last is
//! the one a reviewer should read first, because it is how much of a program the boundary is not
//! covering.
//!
//! # Why the capability is thrown away
//!
//! Because there is nowhere to put it. A capability is four words and the aux plane that lets a
//! pointer sitting in memory carry one is milestone S5, so a witness today leaves nothing behind
//! but the count. That is why the runtime entry point is `witness` rather than `recover`: the
//! classification is a region lookup and one plane read, and the bounds walk that `recover` also
//! does would be paid for an answer with nowhere to go.
//!
//! The counts are the same either way, which is the property that matters. When S5 arrives these
//! call sites become the places a capability is produced rather than the places one is counted, and
//! the number a summary prints does not move.
//!
//! # Which functions can be entered from outside
//!
//! Anything the linker can bind to, plus anything whose address this module takes. A `static`
//! function nobody takes the address of cannot be reached from another object, so witnessing its
//! parameters would be counting a crossing that did not happen, and a count that includes things
//! that did not happen is worse than no count.
//!
//! An external function called from inside this same module is witnessed anyway, and that is not a
//! mistake either: nothing publishes a frame yet, so a call from instrumented code arrives exactly
//! as bare as a call from uninstrumented code and the runtime cannot tell them apart. It is not
//! pretending to. The day the call frame is written at call sites, the ones written here stop being
//! crossings and the count falls to what actually came from outside, which is the number this was
//! always trying to be.
//!
//! # Why a tail call's result is not witnessed
//!
//! There is nowhere to put the call. A tail call's result is the caller's result and the frame is
//! gone by the time there would be one, so witnessing it would mean turning the tail call back into
//! an ordinary one. Giving up a tail call to raise a counter is the wrong trade, and the crossing is
//! still counted at the other end when whoever receives the pointer is instrumented.

use rucc_base::{Interner, Symbol};
use rucc_ir::{
    CallInfo, Extra, Func, Inst, InstData, Linkage, Module, Opcode, Signature, Type, Value,
};

/// The runtime symbol a crossing is spelled as.
pub const WITNESS: &str = "__rucc_cap_witness";

/// How many places in a module a pointer crosses the boundary.
///
/// Two numbers rather than one, because they are different holes. A pointer arriving is a function
/// of this build that somebody else can call, and a pointer returning is a library this build chose
/// to link against. Which of the two a program is mostly made of says something about it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sites {
    /// Pointer parameters of functions uninstrumented code can reach.
    pub entered: usize,
    /// Pointers handed back by a call to a function this build did not instrument.
    pub returned: usize,
}

impl Sites {
    /// Every crossing, whichever way it went.
    #[must_use]
    pub const fn total(self) -> usize {
        self.entered + self.returned
    }
}

/// Puts a witness at every crossing in a module, and says how many it put in.
///
/// Runs where [`crate::redirect`] runs, before the optimizer, and for the same reason: a call to a
/// symbol the optimizer has no opinion about is a call it leaves alone, and a witness inserted
/// afterwards would be a witness on whatever the optimizer left rather than on what the program
/// wrote.
pub fn witness(module: &mut Module, names: &mut Interner) -> Sites {
    let callee = names.intern(WITNESS);
    let outside = reachable(module);
    let defined: Vec<Symbol> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| module[id].name)
        .collect();

    let mut sites = Sites::default();
    let ids: Vec<_> = module.funcs().collect();
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        if outside.contains(&module[id].name) {
            sites.entered += entering(&mut module[id], callee);
        }
        sites.returned += returning(&mut module[id], names, callee, &defined);
    }
    sites
}

/// The functions in a module that code outside it can call.
///
/// Which is every one the linker can bind to, and every one whose address this module puts in a
/// value. The second half is where a callback comes from, and it is the case section 10.8 says has
/// no answer other than recovery.
fn reachable(module: &Module) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    let defined: Vec<Symbol> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| module[id].name)
        .collect();
    for id in module.funcs() {
        if module[id].is_declaration() {
            continue;
        }
        if module[id].linkage != Linkage::Internal {
            out.push(module[id].name);
        }
        let func = &module[id];
        for block in func.blocks() {
            for inst in func.insts(block) {
                if func[inst].opcode != Opcode::GlobalAddr {
                    continue;
                }
                let Extra::Symbol(taken) = func[inst].extra else { continue };
                // A global variable's address is taken the same way a function's is, so the name
                // has to be one of the functions here before it means a callback.
                if defined.contains(&taken) && !out.contains(&taken) {
                    out.push(taken);
                }
            }
        }
    }
    out
}

/// Whether a call to this name leaves the part of the program this build is responsible for.
///
/// A function this module defines is instrumented, so a pointer it hands back was already judged on
/// the way out and there is nothing to recover. A name this compiler put there is the runtime,
/// including the wrappers, and a wrapper's result is the one thing at the boundary that *was*
/// modelled, so counting it would make an instrumented file look like it trusts more than the
/// uninstrumented one it came from.
fn crosses(name: Symbol, names: &Interner, defined: &[Symbol]) -> bool {
    !defined.contains(&name) && !names.resolve(name).starts_with("__rucc_")
}

/// Witnesses every pointer parameter of a function, at the top of its entry block.
fn entering(func: &mut Func, callee: Symbol) -> usize {
    let Some(entry) = func.entry() else { return 0 };
    let Some(first) = func.insts(entry).next() else { return 0 };
    let params: Vec<_> =
        func[entry].params.iter().copied().filter(|&value| func[value].ty.is_ptr()).collect();

    for param in &params {
        let call = crossing(func, callee, *param, first);
        func.insert_before(call, first);
    }
    params.len()
}

/// Witnesses the result of every call in a function that leaves this build.
fn returning(func: &mut Func, names: &Interner, callee: Symbol, defined: &[Symbol]) -> usize {
    let insts: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();

    let mut count = 0;
    for inst in insts {
        // A `CallIndirect` is left alone for the reason a `Call` with no callee is, below.
        if func[inst].opcode != Opcode::Call {
            continue;
        }
        let Extra::Call(at) = func[inst].extra else { continue };
        match func[at].callee {
            Some(name) if !crosses(name, names, defined) => continue,
            Some(_) => {}
            // A call with no callee is one through a pointer. Its result crossed a boundary too,
            // and the summary already counts the call site itself against the trust set, so a
            // witness here would say the same thing twice about something nothing can be done
            // about.
            None => continue,
        }
        let Some(result) = func[inst].results().next() else { continue };
        if !func[result].ty.is_ptr() {
            continue;
        }
        let call = crossing(func, callee, result, inst);
        func.insert_after(call, inst);
        count += 1;
    }
    count
}

/// One call to the runtime, carrying the pointer that crossed.
fn crossing(func: &mut Func, callee: Symbol, pointer: Value, at: Inst) -> Inst {
    let signature = func.add_signature(Signature::new().with_params(&[Type::PTR]));
    // Nothing is passed past the last named parameter, so there is nothing for the ABI to say
    // about the arguments the signature does not name.
    let varargs = func.push_abis(&[]);
    let info = func.add_call(CallInfo { callee: Some(callee), signature, varargs });
    let args = func.push_values(&[pointer]);
    // The span of the instruction it is anchored to, so that a diagnostic about the crossing points
    // at the call or the entry it belongs to rather than at nothing.
    let span = func.span(at);
    func.create_inst(
        InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) },
        &[],
        span,
    )
}

#[cfg(test)]
mod tests {
    use rucc_ir::{Builder, print_func, verify};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// A function taking a pointer and an integer and returning the pointer.
    fn taking(names: &mut Interner, name: &str, linkage: Linkage) -> Func {
        let mut func = Func::new(
            names.intern(name),
            Signature::new().with_params(&[Type::PTR, Type::int(64)]).with_returns(&[Type::PTR]),
        );
        func.linkage = linkage;
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let _n = func.append_param(entry, Type::int(64));
        let mut b = Builder::new(&mut func, entry);
        b.ret(&[p]);
        func
    }

    #[test]
    fn a_function_the_linker_can_bind_to_witnesses_its_pointer_parameters() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("one.c"), &target());
        module.add_func(taking(&mut names, "take", Linkage::External));

        assert_eq!(witness(&mut module, &mut names), Sites { entered: 1, returned: 0 });
        let id = module.funcs().next().expect("the function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            "func @take(ptr, i64) -> ptr, linkage(external) {\n\
             block0(%0: ptr, %1: i64):\n    \
             call @__rucc_cap_witness(%0) : (ptr)\n    \
             return %0\n\
             }\n"
        );
        if let Err(errors) = verify(&module, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_static_function_nobody_takes_the_address_of_is_left_alone() {
        // It cannot be reached from another object, so a witness on it would count a crossing that
        // never happens, and a count that includes things that did not happen is worse than none.
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("one.c"), &target());
        module.add_func(taking(&mut names, "take", Linkage::Internal));
        assert_eq!(witness(&mut module, &mut names), Sites::default());
    }

    #[test]
    fn a_static_function_whose_address_is_taken_is_a_callback_and_is_witnessed() {
        // Section 10.8's case: the address goes to code this build did not compile, which calls it
        // with pointers of its own and publishes no frame.
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("one.c"), &target());
        module.add_func(taking(&mut names, "visit", Linkage::Internal));

        let mut caller = Func::new(names.intern("run"), Signature::new());
        let entry = caller.create_block();
        let taken = names.intern("visit");
        let mut b = Builder::new(&mut caller, entry);
        b.value(
            InstData { extra: Extra::Symbol(taken), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        );
        b.ret(&[]);
        module.add_func(caller);

        assert_eq!(witness(&mut module, &mut names), Sites { entered: 1, returned: 0 });
    }

    #[test]
    fn a_pointer_handed_back_by_a_library_this_build_did_not_instrument_is_witnessed() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("one.c"), &target());

        let mut func = Func::new(names.intern("run"), Signature::new().with_returns(&[Type::PTR]));
        func.linkage = Linkage::Internal;
        let entry = func.create_block();
        let signature = func.add_signature(Signature::new().with_returns(&[Type::PTR]));
        let varargs = func.push_abis(&[]);
        let opened = names.intern("notes_open");
        let info = func.add_call(CallInfo { callee: Some(opened), signature, varargs });
        let mut b = Builder::new(&mut func, entry);
        let got = b
            .value(InstData { extra: Extra::Call(info), ..InstData::new(Opcode::Call) }, Type::PTR);
        b.ret(&[got]);
        module.add_func(func);

        assert_eq!(witness(&mut module, &mut names), Sites { entered: 0, returned: 1 });
        let id = module.funcs().next().expect("the function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            "func @run() -> ptr, linkage(internal) {\n\
             block0:\n    \
             %0 = call @notes_open() : () -> ptr\n    \
             call @__rucc_cap_witness(%0) : (ptr)\n    \
             return %0\n\
             }\n"
        );
        if let Err(errors) = verify(&module, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_call_to_a_function_this_module_defines_is_not_a_crossing() {
        // It is instrumented, so the pointer it hands back was checked on the way out. Counting it
        // would put the file's own calls in the number that is supposed to say what it trusts.
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("one.c"), &target());
        module.add_func(taking(&mut names, "take", Linkage::Internal));

        let mut func = Func::new(names.intern("run"), Signature::new().with_returns(&[Type::PTR]));
        func.linkage = Linkage::Internal;
        let entry = func.create_block();
        let signature = func.add_signature(Signature::new().with_returns(&[Type::PTR]));
        let varargs = func.push_abis(&[]);
        let take = names.intern("take");
        let info = func.add_call(CallInfo { callee: Some(take), signature, varargs });
        let mut b = Builder::new(&mut func, entry);
        let got = b
            .value(InstData { extra: Extra::Call(info), ..InstData::new(Opcode::Call) }, Type::PTR);
        b.ret(&[got]);
        module.add_func(func);

        assert_eq!(witness(&mut module, &mut names), Sites::default());
    }

    #[test]
    fn a_call_that_hands_back_something_that_is_not_a_pointer_is_not_a_crossing() {
        // `strlen` returns a count. There is nothing to recover a capability for and nothing to
        // count, and witnessing it would make the number mean calls rather than pointers.
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("one.c"), &target());

        let mut func =
            Func::new(names.intern("run"), Signature::new().with_returns(&[Type::int(64)]));
        func.linkage = Linkage::Internal;
        let entry = func.create_block();
        let signature = func.add_signature(Signature::new().with_returns(&[Type::int(64)]));
        let varargs = func.push_abis(&[]);
        let counted = names.intern("notes_count");
        let info = func.add_call(CallInfo { callee: Some(counted), signature, varargs });
        let mut b = Builder::new(&mut func, entry);
        let got = b.value(
            InstData { extra: Extra::Call(info), ..InstData::new(Opcode::Call) },
            Type::int(64),
        );
        b.ret(&[got]);
        module.add_func(func);

        assert_eq!(witness(&mut module, &mut names), Sites::default());
    }
}
