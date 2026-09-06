//! Which IR opcodes have somewhere to go, and which do not.
//!
//! Design: `spec/10-backend.md` section 10.2, under **Coverage**.
//!
//! Every opcode has to be lowered by something or be a hole somebody wrote down. Without this the
//! way a hole is found is that somebody compiles a program containing one and the selector reports
//! that it cannot lower an instruction, which is a fine diagnostic and a bad discovery mechanism:
//! it turns a gap in the rule set into a user's problem rather than a failing build.
//!
//! # The three answers
//!
//! An opcode is lowered by a rule, or somewhere a rule cannot reach, or nowhere.
//!
//! The first is the ordinary answer and the one this can check by itself. [`crate::term`] says
//! every name a rule could be written at, the table says every name one is written at, and an
//! opcode is covered when each of its names is in both. That is what makes this a check about
//! widths rather than about opcodes: an `add` with a rule at four widths and no rule at the fifth
//! is not covered, and would be reported here as the missing name rather than as a covered opcode.
//!
//! The second is [`ELSEWHERE`], which is not a gap. `spec/10-backend.md` names five of them and
//! there are more now, and they are all the same kind of thing: an opcode whose lowering depends on
//! something no pattern can see. Where a call's arguments go depends on the signature, where a
//! local lives depends on the frame, an unconditional jump is an edge and edges live on the block,
//! and a `memcpy` is a run of moves whose length is a constant the pattern would have to count. A
//! rule matches one term and can say none of that.
//!
//! The third is [`GAPS`], which is the number `spec/15-testing.md` section 15.8 says we keep. Each
//! entry names why it is there and the issue that closes it, so that an opcode nobody has written a
//! rule for is a decision somebody wrote down rather than a surprise.
//!
//! [`WIDTHS`] and [`NAMES`] are the same third answer said about something smaller than an opcode.
//! A width on [`WIDTHS`] has no names at all, so no opcode is missing a rule at it, and a name on
//! [`NAMES`] is one width of an opcode that lowers at its other widths. Both carry the issue that
//! closes them for the same reason [`GAPS`] does.
//!
//! # What makes the lists honest
//!
//! An entry that stops being true fails. An opcode on either list that a rule starts covering is a
//! stale entry and the tests below say so by name, which is the same rule the exclusion lists in
//! the compatibility harness are kept under: a list nothing checks is a list that only grows.
//!
//! The direction this cannot check is an opcode moving from [`GAPS`] to [`ELSEWHERE`] without the
//! list following it, because where an opcode is lowered by name is a `match` arm and there is
//! nothing to ask about a `match` arm from here. What that costs is one line of a list going out of
//! date; what it does not cost is a gap going unnoticed, since the opcode is still on a list and
//! still counted.
//!
//! # The other question
//!
//! All of the above is about the rule set as it is written. [`Fired`] is about the rule set as it
//! is used: which rules a compilation actually reached. A rule nothing reaches is proved and dead
//! weight, or it is a construct the corpus does not contain and somebody should know which. The
//! selector marks a rule as it fires it, the driver writes the marks out under
//! `-Zrule-coverage=FILE`, and the harness in `tamnd/rucc-compat` unions those files over a corpus,
//! which is what turns coverage of the rule set into a number. `spec/20-execution-testing.md`
//! section 20.9 is the design and `tamnd/rucc#261` is the work.

use core::fmt;
use core::fmt::Write as _;

use rucc_ir::Opcode;
use rucc_target::Arch;

use crate::select::{Table, Test};
use crate::term;

/// An opcode no rule is written about, and the place that lowers it instead.
///
/// Not one of these is a gap. Each is an opcode whose lowering depends on something a pattern
/// cannot see, so the answer lives where that something is known.
pub static ELSEWHERE: &[(Opcode, &str)] = &[
    // The convention. What a call's operands are is whatever the signature made them, and which
    // register each one arrives in depends on the classification of every argument before it.
    (Opcode::Call, "`crate::abi`, which builds a call out of the convention"),
    (Opcode::CallIndirect, "`crate::abi`, the same instruction with the callee in a register"),
    // The frame, which is not known until the allocator has finished running out of registers.
    (Opcode::Alloca, "`crate::lower`, as an address into a frame `crate::frame` lays out later"),
    // A relocation, which is right because of what the linker does rather than because of what
    // any bitvector equals.
    (Opcode::GlobalAddr, "`crate::lower`, a `lea` off the instruction pointer with a name on it"),
    // No instruction at all. The IR keeps the width the same and the machine has one register
    // file for both, so the value is already where it needs to be.
    (Opcode::PtrToInt, "`crate::lower`, which renames the value rather than computing anything"),
    (Opcode::IntToPtr, "`crate::lower`, the same rename the other way round"),
    // Memory SSA, which is built at -O2, read by the passes that need it, and taken back off
    // before selection. Nothing in the back end has ever seen a value of type `mem`.
    (Opcode::MemEntry, "nothing at all, since memory SSA comes off before the back end runs"),
    // The edges and the two ways of writing down that control does not arrive.
    (Opcode::Jump, "`crate::layout`, since an edge is on the block and not in the block"),
    (Opcode::Unreachable, "nothing at all, which is the answer for a place control does not reach"),
    (Opcode::UnreachableHint, "nothing at all, for the same reason"),
    // Rewritten into the opcodes above before selection ever sees them.
    (Opcode::Switch, "`crate::expand`, into the compare and branch chain it is"),
    (Opcode::FConst, "`crate::expand`, into a constant in memory and a load of it"),
    (Opcode::FNeg, "`crate::expand`, into the sign bit flip it is"),
    (Opcode::UIToFP, "`crate::expand`, into a widening and a signed conversion"),
    (Opcode::FPToUI, "`crate::expand`, into a signed conversion and a narrowing"),
    (Opcode::Memcpy, "`crate::expand`, into the moves it stands for"),
    (Opcode::Memset, "`crate::expand`, into the fills it stands for"),
    (Opcode::Memmove, "`crate::expand`, into a call, since the two regions may overlap"),
    (Opcode::Bswap, "`crate::expand`, into the shifts and masks that reverse the bytes"),
    (Opcode::Ctpop, "`crate::expand`, into the halving sum that counts the set bits"),
    (Opcode::Ctlz, "`crate::expand`, into a smear and a set bit count"),
    (Opcode::Cttz, "`crate::expand`, into a mask of the low zeroes and a set bit count"),
    // The variable argument list, which is four opcodes reading a structure the ABI describes.
    (Opcode::VaStart, "`crate::varargs`, which writes the register save area the ABI describes"),
    (Opcode::VaArg, "`crate::varargs`, into the walk over that structure"),
    (Opcode::VaObject, "`crate::varargs`, the same walk for something that arrived in memory"),
    (Opcode::VaCopy, "`crate::varargs`, into a copy of the structure"),
    (Opcode::VaEnd, "`crate::varargs`, which removes it, since there is nothing to undo"),
    // Memory safety. A check is a call to the runtime, and the rewrite happens after the optimizer
    // has run so that the descriptor table only has rows for checks that survived it.
    (Opcode::CheckBounds, "`rucc_safety::lower`, into a call carrying the row that describes it"),
    (Opcode::CheckLive, "`rucc_safety::lower`, the same call over the lifetime plane"),
    (Opcode::CheckDeriv, "`rucc_safety::lower`, the same call where the pointer is computed"),
    // The capability the checks were reading, which the same pass takes out once they are calls,
    // because a call to the runtime is handed an address and finds the rest for itself.
    (Opcode::CapOf, "`rucc_safety::lower`, which removes it, since nothing reads it any more"),
];

/// An opcode nothing lowers, why it is here, and the issue that closes it.
///
/// This is the count `spec/15-testing.md` section 15.8 asks for. It is not zero yet and the
/// spec says it should be, which is the honest reading of where the back end is: every one of
/// these is a feature nobody has written, and all but three of them are opcodes the front end
/// cannot produce either, so a program that reaches one of these is a program that reaches an
/// unimplemented builtin first.
pub static GAPS: &[(Opcode, &str, &str)] = &[
    (Opcode::Splat, "a vector, and no rule is written about a lane count", "tamnd/rucc#200"),
    (
        Opcode::TargetIntrinsic,
        "the same, since what needs one is a vector builtin",
        "tamnd/rucc#200",
    ),
    (Opcode::BlockAddr, "the address of a label", "tamnd/rucc#353"),
    (Opcode::IndirectBr, "the branch a computed goto turns into", "tamnd/rucc#353"),
    (
        Opcode::FRem,
        "a call to `fmod`, so a link line question as much as a lowering one",
        "tamnd/rucc#226",
    ),
    (
        Opcode::Fma,
        "a call or one instruction, depending on what the machine is told it has",
        "tamnd/rucc#226",
    ),
    (Opcode::AtomicLoad, "an ordering, which the IR cannot say yet", "tamnd/rucc#311"),
    (Opcode::AtomicStore, "the same", "tamnd/rucc#311"),
    (Opcode::AtomicRmw, "the same, and a `lock` prefix per operation", "tamnd/rucc#311"),
    (Opcode::Cmpxchg, "the same, and a result that is a pair", "tamnd/rucc#311"),
    (
        Opcode::Fence,
        "the same, and nothing at all on this machine for most orderings",
        "tamnd/rucc#311",
    ),
    (Opcode::Bitreverse, "a node nothing writes and nothing lowers", "tamnd/rucc#363"),
    (
        Opcode::SAddOverflow,
        "a result and a flag together, which no rule can write",
        "tamnd/rucc#309",
    ),
    (Opcode::UAddOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::SSubOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::USubOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::SMulOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::UMulOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::Expect, "a branch weight nothing reads yet", "tamnd/rucc#364"),
    (Opcode::Prefetch, "one instruction, once the hints have somewhere to go", "tamnd/rucc#313"),
    (Opcode::FrameAddress, "a walk up the frame pointers", "tamnd/rucc#312"),
    (Opcode::ReturnAddress, "the same walk, one word further along", "tamnd/rucc#312"),
    (
        Opcode::StackSave,
        "a frame that can grow, as a variable length array needs",
        "tamnd/rucc#291",
    ),
    (Opcode::StackRestore, "the same", "tamnd/rucc#291"),
    (
        Opcode::SetjmpMarker,
        "a call that returns twice, which the allocator has to be told about",
        "tamnd/rucc#223",
    ),
    (Opcode::LongjmpMarker, "the same", "tamnd/rucc#223"),
    (Opcode::TailCall, "a terminator nothing writes and nothing lowers", "tamnd/rucc#365"),
    (
        Opcode::InlineAsm,
        "a template, its constraints, and sixty eight torture programs",
        "tamnd/rucc#349",
    ),
    // Memory safety. These are a gap in a different sense from the rest: nothing emits one yet
    // either, since the passes that would are milestones S2 and after, so there is no program the
    // back end can be handed that reaches one. The four the S1 pass does emit are on `ELSEWHERE`.
    (
        Opcode::CapLoad,
        "a capability, whose runtime shape `spec/safe-memory/05-representation.md` decides",
        "tamnd/rucc#428",
    ),
    (Opcode::CapStore, "the same, and a store into the slot beside a pointer", "tamnd/rucc#428"),
    (
        Opcode::CapNull,
        "the same, and it is whatever the representation says nothing is",
        "tamnd/rucc#428",
    ),
    (Opcode::CapNarrow, "the same, and arithmetic on the bounds it holds", "tamnd/rucc#428"),
    (Opcode::CapRecover, "the same, and a read of the shadow planes", "tamnd/rucc#428"),
    (Opcode::CheckType, "a read of the type plane, which is S5's", "tamnd/rucc#431"),
    (Opcode::CheckInit, "the same, over the init plane, which is S5's too", "tamnd/rucc#431"),
    (Opcode::CheckRace, "the same, over the epoch plane, which is S5's as well", "tamnd/rucc#431"),
    // The plane writes, which the runtime does for itself today because the only ranges anything
    // asks about are the ones its own allocator handed out. A stack object needs these.
    (Opcode::MetaBegin, "a write over a range of the lifetime plane", "tamnd/rucc#428"),
    (
        Opcode::MetaEnd,
        "the same write, with the version bumped past every capability",
        "tamnd/rucc#428",
    ),
    (Opcode::MetaType, "the same over the type plane, which is S5's", "tamnd/rucc#431"),
    (Opcode::MetaInit, "the same over the init plane, which is S5's", "tamnd/rucc#431"),
    (
        Opcode::MetaTransfer,
        "the same, and the state a range is in while a device owns it, which is S2's",
        "tamnd/rucc#428",
    ),
    (
        Opcode::SafeRegionBegin,
        "nothing at all, once the count document 10 section 10.2 asks for has been taken",
        "tamnd/rucc#428",
    ),
    (Opcode::SafeRegionEnd, "the same, which is to say nothing", "tamnd/rucc#428"),
];

/// A width no rule is written at, why, and the issue that closes it.
///
/// The other half of coverage, and the half an opcode list cannot say. An opcode is covered when
/// every name it has is a name a rule is written at, and a width with no name has no names to
/// check: an `add` of two `__int128`s is not a missing rule for `add`, it is a width the rule
/// language cannot spell. So the widths are written down here for the same reason the opcodes are
/// written down above.
pub static WIDTHS: &[(&str, &str, &str)] = &[
    (
        "one bit",
        "everything but and, or, xor, a constant, and the widening out of one",
        "tamnd/rucc#352",
    ),
    (
        "a hundred and twenty eight bits",
        "no register pair, so nothing at that width has a name",
        "tamnd/rucc#351",
    ),
    (
        "eighty bits",
        "a long double is on the x87 stack and no rule is about that stack",
        "tamnd/rucc#326",
    ),
    (
        "a vector of any lane count",
        "a rule at a width says nothing about how many lanes",
        "tamnd/rucc#200",
    ),
];

/// A name a rule could be written at and deliberately is not, why, and the issue that puts it
/// back.
///
/// The third list, and the one that is about a name rather than about an opcode or a width. An
/// opcode on [`GAPS`] has no lowering at any width and a width on [`WIDTHS`] has no names at all,
/// and neither of those can say that `add` is lowered at four widths and left alone at two.
///
/// This list used to be all of the narrow arithmetic. C promotes the operands of an arithmetic
/// operator to `int` before the operator is applied, so `char a, b; a + b` is an `int` addition of
/// two sign extended chars and there is no C program that asks the back end to add two bytes.
/// Rules were written at those names anyway, ahead of the pass that would reach them, and they sat
/// proved and never selected: `tamnd/rucc#261` measured that and `tamnd/rucc#368` took them out.
/// Most of them are back, because the width narrowing pass in `tamnd/rucc#375` is that caller and
/// it writes a byte add out of the truncation the assignment back to a `char` already was.
///
/// What is left is what the pass will not narrow. A divide is not narrowed because the most
/// negative byte over minus one is a defined hundred and twenty eight at four bytes and is the
/// overflow that raises at one, so it wants a range analysis saying that pair cannot happen. A
/// truth value widened to a byte is not narrowed because it is a truncation of an extension that
/// started narrower than the truncation ends, which is a third shape the pass does not have.
///
/// Not every narrow name was ever here, because promotion is not the only way a narrow operation
/// is born. Reading a bitfield is a shift and a mask by constants at the width of the storage
/// unit, writing one is a mask, a shift and an `or` of two values, and a truth test on a narrow
/// scalar is an `icmp_ne` at that scalar's width. Those fire, so those always had rules.
pub static NAMES: &[(&str, &str, &str)] = &[
    ("sdiv.i8", "a narrow divide, which wants a range analysis before it can be narrowed", NARROW),
    ("sdiv.i16", "the same", NARROW),
    ("udiv.i8", "the same", NARROW),
    ("udiv.i16", "the same", NARROW),
    ("srem.i8", "the same", NARROW),
    ("srem.i16", "the same", NARROW),
    ("urem.i8", "the same", NARROW),
    ("urem.i16", "the same", NARROW),
    ("zext.i1.i8", "a truth value widened to a byte, which nothing asks for at that width", NARROW),
    ("zext.i1.i16", "the same", NARROW),
];

/// The issue every entry of [`NAMES`] waits on, since they all wait on the same one.
const NARROW: &str = "tamnd/rucc#375";

/// What a target's rules cover, and what they do not.
#[derive(Debug)]
pub struct Report {
    /// The rule file this is about, so that anything said about it names a file to open.
    pub source: &'static str,
    /// How many opcodes the IR has.
    pub opcodes: usize,
    /// The opcodes every name of which a rule is written at.
    pub by_rule: Vec<Opcode>,
    /// How many names those are, which is one per opcode and width.
    pub names: usize,
    /// A name a rule could be written at and none is, which is what a missing rule looks like.
    pub uncovered: Vec<(Opcode, &'static str)>,
    /// A name on [`NAMES`], which is a missing rule somebody decided to be missing.
    pub deferred: Vec<(Opcode, &'static str)>,
    /// A name a rule is written at that nothing can ever be called, which is a dead rule.
    pub unreachable: Vec<&'static str>,
    /// The opcodes lowered somewhere a rule cannot reach.
    pub elsewhere: Vec<Opcode>,
    /// The opcodes nothing lowers.
    pub gaps: Vec<Opcode>,
    /// The opcodes on none of the three lists, which is what a new opcode is until somebody says
    /// where it goes.
    pub unaccounted: Vec<Opcode>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rucc-codegen: {} lowers {} of the {} IR opcodes by rule at {} names, {} are lowered \
             where no rule reaches, {} have no lowering yet and {} names are left for later",
            self.source,
            self.by_rule.len(),
            self.opcodes,
            self.names,
            self.elsewhere.len(),
            self.gaps.len(),
            self.deferred.len()
        )
    }
}

/// What a table covers.
///
/// Nothing is executed and nothing is compiled. The rule set and the naming of instructions are
/// both data, and the answer is a comparison of two lists.
#[must_use]
pub fn report(table: &Table) -> Report {
    let named = term::heads();
    let patterns = pattern_heads(table);

    let mut by_rule = Vec::new();
    let mut uncovered = Vec::new();
    let mut deferred = Vec::new();
    for &(opcode, name) in &named {
        if patterns.contains(&name) {
            by_rule.push(opcode);
        } else if NAMES.iter().any(|&(deliberate, ..)| deliberate == name) {
            deferred.push((opcode, name));
        } else {
            uncovered.push((opcode, name));
        }
    }
    // An opcode is covered when every name it has is covered, so one missing width takes the
    // whole opcode off the list however many of its other widths are there. A name on `NAMES` does
    // not take it off, because the opcode is lowered and the entry says which widths were left for
    // later and why: that is a narrower claim than the opcode having nowhere to go, and putting it
    // on `GAPS` instead would say the wrong thing about an `add` that lowers perfectly well at
    // four widths.
    for &(opcode, _) in &uncovered {
        by_rule.retain(|&covered| covered != opcode);
    }
    by_rule.sort_unstable();
    by_rule.dedup();

    let names = named.len() - uncovered.len() - deferred.len();
    let unreachable: Vec<&'static str> = patterns
        .iter()
        .filter(|head| !named.iter().any(|(_, name)| name == *head))
        .copied()
        .collect();

    let elsewhere: Vec<Opcode> = ELSEWHERE.iter().map(|&(opcode, _)| opcode).collect();
    let gaps: Vec<Opcode> = GAPS.iter().map(|&(opcode, ..)| opcode).collect();
    let unaccounted: Vec<Opcode> = Opcode::all()
        .filter(|opcode| {
            !by_rule.contains(opcode) && !elsewhere.contains(opcode) && !gaps.contains(opcode)
        })
        .collect();

    Report {
        source: table.source,
        opcodes: Opcode::all().count(),
        by_rule,
        names,
        uncovered,
        deferred,
        unreachable,
        elsewhere,
        gaps,
        unaccounted,
    }
}

/// Every name a rule in a table is written about, which is the first test the trie makes.
///
/// Node zero is the root of the trie over the patterns and the first thing any walk asks is what
/// the term in hand is called, so its tests are exactly the set of pattern heads. There is no
/// wildcard there to worry about: a rule matching any term at all is one nobody has written and
/// one that would be an error to write, since a lowering has to know what it is lowering.
fn pattern_heads(table: &Table) -> Vec<&'static str> {
    let Some(root) = table.nodes.first() else { return Vec::new() };
    let mut found: Vec<&'static str> = root
        .tests
        .iter()
        .filter_map(|(test, _)| match test {
            Test::App { head, .. } => Some(*head),
            // Neither can be at the root. A pattern is a term with a head, so the first step of
            // every one of them is a head, and there is nothing bound yet to be the same as.
            Test::Int(_) | Test::Same(_) => None,
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// The rules a target lowers by, or `None` where no back end in this crate covers it.
///
/// The same question [`crate::pipeline::Machine::for_target`] answers about the rest of a machine,
/// and it is here as well because a caller that wants to write down what a run covered has a
/// target and no machine. An architecture that gets a rule file at M6 gets an arm here at the same
/// time, and until then it has no rules to report coverage of rather than an empty set of them.
#[must_use]
pub fn table(arch: Arch) -> Option<&'static Table> {
    match arch {
        Arch::X86_64 => Some(&crate::select::x86_64::TABLE),
        Arch::Aarch64 | Arch::Riscv64 => None,
    }
}

/// Which rules fired, over one function or over a whole compilation.
///
/// A bit per rule and nothing else. This is on the path of every instruction selected, so what it
/// costs is paid by every compilation whether or not anybody asked for the number, and the cheapest
/// thing that answers the question is a flag per rule set once.
///
/// The index of a rule is how this is kept and not how it is written down. An index moves the
/// moment a rule is added above it, so [`Fired::listing`] names the rule file and the line instead:
/// a line is a place somebody can open, and a report written by one build can still be read against
/// a rule file that has grown since.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fired {
    /// One entry per rule, true once that rule has fired. It grows to fit the highest index
    /// marked rather than being sized from a table, so nothing here has to be told which target
    /// is being compiled for.
    seen: Vec<bool>,
}

impl Fired {
    /// Nothing has fired yet.
    #[must_use]
    pub const fn new() -> Fired {
        Fired { seen: Vec::new() }
    }

    /// Records that the rule at this index fired.
    pub fn mark(&mut self, rule: usize) {
        if self.seen.len() <= rule {
            self.seen.resize(rule + 1, false);
        }
        self.seen[rule] = true;
    }

    /// Whether the rule at this index fired.
    #[must_use]
    pub fn has(&self, rule: usize) -> bool {
        self.seen.get(rule).copied().unwrap_or(false)
    }

    /// How many rules fired.
    #[must_use]
    pub fn count(&self) -> usize {
        self.seen.iter().filter(|fired| **fired).count()
    }

    /// Takes in everything another one recorded.
    ///
    /// One compilation is many functions and one command line is many files, and the question is
    /// about all of them together. Merging rather than writing a file per function is also what
    /// keeps the answer the same however the work was scheduled.
    pub fn merge(&mut self, other: &Fired) {
        if self.seen.len() < other.seen.len() {
            self.seen.resize(other.seen.len(), false);
        }
        for (mine, theirs) in self.seen.iter_mut().zip(&other.seen) {
            *mine |= *theirs;
        }
    }

    /// What `-Zrule-coverage=FILE` writes.
    ///
    /// One line per rule in the table, in the order the rule file writes them, each saying whether
    /// the rule fired and naming the file and line it is written at. Every rule is listed rather
    /// than only the ones that fired, so that one of these files says what the whole rule set was
    /// as well as what this compilation reached: a reader unioning them over a corpus needs both
    /// and would otherwise have to parse the rule file to get the second.
    ///
    /// The first line is a comment holding the count, which is the number a person wants and the
    /// one thing here that is not worth making them add up.
    #[must_use]
    pub fn listing(&self, table: &Table) -> String {
        let fired = table.rules.iter().enumerate().filter(|(index, _)| self.has(*index)).count();
        let mut out = format!(
            "# rucc rule coverage: {fired} of {} rules in {} fired\n",
            table.rules.len(),
            table.source
        );
        for (index, rule) in table.rules.iter().enumerate() {
            let word = if self.has(index) { "fired" } else { "unused" };
            let _ = writeln!(out, "{word} {}:{} {}", table.source, rule.line, rule.pattern);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::x86_64::TABLE;

    /// The claim the whole module is for, in the direction that matters: a name an instruction
    /// can be called by is a name a rule is written at. This is the width check as much as the
    /// opcode check, since a name is an opcode and a width together.
    #[test]
    fn every_name_an_instruction_can_have_is_one_a_rule_is_written_at() {
        let report = report(&TABLE);
        assert!(
            report.uncovered.is_empty(),
            "nothing in {} lowers these, and each is an opcode at a width the rule language can \
             spell: {:?}",
            report.source,
            report.uncovered
        );
    }

    /// And the other direction, which costs nothing to ask and finds a rule that can never fire.
    /// A pattern head no instruction is ever called by is a rule written against a name that was
    /// renamed or misspelled, and it would sit there proved and unreachable.
    #[test]
    fn every_name_a_rule_is_written_at_is_one_an_instruction_can_have() {
        let report = report(&TABLE);
        assert!(
            report.unreachable.is_empty(),
            "{} has rules for these and no instruction is ever called one: {:?}",
            report.source,
            report.unreachable
        );
    }

    /// Every opcode is one of the three things, so a new opcode in the IR fails this until
    /// somebody says where it goes. That is the whole point: the answer for a new opcode should
    /// be written down when it is added rather than discovered by a user compiling a program.
    #[test]
    fn every_opcode_is_lowered_or_is_a_gap_somebody_wrote_down() {
        let report = report(&TABLE);
        assert!(
            report.unaccounted.is_empty(),
            "no rule lowers these, `ELSEWHERE` does not say where they are lowered and `GAPS` \
             does not say why they are not: {:?}",
            report.unaccounted
        );
        assert_eq!(
            report.by_rule.len() + report.elsewhere.len() + report.gaps.len(),
            report.opcodes,
            "the three lists overlap, so an opcode is counted twice"
        );
    }

    /// An entry that starts being covered fails, which is the rule every list in this project is
    /// kept under. An opcode a rule now lowers is one that should be off both lists, and a list
    /// that keeps claiming otherwise is a list nobody can read.
    #[test]
    fn an_entry_a_rule_now_covers_is_a_stale_entry() {
        let report = report(&TABLE);
        for &(opcode, where_) in ELSEWHERE {
            assert!(
                !report.by_rule.contains(&opcode),
                "`{}` is lowered by a rule now, so the `ELSEWHERE` entry saying it is lowered by \
                 {where_} is stale",
                opcode.name()
            );
        }
        for &(opcode, why, issue) in GAPS {
            assert!(
                !report.by_rule.contains(&opcode),
                "`{}` is lowered by a rule now, so the `GAPS` entry saying it is {why} is stale \
                 and {issue} may be closed",
                opcode.name()
            );
            assert!(
                !report.elsewhere.contains(&opcode),
                "`{}` is on both lists, so it is both lowered and not lowered",
                opcode.name()
            );
        }
    }

    /// The same staleness rule one list down. A name a rule is written at is a name that is not
    /// left for later, and an entry claiming otherwise is one that should have gone when the rule
    /// arrived. The other direction is checked too: a name no instruction can ever have is a
    /// misspelling, and it would sit here excusing nothing.
    #[test]
    fn a_name_a_rule_is_written_at_is_not_a_name_left_for_later() {
        let heads = pattern_heads(&TABLE);
        let named = term::heads();
        for &(name, why, issue) in NAMES {
            assert!(
                !heads.contains(&name),
                "`{name}` is lowered by a rule now, so the `NAMES` entry saying it is {why} is \
                 stale and {issue} may be closer than it says"
            );
            assert!(
                named.iter().any(|&(_, head)| head == name),
                "`{name}` is not a name any instruction can have, so the `NAMES` entry excuses \
                 nothing"
            );
        }
        let report = report(&TABLE);
        assert_eq!(report.deferred.len(), NAMES.len(), "{:?}", report.deferred);
    }

    /// Every gap names an issue, since a gap with no issue behind it is a gap nobody has decided
    /// anything about, which is the thing this module exists to stop.
    #[test]
    fn every_gap_names_the_issue_that_closes_it() {
        let issues = GAPS
            .iter()
            .map(|&(_, _, issue)| issue)
            .chain(WIDTHS.iter().map(|&(_, _, issue)| issue))
            .chain(NAMES.iter().map(|&(_, _, issue)| issue));
        for issue in issues {
            let number = issue
                .strip_prefix("tamnd/rucc#")
                .unwrap_or_else(|| panic!("{issue} is not an issue in this project's tracker"));
            assert!(number.parse::<u32>().is_ok(), "{issue} does not name an issue number");
        }
    }

    /// The count, which `spec/15-testing.md` section 15.8 says we keep about ourselves. CI runs
    /// this test with the output shown, so the number lands in a log next to the rule proof
    /// rather than in a file somebody has to go and read.
    #[test]
    fn the_count_is_reported() {
        let report = report(&TABLE);
        println!("{report}");
        for &(opcode, why, issue) in GAPS {
            println!("rucc-codegen: no lowering for `{}`, which is {why}: {issue}", opcode.name());
        }
        for &(width, why, issue) in WIDTHS {
            println!("rucc-codegen: no rule at {width}, which is {why}: {issue}");
        }
        for &(name, why, issue) in NAMES {
            println!("rucc-codegen: no rule at `{name}`, which is {why}: {issue}");
        }
        assert_eq!(report.gaps.len(), GAPS.len());
    }

    /// What the root of the trie is, which is the assumption [`pattern_heads`] rests on. If the
    /// rule compiler ever built the trie some other way this would say so, rather than the
    /// coverage numbers quietly becoming a report about an empty list.
    #[test]
    fn the_root_of_the_trie_is_the_head_of_every_pattern() {
        let heads = pattern_heads(&TABLE);
        assert!(!heads.is_empty(), "the table has rules and the root of the trie tests nothing");
        for rule in TABLE.rules {
            let head = rule
                .pattern
                .strip_prefix('(')
                .and_then(|rest| rest.split([' ', ')']).next())
                .expect("a pattern is an application");
            assert!(
                heads.contains(&head),
                "line {}: {} is a pattern whose head the root of the trie does not test",
                rule.line,
                rule.pattern
            );
        }
    }

    /// The one target with a rule file, and the two that get one at M6. A machine that can be
    /// compiled for has rules to report the coverage of, and one that cannot has none rather than
    /// an empty set of them, which are different answers and would read the same as a number.
    #[test]
    fn a_target_with_a_back_end_is_a_target_with_a_rule_set() {
        let x86 = table(Arch::X86_64).expect("x86-64 is what this crate lowers for");
        assert_eq!(x86.source, TABLE.source);
        assert!(!x86.rules.is_empty());
        assert!(table(Arch::Aarch64).is_none(), "there is no aarch64 rule file yet");
        assert!(table(Arch::Riscv64).is_none(), "there is no riscv64 rule file yet");
    }

    /// What a rule is called outside this process. The index is not it: a rule added at the top of
    /// the file moves every index below it, and a report from last week would then be a report
    /// about the wrong rules. The file and the line do not move that way and are somewhere to look.
    #[test]
    fn a_rule_is_written_down_as_the_place_it_is_written_at() {
        let mut fired = Fired::new();
        fired.mark(0);
        let listing = fired.listing(&TABLE);
        let first =
            format!("fired {}:{} {}", TABLE.source, TABLE.rules[0].line, TABLE.rules[0].pattern);
        assert!(listing.contains(&first), "{listing}");
        assert!(listing.lines().next().is_some_and(|line| line.starts_with('#')), "{listing}");
    }

    /// Every rule is listed and not only the ones that fired, which is what lets one of these files
    /// be read on its own. A reader that only got the rules that fired would have to parse the rule
    /// file to find out what the rest of them were.
    #[test]
    fn one_file_says_what_the_whole_rule_set_is() {
        let listing = Fired::new().listing(&TABLE);
        let lines: Vec<&str> = listing.lines().collect();
        assert_eq!(lines.len(), TABLE.rules.len() + 1, "one line per rule and one for the count");
        assert_eq!(
            lines.iter().filter(|line| line.starts_with("unused ")).count(),
            TABLE.rules.len()
        );
        assert!(lines[0].contains(&format!("0 of {} rules", TABLE.rules.len())), "{}", lines[0]);
    }

    /// A compilation is many functions and a command line is many files, and the question is about
    /// all of them at once. Merging is also what keeps the answer the same however the work was
    /// scheduled, which is the rule `spec/03-architecture.md` section 3.7 holds everything to.
    #[test]
    fn what_two_runs_reached_is_what_either_of_them_reached() {
        let mut one = Fired::new();
        one.mark(3);
        one.mark(3);
        assert_eq!(one.count(), 1, "a rule that fires twice is one rule");
        let mut two = Fired::new();
        two.mark(0);
        two.mark(9);
        one.merge(&two);
        assert_eq!(one.count(), 3);
        assert!(one.has(0) && one.has(3) && one.has(9));
        assert!(!one.has(1));

        // The merge is symmetric, since neither order of two files is the right one.
        let mut back = Fired::new();
        back.mark(0);
        back.mark(9);
        let mut three = Fired::new();
        three.mark(3);
        back.merge(&three);
        assert_eq!(back, one);
    }
}
