//! Taking out a safety check whose answer is already known.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.3, which is the first half of the
//! Tier E budget. `rucc-safety` puts a bounds check and a lifetime check in front of every access
//! and does not try to be clever about it, on purpose: a walk that inserts everything is a walk
//! anybody can read, and every check that is not needed is meant to be taken out here instead.
//! This pass takes them out, and it does the case document 07 expects to be worth the most and to
//! be the easiest to get right, which is a second access to bytes an earlier access already had
//! checked. Both checks in front of that access are the pass's business, because `rucc-safety`
//! emits the pair and taking out one of a pair is half a saving.
//!
//! # The two halves
//!
//! Section 7.7 asks for the pass and the condition to be separate things, and they are. What is in
//! this file is a walk: which check runs before which, which pointer was computed from which, and
//! how far apart two addresses are. Nothing here decides whether that is enough. The condition
//! under which a check may go is a rule in `rules/safety.rules`, a solver has to agree with it
//! before this crate finishes building, and `crate::rules::safety` is the table it compiles into.
//!
//! The split is worth the trouble because the two halves fail differently. A walk that gets the
//! context wrong is a bug of the ordinary kind, and section 14.3's differential check accounting,
//! which runs the instrumented program with every check and again with the discharged ones gone,
//! is what looks for it. A removal condition that is wrong is arithmetic that is off at the ends of
//! the type. It gives the right answer on every test anybody writes and lets one access through in
//! the one case nobody thought of, and nothing observes that until somebody exploits it.
//!
//! # What it establishes and what it asks
//!
//! Walking the dominator tree from the entry, the pass carries a set of facts. A `check_bounds`
//! that stays is a fact, because a check that passes says the bytes it was about lie inside one
//! storage instance, and a check that fails does not return. A fact is remembered as the pointer's
//! base and the constant offset from it, which is what a chain of `ptr_add` over constants comes
//! to, plus how many bytes the access covers.
//!
//! At the next `check_bounds`, the pointer is normalized the same way. When a fact shares its base,
//! the distance between the two accesses is the difference of the two offsets, and that is a number
//! this pass has rather than a claim it makes: both addresses are the same value plus a constant.
//! The question of whether the later bytes are inside the earlier ones is then handed to the table,
//! which answers it in sixty four bit arithmetic rather than in the offsets, and the check goes
//! only if the answer is yes.
//!
//! A check whose extent is an operand is left out of all of this, in both directions. Section 7.4's
//! hoisted check covers as many bytes as its loop runs times, and every range compared here is a
//! pair of numbers, so such a check is neither read as a fact nor asked about. Reading its payload
//! would be worse than skipping it, since the size there is one element of the walk rather than the
//! range the check is about, and a fact recorded from it would be smaller than the truth in one
//! direction and a question asked from it smaller in the other.
//!
//! The capability operand has to be the `cap_of` of the check's own pointer, which is the shape
//! `rucc-safety` emits and the shape the argument needs. The check being removed asks whether its
//! bytes are inside the instance that owns its own pointer, its pointer is inside the range the
//! earlier check established, and that range is inside one instance, so the answer is yes. A check
//! whose capability came from somewhere else is asking about a different instance and is left
//! alone. Nothing is required of the earlier check's capability, because all that is used of it is
//! that the check passed, and a check that passed put its bytes inside one instance whatever
//! capability it named.
//!
//! # The fact nobody had to check for
//!
//! Section 7.2 lists four sources of a discharge and puts the frontend first, because the majority
//! of accesses in real C are to a local at a constant offset and the bounds of a local are not
//! something anybody has to find out. An `alloca` of a fixed size makes one storage instance of
//! that many bytes and says so in its payload, so the range from its address to that many further
//! along is inside one instance for exactly the reason a passing `check_bounds` says its own range
//! is. When the address a check is about normalizes to such an `alloca`, that range is the fact,
//! and the question put to the table is the same question with the same rule answering it.
//!
//! Two things make it worth more than a fact a check established. It is there before anything has
//! run, so the first access to a local is discharged rather than only the second. And no call takes
//! it away: a callee cannot free a frame slot, whatever it does to whatever the slot points at, so
//! this fact is asked separately rather than kept in the set the walk throws away at the first call
//! it cannot see through.
//!
//! Only the fixed size form. A variable length array is an `alloca` with an operand and a payload
//! whose size field reads zero, and reading it anyway would discharge every check in the array.
//!
//! # The lifetime half, and what it borrows from the other one
//!
//! A `check_live` that stays is a fact too, and a smaller one than it looks: it says the storage
//! instance holding its own address is alive, and it says nothing about the address four bytes
//! along, because that address might be in a different instance. On its own that fact discharges
//! only a second lifetime check of the very same address, and the shape `rucc-safety` emits is a
//! lifetime check per field rather than per object, so on its own it would almost never fire.
//!
//! What makes it fire is the bounds fact sitting next to it. A `check_bounds` that passed put its
//! whole range inside one instance, so if the lifetime check's address is in that range, the
//! instance that was found alive is the instance the whole range is in, and the whole range is
//! alive. So a lifetime fact is recorded as the widest checked range containing its address, and a
//! later lifetime check is asked about as a single byte. The question of whether that byte is in
//! that range is the same question the bounds half asks, put to the same rule.
//!
//! The order the two arrive in is what makes this work rather than a coincidence to be careful
//! about: `rucc-safety` emits the bounds check first and the lifetime check second, so the range is
//! established by the time there is a lifetime fact to widen. A lifetime check that arrives with no
//! range around it keeps the narrow fact, which is correct and worth little.
//!
//! # Why a call throws the facts away, and which calls do not
//!
//! Section 7.3 says nothing kills a bounds fact except a redefinition of the capability, which in
//! SSA is never, and this pass is stricter than that: a call, or anything else this pass cannot see
//! through, drops every fact it is carrying.
//!
//! The case is a `free` and then an allocation of something smaller at the same address. The range
//! established before the call is no longer inside one instance after it, and what document 07
//! leaves that to is the lifetime judgement rather than this one. Today's lifetime check is about
//! the address rather than about the version the capability was taken at, so it would not refuse
//! the access either, and a rate this pass reports is worth less than a hole it opens. The strict
//! version is what is written first.
//!
//! A `meta_end` and a `meta_transfer` drop the facts as well. Nothing emits either one yet, so
//! this costs nothing today and is the difference between conservative and wrong on the day the
//! instrumentation starts ending lifetimes. `crate::nofree` treats them the same way.
//!
//! A call that says it reaches nothing which can free is the exception, and it is not this pass
//! being trusting. `crate::nofree` works the answer out over the whole module before the pipeline
//! starts and writes it onto the call site as [`Flags::NOFREE`], because the fact belongs to the
//! callee and a pass is given one function. Reading it here is reading what the IR says, the same
//! way the pass reads an opcode. Nothing else about a call is believed: the facts still go across
//! an unmarked call, a call through an address, and inline assembly.
//!
//! What the strictness still costs is measured rather than guessed. A check that a fact would have
//! covered if a call had not intervened is counted, so `-fopt-info-missed` says per function what
//! is left to win.

use rucc_ir::{Def, Extra, Flags, Func, Inst, Opcode, Value};

use crate::rules::{Piece, Subject, Table, safety};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// Recorded once for each bounds check taken out.
const REMOVED: &str = "bounds check removed, a dominating check covers the same bytes";

/// Recorded once for each bounds check taken out because it was inside a local.
const REMOVED_LOCAL: &str = "bounds check removed, its bytes are inside a local this function \
                             declares";

/// Recorded once for each lifetime check taken out.
const REMOVED_LIVE: &str = "lifetime check removed, a dominating check covers the same storage";

/// Recorded for a bounds check that would have gone if there had been fuel for it.
const NO_FUEL: &str = "bounds check kept, the pass ran out of fuel";

/// Recorded for a lifetime check that would have gone if there had been fuel for it.
const NO_FUEL_LIVE: &str = "lifetime check kept, the pass ran out of fuel";

/// Recorded for a bounds check a call cost, which is the honest price of the paragraph above.
///
/// This one is worth reading rather than skipping. It is the number of checks that are still being
/// paid for because `crate::nofree` could not vouch for a call, so it says per function what the
/// rest of section 7.5's summary work would be worth before anybody writes it.
const PAST_A_CALL: &str =
    "bounds check kept, a call between it and the check that covers it might free";

/// The same, for a lifetime check. Section 8.8 is about this number rather than the one above.
const PAST_A_CALL_LIVE: &str =
    "lifetime check kept, a call between it and the check that covers it might free";

/// Recorded for a bounds check whose operands this pass cannot read.
const UNKNOWN_SHAPE: &str = "bounds check left alone, its pointer is not a base and a constant";

/// Recorded for a bounds check about a range the program worked out.
const COMPUTED_EXTENT: &str =
    "bounds check left alone, how many bytes it covers is a number only the program has";

/// Recorded for a lifetime check whose operands this pass cannot read.
const UNKNOWN_SHAPE_LIVE: &str =
    "lifetime check left alone, its pointer is not a base and a constant";

/// The pass. It holds nothing, because everything it works out is about one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discharge;

impl Pass for Discharge {
    fn name(&self) -> &'static str {
        "discharge"
    }

    fn describe(&self) -> &'static str {
        "a bounds or lifetime check a dominating check already covered is removed"
    }

    fn preserves(&self) -> Preserved {
        // Instructions go and blocks do not. A check is not a terminator and removing one leaves
        // every edge where it was.
        Preserved::ALL
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(entry) = func.entry() else { return stats };
        let dom = an.dominators(func).clone();

        // The walk is a stack rather than recursion because the dominator tree of a long chain of
        // blocks is as deep as the function is long, and a pass is not a place to find that out.
        // Each block carries its own copy of what holds at its start, which is what makes a fact a
        // call killed in one arm of a branch still hold in the other.
        let mut going: Vec<(Inst, &'static str)> = Vec::new();
        let mut work = vec![(entry, Scope::default())];
        while let Some((block, mut scope)) = work.pop() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                if opaque(func, inst) {
                    scope.forget();
                    continue;
                }
                match func[inst].opcode {
                    Opcode::CheckBounds => {
                        if func[func[inst].args].len() > 2 {
                            stats.missed(COMPUTED_EXTENT);
                            continue;
                        }
                        let Some(asked) = about(func, inst) else {
                            stats.missed(UNKNOWN_SHAPE);
                            continue;
                        };
                        let inside =
                            declared(func, asked.base).is_some_and(|local| covers(&local, &asked));
                        if !inside && !scope.bounds.covers(&asked) {
                            if scope.bounds.covered_before(&asked) {
                                stats.missed(PAST_A_CALL);
                            }
                            // A check that stays is a check that runs, and a check that runs
                            // establishes what it was about. One that was removed establishes
                            // nothing new: whatever covered it covers everything it would have.
                            scope.bounds.held.push(asked);
                            continue;
                        }
                        if !fuel.take() {
                            stats.missed(NO_FUEL);
                            scope.bounds.held.push(asked);
                            continue;
                        }
                        going.push((inst, if inside { REMOVED_LOCAL } else { REMOVED }));
                    }
                    Opcode::CheckLive => {
                        let Some(asked) = alive(func, inst) else {
                            stats.missed(UNKNOWN_SHAPE_LIVE);
                            continue;
                        };
                        if !scope.alive.covers(&asked) {
                            if scope.alive.covered_before(&asked) {
                                stats.missed(PAST_A_CALL_LIVE);
                            }
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                            continue;
                        }
                        if !fuel.take() {
                            stats.missed(NO_FUEL_LIVE);
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                            continue;
                        }
                        going.push((inst, REMOVED_LIVE));
                    }
                    _ => continue,
                }
            }
            for child in dom.children(block) {
                work.push((child, scope.clone()));
            }
        }

        for (inst, why) in going {
            func.remove_inst(inst);
            stats.optimized(why);
        }
        stats
    }
}

/// A range of bytes some check has already been passed on, or is being asked about.
///
/// The address is kept as the value it was computed from and the constant distance from it, rather
/// than as the pointer itself, because that is what makes two of these comparable: the whole of
/// what this pass knows about two addresses is that they are one value plus two constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fact {
    /// The value the address was computed from.
    base: Value,
    /// How far past it the access starts.
    offset: i128,
    /// How many bytes it covers.
    size: i128,
}

/// One kind of fact, and what has become of it.
#[derive(Debug, Clone, Default)]
struct Known {
    /// The ranges a check has been passed on and nothing has cast doubt on since.
    held: Vec<Fact>,
    /// The ones a call threw away, kept only so that the cost of throwing them away is a number
    /// somebody can read rather than a paragraph somebody has to believe.
    lost: Vec<Fact>,
}

impl Known {
    /// Whether something still standing answers this.
    fn covers(&self, asked: &Fact) -> bool {
        self.held.iter().any(|fact| covers(fact, asked))
    }

    /// Whether something would have answered it before a call came along.
    fn covered_before(&self, asked: &Fact) -> bool {
        self.lost.iter().any(|fact| covers(fact, asked))
    }

    /// Gives up everything, because something happened that this pass cannot see through.
    fn forget(&mut self) {
        self.lost.append(&mut self.held);
    }
}

/// What holds where the walk has got to.
///
/// The two kinds are apart because they are killed together and answered separately: a range being
/// inside one instance and that instance being alive are different claims, and reporting them as
/// one number would hide which of the two a check is still being paid for.
#[derive(Debug, Clone, Default)]
struct Scope {
    /// Ranges a `check_bounds` established are inside one storage instance.
    bounds: Known,
    /// Ranges a `check_live` established are in an instance that is alive.
    alive: Known,
}

impl Scope {
    /// Gives up every fact of either kind.
    fn forget(&mut self) {
        self.bounds.forget();
        self.alive.forget();
    }
}

/// Whether this instruction could do something to memory that this pass cannot account for.
///
/// A call is the whole of it, in every spelling, and inline assembly with it. A `tail_call` ends
/// the block and there is nothing after it to protect, and it is here anyway so that the reason a
/// fact survives is never that the walk did not think of something.
///
/// A call carrying [`Flags::NOFREE`] reaches nothing that ends a lifetime, so there is nothing for
/// it to have done to the bytes an earlier check was passed on. `crate::nofree` is what put the
/// flag there and what argues for it.
///
/// A `meta_end` and a `meta_transfer` end a lifetime by saying so, which is the plainest way for a
/// fact to stop being true, and neither is emitted today.
fn opaque(func: &Func, inst: Inst) -> bool {
    match func[inst].opcode {
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => {
            !func[inst].flags.contains(Flags::NOFREE)
        }
        Opcode::InlineAsm | Opcode::MetaEnd | Opcode::MetaTransfer => true,
        _ => false,
    }
}

/// What a `check_bounds` is about, when it is one this pass can read.
fn about(func: &Func, check: Inst) -> Option<Fact> {
    let (base, offset) = addressed(func, check)?;
    let Extra::Mem(info) = func[check].extra else { return None };
    Some(Fact { base, offset, size: i128::from(func[info].size) })
}

/// What a `check_live` is about, when it is one this pass can read.
///
/// One byte, because that is the whole of what the check says: the instance holding this address
/// is alive, and nothing about the address next door. The widening to a range that makes the fact
/// useful is [`widened`], and it needs a bounds fact to do it.
fn alive(func: &Func, check: Inst) -> Option<Fact> {
    let (base, offset) = addressed(func, check)?;
    Some(Fact { base, offset, size: 1 })
}

/// The address a check is about, as a base and a constant.
///
/// The capability has to be the `cap_of` of the check's own pointer. That is the shape
/// `rucc-safety` emits and it is what the removal argument in the module comment needs, so a check
/// that does not have it is not a check this pass has anything to say about.
fn addressed(func: &Func, check: Inst) -> Option<(Value, i128)> {
    let args = &func[func[check].args];
    let &capability = args.first()?;
    let &pointer = args.get(1)?;
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(pointer) {
        return None;
    }
    Some(normal(func, pointer))
}

/// The object a local is, when the address a check is about was computed from one.
///
/// This is the fact nobody had to check for, and section 7.2 puts it first of the four sources
/// because it is where most of the win is. An `alloca` of a fixed size is one storage instance of
/// that many bytes, said by the instruction that makes it rather than by a check that passed, so
/// the bytes from its address to that many further along are inside one instance for the same
/// reason a passing `check_bounds` says its own range is.
///
/// Only the fixed size form. The one that takes an operand is a variable length array, and how
/// many bytes it is is a value the program works out rather than a number in the payload, where
/// the field reads zero.
///
/// The fact holds everywhere in the function and no call takes it away, which is the other half of
/// what makes it worth having. A callee cannot free a frame slot: what it could free is whatever a
/// pointer stored in the slot points at, and that is a different instance and a different check.
/// So this is asked separately from the facts the walk carries rather than pushed into them, since
/// everything in there is thrown away at the first call this pass cannot see through.
fn declared(func: &Func, base: Value) -> Option<Fact> {
    let Def::Result { inst, .. } = func[base].def else { return None };
    if func[inst].opcode != Opcode::Alloca || !func[func[inst].args].is_empty() {
        return None;
    }
    let Extra::Mem(info) = func[inst].extra else { return None };
    Some(Fact { base, offset: 0, size: i128::from(func[info].size) })
}

/// A lifetime fact grown from one address to the checked range it sits in.
///
/// The argument is in the module comment: a `check_bounds` that passed put its whole range inside
/// one instance, so the instance this lifetime check found alive is the instance that range is in.
/// With no range around the address the fact stays as it came, which is correct and answers only a
/// repeat of the very same check.
///
/// A local is asked about first, because the object it is is the widest range there can be for an
/// address computed from it and a wider fact answers more later checks. What that gives is a
/// lifetime check anywhere in a local discharging every later one in the same local, up to the
/// first call, which is the shape a function that reads several fields of a local struct has.
fn widened(func: &Func, bounds: &Known, asked: Fact) -> Fact {
    if let Some(local) = declared(func, asked.base).filter(|local| covers(local, &asked)) {
        return local;
    }
    bounds.held.iter().find(|fact| covers(fact, &asked)).copied().unwrap_or(asked)
}

/// The value an address was computed from, and how far past it the address is.
///
/// A `ptr_add` over a constant is walked through, and anything else is where the answer stops. The
/// arithmetic here is exact because it is done in `i128` over offsets that came out of the IR as
/// sixty four bit constants, and whether it is small enough to mean anything at sixty four bits is
/// the rule's question rather than this function's.
fn normal(func: &Func, value: Value) -> (Value, i128) {
    let mut base = value;
    let mut offset: i128 = 0;
    while let Some((from, step)) = walked(func, base) {
        let Some(sum) = offset.checked_add(step) else { break };
        base = from;
        offset = sum;
    }
    (base, offset)
}

/// The pointer one `ptr_add` over a constant was computed from, and by how much.
fn walked(func: &Func, value: Value) -> Option<(Value, i128)> {
    let from = operand_of(func, value, Opcode::PtrAdd, 0)?;
    let by = operand_of(func, value, Opcode::PtrAdd, 1)?;
    Some((from, constant(func, by)?))
}

/// Operand `index` of the instruction that produced `value`, when that instruction is `opcode`.
pub(crate) fn operand_of(func: &Func, value: Value, opcode: Opcode, index: usize) -> Option<Value> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != opcode {
        return None;
    }
    func[func[inst].args].get(index).copied()
}

/// The value of an integer constant, read with its own sign.
fn constant(func: &Func, value: Value) -> Option<i128> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(imm) = func[inst].extra else { return None };
    let ty = func[value].ty;
    ty.is_int().then(|| func[imm].signed(ty))
}

/// Whether an established fact answers the check being asked about.
///
/// This function decides nothing. It puts the two together into the term the rule file is written
/// about and asks the table, which is the whole of section 7.7's split: the paragraph above worked
/// out that the two addresses are one value a constant apart, and whether that is enough is
/// somebody's proof rather than this file's opinion.
fn covers(fact: &Fact, asked: &Fact) -> bool {
    if fact.base != asked.base {
        return false;
    }
    let Some(delta) = asked.offset.checked_sub(fact.offset) else { return false };
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let span = question.number(fact.size);
    let span = question.app("iconst.i64", &[span]);
    let far = question.number(delta);
    let far = question.app("iconst.i64", &[far]);
    let reach = question.number(asked.size);
    let reach = question.app("iconst.i64", &[reach]);
    let term = question.app("covered.i64", &[at, span, far, reach]);
    match safety::TABLE.find(&question, term) {
        Some(found) => yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// Whether the rule that fired answers yes.
///
/// A discharge rule replaces the question with a constant, and one is yes. Every rule in the file
/// answers that today, and reading it off the rule rather than assuming it is what keeps this
/// honest on the day one of them answers something else.
pub(crate) fn yes(table: &Table, rule: usize) -> bool {
    matches!(table.rules[rule].replacement, [Piece::App { .. }, Piece::Int(1)])
}

/// A term built to be asked about, and nothing else.
///
/// The rules are matched against this rather than against the function, because what is being asked
/// about is not in the function: it is what the walk worked out about two of its instructions. So
/// the subject is a small arena of exactly the term being asked, built fresh for each question and
/// thrown away with the answer.
#[derive(Debug, Default)]
pub(crate) struct Question {
    held: Vec<Held>,
}

/// One node of that term.
#[derive(Debug)]
enum Held {
    /// A number the pattern can read and a guard can be about.
    Int(i128),
    /// A head and its arguments.
    App(&'static str, Vec<usize>),
    /// Something with no structure, which is how an address the rule only names is written.
    Opaque,
}

impl Question {
    /// Adds a constant and gives back where it went.
    ///
    /// Named for what it adds rather than for what it holds, because the arena also answers
    /// [`Subject::int`] and one name for the two would read as though building a term and asking
    /// about one were the same act.
    pub(crate) fn number(&mut self, value: i128) -> usize {
        self.held.push(Held::Int(value));
        self.held.len() - 1
    }

    /// Adds an application of `head` to what is already in the arena.
    pub(crate) fn app(&mut self, head: &'static str, args: &[usize]) -> usize {
        self.held.push(Held::App(head, args.to_vec()));
        self.held.len() - 1
    }

    /// Adds something the rule can bind and cannot look inside.
    pub(crate) fn opaque(&mut self) -> usize {
        self.held.push(Held::Opaque);
        self.held.len() - 1
    }
}

impl Subject for Question {
    type Node = usize;

    fn head(&self, node: usize) -> Option<(&str, usize)> {
        match &self.held[node] {
            Held::App(head, args) => Some((head, args.len())),
            Held::Int(_) | Held::Opaque => None,
        }
    }

    fn arg(&self, node: usize, index: usize) -> usize {
        match &self.held[node] {
            Held::App(_, args) => args[index],
            // The walk only asks for an argument `head` said was there, so this is unreachable
            // rather than a case with an answer.
            Held::Int(_) | Held::Opaque => unreachable!("only an application has arguments"),
        }
    }

    fn int(&self, node: usize) -> Option<i128> {
        match self.held[node] {
            Held::Int(value) => Some(value),
            Held::App(..) | Held::Opaque => None,
        }
    }

    fn same(&self, a: usize, b: usize) -> bool {
        // Every node of a question is written once, so two places holding one thing are one place.
        a == b
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        AsmInfo, Block, BlockCallList, Builder, Extra, Flags, Func, InstData, MemInfo, MemOrder,
        Opcode, Restrict, Signature, Type, Value,
    };

    use super::{Discharge, Fact};
    use crate::stats::Kind;
    use crate::{Analyses, Fuel, Pass};

    /// A function taking a pointer, with one block, ready to have accesses put in it.
    fn blank() -> (Interner, Func, Block, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR]));
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        (names, func, block, pointer)
    }

    /// Puts `cap_of` and a `check_bounds` over `size` bytes at `pointer` into a block.
    ///
    /// The same shape `rucc-safety` emits, written out here rather than reached for, because
    /// `rucc-opt` is rank 9 alongside `rucc-safety` and cannot depend on it.
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
    ///
    /// `rucc-safety` emits this straight after the bounds check for the same access and shares the
    /// one `cap_of` between the two. Sharing it is not what the pass reads, so the tests build a
    /// second one, which is the harder shape for it to accept.
    fn live(build: &mut Builder<'_>, pointer: Value) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = build.func().push_values(&[capability, pointer]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckLive) }, &[]);
    }

    /// Both checks in front of one access, in the order `rucc-safety` writes them.
    fn access(build: &mut Builder<'_>, pointer: Value, size: u64) {
        check(build, pointer, size);
        live(build, pointer);
    }

    /// A pointer `bytes` past another one.
    fn past(build: &mut Builder<'_>, pointer: Value, bytes: i128) -> Value {
        let offset = build.iconst(Type::int(64), bytes);
        let args = build.func().push_values(&[pointer, offset]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    /// How many checks are left in a function.
    fn checks(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckBounds)
            .count()
    }

    /// How many lifetime checks are left in a function.
    fn lives(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckLive)
            .count()
    }

    fn run(func: &mut Func) -> crate::Stats {
        Discharge.run(func, &mut Analyses::new(), &mut Fuel::unlimited())
    }

    #[test]
    fn a_second_check_of_the_same_bytes_goes() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
    }

    #[test]
    fn a_check_over_a_length_the_program_worked_out_is_not_this_pass_to_read() {
        // Section 7.4's hoisted check covers as many bytes as its loop runs times, which is a value
        // and not a number. Every range this pass compares is a pair of numbers, so it says so and
        // leaves the check alone rather than reading the payload, whose size is one element.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let bytes = build.iconst(Type::int(64), 4);
        let info = MemInfo {
            size: 4,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        let args = build.func().push_values(&[capability, pointer, bytes]);
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
        build.ret(&[]);

        let stats = run(&mut func);
        assert_eq!(checks(&func), 2, "the second one stays");
        assert_eq!(stats.count(Kind::Missed, super::COMPUTED_EXTENT), 1);
    }

    #[test]
    fn a_check_of_bytes_inside_a_checked_range_goes() {
        // Four bytes at offset four, inside sixteen bytes at offset zero. This is the shape the
        // whole pass is for: a struct whose fields are read one after another through one pointer.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 4);
        check(&mut build, field, 4);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(checks(&func), 1);
    }

    #[test]
    fn a_check_of_bytes_past_the_end_of_a_checked_range_stays() {
        // Four bytes at offset fourteen is two bytes past the end of the sixteen that were
        // checked, and those two bytes are what the check is for.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let over = past(&mut build, pointer, 14);
        check(&mut build, over, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_of_bytes_before_a_checked_range_stays() {
        // The guard's `delta` is not negative, and this is why. A read four bytes below what was
        // checked is a read of somebody else's memory, and it is the bug the check exists for.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let under = past(&mut build, pointer, -4);
        check(&mut build, under, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_through_a_pointer_nothing_relates_to_the_first_stays() {
        let mut names = Interner::new();
        let name = names.intern("two");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR, Type::PTR]));
        let block = func.create_block();
        let one = func.append_param(block, Type::PTR);
        let other = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        check(&mut build, one, 16);
        check(&mut build, other, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_a_call_stands_between_stays_and_is_counted() {
        // The conservatism the module comment argues for, and the number that says what it costs.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 1);
    }

    #[test]
    fn a_check_a_call_that_cannot_free_stands_between_goes() {
        // The other side of the paragraph above. The summary said this call reaches nothing that
        // ends a lifetime, so the range the first check established is still one range.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let callee = names.intern("counts_them");
        let signature = build.func().add_signature(Signature::new());
        let call = build.call(callee, signature, &[]);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        func[call].flags |= Flags::NOFREE;
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 0);
    }

    #[test]
    fn inline_assembly_throws_the_facts_away_whatever_it_is_flagged() {
        // There is no flag that would make this safe. The template is text the compiler does not
        // read, so nothing worked anything out about what it reaches.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        build.inline_asm(
            AsmInfo {
                template: names.intern("nop"),
                constraints: names.intern(""),
                clobbers: names.intern(""),
                targets: BlockCallList::EMPTY,
            },
            &[],
            &[],
            Flags::NONE,
        );
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 1);
    }

    #[test]
    fn a_check_that_only_one_path_covers_stays() {
        // The dominator tree is what makes this right. The check in the arm covers the one in the
        // join on one path and not on the other, and a check that goes has to be one that ran.
        let (_, mut func, block, pointer) = blank();
        let arm = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, arm, &[], join, &[]);
        let mut build = Builder::new(&mut func, arm);
        check(&mut build, pointer, 16);
        build.jump(join, &[]);
        let mut build = Builder::new(&mut func, join);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_a_dominating_block_covers_goes() {
        let (_, mut func, block, pointer) = blank();
        let after = func.create_block();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        build.jump(after, &[]);
        let mut build = Builder::new(&mut func, after);
        let field = past(&mut build, pointer, 8);
        check(&mut build, field, 8);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(checks(&func), 1);
    }

    #[test]
    fn fuel_stops_the_removing_and_not_the_looking() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let mut fuel = Fuel::of(1);
        let stats = Discharge.run(&mut func, &mut Analyses::new(), &mut fuel);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
    }

    #[test]
    fn a_second_lifetime_check_of_the_same_address_goes() {
        // The narrow fact on its own, with no range around it to widen into.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        live(&mut build, pointer);
        live(&mut build, pointer);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_inside_a_checked_range_goes() {
        // The shape the pass is for, with both halves of it. Sixteen bytes are checked and found
        // alive, then a field four bytes in is read, and neither check in front of it survives.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 4);
        access(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_outside_every_checked_range_stays() {
        // Four bytes at offset twenty are past the sixteen that were checked, so nothing says the
        // address is in the instance that was found alive, and it might be in no instance at all.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let over = past(&mut build, pointer, 20);
        live(&mut build, over);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(lives(&func), 2);
    }

    #[test]
    fn a_lifetime_check_with_no_range_around_it_does_not_widen() {
        // Without the bounds check the first lifetime check speaks only for its own address, so
        // the one four bytes along is a different question and stays.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        live(&mut build, pointer);
        let field = past(&mut build, pointer, 4);
        live(&mut build, field);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(lives(&func), 2);
    }

    #[test]
    fn a_lifetime_check_a_call_stands_between_stays_and_is_counted() {
        // Section 8.8's number. This is the one the summaries were written for.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        let field = past(&mut build, pointer, 4);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_a_call_that_cannot_free_stands_between_goes() {
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let callee = names.intern("counts_them");
        let signature = build.func().add_signature(Signature::new());
        let call = build.call(callee, signature, &[]);
        let field = past(&mut build, pointer, 4);
        live(&mut build, field);
        build.ret(&[]);
        func[call].flags |= Flags::NOFREE;
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn ending_a_lifetime_throws_the_facts_away() {
        // Nothing emits `meta_end` yet, so this is the test that says what will happen when
        // something does, rather than a test of anything the compiler does today.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let size = build.iconst(Type::int(64), 16);
        let args = build.func().push_values(&[pointer, size]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaEnd) }, &[]);
        access(&mut build, pointer, 16);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(checks(&func), 2);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 1);
    }

    #[test]
    fn fuel_runs_out_over_both_kinds_of_check() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        let mut fuel = Fuel::of(1);
        let stats = Discharge.run(&mut func, &mut Analyses::new(), &mut fuel);
        assert_eq!(checks(&func), 1);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_LIVE), 1);
    }

    #[test]
    fn a_distance_too_large_to_be_a_real_access_is_not_discharged() {
        // The guard's bound. The two readings of the arithmetic agree while the numbers stay
        // small, so a rule proved at sixty four bits is not asked about anything else. Nothing
        // here is wrong, it simply is not proved, and a check that is not proved to be unnecessary
        // stays.
        let huge = i128::from(u64::MAX) * 4;
        let fact = Fact { base: Value::new(0), offset: 0, size: huge };
        let asked = Fact { base: Value::new(0), offset: huge / 2, size: 4 };
        assert!(!super::covers(&fact, &asked));
    }

    /// A stack slot of `size` bytes, in the entry block where the verifier wants one.
    fn local(build: &mut Builder<'_>, size: u64) -> Value {
        let info = MemInfo {
            size,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        build.value(InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// A stack slot whose size the program works out, which is what a variable length array is.
    fn growable(build: &mut Builder<'_>, size: Value) -> Value {
        let info = MemInfo {
            size: 0,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        let args = build.func().push_values(&[size]);
        build.value(InstData { args, extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    #[test]
    fn a_check_of_bytes_inside_a_local_goes_with_nothing_in_front_of_it() {
        // Section 7.2's first source. No check established this and none had to: an `alloca` of
        // sixteen bytes is sixteen bytes of one storage instance because that is what it makes.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 8);
        check(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 1);
    }

    #[test]
    fn a_check_past_the_end_of_a_local_stays() {
        // The slot is sixteen bytes and the access runs to twenty. Nothing about it being a local
        // says anything about the four bytes after it, which belong to whatever the frame puts
        // there next.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 16);
        check(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 0);
    }

    #[test]
    fn a_check_of_bytes_inside_a_local_goes_across_a_call() {
        // The other half of what makes the fact worth having. A callee cannot free a frame slot,
        // so unlike everything the walk carries this one is not thrown away at a call.
        let (mut names, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        check(&mut build, slot, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 0);
    }

    #[test]
    fn a_check_inside_a_variable_length_array_stays() {
        // How many bytes it is is a value the program works out, and the payload's size field
        // reads zero. A pass that read it anyway would discharge every check in the array.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let bytes = build.iconst(Type::int(64), 64);
        let slot = growable(&mut build, bytes);
        check(&mut build, slot, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 0);
    }

    #[test]
    fn a_lifetime_check_in_a_local_widens_to_the_whole_local() {
        // The widening the module comment argues for, with the local standing in for the checked
        // range. The first lifetime check found the instance holding the slot alive, the slot is
        // one instance, so the second one anywhere in it is asking a question already answered.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 12);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_past_the_end_of_a_local_stays() {
        // The widening stops where the slot does, so an address outside it is a different
        // instance and a question nothing has answered.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 24);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 0);
    }
}
