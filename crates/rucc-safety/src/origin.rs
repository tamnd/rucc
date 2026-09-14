//! One capability per pointer, taken where the pointer is made rather than where it is checked.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.2, and tamnd/rucc#1241 is where the
//! measurement that made this the first thing to fix is written down.
//!
//! The insertion pass used to put a `cap_of` in front of every check, so a function that reads
//! through a parameter and then writes through it took the same parameter's capability twice, and a
//! loop that touched one object ten times took it ten times. That was free while nothing read a
//! capability, because the checks were calls that took an address and `crate::slot` removed every
//! producer nobody read. It stops being free the moment a check reads one: on the SQLite
//! amalgamation at `-O2 -fsafety=detect` the compiler put in 23432 capabilities and 23430 of them
//! lowered to `__rucc_cap_recover`, which is a walk of the lifetime plane linear in the size of the
//! object, and the object grew by thirty per cent. A walk per access is worse than the check it was
//! meant to discharge.
//!
//! So a capability belongs to a pointer rather than to an access, and this is the table that says
//! which one each pointer has.
//!
//! # Where it goes
//!
//! At the definition of the pointer. A parameter's goes at the top of the block that declares it and
//! an instruction's goes immediately after the instruction, which is the only placement that
//! dominates every use without asking anything about the shape of the function: a definition
//! dominates its uses, so anything put directly behind a definition does too.
//!
//! It is also where the answer is going to come from once the rest of tamnd/rucc#1241 lands, which
//! is the better reason. A parameter's capability is the one the caller published and the frame it
//! comes out of is taken at the top of the function, a pointer read out of memory has its capability
//! in the aux slot beside it and the read of the two is one event at the load, and a pointer an
//! allocator returned has its capability in the header behind the call's own result. Every cheap
//! producer there is sits at the definition already. Putting the fallback there too means the
//! placement stops changing when the producer does.
//!
//! The cost of that placement is a pointer whose only check is in a branch nobody takes, which now
//! pays for its capability on the way past instead. That is the right way round: `rucc_opt`'s
//! discharge runs before any of this, so a pointer whose checks are all discharged has nobody
//! reading its capability and `crate::slot`'s prune takes the producer out again, and a pointer that
//! keeps even one check was going to pay for a capability somewhere. Paying once where it is made
//! beats paying at each place it is used.
//!
//! # The walk
//!
//! A pointer derived from another with `ptr_add` has the capability of what it was derived from,
//! because the two are in the same object and a capability is about the object. That is not only
//! cheaper, it is the answer: asking `cap_of` about an interior pointer means recovering whatever
//! object the plane says that address is in, and for a pointer that has already walked off the end
//! of its own object that is somebody else's object, which is a bounds check that passes when it
//! should refuse. Following the derivation back to its base is what makes judgement J1 about the
//! object the program named rather than about the object the address landed in.
//!
//! The walk stops at a block parameter, so it cannot run round a loop: a pointer that is
//! recomputed each time round arrives as a parameter of the loop header, and there is no way to
//! build a cycle out of instruction results in a body that is in SSA form.

use std::collections::HashMap;

use rucc_ir::{Def, Func, Inst, InstData, Opcode, Type, Value};

/// The capability each pointer in one function has.
///
/// One per call of [`crate::insert`], because the values it is keyed on are that function's.
#[derive(Debug, Default)]
pub(crate) struct Origins {
    /// What each pointer's capability is, including the derived ones that share a base's.
    held: HashMap<Value, Value>,
}

impl Origins {
    /// An empty table.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The capability of `pointer`, making one where the pointer is made if this is the first ask.
    ///
    /// `at` is the instruction the capability is wanted for, and it is only used when there is
    /// nowhere better: a pointer whose definition this cannot find a place behind gets its
    /// capability in front of the instruction that wanted it, which is where every one of them used
    /// to go and is correct rather than merely safe.
    pub(crate) fn of(&mut self, func: &mut Func, pointer: Value, at: Inst) -> Value {
        if let Some(&held) = self.held.get(&pointer) {
            return held;
        }
        let base = root(func, pointer);
        let cap = match self.held.get(&base) {
            Some(&held) => held,
            None => {
                let made = cap_of(func, base, at);
                self.held.insert(base, made);
                made
            }
        };
        // Every pointer on the chain, not only the two ends, so a walk built up a step at a time
        // asks once however many steps it took.
        let mut each = pointer;
        while each != base {
            self.held.insert(each, cap);
            let Def::Result { inst, .. } = func[each].def else { break };
            let Some(&next) = func[func[inst].args].first() else { break };
            each = next;
        }
        cap
    }
}

/// The pointer a derivation was computed from, following a chain of them to the end.
///
/// Anything that is not a `ptr_add` over a pointer is its own base, which includes a parameter, a
/// load, a call's result and a cast, and each of those is a place a later box on tamnd/rucc#1241
/// gives a producer of its own.
fn root(func: &Func, pointer: Value) -> Value {
    let mut at = pointer;
    loop {
        let Def::Result { inst, .. } = func[at].def else { return at };
        if func[inst].opcode != Opcode::PtrAdd {
            return at;
        }
        let Some(&base) = func[func[inst].args].first() else { return at };
        if !func[base].ty.is_ptr() {
            return at;
        }
        at = base;
    }
}

/// Puts a `cap_of` for `pointer` where `pointer` is defined, and gives back what it produced.
///
/// `at` is the fallback for a definition with nowhere behind it to put anything, and it is where
/// the span comes from either way, since the span a capability wants is the access the check it
/// feeds is about.
fn cap_of(func: &mut Func, pointer: Value, at: Inst) -> Value {
    let (anchor, behind) = place(func, pointer, at);
    let span = func.span(at);
    let args = func.push_values(&[pointer]);
    let data = InstData { args, ..InstData::new(Opcode::CapOf) };
    let cap = func.create_inst(data, &[Type::CAP], span);
    if behind {
        func.insert_after(cap, anchor);
    } else {
        func.insert_before(cap, anchor);
    }
    func[cap].results().next().expect("cap_of produces one value")
}

/// Which instruction a pointer's capability goes beside, and whether it goes behind it or in front.
fn place(func: &Func, pointer: Value, at: Inst) -> (Inst, bool) {
    match func[pointer].def {
        // Behind the instruction, so it lands between the definition and everything that reads it.
        // A terminator produces no pointer anything here asks about, and if one ever did there
        // would be nothing behind it in the block to go after.
        Def::Result { inst, .. } if !func.is_terminator(inst) => (inst, true),
        // In front of the first thing the block does, past the reservations. An `alloca` of a fixed
        // size belongs in the entry block and the front end writes them in a run at the top, so
        // going in above them would split that run for no reason.
        Def::Param { block, .. } => {
            let first = func.insts(block).find(|&inst| func[inst].opcode != Opcode::Alloca);
            (first.unwrap_or(at), false)
        }
        Def::Result { .. } => (at, false),
    }
}
