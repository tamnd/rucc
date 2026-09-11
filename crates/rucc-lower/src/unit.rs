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
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_ir::{
    Alias, AttrSet, DataList, Datum, FpContract, Func, Global, Imm, Linkage as IrLinkage, Meta,
    Module, Reloc, SymbolRef, TlsModel, Type, Visibility as IrVisibility,
};
use rucc_sema::{
    Base, Const, Conversion, DeclId, DeclKind, Definition, Eval, ExprId, ExprKind, InitEntry,
    InitList, Linkage, StorageDuration, StrId, Tast, Visibility,
};
use rucc_target::TargetInfo;
use rucc_types::{TypeId, TypeKind, Types, compatible};

use crate::abi::{self, Plan};
use crate::aliasing;
use crate::body;
use crate::directives;
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
            .finish_non_exhaustive()
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
        read,
    } = cx;
    let module = Module::new(names.intern(name), target);
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
        read,
        module,
        diagnostics: Vec::new(),
        strings: HashMap::new(),
        statics: HashMap::new(),
        done: HashSet::new(),
        aliases: Vec::new(),
        aliased: HashSet::new(),
        reachable: reach::reachable(tast),
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
    /// How a file a `.incbin` names is read. See [`Context::read`].
    read: &'a mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    pub(crate) module: Module,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// The global each string literal was emitted as, so that two mentions of one literal are
    /// one object.
    strings: HashMap<StrId, Symbol>,
    /// The name each object with no linkage was given.
    statics: HashMap<DeclId, Symbol>,
    /// What has been emitted, because a redeclaration is the same declaration seen twice.
    done: HashSet<DeclId>,
    /// The declarations that are a second name for something rather than a thing of their own,
    /// in the order the file made them.
    ///
    /// Held back rather than emitted where they are met, because what an alias points at may be
    /// written below it and whether anything defines it is a question only the whole file
    /// answers.
    aliases: Vec<DeclId>,
    /// The symbols something in the file is a second name for.
    ///
    /// A `static` function nothing calls is not emitted, and being what an alias points at is a
    /// reason to emit one that no reference in the file says: the string an alias names is not a
    /// use of anything as far as the walk over the tree is concerned.
    aliased: HashSet<Symbol>,
    /// What something in the file reaches, which is what decides whether a function with
    /// internal linkage is emitted at all.
    reachable: HashSet<DeclId>,
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
        self.find_aliased();
        for index in 0..self.tast.top_level().len() {
            let decl = self.tast.top_level()[index];
            if !self.done.insert(decl) {
                continue;
            }
            match self.tast[decl].kind {
                DeclKind::Function => self.function(decl),
                DeclKind::Object => self.object(decl),
            }
        }
        for index in 0..self.aliases.len() {
            self.alias(self.aliases[index]);
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
            let pieces = match directives::assemble(&template, &mut *self.read) {
                Ok(pieces) => pieces,
                Err(directives::Failed::Unsupported(what)) => {
                    self.unsupported(&format!("{what} in an `asm` at file scope"), asm.span);
                    continue;
                }
                Err(directives::Failed::Missing(name, why)) => {
                    let message = format!("cannot open '{name}' for reading: {why}");
                    self.diagnostics.push(Diagnostic::error(message, asm.span).with_code("E0702"));
                    continue;
                }
            };
            for piece in pieces {
                self.piece(piece);
            }
        }
    }

    /// One global an `asm` at file scope defined.
    fn piece(&mut self, piece: directives::Piece) {
        let symbol = self.names.intern(&piece.name);
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

    /// The bytes of a string literal as a name, which is what a symbol in an attribute is.
    fn spelled(&self, id: StrId) -> String {
        self.tast[id].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect()
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
        // A second name for something else is not an object of its own, so nothing is laid out
        // and no image is built. It is held back until the rest of the file has been walked,
        // because what it points at may be below it.
        if node.alias.is_some() {
            self.aliases.push(decl);
            return;
        }

        let symbol = self.symbol_of(decl);
        let size = repr::size_of(self.types, self.target, ty);
        let align = alignment.unwrap_or_else(|| repr::align_of(self.types, self.target, ty));
        let mut global = Global::new(symbol, size, align);
        global.linkage = match linkage {
            Linkage::External => IrLinkage::External,
            Linkage::Internal | Linkage::None => IrLinkage::Internal,
        };
        global.visibility = self.seen(decl);
        global.tls = (duration == StorageDuration::Thread).then_some(TlsModel::GlobalDynamic);
        global.constant = repr::is_read_only(self.types, ty);
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
        let noreturn = node.noreturn;
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
        let Some(plan) = self.plan(ty, &[], span) else { return };

        let mut func = Func::new(name, plan.signature.clone());
        func.align = align;
        // The one thing a declaration says that nobody downstream can work out for themselves.
        // What `abort` does belongs to `abort`, and a translation unit that only declares it has
        // nothing to look at, so the claim has to travel on the declaration or not at all.
        if noreturn {
            func.attrs.set |= AttrSet::NORETURN;
        }
        func.linkage = match linkage {
            Linkage::Internal | Linkage::None => IrLinkage::Internal,
            Linkage::External => IrLinkage::External,
        };
        func.visibility = self.seen(decl);
        // An inline definition is not an external definition, so what goes in the module is the
        // declaration and not the body. C 6.7.4p7 says the calls in this unit go to the definition
        // some other unit holds, which is what the declaration gives them, and glibc's headers
        // rely on it: every one of their inline definitions would otherwise be a second definition
        // of a name the library already defines.
        if body.is_some() && node.inline.emits() {
            body::lower(self, decl, &mut func, &plan);
        }
        self.place_func(func);
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
        let spelled = self.names.resolve(name).to_owned();
        if name == target {
            let what = format!("'{spelled}' is aliased to itself");
            self.diagnostics.push(Diagnostic::error(what, span).with_code("E0697"));
            return;
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
            let what = format!("'{spelled}' is aliased to undefined symbol '{spelling}'");
            let note = "the target of an alias has to be defined in this same file, since an \
                        alias is a second name for an address and not a reference to one";
            let refused = Diagnostic::error(what, span).with_code("E0697");
            self.diagnostics.push(refused.note(note, span));
            return;
        }
        // Something already under this name, which is the program defining one symbol twice. The
        // definition that is there stands, the way it does for a function and for an object.
        if self.module.lookup(name).is_some() {
            return;
        }
        let mut alias = Alias::new(name, target);
        alias.linkage = match self.tast[decl].linkage {
            Linkage::Internal | Linkage::None => IrLinkage::Internal,
            Linkage::External => IrLinkage::External,
        };
        // Its own answer, because the attribute is written on the alias and an alias is a symbol
        // of its own. `weak, alias, visibility("hidden")` is a name a library keeps to itself
        // while the thing it points at stays exported, which is how glibc writes half of them.
        alias.visibility = self.seen(decl);
        self.module.add_alias(alias);
    }

    /// How far a name reaches outside a shared library, which is what a declaration of it said
    /// where one said anything and what the command line asked for where none did.
    ///
    /// gcc's `-fvisibility=` is written as the default rather than as an override, so the
    /// attribute wins wherever it was written, and that is the whole reason a library compiled
    /// with `-fvisibility=hidden` can still export the dozen names it means to export.
    ///
    /// Every symbol gets an answer, including a declaration of something defined elsewhere. That
    /// is what gcc does too and it is not a technicality: a hidden reference is one the link has
    /// to satisfy inside the library, which is the half of the flag that makes the calls cheaper
    /// rather than the half that shortens the table.
    fn seen(&self, decl: DeclId) -> IrVisibility {
        match self.tast[decl].visibility {
            Some(Visibility::Default) => IrVisibility::Default,
            Some(Visibility::Hidden) => IrVisibility::Hidden,
            Some(Visibility::Protected) => IrVisibility::Protected,
            None => self.visibility,
        }
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
        let canonical = self.types.canonical(ty);
        let canonical = match self.types.kind(canonical) {
            // A call goes through a pointer to a function, and the type in hand may be either.
            TypeKind::Pointer(pointee) => self.types.canonical(pointee),
            _ => canonical,
        };
        let TypeKind::Function(id) = self.types.kind(canonical) else {
            self.unsupported("a call through something that is not a function", span);
            return None;
        };
        let signature = self.types.signature(id);
        let ret = signature.ret;
        // A function declared without a prototype takes what it is given, which is what a
        // signature with no parameters and no end to them says. C23 removed these and this is
        // what `int f();` means in every dialect before it.
        let variadic = signature.variadic || !signature.prototyped;
        let params = if at_call && !signature.prototyped {
            // An argument past the end of the list has no parameter to travel as, which is what
            // a call to an unprototyped function with more arguments than the definition takes
            // is, so the list ends where the arguments do.
            signature
                .params
                .iter()
                .zip(actual)
                .map(|(&param, &arg)| if compatible(self.types, param, arg) { param } else { arg })
                .collect()
        } else {
            signature.params.clone()
        };

        match abi::plan(self.types, self.target, ret, &params, actual, variadic) {
            Ok(plan) => Some(plan),
            Err(what) => {
                self.unsupported(what, span);
                None
            }
        }
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
        // How much room is left in the object, which is what a string literal longer than the
        // array it initializes is cut down to. An entry that begins where the object ends is the
        // initializer of a flexible array member, and there the object grows to hold what was
        // written rather than the value being cut to fit, so nothing is taken off it.
        let room = if entry.offset < size { size - entry.offset } else { u64::MAX };
        self.datum(entry.value, room).into_iter().collect()
    }

    /// The compound literal an entry reads, if that is what the entry is.
    ///
    /// Reading an object is a node of its own, so a literal used as a value comes through as a
    /// read of a literal. A literal whose address is taken is not a read and is not this: that
    /// one folds to an address and goes in as a relocation, with the object it points at emitted
    /// on its own.
    fn literal_read(&self, value: ExprId) -> Option<DeclId> {
        let ExprKind::Convert { kind: Conversion::Lvalue, operand } = self.tast[value].kind else {
            return None;
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
            let mut mask = ones << entry.bit_offset;
            let mut placed = ((number as u128) & ones) << entry.bit_offset;
            let mut at = entry.offset;
            while mask != 0 && at < size {
                let (bits, keep) = ((placed & 0xff) as u8, !((mask & 0xff) as u8));
                let byte = bytes.entry(at).or_insert(0);
                *byte = (*byte & keep) | bits;
                mask >>= 8;
                placed >>= 8;
                at += 1;
            }
        }
        bytes
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
                };
                let addend = i64::try_from(address.offset).unwrap_or(0);
                let size = u32::try_from(size).unwrap_or(0);
                Some(Datum::Addr(self.module.add_reloc(Reloc { symbol, addend, size })))
            }
        }
    }

    /// An image of nothing but zeros, which is what a tentative definition has.
    fn zeros(&mut self, size: u64) -> DataList {
        if size == 0 {
            return DataList::EMPTY;
        }
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
        let symbol = self.names.intern(&format!(".Lstr.{}", self.strings.len()));

        let mut global = Global::new(symbol, bytes.len() as u64, align.max(1));
        global.linkage = IrLinkage::Internal;
        // Not because the type says so, since a literal is an array of `char` and not of
        // `const char`, but because writing to one is undefined and every target puts them
        // somewhere read-only.
        global.constant = true;
        let range = self.module.push_bytes(&bytes);
        global.init = Some(self.module.push_data(&[Datum::Bytes(range)]));
        self.module.add_global(global);
        self.strings.insert(id, symbol);
        symbol
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
        Some(self.names.intern(library))
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
            let spelling: String =
                tast[label].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect();
            return self.names.intern(&spelling);
        }
        if node.linkage != Linkage::None {
            let Some(name) = node.name else { return self.names.intern(".Lanon") };
            return self.library_name(name).unwrap_or(name);
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
        }
    }

    /// The value of a constant expression, reporting what folding it reported.
    fn fold(&mut self, expr: ExprId) -> Option<Const> {
        let mut eval = Eval::new(self.tast, self.types, self.target, self.names);
        let folded = eval.constant(expr);
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
