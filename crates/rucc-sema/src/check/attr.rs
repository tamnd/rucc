//! What an attribute means to a layout.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4 and `spec/12-abi-and-runtime.md` section 12.6.
//!
//! Two attributes change the size and the offsets of a record, and getting either of them wrong
//! is a miscompile rather than a missed optimization: a program that lays a structure over a
//! wire format or a hardware register is written against the layout the attribute asks for and
//! reads the wrong bytes without it. Those two are read here.
//!
//! `packed` takes the padding out. On a record it applies to every member, and on a member it
//! applies to that member alone, which is a difference the layout engine already holds.
//!
//! `aligned(n)` raises an alignment and never lowers it, which is the rule for the attribute on
//! its own. Written beside `packed` it is the way a program says both at once, as in
//! `__attribute__((packed, aligned(4)))`, and there the record is packed and then aligned to
//! four, which is not the same as either of them alone.
//!
//! `scalar_storage_order` is the third of that family and is the one that is refused. It reverses
//! the byte order of every scalar in the record, so a compiler that reads past it lays the record
//! out in the host's order and hands back every field with its bytes the wrong way round. There
//! is no harmless reading of it and no partial one, which is why it is an error here rather than
//! a warning: a program that does not build is a compiler that told the truth.
//!
//! `vector_size(n)` is the fourth one read here and is the one that builds a type rather than
//! changing a layout. It says the declared type is `n` bytes of the type that was written, taken
//! as lanes, so `int __attribute__((vector_size(16)))` is four `int` in a row that every operator
//! works on at once. A compiler that reads past it declares the lane type instead and quietly
//! computes on one lane where the program asked for all of them, which is why it is here.
//!
//! `mode(M)` is the fifth and is the other one that builds a type. It says the declared type is
//! whatever type this machine has in mode `M`, so `unsigned int __attribute__((mode(QI)))` is an
//! unsigned type one byte wide and not an `unsigned int`. A compiler that reads past it declares
//! the type as written, which is four times the size the program asked for, and a program that
//! walks such an object byte by byte walks off the end of what it meant.
//!
//! Everything else in an attribute list is left where it is. An attribute nothing implements is
//! not this module's to complain about, since the same list is written on declarations that
//! have no layout at all.

use rucc_ast::{AlignSpec, AttrArg, AttrList};
use rucc_base::float::Format;
use rucc_diag::{Diagnostic, Span};
use rucc_lex::Encoding;
use rucc_target::TargetInfo;
use rucc_types::{
    FloatKind, IntKind, TypeId, TypeKind, float_format, int_width, integer_info, is_arithmetic,
    is_complex, is_real_floating, layout,
};

use crate::check::Checker;
use crate::decl::{DeclKind, Priority, Startup, Visibility};
use crate::eval;
use crate::expr::ExprKind;
use crate::tast::StrId;

/// What the layout engine takes from an attribute list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::check) struct Packing {
    /// Whether `packed` was written.
    pub(in crate::check) packed: bool,
    /// What `aligned` asked for, in bytes, and the largest of them where it was written twice.
    pub(in crate::check) align: Option<u32>,
}

/// What `aligned` with no argument asks for, which GCC calls `BIGGEST_ALIGNMENT`.
///
/// Sixteen on every target this compiler has, which is the alignment `long double` has on
/// x86-64 and the one the vector types have on aarch64 and riscv64. It is written here rather
/// than taken from [`rucc_target::TargetInfo`] because the target table has no field for it and
/// inventing one to hold a number that is the same everywhere would be describing a difference
/// that does not exist.
const BIGGEST_ALIGNMENT: u32 = 16;

/// The attributes that keep a definition nothing in the file refers to.
///
/// Every one of them says that something outside what the compiler can see reaches the
/// definition. `used` and `retain` say so in as many words, and are what a symbol a linker script
/// names is written with. `constructor` and `destructor` are called by the run-up to `main` and
/// the run-down after it, which is code no translation unit writes. `alias` gives a second name
/// to a definition, and the name is in a string that nothing resolves as a use.
const RETAINING: [&str; 5] = ["used", "retain", "constructor", "destructor", "alias"];

/// The highest priority the implementation's own start-up code claims, which GCC calls
/// `MAX_RESERVED_INIT_PRIORITY`.
///
/// A program may still ask for one of these and gcc warns about it, which is what happens here: the
/// numbers are not reserved by anything that could enforce it, and a program that knows it has to
/// run before a library's own constructor is entitled to say so.
const RESERVED_PRIORITY: u16 = 100;

/// The real floating types a machine mode can name, in the order a mode picks between them.
///
/// The order is what makes the answer right on a target where two of these share a format.
/// `long double` is a `double` on Apple and on Windows, so `mode(DF)` has to find `double` before
/// it finds `long double`, and it is quad precision on AArch64 Linux, so `mode(TF)` has to find
/// `long double` before it finds `_Float128`. The two `_Float32x` and `_Float64x` names are left
/// out because no mode names them and reaching one from a mode would be picking the odd spelling
/// of a format for no reason.
const FLOATS: [FloatKind; 5] = [
    FloatKind::Float16,
    FloatKind::Float,
    FloatKind::Double,
    FloatKind::LongDouble,
    FloatKind::Float128,
];

/// What a machine mode names, which is a class of type and a size rather than a type.
///
/// A mode is GCC's name for the shape a value has on the machine, so a mode says how wide and
/// which register file and nothing about what C called it. Two of the three classes are settled by
/// a format rather than by a width, because a width does not tell one apart from another on every
/// target: eighty bits of x87 and a hundred and twenty eight bits of quad precision are both
/// sixteen bytes on x86-64 and are `XF` and `TF`, which are different modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// An integer that many bits wide, with the signedness of the type the attribute was on.
    Int(u32),
    /// A real floating type in that format.
    Float(Format),
    /// A complex type whose two parts are in that format.
    Complex(Format),
}

/// The mode that name names, and [`None`] where this machine has no mode by that name.
///
/// The three names that are not two letters are the ones that say what they want rather than
/// naming a width, and they are the ones a portable header writes. `word` is the machine's natural
/// register and `pointer` is an address, and on every target here those are the same size, which
/// is why they give the same answer and are still written down separately.
fn named_mode(name: &str, target: &TargetInfo) -> Option<Mode> {
    let mode = match name {
        "QI" => Mode::Int(8),
        "HI" => Mode::Int(16),
        "SI" => Mode::Int(32),
        "DI" => Mode::Int(64),
        "TI" => Mode::Int(128),
        "byte" => Mode::Int(8),
        "word" | "unwind_word" | "pointer" => Mode::Int(target.pointer_width),
        "HF" => Mode::Float(Format::Half),
        "SF" => Mode::Float(Format::Single),
        "DF" => Mode::Float(Format::Double),
        "XF" => Mode::Float(Format::X87Extended),
        "TF" => Mode::Float(Format::Quad),
        "HC" => Mode::Complex(Format::Half),
        "SC" => Mode::Complex(Format::Single),
        "DC" => Mode::Complex(Format::Double),
        "XC" => Mode::Complex(Format::X87Extended),
        "TC" => Mode::Complex(Format::Quad),
        _ => return None,
    };
    Some(mode)
}

/// Whether that name is one of GCC's vector modes, which is a `V`, a lane count and a mode.
///
/// Only used to tell one refusal from another. `V4SI` is a mode this does not build and `V4ZZ` is
/// not a mode at all, and the two deserve different sentences.
fn is_vector_mode(name: &str, target: &TargetInfo) -> bool {
    let Some(rest) = name.strip_prefix('V') else {
        return false;
    };
    let lanes = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    lanes > 0 && named_mode(&rest[lanes..], target).is_some()
}

impl Checker<'_> {
    /// The `packed` and the `aligned` in an attribute list.
    ///
    /// Both spellings are read, since `[[gnu::packed]]` and `__attribute__((packed))` are the
    /// same attribute written two ways, and `__packed__` is read as `packed` because a header
    /// writes the armoured name so that a program's own macro cannot take the plain one. An
    /// attribute in some other namespace is not GCC's and is not read.
    pub(in crate::check) fn packing(&mut self, attrs: AttrList) -> Packing {
        let mut packing = Packing::default();
        // Copied out because folding the argument of `aligned` checks an expression, which
        // borrows the checker that the tree is being read through.
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            match rucc_gnu::unarmour(self.text(attr.name)) {
                "packed" => packing.packed = true,
                "aligned" => {
                    if let Some(align) = self.aligned_argument(attr) {
                        packing.align = Some(packing.align.unwrap_or(1).max(align));
                    }
                }
                "scalar_storage_order" => {
                    let what = "'scalar_storage_order' is not implemented yet";
                    let note = "every scalar in this record would be read in the wrong byte order";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0688");
                    self.report(refused.note(note, attr.span));
                }
                _ => {}
            }
        }
        packing
    }

    /// Whether an attribute list asks for the declaration to be kept where nothing refers to it.
    ///
    /// The armour and the namespace are read the same way [`Self::packing`] reads them. What this
    /// settles is only whether the definition exists, which is the one part of each of the five
    /// that a program notices when the definition is dropped instead. What else three of them ask
    /// for is read elsewhere: `alias` by [`Self::aliased`] and the other two by
    /// [`Self::startup`]. `used` and `retain` ask for nothing else.
    pub(in crate::check) fn retains(&mut self, attrs: AttrList) -> bool {
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if RETAINING.contains(&rucc_gnu::unarmour(self.text(attr.name))) {
                return true;
            }
        }
        false
    }

    /// Whether an attribute list says the function runs without anything calling it.
    ///
    /// `constructor` puts it in the run-up to `main` and `destructor` in the run-down after `main`
    /// returns, and a function may carry both, so the two are read into a field each rather than
    /// into one answer. The armour and the namespace are read the way [`Self::packing`] reads
    /// them, and two of the same one on a declaration is the first of them, which is what a list
    /// is read as everywhere else here.
    ///
    /// Only a function can be in either order, and an attribute on anything else is dropped with
    /// the warning gcc gives it. Dropping it silently is what this compiler did until the
    /// attribute was implemented and is the thing that made it hard to find: nothing runs and
    /// nothing is said, and the symptom lands a long way from the declaration.
    pub(in crate::check) fn startup(&mut self, attrs: AttrList, kind: DeclKind) -> Startup {
        let written = self.ast[attrs].to_vec();
        let mut startup = Startup::default();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            // Copied out because reading the priority folds an expression, which borrows the
            // checker that the name was read through.
            let name = rucc_gnu::unarmour(self.text(attr.name)).to_owned();
            let before = match name.as_str() {
                "constructor" => true,
                "destructor" => false,
                _ => continue,
            };
            if kind != DeclKind::Function {
                let what = format!("'{name}' attribute ignored");
                let note = "only a function can be called before `main` or after it returns";
                let dropped = Diagnostic::warning(what, attr.span).with_code("E0703");
                self.report(dropped.note(note, attr.span));
                continue;
            }
            let Some(priority) = self.priority(attr, &name) else { continue };
            let place = if before { &mut startup.before } else { &mut startup.after };
            if place.is_none() {
                *place = Some(priority);
            }
        }
        startup
    }

    /// Where in the order one `constructor` or `destructor` asked to go.
    ///
    /// The number is GCC's and so are the two things said about it. Anything outside nought to
    /// sixty five thousand five hundred and thirty five is refused, because the number is the
    /// whole of what orders the entries and there is nothing to round it to. A number of a hundred
    /// or less is warned about and then honoured, since those are the ones the implementation's own
    /// start-up code claims and a program that takes one is asking to run before something it did
    /// not write.
    fn priority(&mut self, attr: rucc_ast::Attribute, name: &str) -> Option<Priority> {
        let args = self.ast[attr.args].to_vec();
        let range = format!("{name} priorities must be integers from 0 to 65535 inclusive");
        let asked = match args.first() {
            // Written bare, which is not the same as any number: it runs after every numbered one.
            None => return Some(Priority::Unnumbered),
            Some(AttrArg::Expr(expr)) => {
                let value = self.expr(*expr);
                match self.eval_integer(value) {
                    Ok(value) => value,
                    Err(failed) => {
                        if !failed.poisoned {
                            let at = self.tast.expr_span(failed.at);
                            self.report(Diagnostic::error(range, at).with_code("E0703"));
                        }
                        return None;
                    }
                }
            }
            // `constructor(foo)` where `foo` is not an expression, which the parser keeps as an
            // identifier because `format(printf, 1, 2)` does.
            Some(AttrArg::Ident(_)) => {
                self.report(Diagnostic::error(range, attr.span).with_code("E0703"));
                return None;
            }
        };
        let Ok(number) = u16::try_from(asked) else {
            self.report(Diagnostic::error(range, attr.span).with_code("E0703"));
            return None;
        };
        if number <= RESERVED_PRIORITY {
            let what = format!(
                "{name} priorities from 0 to {RESERVED_PRIORITY} are reserved for the \
                 implementation"
            );
            self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
        }
        Some(Priority::Numbered(number))
    }

    /// The symbol an `alias` attribute makes this declaration a second name for.
    ///
    /// What is written there is a string rather than an identifier, and that is deliberate on
    /// GCC's part: the target is the name the linker sees, so a program can alias something no
    /// declaration in the file spells and a program that renamed a name with `__asm__` aliases
    /// the renamed spelling. Nothing here resolves it, and whether anything defines it is
    /// settled where the whole translation unit is known.
    ///
    /// The armour and the namespace are read the way [`Self::packing`] reads them, since the
    /// spelling in a header is `__alias__` for the reason every spelling in a header is
    /// armoured. Two of them on one declaration is the first one, which is what a list is read
    /// as everywhere else here.
    pub(in crate::check) fn aliased(&mut self, attrs: AttrList) -> Option<StrId> {
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if rucc_gnu::unarmour(self.text(attr.name)) == "alias" {
                return self.alias_argument(attr);
            }
        }
        None
    }

    /// The string one `alias` was written with, and nothing when it was not written with one.
    fn alias_argument(&mut self, attr: rucc_ast::Attribute) -> Option<StrId> {
        let args = self.ast[attr.args].to_vec();
        let what = "'alias' requires a string naming the symbol to alias";
        let expr = match args.first() {
            Some(AttrArg::Expr(expr)) => *expr,
            // `alias` written bare, and `alias(foo)` where `foo` is not an expression, which the
            // parser keeps as an identifier because `format(printf, 1, 2)` does.
            None | Some(AttrArg::Ident(_)) => {
                self.report(Diagnostic::error(what, attr.span).with_code("E0695"));
                return None;
            }
        };
        let checked = self.expr(expr);
        let ExprKind::Str(id) = self.tast[checked].kind else {
            let at = self.tast.expr_span(checked);
            self.report(Diagnostic::error(what, at).with_code("E0695"));
            return None;
        };
        // A symbol is bytes, and a wide literal holds code units rather than bytes, so there is
        // nothing an assembler could be handed. `asm` refuses one for the same reason.
        if self.tast[id].encoding != Encoding::Plain {
            let wide = "wide string literal in 'alias'";
            self.report(Diagnostic::error(wide, attr.span).with_code("E0696"));
            return None;
        }
        Some(id)
    }

    /// How far outside a shared library an attribute list says the name reaches.
    ///
    /// `__attribute__((visibility("hidden")))` and its three other strings. The armour and the
    /// namespace are read the way [`Self::packing`] reads them, and two of them on one
    /// declaration is the first one, which is what a list is read as everywhere else here.
    ///
    /// `internal` is read as hidden. It is hidden plus a promise the program makes about never
    /// taking the address of the name across a component boundary, and nothing here derives
    /// anything from that promise, so what comes out is the same symbol with a weaker claim on
    /// it. Every program correct under the promise is correct without it, which is the one shape
    /// of ignoring an option that `spec/04-driver-and-cli.md` section 4.1 leaves room for.
    pub(in crate::check) fn seen(&mut self, attrs: AttrList) -> Option<Visibility> {
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if rucc_gnu::unarmour(self.text(attr.name)) == "visibility" {
                return self.visibility_argument(attr);
            }
        }
        None
    }

    /// The visibility one `visibility` was written with, and nothing when it was not one of the
    /// four strings the attribute takes.
    fn visibility_argument(&mut self, attr: rucc_ast::Attribute) -> Option<Visibility> {
        let args = self.ast[attr.args].to_vec();
        let what = "'visibility' requires a string, which is default, hidden, internal or \
                    protected";
        let expr = match args.first() {
            Some(AttrArg::Expr(expr)) => *expr,
            None | Some(AttrArg::Ident(_)) => {
                self.report(Diagnostic::error(what, attr.span).with_code("E0701"));
                return None;
            }
        };
        let checked = self.expr(expr);
        let ExprKind::Str(id) = self.tast[checked].kind else {
            let at = self.tast.expr_span(checked);
            self.report(Diagnostic::error(what, at).with_code("E0701"));
            return None;
        };
        if self.tast[id].encoding != Encoding::Plain {
            let wide = "wide string literal in 'visibility'";
            self.report(Diagnostic::error(wide, attr.span).with_code("E0701"));
            return None;
        }
        // The elements of a plain literal are its bytes, which is what the four spellings are
        // written in, so anything that is not one of them falls through to the message.
        let written: String =
            self.tast[id].elements.iter().filter_map(|&element| char::from_u32(element)).collect();
        match written.as_str() {
            "default" => Some(Visibility::Default),
            // Read as hidden for the reason written on [`Self::seen`], which is that the extra
            // promise it makes is one nothing here reads.
            "hidden" | "internal" => Some(Visibility::Hidden),
            "protected" => Some(Visibility::Protected),
            _ => {
                self.report(Diagnostic::error(what, attr.span).with_code("E0701"));
                None
            }
        }
    }

    /// Whether an attribute list asks for GNU's reading of `inline` rather than C's.
    ///
    /// This is the attribute glibc writes on every one of its inline definitions, through the
    /// `__extern_inline` macro, and it is why a header full of them adds nothing to an object
    /// file. The armoured spelling is the one that appears there, for the reason every armoured
    /// spelling appears in a header, and both are read the same way [`Self::packing`] reads them.
    pub(in crate::check) fn gnu_inlined(&self, attrs: AttrList) -> bool {
        self.ast[attrs].iter().any(|attr| {
            !attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                && rucc_gnu::unarmour(self.text(attr.name)) == "gnu_inline"
        })
    }

    /// Whether an attribute list says control does not come back from a call to this.
    ///
    /// `__attribute__((noreturn))` and C23's `[[noreturn]]`, which are the same claim written two
    /// ways and are read here as one. The namespace test lets `[[gnu::noreturn]]` and the bare
    /// `[[noreturn]]` both through, which is what is wanted: the first is GCC's spelling of the
    /// attribute and the second is the standard one, and no other namespace has a `noreturn` in it
    /// that would mean something else. The armoured `__noreturn__` is the spelling in a header, for
    /// the reason every armoured spelling is.
    ///
    /// `_Noreturn` is the third way of writing it and is not read here, because it is a keyword the
    /// parser already recognises and puts on the specifiers rather than an attribute in a list.
    pub(in crate::check) fn never_returns(&self, attrs: AttrList) -> bool {
        self.ast[attrs].iter().any(|attr| {
            !attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                && rucc_gnu::unarmour(self.text(attr.name)) == "noreturn"
        })
    }

    /// What an `alignas` on a member asked for, which is the same number `aligned` gives.
    ///
    /// C23 6.7.5 allows one on a member and the two spellings mean the same thing there, so this
    /// is the `_Alignas` half of the same wiring. Whether the number raises or lowers is the
    /// layout engine's to decide, and it raises.
    pub(in crate::check) fn member_alignas(
        &mut self,
        align: Option<AlignSpec>,
        span: Span,
    ) -> Option<u32> {
        let requested = match align? {
            AlignSpec::Type(named) => {
                let named = self.type_name(named);
                i128::from(layout(&self.types, named, self.cx.target).ok()?.align)
            }
            AlignSpec::Expr(expr) => {
                let value = self.expr(expr);
                match self.eval_integer(value) {
                    Ok(value) => value,
                    Err(failed) => {
                        if !failed.poisoned {
                            let at = self.tast.expr_span(failed.at);
                            let what = "requested alignment is not an integer constant";
                            self.report(Diagnostic::error(what, at).with_code("E0606"));
                        }
                        return None;
                    }
                }
            }
        };
        // C23 6.7.5p4 says `alignas(0)` has no effect, which is the one value below one that is
        // not a mistake, and it is the reason this is not the same test as the one above.
        if requested == 0 {
            return None;
        }
        if requested < 0 || requested & (requested - 1) != 0 {
            let what = format!("requested alignment '{requested}' is not a positive power of 2");
            self.report(Diagnostic::error(what, span).with_code("E0607"));
            return None;
        }
        u32::try_from(requested).ok()
    }

    /// The type an attribute list declares, which is not always the type that was written.
    ///
    /// Two attributes change that and both are read here, in the order they compose. `mode` picks
    /// a different scalar and `vector_size` makes lanes of a scalar, so a declaration carrying
    /// both wants the mode applied first and the lanes counted against what it gave. Everything
    /// that reads a declared type reads it through here, so neither of them can be missed at one
    /// of the three places a type is declared.
    pub(in crate::check) fn retyped(&mut self, ty: TypeId, attrs: AttrList) -> TypeId {
        let ty = self.moded(ty, attrs);
        self.vectorized(ty, attrs)
    }

    /// The type a `mode` in an attribute list asks for, and the type as written where there is no
    /// such attribute in it.
    ///
    /// Written twice the last one wins, which is what GCC does and is the reading under which the
    /// second is not applied to what the first produced.
    fn moded(&mut self, ty: TypeId, attrs: AttrList) -> TypeId {
        let written = self.ast[attrs].to_vec();
        let mut moded = ty;
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if rucc_gnu::unarmour(self.text(attr.name)) != "mode" {
                continue;
            }
            if let Some(made) = self.mode_of(ty, attr) {
                moded = made;
            }
        }
        moded
    }

    /// One `mode` applied to the type it was written on, and [`None`] where it named nothing this
    /// compiler has a type for.
    ///
    /// Four things are refused, and the first three are worded the way gcc 16 words them.
    ///
    /// A name that is not a mode is refused, since guessing at it would be picking a width. A
    /// mode whose class is not the written type's class is refused, because the attribute asks
    /// for a different size of the same kind of thing and there is no reading of `mode(SF)` on an
    /// integer. A mode this target has no C type for is refused, which is `XF` anywhere the x87
    /// eighty bit format is not one of the floating types.
    ///
    /// The fourth is a vector mode, `V4SI` and the like, and it is the one refusal that is not
    /// GCC's answer: GCC builds the vector and deprecates the spelling. It is refused rather than
    /// ignored because ignoring it declares one lane where the program asked for four, and the
    /// note points at `vector_size`, which is the spelling GCC's own note points at and which
    /// this compiler does build.
    fn mode_of(&mut self, ty: TypeId, attr: rucc_ast::Attribute) -> Option<TypeId> {
        let args = self.ast[attr.args].to_vec();
        // A mode written as anything but a bare name is ignored rather than refused, which is what
        // GCC does with it: `mode(1)` is an attribute it cannot read and not a type it disagrees
        // about, and the declaration around it is still the declaration that was written.
        let [AttrArg::Ident(named)] = args.as_slice() else {
            return None;
        };
        let target = self.cx.target;
        let name = rucc_gnu::unarmour(self.text(*named)).to_string();
        let Some(mode) = named_mode(&name, target) else {
            if is_vector_mode(&name, target) {
                let what = format!("vector machine mode '{name}' is not implemented yet");
                let note = "use 'vector_size' instead, which builds the same type";
                let refused = Diagnostic::error(what, attr.span).with_code("E0699");
                self.report(refused.note(note, attr.span));
                return None;
            }
            let what = format!("unknown machine mode '{name}'");
            self.report(Diagnostic::error(what, attr.span).with_code("E0698"));
            return None;
        };
        let inappropriate = format!("mode '{name}' applied to inappropriate type");
        let made = match mode {
            Mode::Int(bits) => {
                let Some(shape) = integer_info(&self.types, ty, target) else {
                    self.report(Diagnostic::error(inappropriate, attr.span).with_code("E0698"));
                    return None;
                };
                // `char` is skipped because GCC's answer for a one byte mode is `signed char` or
                // `unsigned char`, and `char` is a third type distinct from both of them.
                let kind = IntKind::ALL.into_iter().find(|&kind| {
                    kind != IntKind::Char
                        && int_width(kind, target) == bits
                        && kind.is_signed(target.char_is_signed) == shape.signed
                })?;
                self.types.int(kind)
            }
            Mode::Float(format) => {
                if !is_real_floating(&self.types, ty) {
                    self.report(Diagnostic::error(inappropriate, attr.span).with_code("E0698"));
                    return None;
                }
                let kind = self.float_in(format, &name, attr.span)?;
                self.types.float(kind)
            }
            Mode::Complex(format) => {
                if !is_complex(&self.types, ty) {
                    self.report(Diagnostic::error(inappropriate, attr.span).with_code("E0698"));
                    return None;
                }
                let kind = self.float_in(format, &name, attr.span)?;
                self.types.complex(kind)
            }
        };
        Some(made)
    }

    /// The floating type this target has in that format, and the refusal where it has none.
    ///
    /// Every target has a single and a double, so the one this turns down in practice is `XF` on
    /// a machine without an x87, where GCC says the same thing.
    fn float_in(&mut self, format: Format, name: &str, span: Span) -> Option<FloatKind> {
        let target = self.cx.target;
        let found = FLOATS.into_iter().find(|&kind| float_format(kind, target) == format);
        if found.is_none() {
            let what = format!("no data type for mode '{name}'");
            self.report(Diagnostic::error(what, span).with_code("E0698"));
        }
        found
    }

    /// The type a `vector_size` in an attribute list asks for, and the type as written where
    /// there is no such attribute in it.
    ///
    /// This is read apart from [`Self::packing`] because it does not change a layout, it changes
    /// which type the declaration declares. An `int` with `vector_size(16)` written on it is not
    /// an `int` laid out differently, it is a vector of four of them, and every rule about what
    /// may be done to it reads that rather than the layout.
    ///
    /// The attribute gives a total size in bytes and not a lane count, so the lanes are that size
    /// over the size of the element. Written twice the last one wins, which is what GCC does and
    /// is the only reading under which the two are not both applied to the same element type.
    pub(in crate::check) fn vectorized(&mut self, ty: TypeId, attrs: AttrList) -> TypeId {
        let written = self.ast[attrs].to_vec();
        let mut vector = ty;
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if rucc_gnu::unarmour(self.text(attr.name)) != "vector_size" {
                continue;
            }
            if let Some(made) = self.vector_of(ty, attr) {
                vector = made;
            }
        }
        vector
    }

    /// One `vector_size` applied to the type it was written on, and [`None`] where it was
    /// written in a way that has no vector in it.
    ///
    /// Five things are refused, and each of them is worded the way gcc 16 words it, because these
    /// are messages a configure script reads and a header falls back on.
    ///
    /// An argument that is not one integer constant is refused because the lane count has to be
    /// known to lay the object out at all. A size of zero is refused because a vector of no lanes
    /// is not a type. A size that is not a whole number of lanes is refused because the leftover
    /// bytes belong to nothing. A lane count that is not a power of two is refused because no
    /// machine has such a register and gcc turns one down as well. And a lane type that is not
    /// arithmetic is refused, which is where this is narrower than gcc: gcc takes a vector of
    /// pointers and this does not have one yet.
    fn vector_of(&mut self, elem: TypeId, attr: rucc_ast::Attribute) -> Option<TypeId> {
        let args = self.ast[attr.args].to_vec();
        let bytes = match args.as_slice() {
            [AttrArg::Expr(expr)] => {
                let value = self.expr(*expr);
                match self.eval_integer(value) {
                    Ok(value) => value,
                    Err(failed) => {
                        if !failed.poisoned {
                            let at = self.tast.expr_span(failed.at);
                            let what =
                                "'vector_size' attribute argument is not an integer constant";
                            self.report(Diagnostic::error(what, at).with_code("E0689"));
                        }
                        return None;
                    }
                }
            }
            // `vector_size(foo)` where `foo` is not an expression, which the parser keeps as an
            // identifier for the same reason `aligned` does, and the two counts either side of
            // one, which gcc words the same way as each other.
            _ => {
                let what = "wrong number of arguments specified for 'vector_size' attribute";
                self.report(Diagnostic::error(what, attr.span).with_code("E0689"));
                return None;
            }
        };
        if bytes < 0 {
            let what = format!("'vector_size' attribute argument value '{bytes}' is negative");
            self.report(Diagnostic::error(what, attr.span).with_code("E0689"));
            return None;
        }
        let canonical = self.types.canonical(elem);
        let boolean = matches!(eval::bare(&self.types, elem), TypeKind::Bool);
        if !is_arithmetic(&self.types, canonical) || boolean {
            let what = "invalid vector type for attribute 'vector_size'";
            let note = "a lane is one of the arithmetic types, and is not a bool";
            let refused = Diagnostic::error(what, attr.span).with_code("E0690");
            self.report(refused.note(note, attr.span));
            return None;
        }
        let size = layout(&self.types, elem, self.cx.target).ok()?.size;
        let bytes = u64::try_from(bytes).ok()?;
        if bytes == 0 {
            self.report(Diagnostic::error("zero vector size", attr.span).with_code("E0690"));
            return None;
        }
        if size == 0 || bytes % size != 0 {
            let what = "vector size not an integral multiple of component size";
            let note = format!("one lane is '{size}' bytes, and every lane has to fit");
            let refused = Diagnostic::error(what, attr.span).with_code("E0690");
            self.report(refused.note(note, attr.span));
            return None;
        }
        let lanes = u32::try_from(bytes / size).ok()?;
        if !lanes.is_power_of_two() {
            let what = format!("number of vector components {lanes} not a power of two");
            let refused = Diagnostic::error(what, attr.span).with_code("E0690");
            self.report(refused.note("no machine has such a register", attr.span));
            return None;
        }
        Some(self.types.vector(elem, lanes))
    }

    /// What one `aligned` asked for, which is a number or nothing when it was written bare.
    fn aligned_argument(&mut self, attr: rucc_ast::Attribute) -> Option<u32> {
        let args = self.ast[attr.args].to_vec();
        let requested = match args.first() {
            None => return Some(BIGGEST_ALIGNMENT),
            Some(AttrArg::Expr(expr)) => {
                let value = self.expr(*expr);
                match self.eval_integer(value) {
                    Ok(value) => value,
                    Err(failed) => {
                        if !failed.poisoned {
                            let at = self.tast.expr_span(failed.at);
                            let what = "requested alignment is not an integer constant";
                            self.report(Diagnostic::error(what, at).with_code("E0606"));
                        }
                        return None;
                    }
                }
            }
            // `aligned(foo)` where `foo` is not an expression, which nothing writes and which
            // the parser keeps as an identifier because `format(printf, 1, 2)` does.
            Some(AttrArg::Ident(_)) => {
                let what = "requested alignment is not an integer constant";
                self.report(Diagnostic::error(what, attr.span).with_code("E0606"));
                return None;
            }
        };
        if requested <= 0 || requested & (requested - 1) != 0 {
            let what = format!("requested alignment '{requested}' is not a positive power of 2");
            self.report(Diagnostic::error(what, attr.span).with_code("E0607"));
            return None;
        }
        u32::try_from(requested).ok()
    }
}
