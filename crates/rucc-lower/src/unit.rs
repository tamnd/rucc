//! The module level of the walk: what a translation unit's declarations become.
//!
//! Design: `spec/08-ir.md` section 8.9.
//!
//! One typed tree becomes one [`Module`]. A file-scope object becomes a global with an image
//! built from its initializer, a function becomes a [`Func`] whose body is built by
//! [`body`](mod@crate::body), and a string literal becomes an unnamed constant global that
//! whatever mentioned it points at.
//!
//! # What an image is
//!
//! An initializer arrives here already flattened: one entry per scalar that is stored, each
//! with the byte offset it goes at, with every designator and every nested brace already
//! resolved. So building the image is a walk over the entries in offset order, filling the gaps
//! between them with zeros, and the only thing that has to be worked out per entry is whether
//! the value is a number, a run of bytes from a string literal, or the address of something the
//! linker has to place.
//!
//! # Names
//!
//! An object with linkage is known by the name it was written with, and there is nothing to
//! invent. A `static` inside a function has no linkage and still needs a name in the object
//! file, so it gets `name.N`, which is what gcc does and is why two functions may each have a
//! `static int count;` without colliding. A string literal has no name at all and gets
//! `.Lstr.N`, whose leading dot keeps it out of the symbol table on every target that has the
//! convention.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use rucc_base::hash::{Map, Set};
use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_ir::{
    Abi, Alias, AliasKind, AttrSet, Builder, CallInfo, DataList, Datum, Dll, Extra, FpContract,
    Func, FuncId, Global, Imm, InstData, Linkage as IrLinkage, Meta, Module, Opcode, Param, Reloc,
    Signature, SymbolRef, TlsModel, Type, Visibility as IrVisibility,
};
use rucc_sema::{
    Address, Base, Const, Conversion, DeclFlags, DeclId, DeclKind, Definition, Effects, Emission,
    Eval, ExprId, ExprKind, InitEntry, InitList, LabelId, Linkage, Priority, StorageDuration,
    StrId, Tast, Version, Visibility,
};
use rucc_target::{Convention, ObjectFormat, TargetInfo};
use rucc_types::{TypeId, TypeKind, Types, compatible, is_complex, is_scalar};

use crate::abi::{self, Plan};
use crate::aliasing;
use crate::body;
use crate::directives;
use crate::nest::{self, Nest};
use crate::reach;
use crate::repr;

/// Which functions get a stack protector, which is what the `-fstack-protector` family decides.
///
/// The question is about the locals a function has, so it is answered here and not in the back
/// end: by the time a frame is laid out the types are gone and every local is a size and an
/// alignment. What the back end then does about the answer is its own business, and it is carried
/// to it as [`rucc_ir::AttrSet::STACK_PROTECT`] on the function.
///
/// The names are gcc's, and so are the rules. A build that has been compiled with one of these for
/// twenty years is entitled to the same set of protected functions from a compiler claiming to be
/// compatible, because the ones left out are the ones an exploit goes looking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protector {
    /// None of them, which is `-fno-stack-protector` and what a command line that says nothing
    /// gets.
    #[default]
    None,
    /// A function with a local array of at least eight bytes, or one whose stack grows while it
    /// runs. `-fstack-protector`, which is the original and the narrowest.
    Buffers,
    /// Any of those, and any function with a local array at all, a local holding one, or a local
    /// whose address is taken. `-fstack-protector-strong`, which is what every distribution builds
    /// its packages with and therefore the one a real build line carries.
    Strong,
    /// Every function that has a frame at all. `-fstack-protector-all`.
    All,
    /// Only a function that asks for one with `__attribute__((stack_protect))`.
    /// `-fstack-protector-explicit`.
    Explicit,
}

/// What an automatic object with no initializer holds before the program writes it, which is
/// `-ftrivial-auto-var-init=`.
///
/// The kernel builds with `zero` under `CONFIG_INIT_STACK_ALL_ZERO`, which a distribution config
/// usually has on, and with `pattern` under `CONFIG_INIT_STACK_ALL_PATTERN`. Either one is written
/// where the declaration is reached, so a declaration a `switch` jumps past is left alone, as gcc
/// leaves it. A local that says `__attribute__((uninitialized))` is left alone too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoInit {
    /// Whatever was there, which is what C says and what a command line that says nothing gets.
    #[default]
    Uninitialized,
    /// Every byte zero, padding included.
    Zero,
    /// Every byte `0xfe`, the byte gcc repeats, except that padding is zero and so is a `bool`
    /// that is a whole object rather than a member of one.
    Pattern,
}

/// What overflows rather than being undefined, which is `-fwrapv` and its relatives.
///
/// Every licence the walk grants the optimizer about overflow is one flag on one instruction, and
/// withdrawing a licence is not setting it. So this is read where the flags are chosen and nowhere
/// else, and a unit built with either of these is a unit whose IR carries less rather than a unit
/// the passes are told something extra about. That is also what makes it correct across link time
/// optimization: a body from a unit that wraps and a body from one that does not keep their own
/// answers when they end up in the same module.
///
/// `-ftrapv` is the exception and is the reason this is not simply two flags. It is the other
/// answer to the question `-fwrapv` answers, and it is the only one of the three that asks for
/// something to be generated rather than for something to be left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wrapping {
    /// Whether signed arithmetic wraps, from `-fwrapv`. Set, and an add, a subtract, a multiply, a
    /// shift and a negation in a signed type stop saying they do not wrap.
    pub signed: bool,
    /// Whether pointer arithmetic wraps, from `-fwrapv-pointer`. Set, and the multiply that turns
    /// an index into a number of bytes stops saying so.
    ///
    /// That multiply is the whole of it here, because the addition itself never claimed anything: a
    /// `ptradd` carries no flags in this IR and no pass reads one off it.
    pub pointer: bool,
    /// Whether a signed overflow stops the program, from `-ftrapv`. Set, and an add, a subtract, a
    /// multiply and a negation in a signed type become calls to the routine in the runtime that
    /// does the arithmetic and checks it.
    ///
    /// Never set at the same time as [`Wrapping::signed`], because a program cannot both wrap and
    /// stop. The driver is what keeps that true.
    pub trap: bool,
}

/// Everything the walk reads, which is a checked translation unit and the target it is for.
///
/// The interner is mutable because the walk invents names the program never wrote: the label a
/// string literal is emitted under, and the mangled name of a function-scope `static`.
pub struct Context<'a> {
    /// The typed tree.
    pub tast: &'a Tast,
    /// The types it points into.
    pub types: &'a Types,
    /// What is being compiled for, which is where every width and every alignment comes from.
    pub target: &'a TargetInfo,
    /// The name table.
    pub names: &'a mut Interner,
    /// What a name that no declaration of it said anything about gets, which is `-fvisibility=`.
    ///
    /// A fact about the compilation rather than about any declaration, which is why it arrives
    /// here rather than on the tree: the checker knows what was written and this knows what the
    /// command line asked for, and the answer is the first of those where there is one.
    pub visibility: IrVisibility,
    /// Which functions get a stack protector, which is `-fstack-protector` and its relatives.
    pub protector: Protector,
    /// What overflows rather than being undefined, which is `-fwrapv` and its relatives.
    ///
    /// A fact about the compilation for the same reason the two above it are: what was written is
    /// on the tree and what was asked for is on the command line.
    pub wrapping: Wrapping,
    /// Whether an access carries the node for the type it goes through, which is
    /// `-fstrict-aliasing` and is on unless `-fno-strict-aliasing` cleared it.
    ///
    /// Clearing it here rather than in the optimizer is what makes the flag one condition in one
    /// place: an access with no node conflicts with every other access, so a unit built with the
    /// flag off is a unit whose IR says less rather than a unit the passes are told something
    /// extra about. That is also what keeps it right across link time optimization, the way
    /// [`Context::wrapping`] is: a body from a unit that named its types and a body from one that
    /// did not keep their own answers when they end up in the same module.
    pub aliasing: bool,
    /// Whether an access says how far the padding after it reaches, which is
    /// `-fsafety-init=nopadding` and is what a build with no safety tier gets too, since nothing
    /// reads the number then.
    ///
    /// Here rather than in the safety pass for the reason [`Context::aliasing`] is here: what the
    /// number is takes a record's layout, and the layout is a thing the walk has in hand and the
    /// pass over the IR does not. The pass reads it and does not decide anything, which keeps the
    /// flag one condition in one place and keeps it right across link time optimization.
    pub padding: bool,
    /// How far a multiply and an addition may be fused into one rounding, which is
    /// `-ffp-contract=`.
    ///
    /// A fact about the compilation like the ones above it, and the one of them that is written
    /// down rather than acted on: it goes onto every function with a body as
    /// [`rucc_ir::Attrs::fp_contract`], because the place that would fuse anything is the code
    /// generator and by the time it runs the command line is gone and the two operations it might
    /// fuse may have come from different statements.
    pub contract: FpContract,
    /// What every function in the unit is aligned to unless it asked for more itself, which is
    /// `-falign-functions` and is `None` for the alignment the target gives anyway.
    ///
    /// A fact about the compilation like the ones above it, and it meets a fact about a
    /// declaration here rather than further down: `__attribute__((aligned(N)))` is a statement
    /// about one function and this is a preference about all of them, so the function takes the
    /// larger of the two and everything below reads one number.
    pub align: Option<u32>,
    /// Whether every function calls the two profiling hooks on the way in and on the way out,
    /// which is `-finstrument-functions`.
    ///
    /// Done here rather than in a pass because gcc does it before it inlines anything, so a body
    /// that is inlined takes its two calls with it and they still name the function they were
    /// written for. A pass over the IR would see the body only after that had happened to it.
    pub instrument: bool,
    /// Whether an exception may unwind through what the walk builds, which is `-fexceptions`.
    ///
    /// What it asks of C is that a `cleanup` handler run when an unwind passes through its scope,
    /// and that takes a landing pad and a table the personality routine reads, neither of which is
    /// built yet. So what it does here is turn down the one shape the missing pad would change the
    /// meaning of, a call made while a handler is owed, rather than build it without the pad.
    pub exceptions: bool,
    /// Whether an instruction that can trap may unwind as well as a call, which is
    /// `-fnon-call-exceptions`.
    ///
    /// A load or a store through a pointer and a division can raise a signal, and a handler that
    /// throws or calls `pthread_exit` from there unwinds out of that instruction. gcc covers such
    /// an instruction with the same landing pad a call in the scope gets, so the `cleanup`
    /// handlers owed there run, and this asks the walk to do the same.
    pub non_call_exceptions: bool,
    /// Whether the walk says where the lifetime of a local in memory ends, which is a
    /// `lifetime_end` wherever control leaves the block the local was declared in by falling out
    /// of it or by a `break` or a `continue`.
    ///
    /// Asked for by a build that lets two locals share bytes in the frame, which is what the
    /// markers are for, and by nothing else. A build that does not share has nothing to read them
    /// and every pass in between would only have one more instruction to walk past, and a build
    /// with the safety instrumentation keeps every local in bytes of its own, since the checks it
    /// puts in were written against a frame in which no two locals share any.
    pub lifetimes: bool,
    /// Whether a tentative definition with external linkage is a common symbol, which is
    /// `-fcommon` and the default on Darwin, rather than a zeroed object in `.bss`.
    pub common: bool,
    /// Whether a call to a library function by its plain name may be taken to mean that function,
    /// which is `-fno-builtin` and `-ffreestanding` turned around.
    ///
    /// Read for `memcpy`, `memset` and `memmove` with a small constant length, which are built as
    /// the IR's own copy and fill rather than as calls, as gcc expands them. A `__builtin_` spelling
    /// means the library whatever this says.
    pub builtins: bool,
    /// The names `-fno-builtin-<name>` took away one at a time, without the prefix.
    pub no_builtin: &'a [String],
    /// What a local with no initializer starts out holding, which is `-ftrivial-auto-var-init=`.
    pub auto_init: AutoInit,
    /// Whether two locals whose addresses are taken may be given one slot when the blocks that
    /// declare them never overlap, which is `-fstack-reuse=` and on above `-O0`.
    ///
    /// The code generator shares the bytes of two locals it can follow the addresses of, and not
    /// of any other. An array handed to a call is one it cannot follow, and a function with four
    /// of them in four blocks one after another kept all four where gcc keeps one. The blocks are
    /// what says they are never wanted at once, and only the walk still has the blocks, so this
    /// is decided here, as one `alloca` for all of them, which is a thing every pass after it
    /// already reads correctly. The rules are on `Body::declare`.
    pub share: bool,
    /// Whether a structure passed by value in the argument area may be used where the caller left
    /// it rather than copied into a slot of its own, which is everywhere but a build with the
    /// safety instrumentation.
    ///
    /// The instrumentation knows an object by the memory the function or its caller made for it,
    /// and the bytes the caller put in the argument area are not an object it made, so a build
    /// with it keeps the copy and checks the slot the body works on. See `Body::in_place`.
    pub in_place: bool,
    /// Whether `x18` is kept for something else on AArch64, which is `-ffixed-x18`.
    ///
    /// The kernel keeps its shadow call stack there. Nothing here ever hands `x18` to a value, so
    /// the flag changes nothing about an ordinary function, but a nested function's static chain
    /// travels in it, and a nested function under the flag is refused rather than allowed to
    /// write over whatever the build keeps there.
    pub fixed_x18: bool,
    /// Whether the objects are laid out newest first, which is what gcc does when it optimizes.
    /// See `Module::reverse_globals`.
    pub reorder: bool,
    /// The extensions the command line builds for, which a function's own `target` attribute
    /// stands in place of.
    ///
    /// Only read to say whether an operator over a whole vector may be one vector instruction,
    /// which on x86-64 needs SSE2. Under `-mno-sse` or `-mgeneral-regs-only` there is no register
    /// to hold the vector in, and the lanes go through memory one at a time the way they did
    /// before. See `Body::whole_vector`.
    pub isa: rucc_target::Isa,
    /// How a file named by a `.incbin` in an `asm` at file scope is read, given the name as the
    /// template wrote it and handing back either the bytes or what went wrong.
    ///
    /// Passed in rather than reached for, because the walk has no business opening files and
    /// because a caller that put its sources somewhere other than a disk has put this file there
    /// too. The name is resolved the way an assembler resolves it, which is against the directory
    /// the compiler was run in and not against the directory the source was found in.
    pub read: &'a mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
}

// Written out rather than derived because a closure has no `Debug`, and printing one would say
// nothing anyway. What is worth reading here is the settings, so those are what this prints.
impl fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("visibility", &self.visibility)
            .field("protector", &self.protector)
            .field("wrapping", &self.wrapping)
            .field("aliasing", &self.aliasing)
            .field("padding", &self.padding)
            .field("contract", &self.contract)
            .field("align", &self.align)
            .field("instrument", &self.instrument)
            .field("exceptions", &self.exceptions)
            .field("non_call_exceptions", &self.non_call_exceptions)
            .field("common", &self.common)
            .field("builtins", &self.builtins)
            .field("no_builtin", &self.no_builtin)
            .field("auto_init", &self.auto_init)
            .field("share", &self.share)
            .field("in_place", &self.in_place)
            .field("fixed_x18", &self.fixed_x18)
            .field("reorder", &self.reorder)
            .field("isa", &self.isa)
            .finish_non_exhaustive()
    }
}

/// One function that runs without anything calling it, waiting for the section it goes in.
///
/// Held back rather than written where the definition is met, because the order they go in is not
/// always the order the file defined them: a format with one section for all of them is a format
/// where the only record of the priority is the position in that section, so they have to be
/// sorted, and sorting means having all of them.
#[derive(Debug, Clone, Copy)]
struct Start {
    /// The function the entry is the address of.
    func: Symbol,
    /// Whether it runs in the run-up to `main` rather than in the run-down after it.
    before: bool,
    /// Where in the order the attribute asked for it to go.
    priority: Priority,
    /// The definition it came from, for the diagnostic a format with no way to say it needs.
    span: Span,
}

impl Start {
    /// Where this goes among the others, which is the order the entries are written in.
    ///
    /// A lower number first, and the unnumbered ones after every numbered one, which is the order
    /// an ELF linker puts the sections in and therefore the order every format has to come out in
    /// for the three of them to agree. The sort is stable, so two at the same priority stay in the
    /// order the file defined them, which is all that decides between them.
    fn order(&self) -> (u8, u16) {
        match self.priority {
            Priority::Numbered(number) => (0, number),
            Priority::Unnumbered => (1, 0),
        }
    }
}

/// What the walk produced.
#[derive(Debug)]
pub struct Lowered {
    /// The module, which is complete even when something was reported: a construct that is not
    /// supported yet leaves the rest of the function around it intact.
    pub module: Module,
    /// What was reported, in the order it was found.
    pub diagnostics: Vec<Diagnostic>,
}

/// Walks a checked translation unit and builds the IR for it.
///
/// `name` is the module's name, which is the file the tree came from.
#[must_use]
pub fn lower(name: &str, cx: Context<'_>) -> Lowered {
    let Context {
        tast,
        types,
        target,
        names,
        visibility,
        protector,
        wrapping,
        aliasing,
        padding,
        contract,
        align,
        instrument,
        exceptions,
        non_call_exceptions,
        lifetimes,
        common,
        builtins,
        no_builtin,
        auto_init,
        share,
        in_place,
        fixed_x18,
        reorder,
        isa,
        read,
    } = cx;
    let module = Module::new(names.intern(name), target);
    let (reachable, named) = reach::reachable(reach::Decide::new(tast, types, target, names));
    let mut unit = Unit {
        tast,
        types,
        target,
        names,
        visibility,
        protector,
        wrapping,
        aliasing,
        padding,
        cliques: 0,
        tree: aliasing::Tree::default(),
        contract,
        align,
        instrument,
        exceptions,
        non_call_exceptions,
        lifetimes,
        common,
        builtins,
        no_builtin,
        auto_init,
        share,
        in_place,
        fixed_x18,
        reorder,
        isa,
        read,
        module,
        diagnostics: Vec::new(),
        strings: Map::default(),
        anonymous: 0,
        statics: Map::default(),
        labels: Map::default(),
        done: Set::default(),
        unreordered: Set::default(),
        bodies: Vec::new(),
        aliases: Vec::new(),
        sets: Vec::new(),
        aliased: Set::default(),
        starts: Vec::new(),
        renamed: Map::default(),
        reachable,
        named,
        nest: Nest::default(),
    };
    unit.run();
    Lowered { module: unit.module, diagnostics: unit.diagnostics }
}

/// The walk over one translation unit, and everything it has built so far.
pub(crate) struct Unit<'a> {
    pub(crate) tast: &'a Tast,
    pub(crate) types: &'a Types,
    pub(crate) target: &'a TargetInfo,
    pub(crate) names: &'a mut Interner,
    /// What a name no declaration said anything about gets. See [`Context::visibility`].
    visibility: IrVisibility,
    /// Which functions get a stack protector. See [`Context::protector`].
    pub(crate) protector: Protector,
    /// What wraps rather than being undefined. See [`Context::wrapping`].
    pub(crate) wrapping: Wrapping,
    /// Whether an access names the type it goes through. See [`Context::aliasing`].
    aliasing: bool,
    /// Whether an access says how far the padding after it reaches. See [`Context::padding`].
    pub(crate) padding: bool,
    /// How many `restrict` scopes have been handed out, which is a number the whole module shares
    /// so that no two functions promise different things with the same one. See
    /// [`restrict`](mod@crate::restrict) for why that matters before there is an inliner.
    pub(crate) cliques: u16,
    /// The type based aliasing tree built so far, which is one per module.
    tree: aliasing::Tree,
    /// How far a multiply and an addition may be fused. See [`Context::contract`].
    pub(crate) contract: FpContract,
    /// What every function is aligned to unless it asked for more. See [`Context::align`].
    align: Option<u32>,
    /// Whether the profiling hooks are called. See [`Context::instrument`].
    pub(crate) instrument: bool,
    /// Whether an exception may unwind through the unit. See [`Context::exceptions`].
    pub(crate) exceptions: bool,
    /// Whether a trapping instruction may unwind. See [`Context::non_call_exceptions`].
    pub(crate) non_call_exceptions: bool,
    /// Whether the end of a local's lifetime is written down. See [`Context::lifetimes`].
    pub(crate) lifetimes: bool,
    /// Whether a tentative definition is a common symbol. See [`Context::common`].
    common: bool,
    /// Whether a plain library name means the library. See [`Context::builtins`].
    builtins: bool,
    /// The names taken away one at a time. See [`Context::no_builtin`].
    no_builtin: &'a [String],
    /// What a local with no initializer starts out holding. See [`Context::auto_init`].
    pub(crate) auto_init: AutoInit,
    /// Whether locals in blocks that never overlap may share a slot. See [`Context::share`].
    pub(crate) share: bool,
    /// Whether a parameter may be used where the caller left it. See [`Context::in_place`].
    pub(crate) in_place: bool,
    /// Whether `x18` is kept for something else. See [`Context::fixed_x18`].
    fixed_x18: bool,
    /// Whether the objects are laid out newest first. See [`Context::reorder`].
    reorder: bool,
    /// The extensions the command line builds for. See [`Context::isa`].
    pub(crate) isa: rucc_target::Isa,
    /// How a file a `.incbin` names is read. See [`Context::read`].
    read: &'a mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    pub(crate) module: Module,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// The global each string literal was emitted as, so that two mentions of one literal are
    /// one object.
    strings: Map<StrId, Symbol>,
    /// How many runs of bytes written under no label in an `asm` at file scope have been given a
    /// name, which is what keeps the next one from being given the same one.
    anonymous: usize,
    /// The name each object with no linkage was given.
    statics: Map<DeclId, Symbol>,
    /// The name each label an image holds the address of was given.
    ///
    /// A label is a place inside a function and has no name in the object file, because a jump to
    /// one is a distance the assembler works out and never a symbol. An image is the one thing
    /// that cannot do that: it is in another section, so what it holds is a relocation, and a
    /// relocation names a symbol. So a label an image points at gets one, minted here because the
    /// image is lowered before the body is walked and the block the label starts does not exist
    /// yet when the name is first asked for.
    labels: Map<LabelId, Symbol>,
    /// What has been emitted, because a redeclaration is the same declaration seen twice.
    done: Set<DeclId>,
    /// The objects written `no_reorder`, which keep the order the source wrote them in ahead of
    /// the rest when [`Self::reorder`] turns the others around.
    unreordered: Set<Symbol>,
    /// The functions lowered with a body, by the statement the body is and the symbol it went
    /// under. The statements were made as the source was read, so sorting by them is the order
    /// the bodies were written in, which is not the order the walk reaches them: that is the order
    /// of the first declaration, and a prototype at the top of a file puts a function first.
    bodies: Vec<(rucc_sema::StmtId, Symbol)>,
    /// The declarations that are a second name for something rather than a thing of their own,
    /// in the order the file made them.
    ///
    /// Held back rather than emitted where they are met, because what an alias points at may be
    /// written below it and whether anything defines it is a question only the whole file
    /// answers.
    aliases: Vec<DeclId>,
    /// The names a `.set` in an `asm` at file scope gave to something else, with the block each
    /// one was written in, in the order the file wrote them.
    ///
    /// Held back for the reason above and written out beside the aliases, since the two are the
    /// same thing said two ways: a second symbol at an address this object already has.
    sets: Vec<(directives::Set, Span)>,
    /// The symbols something in the file is a second name for.
    ///
    /// A `static` function nothing calls is not emitted, and being what an alias points at is a
    /// reason to emit one that no reference in the file says: the string an alias names is not a
    /// use of anything as far as the walk over the tree is concerned.
    aliased: Set<Symbol>,
    /// The functions the file asked to have run without anything calling them, in the order it
    /// defined them.
    ///
    /// Held back rather than emitted where they are met, because the entries go in the order the
    /// priorities put them and a function written at the top of the file may have asked to run
    /// last. Only the whole file settles that order.
    starts: Vec<Start>,
    /// The assembler name the file gave to a name with linkage, kept by the name that was
    /// written rather than by the declaration that wrote it.
    ///
    /// For [`Unit::library_name`], which knows what the C library calls a function and not what
    /// this file has said about it. The declaration that renames `memcpy` is a different
    /// declaration from the implicit one the checker made for `__builtin_memcpy`, so the label
    /// on the first is never reached from the second, and a program that renames a function and
    /// then calls the builtin means the call to go to the new name.
    renamed: Map<Symbol, Symbol>,
    /// What something in the file reaches, which is what decides whether a function with
    /// internal linkage is emitted at all.
    reachable: Set<DeclId>,
    /// What an expression in something reached names, which is what a use of a name is. Only a
    /// weakref asks, since whether its target stays weak is whether anything uses it by name.
    named: Set<DeclId>,
    /// The nested functions met so far and what each of them reaches. See [`crate::nest`].
    pub(crate) nest: Nest,
}

// The debug is by hand and short: a translation unit is not something anybody wants printed as
// a `{:?}`, and the module has a printer of its own for when they do.
impl fmt::Debug for Unit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Unit")
            .field("module", &self.module.counts())
            .field("diagnostics", &self.diagnostics.len())
            .finish()
    }
}

impl Unit<'_> {
    /// The aliasing node an access through `ty` carries, and [`None`] when it carries none.
    ///
    /// [`None`] is also every answer under `-fno-strict-aliasing`, which is the whole of what that
    /// flag does here. See [`aliasing`](mod@crate::aliasing) for which types have a node.
    pub(crate) fn alias_node(&mut self, ty: TypeId) -> Option<Meta> {
        if !self.aliasing {
            return None;
        }
        self.tree.node(&mut self.module, self.names, self.types, ty)
    }

    /// The root of the aliasing tree, which is the node an access that may be punned carries.
    ///
    /// The root is `char` and it conflicts with everything, so an access carrying it is an access
    /// nothing may be reordered across and, in the type plane, a byte nothing has settled the type
    /// of. `crate::body` says which accesses those are.
    pub(crate) fn alias_root(&mut self) -> Option<Meta> {
        if !self.aliasing {
            return None;
        }
        Some(self.tree.root(&mut self.module, self.names))
    }

    /// Every declaration the file made, in the order it made them.
    fn run(&mut self) {
        self.file_asms();
        let written = self.module.globals().count();
        self.find_aliased();
        self.find_renamed();
        for index in 0..self.tast.top_level().len() {
            let decl = self.tast.top_level()[index];
            if !self.done.insert(decl) {
                continue;
            }
            match self.tast[decl].kind {
                DeclKind::Function => self.function(decl),
                DeclKind::Object => self.object(decl),
                // A name for a type is only in the tree at block scope and nothing is emitted
                // for one.
                DeclKind::Type => {}
            }
        }
        self.bodies.sort_by_key(|&(body, _)| body.index());
        for index in 0..self.bodies.len() {
            if let Some(SymbolRef::Func(id)) = self.module.lookup(self.bodies[index].1) {
                self.module.wrote(id);
            }
        }
        self.weak_references();
        self.unused_weak();
        for index in 0..self.aliases.len() {
            self.alias(self.aliases[index]);
        }
        self.symvers();
        for index in 0..self.sets.len() {
            let (set, span) = self.sets[index].clone();
            self.equated(&set, span);
        }
        // The entries of the constructors go after the turn, so that they keep the order that
        // `startups` sorts them to. gcc writes them as it writes each function and not as a
        // variable, so its turn of the variables does not reach them.
        if self.reorder {
            self.module.reverse_globals(written, &self.unreordered);
        }
        self.startups();
        self.mismatched_calls();
    }

    /// A call by a name the file defines with another signature, made a call through its address.
    ///
    /// A declaration at block scope can give a function the assembler name of one the file
    /// defines with other parameters. nolibc's `_start_c` declares `_nolibc_main` with three of
    /// them and the label `main`, and a program using it defines `int main(void)`. C leaves the
    /// call to the link and gcc makes it as written. A call by name here has to agree with the
    /// function it names, and a call through the address is the way to make it as written.
    fn mismatched_calls(&mut self) {
        let ids: Vec<FuncId> = self.module.funcs().collect();
        for id in ids {
            let func = &self.module[id];
            let mut found = Vec::new();
            for block in func.blocks() {
                for inst in func.insts(block) {
                    let data = &func[inst];
                    let (Opcode::Call, Extra::Call(at)) = (data.opcode, data.extra) else {
                        continue;
                    };
                    let Some(callee) = func[at].callee else { continue };
                    let Some(SymbolRef::Func(to)) = self.module.lookup(callee) else { continue };
                    if self.module[to].signature() != &func[func[at].signature]
                        && func.arg_groups(inst).is_none()
                    {
                        found.push((inst, callee));
                    }
                }
            }
            let func = &mut self.module[id];
            for (inst, callee) in found {
                let Extra::Call(at) = func[inst].extra else { continue };
                let span = func.span(inst);
                let data =
                    InstData { extra: Extra::Symbol(callee), ..InstData::new(Opcode::GlobalAddr) };
                let made = func.create_inst(data, &[Type::PTR], span);
                func.insert_before(made, inst);
                let addr = func[made].first_result.expect("an address has a result");
                let mut operands = vec![addr];
                operands.extend_from_slice(&func[func[inst].args]);
                let args = func.push_values(&operands);
                let info = func.add_call(CallInfo { callee: None, ..func[at] });
                let data = &mut func[inst];
                data.opcode = Opcode::CallIndirect;
                data.args = args;
                data.extra = Extra::Call(info);
            }
        }
    }

    /// The `asm` written at file scope, read into the globals they define.
    ///
    /// Ahead of the declarations rather than among them. A block usually names more than one
    /// thing and means them to be next to each other, the object writer lays globals out in the
    /// order the module holds them, and adding a block's globals together is what makes them a
    /// run. A declaration of one of those names below the block then finds a definition already
    /// there and leaves it alone, which is the division the program wrote: the template says what
    /// the bytes are and the C declaration says what they are to be read as.
    fn file_asms(&mut self) {
        for index in 0..self.tast.file_asms().len() {
            let asm = self.tast.file_asms()[index];
            let template = self.spelled(asm.template);
            let read = match directives::assemble(&template, &mut *self.read) {
                Ok(read) => read,
                // A directive this reader does not take, which on ELF is the assembler's to read
                // for the same reason an instruction is. The listing is one stream for the whole
                // object, so a `.macro` written here is there for a template in a function below
                // to use, and a `.pushsection` or a numbered label here is read in the same place
                // and the same order as gcc would have handed it to gas.
                Err(directives::Failed::Unsupported(_))
                    if self.target.object_format == ObjectFormat::Elf =>
                {
                    self.module.add_file_asm(template);
                    continue;
                }
                Err(directives::Failed::Unsupported(what)) => {
                    self.unsupported(&format!("{what} in an `asm` at file scope"), asm.span);
                    continue;
                }
                // An instruction, which the assembler reads rather than this, so the template is
                // kept as it was written and the unit is assembled from its listing, where the
                // template goes in the way gcc puts it into its own. Only on ELF, which is the
                // syntax the listing reader knows.
                Err(directives::Failed::Instruction(_))
                    if self.target.object_format == ObjectFormat::Elf =>
                {
                    self.module.add_file_asm(template);
                    continue;
                }
                Err(directives::Failed::Instruction(word)) => {
                    let what = format!("the instruction '{word}' in an `asm` at file scope");
                    self.unsupported(&what, asm.span);
                    continue;
                }
                Err(directives::Failed::Missing(name, why)) => {
                    let message = format!("cannot open '{name}' for reading: {why}");
                    self.diagnostics.push(Diagnostic::error(message, asm.span).with_code("E0702"));
                    continue;
                }
            };
            // The name of every global of the block first, because a distance one of them writes
            // is measured to a place in another of them and a relocation names a symbol, so the
            // name has to be to hand before the bytes that refer to it are built.
            let symbols: Vec<Symbol> = read
                .pieces
                .iter()
                .map(|piece| match &piece.name {
                    Some(name) => self.names.intern(name),
                    None => {
                        let name = format!(".Lasm.{}", self.anonymous);
                        self.anonymous += 1;
                        self.names.intern(&name)
                    }
                })
                .collect();
            for (index, piece) in read.pieces.into_iter().enumerate() {
                self.piece(piece, symbols[index], &symbols);
            }
            // Held back until the file has been walked, because a name a block equates may be
            // defined below the block, and remembered as a name something points at, because a
            // `static` function an equate is the only reference to is one that has to be emitted.
            for set in read.sets {
                let target = self.names.intern(&set.target);
                self.aliased.insert(target);
                self.sets.push((set, asm.span));
            }
        }
    }

    /// One global an `asm` at file scope defined, under the name minted for it and with the names
    /// of the whole block to hand.
    ///
    /// The bytes a template writes before it writes any label are a global like the rest and a
    /// global has to have a name, so one is minted for them. Nothing refers to it by that name, so
    /// the only thing it has to be is one nothing else takes, and the leading dot keeps it out of
    /// the symbol table the way the name of a string literal does.
    fn piece(&mut self, piece: directives::Piece, symbol: Symbol, symbols: &[Symbol]) {
        let mut global = Global::new(symbol, piece.size, piece.align.max(1));
        global.linkage = piece.linkage;
        global.visibility = piece.visibility;
        let bss = matches!(piece.section, directives::Section::Bss);
        match piece.section {
            // Which of the sections the object writer has an answer of its own for. Asking for
            // `.rodata` by name would produce a second section with that spelling and with the
            // flags of a writable one, so what is said here is what the global is instead.
            directives::Section::ReadOnly => global.constant = true,
            directives::Section::Data | directives::Section::Bss => {}
            directives::Section::Named(name) => global.section = Some(self.names.intern(&name)),
            // Refused where the template was read, since what goes in that section is
            // instructions and there is nothing here that makes one.
            directives::Section::Text => return,
        }
        let mut data = Vec::with_capacity(piece.items.len());
        if piece.items.is_empty() && bss {
            // A label at the end of the zero filled section, which has nothing under it and
            // still has to land there rather than in the section of written bytes. An image of
            // no zeros is what says so, since being all zeros is how a global asks for that
            // section and an empty image asks for nothing.
            data.push(Datum::Zero(0));
        }
        for item in piece.items {
            data.push(match item {
                directives::Item::Bytes(bytes) => Datum::Bytes(self.module.push_bytes(&bytes)),
                directives::Item::Int { width, value } => {
                    let ty = Type::int(u32::from(width) * 8);
                    Datum::Scalar {
                        ty,
                        value: self.module.add_imm(Imm::int(i128::from(value), ty)),
                    }
                }
                directives::Item::Zero(bytes) => Datum::Zero(bytes),
                // Four bytes holding how far that global is from these bytes, which the reader
                // said in globals of this block rather than in names because the place it
                // measures to is usually a label the object file holds no name for.
                directives::Item::Away { piece, addend } => {
                    let reloc = Reloc { symbol: symbols[piece], addend, size: 4 };
                    Datum::Away(self.module.add_reloc(reloc))
                }
            });
        }
        global.init = Some(self.module.push_data(&data));
        self.place_global(global);
    }

    /// Which symbols the file gives a second name to, before anything is emitted.
    ///
    /// Ahead of the walk rather than during it, because a `static` function is emitted or not on
    /// the strength of what reaches it and the alias that reaches one may be written below it.
    fn find_aliased(&mut self) {
        for index in 0..self.tast.top_level().len() {
            let decl = self.tast.top_level()[index];
            let Some(target) = self.tast[decl].alias else { continue };
            let spelling = self.spelled(target);
            let symbol = self.names.intern(&spelling);
            self.aliased.insert(symbol);
        }
    }

    /// Which names the file gave an assembler name of their own, before anything is emitted.
    ///
    /// Ahead of the walk for the reason [`Unit::find_aliased`] is: the call to
    /// `__builtin_memcpy` may be written above the declaration of `memcpy` that renames it, and
    /// the two spellings are one function.
    fn find_renamed(&mut self) {
        for index in 0..self.tast.top_level().len() {
            let decl = self.tast.top_level()[index];
            let node = &self.tast[decl];
            let (linkage, name, label) = (node.linkage, node.name, node.asm_label);
            if linkage == Linkage::None {
                continue;
            }
            let (Some(name), Some(label)) = (name, label) else { continue };
            let spelling = self.spelled(label);
            let symbol = self.names.intern(&spelling);
            self.renamed.insert(name, symbol);
        }
    }

    /// The bytes of a string literal as a name, which is what a symbol in an attribute is.
    fn spelled(&self, id: StrId) -> String {
        self.tast[id].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect()
    }

    /// A plain string literal as a name, from its bytes, so that a name in UTF-8 stays the same
    /// name. A wasm export or import name is any UTF-8 string.
    fn spelled_symbol(&mut self, id: StrId) -> Symbol {
        let bytes: Vec<u8> =
            self.tast[id].elements.iter().filter_map(|&unit| u8::try_from(unit).ok()).collect();
        self.names.intern(&String::from_utf8_lossy(&bytes))
    }

    /// The section a `section` attribute put this function or object in, as the object format
    /// spells a section name.
    ///
    /// ELF and COFF take the name as written. A Mach-O section is a segment and a section of up to
    /// sixteen bytes each with a comma between them, and a program written for ELF says `.mine`,
    /// which no Mach-O assembler takes. So a name with no comma in it is given the segment its
    /// contents belong in, `__TEXT` for code and `__DATA` for anything else, and the leading dots
    /// become the two underscores every Mach-O section name starts with: `.init.text` on a
    /// function is `__TEXT,__init_text`. A name that already has a comma is the program speaking
    /// Mach-O and is left alone.
    fn section_of(&mut self, decl: DeclId, code: bool) -> Option<Symbol> {
        let written = self.spelled(self.tast.section(decl)?);
        let name = match self.target.object_format {
            ObjectFormat::MachO if !written.contains(',') => {
                let segment = if code { "__TEXT" } else { "__DATA" };
                let mut section =
                    format!("__{}", written.trim_start_matches('.').replace('.', "_"));
                while section.len() > 16 {
                    section.pop();
                }
                format!("{segment},{section}")
            }
            _ => written,
        };
        Some(self.names.intern(&name))
    }

    /// One object with static storage duration.
    fn object(&mut self, decl: DeclId) {
        let tast = self.tast;
        let node = &tast[decl];
        let (ty, state, init) = (node.ty, node.state, node.init);
        let (linkage, duration, alignment) = (node.linkage, node.duration, node.alignment);
        let span = tast.decl_span(decl);
        if duration == StorageDuration::Automatic {
            // A block-scope object with automatic storage is a slot or a value in the function
            // that declares it, and the body is what makes it. Nothing is emitted here.
            return;
        }
        // A register kept for the whole program, `register void *teb asm ("x18")`, is the
        // register and not memory, so there is nothing to lay out. Every read of it is a read of
        // the register, made where it stands.
        if node.register.is_some() {
            return;
        }
        // A second name for something else is not an object of its own, so nothing is laid out
        // and no image is built. It is held back until the rest of the file has been walked,
        // because what it points at may be below it.
        if node.alias.is_some() {
            self.aliases.push(decl);
            return;
        }

        // `__declspec(dllimport) int x;` with no `extern` in front says the object is in another
        // DLL, which is a declaration and not the tentative definition the same words are
        // without the attribute. gcc and clang both read it that way, and making room for it here
        // as well would be a second object the program never reaches.
        let state = if state == Definition::Tentative
            && node.flags.contains(DeclFlags::DLLIMPORT)
            && duration == StorageDuration::Static
        {
            Definition::Declared
        } else {
            state
        };
        let symbol = self.symbol_of(decl);
        if node.flags.contains(DeclFlags::NO_REORDER) {
            self.unreordered.insert(symbol);
        }
        let size = repr::size_of(self.types, self.target, ty);
        let align = match alignment {
            Some(align) => align,
            None => {
                let natural = repr::align_of(self.types, self.target, ty);
                if state == Definition::Declared {
                    natural
                } else {
                    let thread = duration == StorageDuration::Thread;
                    repr::static_align(self.types, self.target, ty, natural, thread)
                }
            }
        };
        let mut global = Global::new(symbol, size, align);
        global.linkage = self.told(decl, linkage);
        // A tentative definition counts as one, because it is one: `int x;` at file scope puts a
        // symbol in this object and the linker never has to look anywhere else for it.
        global.visibility = self.seen(decl, state != Definition::Declared);
        // A thread-local is reached through the index and the block the loader sets up for this
        // module, so there is no pointer to another DLL's copy to go through, and gcc refuses
        // the pair outright. It is left as an ordinary external name here.
        if duration == StorageDuration::Static {
            global.dll = self.dll(decl, state != Definition::Declared);
        }
        global.tls = (duration == StorageDuration::Thread).then_some(TlsModel::GlobalDynamic);
        global.constant = repr::is_read_only(self.types, ty);
        global.section = self.section_of(decl, false);
        // A `static` object is the optimizer's to take away once nothing names it, which is what
        // gcc does, unless an attribute asked for it to be kept. The kernel's table of operations
        // for a feature it was built without is one: the call that took its address was to a stub
        // that ignores it, and the functions the table names call what the kernel never defined.
        global.droppable = global.linkage == IrLinkage::Internal
            && state != Definition::Declared
            && !node.flags.contains(DeclFlags::RETAINED);
        global.retain = node.flags.contains(DeclFlags::RETAIN);
        // gcc places a `noinit` object only when the definition has no initializer and a
        // `persistent` one only when it has one, asking of the definition rather than of the
        // declaration that said it, so `extern int x __attribute__((noinit)); int x = 5;` is in
        // `.data`. See `check::noinit` in `rucc-sema` for what is said about the rest.
        let defined = state != Definition::Declared;
        global.noinit = defined && init.is_none() && node.flags.contains(DeclFlags::NOINIT);
        global.persistent = defined && init.is_some() && node.flags.contains(DeclFlags::PERSISTENT);
        // Under `-fcommon` an `int x;` that nothing initializes is offered to the linker to merge
        // with every other one of the same name, and with a real definition if there is one. Only
        // the plain case is: a thread-local one has to be a copy per thread, a weak or internal
        // one is not the linker's to merge, and one the program put in a section of its own is in
        // that section, which is gcc's answer too. And one written `nocommon` is the program saying
        // this object is not to be merged whatever the command line says, one written `common`
        // says the opposite, and one written `retain` or `noinit` goes in a section of its own,
        // which gcc gives it rather than merging it.
        let common = if self.common {
            !node.flags.contains(DeclFlags::NO_COMMON)
        } else {
            node.flags.contains(DeclFlags::COMMON)
        };
        if common
            && state == Definition::Tentative
            && global.linkage == IrLinkage::External
            && global.tls.is_none()
            && global.section.is_none()
            && !global.retain
            && !global.noinit
        {
            global.linkage = IrLinkage::Common;
        }
        global.init = match state {
            // `extern int x;` and nothing else names an object another translation unit
            // defines. The global is here so that a reference to it has something to resolve
            // against, and it has no image, which is what makes it a declaration.
            Definition::Declared => None,
            Definition::Tentative => Some(self.zeros(size)),
            Definition::Defined => {
                let (data, covered) = self.image(init, size, span);
                // The object is as large as its image when the image is the larger of the two.
                // A structure whose last member is a flexible array is the only way that
                // happens: `sizeof` answers without the array and an initializer that fills it
                // makes an object big enough to hold what was written. C 6.7.2.1p18 leaves the
                // size to the implementation, gcc grows the object, and this does the same
                // rather than hand the linker a size the image does not fit in.
                global.size = size.max(covered);
                Some(data)
            }
        };
        self.place_global(global);
    }

    /// One function, with its body when it has one.
    fn function(&mut self, decl: DeclId) {
        let tast = self.tast;
        let node = &tast[decl];
        let (ty, linkage, body, align) = (node.ty, node.linkage, node.body, node.alignment);
        let noreturn = node.flags.contains(DeclFlags::NORETURN);
        let naked = node.flags.contains(DeclFlags::NAKED);
        let untraced = node.flags.contains(DeclFlags::NO_INSTRUMENT);
        let unprofiled = node.flags.contains(DeclFlags::NO_PROFILE);
        let twice = node.flags.contains(DeclFlags::RETURNS_TWICE);
        let effects = node.effects;
        let startup = node.startup;
        let span = tast.decl_span(decl);
        if node.name.is_none() {
            return;
        }
        // The same as for an object: a second name is not a function of its own, and it is held
        // back until what it points at has been emitted.
        if node.alias.is_some() {
            self.aliases.push(decl);
            return;
        }
        // Which asks the one question the reference to it asks, so that a declaration that
        // renamed the symbol renames the definition as well and the two still meet.
        let name = self.symbol_of(decl);
        if self.is_dropped(decl, name) {
            return;
        }
        // A declaration with no body is let go quietly when its parameters cannot be laid out,
        // which is a parameter of an enumeration nobody has finished yet. gcc says nothing about
        // one of those until something calls it, and the kernel's `irq.h` declares one ahead of
        // the header that finishes it. A call still reports it, from where the call is planned.
        let plan = if body.is_none() {
            self.try_plan(ty, &[], false).ok()
        } else {
            self.plan(ty, &[], span)
        };
        let Some(mut plan) = plan else { return };
        // A nested function takes its static chain after everything the program wrote, which is
        // what keeps every other parameter where the plan put it. See [`crate::nest`].
        let frame = self.nest.frame(decl).filter(|frame| frame.nested).cloned();
        if let Some(frame) = &frame {
            if !self.chains(span) {
                return;
            }
            plan.signature.params.push(Param::with_abi(Type::PTR, Abi::Chain));
            // One written without a prototype is given a variadic signature like any other,
            // which a nested function cannot have and does not need, since the checking has
            // turned down one that really is. Its calls are changed to match. See
            // [`crate::nest`].
            plan.signature.variadic = false;
            // The trampoline is the only thing that reaches the function when every direct call
            // to it has been inlined, and nothing in the module can see the trampoline, so the
            // function is kept whatever the optimizer makes of the calls.
            if frame.escapes {
                self.trampoline_target(name);
            }
        }

        let mut func = Func::new(name, plan.signature.clone());
        // The name the source spelled, where an assembler name says the symbol is not it. A
        // declaration of `strstr` renamed to `my_strstr` is a declaration of `strstr` still, and
        // once the symbol is the only name left there is nothing to find that out again from.
        if node.asm_label.is_some() {
            func.spelled = node.name.filter(|&spelled| spelled != name);
        }
        // Where the body begins, which is the line a debugger names over the prologue. gcc says the
        // line the opening brace is on rather than the line the declarator is on, and the two
        // differ in the style that puts the brace underneath. No instruction in a prologue has a
        // span of its own, so this is the only place the fact can come from. A declaration has no
        // body and produces no prologue, so it falls back to the declarator and nothing reads it.
        func.declared = body.map_or(span, |body| tast.stmt_span(body));
        // And where the name is written in the definition, which is what a report about the
        // function as a whole points at. Not `span`, which is the first declaration and is a
        // prototype at the top of the file as often as not.
        func.named = tast.definition_span(decl);
        // The larger of what this function asked for and what the command line asked of all of
        // them, since the attribute is a requirement and the flag is a preference, and a
        // preference does not get to move a function off a boundary its own source named.
        func.align = match (align, self.align) {
            (Some(mine), Some(everyones)) => Some(mine.max(everyones)),
            (mine, everyones) => mine.or(everyones),
        };
        // The one thing a declaration says that nobody downstream can work out for themselves.
        // What `abort` does belongs to `abort`, and a translation unit that only declares it has
        // nothing to look at, so the claim has to travel on the declaration or not at all.
        if noreturn {
            func.attrs.set |= AttrSet::NORETURN;
        }
        // Which is not a claim about what a call to it does but a fact about how the function
        // itself is written, so unlike the two around it there is nothing here for a declaration
        // alone to be useful for. It travels the same way because the attribute is written in the
        // same places. See [`rucc_codegen`] for what reads it, which is the frame.
        if naked {
            func.attrs.set |= AttrSet::NAKED;
        }
        // Facts about the body as well, written in the same places, and read by the code generator
        // for what is saved around the body and how it goes back. See `rucc_codegen::frame`.
        if node.flags.contains(DeclFlags::INTERRUPT) {
            func.attrs.set |= AttrSet::INTERRUPT;
        }
        if node.flags.contains(DeclFlags::SAVES_ALL) {
            func.attrs.set |= AttrSet::SAVES_ALL;
        }
        // Also a fact about the body, read by the frame when `-pg` asks for a hook in every
        // function but this one.
        if untraced {
            func.attrs.set |= AttrSet::NO_INSTRUMENT;
        }
        // Read by the coverage pass, which puts no counters in it.
        if unprofiled {
            func.attrs.set |= AttrSet::NO_PROFILE;
        }
        // A claim about what a call to it does, like `noreturn`, and it has to travel for the same
        // reason: `sigsetjmp` is only ever declared here, and the frame of whoever calls it is
        // what the claim changes. See `rucc_codegen::tail::comes_back` for what reads it.
        if twice {
            func.attrs.set |= AttrSet::RETURNS_TWICE;
        }
        // A fact about the type, which leaves the landing pad out of the prologue. See
        // `rucc_codegen::pipeline`.
        if self.untracked(ty) {
            func.attrs.set |= AttrSet::NOCF;
        }
        // And one about how a call to it comes back, which every call carries on its own, but
        // which a tail call out of this function reads off the function itself. See
        // `rucc_codegen::tail`.
        if self.returns_by_jump(ty) {
            func.attrs.set |= AttrSet::INDIRECT_RETURN;
        }
        // And the one that asks for the pad when `-mmanual-endbr` leaves it out of the rest.
        if node.flags.contains(DeclFlags::CF_CHECK) {
            func.attrs.set |= AttrSet::CF_CHECK;
        }
        // And the one that says the caller is not to be trusted with the stack's alignment.
        if node.flags.contains(DeclFlags::FORCE_ALIGN) {
            func.attrs.set |= AttrSet::FORCE_ALIGN;
        }
        // And the one that keeps it where the source wrote it. See `rucc_opt::expand`.
        if node.flags.contains(DeclFlags::NO_REORDER) {
            func.attrs.set |= AttrSet::NO_REORDER;
        }
        // And the one that has it open with what a hot patcher writes over. See
        // `rucc_asm::hook`.
        if node.flags.contains(DeclFlags::MS_HOOK) {
            func.attrs.set |= AttrSet::MS_HOOK;
        }
        // And the other one, for the same reason. What a call to `strtol` reads belongs to
        // `strtol`, and the purity analysis answers opaque for everything it cannot see a body
        // for, so a unit that only declares the function gets nothing out of it unless the
        // promise arrives here. `const` says the result comes from the arguments alone, which
        // is `readnone`, and `pure` says it may read memory, which is `readonly`. The two are
        // an incompatible pair in the IR and only one of them is ever set. Neither is kept for a
        // function that returns through memory, because in the IR the value it returns is a store
        // through the pointer it was handed, and a call that promised to write nothing would have
        // that store forgotten and the call deleted. A `_Complex double` is one of those on
        // Windows x64, which is `execute/20050121-1.c`, and a large enough struct is one anywhere.
        func.attrs.set |= match effects {
            _ if plan.returns_through_memory() => AttrSet::NONE,
            Effects::Any => AttrSet::NONE,
            Effects::Pure => AttrSet::READONLY,
            Effects::Const => AttrSet::READNONE,
        };
        // Whether to inline it, which `rucc_opt::inline` reads for `always_inline` at every level
        // and for a plain `inline` from `-O1` up. The IR will not have both of the first two, so a
        // name that said both gets the one gcc keeps, which is `always_inline` after a warning that
        // the other was ignored, and either of them says more than the keyword does.
        //
        // `optimize ("O0")` is the IR's `optnone`, which also keeps it out of the inliner both
        // ways. The IR will not have that with `always_inline` either, and a name that said both
        // is inlined, since the promise a caller was made is the one that has to be kept.
        if node.flags.contains(DeclFlags::OPTIMIZE_NONE)
            && !node.flags.contains(DeclFlags::ALWAYS_INLINE)
        {
            func.attrs.set |= AttrSet::OPTNONE;
        }
        if node.flags.contains(DeclFlags::NO_LOOP_IDIOM) {
            func.attrs.set |= AttrSet::NO_LOOP_IDIOM;
        }
        // `noipa` wins over `always_inline` as gcc has it, which drops `always_inline` with a
        // warning wherever the two meet, so a call to the function stays a call.
        if node.flags.contains(DeclFlags::NOIPA) {
            func.attrs.set |= AttrSet::NOIPA | AttrSet::NOINLINE;
        } else if node.flags.contains(DeclFlags::ALWAYS_INLINE) {
            func.attrs.set |= AttrSet::ALWAYS_INLINE;
        } else if node.flags.contains(DeclFlags::NOINLINE) {
            func.attrs.set |= AttrSet::NOINLINE;
        } else if node.flags.contains(DeclFlags::DECLARED_INLINE) {
            func.attrs.set |= AttrSet::INLINE_HINT;
        }
        // How often it is called, which `rucc_opt::predict` reads off the callee to make the path
        // to a call of a cold function the unlikely one. The IR will not have both, and a name
        // that said both gets `cold`.
        if node.flags.contains(DeclFlags::COLD) {
            func.attrs.set |= AttrSet::COLD;
        } else if node.flags.contains(DeclFlags::HOT) {
            func.attrs.set |= AttrSet::HOT;
        }
        // Kept on the function as well as acted on, since the body never asks for a canary when it
        // says this, so that a listing shows why a function the flag covers has none.
        if node.flags.contains(DeclFlags::NO_STACK_PROTECTOR) {
            func.attrs.set |= AttrSet::NO_STACK_PROTECTOR;
        }
        // The speculation mitigations a function opted out of, which the code generator reads.
        if node.flags.contains(DeclFlags::RETURN_KEEP) {
            func.attrs.set |= AttrSet::RETURN_KEEP;
        }
        if node.flags.contains(DeclFlags::INDIRECT_KEEP) {
            func.attrs.set |= AttrSet::INDIRECT_KEEP;
        }
        // `used`, and the other attributes that keep a definition nothing in the file calls, so
        // that nothing after this takes the body away. The kernel's `asm-offsets.c` writes every
        // offset from `static void __used common(void)`, which no one calls.
        if node.flags.contains(DeclFlags::RETAINED) || frame.as_ref().is_some_and(|f| f.escapes) {
            func.attrs.set |= AttrSet::USED;
        }
        // And `retain`, which asks the linker to keep it too. See [`AttrSet::RETAIN`].
        if node.flags.contains(DeclFlags::RETAIN) {
            func.attrs.set |= AttrSet::RETAIN;
        }
        // What a function said about zeroing registers on the way out, which overrides the
        // command line for its body. Skip and the vector registers share a bit on the declaration,
        // which the two questions tell apart, and have one each here.
        for (said, kept) in [
            (node.flags.zero_skip(), AttrSet::ZERO_SKIP),
            (node.flags.contains(DeclFlags::ZERO_USED), AttrSet::ZERO_USED),
            (node.flags.contains(DeclFlags::ZERO_ALL), AttrSet::ZERO_ALL),
            (node.flags.contains(DeclFlags::ZERO_ARG), AttrSet::ZERO_ARG),
            (node.flags.zero_wide(), AttrSet::ZERO_WIDE),
        ] {
            if said {
                func.attrs.set |= kept;
            }
        }
        // What a `target` attribute said the function is built for, which the inliner compares
        // against each caller: a body built for SSE4.2 is not copied into one that is not.
        func.target = tast.target(decl);
        // The room a patcher gets in this function, when an attribute said rather than the
        // command line. See `rucc_codegen::pipeline`.
        func.patchable = tast.patchable(decl);
        // And the hook and the list for the profiler's call, the same way.
        if let Some(name) = tast.fentry_name(decl) {
            let spelled = self.spelled(name);
            func.fentry_name = Some(self.names.intern(&spelled));
        }
        if let Some(section) = tast.fentry_section(decl) {
            let spelled = self.spelled(section);
            func.fentry_section = Some(self.names.intern(&spelled));
        }
        func.section = self.section_of(decl, true);
        // The names a wasm object gives it in place of its own, from clang's attributes, which
        // sema records on a wasm row alone.
        let wasm = tast.wasm_names(decl);
        func.wasm = rucc_ir::WasmNames {
            export: wasm.export.map(|name| self.spelled_symbol(name)),
            module: wasm.module.map(|name| self.spelled_symbol(name)),
            field: wasm.field.map(|name| self.spelled_symbol(name)),
        };
        // A claim about what a call to it returns, which travels on the declaration for the reason
        // `noreturn` does: `malloc` is only ever declared here. `rucc_opt::objsize` reads it off
        // the callee of the call an address came out of.
        func.attrs.alloc_size = tast
            .alloc_size(decl)
            .map(|alloc| rucc_ir::AllocSize { size: alloc.size, count: alloc.count });
        // What a call that survives the optimizer is reported with, which the driver reads once
        // the optimizer is done. A declaration is the usual carrier, since the function is one
        // nothing should ever call.
        let notices = tast.notices(decl);
        func.notices = rucc_ir::Notices {
            error: notices.error.map(|id| self.spelled(id)),
            warning: notices.warning.map(|id| self.spelled(id)),
        };
        // An inline definition this unit calls, which this unit puts a copy of out of line for
        // every call the inliner leaves alone. See [`Self::out_of_line`].
        let copied = body.is_some() && self.out_of_line(decl, node.inline);
        // Under GNU's reading the copy is for the inliner and nothing else. See
        // [`Self::out_of_line`].
        if copied && node.flags.contains(DeclFlags::GNU_INLINE) {
            func.attrs.set |= AttrSet::INLINE_ONLY;
        }
        // The backend writes a small constant `memcpy` it finds after inlining as moves, which is
        // only right while the name means the library's function.
        if !["memcpy", "memset", "memmove"].iter().all(|name| self.means_the_library(name)) {
            func.attrs.set |= AttrSet::NO_BUILTIN;
        }
        func.linkage = if copied { IrLinkage::LinkOnce } else { self.told(decl, linkage) };
        // The same question as for an object, and the same answer, with one wrinkle: an inline
        // definition this unit neither emits nor calls is a declaration here, since C 6.7.4p7
        // sends the calls to whatever unit holds the external definition, so it is not this
        // file's to describe. That is the condition the body is lowered under, a few lines below.
        func.visibility = self.seen(decl, body.is_some() && (node.inline.emits() || copied));
        // An inline copy made for this unit's own calls is not the definition another DLL would
        // be sent to, so it is not exported, and it is not imported either, since it is here.
        func.dll = match self.dll(decl, body.is_some() && node.inline.emits()) {
            Dll::Import if copied => Dll::Default,
            dll => dll,
        };
        // An inline definition is not an external definition, so what goes in the module is the
        // declaration and not the body. C 6.7.4p7 says the calls in this unit go to the definition
        // some other unit holds, which is what the declaration gives them, and glibc's headers
        // rely on it: every one of their inline definitions would otherwise be a second definition
        // of a name the library already defines. Unless this unit is one of the callers, which is
        // the case [`Self::out_of_line`] is about.
        // A function `target_clones` asked to have built more than once, which is a body for each
        // version and the name an indirect function choosing between them. An inline copy made for
        // this unit's own calls is built once, as the inliner wants it.
        if let Some(versions) = tast.versions(decl).filter(|_| body.is_some() && !copied) {
            if node.inline.emits() {
                self.clones(decl, &func, versions, &plan);
                for (before, priority) in [(true, startup.before), (false, startup.after)] {
                    if let Some(priority) = priority {
                        self.starts.push(Start { func: name, before, priority, span });
                    }
                }
                return;
            }
        }
        if body.is_some() && (node.inline.emits() || copied) {
            // `optimize ("no-strict-aliasing")` on the function, which has to be kept in the IR
            // rather than on the command line because the inliner may copy this body into a
            // function that is under the rule.
            //
            // `optimize ("wrapv")` is kept the same way and for the same reason, as instructions
            // that do not say they cannot wrap. It is the other answer to the question `-ftrapv`
            // answers, so it turns that off for the body as the command line's `-fwrapv` does.
            let strict = self.aliasing;
            let wrapping = self.wrapping;
            self.aliasing &= !node.flags.contains(DeclFlags::NO_STRICT_ALIASING);
            if node.flags.contains(DeclFlags::WRAPV) {
                self.wrapping.signed = true;
                self.wrapping.trap = false;
            }
            body::lower(self, decl, &mut func, &plan);
            self.aliasing = strict;
            self.wrapping = wrapping;
            // Only for a definition, because an entry is an address and a declaration of something
            // another file defines has none to put there. gcc reads the attribute off whichever
            // declaration carried it and then waits for the definition in the same way, which is
            // why writing `__attribute__((constructor)) void f(void);` in a header costs every
            // file that includes it nothing.
            if let Some(priority) = startup.before {
                self.starts.push(Start { func: name, before: true, priority, span });
            }
            if let Some(priority) = startup.after {
                self.starts.push(Start { func: name, before: false, priority, span });
            }
        }
        if let Some(body) = body {
            self.bodies.push((body, name));
        }
        self.place_func(func);
    }

    /// Whether a nested function can be built for this target, saying why not when it cannot.
    ///
    /// What it needs is a register for the static chain that nothing else is using, which is
    /// [`rucc_target::CallRegs::chain`], and libgcc's heap trampolines, which are on every system
    /// whose objects are ELF. Apple and Windows keep the register AArch64 would use, and on x86-64
    /// Windows passes its arguments somewhere else. `-ffixed-x18` keeps the register for the
    /// kernel's shadow call stack.
    fn chains(&mut self, span: Span) -> bool {
        let chain = self.target.call_regs.and_then(|regs| regs.chain);
        let why = match chain {
            None => {
                Some("the calling convention of this target has no register for the static chain")
            }
            Some(_) if self.target.object_format != ObjectFormat::Elf => {
                Some("the heap trampolines it is called through are libgcc's, which is ELF only")
            }
            Some(_) if self.fixed_x18 && self.target.tuple.arch() == rucc_tuple::Arch::Aarch64 => {
                Some("its static chain travels in x18, which -ffixed-x18 keeps for something else")
            }
            Some(_) => None,
        };
        let Some(why) = why else { return true };
        // Once per file, since every nested function in it would say the same thing.
        if !self.nest.refused {
            self.nest.refused = true;
            self.diagnostics.push(
                Diagnostic::error("a nested function cannot be built for this target", span)
                    .with_code("E0519")
                    .note(why, span),
            );
        }
        false
    }

    /// The stub a nested function's trampoline is pointed at, on a machine that needs one, put in
    /// the module as assembly at file scope with a declaration of its name beside it so that the
    /// address taken of it is the address of something in this file.
    fn trampoline_target(&mut self, function: Symbol) {
        if self.target.tuple.arch() != rucc_tuple::Arch::X86_64 {
            return;
        }
        let spelled = self.names.resolve(function).to_string();
        self.module.add_file_asm(nest::stub(&spelled));
        let stub = self.names.intern(&nest::stub_name(&spelled));
        let mut declared = Func::new(stub, Signature::new());
        declared.linkage = IrLinkage::Internal;
        self.place_func(declared);
    }

    /// The versions of a function `target_clones` asked for, the resolver that picks one when the
    /// program is loaded and the indirect function under the function's own name that calls it,
    /// which is what gcc 16 builds on an x86-64 ELF target.
    ///
    /// Each version is a local function named the function's name, a dot and the version's
    /// suffix, with everything the function said about itself and the extensions of its version,
    /// less `always_inline`, which the checker warned was dropped. The body is lowered once for
    /// each, and a `static` in it is one object all of them share, since the symbol a local static
    /// is given is kept per declaration.
    ///
    /// The resolver asks libgcc to fill in `__cpu_model` and `__cpu_features2`, then tests the
    /// versions' bits from the last to the first, each one that is set taking the place of the
    /// answer so far, so the first version the processor can run is the answer and `default` is
    /// the answer when it can run none. gcc tests them first to last and returns at the first; the
    /// address that comes back is the same. Like gcc's it is weak for a function another unit can
    /// call, so that two units cloning one inline definition keep one, and local for a `static`.
    fn clones(&mut self, decl: DeclId, func: &Func, versions: &[Version], plan: &Plan) {
        let called = self.names.resolve(func.name).to_owned();
        let mut built = Vec::with_capacity(versions.len());
        for version in versions {
            let name = self.names.intern(&format!("{called}.{}", version.suffix));
            let mut clone = Func::new(name, plan.signature.clone());
            clone.declared = func.declared;
            clone.named = func.named;
            clone.align = func.align;
            clone.patchable = func.patchable;
            clone.fentry_name = func.fentry_name;
            clone.fentry_section = func.fentry_section;
            clone.attrs = func.attrs;
            clone.attrs.set = clone.attrs.set.without(AttrSet::ALWAYS_INLINE);
            clone.section = func.section;
            clone.notices = func.notices.clone();
            clone.linkage = IrLinkage::Internal;
            clone.target = version.isa;
            let strict = self.aliasing;
            self.aliasing &= !self.tast[decl].flags.contains(DeclFlags::NO_STRICT_ALIASING);
            body::lower(self, decl, &mut clone, plan);
            self.aliasing = strict;
            if let Some(body) = self.tast[decl].body {
                self.bodies.push((body, name));
            }
            self.place_func(clone);
            let test = version.test.map(|(object, word, bit)| {
                (self.libgcc_object(object.symbol(), object.size()), word, bit)
            });
            built.push((name, test));
        }
        let linkage = self.told(decl, self.tast[decl].linkage);
        let resolver = self.names.intern(&format!("{called}.resolver"));
        let mut chooser = Func::new(resolver, Signature::new().with_returns(&[Type::PTR]));
        chooser.linkage = match linkage {
            IrLinkage::Internal => IrLinkage::Internal,
            _ => IrLinkage::LinkOnce,
        };
        chooser.declared = func.declared;
        chooser.named = func.named;
        let entry = chooser.create_block();
        let sig = chooser.add_signature(Signature::new());
        let init = self.names.intern("__cpu_indicator_init");
        let address = |build: &mut Builder<'_>, symbol: Symbol| {
            let data =
                InstData { extra: Extra::Symbol(symbol), ..InstData::new(Opcode::GlobalAddr) };
            build.value(data, Type::PTR)
        };
        let mut build = Builder::new(&mut chooser, entry);
        build.call(init, sig, &[]);
        let mut chosen = None;
        for &(name, test) in built.iter().rev() {
            let here = address(&mut build, name);
            let Some((object, word, bit)) = test else {
                chosen = Some(here);
                continue;
            };
            let base = address(&mut build, object);
            let amount = build.iconst(Type::int(64), i128::from(word) * 4);
            let args = build.func().push_values(&[base, amount]);
            let at = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
            let int = Type::int(32);
            let loaded = build.load(int, at, body::untyped(4), rucc_ir::Flags::NONE);
            let mask = build.iconst(int, i128::from(1u32 << bit));
            let masked = build.binary(Opcode::And, loaded, mask, rucc_ir::Flags::NONE);
            let zero = build.iconst(int, 0);
            let set = build.icmp(rucc_ir::IntPred::Ne, masked, zero);
            chosen = Some(match chosen {
                Some(so_far) => build.select(set, here, so_far),
                None => here,
            });
        }
        let chosen = chosen.expect("a cloned function has a default version");
        build.ret(&[chosen]);
        if let Some(body) = self.tast[decl].body {
            self.bodies.push((body, chooser.name));
        }
        self.place_func(chooser);
        let name = func.name;
        let alias = Alias {
            name,
            target: resolver,
            kind: AliasKind::IFunc,
            linkage,
            visibility: func.visibility,
        };
        match self.module.lookup(name) {
            None => {
                self.module.add_alias(alias);
            }
            Some(SymbolRef::Func(id)) if self.module[id].is_declaration() => {
                self.module.add_alias_over(alias);
            }
            Some(_) => {}
        }
    }

    /// Puts a function in the module under a name something may already be under.
    ///
    /// Two declarations of one identifier were merged before this, so the only way one name
    /// arrives twice is an assembler name that renames one identifier onto another: a
    /// declaration of `f` renamed to `g` beside a definition of `g` is one symbol written two
    /// ways, which is what the program asked for and what the linker is going to see. The
    /// definition wins wherever there is one, since what the declaration is here for is to give
    /// the calls something to resolve against and the definition does that as well.
    ///
    /// A name already carrying a definition keeps it. That is the program defining one symbol
    /// twice, and the assembler says so with the name in front of it, which is a better message
    /// than anything available here.
    fn place_func(&mut self, func: Func) {
        match self.module.lookup(func.name) {
            None => {
                self.module.add_func(func);
            }
            Some(SymbolRef::Func(id))
                if self.module[id].is_declaration() && !func.is_declaration() =>
            {
                self.module[id] = func;
            }
            // A second declaration of the symbol, which may be the one that says what the source
            // calls it. mingw-w64's `<stdio.h>` declares `__mingw_fprintf` under its own name and
            // then `fprintf` with that name as its assembler name, and a module that kept only the
            // first would have no `fprintf` in it for the library call folds to find.
            Some(SymbolRef::Func(id)) => {
                if self.module[id].spelled.is_none() {
                    self.module[id].spelled = func.spelled;
                }
                // A later declaration's `error` or `warning` replaces an earlier one's, which is
                // what gcc reports a call with. Two declarations inside two blocks are two
                // declarations here and one function in the module.
                let notices = &mut self.module[id].notices;
                if func.notices.error.is_some() {
                    notices.error = func.notices.error;
                }
                if func.notices.warning.is_some() {
                    notices.warning = func.notices.warning;
                }
            }
            Some(_) => {}
        }
    }

    /// One declaration that is a second name for something the same file defines.
    ///
    /// Emitted after everything else, so the target is looked up in a module that already holds
    /// whatever the file defines whether it was written above the alias or below it.
    ///
    /// The target has to be defined here and not merely declared, which is gcc's rule and is
    /// what the object format can express: an alias is a symbol at another symbol's address, and
    /// a name this file does not define has no address for one to be at. A program that writes
    /// an alias of something in another object wants a reference rather than a definition, and
    /// what it gets from gcc is this same error rather than a name the linker cannot resolve.
    fn alias(&mut self, decl: DeclId) {
        let Some(written) = self.tast[decl].alias else { return };
        let span = self.tast.decl_span(decl);
        let name = self.symbol_of(decl);
        let spelling = self.spelled(written);
        let target = self.names.intern(&spelling);
        if self.no_address(name, target, span) {
            return;
        }
        // Something already under this name, which is the program defining one symbol twice. The
        // definition that is there stands, the way it does for a function and for an object.
        if self.module.lookup(name).is_some() {
            return;
        }
        let mut alias = Alias::new(name, target);
        // `ifunc` names the resolver rather than the code, and only a function is one. An object
        // under the resolver's name is gcc's error about a function aliased to a variable, which
        // the checker has said where it could see the object and is said here where only the
        // symbols could, a name given with `__asm__` being the one way to get there.
        if self.tast.is_ifunc(decl) {
            if !matches!(self.module.lookup(target), Some(SymbolRef::Func(_))) {
                let spelled = self.names.resolve(name).to_owned();
                let what =
                    format!("'{spelled}' alias between function and variable is not supported");
                self.diagnostics.push(Diagnostic::error(what, span).with_code("E0821"));
                return;
            }
            alias.kind = AliasKind::IFunc;
        }
        alias.linkage = self.told(decl, self.tast[decl].linkage);
        // Its own answer, because the attribute is written on the alias and an alias is a symbol
        // of its own. `weak, alias, visibility("hidden")` is a name a library keeps to itself
        // while the thing it points at stays exported, which is how glibc writes half of them.
        // Always a definition. An alias is a symbol this object puts at an address in this object,
        // and one whose target is merely declared was refused a few lines above.
        alias.visibility = self.seen(decl, true);
        self.module.add_alias(alias);
    }

    /// One name a `.set` in an `asm` at file scope gave to something else.
    ///
    /// The same thing as the alias above it and written out the same way, with the two answers
    /// about the name coming from the directives around the `.set` rather than from an attribute:
    /// `.globl` and `.weak` say how the linker sees it, `.hidden` and `.protected` say how far it
    /// reaches, and a name no directive spoke about is local, which is what an assembler does with
    /// one. A name the file also defines keeps its own definition, which is the rule everything
    /// else here follows and is what gcc's output shows for a `.set` written above a definition of
    /// the same name.
    ///
    /// A name made by `.symver` is bound the way the name it stands for is, which is what gas
    /// does, since no directive can speak about a name spelled with an `@` in it.
    fn equated(&mut self, set: &directives::Set, span: Span) {
        let name = self.names.intern(&set.name);
        let mut target = self.names.intern(&set.target);
        // A target that is itself an alias stands for what that one stands for. xz gives a version
        // to a name it declared with the alias attribute, and every alias the attributes made was
        // added before the first of these, so the chain is already there to follow. The count
        // bounds a cycle, which the attributes refused where they were written.
        for _ in 0..self.module.aliases().count() {
            match self.module.lookup(target) {
                Some(SymbolRef::Alias(id)) if self.module[id].kind == AliasKind::Alias => {
                    target = self.module[id].target;
                }
                _ => break,
            }
        }
        if self.no_address(name, target, span) {
            return;
        }
        // A name the file only declared is one the `.set` gives an address to, and tcc's test
        // calls a function declared `extern` in C and defined by `.set` in a file-scope `asm`.
        let declared = match self.module.lookup(name) {
            None => true,
            Some(SymbolRef::Func(id)) => self.module[id].is_declaration(),
            Some(SymbolRef::Global(id)) => self.module[id].is_declaration(),
            Some(SymbolRef::Alias(_)) => false,
        };
        if !declared {
            return;
        }
        let mut alias = Alias::new(name, target);
        alias.linkage = set.linkage;
        alias.visibility = set.visibility;
        if set.versioned {
            (alias.linkage, alias.visibility) = match self.module.lookup(target) {
                Some(SymbolRef::Func(id)) => (self.module[id].linkage, self.module[id].visibility),
                Some(SymbolRef::Global(id)) => {
                    (self.module[id].linkage, self.module[id].visibility)
                }
                _ => (set.linkage, set.visibility),
            };
        }
        self.module.add_alias_over(alias);
    }

    /// The versioned names `__attribute__((symver("name@node")))` asked for, each a second name
    /// spelled with its version, which is what a `.symver` in an `asm` makes and is made the same
    /// way. See [`Self::equated`].
    ///
    /// gcc's rules first, in gcc's words and in its order. One version asked for twice is refused
    /// on each declaration but the last, which is the one that keeps it. What is versioned has to
    /// be defined here, which a declaration nothing refers to need not be, since gcc never looks
    /// at one, and an inline definition this unit does not emit is let go the same way. It must
    /// not be common, a copy the linker picks one of, or a weakref, and it must be public, with
    /// default visibility.
    fn symvers(&mut self) {
        let tast = self.tast;
        let asked = tast.symvers();
        let mut last: Map<&str, Span> = Map::default();
        for (_, names) in &asked {
            for (name, span) in names {
                last.insert(name.as_str(), *span);
            }
        }
        let mut kept: Set<&str> = Set::default();
        for (decl, names) in asked.iter().rev() {
            let decl = *decl;
            let mut refused = None;
            for (name, span) in names.iter().rev() {
                if !kept.insert(name.as_str()) {
                    refused = Some((*span, last[name.as_str()]));
                }
            }
            if let Some((span, there)) = refused {
                let what = "duplicate definition of a symbol version";
                let note = "same version was previously defined here";
                let refused = Diagnostic::error(what, span).with_code("E0830");
                self.diagnostics.push(refused.note(note, there));
                continue;
            }
            let node = &tast[decl];
            if node.kind == DeclKind::Function && !node.inline.emits() {
                continue;
            }
            let span = names[0].1;
            let symbol = self.symbol_of(decl);
            let (linkage, visibility, defined) = match self.module.lookup(symbol) {
                Some(SymbolRef::Func(id)) => {
                    let func = &self.module[id];
                    (func.linkage, func.visibility, !func.is_declaration())
                }
                Some(SymbolRef::Global(id)) => {
                    let global = &self.module[id];
                    let common = global.linkage == IrLinkage::Common;
                    (global.linkage, global.visibility, common || global.init.is_some())
                }
                Some(SymbolRef::Alias(id)) => {
                    (self.module[id].linkage, self.module[id].visibility, true)
                }
                None => (IrLinkage::External, IrVisibility::Default, false),
            };
            let weakref = node.flags.contains(DeclFlags::WEAKREF);
            let what = if !defined || weakref {
                if !self.named.contains(&decl) {
                    continue;
                }
                "symbol needs to be defined to have a version"
            } else if linkage == IrLinkage::Common {
                "common symbol cannot be versioned"
            } else if linkage == IrLinkage::LinkOnce {
                "comdat symbol cannot be versioned"
            } else if linkage == IrLinkage::Internal || node.linkage != Linkage::External {
                "versioned symbol must be public"
            } else if visibility != IrVisibility::Default {
                "versioned symbol must have default visibility"
            } else {
                let target = self.names.resolve(symbol).to_owned();
                for (name, _) in names {
                    let set = directives::Set {
                        name: name.clone(),
                        target: target.clone(),
                        linkage,
                        visibility,
                        versioned: true,
                    };
                    self.equated(&set, span);
                }
                continue;
            };
            self.diagnostics.push(Diagnostic::error(what, span).with_code("E0830"));
        }
    }

    /// Whether there is no address for a second name to be at, reporting why when there is not.
    ///
    /// The target has to be defined here and not merely declared, because an alias is a symbol at
    /// another symbol's address and a name this file does not define has no address in it. A
    /// program that writes one of these about something in another object wants a reference rather
    /// than a definition, and gcc turns that down as well.
    fn no_address(&mut self, name: Symbol, target: Symbol, span: Span) -> bool {
        let spelled = self.names.resolve(name).to_owned();
        if name == target {
            let what = format!("'{spelled}' is aliased to itself");
            self.diagnostics.push(Diagnostic::error(what, span).with_code("E0697"));
            return true;
        }
        let defined = match self.module.lookup(target) {
            Some(SymbolRef::Func(id)) => !self.module[id].is_declaration(),
            Some(SymbolRef::Global(id)) => self.module[id].init.is_some(),
            // A chain of them is a thing gcc takes and this does not yet, because resolving one
            // wants the aliases put in an order that the file they were written in need not be
            // in. It is reported rather than written out as a name pointing at a name.
            Some(SymbolRef::Alias(_)) | None => false,
        };
        if !defined {
            let spelling = self.names.resolve(target).to_owned();
            let what = format!("'{spelled}' is aliased to undefined symbol '{spelling}'");
            let note = "the target of an alias has to be defined in this same file, since an \
                        alias is a second name for an address and not a reference to one";
            let refused = Diagnostic::error(what, span).with_code("E0697");
            self.diagnostics.push(refused.note(note, span));
            return true;
        }
        false
    }

    /// The list of functions to run around `main`, written out as the entries that run them.
    ///
    /// In priority order rather than in the order the file defined them, because two of the four
    /// formats get their order from the order the entries are in. ELF and wasm sort them at link
    /// time, and the order that this gives is the order that they sort to.
    fn startups(&mut self) {
        let mut starts = std::mem::take(&mut self.starts);
        if matches!(self.target.object_format, ObjectFormat::Coff | ObjectFormat::Wasm) {
            for start in &mut starts {
                if !start.before {
                    *start = self.registrar(start);
                }
            }
        }
        starts.sort_by_key(Start::order);
        for start in starts {
            self.start_entry(&start);
        }
    }

    /// A constructor that hands a destructor to `atexit`, and the entry that runs it.
    ///
    /// COFF has a run-up list and no run-down one, so a destructor is registered from the run-up
    /// instead, which is what mingw's own CRT does with the `.dtors` it collects. The registration
    /// runs at the destructor's own priority, and `atexit` calls what it was given last first, so
    /// the destructors come out in the reverse of the order their constructors went in, which is
    /// the order gcc's come out in.
    ///
    /// wasm has a run-up list only, too. clang registers the destructors of one priority with
    /// `__cxa_atexit` from a constructor that `WebAssemblyLowerGlobalDtors` writes, and `atexit` of
    /// wasi-libc does the same thing for one function.
    fn registrar(&mut self, start: &Start) -> Start {
        let called = self.names.resolve(start.func).to_owned();
        let name = self.names.intern(&format!("__rucc_atexit.{called}"));
        let atexit = self.names.intern("atexit");
        let mut func = Func::new(name, Signature::new());
        func.linkage = IrLinkage::Internal;
        let entry = func.create_block();
        let sig = func.add_signature(
            Signature::new().with_params(&[Type::PTR]).with_returns(&[Type::int(32)]),
        );
        let mut build = Builder::new(&mut func, entry);
        let what = build.value(
            InstData { extra: Extra::Symbol(start.func), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        );
        build.call(atexit, sig, &[what]);
        build.ret(&[]);
        self.place_func(func);
        Start { func: name, before: true, ..*start }
    }

    /// One entry, which is a pointer wide object in the section the format runs.
    ///
    /// A relocation against the function rather than a value, since the address is not known until
    /// the link. The object has internal linkage and a name nothing refers to: the only thing that
    /// reads it is the CRT walking the section, which finds it by where it is and not by what it is
    /// called. gcc emits no symbol at all for one, and a name with a dot in it is the nearest thing
    /// to that here, being one no C program can write and therefore one no program collides with.
    fn start_entry(&mut self, start: &Start) {
        let Some(section) = self.start_section(start) else {
            self.no_start(start);
            return;
        };
        let size = u64::from(self.target.pointer_width / 8);
        let align = u32::try_from(size).unwrap_or(1);
        let called = self.names.resolve(start.func).to_owned();
        let which = if start.before { "ctor" } else { "dtor" };
        let name = self.names.intern(&format!("__rucc_{which}.{called}"));
        let section = self.names.intern(&section);
        let mut global = Global::new(name, size, align);
        global.linkage = IrLinkage::Internal;
        global.section = Some(section);
        let size = u32::try_from(size).unwrap_or(0);
        let reloc = self.module.add_reloc(Reloc { symbol: start.func, addend: 0, size });
        global.init = Some(self.module.push_data(&[Datum::Addr(reloc)]));
        self.place_global(global);
    }

    /// The pointer to the personality routine every landing pad in this unit is run by, which the
    /// header of the unwind table reaches through. Made once however many pads there are.
    ///
    /// gcc makes it a hidden weak object in a group of its own, so that every object of a link
    /// shares one. This one is local to the unit, which costs a word per object and needs neither
    /// a group nor a weak definition, and what an unwinder reads through it is the same address.
    /// It is a pointer rather than the routine itself because the routine is in the C runtime's
    /// shared library, and a table in a read only section cannot hold the distance to a name the
    /// dynamic linker places: the header holds the distance to this, and this holds the address.
    pub(crate) fn personality(&mut self) {
        let name = self.names.intern(rucc_ir::PERSONALITY_REF);
        if self.module.lookup(name).is_some() {
            return;
        }
        let size = u64::from(self.target.pointer_width / 8);
        let align = u32::try_from(size).unwrap_or(1);
        let mut global = Global::new(name, size, align);
        global.linkage = IrLinkage::Internal;
        let routine = self.names.intern(rucc_ir::PERSONALITY);
        let size = u32::try_from(size).unwrap_or(0);
        let reloc = self.module.add_reloc(Reloc { symbol: routine, addend: 0, size });
        global.init = Some(self.module.push_data(&[Datum::Addr(reloc)]));
        self.module.add_global(global);
    }

    /// The section an entry goes in, and [`None`] for a format with no way to ask for one.
    ///
    /// ELF has both halves and the linker sorts the numbered sections ahead of the plain one, so
    /// the number goes in the name and the order comes out right however the files were linked.
    ///
    /// COFF has the run-up only. The name is sorted by what follows the `$` and the CRT walks
    /// everything between the `.CRT$XCA` and `.CRT$XCZ` markers, so a numbered entry goes just
    /// after the first marker and an unnumbered one at `U`, which keeps the numbered ones first.
    ///
    /// Mach-O has the run-up only as well, and it has no sorting at all: the entries run in the
    /// order the section holds them, which is the order [`Self::startups`] put them in.
    ///
    /// wasm has the run-up only as well. The names are the names of ELF, which are the names that
    /// clang gives in its `-S` text, and the backend writes each entry as a constructor of the
    /// `linking` section with the number as its priority. `wasm-ld` sorts the constructors by
    /// priority and calls them from `__wasm_call_ctors`.
    fn start_section(&self, start: &Start) -> Option<String> {
        match self.target.object_format {
            ObjectFormat::Elf => {
                let base = if start.before { ".init_array" } else { ".fini_array" };
                Some(match start.priority {
                    Priority::Numbered(number) => format!("{base}.{number:05}"),
                    Priority::Unnumbered => base.to_owned(),
                })
            }
            ObjectFormat::Coff if start.before => Some(match start.priority {
                Priority::Numbered(number) => format!(".CRT$XCA{number:05}"),
                Priority::Unnumbered => ".CRT$XCU".to_owned(),
            }),
            ObjectFormat::MachO if start.before => {
                Some("__DATA,__mod_init_func,mod_init_funcs".to_owned())
            }
            ObjectFormat::Wasm if start.before => Some(match start.priority {
                Priority::Numbered(number) => format!(".init_array.{number:05}"),
                Priority::Unnumbered => ".init_array".to_owned(),
            }),
            ObjectFormat::Coff | ObjectFormat::MachO | ObjectFormat::Wasm => None,
        }
    }

    /// Reports an attribute this format has nowhere to put.
    ///
    /// Refused rather than dropped, because the whole point of the attribute is that something
    /// else calls the function and a program that quietly does not get its call has no way of
    /// noticing until whatever the function set up is missing.
    ///
    /// The run-down is what is missing on Mach-O. It used to have a terminator list and dyld
    /// stopped running it, so clang registers the call with `__cxa_atexit` from a constructor it
    /// writes for the purpose. COFF gets the same thing from [`Self::registrar`] with `atexit`,
    /// and doing it for Mach-O as well is what would take this message away.
    fn no_start(&mut self, start: &Start) {
        let which = if start.before { "constructor" } else { "destructor" };
        let format = self.target.object_format.as_str();
        let what = format!("the '{which}' attribute on a {format} target");
        self.unsupported(&what, start.span);
    }

    /// How far a name reaches outside a shared library, which is what a declaration of it said
    /// where one said anything and what the command line asked for where none did.
    ///
    /// gcc's `-fvisibility=` is written as the default rather than as an override, so the
    /// attribute wins wherever it was written, and that is the whole reason a library compiled
    /// with `-fvisibility=hidden` can still export the dozen names it means to export.
    ///
    /// The default reaches what this unit defines and stops there, which is the `defined`
    /// argument and is the whole of tamnd/rucc#1234. `-fvisibility=hidden` is a claim about the
    /// names this file puts into the library, and a name it only mentions is one it knows nothing
    /// about: `stderr` is in libc however the file that reads it was compiled, and calling it
    /// hidden tells the linker to resolve it inside this object, which it cannot do. The attribute
    /// on a declaration is a different thing and still counts, because a program that writes it
    /// has said where the definition is going to come from.
    ///
    /// Measured against gcc 16.2.0 rather than read off the manual, since the manual says the flag
    /// applies to declarations and does not say which ones. For `extern int plain;` beside
    /// `__attribute__((visibility("hidden"))) extern int marked;` at `-fPIC -fvisibility=hidden`,
    /// gcc writes `plain` as `GLOBAL DEFAULT UND` and reaches it through the global offset table,
    /// and writes `marked` as `GLOBAL HIDDEN UND` and reaches it from the instruction pointer.
    fn seen(&self, decl: DeclId, defined: bool) -> IrVisibility {
        match self.tast[decl].visibility {
            Some(Visibility::Default) => IrVisibility::Default,
            Some(Visibility::Hidden) => IrVisibility::Hidden,
            Some(Visibility::Protected) => IrVisibility::Protected,
            None if defined => self.visibility,
            None => IrVisibility::Default,
        }
    }

    /// What a name says about which DLL it is in, which only the code written for a COFF object
    /// reads.
    ///
    /// `dllimport` means something only where this unit does not define the name and `dllexport`
    /// only where it does, which is the `defined` argument, and neither means anything for a name
    /// the linker never sees. gcc and clang both drop an import on a name the file goes on to
    /// define, without a word at `-O0` and with a warning that it was ignored above that, and
    /// export a name that says both, since the definition is here. Neither exports a name this
    /// file only declares, so a header that writes `dllexport` on every prototype costs the files
    /// that include it nothing.
    fn dll(&self, decl: DeclId, defined: bool) -> Dll {
        let node = &self.tast[decl];
        if node.linkage != Linkage::External {
            return Dll::Default;
        }
        if defined && node.flags.contains(DeclFlags::DLLEXPORT) {
            Dll::Export
        } else if !defined && node.flags.contains(DeclFlags::DLLIMPORT) {
            Dll::Import
        } else {
            Dll::Default
        }
    }

    /// What the linker is told about a name, which is its C linkage unless a declaration of it
    /// wrote `weak`.
    ///
    /// The attribute is refused on internal linkage where it is read, so external is the only
    /// thing it can change, and the two things a program means by it are one thing to the linker.
    /// On a definition it says another object's definition of the name beats this one, which is
    /// how a library ships a default. On a reference to something this file does not define it
    /// says the link may leave the name undefined and hand the reference a zero address, which is
    /// how a library offers a hook and why zstd's thirty files link at all.
    ///
    /// A weakref is the other way a reference is weak, and its linkage is internal only as far as
    /// C is concerned: the symbol it is under is its target, which is some other object's. So it
    /// is a weak reference here whatever its linkage says, and [`Unit::weak_references`] settles
    /// afterwards whether the rest of the file lets it stay one.
    fn told(&self, decl: DeclId, linkage: Linkage) -> IrLinkage {
        if self.tast[decl].flags.contains(DeclFlags::WEAKREF) {
            return IrLinkage::Weak;
        }
        match linkage {
            Linkage::External if self.tast[decl].flags.contains(DeclFlags::WEAK) => IrLinkage::Weak,
            Linkage::External => IrLinkage::External,
            Linkage::Internal | Linkage::None => IrLinkage::Internal,
        }
    }

    /// A weak declaration nothing in the file names is an ordinary one, so nothing is written for
    /// it.
    ///
    /// gcc writes `.weak` for an undefined name only when the file refers to it, whether the name
    /// was made weak by the attribute or by `#pragma weak`, and a header that marks every hook it
    /// offers is a header most files that include it use none of. What counts as a reference is
    /// what [`Unit::weak_references`] counts, by symbol rather than by declaration since a name
    /// may be declared more than once, and a second name's target counts too.
    fn unused_weak(&mut self) {
        let tast = self.tast;
        let named: Vec<DeclId> = self.named.iter().copied().collect();
        let mut used: Set<Symbol> = self.aliased.clone();
        for decl in named {
            if tast[decl].kind != DeclKind::Type {
                used.insert(self.symbol_of(decl));
            }
        }
        for id in self.module.funcs() {
            let func = &mut self.module[id];
            if func.is_declaration()
                && func.linkage == IrLinkage::Weak
                && !used.contains(&func.name)
            {
                func.linkage = IrLinkage::External;
            }
        }
        for id in self.module.globals() {
            let global = &mut self.module[id];
            if global.is_declaration()
                && global.linkage == IrLinkage::Weak
                && !used.contains(&global.name)
            {
                global.linkage = IrLinkage::External;
            }
        }
    }

    /// Whether each symbol a `weakref` refers to stays a weak reference, once the whole file has
    /// been placed in the module.
    ///
    /// What gcc writes for one is `.weakref local, target`, and what the assembler makes of that
    /// is a weak undefined `target` only when nothing else in the object refers to it. A
    /// reference to the target by its own name is an ordinary one, and one ordinary reference
    /// makes the symbol ordinary for the whole object, since the object has one symbol table
    /// entry for it and not one per spelling. A definition of it in the file is the address every
    /// spelling reaches, and is left as it was defined. This gives the module the same answer
    /// directly, since the weakref was already placed under the target's name: a declaration
    /// under a weakref's target is weak when every reference the file reaches is through a
    /// weakref and external as soon as one is not.
    ///
    /// What counts as referred to is what [`reach`] found an expression naming in something it
    /// reached, rather than everything it reached, since a declaration of an external function
    /// and every object with static storage are reached whether or not anything uses them, and
    /// a declaration nothing uses is not a reference.
    fn weak_references(&mut self) {
        let tast = self.tast;
        let mut reached: Vec<DeclId> = self.named.iter().copied().collect();
        reached.retain(|&decl| {
            let node = &tast[decl];
            node.kind != DeclKind::Type
                && (node.linkage != Linkage::None || node.flags.contains(DeclFlags::WEAKREF))
        });
        let mut weak: Set<Symbol> = Set::default();
        let mut strong: Set<Symbol> = Set::default();
        for decl in reached {
            let symbol = self.symbol_of(decl);
            if tast[decl].flags.contains(DeclFlags::WEAKREF) {
                weak.insert(symbol);
            } else {
                strong.insert(symbol);
            }
        }
        for symbol in weak {
            let linkage =
                if strong.contains(&symbol) { IrLinkage::External } else { IrLinkage::Weak };
            match self.module.lookup(symbol) {
                Some(SymbolRef::Func(id)) if self.module[id].is_declaration() => {
                    self.module[id].linkage = linkage;
                }
                Some(SymbolRef::Global(id)) if self.module[id].init.is_none() => {
                    self.module[id].linkage = linkage;
                }
                _ => {}
            }
        }
    }

    /// Whether a body this unit is not meant to emit has to be emitted anyway, because this unit
    /// calls it and has nothing else to send the call to.
    ///
    /// C 6.7.4p7 says an inline definition is not an external definition, and the bargain it
    /// offers is that the call is replaced by the body, so nobody ever has to resolve the name.
    /// A compiler that inlines keeps its end of it. This one inlines only a function marked
    /// `always_inline`, so any other call left standing is a call to a name no object file
    /// defines, and the program fails at the link on a function it can see the body of. micropython is a program that does exactly that:
    /// `py/misc.h` writes `MP_COMPRESSED_ROM_TEXT` as `inline __attribute__((always_inline))`,
    /// nothing anywhere defines it out of line, and every file that reports an error calls it.
    ///
    /// So a copy goes out of line, under [`IrLinkage::LinkOnce`]. Every unit that calls one emits
    /// its own copy of the same body, the linker keeps one and the rest are discarded, and a unit
    /// that holds the real external definition beats all of them because a strong definition
    /// beats a weak one. What that costs is object size in the units that call one. What it buys
    /// is that the address of the function is the same everywhere and that the program links,
    /// which is the whole of what the program was asking for.
    ///
    /// Only when this unit names it, which is why [`reach`] stopped treating one of these as a
    /// root. An unreferenced inline definition is still emitted as nothing at all, which is what
    /// keeps a file that includes `stdio.h` from carrying its own `vprintf`, `putchar`, `getchar`
    /// and the dozen more glibc writes beside them.
    ///
    /// Under GNU's reading of `inline` the copy is marked `inline_only`, so the inliner still has
    /// the body and nothing emits it. There `extern inline` promises that the external definition
    /// is in some other object, and gcc never emits one of these bodies. glibc's `_FORTIFY_SOURCE`
    /// wrappers are exactly that: `memcpy` is written `extern inline` under `gnu_inline` with a
    /// body that calls `__builtin___memcpy_chk`, which becomes a call to `memcpy` again. That body
    /// emitted is a `memcpy` that calls itself, and it wins over the C library's inside the shared
    /// object that holds it.
    fn out_of_line(&self, decl: DeclId, emission: Emission) -> bool {
        !emission.emits() && self.reachable.contains(&decl)
    }

    /// The same for an object, where a global with no image is the declaration.
    fn place_global(&mut self, global: Global) {
        match self.module.lookup(global.name) {
            None => {
                self.module.add_global(global);
            }
            Some(SymbolRef::Global(id))
                if self.module[id].init.is_none() && global.init.is_some() =>
            {
                self.module[id] = global;
            }
            Some(_) => {}
        }
    }

    /// Whether this function is one nothing can call, which is the set that is not emitted.
    ///
    /// A name with internal linkage is not visible to another translation unit, so a definition
    /// of one that nothing here refers to is a definition of something that can never run.
    /// [`reach`](mod@crate::reach) is what worked out which those are, and an attribute that asks
    /// for the definition to be kept has already been read into the answer.
    ///
    /// A second name for it is the one reason to keep it that the walk over the tree cannot see,
    /// since what an alias points at is a string and not a reference to anything. So the symbol
    /// is what is asked about here rather than the declaration: an alias names what the linker
    /// will look for, which is what a declaration that renamed itself with `__asm__` is under.
    ///
    /// Nothing is said about it. gcc has `-Wunused-function` for a `static` function nobody
    /// wrote a call to, which is a warning about the program, and this is not that: the header
    /// that defines six of them is not the file being compiled and its author is not the person
    /// reading the output.
    fn is_dropped(&self, decl: DeclId, symbol: Symbol) -> bool {
        self.tast[decl].linkage != Linkage::External
            && !self.reachable.contains(&decl)
            && !self.aliased.contains(&symbol)
    }

    /// How everything a call to this function type hands over travels, and [`None`] for one the
    /// walk cannot make.
    ///
    /// `actual` is the types of the arguments at a call site, which matter only past the end of
    /// the prototype: what a variadic argument does is decided from what was written there, and
    /// there is no parameter to decide it from. A definition passes nothing for it.
    pub(crate) fn plan(&mut self, ty: TypeId, actual: &[TypeId], span: Span) -> Option<Plan> {
        self.plan_with(ty, actual, false, span)
    }

    /// The same, as the call site sees it rather than as the function does.
    ///
    /// The two differ for a type that is not a prototype. An old style definition is the one of
    /// those that knows what its parameters are, and 6.5.2.2p6 checks a call against a prototype
    /// and against nothing at all otherwise, so a parameter it disagrees with does not make the
    /// call wrong and cannot be what the argument travels as either: the value at the call is
    /// the argument's own type and nothing converted it. So a parameter the argument facing it
    /// is compatible with is used, which is the usual case and is what makes the call go to the
    /// name, and one it is not compatible with gives way to what was actually written. A call
    /// like that is undefined behaviour if control reaches it and the file still has to
    /// translate, which is the same position [`Body::direct`](crate::body) already takes.
    pub(crate) fn call_plan(&mut self, ty: TypeId, actual: &[TypeId], span: Span) -> Option<Plan> {
        self.plan_with(ty, actual, true, span)
    }

    fn plan_with(
        &mut self,
        ty: TypeId,
        actual: &[TypeId],
        at_call: bool,
        span: Span,
    ) -> Option<Plan> {
        match self.try_plan(ty, actual, at_call) {
            Ok(plan) => Some(plan),
            Err(what) => {
                self.unsupported(what, span);
                None
            }
        }
    }

    /// Whether a function, or the function a pointer points at, is of a type written
    /// `__attribute__((nocf_check))`: the function has no landing pad and a call through a
    /// pointer to it carries `rucc_ir::Flags::NOTRACK`.
    pub(crate) fn untracked(&self, ty: TypeId) -> bool {
        let canonical = self.types.canonical(ty);
        let canonical = match self.types.kind(canonical) {
            TypeKind::Pointer(pointee) => self.types.canonical(pointee),
            _ => canonical,
        };
        matches!(self.types.kind(canonical), TypeKind::Function(id) if self.types.signature(id).nocf)
    }

    /// Whether a call through `ty`, a function or a pointer to one, can come back by a jump, from
    /// `__attribute__((indirect_return))`: the call carries `rucc_ir::Flags::INDIRECT_RETURN` and
    /// a function of the type `rucc_ir::AttrSet::INDIRECT_RETURN`. Only a prototype says so, since
    /// gcc reads the attribute off the argument types and `()` has none.
    pub(crate) fn returns_by_jump(&self, ty: TypeId) -> bool {
        let canonical = self.types.canonical(ty);
        let canonical = match self.types.kind(canonical) {
            TypeKind::Pointer(pointee) => self.types.canonical(pointee),
            _ => canonical,
        };
        matches!(
            self.types.kind(canonical),
            TypeKind::Function(id)
                if self.types.signature(id).indirect_return && self.types.signature(id).prototyped
        )
    }

    /// The plan, or what stopped it, said to nobody.
    fn try_plan(&self, ty: TypeId, actual: &[TypeId], at_call: bool) -> Result<Plan, &'static str> {
        let canonical = self.types.canonical(ty);
        let canonical = match self.types.kind(canonical) {
            // A call goes through a pointer to a function, and the type in hand may be either.
            TypeKind::Pointer(pointee) => self.types.canonical(pointee),
            _ => canonical,
        };
        let TypeKind::Function(id) = self.types.kind(canonical) else {
            return Err("a call through something that is not a function");
        };
        let signature = self.types.signature(id);
        let ret = signature.ret;
        // Normalised against `-mregparm=`, which a variadic function does not follow. A function
        // with no prototype is not variadic for this, which is how gcc has it.
        let convention = self.target.convention_for(signature.convention, signature.variadic);
        // A function declared without a prototype takes what it is given, which is what a
        // signature with no parameters and no end to them says. C23 removed these and this is
        // what `int f();` means in every dialect before it.
        //
        // wasm32 is the exception, as clang has it. A wasm function has one type and a call must
        // give exactly that type, so the function is the type of its definition, `int f()` takes
        // nothing, and a call passes each promoted argument as a parameter. A call that does not
        // match the callee goes through the table, where the check traps when it runs.
        let wasm = self.target.tuple.arch() == rucc_tuple::Arch::Wasm32;
        let variadic = signature.variadic || (!signature.prototyped && !wasm);
        let params = if at_call && !signature.prototyped {
            // An argument past the end of the list has no parameter to travel as, which is what
            // a call to an unprototyped function with more arguments than the definition takes
            // is, so the list ends where the arguments do. On wasm32 each argument past the end
            // is a parameter of its own type.
            let past = if wasm { actual.get(signature.params.len()..).unwrap_or(&[]) } else { &[] };
            signature
                .params
                .iter()
                .zip(actual)
                .map(|(&param, &arg)| if compatible(self.types, param, arg) { param } else { arg })
                .chain(past.iter().copied())
                .collect()
        } else {
            signature.params.clone()
        };

        abi::plan(self.types, self.target, convention, ret, &params, actual, variadic)
    }

    /// The image of an initializer: the entries in ascending order, with the gaps zeroed, and
    /// how many bytes it covers.
    ///
    /// The count is the size that was asked for except when a flexible array member was given
    /// something to hold, which is the one case where an image is larger than the type it is an
    /// image of.
    pub(crate) fn image(
        &mut self,
        init: Option<InitList>,
        size: u64,
        span: Span,
    ) -> (DataList, u64) {
        let Some(init) = init else { return (self.zeros(size), size) };
        let (data, at) = self.pieces(init, size, span);
        if data.is_empty() {
            return (self.zeros(at), at);
        }
        (self.module.push_data(&data), at)
    }

    /// The data an image is made of, before it becomes a [`DataList`].
    ///
    /// This is apart from [`Self::image`] so that an image can be built inside another one,
    /// which is what a compound literal used as a value in an initializer needs.
    fn pieces(&mut self, init: InitList, size: u64, span: Span) -> (Vec<Datum>, u64) {
        let entries = self.in_image_order(&self.tast[init]);
        let mut packed = self.packed(&entries, size);
        let mut data: Vec<Datum> = Vec::with_capacity(entries.len());
        let mut at = 0;
        for entry in entries {
            let piece = self.entry(entry, &mut packed, size);
            if piece.is_empty() {
                continue;
            }
            let covered: u64 = piece.iter().map(|datum| datum.size(&self.module)).sum();
            match entry.offset.cmp(&at) {
                Ordering::Greater => data.push(Datum::Zero(entry.offset - at)),
                // An entry that begins inside the one before it, which is neither the same
                // place nor a later one. A union whose members are initialized through two
                // designators is the way to write it. The earlier bytes are already in the
                // list and the image cannot take them out again, so this is refused, and
                // nothing here is wrong enough to drop the rest of the image.
                Ordering::Less => {
                    self.unsupported("an initializer that writes over an earlier one", span);
                    continue;
                }
                Ordering::Equal => {}
            }
            at = entry.offset + covered;
            data.extend(piece);
        }
        if at < size {
            // The tail of a partly initialized object, which C says is zero. So is the tail of
            // an array the initializer did not fill, and so is every byte of padding.
            data.push(Datum::Zero(size - at));
            at = size;
        }
        (data, at)
    }

    /// The entries an image is written from, which is not the order they were written in.
    ///
    /// A designator names a place, and the places may be named in any order at all:
    /// `{ .b = 2, .a = 1 }` is the same object as `{ .a = 1, .b = 2 }` and C says so in as many
    /// words. An image is bytes in ascending order, so the entries are put in that order here.
    /// The sort is stable, which is what makes the rest of the rule work: naming one place
    /// twice is legal and the last of them is the one that stands, so among the entries at one
    /// offset the written order is kept and all but the last are dropped.
    ///
    /// A bit-field is never dropped, because several of them share one offset without writing
    /// over anything. Which bytes they came to is settled by [`Self::packed`] before this runs
    /// and the whole run goes in under the first entry that has a bit in it.
    fn in_image_order(&self, entries: &[InitEntry]) -> Vec<InitEntry> {
        let mut sorted = entries.to_vec();
        sorted.sort_by_key(|entry| entry.offset);
        let mut kept: Vec<InitEntry> = Vec::with_capacity(sorted.len());
        for entry in sorted {
            if !entry.is_bit_field() {
                let over = |last: &InitEntry| last.offset == entry.offset && !last.is_bit_field();
                while kept.last().is_some_and(over) {
                    kept.pop();
                }
            }
            kept.push(entry);
        }
        kept
    }

    /// What one entry of an initializer puts in the image.
    ///
    /// A bit-field is not a datum of its own, because two of them can live in one byte and an
    /// image is written in bytes. They were put together into their bytes by [`Self::packed`]
    /// before this ran, and the whole run of bytes goes in under the first entry that lies in
    /// it, which is why a later one in the same run answers with nothing.
    ///
    /// The zeroes at the end of a run are left off it, and a run that is nothing but zeroes
    /// answers with nothing at all. Either way the gap before the next entry covers them, which
    /// is the same image and is a smaller one to carry, and it is what keeps an object whose
    /// bit-fields are all zero in `.bss`. A zero at the front of a run or inside one stays, since
    /// that is where the run starts and what makes it one run. The run comes out of the map
    /// whatever is in it, so a later entry lying in it answers with nothing for the usual reason
    /// rather than writing the run a second time.
    ///
    /// An entry is usually one datum and a compound literal read is the reason the answer is a
    /// list: that entry is a whole object and puts as many data in as the object it is.
    fn entry(&mut self, entry: InitEntry, packed: &mut BTreeMap<u64, u8>, size: u64) -> Vec<Datum> {
        if entry.is_bit_field() {
            let Some(bytes) = take_run(packed, entry.offset) else { return Vec::new() };
            let Some(last) = bytes.iter().rposition(|&byte| byte != 0) else { return Vec::new() };
            return vec![Datum::Bytes(self.module.push_bytes(&bytes[..=last]))];
        }
        if let Some(literal) = self.literal_read(entry.value) {
            return self.literal_image(literal, self.tast.expr_span(entry.value));
        }
        if entry.reverse {
            if let Some(reversed) = self.reversed_datum(entry) {
                return reversed;
            }
        }
        // How much room is left in the object, which is what a string literal longer than the
        // array it initializes is cut down to. An entry that begins where the object ends is the
        // initializer of a flexible array member, and there the object grows to hold what was
        // written rather than the value being cut to fit, so nothing is taken off it.
        let room = if entry.offset < size { size - entry.offset } else { u64::MAX };
        if let Some(halves) = self.complex_image(entry.value) {
            return halves;
        }
        self.datum(entry.value, room).into_iter().collect()
    }

    /// A complex constant as the two data an image holds it in, and [`None`] for anything else.
    ///
    /// A complex value is two real ones and an image is bytes, so `1.0 + 2.0i` goes in as the two
    /// halves one after the other, which is the layout every ABI here already reads it as. It is
    /// two data rather than one because a datum is one scalar, and it is here rather than in
    /// [`Self::datum`] for the same reason.
    fn complex_image(&mut self, value: ExprId) -> Option<Vec<Datum>> {
        let ty = self.tast[value].ty;
        let part = rucc_types::real_part(self.types, ty)?;
        let span = self.tast.expr_span(value);
        // Everything below this point answers with something, because the folding reports its own
        // failure and asking for the value a second time would report it twice.
        let folded = match self.fold(value) {
            Some(folded) => folded,
            None => return Some(Vec::new()),
        };
        let Some(ty) = repr::value_type(self.types, self.target, part) else {
            self.unsupported("this complex initializer", span);
            return Some(Vec::new());
        };
        // Each half goes in as the half's own type would, which is the bits of a floating value
        // and the number of an integer one.
        let halves = match folded {
            Const::Complex { real, imag } => {
                [real, imag].map(|half| Imm::from_bits(half.to_bits()))
            }
            Const::ComplexInt { real, imag } => [real, imag].map(|half| Imm::int(half, ty)),
            _ => {
                self.unsupported("this complex initializer", span);
                return Some(Vec::new());
            }
        };
        let data = halves
            .into_iter()
            .map(|half| {
                let imm = self.module.add_imm(half);
                Datum::Scalar { ty, value: imm }
            })
            .collect();
        Some(data)
    }

    /// The compound literal an entry reads, if that is what the entry is.
    ///
    /// Reading an object is a node of its own, so a literal used as a value comes through as a
    /// read of a literal. A literal whose address is taken is not a read and is not this: that
    /// one folds to an address and goes in as a relocation, with the object it points at emitted
    /// on its own.
    ///
    /// GNU's cast to a union is the one literal that arrives without the read. The front end
    /// builds `(union semun) &buf` as a literal holding the value in the member of its type, and
    /// that literal is already the value rather than an object to be read, which is how LTP's
    /// `semctl01` fills a static table.
    fn literal_read(&self, value: ExprId) -> Option<DeclId> {
        let operand = match self.tast[value].kind {
            ExprKind::Convert { kind: Conversion::Lvalue, operand } => operand,
            ExprKind::CompoundLiteral(_) => value,
            _ => return None,
        };
        match self.tast[operand].kind {
            ExprKind::CompoundLiteral(decl) => Some(decl),
            _ => None,
        }
    }

    /// The bytes a compound literal contributes where it is read, which are its own image.
    ///
    /// The literal has static storage duration here, since a file-scope initializer is the only
    /// place this is reached from, and C 6.7.11p4 is what lets it stand as a constant element.
    /// Its own initializer is built at the offset the entry is at, so the parent image ends up
    /// with the literal's bytes laid into it rather than a name pointing at a second object.
    fn literal_image(&mut self, literal: DeclId, span: Span) -> Vec<Datum> {
        let size = repr::size_of(self.types, self.target, self.tast[literal].ty);
        let Some(init) = self.tast[literal].init else {
            return if size == 0 { Vec::new() } else { vec![Datum::Zero(size)] };
        };
        self.pieces(init, size, span).0
    }

    /// The bit-fields of an initializer, put together into the bytes they lie in.
    ///
    /// Every byte a field lies in is in the map, whatever the bits it put there are. It is
    /// tempting to leave a zero byte out, on the grounds that what an image does not say is zero
    /// anyway, and it is wrong: the run a field's bytes make is taken out of the map from the
    /// byte the field starts at, so a field whose first byte happens to be zero would have its
    /// whole run left behind and `struct { unsigned f : 20; } x = { 0x12300 };` would read as
    /// zero. A run that is all zeroes is written as zeroes by [`Self::entry`], so an object that
    /// really is zero still costs nothing in the image.
    ///
    /// A field named twice takes only the bits of the field, so the last of them stands and does
    /// not read as the two values together.
    fn packed(&mut self, entries: &[InitEntry], size: u64) -> BTreeMap<u64, u8> {
        let mut bytes = BTreeMap::new();
        for entry in entries.iter().filter(|entry| entry.is_bit_field()) {
            let Some(folded) = self.fold(entry.value) else { continue };
            let Const::Int(number) = folded else {
                let span = self.tast.expr_span(entry.value);
                let what = "a bit-field initialized by something that is not an integer";
                self.unsupported(what, span);
                continue;
            };
            let width = entry.bit_width;
            let ones = if width >= 128 { u128::MAX } else { (1u128 << width) - 1 };
            // Which bytes the field lies in and where in them it sits. A reversed field lies in
            // the same bytes and is counted from the top of them, and the byte at its address is
            // then the most significant of the ones the value is assembled in rather than the
            // least, which is why the walk below runs the other way as well.
            let span = u64::from((entry.bit_offset + width).div_ceil(8));
            let start = if entry.reverse {
                u32::try_from(span * 8).unwrap_or(u32::MAX) - entry.bit_offset - width
            } else {
                entry.bit_offset
            };
            let mut mask = ones << start;
            let mut placed = ((number as u128) & ones) << start;
            let mut step = 0;
            while mask != 0 && step < span {
                let at = if entry.reverse {
                    entry.offset + span - 1 - step
                } else {
                    entry.offset + step
                };
                if at < size {
                    let (bits, keep) = ((placed & 0xff) as u8, !((mask & 0xff) as u8));
                    let byte = bytes.entry(at).or_insert(0);
                    *byte = (*byte & keep) | bits;
                }
                mask >>= 8;
                placed >>= 8;
                step += 1;
            }
        }
        bytes
    }

    /// What one entry of a record whose scalars are stored the other way round puts in the image.
    ///
    /// The bytes of the value, written in the order opposite to the target's, which is the whole of
    /// what the attribute asks for. It answers with nothing where the ordinary path is already
    /// right: a value one byte wide has only one order, and an aggregate is bytes its own members
    /// put there in whatever order each of them is stored in.
    ///
    /// Two things are refused rather than written the wrong way. A complex value is two scalars and
    /// this is one, and an address is a number the linker fills in later and there is nowhere to
    /// say it goes in backwards. Both are worth an answer one day and neither is worth a wrong one.
    fn reversed_datum(&mut self, entry: InitEntry) -> Option<Vec<Datum>> {
        let ty = self.tast[entry.value].ty;
        let span = self.tast.expr_span(entry.value);
        if is_complex(self.types, ty) {
            let what = "a complex member of a record whose scalars are stored the other way round";
            self.unsupported(what, span);
            return Some(Vec::new());
        }
        let size = repr::size_of(self.types, self.target, ty);
        if size < 2 || !is_scalar(self.types, ty) {
            return None;
        }
        let bits = match self.fold(entry.value) {
            Some(Const::Int(number)) => number as u128,
            Some(Const::Float(number)) => number.to_bits(),
            Some(Const::Address(Address { base: Base::Absolute, offset })) => offset as u128,
            Some(_) => {
                let what = "an address in a record whose scalars are stored the other way round";
                self.unsupported(what, span);
                return Some(Vec::new());
            }
            None => return Some(Vec::new()),
        };
        let take = cap(size).min(16);
        let mut bytes = bits.to_le_bytes()[..take].to_vec();
        if self.target.little_endian {
            bytes.reverse();
        }
        Some(vec![Datum::Bytes(self.module.push_bytes(&bytes))])
    }

    /// One entry of an image, given how many bytes are left in the object it goes in.
    fn datum(&mut self, value: ExprId, room: u64) -> Option<Datum> {
        let tast = self.tast;
        let ty = tast[value].ty;
        let span = tast.expr_span(value);
        if let TypeKind::Array { .. } = self.types.kind(self.types.canonical(ty)) {
            // An array in an initializer is a string literal initializing it, because that is
            // the only way an array is ever a value. `char s[2] = "hi";` drops the terminator,
            // which is the one case where the literal is longer than what it initializes, and
            // the front end has already given the value the type of the array it is filling, so
            // the type is what says how many of the literal's bytes are part of it. `room` is
            // still consulted because a flexible array member is filled by a literal that keeps
            // its own type and there is no size in the object for it to be cut to.
            let ExprKind::Str(id) = tast[value].kind else {
                self.unsupported("this initializer", span);
                return None;
            };
            let bytes = tast[id].bytes(self.target);
            let holds = repr::size_of(self.types, self.target, ty);
            let take = bytes.len().min(cap(holds)).min(cap(room));
            return Some(Datum::Bytes(self.module.push_bytes(&bytes[..take])));
        }

        let size = repr::size_of(self.types, self.target, ty);
        match self.fold(value)? {
            Const::Int(number) => {
                let ty = repr::value_type(self.types, self.target, ty)?;
                // An integer constant of pointer type is a null pointer constant, which is what
                // `NULL` is, or an address the program wrote as a number. An image is bytes and
                // `ptr` says nothing about how many, so it goes in as the integer it is at the
                // width the target's addresses have. An address the linker has to fill in is
                // the arm below, and is the only one that stays a pointer.
                let ty = if ty.is_ptr() { Type::int(self.target.pointer_width) } else { ty };
                let imm = self.module.add_imm(Imm::int(number, ty));
                Some(Datum::Scalar { ty, value: imm })
            }
            Const::Float(number) => {
                let ty = repr::value_type(self.types, self.target, ty)?;
                let imm = self.module.add_imm(Imm::from_bits(number.to_bits()));
                Some(Datum::Scalar { ty, value: imm })
            }
            // A complex constant is two scalars and this answers with one, so it is not one of
            // these. [`Self::complex_image`] puts one in before this is reached.
            Const::Complex { .. } | Const::ComplexInt { .. } => None,
            // An address into nothing is a number, so it goes into the image as one and there is
            // no relocation for the linker to fill in. `static char *p = &((struct S *)0)->f;` is
            // a pointer whose value is known here, and the walk that folded it already said so.
            Const::Address(Address { base: Base::Absolute, offset }) => {
                let ty = repr::value_type(self.types, self.target, ty)?;
                let ty = if ty.is_ptr() { Type::int(self.target.pointer_width) } else { ty };
                let imm = self.module.add_imm(Imm::int(offset, ty));
                Some(Datum::Scalar { ty, value: imm })
            }
            // Two labels, both named for the image the way one is for `&&l`, and the width is
            // the type's since the distance is a number and not an address.
            Const::Apart { to, from } => {
                let to = self.label_name(to);
                let from = self.label_name(from);
                let size = u32::try_from(size).unwrap_or(0);
                let to = self.module.add_reloc(Reloc { symbol: to, addend: 0, size });
                Some(Datum::Apart { to, from })
            }
            Const::Address(address) => {
                let symbol = match address.base {
                    Base::Decl(decl) => {
                        // A compound literal is an object nothing declares, so the address of
                        // one is also the only thing that asks for it to be emitted. Without
                        // this the image names a symbol the module never defines and the link
                        // is what finds out. Anything with a name of its own is left alone,
                        // since the walk over the unit reaches those on its own.
                        if self.tast[decl].name.is_none() {
                            self.local_static(decl);
                        }
                        self.symbol_of(decl)
                    }
                    Base::Str(id) => self.string(id),
                    Base::Label(label) => self.label_name(label),
                    // Answered above, where it becomes a number rather than a reference.
                    Base::Absolute => return None,
                };
                let addend = i64::try_from(address.offset).unwrap_or(0);
                let size = u32::try_from(size).unwrap_or(0);
                Some(Datum::Addr(self.module.add_reloc(Reloc { symbol, addend, size })))
            }
        }
    }

    /// An image of nothing but zeros, which is what a tentative definition has.
    ///
    /// Even when there are none. An empty struct is an object of no bytes, and an image of no
    /// zeros is what sends it to `.bss` with gcc rather than to `.data`, where an image of nothing
    /// sends it. The kernel's `static struct lock_class_key __key` is one when lockdep is off, and
    /// in `.data` it lands on the same address as the next object, which modpost then names it by.
    fn zeros(&mut self, size: u64) -> DataList {
        self.module.push_data(&[Datum::Zero(size)])
    }

    /// The global a string literal is emitted as, making it the first time it is asked for.
    pub(crate) fn string(&mut self, id: StrId) -> Symbol {
        if let Some(&symbol) = self.strings.get(&id) {
            return symbol;
        }
        let literal = &self.tast[id];
        let bytes = literal.bytes(self.target);
        let align = literal.encoding.element_width(self.target) / 8;
        if self.tast.is_function_name(id) {
            return self.function_name(id, bytes, align);
        }
        let symbol = self.names.intern(&format!(".Lstr.{}", self.strings.len()));

        let mut global = Global::new(symbol, bytes.len() as u64, align.max(1));
        global.linkage = IrLinkage::Internal;
        // Not because the type says so, since a literal is an array of `char` and not of
        // `const char`, but because writing to one is undefined and every target puts them
        // somewhere read-only.
        global.constant = true;
        global.literal = true;
        // And one the optimizer may take away once nothing names it, as a `static` object is,
        // since the function or table that named it may itself have gone.
        global.droppable = true;
        let range = self.module.push_bytes(&bytes);
        global.init = Some(self.module.push_data(&[Datum::Bytes(range)]));
        self.module.add_global(global);
        self.strings.insert(id, symbol);
        symbol
    }

    /// The object `__func__` stands for in one function, which is `static const char __func__[]`
    /// and not a literal.
    ///
    /// gcc emits it as a local `__func__.N` in `.rodata`, so it is never merged with a literal
    /// spelled the same and is aligned the way gcc aligns any array of its size on x86-64 when
    /// optimizing: eight bytes for one of at least eight, sixteen for one of at least sixteen, and
    /// thirty two for one of at least thirty two. The kernel names `__func__` in nearly every
    /// warning, and its objects have a `.rodata` under gcc for that alone.
    fn function_name(&mut self, id: StrId, bytes: Vec<u8>, align: u32) -> Symbol {
        let symbol = self.names.intern(&format!("__func__.{}", self.strings.len()));
        let size = bytes.len() as u64;
        let align = if self.target.tuple.arch() == rucc_tuple::Arch::X86_64 {
            match size {
                32.. => 32,
                16.. => 16,
                8.. => 8,
                _ => align.max(1),
            }
        } else {
            align.max(1)
        };
        let mut global = Global::new(symbol, size, align);
        global.linkage = IrLinkage::Internal;
        global.constant = true;
        global.droppable = true;
        let range = self.module.push_bytes(&bytes);
        global.init = Some(self.module.push_data(&[Datum::Bytes(range)]));
        self.module.add_global(global);
        self.strings.insert(id, symbol);
        symbol
    }

    /// The name a label an image holds the address of is known by, minting one the first time.
    ///
    /// The number is what makes two labels in two functions two names, the same way it does for a
    /// `static` inside a function. Nothing but the relocation and the definition the back end
    /// writes for it ever reads this, so the spelling only has to be one the object format lets a
    /// local symbol have, and the leading dot is what keeps it out of the symbol table on the
    /// formats that have the convention.
    pub(crate) fn label_name(&mut self, label: LabelId) -> Symbol {
        if let Some(&symbol) = self.labels.get(&label) {
            return symbol;
        }
        let symbol = self.names.intern(&format!(".Llbl.{}", self.labels.len()));
        self.labels.insert(label, symbol);
        symbol
    }

    /// The name a label was given, or `None` for a label no image points at.
    pub(crate) fn named_label(&self, label: LabelId) -> Option<Symbol> {
        self.labels.get(&label).copied()
    }

    /// The name the C library gives a function the program named with the `__builtin_` prefix,
    /// and nothing for every other name.
    ///
    /// `__builtin_abort` is a call to `abort`: the prefix is how a program reaches the function
    /// the library promises where a macro or a definition of its own has taken the plain name,
    /// so the two spellings are one function and the one the linker will look for is the short
    /// one. Which names those are is [`rucc_sema::library_name`]'s to say, since it is the same
    /// answer the front end declared them out of.
    fn library_name(&mut self, name: Symbol) -> Option<Symbol> {
        let library = rucc_sema::library_name(self.names.resolve(name))?;
        let symbol = self.names.intern(library);
        // And then whatever the file said that name is called in the object file. A program is
        // allowed to declare `memcpy` with an assembler name of its own and go on calling
        // `__builtin_memcpy`, and what it means by that is the renamed one: the prefix picks the
        // function out of the library, it does not ask for a symbol the file has renamed away.
        Some(self.renamed.get(&symbol).copied().unwrap_or(symbol))
    }

    /// Whether a call written with this name may be taken to mean the C library function of the
    /// name without the prefix, which is the checker's rule read again where the call is built.
    pub(crate) fn means_the_library(&self, spelled: &str) -> bool {
        match spelled.strip_prefix("__builtin_") {
            Some(_) => true,
            None => self.builtins && !self.no_builtin.iter().any(|off| off == spelled),
        }
    }

    /// The symbol of one of the objects libgcc defines and the program only reads, declared in
    /// the module the first time it is asked for.
    ///
    /// These are `__cpu_model` and `__cpu_features2`, which `__builtin_cpu_supports` and
    /// `__builtin_cpu_is` read. The program never declares them, so without this the load would
    /// be from a name the module knows nothing about. The declaration is the one gcc makes, an
    /// external object with the ordinary visibility and no image, and a declaration the program
    /// did write of the same name is left as it is, since it says the same thing.
    pub(crate) fn libgcc_object(&mut self, name: &str, size: u64) -> Symbol {
        let symbol = self.names.intern(name);
        // External, the ordinary visibility and no image, which is what a new global is.
        self.place_global(Global::new(symbol, size, 4));
        symbol
    }

    /// The name an object or a function is known by in the object file.
    pub(crate) fn symbol_of(&mut self, decl: DeclId) -> Symbol {
        let tast = self.tast;
        let node = &tast[decl];
        // The assembler name a declaration wrote, which is the symbol whatever the identifier
        // spells. It stands for a `static` and for a local one as well as for a name the linker
        // sees, so it is read before anything else here: a program that renames a name has said
        // what the symbol is, and the numbering below is for the ones that have not.
        if let Some(label) = node.asm_label {
            let spelling = assembler_name(tast, self.target, label);
            return self.names.intern(&spelling);
        }
        if node.linkage != Linkage::None {
            let Some(name) = node.name else { return self.names.intern(".Lanon") };
            let ty = node.ty;
            let symbol = self.library_name(name).unwrap_or(name);
            return self.decorated(ty, symbol);
        }
        if let Some(&symbol) = self.statics.get(&decl) {
            return symbol;
        }
        // A `static` in a function, or a compound literal with static storage duration. The
        // number is what makes two of them in two functions two objects.
        let base = match node.name {
            Some(name) => self.names.resolve(name).to_string(),
            None => ".Lanon".to_string(),
        };
        let symbol = self.names.intern(&format!("{base}.{}", self.statics.len()));
        self.statics.insert(decl, symbol);
        symbol
    }

    /// The name a function of that type is known by in the object file on 32-bit Windows, which
    /// for a `stdcall` function is its name with `@` and the bytes of its arguments after it and
    /// for a `fastcall` one the same with another `@` in front.
    ///
    /// The count is every parameter the declaration names, each taken up to a word, and not the
    /// address a structure comes back through. It is what the argument area holds, less that
    /// address, and the registers `fastcall` passes the first two in are counted as though they
    /// were not registers. That is gcc's and Microsoft's number both, so `_Sleep@4` and
    /// `@f@12` are what a library built by either has in it. The underscore every C name gets on
    /// this target is added by the object writer, which is why `stdcall` has none here.
    ///
    /// Everything else, a variadic function included since the front end never gives one either
    /// convention, is the name as it was.
    fn decorated(&mut self, ty: TypeId, symbol: Symbol) -> Symbol {
        if self.target.object_format != ObjectFormat::Coff
            || self.target.tuple.arch() != rucc_tuple::Arch::X86
        {
            return symbol;
        }
        let TypeKind::Function(id) = self.types.kind(self.types.canonical(ty)) else {
            return symbol;
        };
        let signature = self.types.signature(id);
        let front = match signature.convention {
            Convention::Stdcall => "",
            Convention::Fastcall => "@",
            _ => return symbol,
        };
        if signature.variadic {
            return symbol;
        }
        let word = u64::from(self.target.pointer_width / 8);
        let bytes: u64 = signature
            .params
            .iter()
            .map(|&param| repr::size_of(self.types, self.target, param).next_multiple_of(word))
            .sum();
        let name = self.names.resolve(symbol).to_string();
        self.names.intern(&format!("{front}{name}@{bytes}"))
    }

    /// Emits the global for an object with static storage duration declared inside a function.
    pub(crate) fn local_static(&mut self, decl: DeclId) {
        if !self.done.insert(decl) {
            return;
        }
        match self.tast[decl].kind {
            // A function declared inside a body is a declaration of the function, not an
            // object with static storage that happens to be one.
            DeclKind::Function => self.function(decl),
            DeclKind::Object => self.object(decl),
            DeclKind::Type => {}
        }
    }

    /// The value of a constant expression, reporting what folding it reported.
    ///
    /// Everything this is asked about is part of the image of an object that exists before the
    /// program runs, which is the one place C23 6.6p10 lets a compiler take more than the rest of
    /// 6.6 does, so it asks for the reading the front end already accepted there. Asking the
    /// strict way instead would refuse here what was allowed a pass earlier, which is a wrong
    /// answer arriving late rather than an extra check.
    ///
    /// A `const` object is read as its value for the same reason. The front end allowed that only
    /// under the GNU dialects and refused the program under the others, so whatever reaches here
    /// with one in it was allowed, and the dialect does not need asking again.
    fn fold(&mut self, expr: ExprId) -> Option<Const> {
        let mut eval = Eval::new(self.tast, self.types, self.target, self.names).objects(true);
        let folded = eval.initializer(expr);
        let reported = eval.finish();
        self.diagnostics.extend(reported);
        match folded {
            Ok(value) => Some(value),
            Err(stop) => {
                if !stop.poisoned {
                    let span = self.tast.expr_span(stop.at);
                    self.unsupported("an initializer this compiler cannot fold", span);
                }
                None
            }
        }
    }

    /// Reports a construct the walk does not build IR for yet.
    pub(crate) fn unsupported(&mut self, what: &str, span: Span) {
        self.diagnostics.push(
            Diagnostic::error(format!("{what} is not supported yet"), span).with_code("E0519"),
        );
    }

    /// Reports a call to a builtin this compiler knows the name of and does nothing with.
    ///
    /// It is its own message rather than [`Self::unsupported`] because the construct is not the
    /// problem: a call is a call, and what is missing is the one function it goes to. The note is
    /// what a reader needs, since a builtin is the one name a programmer does not expect to have
    /// to provide and the alternative to this message is a linker asking them for it.
    pub(crate) fn missing_builtin(&mut self, spelled: &str, span: Span) {
        let message = format!("`{spelled}` is not implemented yet");
        let note = "a call to it would go to a symbol no object file defines, so this is refused \
                    here rather than at the link";
        self.diagnostics.push(Diagnostic::error(message, span).with_code("E0686").note(note, span));
    }
}

/// A count of bytes as a length of a slice of them, saturating on a target whose addresses are
/// wider than this host's.
fn cap(bytes: u64) -> usize {
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// The run of bytes a bit-field entry starts, taken out of the map.
///
/// [`None`] when there is no byte at that offset, which means an earlier entry in the same run
/// already took it, since [`Unit::packed`] puts every byte a field lies in into the map.
fn take_run(bytes: &mut BTreeMap<u64, u8>, start: u64) -> Option<Vec<u8>> {
    let mut run = vec![bytes.remove(&start)?];
    let mut at = start + 1;
    while let Some(byte) = bytes.remove(&at) {
        run.push(byte);
        at += 1;
    }
    Some(run)
}

/// The symbol an assembler name stands for, spelled the way every other name in the module is.
///
/// An `asm` label is the name the object file holds, underscore and all, and every other symbol
/// here is the name C spells, which the listing and the object writer decorate for Mach-O on the
/// way out. So on Mach-O a label's own underscore comes off here and goes back on there, which is
/// how Apple's `FILE *fopen(...) __asm("_fopen")` stays `_fopen` rather than becoming `__fopen`.
/// A Mach-O label with no underscore names a symbol no C name decorates to, and is left alone.
///
/// COFF on i386 decorates the same way, and mingw-w64's headers rename a function to its
/// underscored name with `__asm__("_" "name")` exactly as Apple's do.
fn assembler_name(tast: &Tast, target: &TargetInfo, id: StrId) -> String {
    let spelled: String =
        tast[id].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect();
    let underscored = target.object_format == ObjectFormat::MachO
        || (target.object_format == ObjectFormat::Coff
            && target.tuple.arch() == rucc_tuple::Arch::X86);
    if underscored {
        if let Some(bare) = spelled.strip_prefix('_') {
            return bare.to_string();
        }
    }
    spelled
}
