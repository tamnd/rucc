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
//! `scalar_storage_order` is the third of that family and is the one that changes no offset at
//! all. It says every scalar in the record is stored in the byte order it names, so on a target
//! whose order is the other one every load through a member swaps and so does every store, and a
//! bit-field is allocated from the other end of its storage unit. A compiler that reads past it
//! lays the record out the same way and hands back every field with its bytes the wrong way round,
//! which is why the attribute is read here and only the order is taken from it.
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

use rucc_ast::{AlignSpec, AttrArg, AttrList, Attribute};
use rucc_base::float::Format;
use rucc_diag::{Diagnostic, Span};
use rucc_gnu::{Kind, Status};
use rucc_lex::Encoding;
use rucc_target::{BitFieldStyle, Convention, Isa, Target, TargetInfo};
use rucc_types::{
    FloatKind, FunctionId, FunctionType, IntKind, TypeId, TypeKind, float_format, int_width,
    integer_info, is_arithmetic, is_complex, is_real_floating, layout,
};

use crate::check::Checker;
use crate::decl::{
    DeclFlags, DeclId, DeclKind, Effects, Priority, Startup, StorageDuration, Visibility,
};
use crate::eval;
use crate::expr::ExprKind;
use crate::scope::Binding;
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

/// The format archetypes gcc 16 knows on every target, which is what `format(archetype, ...)` may
/// name without a warning.
///
/// The four plain names are the same as their `gnu_` names. The ones for gcc's own diagnostics
/// and `asm_fprintf` are there because gcc knows them, and `NSString` because gcc knows it in C as
/// well as in Objective-C.
const FORMAT_ARCHETYPES: [&str; 16] = [
    "printf",
    "scanf",
    "strftime",
    "strfmon",
    "gnu_printf",
    "gnu_scanf",
    "gnu_strftime",
    "gnu_strfmon",
    "asm_fprintf",
    "gcc_diag",
    "gcc_tdiag",
    "gcc_cdiag",
    "gcc_cxxdiag",
    "gcc_gfc",
    "gcc_dump_printf",
    "NSString",
];

/// The archetypes gcc knows on Windows as well, which are the conversions msvcrt accepts.
///
/// There is no `ms_strfmon`, because Windows has no `strfmon`, and mingw-w64 gcc warns about one.
const WINDOWS_ARCHETYPES: [&str; 3] = ["ms_printf", "ms_scanf", "ms_strftime"];

/// The archetypes clang knows beyond gcc's, which a Darwin target takes because clang is the
/// reference there.
const DARWIN_ARCHETYPES: [&str; 10] = [
    "CFString",
    "printf0",
    "syslog",
    "kprintf",
    "cmn_err",
    "vcmn_err",
    "zcmn_err",
    "freebsd_kprintf",
    "os_trace",
    "os_log",
];

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
    /// The name an attribute has in `crates/rucc-gnu/features.toml`, or an empty string when the
    /// checker does not read it.
    ///
    /// Every attribute the checker gives a meaning to is named through here, so what the checker
    /// acts on and what `__has_attribute` answers come out of the same table. A row marked
    /// implemented or partial is read. A row marked unimplemented or rejected, and a name the
    /// table has no row for, come back empty and match nothing, which is what the preprocessor
    /// answering no has already told the program. `__packed__` is read as `packed`, and an
    /// attribute in some namespace other than `gnu` is not GCC's and is not read.
    fn gnu_name(&self, attr: &Attribute) -> &'static str {
        if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
            return "";
        }
        match rucc_gnu::lookup(Kind::Attribute, self.text(attr.name)) {
            Some(row) if matches!(row.status, Status::Implemented | Status::Partial) => row.name,
            _ => "",
        }
    }

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
            match self.gnu_name(&attr) {
                "packed" => packing.packed = true,
                "aligned" => {
                    if let Some(align) = self.aligned_argument(attr) {
                        packing.align = Some(packing.align.unwrap_or(1).max(align));
                    }
                }
                _ => {}
            }
        }
        packing
    }

    /// Where `transparent_union` was written in an attribute list, if it was.
    ///
    /// The span comes back rather than a flag because whether the attribute holds up is decided by
    /// the members of the union it is on, and the sentence about a union it cannot hold up on has
    /// to point at the attribute. The armour and the namespace are read the way [`Self::packing`]
    /// reads them, and glibc writes the armoured spelling.
    pub(in crate::check) fn transparent_union(&self, attrs: AttrList) -> Option<Span> {
        self.ast[attrs].iter().find_map(|attr| {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                return None;
            }
            (self.gnu_name(attr) == "transparent_union").then_some(attr.span)
        })
    }

    /// Whether `may_alias` was written in an attribute list.
    ///
    /// It is read on a typedef and on a record, which are the two places it says something about
    /// a type, and the alias analysis is what listens: an access through the type carries the
    /// character type's node, so nothing is reordered across it on the strength of the type it
    /// was written with. That is what `<emmintrin.h>` and every `get_unaligned` rely on, and
    /// ignoring it is wrong code under `-fstrict-aliasing` rather than slow code.
    pub(in crate::check) fn may_alias(&self, attrs: AttrList) -> bool {
        self.ast[attrs].iter().any(|attr| self.gnu_name(attr) == "may_alias")
    }

    /// The byte order a `scalar_storage_order` in an attribute list asked for.
    ///
    /// True for `"big-endian"` and false for `"little-endian"`, and nothing at all when the
    /// attribute was not written or when what it was written with is not one of those two words.
    /// Whether the answer means anything is a question about the target and is asked where the
    /// record is completed, since asking for the order the target already has is a program saying
    /// what would have happened anyway.
    ///
    /// The armour and the namespace are read the way [`Self::packing`] reads them. Only a record is
    /// ever asked, so an attribute written anywhere else is ignored rather than diagnosed, which is
    /// what this compiler does with every attribute it has no use for in that position.
    pub(in crate::check) fn storage_order(&mut self, attrs: AttrList) -> Option<bool> {
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if self.gnu_name(&attr) == "scalar_storage_order" {
                return self.storage_order_argument(attr);
            }
        }
        None
    }

    /// The order one `scalar_storage_order` names, reporting an argument that names neither.
    ///
    /// gcc's wording, down to the quotes around the two words, because the attribute exists for
    /// programs that read a wire format and one of those would rather be told the spelling it got
    /// wrong than be handed a record laid out in the order it did not ask for.
    fn storage_order_argument(&mut self, attr: Attribute) -> Option<bool> {
        let args = self.ast[attr.args].to_vec();
        let what = "'scalar_storage_order' argument must be one of \"big-endian\" or \
                    \"little-endian\"";
        let expr = match args.first() {
            Some(AttrArg::Expr(expr)) => *expr,
            None | Some(AttrArg::Ident(_)) => {
                self.report(Diagnostic::error(what, attr.span).with_code("E0688"));
                return None;
            }
        };
        let checked = self.expr(expr);
        let ExprKind::Str(id) = self.tast[checked].kind else {
            let at = self.tast.expr_span(checked);
            self.report(Diagnostic::error(what, at).with_code("E0688"));
            return None;
        };
        let literal = &self.tast[id];
        let spelled: Option<String> = (literal.encoding == Encoding::Plain)
            .then(|| literal.elements.iter().map(|&unit| char::from(unit as u8)).collect());
        match spelled.as_deref() {
            Some("big-endian") => Some(true),
            Some("little-endian") => Some(false),
            _ => {
                self.report(Diagnostic::error(what, attr.span).with_code("E0688"));
                None
            }
        }
    }

    /// The bit-field rule `ms_struct` or `gcc_struct` asked a record to be laid out by.
    ///
    /// `ms_struct` is Microsoft's rule and `gcc_struct` is the Itanium one, which gcc calls its
    /// own. Asking for the rule the target already has is allowed and changes nothing, so the
    /// answer is the rule either way and the layout engine decides whether it is a change.
    ///
    /// Only x86 and Windows targets read them. gcc takes the pair on x86 alone, where it honours
    /// them on Linux as well as on mingw, and warns that they are ignored everywhere else. The
    /// Windows targets on other architectures are here because clang is their reference and
    /// clang takes `ms_struct` on every target. Elsewhere the attribute is left alone, which is
    /// what this compiler does with every attribute it has no use for.
    ///
    /// The first one written wins and a later one of the other kind is dropped with gcc's warning,
    /// so `__attribute__((gcc_struct, ms_struct))` is `gcc_struct`. Measured with gcc 16 and with
    /// mingw-w64 gcc, which agree. The armour and the namespace are read the way
    /// [`Self::packing`] reads them.
    pub(in crate::check) fn bit_field_style(&mut self, attrs: AttrList) -> Option<BitFieldStyle> {
        let tuple = self.cx.target.tuple;
        if !matches!(tuple.arch().as_str(), "x86_64" | "i686") && tuple.os().as_str() != "windows" {
            return None;
        }
        let mut chosen = None;
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            let name = self.gnu_name(&attr);
            let style = match name {
                "ms_struct" => BitFieldStyle::Microsoft,
                "gcc_struct" => BitFieldStyle::Itanium,
                _ => continue,
            };
            match chosen {
                None => chosen = Some(style),
                Some(first) if first == style => {}
                Some(_) => {
                    let what = format!("'{name}' incompatible attribute ignored");
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0746"));
                }
            }
        }
        chosen
    }

    /// Checks the archetype each `format` attribute in a list names, and says so where gcc would
    /// not know it.
    ///
    /// The archetype is the first argument, the `printf` of `format(printf, 1, 2)`, and it names
    /// the language the format string is written in. gcc knows a set of them that depends on the
    /// target, and a name outside that set gets its warning and nothing else. The set is what
    /// matters here: mingw-w64's headers write `__MINGW_PRINTF_FORMAT`, which is `ms_printf` or
    /// `gnu_printf` depending on whether the program asked for the C99 `printf`, and the same for
    /// `scanf` and `strftime`, so a Windows program that includes `<stdio.h>` has these on every
    /// declaration in it and must not be warned about any of them.
    ///
    /// The `ms_` names are the Windows ones, and are the conversions msvcrt and the UCRT accept,
    /// such as `%I64d`. The `gnu_` names are the C99 set with GNU's additions, and `printf`,
    /// `scanf`, `strftime` and `strfmon` are the same as their `gnu_` names on every target. gcc
    /// on Linux does not know the `ms_` names and warns about them, which is what happens here as
    /// well. The Darwin targets take clang's names as well as gcc's, because clang is the
    /// reference there and Apple's headers write `os_log` and `CFString`.
    ///
    /// Nothing reads the format strings yet (#485 is where a warning group for them would go), so
    /// an archetype this accepts is checked no further, and that is the same for all of them.
    pub(in crate::check) fn format_archetypes(&mut self, attrs: AttrList) {
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if self.gnu_name(&attr) != "format" {
                continue;
            }
            // `format(printf, 1, 2)` keeps its first argument as an identifier. Anything else in
            // that place is not an archetype and is not this function's to judge.
            let Some(&AttrArg::Ident(archetype)) = self.ast[attr.args].first() else {
                continue;
            };
            let name = rucc_gnu::unarmour(self.text(archetype)).to_owned();
            if !self.knows_archetype(&name) {
                let what = format!("'{name}' is an unrecognized format function type");
                self.report(Diagnostic::warning(what, attr.span).with_code("E0747"));
            }
        }
    }

    /// Whether `name` is a format archetype gcc knows on this target, or clang on a Darwin one.
    fn knows_archetype(&self, name: &str) -> bool {
        let tuple = self.cx.target.tuple;
        if FORMAT_ARCHETYPES.contains(&name) {
            return true;
        }
        match tuple.os().as_str() {
            "windows" => WINDOWS_ARCHETYPES.contains(&name),
            _ if tuple.os().is_darwin() => DARWIN_ARCHETYPES.contains(&name),
            _ => false,
        }
    }

    /// Where `weak` was written in an attribute list, if it was.
    ///
    /// The span comes back rather than a flag for the reason [`Self::transparent_union`]'s does:
    /// the attribute is refused on a name the linker never sees, the way gcc refuses it, and the
    /// sentence about that has to point at the attribute rather than at the whole declaration.
    /// The armour and the namespace are read the way [`Self::packing`] reads them, and a library
    /// offering a hook writes the armoured spelling, since the header is read by everybody.
    ///
    /// This is a fact about the name and not about one declaration of it, which is why it is kept
    /// where `visibility` is kept and merged the same way. A header saying it once is enough, and
    /// the definition below it that says nothing is still weak.
    ///
    /// Microsoft's `selectany`, which `__declspec` hands on under that name, is read as the same
    /// thing. It asks for one copy of a definition that several objects each have, and the
    /// universal CRT's <wchar.h> defines a variable that way which libucrt.lib defines as well. A
    /// weak definition is what gives that here: a link with several keeps one, and a link with a
    /// strong one keeps that.
    pub(in crate::check) fn weakened(&self, attrs: AttrList) -> Option<Span> {
        self.ast[attrs].iter().find_map(|attr| {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                return None;
            }
            matches!(self.gnu_name(attr), "weak" | "selectany").then_some(attr.span)
        })
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
            if RETAINING.contains(&self.gnu_name(&attr)) {
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
            let name = self.gnu_name(&attr).to_owned();
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
    fn priority(&mut self, attr: Attribute, name: &str) -> Option<Priority> {
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
            if self.gnu_name(&attr) == "alias" {
                return self.alias_argument(attr);
            }
        }
        None
    }

    /// The string one `alias` was written with, and nothing when it was not written with one.
    fn alias_argument(&mut self, attr: Attribute) -> Option<StrId> {
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

    /// The section a `section` attribute asks for the definition to go in, with where it was
    /// written.
    ///
    /// The armour and the namespace are read the way [`Self::packing`] reads them, and two of them
    /// on one declaration is the first one. An object inside a block with no `static` is a slot in
    /// the frame rather than something the linker places, and gcc refuses the attribute there
    /// with the same sentence this does.
    pub(in crate::check) fn sectioned(
        &mut self,
        attrs: AttrList,
        duration: StorageDuration,
    ) -> Option<(StrId, Span)> {
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if rucc_gnu::unarmour(self.text(attr.name)) != "section" {
                continue;
            }
            if duration == StorageDuration::Automatic {
                let what = "section attribute cannot be specified for local variables";
                self.report(Diagnostic::error(what, attr.span).with_code("E0750"));
                return None;
            }
            return self.section_argument(attr).map(|id| (id, attr.span));
        }
        None
    }

    /// The string one `section` was written with, and nothing when it was not written with one.
    fn section_argument(&mut self, attr: Attribute) -> Option<StrId> {
        let args = self.ast[attr.args].to_vec();
        let expr = match args.as_slice() {
            [AttrArg::Expr(expr)] => *expr,
            [AttrArg::Ident(_)] => {
                let what = "section attribute argument not a string constant";
                self.report(Diagnostic::error(what, attr.span).with_code("E0750"));
                return None;
            }
            _ => {
                let what = format!(
                    "wrong number of arguments specified for 'section' attribute, which takes one \
                     and was given {}",
                    args.len()
                );
                self.report(Diagnostic::error(what, attr.span).with_code("E0750"));
                return None;
            }
        };
        let checked = self.expr(expr);
        let what = "section attribute argument not a string constant";
        let ExprKind::Str(id) = self.tast[checked].kind else {
            let at = self.tast.expr_span(checked);
            self.report(Diagnostic::error(what, at).with_code("E0750"));
            return None;
        };
        // A section name is bytes in the object file's string table, and a wide literal holds code
        // units rather than bytes, which is the reason `alias` refuses one.
        if self.tast[id].encoding != Encoding::Plain {
            self.report(Diagnostic::error(what, attr.span).with_code("E0750"));
            return None;
        }
        if self.tast[id].elements.is_empty() {
            let empty = "a section attribute naming no section";
            self.report(Diagnostic::error(empty, attr.span).with_code("E0750"));
            return None;
        }
        Some(id)
    }

    /// Keeps the section a declaration named, or warns when an earlier declaration of the same
    /// name already named a different one.
    ///
    /// The first one stands, which is gcc's answer and its warning: whatever the file has already
    /// been told about where the name lives is what a later declaration would be contradicting.
    pub(in crate::check) fn record_section(&mut self, decl: DeclId, asked: Option<(StrId, Span)>) {
        let Some((id, span)) = asked else { return };
        let spell = |checker: &Self, id: StrId| -> String {
            checker.tast[id].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect()
        };
        if let Some(before) = self.tast.section(decl) {
            let (was, now) = (spell(self, before), spell(self, id));
            if was != now {
                let what = format!(
                    "ignoring attribute 'section (\"{now}\")' because it conflicts with previous \
                     'section (\"{was}\")'"
                );
                let at = self.tast.decl_span(decl);
                self.report(
                    Diagnostic::warning(what, span)
                        .with_code("E0750")
                        .note("previous declaration here", at),
                );
            }
            return;
        }
        self.tast.record_section(decl, id);
    }

    /// Reads `error("...")` and `warning("...")` off the lists a declaration was written with and
    /// keeps the message against the declaration, for a call that is still there once the
    /// optimizer has run.
    ///
    /// Nothing is said about a call here. The kernel's `compiletime_assert` declares a function
    /// carrying one and calls it under a condition that is only settled after inlining, and the
    /// FORTIFY checks do the same, so a call written in the source is not yet a call the program
    /// makes. The driver asks once the optimizer is done, and a call it took out is not reported.
    ///
    /// What is refused is what gcc refuses, in its words: the attribute on anything but a function
    /// is dropped with a warning, and so is one whose argument is not a string, while the wrong
    /// number of arguments is an error. The lists are read in the order given, so a message written
    /// later replaces one written earlier, the way a later declaration's does.
    pub(in crate::check) fn record_notices(
        &mut self,
        decl: DeclId,
        lists: &[AttrList],
        kind: DeclKind,
    ) {
        for &attrs in lists {
            let written = self.ast[attrs].to_vec();
            for attr in written {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                    continue;
                }
                let error = match rucc_gnu::unarmour(self.text(attr.name)) {
                    "error" => true,
                    "warning" => false,
                    _ => continue,
                };
                let name = if error { "error" } else { "warning" };
                let ignored = format!("'{name}' attribute ignored");
                let args = self.ast[attr.args].to_vec();
                if args.len() != 1 {
                    let what =
                        format!("wrong number of arguments specified for '{name}' attribute");
                    let note = format!("expected 1, found {}", args.len());
                    let refused = Diagnostic::error(what, attr.span).with_code("E0751");
                    self.report(refused.note(note, attr.span));
                    continue;
                }
                if kind != DeclKind::Function {
                    let what = "only a call to a function can be reported";
                    let dropped = Diagnostic::warning(ignored, attr.span).with_code("E0751");
                    self.report(dropped.note(what, attr.span));
                    continue;
                }
                let message = match args[0] {
                    AttrArg::Expr(expr) => {
                        let checked = self.expr(expr);
                        match self.tast[checked].kind {
                            ExprKind::Str(id) if self.tast[id].encoding == Encoding::Plain => {
                                Some(id)
                            }
                            _ => None,
                        }
                    }
                    AttrArg::Ident(_) => None,
                };
                let Some(message) = message else {
                    let what = "its argument has to be a string literal";
                    let dropped = Diagnostic::warning(ignored, attr.span).with_code("E0751");
                    self.report(dropped.note(what, attr.span));
                    continue;
                };
                self.tast.record_notice(decl, error, message);
            }
        }
    }

    /// The function a `cleanup` attribute asks to have called when the object goes out of scope.
    ///
    /// This is what glib writes as `g_autoptr`, systemd as `_cleanup_free_` and jansson as
    /// `json_auto_t`, and a compiler that reads past it leaks whatever the handler would have
    /// given back. The armour and the namespace are read the way [`Self::packing`] reads them,
    /// and two of them on one declaration is the first one, which is what a list is read as
    /// everywhere else here.
    ///
    /// The argument is an identifier rather than a string, unlike `alias` above, and it names a
    /// function that is looked up here in the scope the declaration is in. What it resolves to is
    /// kept on the declaration rather than on the name, because only an object inside a block can
    /// carry the attribute and such an object is declared once.
    pub(in crate::check) fn cleanup(
        &mut self,
        attrs: AttrList,
        kind: DeclKind,
        duration: StorageDuration,
    ) -> Option<DeclId> {
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if self.gnu_name(&attr) == "cleanup" {
                return self.cleanup_argument(attr, kind, duration);
            }
        }
        None
    }

    /// The handler one `cleanup` named, and nothing when there is no point in calling it or when
    /// what was named is not something that can be called.
    fn cleanup_argument(
        &mut self,
        attr: Attribute,
        kind: DeclKind,
        duration: StorageDuration,
    ) -> Option<DeclId> {
        let args = self.ast[attr.args].to_vec();
        let what = "'cleanup' requires the name of a function to call";
        // `cleanup(free)` is a lone identifier, which the parser keeps as one because
        // `format(printf, 1, 2)` does, so an expression here is something else entirely.
        let Some(&AttrArg::Ident(name)) = args.first() else {
            self.report(Diagnostic::error(what, attr.span).with_code("E0706"));
            return None;
        };
        // Only an object that lives in a block has a scope to leave, and there is nowhere to put
        // the call for anything else, so the attribute is dropped with the warning gcc drops it
        // with. Saying nothing is what makes this shape of wrongness hard to find.
        if kind != DeclKind::Object || duration != StorageDuration::Automatic {
            let only = "'cleanup' is only for an object with automatic storage duration";
            self.report(Diagnostic::warning(only, attr.span).with_code("E0707"));
            return None;
        }
        let Some(Binding::Decl(handler)) = self.scopes.lookup(name) else {
            let undeclared = format!("'{}' in 'cleanup' does not name anything", self.text(name));
            self.report(Diagnostic::error(undeclared, attr.span).with_code("E0706"));
            return None;
        };
        if self.tast[handler].kind != DeclKind::Function {
            let not = format!("'{}' in 'cleanup' is not a function", self.text(name));
            self.report(Diagnostic::error(not, attr.span).with_code("E0706"));
            return None;
        }
        // The handler is called with the address of the object and with nothing else, so one
        // parameter that is a pointer is the only shape there is anything to call. A declaration
        // with no prototype says nothing about what it takes and so says nothing about how the
        // address travels either, which leaves the call with nothing to be built from.
        //
        // Whether that pointer is to the object's own type is not asked. gcc warns where the two
        // disagree and calls it anyway, and a handler written for `void *` is the ordinary case,
        // so what the declaration wrote is what the address is passed as.
        let declared = self.types.canonical(self.tast[handler].ty);
        let ok = match self.types.kind(declared) {
            TypeKind::Function(id) => {
                let signature = self.types.signature(id);
                let first = signature.params.first().copied();
                signature.prototyped
                    && signature.params.len() == 1
                    && first.is_some_and(|param| {
                        matches!(self.types.kind(self.types.canonical(param)), TypeKind::Pointer(_))
                    })
            }
            _ => false,
        };
        if !ok {
            let one = format!(
                "'{}' in 'cleanup' takes one argument, which is the address of the object",
                self.text(name)
            );
            self.report(Diagnostic::error(one, attr.span).with_code("E0706"));
            return None;
        }
        Some(handler)
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
            if self.gnu_name(&attr) == "visibility" {
                return self.visibility_argument(attr);
            }
        }
        None
    }

    /// The visibility one `visibility` was written with, and nothing when it was not one of the
    /// four strings the attribute takes.
    fn visibility_argument(&mut self, attr: Attribute) -> Option<Visibility> {
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
                && self.gnu_name(attr) == "gnu_inline"
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
                && self.gnu_name(attr) == "noreturn"
        })
    }

    /// Whether an attribute list says the function is written without a prologue or an epilogue.
    ///
    /// `__attribute__((naked))`, under the namespace test [`Self::never_returns`] is under and
    /// through the same unarmouring, so `__naked__` in a header and `[[gnu::naked]]` are both read.
    /// There is no standard spelling of this one, so unlike `noreturn` the bare form is gcc's as
    /// well.
    pub(in crate::check) fn is_naked(&self, attrs: AttrList) -> bool {
        self.ast[attrs].iter().any(|attr| {
            !attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                && self.gnu_name(attr) == "naked"
        })
    }

    /// Whether an attribute list says a call to this function may come back more than once.
    ///
    /// `__attribute__((returns_twice))`, under the namespace test [`Self::never_returns`] is under
    /// and through the same unarmouring. glibc writes it on `setjmp`, `vfork` and their relatives
    /// as `__returns_twice__`, and Postgres checks for it with `__has_attribute` before it trusts
    /// its own `sigsetjmp` wrappers.
    pub(in crate::check) fn returns_twice(&self, attrs: AttrList) -> bool {
        self.ast[attrs].iter().any(|attr| {
            !attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                && self.gnu_name(attr) == "returns_twice"
        })
    }

    /// What an attribute list says about inlining and about what is written around the body, as
    /// the bits it can set.
    ///
    /// `always_inline`, `noinline`, `no_instrument_function`, `no_stack_protector`,
    /// `stack_protect`, `cold`, `hot`, `function_return("keep")` and `indirect_branch("keep")`, and
    /// the `optimize` options that stand for two of them, under the namespace test
    /// [`Self::never_returns`] is under and through the same
    /// unarmouring, so `__always_inline__` in a header and `[[gnu::noinline]]` are both read.
    /// Nothing else in the list is looked at, so the answer is [`DeclFlags::NONE`] for almost every
    /// declaration.
    pub(in crate::check) fn inlining(&mut self, attrs: AttrList) -> DeclFlags {
        let mut flags = DeclFlags::NONE;
        let ast = self.ast;
        for &attr in &ast[attrs] {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            let name = self.gnu_name(&attr).to_string();
            match name.as_str() {
                "always_inline" => flags |= DeclFlags::ALWAYS_INLINE,
                "noinline" => flags |= DeclFlags::NOINLINE,
                "no_instrument_function" => flags |= DeclFlags::NO_INSTRUMENT,
                "no_stack_protector" => flags = flags.then(DeclFlags::NO_STACK_PROTECTOR),
                "stack_protect" => flags = flags.then(DeclFlags::STACK_PROTECT),
                "cold" => flags |= DeclFlags::COLD,
                "hot" => flags |= DeclFlags::HOT,
                // Only `keep` changes anything. The other values ask for a thunk on a function
                // the command line left alone, which the kernel never writes.
                "function_return" if self.optimize_options(attr) == ["keep"] => {
                    flags |= DeclFlags::RETURN_KEEP;
                }
                "indirect_branch" if self.optimize_options(attr) == ["keep"] => {
                    flags |= DeclFlags::INDIRECT_KEEP;
                }
                // An option is written with or without its `-f`. `no-stack-protector` here is
                // what the kernel's `__nostackprotector` was before gcc 11 had the attribute, and
                // what it still falls back to on a compiler that says it does not have it.
                "optimize" => {
                    for option in self.optimize_options(attr) {
                        match option.trim_start_matches('-').trim_start_matches('f') {
                            "no-strict-aliasing" => flags |= DeclFlags::NO_STRICT_ALIASING,
                            "no-stack-protector" => {
                                flags = flags.then(DeclFlags::NO_STACK_PROTECTOR);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        flags
    }

    /// What an attribute list says about which DLL the name is in, as [`DeclFlags::DLLIMPORT`] and
    /// [`DeclFlags::DLLEXPORT`].
    ///
    /// `__declspec(dllimport)` is a macro for `__attribute__((dllimport))` in every Windows header,
    /// so the one spelling is all there is to read, under the namespace test and the unarmouring
    /// [`Self::inlining`] uses. It is read on every target, as gcc and clang read it, and only the
    /// code written for a COFF object does anything with it.
    pub(in crate::check) fn dll(&self, attrs: AttrList) -> DeclFlags {
        let mut flags = DeclFlags::NONE;
        for attr in &self.ast[attrs] {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            match self.gnu_name(attr) {
                "dllimport" => flags |= DeclFlags::DLLIMPORT,
                "dllexport" => flags |= DeclFlags::DLLEXPORT,
                _ => {}
            }
        }
        flags
    }

    /// The options an `optimize` attribute names, one for each string and each comma in one.
    ///
    /// A number, such as `optimize (2)`, is a level and names no option, so it gives nothing.
    fn optimize_options(&mut self, attr: Attribute) -> Vec<String> {
        let mut options = Vec::new();
        let ast = self.ast;
        for &arg in &ast[attr.args] {
            let AttrArg::Expr(expr) = arg else { continue };
            let checked = self.expr(expr);
            let ExprKind::Str(id) = self.tast[checked].kind else { continue };
            let literal = &self.tast[id];
            if literal.encoding != Encoding::Plain {
                continue;
            }
            let text: String =
                literal.elements.iter().map(|&unit| char::from(unit as u8)).collect();
            options.extend(text.split(',').map(|part| part.trim().to_string()));
        }
        options
    }

    /// The extensions `__attribute__((target(...)))` says a function is built for, read from
    /// every list the declaration has, and nothing when none of them has the attribute.
    ///
    /// Every string of every `target` attribute is one comma separated list to gcc, so on x86-64
    /// they are read into one [`Target`] and applied over the unit's set once, at the end. A name
    /// gcc does not know is refused in gcc's words, and a declaration with a refused string is
    /// taken to be built for the unit, since gcc drops the attribute that carried it.
    ///
    /// AArch64's strings are read with [`Isa::aarch64_target`], one after another over the
    /// unit's set, and what they say about the CRC32 extension is kept, which is what lets a
    /// function with `target("+crc")` call the intrinsics in `<arm_acle.h>`. Nothing in them is
    /// refused, since none of them was before. Every other target has strings of its own and
    /// nothing here reads them.
    pub(in crate::check) fn targeted(&mut self, lists: &[AttrList]) -> Option<Isa> {
        let x86 = match self.cx.target.tuple.arch().as_str() {
            "x86_64" => true,
            "aarch64" => false,
            _ => return None,
        };
        let ast = self.ast;
        let mut target = Target::new();
        let mut arm = self.cx.isa;
        let (mut said, mut refused) = (false, false);
        for &list in lists {
            for &attr in &ast[list] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                    || self.gnu_name(&attr) != "target"
                {
                    continue;
                }
                said = true;
                for &arg in &ast[attr.args] {
                    let AttrArg::Expr(expr) = arg else { continue };
                    let checked = self.expr(expr);
                    let at = self.tast.expr_span(checked);
                    let ExprKind::Str(id) = self.tast[checked].kind else {
                        let what = "attribute 'target' argument not a string";
                        self.report(Diagnostic::error(what, at).with_code("E0720"));
                        refused = true;
                        continue;
                    };
                    let text: String = self.tast[id]
                        .elements
                        .iter()
                        .filter_map(|&unit| char::from_u32(unit))
                        .collect();
                    let read = if x86 {
                        target.read(&text)
                    } else {
                        arm = arm.aarch64_target(&text);
                        Ok(())
                    };
                    if let Err(why) = read {
                        self.report(Diagnostic::error(why.to_string(), at).with_code("E0720"));
                        refused = true;
                    }
                }
            }
        }
        (said && !refused).then(|| if x86 { target.over(self.cx.isa) } else { arm })
    }

    /// What an attribute list promises a call to this function does.
    ///
    /// `__attribute__((const))` and `__attribute__((pure))`, under the namespace test
    /// [`Self::never_returns`] is under and through the same unarmouring, so `__const__` in a
    /// header and `[[gnu::pure]]` are both read. `const` is the stronger of the two and wins if a
    /// list somehow carries both, which is what taking the maximum does.
    ///
    /// The `const` here is the attribute and not the type qualifier, and the parser is what makes
    /// that work: an attribute name is a token rather than an identifier, so the keyword arrives
    /// as the name. There is nowhere in an attribute list a qualifier could have meant anything
    /// else.
    pub(in crate::check) fn promised_effects(&self, attrs: AttrList) -> Effects {
        self.ast[attrs]
            .iter()
            .filter(|attr| !attr.namespace.is_some_and(|ns| self.text(ns) != "gnu"))
            .map(|attr| match self.gnu_name(attr) {
                "const" => Effects::Const,
                "pure" => Effects::Pure,
                _ => Effects::Any,
            })
            .max()
            .unwrap_or_default()
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

    /// The type a declaration's own attribute list declares once the calling convention it names
    /// is applied, and the type as written where it names none.
    ///
    /// gcc reads `ms_abi` and `sysv_abi` as attributes of a function type, and a list written on a
    /// declaration hands one to the type it declares. That is the function itself when a function
    /// is declared, and the function pointed at when a pointer to one is, one level down and no
    /// further, which is gcc's rule for a function type attribute that lands on a pointer. Anything
    /// else has no convention to take and is warned about in gcc's words.
    ///
    /// The other conventions a compiler for 32-bit x86 knows, `stdcall` and its relatives, are
    /// read here too, so that none of them is dropped without a word. On Windows gcc accepts them
    /// and does nothing, since a 64-bit Windows program has one convention and the headers write
    /// these on every declaration for the sake of the 32-bit build. Anywhere else gcc says they are
    /// ignored, and so does this.
    pub(in crate::check) fn convened(&mut self, ty: TypeId, attrs: AttrList) -> TypeId {
        match self.convention_in(attrs) {
            Some((convention, name, span)) => self.with_convention(ty, convention, &name, span),
            None => ty,
        }
    }

    /// The type a pointer's attributes make of what it points at, which is where
    /// `EFI_STATUS (EFIAPI *F)(...)` puts the convention.
    ///
    /// The pointer has not been made yet when this is asked, so `pointee` is the function itself
    /// and the one level down that [`Self::convened`] allows for is already taken.
    pub(in crate::check) fn convened_pointee(
        &mut self,
        pointee: TypeId,
        attrs: AttrList,
    ) -> TypeId {
        let Some((convention, name, span)) = self.convention_in(attrs) else { return pointee };
        if let TypeKind::Function(function) = self.types.kind(self.types.canonical(pointee)) {
            return self.function_under(function, convention);
        }
        self.not_a_function(&name, span);
        pointee
    }

    /// The convention an attribute list names, with the name that named it and where, after
    /// saying whatever there is to say about the convention attributes in it.
    ///
    /// Three things are said. `ms_abi` or `sysv_abi` on a target other than x86-64 is ignored with
    /// a warning, since there is no second convention there to pick. One of each in the same list
    /// is refused, as gcc refuses it, because a function cannot be both. And `stdcall`, `cdecl`,
    /// `fastcall`, `thiscall`, `regparm` and `vectorcall` are ignored with a warning anywhere but
    /// Windows. The same name twice is the one convention asked for twice, which is no conflict.
    fn convention_in(&mut self, attrs: AttrList) -> Option<(Convention, String, Span)> {
        let written = self.ast[attrs].to_vec();
        let tuple = self.cx.target.tuple;
        let windows = tuple.os().as_str() == "windows";
        // Where gcc says the 32-bit Windows conventions are ignored. On 32-bit x86 they mean
        // something and on Windows gcc accepts them without a word, and neither is this.
        let foreign_to_them = tuple.arch().as_str() == "x86_64" && !windows;
        let mut asked: Option<(Convention, String, Span)> = None;
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            let name = self.gnu_name(&attr).to_owned();
            match name.as_str() {
                "ms_abi" | "sysv_abi" => {
                    let Some(convention) = Convention::asked(tuple, &name) else {
                        let what = format!("'{name}' attribute ignored");
                        let note = "only x86-64 has a second calling convention to pick";
                        let dropped = Diagnostic::warning(what, attr.span).with_code("E0703");
                        self.report(dropped.note(note, attr.span));
                        continue;
                    };
                    match &asked {
                        Some((_, first, _)) if *first != name => {
                            let what = "'ms_abi' and 'sysv_abi' attributes are not compatible";
                            let refused = Diagnostic::error(what.to_string(), attr.span);
                            self.report(refused.with_code("E0740"));
                        }
                        Some(_) => {}
                        None => asked = Some((convention, name, attr.span)),
                    }
                }
                "stdcall" | "cdecl" | "fastcall" | "thiscall" | "regparm" | "vectorcall"
                    if foreign_to_them =>
                {
                    let what = format!("'{name}' attribute ignored");
                    let note = "the attribute names a calling convention of 32-bit x86, which \
                                x86-64 outside Windows does not have";
                    let dropped = Diagnostic::warning(what, attr.span).with_code("E0703");
                    self.report(dropped.note(note, attr.span));
                }
                _ => {}
            }
        }
        asked
    }

    /// The type with its function, or the function it points at, given that convention.
    fn with_convention(
        &mut self,
        ty: TypeId,
        convention: Convention,
        name: &str,
        span: Span,
    ) -> TypeId {
        let canonical = self.types.canonical(ty);
        match self.types.kind(canonical) {
            // Already the convention asked for, which is every `sysv_abi` on Linux, so the type is
            // left as written and a diagnostic still prints the typedef the program used.
            TypeKind::Function(function)
                if self.types.signature(function).convention == convention =>
            {
                ty
            }
            TypeKind::Function(function) => self.function_under(function, convention),
            TypeKind::Pointer(pointee) => {
                let pointee = self.types.canonical(pointee);
                let TypeKind::Function(function) = self.types.kind(pointee) else {
                    self.not_a_function(name, span);
                    return ty;
                };
                let quals = self.types.quals(canonical);
                let function = self.function_under(function, convention);
                let pointer = self.types.pointer(function);
                self.types.qualified(pointer, quals)
            }
            _ => {
                self.not_a_function(name, span);
                ty
            }
        }
    }

    /// Refuses the definition of a variadic function in the convention the target does not call
    /// its own.
    ///
    /// Calling one is fine and is done. Defining one is not here yet: its body would need the
    /// other platform's `va_list`, which is a plain pointer on Windows and a structure of four
    /// fields everywhere else, and `va_start` and `va_arg` in it would have to build and walk the
    /// other one of the two, with `__builtin_ms_va_list` and its relatives for a program to name
    /// it. gcc has all of that, and until this compiler does a definition is refused in words that
    /// name the attribute rather than compiled with the wrong list.
    pub(in crate::check) fn foreign_variadic(&mut self, ty: TypeId, span: Span) {
        let TypeKind::Function(function) = self.types.kind(self.types.canonical(ty)) else {
            return;
        };
        let signature = self.types.signature(function);
        let Some(name) = signature.convention.attribute() else { return };
        if !signature.variadic {
            return;
        }
        let what = format!(
            "defining a variadic function with '__attribute__(({name}))' is not supported on \
             this target"
        );
        let note = "calling one is supported, and so is defining one without the attribute or \
                    without the '...'";
        let refused = Diagnostic::error(what, span).with_code("E0741");
        self.report(refused.note(note, span));
    }

    /// The same function type under another convention.
    fn function_under(&mut self, function: FunctionId, convention: Convention) -> TypeId {
        let signature = FunctionType { convention, ..self.types.signature(function).clone() };
        self.types.function(signature)
    }

    /// gcc's warning for a convention written on something that is not a function.
    fn not_a_function(&mut self, name: &str, span: Span) {
        let what = format!("'{name}' attribute only applies to function types");
        self.report(Diagnostic::warning(what, span).with_code("E0703"));
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
            if self.gnu_name(&attr) != "mode" {
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
    fn mode_of(&mut self, ty: TypeId, attr: Attribute) -> Option<TypeId> {
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
                self.types.complex_float(kind)
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
            if self.gnu_name(&attr) != "vector_size" {
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
    fn vector_of(&mut self, elem: TypeId, attr: Attribute) -> Option<TypeId> {
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
    fn aligned_argument(&mut self, attr: Attribute) -> Option<u32> {
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
