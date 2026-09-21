//! Unused parameter removal: a parameter nothing reads, and the argument every call passed to it.
//!
//! Design: `spec/optimizer/34-ipa.md` section 34.6, which asks for "unused parameter and return
//! value removal, roughly 300 lines, at `-O2`, the cheap half of 34.5's IPA-SRA".
//!
//! Section 34.5 reads gcc's `gcc/ipa-sra.cc`, 4,753 lines, and splits it in two. The expensive half
//! takes a structure passed by reference and passes the fields the callee uses instead, and that
//! one is in section 34.6's list of what M4 does not build. The cheap half is this. Something has
//! to stop reading a parameter before it is worth anything, and two things do: inlining, and the
//! propagation in [`crate::ipcp`] putting a constant into the body so that the parameter is named
//! nowhere. Both of those leave a caller computing a value and handing it to nobody.
//!
//! This is the parameter half. The return value half is the same machinery pointed the other way,
//! rewriting the call sites rather than the body, and it is not here yet.
//!
//! # What a function has to be
//!
//! [`ipa::closed`], which is section 34.6 asking for the care it asks for: "a function whose
//! signature changes must have every call site updated, and any function whose address is taken or
//! which is externally visible cannot be changed at all". Internal linkage and no address handed
//! out are what make the calls this unit can see the whole set. Not variadic, because a position
//! past the named parameters is not a parameter. And every call has to pass the number of arguments
//! the function takes, because one call this cannot rewrite is one call left handing the old list
//! to the new signature.
//!
//! A function nothing in the unit calls is left alone. No call site reads any parameter of it, so
//! trimming it would be a `-fopt-info` line about a function nothing reaches. Removing that
//! function is dead code elimination's, and until something does, its signature is the one it was
//! written with.
//!
//! # The transitive case
//!
//! Section 34.5 quotes the shape gcc's own header highlights: if two parameters of one function are
//! used only in a sum passed to another function that does not use it, all three parameters
//! disappear. Reaching that takes two steps and only the first is here. Taking the callee's
//! parameter out takes the argument out of the call site, so the sum is computed and read by
//! nothing. Then something has to notice the sum is dead, or its operands still look read and the
//! caller's own two parameters stay.
//!
//! [`crate::dce`] is what notices, run over each function a round changed, and then the round runs
//! again over bodies with the dead arithmetic gone. Three rounds, which is a level deeper than the
//! quoted shape needs.
//!
//! Gcc gets the same answer in one traversal, with the two sweeps its header describes, callees to
//! callers for parameters and callers to callees for return values, and the strongly connected
//! components iterated inside. The rounds here are what stands in for that iteration. The reason to
//! spend a round rather than write the liveness out again is that "does anything read this" already
//! has an answer in this crate, and a second answer would be a second thing to be wrong.
//!
//! # What it does not do
//!
//! Not an aggregate, per the split above.
//!
//! Not a return value, yet.
//!
//! Not a parameter whose only reader is the recursive call that hands it back to itself. Nothing
//! outside the cycle reads it and gcc's sweep over a component takes it. Here the count of readers
//! is one rather than none and it stays, which is a missed optimization and not a wrong answer.
//!
//! Not a parameter read only by an instruction that is itself dead. The sweep at the end of a round
//! runs over the functions the round changed, which are the callers, and a callee nothing has
//! changed yet is read as it stands. Whatever ran [`crate::dce`] before this is what decides how
//! much of that there is.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_ir::{CallInfo, Extra, Func, FuncId, Inst, Module, Signature, Value};

use crate::purity::Facts;
use crate::stats::Kind;
use crate::{CallGraph, Fuel, Stats, dce, ipa, purity};

/// Recorded for each parameter that went, and for the argument that went with it at every call.
const GONE: &str = "parameter nothing reads removed, and the argument at every call with it";

/// Recorded for a parameter that would have gone if there had been fuel for it.
///
/// Not a missed optimization in the ordinary sense, since the fuel is somebody deliberately
/// stopping the pass. It is here because it is the number a bisection is searching for.
const NO_FUEL: &str = "parameter left alone, the pass ran out of fuel";

/// What this is called, which is gcc's spelling of it so that `-fno-ipa-sra` means what it means
/// everywhere else.
pub const NAME: &str = "ipa-sra";

/// How many times the whole thing repeats.
///
/// A round past the first reads the bodies the round before it left dead arithmetic in, so the
/// number is how many levels of the chain in the transitive case above the removal reaches. Three,
/// which is one more than the shape section 34.5 quotes needs.
const ROUNDS: usize = 3;

/// Takes out the parameters nothing reads, and the arguments the calls were passing to them.
///
/// Hands back what it did to each function it changed, one entry per function, for the manager to
/// turn into the remark a `-fopt-info` line comes from. Both sides of a removal are in that list:
/// the function that lost the parameter, and every function that lost an argument, the second of
/// those even where the sweep afterwards found nothing to take, because its call was rewritten and
/// a caller that was rewritten is a caller a verifying build should look at.
pub fn remove(
    module: &mut Module,
    graph: &CallGraph,
    names: &Interner,
    fuel: &mut Fuel,
) -> Vec<(FuncId, Stats)> {
    let closed = ipa::closed(module, graph);
    if closed.is_empty() {
        return Vec::new();
    }
    // Once, before anything moves. Taking out a parameter nothing reads does not change what a
    // function does to memory or whether it comes back, so what this says stays what it says
    // through every round below, and the sweep is its only reader.
    let mut facts = Facts::of_module(module, names);
    purity::infer(module, graph, &mut facts);
    let mut stats: HashMap<FuncId, Stats> = HashMap::new();
    for _ in 0..ROUNDS {
        let changed = round(module, &closed, fuel, &mut stats);
        if changed.is_empty() {
            break;
        }
        // The argument setup that stopped being read when the argument went. Its fuel is not this
        // pass's, because what it removes is the other half of a removal this pass already paid
        // for, and a bisection that cut here would leave a caller computing a value for a call
        // that no longer takes one.
        //
        // What it did is thrown away rather than recorded against this pass. It is a whole sweep
        // over the caller and most of what it finds in a real function at this point in the
        // pipeline is dead code that was already there, which this pass did not cause and should
        // not claim: one measurement on the SQLite amalgamation had it reporting 228 removed
        // instructions in a function where one parameter went. The caller is still entered in the
        // map, with nothing said about it, because a function whose call this rewrote is a
        // function a verifying build should be handed.
        for id in changed {
            dce::dce_in(&mut module[id], &facts, &mut Fuel::unlimited());
            stats.entry(id).or_default();
        }
    }
    // In module order rather than in the order the rounds found them, so that two builds of the
    // same module report in the same order, per spec 03.
    module.funcs().filter_map(|id| Some((id, stats.remove(&id)?))).collect()
}

/// One round, which hands back the callers whose argument lists it shortened.
///
/// The callers rather than the functions that lost a parameter, because the caller is where the
/// value that stopped being read is. A callee's body is the same body it was before.
///
/// The call sites are read once at the top and the rewriting happens under them. A function that
/// is a callee here and a caller there can therefore be read after it has already been changed,
/// and what that costs is an opportunity rather than an answer: nothing a round does adds a use,
/// so a parameter that looks read is read.
fn round(
    module: &mut Module,
    closed: &[FuncId],
    fuel: &mut Fuel,
    stats: &mut HashMap<FuncId, Stats>,
) -> Vec<FuncId> {
    let sites = ipa::sites(module, closed);
    let mut changed: Vec<FuncId> = Vec::new();
    for &id in closed {
        // One call this cannot rewrite stops the whole function, because a signature moves at
        // every call site or at none. No call at all is the case the module documentation covers.
        let Some(calls) = sites.get(&id) else { continue };
        if calls.ragged || calls.calls.is_empty() {
            continue;
        }
        let mut keep = read(&module[id]);
        if keep.iter().all(|&it| it) {
            continue;
        }
        let (mut gone, mut denied) = (0_u32, 0_u32);
        for keeping in &mut keep {
            if *keeping {
                continue;
            }
            if fuel.take() {
                gone += 1;
            } else {
                // Out of fuel, which is a request to stop transforming and not to stop looking, so
                // the rest of the list is still walked and the number of parameters that could
                // have gone is the same at every fuel setting. That is what makes a bisection over
                // it monotonic.
                *keeping = true;
                denied += 1;
            }
        }
        let counted = stats.entry(id).or_default();
        counted.record(Kind::Optimized, GONE, gone);
        counted.record(Kind::Missed, NO_FUEL, denied);
        if gone == 0 {
            continue;
        }
        let signature = trim(module[id].signature(), &keep);
        narrow(&mut module[id], signature.clone(), &keep);
        for &(caller, at) in &calls.calls {
            shorten(&mut module[caller], at, signature.clone(), &keep);
            if !changed.contains(&caller) {
                changed.push(caller);
            }
        }
    }
    changed
}

/// Which of a function's parameters have to stay, one answer per parameter in signature order.
///
/// A parameter stays when something in the body names it. [`ipa::operands`] is every value the body
/// reads, as an operand of an instruction or as an argument on an edge, and those two are the whole
/// of how a value is read in this IR, so a parameter in neither is a parameter nothing reads.
fn read(func: &Func) -> Vec<bool> {
    let Some(entry) = func.entry() else { return vec![true; func.signature().params.len()] };
    let named = ipa::operands(func);
    func[entry].params.iter().map(|value| named.contains(value)).collect()
}

/// The same signature with the parameters that are not staying taken out of it.
fn trim(signature: &Signature, keep: &[bool]) -> Signature {
    let mut trimmed = signature.clone();
    trimmed.params =
        signature.params.iter().zip(keep).filter(|&(_, &keep)| keep).map(|(&it, _)| it).collect();
    trimmed
}

/// Takes the parameters out of the function itself, signature and entry block in one breath.
///
/// The two have to say the same thing or the IR does not verify, which is the verifier's check that
/// the entry block takes what the signature says. The counter is how the mask reaches
/// [`Func::retain_params`], whose predicate is called once per parameter in the order they are in.
fn narrow(func: &mut Func, signature: Signature, keep: &[bool]) {
    let entry = func.entry().expect("a closed function has an entry block");
    func.set_signature(signature);
    let mut index = 0;
    func.retain_params(entry, |_| {
        let keeping = keep[index];
        index += 1;
        keeping
    });
}

/// Takes the arguments out of one call, and gives it the signature the callee now has.
///
/// The callee's signature rather than this call's own list trimmed the same way, because the
/// verifier asks that a direct call to a function the module holds carry that function's signature
/// exactly. The two were equal before or the module did not verify, so it is the same signature
/// either way and this is the one that is certainly right.
fn shorten(func: &mut Func, inst: Inst, signature: Signature, keep: &[bool]) {
    let Extra::Call(at) = func[inst].extra else { return };
    let args: Vec<Value> = func[func[inst].args]
        .iter()
        .zip(keep)
        .filter(|&(_, &keep)| keep)
        .map(|(&it, _)| it)
        .collect();
    let signature = func.add_signature(signature);
    let info = CallInfo { signature, ..func[at] };
    let call = func.add_call(info);
    let args = func.push_values(&args);
    func[inst].args = args;
    func[inst].extra = Extra::Call(call);
}

#[cfg(test)]
mod tests {
    use rucc_base::Symbol;
    use rucc_ir::{Builder, Def, Flags, InstData, Linkage, Opcode, Pic, Type};
    use rucc_target::{TargetInfo, Triple};

    use super::*;

    /// The type every test below uses.
    const INT: Type = Type::int(32);

    /// A module for a sixty four bit Linux, with somewhere to keep the names.
    struct Unit {
        names: Interner,
        module: Module,
        side: Symbol,
    }

    impl Unit {
        /// An empty one.
        fn new() -> Self {
            let mut names = Interner::new();
            let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
            let module = Module::new(names.intern("t.c"), &target);
            let side = names.intern("side");
            Self { names, module, side }
        }

        /// A name, which a test needs before the function itself when the body mentions it.
        fn name(&mut self, name: &str) -> Symbol {
            self.names.intern(name)
        }

        /// The name of the function this unit does not hold, for [`opaque`].
        fn side(&self) -> Symbol {
            self.side
        }

        /// Puts a function of that linkage under that signature into the module.
        ///
        /// The closure is handed the entry block parameters, in the order the signature named
        /// them, which is the order a call passes its arguments in. A return of nothing is added
        /// after it, so a body is only what the test is about.
        fn add(
            &mut self,
            name: Symbol,
            linkage: Linkage,
            signature: Signature,
            body: impl FnOnce(&mut Builder<'_>, &[Value]),
        ) {
            let params = signature.params.iter().map(|it| it.ty).collect::<Vec<Type>>();
            let mut func = Func::new(name, signature);
            func.linkage = linkage;
            let block = func.create_block();
            let values: Vec<Value> =
                params.into_iter().map(|ty| func.append_param(block, ty)).collect();
            let mut build = Builder::new(&mut func, block);
            body(&mut build, &values);
            build.ret(&[]);
            self.module.add_func(func);
        }

        /// The same, internal, which is what a `static` function in the source is.
        fn private(
            &mut self,
            name: &str,
            params: &[Type],
            body: impl FnOnce(&mut Builder<'_>, &[Value]),
        ) -> Symbol {
            let name = self.name(name);
            self.add(name, Linkage::Internal, Signature::new().with_params(params), body);
            name
        }

        /// A function outside anything the removal looks at, to put call sites in.
        fn outside(&mut self, body: impl FnOnce(&mut Builder<'_>, &[Value])) -> Symbol {
            let name = self.name("f");
            self.add(name, Linkage::External, Signature::new(), body);
            name
        }

        /// Runs the removal over it with as much fuel as it asks for.
        fn remove(&mut self) -> Vec<(FuncId, Stats)> {
            self.run(&mut Fuel::unlimited())
        }

        /// Runs the removal over it.
        fn run(&mut self, fuel: &mut Fuel) -> Vec<(FuncId, Stats)> {
            let graph = CallGraph::of(&self.module, Pic::Executable);
            remove(&mut self.module, &graph, &self.names, fuel)
        }

        /// The function of that name.
        fn func(&self, at: Symbol) -> &Func {
            let id = self.module.funcs().find(|&id| self.module[id].name == at);
            &self.module[id.expect("the module has a function of that name")]
        }

        /// What that function takes, said twice: by its signature, and by its entry block.
        ///
        /// Both, because the two saying different things is the one way this pass can leave a
        /// module that does not verify, and a test reading only one of them would not see it.
        fn shape(&self, at: Symbol) -> (Vec<Type>, Vec<Type>) {
            let func = self.func(at);
            let entry = func.entry().expect("a function with a body");
            let block = func[entry].params.iter().map(|&it| func[it].ty).collect();
            (func.signature().param_types().collect(), block)
        }

        /// The numbers the first call from one function to another hands it.
        fn passes(&self, at: Symbol, to: Symbol) -> Vec<i128> {
            let func = self.func(at);
            for block in func.blocks() {
                for inst in func.insts(block) {
                    let Extra::Call(call) = func[inst].extra else { continue };
                    if func[call].callee != Some(to) {
                        continue;
                    }
                    return func[func[inst].args].iter().map(|&it| number(func, it)).collect();
                }
            }
            panic!("the caller still has a call to that function")
        }

        /// How many instructions of that opcode the function holds.
        fn counts(&self, at: Symbol, opcode: Opcode) -> usize {
            let func = self.func(at);
            func.blocks()
                .flat_map(|block| func.insts(block))
                .filter(|&inst| func[inst].opcode == opcode)
                .count()
        }

        /// Every function in the module verifies.
        ///
        /// For this pass that is two checks above all: that the entry block takes what the
        /// signature says, and that a call to a function the module holds carries that function's
        /// signature.
        fn verified(&self) {
            for id in self.module.funcs() {
                let done = rucc_ir::verify_func(&self.module, &self.module[id], &self.names);
                assert!(done.is_ok(), "{done:?}");
            }
        }

        /// What the run said about the function of that name.
        fn said(&self, done: &[(FuncId, Stats)], at: Symbol) -> Stats {
            done.iter()
                .find(|(id, _)| self.module[*id].name == at)
                .map_or_else(Stats::new, |(_, stats)| stats.clone())
        }
    }

    /// The number an argument is, which every argument the tests below pass is one of.
    fn number(func: &Func, value: Value) -> i128 {
        let Def::Result { inst, .. } = func[value].def else {
            panic!("an argument that is a number")
        };
        let Extra::Imm(at) = func[inst].extra else { panic!("an argument that is a number") };
        func[at].signed(func[value].ty)
    }

    /// A call to a function this unit does not hold, which is a body nothing may call pure.
    ///
    /// Without one the sweep at the end of a round is free to remove the call a test is looking at,
    /// since a call whose callee touches nothing and whose results nobody reads is dead. That is
    /// the right thing for it to do and it is not what these tests are about.
    fn opaque(build: &mut Builder<'_>, side: Symbol) {
        let signature = build.func().add_signature(Signature::new());
        build.call(side, signature, &[]);
    }

    /// Calls that function with those arguments, under a signature matching what it takes.
    fn call(build: &mut Builder<'_>, at: Symbol, params: &[Type], args: &[Value]) {
        let signature = build.func().add_signature(Signature::new().with_params(params));
        build.call(at, signature, args);
    }

    #[test]
    fn a_parameter_nothing_in_the_body_reads_goes_and_so_does_the_argument() {
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.private("g", &[INT], |build, _| opaque(build, side));
        let f = unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        let done = unit.remove();
        assert_eq!(unit.said(&done, g).count(Kind::Optimized, GONE), 1);
        assert_eq!(unit.shape(g), (Vec::new(), Vec::new()));
        assert_eq!(unit.passes(f, g), Vec::<i128>::new());
        assert_eq!(unit.counts(f, Opcode::IConst), 0, "the seven was only there for the call");
        unit.verified();
    }

    #[test]
    fn a_parameter_the_body_reads_stays() {
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.private("g", &[INT], |build, params| {
            build.binary(Opcode::SDiv, params[0], params[0], Flags::NONE);
            opaque(build, side);
        });
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        assert!(unit.remove().is_empty());
        assert_eq!(unit.shape(g), (vec![INT], vec![INT]));
    }

    #[test]
    fn the_parameters_that_stay_keep_their_order_and_the_arguments_they_were_passed() {
        // The middle one of three, which is the case a mask read off the wrong end gets wrong in a
        // way that still verifies and still runs.
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.private("g", &[INT, INT, INT], |build, params| {
            build.binary(Opcode::SDiv, params[1], params[1], Flags::NONE);
            opaque(build, side);
        });
        let f = unit.outside(|build, _| {
            let one = build.iconst(INT, 1);
            let two = build.iconst(INT, 2);
            let three = build.iconst(INT, 3);
            call(build, g, &[INT, INT, INT], &[one, two, three]);
        });
        let done = unit.remove();
        assert_eq!(unit.said(&done, g).count(Kind::Optimized, GONE), 2);
        assert_eq!(unit.shape(g), (vec![INT], vec![INT]));
        assert_eq!(unit.passes(f, g), vec![2]);
        unit.verified();
    }

    #[test]
    fn a_value_the_caller_worked_out_only_for_the_argument_goes_with_it() {
        // The point of the whole thing. The parameter is what the pass removes and the arithmetic
        // behind the argument is what that removal was for.
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.private("g", &[INT], |build, _| opaque(build, side));
        let f = unit.outside(|build, _| {
            let three = build.iconst(INT, 3);
            let five = build.iconst(INT, 5);
            let product = build.binary(Opcode::Mul, three, five, Flags::NONE);
            call(build, g, &[INT], &[product]);
        });
        unit.remove();
        assert_eq!(unit.counts(f, Opcode::Mul), 0);
        assert_eq!(unit.counts(f, Opcode::IConst), 0);
    }

    #[test]
    fn the_sum_two_parameters_were_only_used_in_takes_all_three_away() {
        // Section 34.5's transitive case, quoted in the module documentation. `deep` loses its
        // parameter, so the add in `mid` is read by nothing, so the sweep takes it, so the round
        // after that reads a `mid` whose own two parameters are read by nothing either.
        let mut unit = Unit::new();
        let side = unit.side();
        let deep = unit.private("deep", &[INT], |build, _| opaque(build, side));
        let mid = unit.private("mid", &[INT, INT], |build, params| {
            let sum = build.binary(Opcode::Add, params[0], params[1], Flags::NONE);
            call(build, deep, &[INT], &[sum]);
            opaque(build, side);
        });
        let f = unit.outside(|build, _| {
            let one = build.iconst(INT, 1);
            let two = build.iconst(INT, 2);
            call(build, mid, &[INT, INT], &[one, two]);
        });
        let done = unit.remove();
        assert_eq!(unit.said(&done, deep).count(Kind::Optimized, GONE), 1);
        assert_eq!(unit.said(&done, mid).count(Kind::Optimized, GONE), 2);
        assert_eq!(unit.shape(deep), (Vec::new(), Vec::new()));
        assert_eq!(unit.shape(mid), (Vec::new(), Vec::new()));
        assert_eq!(unit.counts(mid, Opcode::Add), 0);
        assert_eq!(unit.passes(f, mid), Vec::<i128>::new());
        unit.verified();
    }

    #[test]
    fn a_function_another_object_can_call_is_left_alone() {
        // External linkage, so the call in this unit is one of its call sites and not all of them,
        // and a signature the others were compiled against is not this unit's to change.
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.name("g");
        unit.add(g, Linkage::External, Signature::new().with_params(&[INT]), |build, _| {
            opaque(build, side);
        });
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        assert!(unit.remove().is_empty());
        assert_eq!(unit.shape(g), (vec![INT], vec![INT]));
    }

    #[test]
    fn a_function_whose_address_this_unit_hands_out_is_left_alone() {
        // The address could be called through, and a call through an address is not in the list of
        // call sites there is to rewrite.
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.private("g", &[INT], |build, _| opaque(build, side));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
            let extra = Extra::Symbol(g);
            build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        });
        assert!(unit.remove().is_empty());
        assert_eq!(unit.shape(g), (vec![INT], vec![INT]));
    }

    #[test]
    fn a_function_nothing_in_the_unit_calls_is_left_alone() {
        // Unreachable, which is dead code elimination's to act on. Trimming it would be a line in
        // `-fopt-info` about a function that is not in the program.
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.private("g", &[INT], |build, _| opaque(build, side));
        assert!(unit.remove().is_empty());
        assert_eq!(unit.shape(g), (vec![INT], vec![INT]));
    }

    #[test]
    fn a_variadic_function_is_left_alone() {
        // A position past the named parameters is not a parameter, so the positions a call passes
        // and the positions the body reads are not the same list.
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.name("g");
        let mut signature = Signature::new().with_params(&[INT]);
        signature.variadic = true;
        unit.add(g, Linkage::Internal, signature, |build, _| opaque(build, side));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        assert!(unit.remove().is_empty());
        assert_eq!(unit.shape(g), (vec![INT], vec![INT]));
    }

    #[test]
    fn a_call_passing_the_wrong_number_of_arguments_stops_the_removal() {
        // A prototype disagreeing with the definition, which a translation unit may contain. That
        // call is one this cannot rewrite, and a signature moves at every call site or at none.
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.private("g", &[INT], |build, _| opaque(build, side));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
            call(build, g, &[INT, INT], &[seven, seven]);
        });
        assert!(unit.remove().is_empty());
        assert_eq!(unit.shape(g), (vec![INT], vec![INT]));
    }

    #[test]
    fn a_parameter_only_the_recursive_call_hands_on_is_left_alone() {
        // The limit the module documentation names. The parameter is read, by the call `g` makes
        // to itself, and breaking that cycle is what gcc's sweep over a component does and this
        // does not.
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.name("g");
        unit.add(g, Linkage::Internal, Signature::new().with_params(&[INT]), |build, params| {
            call(build, g, &[INT], &[params[0]]);
            opaque(build, side);
        });
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        assert!(unit.remove().is_empty());
        assert_eq!(unit.shape(g), (vec![INT], vec![INT]));
    }

    #[test]
    fn without_fuel_the_parameters_stay_and_the_chances_are_still_counted() {
        let mut unit = Unit::new();
        let side = unit.side();
        let g = unit.private("g", &[INT, INT], |build, _| opaque(build, side));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT, INT], &[seven, seven]);
        });
        let done = unit.run(&mut Fuel::of(0));
        assert_eq!(unit.said(&done, g).count(Kind::Missed, NO_FUEL), 2);
        assert_eq!(unit.said(&done, g).count(Kind::Optimized, GONE), 0);
        assert_eq!(unit.shape(g), (vec![INT, INT], vec![INT, INT]));
    }
}
