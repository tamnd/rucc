//! What a call is allowed to do, which is the question every pass asks before it moves one.
//!
//! Design: section 41.3 of `spec/optimizer/41-correctness.md`. Documents 08, 17, 20 and 34 all
//! depend on classifying calls and all of them left the completeness argument to that section.
//!
//! # Not a boolean, and not a lattice anybody may extend casually
//!
//! GCC's version is the nineteen `ECF_` bits at `gcc/tree-core.h:46`. The ones that decide
//! anything are `ECF_CONST`, which is a result that depends only on the arguments, and `ECF_PURE`,
//! which reads memory but does not write it, and then `ECF_LOOPING_CONST_OR_PURE`, which is the
//! one worth noticing: a function's result can depend only on its arguments while the function
//! still fails to return, and deleting a call to one of those is not the same decision. GCC keeps
//! the two properties apart and so does [`Purity`].
//!
//! # Opaque is the default and it is the most conservative answer
//!
//! There is no `Unknown` here. Where nothing is known the answer is [`Purity::Opaque`], which
//! permits everything, so a classifier that has not been taught about something produces a missed
//! optimization rather than a wrong program. The library table below can only ever strengthen an
//! answer, which means a name missing from it costs nothing and a wrong entry in it is a
//! miscompilation. That is the bar for adding one.
//!
//! # Exhaustive over what is being called
//!
//! [`Facts::purity_of`] matches on [`Callee`] with no wildcard arm. Adding a new kind of callee to
//! the IR is then a compile error here until somebody says what it can do, which is the one thing
//! Rust offers a compiler over C++ in this file and is not worth giving away to save four lines.
//!
//! # What the user wrote and what the compiler worked out are separate
//!
//! A person writing `__attribute__((const))` on a function that is not const is asserting
//! something, and the compiler honours the assertion. Document 34's analysis will work out its own
//! answer for the functions it can see, and that answer lives in a different field of [`Facts`],
//! because keeping them apart is what makes it possible to check one against the other later. The
//! two are combined with [`Purity::stronger`] at the point of use and nowhere else.

use std::collections::{HashMap, HashSet};

use rucc_base::{Interner, Symbol};
use rucc_ir::{AttrSet, Extra, Func, Inst, Module, Opcode};

/// What a call can do.
///
/// Five, because there are two questions with two answers each and then everything else. Does the
/// result depend on memory, does the call come back, and if either answer is not known then the
/// call is [`Purity::Opaque`] and no pass may assume anything at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purity {
    /// Reads no memory, writes none, and comes back. `__attribute__((const))`, GCC's `ECF_CONST`.
    Const,
    /// Reads no memory and writes none, and may not come back. GCC's `ECF_CONST` together with
    /// `ECF_LOOPING_CONST_OR_PURE`.
    LoopingConst,
    /// Reads memory, writes none, and comes back. `__attribute__((pure))`, GCC's `ECF_PURE`.
    Pure,
    /// Reads memory, writes none, and may not come back.
    LoopingPure,
    /// Anything, which is what a call is until something says otherwise.
    Opaque,
}

impl Purity {
    /// The five, for a test that walks them.
    pub const ALL: [Self; 5] =
        [Self::Const, Self::LoopingConst, Self::Pure, Self::LoopingPure, Self::Opaque];

    /// How it reads in a dump.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Const => "const",
            Self::LoopingConst => "const, may not return",
            Self::Pure => "pure",
            Self::LoopingPure => "pure, may not return",
            Self::Opaque => "opaque",
        }
    }

    /// Whether the call may read memory the caller cares about.
    #[must_use]
    pub const fn reads_memory(self) -> bool {
        match self {
            Self::Const | Self::LoopingConst => false,
            Self::Pure | Self::LoopingPure | Self::Opaque => true,
        }
    }

    /// Whether the call may write memory.
    ///
    /// Only an opaque call may. That is what the other four have in common and it is most of what
    /// makes them worth telling apart from the rest.
    #[must_use]
    pub const fn writes_memory(self) -> bool {
        matches!(self, Self::Opaque)
    }

    /// Whether control is known to come back from the call.
    ///
    /// Not known and known not to are the same answer here, because both of them stop the same
    /// transformations. Which of the two it is belongs to `noreturn`, which is an attribute on the
    /// function rather than a level of this.
    #[must_use]
    pub const fn terminates(self) -> bool {
        matches!(self, Self::Const | Self::Pure)
    }

    /// Whether the result is a function of the arguments and nothing else.
    ///
    /// This is what lets two calls with the same arguments become one call with no question asked
    /// about what happened to memory in between. A [`Purity::Pure`] call can be folded the same way
    /// when the caller can show nothing wrote memory between the two, which is a question for the
    /// alias analysis and not for this.
    #[must_use]
    pub const fn depends_only_on_arguments(self) -> bool {
        !self.reads_memory() && !self.writes_memory()
    }

    /// Whether a call whose result nothing reads may be removed.
    ///
    /// Both halves are needed. A call that writes memory does something even when its result is
    /// thrown away, and a call that may not come back does something by not coming back, which is
    /// why the looping levels exist at all.
    #[must_use]
    pub const fn can_be_deleted_when_unused(self) -> bool {
        !self.writes_memory() && self.terminates()
    }

    /// The strongest thing true of both, for a caller that has two sources and believes each.
    ///
    /// [`Purity::Opaque`] is nothing known, so it gives way to whatever the other source says. Two
    /// sources that each know half give the whole: a declaration saying the result comes out of the
    /// arguments and an analysis saying the loop inside terminates add up to [`Purity::Const`].
    #[must_use]
    pub const fn stronger(self, other: Self) -> Self {
        match (self, other) {
            (Self::Opaque, it) | (it, Self::Opaque) => it,
            (one, two) => Self::of(
                one.reads_memory() && two.reads_memory(),
                one.terminates() || two.terminates(),
            ),
        }
    }

    /// The strongest thing true of either, for a caller that has to cover both.
    ///
    /// Which is what a call site with more than one possible callee needs, and what a caller
    /// summarising a whole function needs.
    #[must_use]
    pub const fn weaker(self, other: Self) -> Self {
        match (self, other) {
            (Self::Opaque, _) | (_, Self::Opaque) => Self::Opaque,
            (one, two) => Self::of(
                one.reads_memory() || two.reads_memory(),
                one.terminates() && two.terminates(),
            ),
        }
    }

    /// The level with those two answers, which is the four that are not opaque.
    const fn of(reads: bool, terminates: bool) -> Self {
        match (reads, terminates) {
            (false, true) => Self::Const,
            (false, false) => Self::LoopingConst,
            (true, true) => Self::Pure,
            (true, false) => Self::LoopingPure,
        }
    }
}

impl std::fmt::Display for Purity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What is being called.
///
/// The thing [`Facts::purity_of`] is exhaustive over. The closed intrinsics are not here because
/// they are opcodes rather than calls and each carries its own meaning in the opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Callee {
    /// A named function, which may or may not be one this module defines.
    Direct(Symbol),
    /// A call through an address.
    Indirect,
    /// A target-specific intrinsic, named on the instruction, which is the open half of the
    /// intrinsic set and is where the vector builtins land.
    Intrinsic(Symbol),
    /// Inline assembly, including `asm goto`.
    Asm,
}

impl Callee {
    /// What this instruction calls, and `None` for an instruction that calls nothing.
    #[must_use]
    pub fn of(func: &Func, inst: Inst) -> Option<Self> {
        let data = &func[inst];
        match data.opcode {
            Opcode::Call | Opcode::TailCall | Opcode::CallIndirect => match data.extra {
                Extra::Call(at) => Some(match func[at].callee {
                    Some(name) => Self::Direct(name),
                    None => Self::Indirect,
                }),
                _ => Some(Self::Indirect),
            },
            Opcode::TargetIntrinsic => match data.extra {
                Extra::Symbol(name) => Some(Self::Intrinsic(name)),
                _ => Some(Self::Asm),
            },
            Opcode::InlineAsm => Some(Self::Asm),
            _ => None,
        }
    }
}

/// What is known about the functions a call could reach.
///
/// Built once from the module, because the attributes belong to the callee and there is one callee
/// and many call sites. A caller with no module has [`Facts::nothing`], which answers
/// [`Purity::Opaque`] to everything and is correct.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    declared: HashMap<Symbol, AttrSet>,
    inferred: HashMap<Symbol, Purity>,
    from_the_library: HashMap<Symbol, Purity>,
}

impl Facts {
    /// Nothing known about anything, which is what a pass holding one function has.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// What the module says about each of its functions.
    ///
    /// The interner is here for the library table, which is written in text because that is what
    /// the C standard names. Nothing after this call needs it.
    #[must_use]
    pub fn of_module(module: &Module, names: &Interner) -> Self {
        let mut facts = Self::default();
        let mut defined = HashSet::new();
        for id in module.funcs() {
            let func = &module[id];
            facts.declared.insert(func.name, func.attrs.set);
            if !func.is_declaration() {
                defined.insert(func.name);
            }
        }
        // A name this module defines is not the library's, whatever it is spelled, because the
        // definition in hand is the function that will be called.
        for &name in facts.declared.keys() {
            if defined.contains(&name) {
                continue;
            }
            if let Some(purity) = library_purity(names.resolve(name)) {
                facts.from_the_library.insert(name, purity);
            }
        }
        facts
    }

    /// Turns off the whole library table, which is `-fno-builtin` and `-ffreestanding`.
    ///
    /// A freestanding program has no C library for the name to be the name of, and a program that
    /// means its own thing by `strlen` is the reason the flag exists.
    pub fn without_the_library(&mut self) {
        self.from_the_library.clear();
    }

    /// Takes one name away from the table, which is `-fno-builtin-<name>`.
    ///
    /// What a build that means its own `memcpy` and the library's everything else writes, which is
    /// what the kernel does for a handful of names.
    pub fn not_the_library_name(&mut self, name: Symbol) {
        self.from_the_library.remove(&name);
    }

    /// Records what document 34's analysis worked out about a function.
    ///
    /// A separate field from the declaration on purpose. The two are combined where they are read
    /// and are never written over each other, so that a later build can check one against the other
    /// and report the function whose attribute was a lie.
    pub fn record_inferred(&mut self, name: Symbol, purity: Purity) {
        self.inferred.insert(name, purity);
    }

    /// What the user declared about this function, on its own.
    #[must_use]
    pub fn declared(&self, name: Symbol) -> Purity {
        match self.declared.get(&name) {
            Some(&set) => from_attributes(set),
            None => Purity::Opaque,
        }
    }

    /// What analysis worked out about this function, on its own.
    #[must_use]
    pub fn inferred(&self, name: Symbol) -> Purity {
        self.inferred.get(&name).copied().unwrap_or(Purity::Opaque)
    }

    /// What this call can do.
    ///
    /// The match has no wildcard arm and adding a kind of callee should keep it that way.
    #[must_use]
    pub fn purity_of(&self, callee: Callee) -> Purity {
        match callee {
            Callee::Direct(name) => self.of_name(name),
            // The address could be anything with its own definition, including a function this
            // module never saw. Document 34's call graph narrows this and until then it does not.
            Callee::Indirect => Purity::Opaque,
            // The open half of the intrinsic set is named rather than enumerated, so nothing here
            // knows what one does. The closed half are opcodes and never reach this.
            Callee::Intrinsic(_) => Purity::Opaque,
            // A template the compiler does not read, with a clobber list it has to believe.
            Callee::Asm => Purity::Opaque,
        }
    }

    /// Everything known about a named function, from all three sources.
    fn of_name(&self, name: Symbol) -> Purity {
        let mut purity = self.declared(name).stronger(self.inferred(name));
        if let Some(&known) = self.from_the_library.get(&name) {
            purity = purity.stronger(known);
        }
        purity
    }
}

/// What an attribute set says on its own.
///
/// `noreturn` is what turns either level into its looping one. A call that does not come back does
/// something by not coming back, however little it touches, and that is the case
/// `ECF_LOOPING_CONST_OR_PURE` exists for.
fn from_attributes(set: AttrSet) -> Purity {
    let terminates = !set.contains(AttrSet::NORETURN);
    if set.contains(AttrSet::READNONE) {
        return Purity::of(false, terminates);
    }
    if set.contains(AttrSet::READONLY) {
        return Purity::of(true, terminates);
    }
    Purity::Opaque
}

/// What the C standard library functions do, for the ones where the answer is not arguable.
///
/// Only entries that strengthen the answer are here, so a name that is missing costs a missed
/// optimization and a name that is wrong costs a wrong program. Nothing that writes memory, sets
/// `errno`, touches a stream or allocates belongs in here, which rules out most of the library and
/// all of `<math.h>`, since a math function sets `errno` unless the command line says it does not.
///
/// Sorted, and a test checks that it is sorted and says each name once.
const LIBRARY: &[(&str, Purity)] = &[
    ("abs", Purity::Const),
    ("imaxabs", Purity::Const),
    ("labs", Purity::Const),
    ("llabs", Purity::Const),
    ("memchr", Purity::Pure),
    ("memcmp", Purity::Pure),
    ("strchr", Purity::Pure),
    ("strcmp", Purity::Pure),
    ("strcspn", Purity::Pure),
    ("strlen", Purity::Pure),
    ("strncmp", Purity::Pure),
    ("strnlen", Purity::Pure),
    ("strpbrk", Purity::Pure),
    ("strrchr", Purity::Pure),
    ("strspn", Purity::Pure),
    ("strstr", Purity::Pure),
];

/// What the library says about a name, under either spelling.
///
/// The `__builtin_` prefix is the program saying which function it means, so it reaches the same
/// entry. Whether the plain spelling is allowed to is decided before this is called.
fn library_purity(name: &str) -> Option<Purity> {
    let name = name.strip_prefix("__builtin_").unwrap_or(name);
    LIBRARY.binary_search_by_key(&name, |&(named, _)| named).ok().map(|at| LIBRARY[at].1)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        AsmInfo, AttrSet, BlockCallList, Builder, CallInfo, Extra, Flags, Func, InstData, Module,
        Opcode, Signature, Type,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{Callee, Facts, LIBRARY, Purity};

    /// A module with those functions in it, declared unless they are asked to have a body.
    fn module(named: &[(&str, bool, AttrSet)]) -> (Interner, Module) {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        for &(name, defined, attrs) in named {
            let mut func = Func::new(names.intern(name), Signature::new());
            func.attrs.set = attrs;
            if defined {
                let block = func.create_block();
                let mut build = Builder::new(&mut func, block);
                let zero = build.iconst(Type::int(32), 0);
                build.ret(&[zero]);
            }
            module.add_func(func);
        }
        (names, module)
    }

    /// What the module says about a name.
    fn purity(names: &mut Interner, module: &Module, name: &str) -> Purity {
        let facts = Facts::of_module(module, names);
        let symbol = names.intern(name);
        facts.purity_of(Callee::Direct(symbol))
    }

    #[test]
    fn a_function_nobody_promised_anything_about_is_opaque() {
        let (mut names, module) = module(&[("f", true, AttrSet::NONE)]);
        assert_eq!(purity(&mut names, &module, "f"), Purity::Opaque);
    }

    #[test]
    fn a_name_this_module_never_heard_of_is_opaque_as_well() {
        let (mut names, module) = module(&[("f", true, AttrSet::NONE)]);
        let facts = Facts::of_module(&module, &names);
        assert_eq!(facts.purity_of(Callee::Direct(names.intern("g"))), Purity::Opaque);
    }

    #[test]
    fn the_const_attribute_is_honoured_because_the_user_asserted_it() {
        let (mut names, module) = module(&[("f", false, AttrSet::READNONE)]);
        let purity = purity(&mut names, &module, "f");
        assert_eq!(purity, Purity::Const);
        assert!(purity.depends_only_on_arguments());
        assert!(purity.can_be_deleted_when_unused());
    }

    #[test]
    fn the_pure_attribute_reads_memory_and_writes_none() {
        let (mut names, module) = module(&[("f", false, AttrSet::READONLY)]);
        let purity = purity(&mut names, &module, "f");
        assert_eq!(purity, Purity::Pure);
        assert!(purity.reads_memory());
        assert!(!purity.writes_memory());
        assert!(!purity.depends_only_on_arguments());
        assert!(purity.can_be_deleted_when_unused());
    }

    #[test]
    fn a_const_function_that_does_not_come_back_may_not_be_deleted() {
        // Which is the whole reason the looping levels are in the enum. Its result depends only
        // on its arguments and the call still does something, which is not come back.
        let (mut names, module) =
            module(&[("f", false, AttrSet::READNONE.union(AttrSet::NORETURN))]);
        let purity = purity(&mut names, &module, "f");
        assert_eq!(purity, Purity::LoopingConst);
        assert!(purity.depends_only_on_arguments());
        assert!(!purity.can_be_deleted_when_unused());
    }

    #[test]
    fn nothing_that_is_not_a_direct_call_is_anything_but_opaque() {
        let (mut names, module) = module(&[("f", true, AttrSet::READNONE)]);
        let facts = Facts::of_module(&module, &names);
        // Even though the module holds a const function of that name, none of these is known to
        // be it, and each is opaque for its own reason.
        assert_eq!(facts.purity_of(Callee::Indirect), Purity::Opaque);
        assert_eq!(facts.purity_of(Callee::Asm), Purity::Opaque);
        let vector = names.intern("__builtin_ia32_paddb");
        assert_eq!(facts.purity_of(Callee::Intrinsic(vector)), Purity::Opaque);
    }

    #[test]
    fn the_library_names_are_known_under_both_spellings() {
        let (mut names, module) = module(&[
            ("strlen", false, AttrSet::NONE),
            ("abs", false, AttrSet::NONE),
            ("__builtin_strlen", false, AttrSet::NONE),
            ("printf", false, AttrSet::NONE),
        ]);
        assert_eq!(purity(&mut names, &module, "strlen"), Purity::Pure);
        assert_eq!(purity(&mut names, &module, "__builtin_strlen"), Purity::Pure);
        assert_eq!(purity(&mut names, &module, "abs"), Purity::Const);
        // Everything else in the library, which is most of it, is opaque and stays that way.
        assert_eq!(purity(&mut names, &module, "printf"), Purity::Opaque);
    }

    #[test]
    fn a_module_that_defines_strlen_means_its_own() {
        let (mut names, module) = module(&[("strlen", true, AttrSet::NONE)]);
        assert_eq!(purity(&mut names, &module, "strlen"), Purity::Opaque);
    }

    #[test]
    fn no_builtin_takes_the_table_away_and_the_named_form_takes_one_entry() {
        let (mut names, module) =
            module(&[("strlen", false, AttrSet::NONE), ("abs", false, AttrSet::NONE)]);
        let mut facts = Facts::of_module(&module, &names);
        let strlen = names.intern("strlen");
        let abs = names.intern("abs");
        facts.not_the_library_name(strlen);
        assert_eq!(facts.purity_of(Callee::Direct(strlen)), Purity::Opaque);
        assert_eq!(facts.purity_of(Callee::Direct(abs)), Purity::Const);
        facts.without_the_library();
        assert_eq!(facts.purity_of(Callee::Direct(abs)), Purity::Opaque);
    }

    #[test]
    fn what_the_user_wrote_and_what_analysis_worked_out_are_kept_apart() {
        let (mut names, module) =
            module(&[("f", true, AttrSet::READNONE.union(AttrSet::NORETURN))]);
        let mut facts = Facts::of_module(&module, &names);
        let f = names.intern("f");
        assert_eq!(facts.declared(f), Purity::LoopingConst);
        assert_eq!(facts.inferred(f), Purity::Opaque);
        // Document 34 gets to say the loop inside it terminates. The declaration said the result
        // comes out of the arguments. Together that is const, and each is still readable on its
        // own, which is what makes checking one against the other possible later.
        facts.record_inferred(f, Purity::Pure);
        assert_eq!(facts.declared(f), Purity::LoopingConst);
        assert_eq!(facts.inferred(f), Purity::Pure);
        assert_eq!(facts.purity_of(Callee::Direct(f)), Purity::Const);
    }

    #[test]
    fn what_an_instruction_calls_is_read_off_the_instruction() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("caller"), Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(Signature::new());
        let direct = build.call(names.intern("f"), signature, &[]);
        let varargs = build.func().push_abis(&[]);
        let info = build.func().add_call(CallInfo { callee: None, signature, varargs });
        let indirect = build.inst(
            InstData { extra: Extra::Call(info), ..InstData::new(Opcode::CallIndirect) },
            &[],
        );
        let asm = build.inline_asm(
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
        let nothing = build.ret(&[]);

        let f = names.intern("f");
        assert_eq!(Callee::of(&func, nothing), None);
        assert_eq!(Callee::of(&func, direct), Some(Callee::Direct(f)));
        assert_eq!(Callee::of(&func, indirect), Some(Callee::Indirect));
        assert_eq!(Callee::of(&func, asm), Some(Callee::Asm));
    }

    #[test]
    fn the_two_ways_of_combining_are_the_lattice_they_claim_to_be() {
        for one in Purity::ALL {
            assert_eq!(one.stronger(one), one, "{one} is not idempotent");
            assert_eq!(one.weaker(one), one, "{one} is not idempotent");
            assert_eq!(one.stronger(Purity::Opaque), one, "opaque should say nothing");
            assert_eq!(one.weaker(Purity::Opaque), Purity::Opaque, "opaque covers everything");
            for two in Purity::ALL {
                assert_eq!(one.stronger(two), two.stronger(one), "{one} and {two} disagree");
                assert_eq!(one.weaker(two), two.weaker(one), "{one} and {two} disagree");
                // Whatever comes out of the weaker of the two permits whatever either permitted.
                let both = one.weaker(two);
                assert!(both.reads_memory() >= one.reads_memory());
                assert!(both.writes_memory() >= one.writes_memory());
                assert!(both.terminates() <= one.terminates());
            }
        }
    }

    #[test]
    fn only_an_opaque_call_may_write_memory() {
        for purity in Purity::ALL {
            assert_eq!(purity.writes_memory(), purity == Purity::Opaque, "{purity}");
            assert_eq!(purity.can_be_deleted_when_unused(), purity.terminates(), "{purity}");
        }
    }

    #[test]
    fn the_library_table_is_sorted_says_each_name_once_and_writes_no_memory() {
        // Sorted because the lookup is a binary search, and the rest because an entry here is
        // believed without being checked against anything.
        for pair in LIBRARY.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} and {} are out of order", pair[0].0, pair[1].0);
        }
        for &(name, purity) in LIBRARY {
            assert!(!purity.writes_memory(), "{name} would not be worth an entry");
            assert!(purity.terminates(), "{name} is in the table to be deletable");
            assert!(!name.starts_with("__builtin_"), "{name} is reached under both spellings");
        }
    }
}
