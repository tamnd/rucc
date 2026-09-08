//! Taking out a safety check whose answer is already known.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.3, which is the first half of the
//! Tier E budget. `rucc-safety` puts a bounds check and a lifetime check in front of every access
//! and does not try to be clever about it, on purpose: a walk that inserts everything is a walk
//! anybody can read, and every check that is not needed is meant to be taken out here instead.
//! This pass is the beginning of taking them out, and it does the case document 07 expects to be
//! worth the most and to be the easiest to get right, which is a second access to bytes an earlier
//! access already had checked.
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
//! The capability operand has to be the `cap_of` of the check's own pointer, which is the shape
//! `rucc-safety` emits and the shape the argument needs. The check being removed asks whether its
//! bytes are inside the instance that owns its own pointer, its pointer is inside the range the
//! earlier check established, and that range is inside one instance, so the answer is yes. A check
//! whose capability came from somewhere else is asking about a different instance and is left
//! alone. Nothing is required of the earlier check's capability, because all that is used of it is
//! that the check passed, and a check that passed put its bytes inside one instance whatever
//! capability it named.
//!
//! # Why a call throws the facts away
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
//! What that costs is measured rather than guessed. A check that a fact would have covered if a
//! call had not intervened is counted, so `-fopt-info-missed` says per function what the kill is
//! worth, and it is the same number the `nofree` summaries on milestone S4's list would buy back.

use rucc_ir::{Def, Extra, Func, Inst, Opcode, Value};

use crate::rules::{Piece, Subject, Table, safety};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// Recorded once for each check taken out.
const REMOVED: &str = "bounds check removed, a dominating check covers the same bytes";

/// Recorded for a check that would have gone if there had been fuel for it.
const NO_FUEL: &str = "bounds check kept, the pass ran out of fuel";

/// Recorded for a check a call cost, which is the honest price of the paragraph above.
///
/// This one is worth reading rather than skipping. It is the number of checks that would have gone
/// if the pass trusted a call not to free, which is what an interprocedural `nofree` summary would
/// establish, so it says per function what that piece of work is worth before anybody writes it.
const PAST_A_CALL: &str =
    "bounds check kept, a call between it and the check that covers it might free";

/// Recorded for a check whose operands this pass cannot read.
const UNKNOWN_SHAPE: &str = "bounds check left alone, its pointer is not a base and a constant";

/// The pass. It holds nothing, because everything it works out is about one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discharge;

impl Pass for Discharge {
    fn name(&self) -> &'static str {
        "discharge"
    }

    fn describe(&self) -> &'static str {
        "a bounds check on bytes a dominating check already covered is removed"
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
        let mut going: Vec<Inst> = Vec::new();
        let mut work = vec![(entry, Scope::default())];
        while let Some((block, mut scope)) = work.pop() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                if opaque(func, inst) {
                    scope.forget();
                    continue;
                }
                if func[inst].opcode != Opcode::CheckBounds {
                    continue;
                }
                let Some(asked) = about(func, inst) else {
                    stats.missed(UNKNOWN_SHAPE);
                    continue;
                };
                if !scope.established.iter().any(|fact| covers(fact, &asked)) {
                    if scope.lost.iter().any(|fact| covers(fact, &asked)) {
                        stats.missed(PAST_A_CALL);
                    }
                    // A check that stays is a check that runs, and a check that runs establishes
                    // what it was about. One that was removed establishes nothing new: whatever
                    // covered it covers everything it would have.
                    scope.established.push(asked);
                    continue;
                }
                if !fuel.take() {
                    stats.missed(NO_FUEL);
                    scope.established.push(asked);
                    continue;
                }
                going.push(inst);
            }
            for child in dom.children(block) {
                work.push((child, scope.clone()));
            }
        }

        for inst in going {
            func.remove_inst(inst);
            stats.optimized(REMOVED);
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

/// What holds where the walk has got to.
#[derive(Debug, Clone, Default)]
struct Scope {
    /// The ranges a check has been passed on and nothing has cast doubt on since.
    established: Vec<Fact>,
    /// The ones a call threw away, kept only so that the cost of throwing them away is a number
    /// somebody can read rather than a paragraph somebody has to believe.
    lost: Vec<Fact>,
}

impl Scope {
    /// Gives up every fact, because something happened that this pass cannot see through.
    fn forget(&mut self) {
        self.lost.append(&mut self.established);
    }
}

/// Whether this instruction could do something to memory that this pass cannot account for.
///
/// A call is the whole of it, in either spelling, and inline assembly with it. A `tail_call` ends
/// the block and there is nothing after it to protect, and it is here anyway so that the reason a
/// fact survives is never that the walk did not think of something.
fn opaque(func: &Func, inst: Inst) -> bool {
    matches!(
        func[inst].opcode,
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall | Opcode::InlineAsm
    )
}

/// What a `check_bounds` is about, when it is one this pass can read.
///
/// The capability has to be the `cap_of` of the check's own pointer. That is the shape
/// `rucc-safety` emits and it is what the removal argument in the module comment needs, so a check
/// that does not have it is not a check this pass has anything to say about.
fn about(func: &Func, check: Inst) -> Option<Fact> {
    let args = &func[func[check].args];
    let &capability = args.first()?;
    let &pointer = args.get(1)?;
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(pointer) {
        return None;
    }
    let Extra::Mem(info) = func[check].extra else { return None };
    let size = i128::from(func[info].size);
    let (base, offset) = normal(func, pointer);
    Some(Fact { base, offset, size })
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
fn operand_of(func: &Func, value: Value, opcode: Opcode, index: usize) -> Option<Value> {
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
fn yes(table: &Table, rule: usize) -> bool {
    matches!(table.rules[rule].replacement, [Piece::App { .. }, Piece::Int(1)])
}

/// A term built to be asked about, and nothing else.
///
/// The rules are matched against this rather than against the function, because what is being asked
/// about is not in the function: it is what the walk worked out about two of its instructions. So
/// the subject is a small arena of exactly the term being asked, built fresh for each question and
/// thrown away with the answer.
#[derive(Debug, Default)]
struct Question {
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
    fn number(&mut self, value: i128) -> usize {
        self.held.push(Held::Int(value));
        self.held.len() - 1
    }

    /// Adds an application of `head` to what is already in the arena.
    fn app(&mut self, head: &'static str, args: &[usize]) -> usize {
        self.held.push(Held::App(head, args.to_vec()));
        self.held.len() - 1
    }

    /// Adds something the rule can bind and cannot look inside.
    fn opaque(&mut self) -> usize {
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
        Block, Builder, Extra, Func, InstData, MemInfo, MemOrder, Opcode, Restrict, Signature,
        Type, Value,
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
}
