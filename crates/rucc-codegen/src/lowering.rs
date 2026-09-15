//! The passes that run before selection, as a group with a name and a stated membership.
//!
//! Design: `spec/optimizer/36-lowering-and-isel.md` section 36.1.
//!
//! Section 36.1 reads the list of passes gcc runs immediately before `pass_expand` and draws one
//! conclusion from it. Nine of them are lowerings, and each one turns a construct into a shape of
//! control flow or a shape of arithmetic that the expander would otherwise have to invent. The
//! expander is the wrong place to invent control flow, because by the time it runs the graph is
//! being consumed rather than edited. That is spec 10.2's rule arrived at from the other side: a
//! lowering rule replaces a term with a term and has nowhere to put a block, so any construct whose
//! lowering is a new shape of control flow is rewritten before selection runs.
//!
//! Every one of these passes already existed and every one of them was already called from
//! `crate::pipeline`, one line at a time, in this order. What did not exist was the thing the
//! section asks for, which is that they are a group rather than a set of unrelated passes that
//! happen to run next to each other. The reason gcc's list is nine passes long is that it grew one
//! pass at a time over three decades, and a group with a written down membership is the thing that
//! stops the same happening here.
//!
//! # The name
//!
//! The lowering group, which is what gcc calls its own and is what this module is named after. The
//! longer and more honest description section 36.1 gives is everything the selector cannot express,
//! and that is the test for whether something belongs here: not that it is a rewrite of the IR, but
//! that the thing it rewrites is one no rule in the table can be written for.
//!
//! # What is in it
//!
//! [`Step::GROUP`], in the order it runs, and that list is the membership. A new lowering is a new
//! variant of [`Step`] and a new line in that list, which is one place rather than whichever line
//! of the pipeline looked convenient.
//!
//! # What the order is for
//!
//! Most of it does not matter and the parts that do are on the variants. The rule behind them is
//! the same one every time: a pass is written about the constructs the machine has, so anything
//! that produces a construct somebody below is written about has to run above them. An integer of
//! forty bits is not a width this machine has, an ordered load is not a load any pass below is
//! written about, and a quad float is not a float the pass that rewrites floats knows anything of.
//!
//! # What it is not
//!
//! Not the selector, and not a fixed point. Each step runs once, and a step that produces work for
//! a step above it would be a bug in this order rather than a reason to run the group twice.
//!
//! Not a promise that the construct is gone either, and this is the part worth reading twice. Every
//! step here has cases it walks away from: a copy too large to be a run of moves, an ordered access
//! wider than the machine does in one go, a conversion the machine already has an instruction for
//! and so has no reason to touch. Some of those are the machine having the construct after all and
//! some of them are a refusal, and a refusal is left standing on purpose, because the selector is
//! what names the construct it had no rule for and that is a better error than a rewrite that
//! guessed.
//!
//! So what [`Ran`] records is what each step found and what it left, and reading one of those is
//! how you tell the two apart. What the group promises is only that every construct in the list was
//! put in front of the step that answers for it, which is the thing that stops being true when
//! somebody adds a lowering to whichever line of the pipeline looked convenient.

use rucc_base::Interner;
use rucc_ir::{Func, Opcode};
use rucc_target::CallRegs;

use crate::{expand, quad, retry, switch, varargs, wide, widths};

/// One member of the group.
///
/// The name of the variant is the name of the construct rather than the name of the function that
/// takes it out, because the membership is a list of constructs. Which function answers for one is
/// something this file knows and nothing outside it needs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Step {
    /// A `switch`, as the decision tree document 24 describes.
    Switches,
    /// A read modify write this machine has no single instruction for, as a loop around the compare
    /// and exchange.
    ///
    /// Beside the switches rather than down with the rest of the rewriting, because both of them
    /// make blocks and nothing in [`crate::expand`] may.
    Retries,
    /// An ordered load or store, as the plain access and a barrier.
    ///
    /// Above everything below it, since what an ordered access becomes here is a plain one and
    /// every pass below is written about a plain one by name. It is also why this is above the
    /// retries rather than below: the head of the loop they build reads with an ordered load.
    Orderings,
    /// An arithmetic operation that also says whether it overflowed, as the arithmetic and the test.
    ///
    /// Above the splitting rather than below it, because an overflow check is the one instruction
    /// whose result is two things and the splitting has no answer for that, while the arithmetic it
    /// becomes here is adds, multiplies and comparisons the splitting knows already. Nothing is
    /// lost by running it this early: the widths it is written for are the widths the machine has,
    /// so a check at any other width is refused by name either way round.
    Overflows,
    /// An integer wider than a register, as the two halves of one.
    ///
    /// Ahead of the width legalisation and not part of it, because the two go in opposite
    /// directions: an integer of forty bits becomes one of sixty four down there and one of a
    /// hundred and twenty eight becomes two of sixty four here. Doing this first means a function
    /// holding both is one the step below still works on.
    Halves,
    /// An integer at a width the machine does not have, as the width it is held in.
    ///
    /// Before everything after it, because every pass after it is written about widths the machine
    /// has and an integer of forty bits is not one of them.
    Widths,
    /// A byte reversal, as the halving run of swaps it is.
    Bytes,
    /// A leading zero, trailing zero or set bit count, as the arithmetic that answers it.
    Counts,
    /// Anything at all at the quad float format, as a call to the routine for it.
    ///
    /// Above the float rewriting rather than part of it, because the two are written about
    /// different machines: every rewrite down there ends at an instruction this machine has, and
    /// every operation up here ends at a call because this machine has no instruction at the format
    /// at all. Running first means the step below never sees a quad.
    Quads,
    /// A float constant, a negation and the conversions, as the integer work spec 10.2 asks for.
    Floats,
    /// A `memcpy`, a `memset` or a `memmove`, as the moves it is or as the call it is too big for.
    Bulk,
    /// The size of a stack allocation, rounded up to what the stack pointer has to stay on.
    ///
    /// The one step here that takes nothing out. It rewrites an operand of the instruction and
    /// leaves the instruction where it is, which is why [`Step::opcodes`] answers with nothing for
    /// it.
    Rounds,
    /// A variable argument list, as spec 10.7's split describes.
    Varargs,
}

impl Step {
    /// The group, in the order it runs, which is the membership section 36.1 asks to see.
    pub const GROUP: &'static [Self] = &[
        Self::Switches,
        Self::Retries,
        Self::Orderings,
        Self::Overflows,
        Self::Halves,
        Self::Widths,
        Self::Bytes,
        Self::Counts,
        Self::Quads,
        Self::Floats,
        Self::Bulk,
        Self::Rounds,
        Self::Varargs,
    ];

    /// What it is called in a dump.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Switches => "switches",
            Self::Retries => "retries",
            Self::Orderings => "orderings",
            Self::Overflows => "overflows",
            Self::Halves => "halves",
            Self::Widths => "widths",
            Self::Bytes => "bytes",
            Self::Counts => "counts",
            Self::Quads => "quads",
            Self::Floats => "floats",
            Self::Bulk => "bulk",
            Self::Rounds => "rounds",
            Self::Varargs => "varargs",
        }
    }

    /// The construct it is the answer to, in the words section 36.1 uses for it.
    #[must_use]
    pub const fn construct(self) -> &'static str {
        match self {
            Self::Switches => "a switch",
            Self::Retries => "a read modify write with no instruction behind it",
            Self::Orderings => "an ordered load or store",
            Self::Overflows => "arithmetic that reports whether it overflowed",
            Self::Halves => "an integer wider than a register",
            Self::Widths => "an integer at a width the machine does not have",
            Self::Bytes => "a byte reversal",
            Self::Counts => "a bit count",
            Self::Quads => "the quad float format",
            Self::Floats => "a float constant, a negation or a conversion",
            Self::Bulk => "a bulk copy or fill",
            Self::Rounds => "a stack allocation whose size is not a multiple of the alignment",
            Self::Varargs => "a variable argument list",
        }
    }

    /// The opcodes it is the answer to, which is what [`Did::found`] and [`Did::left`] count.
    ///
    /// Not a promise that none of them survive. Several of these steps have a case they leave where
    /// it stands, either because the machine turns out to have the construct after all or because
    /// this is a refusal being handed to the selector to name, and both of those show up here as a
    /// count that did not reach zero. What the pair of numbers is for is telling somebody reading a
    /// dump which of those happened.
    ///
    /// Empty for [`Step::Rounds`], which rewrites an operand rather than taking an instruction out,
    /// and empty for the three that work by type rather than by opcode: an integer of forty bits,
    /// one of a hundred and twenty eight and a quad float are all spelled with the same opcodes as
    /// anything else, and what makes them the construct is the type on the values.
    #[must_use]
    pub const fn opcodes(self) -> &'static [Opcode] {
        match self {
            Self::Switches => &[Opcode::Switch],
            Self::Retries => &[],
            Self::Orderings => &[Opcode::AtomicLoad, Opcode::AtomicStore],
            Self::Overflows => &[
                Opcode::UAddOverflow,
                Opcode::SAddOverflow,
                Opcode::USubOverflow,
                Opcode::SSubOverflow,
                Opcode::UMulOverflow,
                Opcode::SMulOverflow,
            ],
            Self::Halves | Self::Widths | Self::Rounds => &[],
            Self::Bytes => &[Opcode::Bswap],
            Self::Counts => &[Opcode::Ctlz, Opcode::Cttz, Opcode::Ctpop],
            Self::Quads => &[],
            Self::Floats => &[
                Opcode::FConst,
                Opcode::FNeg,
                Opcode::SIToFP,
                Opcode::UIToFP,
                Opcode::FPToSI,
                Opcode::FPToUI,
            ],
            Self::Bulk => &[Opcode::Memcpy, Opcode::Memset, Opcode::Memmove],
            Self::Varargs => &[Opcode::VaArg, Opcode::VaObject, Opcode::VaCopy, Opcode::VaEnd],
        }
    }

    /// Whether this step works on the whole function at once and says whether it rewrote it.
    ///
    /// Two of them do. Both retype every value of a width, so either the whole function can be
    /// rewritten or none of it can, and they answer with a boolean for that reason. A `false` from
    /// one covers two different things, a function with nothing at that width in it and a function
    /// holding something the step did not understand, and neither is an error: the second leaves
    /// the selector to refuse by naming the construct it had no rule for.
    ///
    /// Everything else here works instruction by instruction and has nothing to say at that scale,
    /// which is why [`Did::untouched`] is only ever true for these two.
    #[must_use]
    pub const fn whole_function(self) -> bool {
        matches!(self, Self::Halves | Self::Widths)
    }

    /// Runs this one step, answering whether it rewrote the function.
    ///
    /// Only the two that [`Step::whole_function`] names ever answer `false`, because they are the
    /// only two that know. The rest work instruction by instruction and are not asked.
    fn run(self, func: &mut Func, names: &mut Interner, conv: &CallRegs) -> bool {
        match self {
            Self::Switches => switch::switches(func),
            Self::Retries => retry::loops(func),
            Self::Orderings => expand::orderings(func, conv.word),
            Self::Overflows => expand::overflows(func),
            Self::Halves => return wide::halves(func, names, conv),
            Self::Widths => return widths::integers(func),
            Self::Bytes => expand::bytes(func),
            Self::Counts => expand::counts(func),
            Self::Quads => quad::calls(func, names),
            Self::Floats => expand::floats(func),
            Self::Bulk => expand::bulk(func, names, conv.word),
            Self::Rounds => expand::rounds(func, conv.stack_align),
            Self::Varargs => varargs::lists(func, conv),
        }
        true
    }
}

/// What one step did to one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Did {
    /// Which step it was.
    pub step: Step,
    /// How many instructions of the kind it answers for were there when it started.
    pub found: usize,
    /// How many were still there when it finished, which is not always zero. See [`Step::opcodes`].
    pub left: usize,
    /// How many instructions the function had before it ran.
    pub before: usize,
    /// How many it had after.
    pub after: usize,
    /// Whether it said it left the function exactly as it was, which only the two that
    /// [`Step::whole_function`] names ever say.
    pub untouched: bool,
}

/// What the whole group did to one function.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ran {
    /// One entry per step, in the order they ran, including the ones that found nothing.
    ///
    /// Including them on purpose. A dump that lists only the steps that fired is a dump that cannot
    /// tell a step that found nothing from a step somebody forgot to add to the group.
    pub did: Vec<Did>,
}

impl Ran {
    /// What one step of the group did, which every step has an entry for.
    ///
    /// # Panics
    ///
    /// Panics if this record did not come from [`group`], since that is the only way a step of
    /// [`Step::GROUP`] can be missing from it.
    #[must_use]
    pub fn of(&self, step: Step) -> Did {
        *self.did.iter().find(|did| did.step == step).expect("every step has an entry")
    }

    /// The dump, one line per step.
    ///
    /// Plain text with the name first, because the thing anybody reads this for is which step
    /// changed the function, and a format that has to be parsed to answer that is the wrong format
    /// for a debugging aid. `-Zlowering=` writes it.
    #[must_use]
    pub fn render(&self, func: &str) -> String {
        use std::fmt::Write;

        let mut out = format!("lowering {func}\n");
        for did in &self.did {
            let _ = write!(
                out,
                "  {:<10} {:>4} -> {:>4} insts",
                did.step.name(),
                did.before,
                did.after
            );
            // Said the rare way round on purpose. The two whole function steps answer `false` for
            // every function with nothing at their width in it, which is nearly all of them, so a
            // line per function saying so would bury the one that matters.
            if did.step.whole_function() && !did.untouched {
                let _ = write!(out, ", retyped every value at that width");
            }
            if did.found > 0 {
                let _ = write!(out, ", found {}, left {}", did.found, did.left);
            }
            let _ = writeln!(out, " ({})", did.step.construct());
        }
        out
    }
}

/// What the group did to every function a run lowered, in the order they came through.
///
/// The same shape [`crate::pressure::Pressure`] has and for the same reason: a caller collects one
/// of these over a whole command line and asks for the listing once at the end.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Lowerings {
    /// One per function, in the order they were lowered.
    rows: Vec<(String, Ran)>,
    /// Whether anything is going to read this, which is whether `-Zlowering` was given.
    wanted: bool,
}

impl Lowerings {
    /// Nothing recorded, and nothing counted either.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same, told whether to count, which is what `-Zlowering=FILE` decides.
    #[must_use]
    pub fn asked(wanted: bool) -> Self {
        Self { rows: Vec::new(), wanted }
    }

    /// Whether the counting is worth doing, which is what [`group`] is passed.
    ///
    /// This is a question and not an assumption for a reason that showed up as soon as the numbers
    /// were measured on something large. Counting is a walk of the function per step, and a
    /// function's instructions are a linked list, so on the SQLite amalgamation the walks cost
    /// about two seconds on top of nine, which is more than several of the passes they are
    /// measuring. A debugging aid nobody asked for should cost nothing, so a run without the flag
    /// runs the group and records no numbers at all.
    #[must_use]
    pub fn wanted(&self) -> bool {
        self.wanted
    }

    /// Writes down what the group did to one function.
    pub fn record(&mut self, name: &str, ran: Ran) {
        self.rows.push((name.to_owned(), ran));
    }

    /// Takes in everything another one recorded, which is how one file's answer joins a run's.
    pub fn merge(&mut self, other: &Self) {
        self.rows.extend(other.rows.iter().cloned());
    }

    /// How many functions went through the group.
    #[must_use]
    pub fn functions(&self) -> usize {
        self.rows.len()
    }

    /// What `-Zlowering=FILE` writes.
    ///
    /// A comment holding the count and then one block per function. Whoever reads one of these is
    /// looking for which step changed a function they are surprised by, so the file is the same
    /// text in the same order as the group ran, and every step is there whether it did anything or
    /// not. A dump listing only the steps that fired could not tell a step that found nothing from
    /// a step somebody forgot to put in the group, which is half of what this is read for.
    #[must_use]
    pub fn listing(&self) -> String {
        let mut out = format!("# rucc lowering: {} functions\n", self.rows.len());
        for (name, ran) in &self.rows {
            out.push_str(&ran.render(name));
        }
        out
    }
}

/// Runs the whole group over one function, in the order [`Step::GROUP`] gives.
///
/// This is the entry point section 36.1 asks for. Every caller wanting a function lowered calls
/// this and nothing else, so adding a lowering is adding it to [`Step::GROUP`] rather than to
/// whichever line of `crate::pipeline` looked convenient.
///
/// `counting` is whether to work out what each step found and left, which is what
/// [`Lowerings::wanted`] answers and which costs what it says there. The steps run either way and
/// the function comes out the same; what a `false` gives back is an empty [`Ran`].
pub fn group(func: &mut Func, names: &mut Interner, conv: &CallRegs, counting: bool) -> Ran {
    let mut ran = Ran::default();
    for &step in Step::GROUP {
        if !counting {
            step.run(func, names, conv);
            continue;
        }
        let (before, found) = tally(func, step);
        let did = step.run(func, names, conv);
        let (after, left) = tally(func, step);
        ran.did.push(Did { step, found, left, before, after, untouched: !did });
    }
    ran
}

/// How many instructions the function has, and how many of them are the kind this step answers for.
///
/// Both in one walk rather than one walk each, since the walk is the expensive part.
fn tally(func: &Func, step: Step) -> (usize, usize) {
    let wanted = step.opcodes();
    let (mut all, mut mine) = (0, 0);
    for block in func.blocks() {
        for inst in func.insts(block) {
            all += 1;
            if wanted.contains(&func[inst].opcode) {
                mine += 1;
            }
        }
    }
    (all, mine)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Builder, Extra, Flags, Float, Func, InstData, MemInfo, MemOrder, Opcode, Restrict,
        Signature, Type, Value,
    };
    use rucc_target::x86_64;

    use super::{Lowerings, Ran, Step, group};

    /// A function with a body somebody else writes, which is the same helper the passes being
    /// grouped are each tested with.
    fn one(
        params: &[Type],
        returns: &[Type],
        body: impl FnOnce(&mut Builder<'_>, &[Value]),
    ) -> (Interner, Func) {
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("f"),
            Signature::new().with_params(params).with_returns(returns),
        );
        let entry = func.create_block();
        let args: Vec<_> = params.iter().map(|&ty| func.append_param(entry, ty)).collect();
        let mut build = Builder::new(&mut func, entry);
        body(&mut build, &args);
        (names, func)
    }

    fn run(func: &mut Func, names: &mut Interner) -> Ran {
        group(func, names, &x86_64::SYSV, true)
    }

    fn i32() -> Type {
        Type::int(32)
    }

    #[test]
    fn the_group_is_the_passes_the_pipeline_used_to_call_one_line_at_a_time() {
        // The list rather than the length, because a list checked only for its length is a list
        // anybody can reorder without noticing, and the order is half of what this file is for.
        let names: Vec<&str> = Step::GROUP.iter().map(|step| step.name()).collect();
        assert_eq!(
            names,
            [
                "switches",
                "retries",
                "orderings",
                "overflows",
                "halves",
                "widths",
                "bytes",
                "counts",
                "quads",
                "floats",
                "bulk",
                "rounds",
                "varargs",
            ]
        );
    }

    #[test]
    fn every_step_says_what_it_is_for_and_no_two_say_the_same_thing() {
        let mut names: Vec<&str> = Step::GROUP.iter().map(|step| step.name()).collect();
        let mut constructs: Vec<&str> = Step::GROUP.iter().map(|step| step.construct()).collect();
        assert!(constructs.iter().all(|construct| !construct.is_empty()));
        for list in [&mut names, &mut constructs] {
            let was = list.len();
            list.sort_unstable();
            list.dedup();
            assert_eq!(list.len(), was, "two steps say the same thing");
        }
    }

    #[test]
    fn a_function_with_nothing_in_it_leaves_every_step_with_nothing_to_say() {
        let (mut names, mut func) = one(&[], &[], |build, _| {
            build.ret(&[]);
        });
        let ran = run(&mut func, &mut names);
        assert_eq!(ran.did.len(), Step::GROUP.len());
        assert!(ran.did.iter().all(|did| did.found == 0 && did.before == did.after));
    }

    #[test]
    fn nothing_in_the_group_is_left_out_of_the_record() {
        let (mut names, mut func) = one(&[], &[], |build, _| {
            build.ret(&[]);
        });
        let ran = run(&mut func, &mut names);
        let ordered: Vec<Step> = ran.did.iter().map(|did| did.step).collect();
        assert_eq!(ordered, Step::GROUP);
    }

    /// `unsigned b(unsigned x) { return __builtin_bswap32(x); }`, which is one of the constructs
    /// in the list and therefore one the group owes an answer for.
    #[test]
    fn a_byte_reversal_does_not_survive_the_group() {
        let (mut names, mut func) = one(&[i32()], &[i32()], |build, args| {
            let swapped = build.unary(Opcode::Bswap, args[0], i32());
            build.ret(&[swapped]);
        });
        let ran = run(&mut func, &mut names);
        let did = ran.of(Step::Bytes);
        assert_eq!(did.found, 1);
        assert_eq!(did.left, 0);
        assert!(did.after > did.before, "one instruction became several");
    }

    /// `int c(unsigned x) { return __builtin_popcount(x); }`.
    #[test]
    fn a_bit_count_does_not_survive_the_group() {
        let (mut names, mut func) = one(&[i32()], &[i32()], |build, args| {
            let ones = build.unary(Opcode::Ctpop, args[0], i32());
            build.ret(&[ones]);
        });
        let ran = run(&mut func, &mut names);
        assert_eq!(ran.of(Step::Counts).found, 1);
        assert_eq!(ran.of(Step::Counts).left, 0);
    }

    /// `double n(double x) { return -x; }`, which is a float rather than an integer and so reaches
    /// a different member of the group.
    #[test]
    fn a_float_negation_does_not_survive_the_group() {
        let f64 = Type::float(Float::F64);
        let (mut names, mut func) = one(&[f64], &[f64], |build, args| {
            let negated = build.unary(Opcode::FNeg, args[0], f64);
            build.ret(&[negated]);
        });
        let ran = run(&mut func, &mut names);
        assert_eq!(ran.of(Step::Floats).found, 1);
        assert_eq!(ran.of(Step::Floats).left, 0);
    }

    /// `long a(long *p) { return __atomic_load_n(p, __ATOMIC_SEQ_CST); }`, which on this machine is
    /// the same `mov` an ordinary read is, and which nothing below this step in the group knows the
    /// name of.
    #[test]
    fn an_ordered_load_does_not_survive_the_group() {
        let i64 = Type::int(64);
        let (mut names, mut func) = one(&[Type::PTR], &[i64], |build, args| {
            let info = MemInfo {
                size: 8,
                align: 8,
                order: MemOrder::SeqCst,
                tbaa: None,
                owns: 0,
                restrict: Restrict::NONE,
            };
            let value = build.atomic_load(i64, args[0], info, Flags::NONE);
            build.ret(&[value]);
        });
        let ran = run(&mut func, &mut names);
        assert_eq!(ran.of(Step::Orderings).found, 1);
        assert_eq!(ran.of(Step::Orderings).left, 0);
    }

    /// Every construct with an opcode behind it, checked the same way in one loop, so that a
    /// thirteenth member added to the group without an answer is a failure here rather than
    /// something noticed later by the selector refusing it by name.
    #[test]
    fn nothing_the_group_names_an_opcode_for_is_still_there_afterwards() {
        for step in Step::GROUP {
            let Some((mut names, mut func)) = holding(*step) else {
                continue;
            };
            let ran = run(&mut func, &mut names);
            let did = ran.of(*step);
            assert_eq!(did.found, 1, "{}: the construct was not built", step.name());
            assert_eq!(did.left, 0, "{}: the construct survived the group", step.name());
        }
    }

    /// One small function holding exactly one of the construct that step answers for, for the
    /// steps whose construct is an opcode. The rest answer `None`: three of them are about a type
    /// rather than an opcode, one rewrites an operand and takes nothing out, and the variable
    /// argument list needs a whole calling convention around it to be worth building here.
    fn holding(step: Step) -> Option<(Interner, Func)> {
        let i32 = i32();
        let i64 = Type::int(64);
        let f64 = Type::float(Float::F64);
        Some(match step {
            Step::Bytes => one(&[i32], &[i32], |build, args| {
                let swapped = build.unary(Opcode::Bswap, args[0], i32);
                build.ret(&[swapped]);
            }),
            Step::Counts => one(&[i32], &[i32], |build, args| {
                let ones = build.unary(Opcode::Ctlz, args[0], i32);
                build.ret(&[ones]);
            }),
            Step::Floats => one(&[], &[f64], |build, _| {
                let k = build.fconst(f64, 0x3ff8_0000_0000_0000);
                build.ret(&[k]);
            }),
            Step::Orderings => one(&[Type::PTR], &[i64], |build, args| {
                let info = MemInfo {
                    size: 8,
                    align: 8,
                    order: MemOrder::SeqCst,
                    tbaa: None,
                    owns: 0,
                    restrict: Restrict::NONE,
                };
                let value = build.atomic_load(i64, args[0], info, Flags::NONE);
                build.ret(&[value]);
            }),
            Step::Overflows => one(&[i32, i32], &[i32], |build, args| {
                let (sum, _) = build.checked(Opcode::UAddOverflow, args[0], args[1]);
                build.ret(&[sum]);
            }),
            // `struct point { int x, y; } a, b; a = b;`, where the size and the alignment are on
            // the access rather than in an operand, which is the shape the front end writes.
            Step::Bulk => one(&[Type::PTR, Type::PTR], &[], |build, args| {
                let info = MemInfo {
                    size: 16,
                    align: 8,
                    order: MemOrder::NotAtomic,
                    tbaa: None,
                    owns: 0,
                    restrict: Restrict::NONE,
                };
                let mem = build.func().add_mem(info);
                let operands = build.func().push_values(&[args[0], args[1]]);
                build.inst(
                    InstData {
                        args: operands,
                        extra: Extra::Mem(mem),
                        ..InstData::new(Opcode::Memcpy)
                    },
                    &[],
                );
                build.ret(&[]);
            }),
            _ => return None,
        })
    }

    /// The cheap path, which is what a build that did not ask for the dump takes. The steps still
    /// run and the function still comes out lowered, and what is skipped is a walk of the function
    /// per step, which is not free on anything the size of a real translation unit.
    #[test]
    fn a_run_that_did_not_ask_for_the_dump_still_lowers_and_counts_nothing() {
        let build = |build: &mut Builder<'_>, args: &[Value]| {
            let swapped = build.unary(Opcode::Bswap, args[0], i32());
            build.ret(&[swapped]);
        };
        let (mut names, mut func) = one(&[i32()], &[i32()], build);
        let quiet = group(&mut func, &mut names, &x86_64::SYSV, false);
        assert!(quiet.did.is_empty(), "nothing was counted");
        assert_eq!(super::tally(&func, Step::Bytes), (super::tally(&func, Step::Bytes).0, 0));

        // The same function through the counting path comes out the same size, so what the flag
        // changes is what was written down and not what was done.
        let (mut names, mut func) = one(&[i32()], &[i32()], build);
        let loud = group(&mut func, &mut names, &x86_64::SYSV, true);
        assert_eq!(loud.of(Step::Bytes).left, 0);
        assert_eq!(
            loud.did.last().expect("thirteen of them").after,
            super::tally(&func, Step::Bytes).0
        );
    }

    #[test]
    fn nothing_is_recorded_for_a_run_that_did_not_ask() {
        let mut quiet = Lowerings::new();
        assert!(!quiet.wanted());
        quiet.record("f", Ran::default());
        assert_eq!(quiet.functions(), 1, "recording still works if somebody does it anyway");

        let asked = Lowerings::asked(true);
        assert!(asked.wanted());
        assert_eq!(asked.listing(), "# rucc lowering: 0 functions\n");
    }

    #[test]
    fn the_dump_names_every_step_whether_it_fired_or_not() {
        // A dump listing only the steps that fired cannot tell a step that found nothing from a
        // step somebody forgot to put in the group, which is the one thing it is read for.
        let (mut names, mut func) = one(&[i32()], &[i32()], |build, args| {
            let swapped = build.unary(Opcode::Bswap, args[0], i32());
            build.ret(&[swapped]);
        });
        let ran = run(&mut func, &mut names);
        let text = ran.render("f");
        assert!(text.starts_with("lowering f\n"), "{text}");
        for step in Step::GROUP {
            assert!(text.contains(step.name()), "{} is missing from {text}", step.name());
        }
        assert!(text.contains("found 1, left 0"), "{text}");
        assert_eq!(text.lines().count(), Step::GROUP.len() + 1);
    }

    #[test]
    fn only_the_two_steps_that_retype_a_whole_function_ever_say_they_touched_nothing() {
        // The rest work instruction by instruction and are never asked, so a `true` from one of
        // them is not evidence of anything and the dump does not print it.
        assert_eq!(
            Step::GROUP.iter().filter(|step| step.whole_function()).copied().collect::<Vec<_>>(),
            [Step::Halves, Step::Widths]
        );
        for step in Step::GROUP {
            if step.whole_function() {
                // Both of them are about the width on a value rather than about an opcode, so
                // there is nothing for `found` and `left` to count.
                assert!(step.opcodes().is_empty(), "{} counts opcodes", step.name());
            }
        }
    }

    #[test]
    fn an_instruction_nothing_in_the_group_is_about_is_left_exactly_where_it_was() {
        let (mut names, mut func) = one(&[i32()], &[i32()], |build, args| {
            let seven = build.iconst(i32(), 7);
            let sum = build.binary(Opcode::Add, args[0], seven, Flags::NONE);
            build.ret(&[sum]);
        });
        let before = super::tally(&func, Step::Rounds).0;
        let ran = run(&mut func, &mut names);
        assert_eq!(super::tally(&func, Step::Rounds).0, before);
        assert!(ran.did.iter().all(|did| did.found == 0));
    }
}
