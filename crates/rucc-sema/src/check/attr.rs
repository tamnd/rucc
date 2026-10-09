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

use std::num::NonZeroU32;

use rucc_ast::{AlignSpec, AttrArg, AttrList, AttrSyntax, Attribute, PragmaOptions};
use rucc_base::Symbol;
use rucc_base::float::Format;
use rucc_diag::{Diagnostic, Span};
use rucc_gnu::{Answer, Kind, Status};
use rucc_lex::Encoding;
use rucc_target::{BitFieldStyle, Convention, Isa, ObjectFormat, Target, TargetInfo};
use rucc_types::{
    FloatKind, FunctionId, FunctionType, IntKind, TypeId, TypeKind, float_format, int_width,
    integer_info, is_arithmetic, is_complex, is_function, is_real_floating, layout,
};

use crate::check::Checker;
use crate::decl::{
    DeclFlags, DeclId, DeclKind, Effects, Linkage, Priority, Startup, StorageDuration, Visibility,
};
use crate::eval;
use crate::expr::ExprKind;
use crate::scope::Binding;
use crate::tast::{AllocSize, StrId};

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
pub(in crate::check) const BIGGEST_ALIGNMENT: u32 = 16;

/// What `alias` says of an argument that is not a string, which `weakref` shares.
const ALIAS_STRING: &str = "'alias' requires a string naming the symbol to alias";

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
    /// answering no has already told the program. So does a row newer than the gcc the persona
    /// claims. `__packed__` is read as `packed`, and an attribute in some namespace other than
    /// `gnu` is not GCC's and is not read.
    pub(in crate::check) fn gnu_name(&self, attr: &Attribute) -> &'static str {
        if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
            return "";
        }
        if let Some(&name) = self.gnu_names.borrow().get(&attr.name) {
            return name;
        }
        let name = match rucc_gnu::lookup(Kind::Attribute, self.text(attr.name)) {
            Some(row)
                if matches!(row.status, Status::Implemented | Status::Partial)
                    && row.is_known_to(self.cx.gnuc) =>
            {
                row.name
            }
            _ => "",
        };
        self.gnu_names.borrow_mut().insert(attr.name, name);
        name
    }

    /// Refuses every GNU attribute in the unit that the table says would be wrong code to ignore.
    ///
    /// An attribute the checker does not read is dropped wherever it was written, which is right
    /// for one that only asks for a warning or an optimization and wrong for one that changes
    /// what the code does: an `interrupt` handler that returns with `ret`, or a
    /// `no_caller_saved_registers` function that clobbers what its caller kept in a register,
    /// links and runs and breaks the machine later. Those rows say `answer = "error"`, and this is
    /// where that is kept. It is asked once of every attribute in the tree rather than by each
    /// reader, because the point is to catch the ones no reader looks at, and an attribute on
    /// something the checker never visits is still one the program relied on.
    pub(in crate::check) fn refuse_unimplemented_attributes(&mut self) {
        let ast = self.ast;
        let mut refused: Vec<Span> = Vec::new();
        for attr in ast.attributes() {
            let gnu = match attr.namespace {
                Some(ns) => self.text(ns) == "gnu",
                None => attr.syntax == AttrSyntax::Gnu,
            };
            if !gnu || refused.contains(&attr.span) {
                continue;
            }
            let Some(row) = rucc_gnu::lookup(Kind::Attribute, self.text(attr.name)) else {
                continue;
            };
            // An attribute that came in a later gcc than the persona claims is one that gcc has
            // never heard of, and it says so under -Wattributes. The kernel's check for
            // `counted_by` is a compile under -Werror, so the warning is the answer it reads.
            if !row.is_known_to(self.cx.gnuc) {
                refused.push(attr.span);
                let what = match attr.namespace {
                    Some(_) => format!("'gnu::{}' scoped attribute directive ignored", row.name),
                    None => format!("'{}' attribute directive ignored", row.name),
                };
                self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                continue;
            }
            // gcc has no wasm target, so on any other row the names that only clang's wasm
            // target has are names that gcc 16 has never heard of, and it says so.
            let tuple = &self.cx.target.tuple;
            let here = rucc_gnu::Target::new(tuple.arch().as_str(), tuple.os().as_str());
            if row.targets == [rucc_gnu::Place::Wasm] && !row.is_on(here) {
                refused.push(attr.span);
                let what = format!("'{}' attribute directive ignored", row.name);
                self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                continue;
            }
            // The two attributes about saving the machine are implemented for x86-64 alone, so
            // everywhere else they are what the row would have said before they were. See
            // [`Self::handler`].
            let elsewhere = matches!(row.name, "interrupt" | "no_caller_saved_registers")
                && self.cx.target.tuple.arch().as_str() != "x86_64";
            if row.answer != Answer::Error
                || (matches!(row.status, Status::Implemented | Status::Partial) && !elsewhere)
            {
                continue;
            }
            refused.push(attr.span);
            let what = format!("'{}' attribute is not supported", row.name);
            let note = "ignoring it would change what the program does, so it is refused";
            self.report(
                Diagnostic::error(what, attr.span).with_code("E0753").note(note, attr.span),
            );
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
    /// The format strings themselves are read by `check/format.rs`, under `-Wformat`, for the
    /// `printf`, `scanf` and `strftime` archetypes and their `gnu_` names. The others are accepted
    /// and read no further.
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
    /// [`Self::startup`]. `used` and `retain` ask for nothing else. On a wasm row `export_name`
    /// keeps the definition too, because the host calls it, which is what clang does.
    pub(in crate::check) fn retains(&mut self, attrs: AttrList) -> bool {
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            let named = self.gnu_name(&attr);
            let exported =
                named == "export_name" && self.cx.target.object_format == ObjectFormat::Wasm;
            if RETAINING.contains(&named) || exported {
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
            // `constructor(P)` with `P` an enumerator, which the parser keeps as an identifier
            // because `format(printf, 1, 2)` does.
            Some(&AttrArg::Ident(name)) => {
                let Some(value) = self.enumerator(name) else {
                    self.report(Diagnostic::error(range, attr.span).with_code("E0703"));
                    return None;
                };
                value
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
            self.report(Diagnostic::warning(what, attr.span).with_code("E0769"));
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
                return self.alias_argument(attr, ALIAS_STRING);
            }
        }
        None
    }

    /// The resolver an `ifunc` attribute names, once gcc's rules about where one may be written
    /// have been checked.
    ///
    /// `ifunc("resolver")` makes the function's own name an indirect function: the symbol is
    /// typed `@gnu_indirect_function` and set to the resolver, and the dynamic linker calls the
    /// resolver once and binds every call to the function it hands back. That is an alias with
    /// another symbol type, so the name goes where `alias` puts its target and the declaration is
    /// marked as the one kind of alias that is not a second name for the same code. Whether the
    /// resolver is defined is settled with the aliases, and what it returns once the file is
    /// checked, by [`Self::check_resolvers`].
    ///
    /// The number of arguments is refused first, on whatever it is written on, and the attribute
    /// is ignored with gcc's warning on anything but a function at file scope. Only ELF has the
    /// symbol type, so on a COFF or Mach-O target the function would have no body at all, and it
    /// is refused in gcc's words. `lists` are the specifiers' attributes and the declarator's.
    pub(in crate::check) fn resolver(
        &mut self,
        lists: &[AttrList],
        kind: DeclKind,
        span: Span,
    ) -> Option<StrId> {
        let ast = self.ast;
        let attr = lists
            .iter()
            .flat_map(|&list| ast[list].iter().copied())
            .find(|attr| self.gnu_name(attr) == "ifunc")?;
        let args = ast[attr.args].len();
        if args != 1 {
            let what = "wrong number of arguments specified for 'ifunc' attribute";
            let note = format!("expected 1, found {args}");
            let refused = Diagnostic::error(what, attr.span).with_code("E0819");
            self.report(refused.note(note, attr.span));
            return None;
        }
        if kind != DeclKind::Function || !self.scopes.at_file_scope() {
            let what = "'ifunc' attribute ignored";
            self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            return None;
        }
        let resolver = self.alias_argument(attr, "attribute 'ifunc' argument not a string")?;
        if self.cx.target.object_format != ObjectFormat::Elf {
            let what = "'ifunc' is not supported on this target";
            let note = "only an ELF object has a symbol type for a function a resolver picks";
            self.report(Diagnostic::error(what, span).with_code("E0821").note(note, attr.span));
            return None;
        }
        Some(resolver)
    }

    /// What every `ifunc` in the file names, checked against the function it stands for once
    /// the whole file is known, since the resolver may be defined below the declaration.
    ///
    /// gcc's rule is about what the resolver returns. Something that is not a pointer is not an
    /// address the dynamic linker could bind a call to, and is refused. A pointer to another
    /// function type, or to an object, is a call that would go wrong at run time and is warned
    /// about under `-Wattribute-alias`, and `void *` is what glibc's resolvers return and is
    /// taken as it is. A resolver that is an object is refused as an alias between a function
    /// and a variable. A resolver the file never declares is left to the aliases, which report a
    /// target that is not defined.
    pub(crate) fn check_resolvers(&mut self) {
        let mut ifuncs = self.tast.ifuncs();
        ifuncs.sort_by_key(|decl| decl.index());
        for decl in ifuncs {
            let node = self.tast[decl].clone();
            let Some(written) = node.alias else { continue };
            let spelling: String = self.tast[written]
                .elements
                .iter()
                .filter_map(|&unit| char::from_u32(unit))
                .collect();
            let Some(resolver) = self.cx.names.find(&spelling) else { continue };
            let Some(found) = self.file_scope_decl(resolver) else { continue };
            let at = self.tast.decl_span(decl);
            let there = self.tast.decl_span(found);
            let name = node.name.map_or("", |name| self.text(name)).to_owned();
            let target = self.tast[found].clone();
            if target.kind != DeclKind::Function {
                let what = format!("'{name}' alias between function and variable is not supported");
                let refused = Diagnostic::error(what, at).with_code("E0821");
                self.report(refused.note("aliased declaration here", there));
                continue;
            }
            let declared = self.types.canonical(target.ty);
            let TypeKind::Function(signature) = self.types.kind(declared) else { continue };
            let returned = self.types.signature(signature).ret;
            let wanted = self.types.pointer(node.ty);
            let wanted_spelled = self.spell(wanted);
            let note = "resolver indirect function declared here";
            match rucc_types::pointee(&self.types, returned) {
                None => {
                    let what =
                        format!("'ifunc' resolver for '{name}' must return '{wanted_spelled}'");
                    let refused = Diagnostic::error(what, there).with_code("E0821");
                    self.report(refused.note(note, at));
                }
                Some(pointee) => {
                    if rucc_types::is_void(&self.types, pointee)
                        || rucc_types::compatible(&self.types, pointee, node.ty)
                    {
                        continue;
                    }
                    let what =
                        format!("'ifunc' resolver for '{name}' should return '{wanted_spelled}'");
                    let warned = Diagnostic::warning(what, there).with_code("E0820");
                    self.report(warned.note(note, at));
                }
            }
        }
    }

    /// The symbol a `weakref` attribute makes this declaration a weak reference to, once gcc's
    /// rules about where one may be written have been checked.
    ///
    /// Two spellings name the target. `weakref("target")` names it as the attribute's own
    /// argument, and `weakref` with `alias("target")` after it names it through the alias, which
    /// is the older form and the one glibc's `weak_extern` machinery writes. gcc refuses the
    /// alias in front of the weakref, since it has already made the name an alias by the time it
    /// reads the weakref, and a bare `weakref` with no alias anywhere is a warning and nothing
    /// else: the declaration is then an ordinary one, and a reference to it is a strong one.
    ///
    /// The name has to be `static`, which is gcc's rule and is what keeps the local spelling from
    /// being a symbol of its own that another object could see. It cannot be given a value either,
    /// since a name that is a reference to something else has no storage for one to go in. And a
    /// weakref inside a block is ignored with a warning, which is what gcc does with it there.
    ///
    /// Only an ELF object has a way to write the reference: `.weakref` and a weak undefined
    /// symbol are both ELF's, and a COFF or Mach-O target has no reading of it that this compiler
    /// could emit. There it is refused with the sentence a table row gets when ignoring it would
    /// change what the program does, which it would.
    ///
    /// `lists` are the specifiers' attributes and then the declarator's, in the order they were
    /// written, which is the order the rule about `alias` is a rule about.
    pub(in crate::check) fn weak_reference(
        &mut self,
        lists: [AttrList; 2],
        alias: Option<StrId>,
        linkage: Linkage,
        initialized: bool,
        name: Symbol,
        span: Span,
    ) -> Option<StrId> {
        let mut after_alias = false;
        let mut found = None;
        'lists: for list in lists {
            let written = self.ast[list].to_vec();
            for attr in written {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                    continue;
                }
                match self.gnu_name(&attr) {
                    "alias" => after_alias = true,
                    "weakref" => {
                        found = Some(attr);
                        break 'lists;
                    }
                    _ => {}
                }
            }
        }
        let attr = found?;
        let at = attr.span;
        let spelled = self.text(name).to_owned();
        if self.cx.target.object_format != ObjectFormat::Elf {
            let what = "'weakref' attribute is not supported";
            let note =
                "only an ELF object has a way to refer to a symbol weakly under another name";
            self.report(Diagnostic::error(what, at).with_code("E0753").note(note, at));
            return None;
        }
        if !self.scopes.at_file_scope() {
            let what = "'weakref' attribute ignored";
            self.report(Diagnostic::warning(what, at).with_code("E0784"));
            return None;
        }
        if after_alias {
            let what = "'weakref' attribute must appear before 'alias' attribute";
            self.report(Diagnostic::error(what, span).with_code("E0782"));
            return None;
        }
        // An argument that is not a string has been reported by the reader `alias` shares, and
        // the declaration is then an ordinary one, which is what is left once gcc has said so.
        let own = match self.ast[attr.args].first() {
            None => None,
            Some(_) => Some(self.alias_argument(attr, ALIAS_STRING)?),
        };
        if (own.is_some() && alias.is_some()) || initialized {
            let what = format!("'{spelled}' defined both normally and as 'alias' attribute");
            self.report(Diagnostic::error(what, span).with_code("E0783"));
            return None;
        }
        let Some(target) = own.or(alias) else {
            let what = "'weakref' attribute should be accompanied with an 'alias' attribute";
            self.report(Diagnostic::warning(what, at).with_code("E0784"));
            return None;
        };
        if linkage != Linkage::Internal {
            let what = format!("'weakref' symbol '{spelled}' must have static linkage");
            self.report(Diagnostic::error(what, span).with_code("E0781"));
            return None;
        }
        Some(target)
    }

    /// The string one `alias` was written with, and nothing when it was not written with one.
    ///
    /// `what` is the sentence for an argument that is not a string, which `ifunc` words as gcc
    /// does and `alias` as this compiler always has.
    fn alias_argument(&mut self, attr: Attribute, what: &str) -> Option<StrId> {
        let args = self.ast[attr.args].to_vec();
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
            let named = self.gnu_name(&attr);
            let wide = format!("wide string literal in '{named}'");
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

    /// The declaration a `copy(name)` in one of the lists names, with where the attribute was
    /// written, for [`Checker::copy_onto`] to take the attributes of.
    ///
    /// The kernel writes this on the aliases `module_init` and `module_exit` make, through
    /// `__copy(initfn)`, so that the second name for an init function carries the section and
    /// the `cold` of the first. glibc writes it the same way on its own aliases. The argument is
    /// an identifier, kept as one the way `cleanup(free)` is, and it is looked up in the scope the
    /// declaration is in, so it names something declared above. The first list that has one is
    /// the one read, which is what a list is read as everywhere else here.
    ///
    /// What is refused is refused in gcc's words where gcc has words for it: the wrong number of
    /// arguments, a name nothing is declared as, and something that is not a name at all. gcc
    /// also takes an expression and copies the attributes of its type, which nothing anybody
    /// writes does and which this does not, so an expression is refused with the rest.
    pub(in crate::check) fn copied(&mut self, lists: &[AttrList]) -> Option<(DeclId, Span)> {
        for &attrs in lists {
            let written = self.ast[attrs].to_vec();
            for attr in written {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                    continue;
                }
                if self.gnu_name(&attr) != "copy" {
                    continue;
                }
                let args = self.ast[attr.args].to_vec();
                if args.len() != 1 {
                    let what = "wrong number of arguments specified for 'copy' attribute";
                    let note = format!("expected 1, found {}", args.len());
                    let refused = Diagnostic::error(what, attr.span).with_code("E0793");
                    self.report(refused.note(note, attr.span));
                    return None;
                }
                let AttrArg::Ident(name) = args[0] else {
                    let what = "'copy' attribute argument is not the name of a declaration";
                    self.report(Diagnostic::error(what, attr.span).with_code("E0793"));
                    return None;
                };
                let Some(Binding::Decl(source)) = self.scopes.lookup(name) else {
                    let what = format!("'{}' undeclared here", self.text(name));
                    self.report(Diagnostic::error(what, attr.span).with_code("E0793"));
                    return None;
                };
                return Some((source, attr.span));
            }
        }
        None
    }

    /// Reads `alloc_size(n)` and `alloc_size(n, m)` off the lists a declaration was written with
    /// and keeps the argument numbers against the declaration, for `rucc_opt::objsize` to read
    /// off a call to it.
    ///
    /// glibc writes it on `malloc`, `calloc`, `realloc` and their friends through `__wur
    /// __attribute_alloc_size__`, and the kernel on `kmalloc` and every allocator under it, and
    /// that is what lets `_FORTIFY_SOURCE` check a copy into what one of them gave back. Every
    /// number is checked the way gcc 16 checks it and a list with a bad one is dropped with gcc's
    /// warning: zero, a number past the parameters, a parameter that is not an integer, and the
    /// attribute on something that is not a function returning a pointer. The wrong number of
    /// arguments is an error, as it is in gcc. The lists are read in order and a later one stands.
    pub(in crate::check) fn record_alloc_size(
        &mut self,
        decl: DeclId,
        lists: &[AttrList],
        kind: DeclKind,
        ty: TypeId,
    ) {
        for &attrs in lists {
            let written = self.ast[attrs].to_vec();
            for attr in written {
                if self.gnu_name(&attr) != "alloc_size" {
                    continue;
                }
                if let Some(alloc) = self.alloc_size_argument(attr, kind, ty) {
                    self.tast.record_alloc_size(decl, alloc);
                }
            }
        }
    }

    /// Records the room a `patchable_function_entry` in the lists asks a function to open with,
    /// in place of what `-fpatchable-function-entry=` asks of every function.
    ///
    /// What is refused is what gcc 13 refuses, in its words: the wrong number of arguments is an
    /// error, and an argument that is not a constant from 0 to 65535 drops the attribute with a
    /// warning. A part in front of the label larger than the total is warned about and taken as
    /// none, which is what gcc writes. gcc says nothing about the attribute on a variable or a
    /// type and neither is this, and it is dropped there. So is it on a target whose objects are
    /// not ELF, which has no section to list the room in, the reason the flag is refused there.
    pub(in crate::check) fn record_patchable(
        &mut self,
        decl: DeclId,
        lists: &[AttrList],
        kind: DeclKind,
    ) {
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.gnu_name(&attr) != "patchable_function_entry" {
                    continue;
                }
                let Some((total, before)) = self.patchable_argument(attr) else { continue };
                if kind == DeclKind::Function && self.cx.target.object_format == ObjectFormat::Elf {
                    self.tast.record_patchable(decl, total, before);
                }
            }
        }
    }

    /// The total and the part in front of the label one `patchable_function_entry` asks for, or
    /// nothing where gcc refuses it or drops it.
    fn patchable_argument(&mut self, attr: Attribute) -> Option<(u32, u32)> {
        let args = self.ast[attr.args].to_vec();
        if args.is_empty() || args.len() > 2 {
            let what =
                "wrong number of arguments specified for 'patchable_function_entry' attribute";
            let note = format!("expected between 1 and 2, found {}", args.len());
            let refused = Diagnostic::error(what, attr.span).with_code("E0818");
            self.report(refused.note(note, attr.span));
            return None;
        }
        let mut counts = [0u32; 2];
        for (count, arg) in counts.iter_mut().zip(args) {
            let (value, spelled) = match arg {
                AttrArg::Ident(name) => (None, Some(self.text(name).to_owned())),
                AttrArg::Expr(expr) => {
                    let named = match self.ast[expr] {
                        rucc_ast::Expr::Name(name) => Some(self.text(name).to_owned()),
                        _ => None,
                    };
                    let value = self.expr(expr);
                    let folded = self.eval_integer(value).ok();
                    (folded, named.or_else(|| folded.map(|n| n.to_string())))
                }
            };
            let quoted = spelled.map_or_else(String::new, |spelled| format!(" '{spelled}'"));
            let what = match value {
                Some(n) if n > 65535 => {
                    format!("'patchable_function_entry' attribute argument{quoted} exceeds 65535")
                }
                Some(n) if n >= 0 => {
                    *count = u32::try_from(n).unwrap_or_default();
                    continue;
                }
                _ => format!(
                    "'patchable_function_entry' attribute argument{quoted} is not an integer \
                     constant"
                ),
            };
            self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            return None;
        }
        let [total, before] = counts;
        if before > total {
            let what = format!("patchable function entry {before} exceeds size {total}");
            self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            return Some((total, 0));
        }
        Some((total, before))
    }

    /// Records the names that `export_name`, `import_module` and `import_name` give a function
    /// on a wasm row, in place of the names that a wasm object gives it by default.
    ///
    /// These are clang attributes and gcc has no wasm target, so what is refused is what clang 23
    /// from wasi-sdk 34 refuses, in its words, and each refusal is an error: the wrong number of
    /// arguments, the attribute on anything but a function, and an argument that is not a plain
    /// string literal. On a row that is not wasm the table does not have the three names, and
    /// [`Self::refuse_unimplemented_attributes`] warns about them as gcc does.
    pub(in crate::check) fn record_wasm_names(
        &mut self,
        decl: DeclId,
        lists: &[AttrList],
        kind: DeclKind,
    ) {
        if self.cx.target.object_format != ObjectFormat::Wasm {
            return;
        }
        let ast = self.ast;
        let mut said = crate::tast::WasmNames::default();
        for &attrs in lists {
            for &attr in &ast[attrs] {
                let named = self.gnu_name(&attr);
                if !matches!(named, "export_name" | "import_module" | "import_name") {
                    continue;
                }
                let Some(name) = self.wasm_name_argument(attr, named, kind) else { continue };
                match named {
                    "export_name" => said.export = Some(name),
                    "import_module" => said.module = Some(name),
                    _ => said.field = Some(name),
                }
            }
        }
        if said != crate::tast::WasmNames::default() {
            self.tast.record_wasm_names(decl, said);
        }
    }

    /// The string one `export_name`, `import_module` or `import_name` gives, or nothing where
    /// clang refuses it.
    fn wasm_name_argument(
        &mut self,
        attr: Attribute,
        named: &str,
        kind: DeclKind,
    ) -> Option<StrId> {
        let args = self.ast[attr.args].to_vec();
        if args.len() != 1 {
            let what = format!("'{named}' attribute takes one argument");
            self.report(Diagnostic::error(what, attr.span).with_code("E0822"));
            return None;
        }
        if kind != DeclKind::Function {
            let what = format!("'{named}' attribute only applies to functions");
            self.report(Diagnostic::error(what, attr.span).with_code("E0822"));
            return None;
        }
        let what = format!("expected string literal as argument of '{named}' attribute");
        let AttrArg::Expr(expr) = args[0] else {
            self.report(Diagnostic::error(what, attr.span).with_code("E0822"));
            return None;
        };
        let checked = self.expr(expr);
        match self.tast[checked].kind {
            ExprKind::Str(id) if self.tast[id].encoding == Encoding::Plain => Some(id),
            _ => {
                let at = self.tast.expr_span(checked);
                self.report(Diagnostic::error(what, at).with_code("E0822"));
                None
            }
        }
    }

    /// The argument numbers one `alloc_size` names, or nothing where gcc drops or refuses it.
    fn alloc_size_argument(
        &mut self,
        attr: Attribute,
        kind: DeclKind,
        ty: TypeId,
    ) -> Option<AllocSize> {
        let args = self.ast[attr.args].to_vec();
        if args.is_empty() || args.len() > 2 {
            let what = "wrong number of arguments specified for 'alloc_size' attribute";
            let note = format!("expected between 1 and 2, found {}", args.len());
            let refused = Diagnostic::error(what, attr.span).with_code("E0794");
            self.report(refused.note(note, attr.span));
            return None;
        }
        let ignored = |checker: &mut Self, what: String| {
            checker.report(Diagnostic::warning(what, attr.span).with_code("E0794"));
            None
        };
        let function = match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Function(id) if kind == DeclKind::Function => id,
            _ => {
                return ignored(
                    self,
                    "'alloc_size' attribute only applies to function types".into(),
                );
            }
        };
        let signature = self.types.signature(function).clone();
        if !matches!(self.types.kind(self.types.canonical(signature.ret)), TypeKind::Pointer(_)) {
            let what =
                "'alloc_size' attribute ignored on a function that does not return a pointer";
            return ignored(self, what.into());
        }
        let mut numbers = Vec::with_capacity(args.len());
        for arg in args {
            let AttrArg::Expr(expr) = arg else {
                return ignored(self, "'alloc_size' attribute argument is invalid".into());
            };
            let value = self.expr(expr);
            let Ok(number) = self.eval_integer(value) else {
                return ignored(self, "'alloc_size' attribute argument is invalid".into());
            };
            let params = signature.params.len();
            let Some(index) = usize::try_from(number).ok().and_then(|n| n.checked_sub(1)) else {
                let what = format!(
                    "'alloc_size' attribute argument value '{number}' does not refer to a function \
                     parameter"
                );
                return ignored(self, what);
            };
            let Some(&param) = signature.params.get(index) else {
                let what = format!(
                    "'alloc_size' attribute argument value '{number}' exceeds the number of \
                     function parameters {params}"
                );
                return ignored(self, what);
            };
            if !rucc_types::is_integer(&self.types, self.types.canonical(param)) {
                let what = format!(
                    "'alloc_size' attribute argument value '{number}' refers to a parameter that \
                     is not an integer"
                );
                return ignored(self, what);
            }
            numbers.push(u8::try_from(number).ok()?);
        }
        Some(AllocSize { size: numbers[0], count: numbers.get(1).copied() })
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

    /// What the attribute lists say about how much of the machine the function puts back, as
    /// [`DeclFlags::INTERRUPT`] and [`DeclFlags::SAVES_ALL`], with gcc's checks of where they
    /// were written.
    ///
    /// `__attribute__((interrupt))` and `__attribute__((no_caller_saved_registers))`, under the
    /// namespace test [`Self::never_returns`] is under and through the same unarmouring. Only
    /// x86-64 reads them, and on every other target this leaves them alone for
    /// [`Self::refuse_unimplemented_attributes`] to refuse, since a handler compiled as an ordinary
    /// function returns with the wrong instruction.
    ///
    /// On something that is not a function either is gcc's warning and nothing else. On a function,
    /// `interrupt` asks for the signature the processor calls a handler with, which gcc checks
    /// on every declaration in the order this does: a pointer to the frame it pushed first, an
    /// optional error code the width of a register second, nothing else, and no value back. A
    /// naked handler is refused as well, because the attribute's whole job is the prologue and the
    /// epilogue `naked` says not to write. `naked` is whether the same declaration said that.
    pub(in crate::check) fn handler(
        &mut self,
        lists: &[AttrList],
        ty: TypeId,
        naked: bool,
    ) -> DeclFlags {
        if self.cx.target.tuple.arch().as_str() != "x86_64" {
            return DeclFlags::NONE;
        }
        let ast = self.ast;
        let mut flags = DeclFlags::NONE;
        for &list in lists {
            for &attr in &ast[list] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                    continue;
                }
                let (name, flag) = match self.gnu_name(&attr) {
                    "interrupt" => ("interrupt", DeclFlags::INTERRUPT),
                    "no_caller_saved_registers" => {
                        ("no_caller_saved_registers", DeclFlags::SAVES_ALL)
                    }
                    _ => continue,
                };
                let TypeKind::Function(function) = self.types.kind(self.types.canonical(ty)) else {
                    self.not_a_function(name, attr.span);
                    continue;
                };
                if flag == DeclFlags::INTERRUPT {
                    self.interrupt_signature(function, naked, attr.span);
                }
                flags |= flag;
            }
        }
        flags
    }

    /// gcc's checks of the signature of a function written `__attribute__((interrupt))`, in gcc's
    /// order and gcc's words.
    ///
    /// The processor pushes a frame and calls the handler with nothing in a register, so what the
    /// handler takes is where that frame is and, for the exceptions that push one, the error code
    /// under it. The code is a word wide, so the second parameter is a 64 bit integer, signed or
    /// not; gcc names `unsigned long int` because that is what it suggests writing, and an
    /// enumeration or a `_Bool` is not an integer type in its sense. An unprototyped declaration
    /// takes nothing, which is the third complaint.
    fn interrupt_signature(&mut self, function: FunctionId, naked: bool, span: Span) {
        let signature = self.types.signature(function).clone();
        if let Some(&frame) = signature.params.first() {
            if !rucc_types::is_pointer(&self.types, frame) {
                let what = "interrupt service routine should have a pointer as the first argument";
                self.report(Diagnostic::error(what, span).with_code("E0791"));
            }
        }
        if let Some(&code) = signature.params.get(1) {
            let word = matches!(
                self.types.kind(self.types.canonical(code)),
                TypeKind::Int(
                    IntKind::Long | IntKind::ULong | IntKind::LongLong | IntKind::ULongLong
                )
            );
            if !word {
                let what = "interrupt service routine should have 'unsigned long int' as the second argument";
                self.report(Diagnostic::error(what, span).with_code("E0791"));
            }
        }
        if signature.params.is_empty() || signature.params.len() > 2 {
            let what = "interrupt service routine can only have a pointer argument and an optional \
                        integer argument";
            self.report(Diagnostic::error(what, span).with_code("E0791"));
        }
        if !rucc_types::is_void(&self.types, signature.ret) {
            let what = "interrupt service routine must return 'void'";
            self.report(Diagnostic::error(what, span).with_code("E0791"));
        }
        if naked {
            let what = "interrupt and naked attributes are not compatible";
            self.report(Diagnostic::error(what, span).with_code("E0791"));
        }
    }

    /// Refuses the definition of a function that saves the general purpose registers it touches
    /// and nothing else, when it is built for registers it does not save.
    ///
    /// An interrupt handler and a `no_caller_saved_registers` function promise to give back every
    /// register they were handed as it was, and what this compiler puts back is the general
    /// purpose ones. A body that may reach for a vector register or the x87 stack could break that
    /// promise without anything in the source saying so, which is why gcc says sorry for one unless
    /// the function is built without them, by `-mgeneral-regs-only` or a `target` attribute saying
    /// `general-regs-only`. Its words, naming the first of the three the function may still use,
    /// and calling a handler that takes an error code an exception service routine. `isa` and
    /// `x87` are what the function is built for, its own `target` attribute included.
    ///
    /// The order the three are asked in is gcc's, which is not the same for the two attributes. A
    /// handler names SSE first. A `no_caller_saved_registers` function names MMX first and then the
    /// x87 stack, and gcc does not ask about SSE for it at all, because it saves the vector
    /// registers of one of those in its prologue. This compiler does not, so it asks about SSE last
    /// and in the same words, which is a refusal gcc does not make, and only on a command line that
    /// took MMX and the x87 stack away and left SSE, which is not one the kernel uses.
    pub(in crate::check) fn handler_registers(
        &mut self,
        flags: DeclFlags,
        ty: TypeId,
        isa: Isa,
        x87: bool,
        span: Span,
    ) {
        let interrupt = flags.contains(DeclFlags::INTERRUPT);
        if !interrupt && !flags.contains(DeclFlags::SAVES_ALL) {
            return;
        }
        let has = |name: &str| rucc_target::Feature::named(name).is_some_and(|it| isa.has(it));
        let (sse, mmx) = (has("sse"), has("mmx"));
        let used = match (interrupt, sse, mmx, x87) {
            (true, true, _, _) => "SSE",
            (_, _, true, _) => "MMX/3Dnow",
            (_, _, _, true) => "80387",
            (false, true, _, _) => "SSE",
            _ => return,
        };
        let what = if interrupt {
            let exception = match self.types.kind(self.types.canonical(ty)) {
                TypeKind::Function(function) => self.types.signature(function).params.len() == 2,
                _ => false,
            };
            let kind = if exception { "exception" } else { "interrupt" };
            format!("{used} instructions aren't allowed in an {kind} service routine")
        } else {
            format!(
                "{used} instructions aren't allowed in a function with the \
                 'no_caller_saved_registers' attribute"
            )
        };
        let help = "build it with '-mgeneral-regs-only' or '__attribute__((target(\"general-regs-only\")))'";
        self.report(Diagnostic::error(what, span).with_code("E0792").help(help, span));
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

    /// [`Self::inlining`] of the attributes on the specifiers of a declaration, when `after` is what
    /// the ones after its declarator say.
    ///
    /// gcc applies the ones after the declarator first, so of `common` and `nocommon` written one
    /// on each side it is the one after that stands, and the one on the specifiers is ignored with
    /// the warning it gets when the two are in one list.
    pub(in crate::check) fn inlining_before(
        &mut self,
        attrs: AttrList,
        after: DeclFlags,
    ) -> DeclFlags {
        let mut flags = self.inlining(attrs);
        let ast = self.ast;
        for &attr in &ast[attrs] {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            let (now, other, said) = match self.gnu_name(&attr) {
                "common" => (DeclFlags::COMMON, DeclFlags::NO_COMMON, "nocommon"),
                "nocommon" => (DeclFlags::NO_COMMON, DeclFlags::COMMON, "common"),
                _ => continue,
            };
            if flags.contains(now) && after.contains(other) {
                let name = self.gnu_name(&attr);
                let what = format!(
                    "ignoring attribute '{name}' because it conflicts with attribute '{said}'"
                );
                self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                flags = flags.with(now, false);
            }
        }
        flags
    }

    /// What an attribute list says about inlining and about what is written around the body, as
    /// the bits it can set.
    ///
    /// `always_inline`, `noinline`, `noipa`, `no_instrument_function`, `no_stack_protector`,
    /// `stack_protect`, `cold`, `hot`, `function_return("keep")`, `indirect_branch("keep")` and
    /// `zero_call_used_regs`, `cf_check`, `force_align_arg_pointer`, `no_reorder`, `ms_hook_prologue`, and the `optimize` options that stand for two of them, and
    /// `uninitialized`, `common`, `nocommon` and `retain`, which are not about inlining but are read in the
    /// same two places, under the namespace test [`Self::never_returns`] is under and through the
    /// same unarmouring, so `__always_inline__` in a header and `[[gnu::noinline]]` are both read.
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
                "noipa" => flags |= DeclFlags::NOINLINE | DeclFlags::NOIPA,
                "no_instrument_function" => flags |= DeclFlags::NO_INSTRUMENT,
                "no_profile_instrument_function" => flags |= DeclFlags::NO_PROFILE,
                "no_sanitize_coverage" => flags |= DeclFlags::NO_SANCOV,
                "cf_check" if matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") => {
                    flags |= DeclFlags::CF_CHECK;
                }
                "force_align_arg_pointer"
                    if matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") =>
                {
                    flags |= DeclFlags::FORCE_ALIGN;
                }
                "no_reorder" => flags |= DeclFlags::NO_REORDER,
                "ms_hook_prologue"
                    if matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") =>
                {
                    flags |= DeclFlags::MS_HOOK;
                }
                "no_stack_protector" => flags = flags.then(DeclFlags::NO_STACK_PROTECTOR),
                "stack_protect" => flags = flags.then(DeclFlags::STACK_PROTECT),
                "uninitialized" => flags |= DeclFlags::UNINITIALIZED,
                // The second of the two in one declaration is ignored with gcc's warning.
                "common" | "nocommon" => {
                    let (now, other) = match name.as_str() {
                        "common" => (DeclFlags::COMMON, "nocommon"),
                        _ => (DeclFlags::NO_COMMON, "common"),
                    };
                    let said = flags.then(now);
                    if !said.contains(now) {
                        let what = format!(
                            "ignoring attribute '{name}' because it conflicts with attribute \
                             '{other}'"
                        );
                        self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    }
                    flags = said;
                }
                "retain" => flags |= DeclFlags::RETAIN,
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
                "zero_call_used_regs" => flags |= self.zero_choice(attr),
                "optimize" => {
                    for option in self.optimize_options(attr) {
                        flags = optimizing(flags, &option);
                    }
                }
                _ => {}
            }
        }
        flags
    }

    /// What `#pragma GCC optimize` has in effect over a function, as the bits its options set.
    ///
    /// gcc reads a function's own `optimize` attribute on top of the line's options, so where the
    /// two disagree the attribute wins, which is why the caller puts these after it.
    pub(in crate::check) fn pragma_optimize(&self, pragma: PragmaOptions) -> DeclFlags {
        let Some(id) = pragma.optimize else { return DeclFlags::NONE };
        let text = self.pragma_text(id);
        text.split(',').fold(DeclFlags::NONE, |flags, option| optimizing(flags, option.trim()))
    }

    /// The options a `#pragma GCC target` or `optimize` line left, which the parser wrote as one
    /// plain string with commas between them.
    fn pragma_text(&self, id: rucc_ast::StrId) -> String {
        self.ast[id].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect()
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

    /// The bits one `zero_call_used_regs` sets, reporting a choice gcc does not have.
    ///
    /// The four choices without `-gpr` in them are the four with it and [`DeclFlags::ZERO_WIDE`],
    /// which clears the vector registers and the x87 stack as well.
    fn zero_choice(&mut self, attr: Attribute) -> DeclFlags {
        let choice = self.optimize_options(attr);
        match choice.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["skip"] => DeclFlags::ZERO_SKIP,
            ["used-gpr"] => DeclFlags::ZERO_USED,
            ["used-gpr-arg"] => DeclFlags::ZERO_USED | DeclFlags::ZERO_ARG,
            ["all-gpr"] => DeclFlags::ZERO_ALL,
            ["all-gpr-arg"] => DeclFlags::ZERO_ALL | DeclFlags::ZERO_ARG,
            ["used"] => DeclFlags::ZERO_USED | DeclFlags::ZERO_WIDE,
            ["used-arg"] => DeclFlags::ZERO_USED | DeclFlags::ZERO_ARG | DeclFlags::ZERO_WIDE,
            ["all"] => DeclFlags::ZERO_ALL | DeclFlags::ZERO_WIDE,
            ["all-arg"] => DeclFlags::ZERO_ALL | DeclFlags::ZERO_ARG | DeclFlags::ZERO_WIDE,
            _ => {
                let what = format!(
                    "unrecognized 'zero_call_used_regs' attribute argument '{}'",
                    choice.join(",")
                );
                self.report(Diagnostic::error(what, attr.span).with_code("E0688"));
                DeclFlags::NONE
            }
        }
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
            // A number is a level, `optimize(0)` being `optimize("O0")` to gcc, so it is read as
            // the option it stands for and the caller only has one spelling to match.
            if !matches!(self.tast[checked].kind, ExprKind::Str(_)) {
                if let Ok(level) = self.eval_integer(checked) {
                    options.push(format!("O{level}"));
                }
                continue;
            }
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
    ///
    /// The `bool` beside the extensions is whether the function may use the x87 stack, which is
    /// the unit's answer unless an x86-64 string said `80387`, `no-80387` or `general-regs-only`.
    /// See [`Target::x87`].
    pub(in crate::check) fn targeted(
        &mut self,
        lists: &[AttrList],
        pragma: PragmaOptions,
        span: Span,
    ) -> Option<(Isa, bool)> {
        let x86 = match self.cx.target.tuple.arch().as_str() {
            "x86_64" => true,
            "aarch64" => false,
            _ => return None,
        };
        let ast = self.ast;
        let mut target = Target::new();
        let mut arm = self.cx.isa;
        let (mut said, mut refused) = (false, false);
        // What `#pragma GCC target` has in effect goes ahead of the attribute's strings, which is
        // where gcc puts it. A name in it gcc does not know is refused once, the way gcc refuses
        // the line once, and the lines are then left out rather than the function refused.
        if let Some(id) = pragma.target {
            let text = self.pragma_text(id);
            let read = if x86 {
                target.read(&text)
            } else {
                arm = arm.aarch64_target(&text);
                Ok(())
            };
            match read {
                Ok(()) => said = true,
                Err(why) => {
                    let what = why.to_string();
                    if self.refused_pragmas.insert(what.clone()) {
                        self.report(Diagnostic::error(what, span).with_code("E0720"));
                    }
                    (target, arm) = (Target::new(), self.cx.isa);
                }
            }
        }
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
        (said && !refused).then(|| {
            if x86 {
                (target.over(self.cx.isa), target.x87(self.cx.x87))
            } else {
                (arm, self.cx.x87)
            }
        })
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
    /// Three attributes change that and all three are read here, in the order they compose.
    /// `mode` picks a different scalar, `hardbool` makes a boolean of an integer and `vector_size`
    /// makes lanes of a scalar, so a declaration carrying them wants the mode applied first, the
    /// boolean made of what it gave and the lanes counted last. Everything that reads a declared
    /// type reads it through here, so none of them can be missed at one of the three places a type
    /// is declared.
    pub(in crate::check) fn retyped(&mut self, ty: TypeId, attrs: AttrList) -> TypeId {
        let ty = self.moded(ty, attrs);
        let ty = self.hardened(ty, attrs);
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
    ///
    /// `nocf_check` is a function type attribute too and lands where a convention does, so it is
    /// read here as well, after the convention. See [`Self::untracked`]. So is `indirect_return`,
    /// which is [`FunctionType::indirect_return`], `sseregparm`, which is
    /// [`FunctionType::sse_regparm`], and `callee_pop_aggregate_return` last, which is
    /// [`FunctionType::return_pointer_popped`].
    pub(in crate::check) fn convened(&mut self, ty: TypeId, attrs: AttrList) -> TypeId {
        let ty = match self.convention_in(attrs) {
            Some((convention, name, span)) => self.with_convention(ty, convention, &name, span),
            None => ty,
        };
        let ty = match self.nocf_in(attrs) {
            Some(span) => self.without_landing_pad(ty, span),
            None => ty,
        };
        let ty = match self.flag_in(attrs, "indirect_return", "E0823") {
            Some(span) => self.returning_by_jump(ty, span),
            None => ty,
        };
        let ty = self.sse_registers(ty, attrs, false);
        self.popping(ty, attrs, false)
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
        let mut pointee = pointee;
        if let Some((convention, name, span)) = self.convention_in(attrs) {
            match self.types.kind(self.types.canonical(pointee)) {
                TypeKind::Function(function) => pointee = self.function_under(function, convention),
                _ => self.not_a_function(&name, span),
            }
        }
        if let Some(span) = self.nocf_in(attrs) {
            match self.types.kind(self.types.canonical(pointee)) {
                TypeKind::Function(function) => {
                    pointee = self.untracked(function, span).unwrap_or(pointee);
                }
                _ => self.not_a_function("nocf_check", span),
            }
        }
        if let Some(span) = self.flag_in(attrs, "indirect_return", "E0823") {
            match self.types.kind(self.types.canonical(pointee)) {
                TypeKind::Function(function) => pointee = self.jumping_back(function),
                _ => self.not_a_function("indirect_return", span),
            }
        }
        let pointee = self.sse_registers(pointee, attrs, true);
        self.popping(pointee, attrs, true)
    }

    /// Where an attribute list says `nocf_check`, after refusing each one written with arguments
    /// in gcc's words.
    ///
    /// Read on x86 alone, which is the one family with landing pads and the `notrack` prefix, and
    /// the one gcc knows the attribute on.
    fn nocf_in(&mut self, attrs: AttrList) -> Option<Span> {
        self.flag_in(attrs, "nocf_check", "E0816")
    }

    /// Where an attribute list says `name`, an x86 function type attribute that takes nothing,
    /// after refusing each one written with arguments in gcc's words, under `code`.
    fn flag_in(&mut self, attrs: AttrList, name: &str, code: &'static str) -> Option<Span> {
        if !matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") {
            return None;
        }
        let ast = self.ast;
        let mut asked = None;
        for &attr in &ast[attrs] {
            if self.gnu_name(&attr) != name {
                continue;
            }
            let count = self.ast[attr.args].len();
            if count > 0 {
                let what = format!("wrong number of arguments specified for '{name}' attribute");
                let refused = Diagnostic::error(what, attr.span).with_code(code);
                self.report(refused.note(format!("expected 0, found {count}"), attr.span));
                continue;
            }
            asked = asked.or(Some(attr.span));
        }
        asked
    }

    /// What gcc says about `cf_check` in the lists of one declaration of `kind`: an argument is
    /// refused in gcc's words, and on anything but a function the attribute is dropped with its
    /// warning. Whether the function opens with a landing pad under `-mmanual-endbr` is read with
    /// the inlining flags, as [`DeclFlags::CF_CHECK`].
    ///
    /// Read on x86 alone, for the reason [`Self::nocf_in`] is.
    pub(in crate::check) fn cf_checked(&mut self, lists: &[AttrList], kind: DeclKind) {
        if !matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") {
            return;
        }
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                    || self.gnu_name(&attr) != "cf_check"
                {
                    continue;
                }
                let count = ast[attr.args].len();
                if count > 0 {
                    let what = "wrong number of arguments specified for 'cf_check' attribute";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0817");
                    self.report(refused.note(format!("expected 0, found {count}"), attr.span));
                } else if kind != DeclKind::Function {
                    let what = "'cf_check' attribute only applies to functions";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
    }

    /// What gcc says about `expected_throw` in the lists of one declaration of `kind`: an argument
    /// is refused in gcc's words, and on anything but a function the attribute is dropped with its
    /// warning.
    ///
    /// gcc 14 reads it only under `-fharden-control-flow-redundancy`, which checks the path taken
    /// before a call to such a function, since the call is expected to throw past the check at the
    /// end. This compiler has no such hardening, so on a function it says nothing and does nothing.
    pub(in crate::check) fn expected_throws(&mut self, lists: &[AttrList], kind: DeclKind) {
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.gnu_name(&attr) != "expected_throw" {
                    continue;
                }
                let count = ast[attr.args].len();
                if count > 0 {
                    let what = "wrong number of arguments specified for 'expected_throw' attribute";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0843");
                    self.report(refused.note(format!("expected 0, found {count}"), attr.span));
                } else if kind != DeclKind::Function {
                    let what = "'expected_throw' attribute ignored";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
    }

    /// What gcc says about `force_align_arg_pointer` in the lists of one declaration whose type is
    /// `ty`: an argument is refused in gcc's words, and on anything whose type is neither a
    /// function nor a pointer to one the attribute is dropped with its warning. gcc puts it on the
    /// function type, so a typedef of one and a pointer to one take it without a word. What it
    /// does to the frame is read with the inlining flags, as [`DeclFlags::FORCE_ALIGN`].
    ///
    /// Read on x86 alone, where gcc has it.
    pub(in crate::check) fn force_aligned(&mut self, lists: &[AttrList], ty: TypeId) {
        if !matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") {
            return;
        }
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                    || self.gnu_name(&attr) != "force_align_arg_pointer"
                {
                    continue;
                }
                let count = ast[attr.args].len();
                let pointee = rucc_types::pointee(&self.types, ty);
                let function = is_function(&self.types, ty)
                    || pointee.is_some_and(|to| is_function(&self.types, to));
                if count > 0 {
                    let what = "wrong number of arguments specified for 'force_align_arg_pointer' attribute";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0831");
                    self.report(refused.note(format!("expected 0, found {count}"), attr.span));
                } else if !function {
                    let what = "'force_align_arg_pointer' attribute only applies to function types";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
    }

    /// What gcc says about `ms_hook_prologue` in the lists of one declaration, which is whether a
    /// function is what it is written on: an argument is refused in gcc's words, and on anything
    /// else, a pointer to a function and a typedef of a function type included, the attribute is
    /// dropped with its warning. What it does to the function is read with the inlining flags, as
    /// [`DeclFlags::MS_HOOK`].
    ///
    /// Read on x86 alone, where gcc has it.
    pub(in crate::check) fn ms_hooked(&mut self, lists: &[AttrList], function: bool) {
        if !matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") {
            return;
        }
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                    || self.gnu_name(&attr) != "ms_hook_prologue"
                {
                    continue;
                }
                let count = ast[attr.args].len();
                if count > 0 {
                    let what =
                        "wrong number of arguments specified for 'ms_hook_prologue' attribute";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0834");
                    self.report(refused.note(format!("expected 0, found {count}"), attr.span));
                } else if !function {
                    let what = "'ms_hook_prologue' attribute only applies to functions";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
    }

    /// What gcc says about `no_reorder` in the lists of one declaration: an argument is refused in
    /// gcc's words, and where `top` is false, which is a member, a typedef or a parameter, the
    /// attribute is dropped with its warning. Any variable or function is `top`, the automatic
    /// variable in a block included, which gcc lets through without a word and does nothing with.
    /// Where the declaration is written is read with the inlining flags, as
    /// [`DeclFlags::NO_REORDER`].
    pub(in crate::check) fn no_reordered(&mut self, lists: &[AttrList], top: bool) {
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                    || self.gnu_name(&attr) != "no_reorder"
                {
                    continue;
                }
                let count = ast[attr.args].len();
                if count > 0 {
                    let what = "wrong number of arguments specified for 'no_reorder' attribute";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0832");
                    self.report(refused.note(format!("expected 0, found {count}"), attr.span));
                } else if !top {
                    let what = "'no_reorder' attribute only affects top level objects";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
    }

    /// Records what `fentry_name("hook")` and `fentry_section("section")` ask of the call `-pg`
    /// writes into a function: the hook it goes to, in place of `__fentry__` or `mcount`, and the
    /// section its address is listed in, which lists it whether or not `-mrecord-mcount` asked.
    /// Both are x86 attributes, read on an x86 target only, as gcc reads them.
    ///
    /// The wrong number of arguments is refused, on whatever it is written on. Anything else gcc
    /// cannot use it ignores with a warning: a variable or a type, and an argument that is not a
    /// string. An empty string is ignored the same way here, where gcc writes a call to nothing
    /// and leaves the assembler to refuse it. The second of two on a declaration counts, and a
    /// later declaration over an earlier one, the definition included, since gcc reads them once
    /// the file is done. `decl` is `None` for a typedef, which has nothing to record them on.
    pub(in crate::check) fn record_fentry(
        &mut self,
        decl: Option<DeclId>,
        lists: &[AttrList],
        kind: DeclKind,
    ) {
        if self.cx.target.tuple.arch().as_str() != "x86_64" {
            return;
        }
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                let name = self.gnu_name(&attr);
                if !matches!(name, "fentry_name" | "fentry_section") {
                    continue;
                }
                let args = ast[attr.args].to_vec();
                if args.len() != 1 {
                    let what =
                        format!("wrong number of arguments specified for '{name}' attribute");
                    let note = format!("expected 1, found {}", args.len());
                    let refused = Diagnostic::error(what, attr.span).with_code("E0822");
                    self.report(refused.note(note, attr.span));
                    continue;
                }
                let string = match args[0] {
                    AttrArg::Expr(expr) => {
                        let checked = self.expr(expr);
                        match self.tast[checked].kind {
                            ExprKind::Str(id)
                                if self.tast[id].encoding == Encoding::Plain
                                    && !self.tast[id].elements.is_empty() =>
                            {
                                Some(id)
                            }
                            _ => None,
                        }
                    }
                    AttrArg::Ident(_) => None,
                };
                let (Some(decl), Some(id), DeclKind::Function) = (decl, string, kind) else {
                    let what = format!("'{name}' attribute ignored");
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                };
                if name == "fentry_name" {
                    self.tast.record_fentry_name(decl, id);
                } else {
                    self.tast.record_fentry_section(decl, id);
                }
            }
        }
    }

    /// Records the versioned names `symver("name@node")` asks for, which are second names of a
    /// function or an object with a symbol version in their spelling, as gas reads them in a
    /// `.symver`. Whether the declaration may have them is settled where the symbols are, since
    /// the definition may be below it, which is where the IR is built.
    ///
    /// Checked in gcc's words and in gcc's order. The wrong number of arguments is refused on
    /// whatever it is written on. On anything but a function or a variable it is ignored with a
    /// warning, `decl` being `None` there, and on an object in a frame with another, since it has
    /// no symbol. An argument that is not a string, or a string without one or two `@` in it, is
    /// refused, and so is one gas would refuse, which gcc leaves to gas. Only the first argument
    /// of one attribute counts, which is what gcc does with the others. An object format other
    /// than ELF has no symbol versions, and gcc refuses the attribute there.
    pub(in crate::check) fn record_symver(
        &mut self,
        decl: Option<DeclId>,
        lists: &[AttrList],
        duration: StorageDuration,
        span: Span,
    ) {
        let ast = self.ast;
        let mut names = Vec::new();
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.gnu_name(&attr) != "symver" {
                    continue;
                }
                let args = ast[attr.args].to_vec();
                if args.is_empty() {
                    let what = "wrong number of arguments specified for 'symver' attribute";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0828");
                    self.report(refused.note("expected 1 or more, found 0", attr.span));
                    continue;
                }
                if decl.is_none() {
                    let what = "'symver' attribute only applies to functions and variables";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                }
                if duration == StorageDuration::Automatic {
                    let what = "'symver' attribute is only applicable to symbols";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                }
                let mut spellings = Vec::new();
                for arg in args {
                    let string = match arg {
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
                    let Some(id) = string else {
                        let what = "'symver' attribute argument not a string constant";
                        self.report(Diagnostic::error(what, attr.span).with_code("E0829"));
                        spellings.clear();
                        break;
                    };
                    let spelled: String = self.tast[id]
                        .elements
                        .iter()
                        .filter_map(|&unit| char::from_u32(unit))
                        .collect();
                    if !matches!(spelled.matches('@').count(), 1 | 2) {
                        let what = "symver attribute argument must have format 'name@nodename'";
                        self.report(Diagnostic::error(what, attr.span).with_code("E0829"));
                        let what = format!(
                            "'symver' attribute argument '{spelled}' must contain one or two '@'"
                        );
                        self.report(Diagnostic::error(what, attr.span).with_code("E0829"));
                        spellings.clear();
                        break;
                    }
                    spellings.push(spelled);
                }
                let Some(spelled) = spellings.into_iter().next() else { continue };
                if !is_versioned_name(&spelled) {
                    let what = format!(
                        "'symver' attribute argument '{spelled}' is not a name, an '@' or '@@' \
                         and a version node"
                    );
                    self.report(Diagnostic::error(what, attr.span).with_code("E0829"));
                    continue;
                }
                if self.cx.target.object_format != ObjectFormat::Elf {
                    let what = "symver is only supported on ELF platforms";
                    self.report(Diagnostic::error(what, span).with_code("E0830"));
                    return;
                }
                names.push(spelled);
            }
        }
        if let (Some(decl), false) = (decl, names.is_empty()) {
            self.tast.record_symvers(decl, names, span);
        }
    }

    /// The type with its function, or the function it points at, made one without a landing pad,
    /// which is where [`Self::with_convention`] puts a convention and for the same reason.
    fn without_landing_pad(&mut self, ty: TypeId, span: Span) -> TypeId {
        let canonical = self.types.canonical(ty);
        match self.types.kind(canonical) {
            TypeKind::Function(function) => self.untracked(function, span).unwrap_or(ty),
            TypeKind::Pointer(pointee) => {
                let pointee = self.types.canonical(pointee);
                let TypeKind::Function(function) = self.types.kind(pointee) else {
                    self.not_a_function("nocf_check", span);
                    return ty;
                };
                let Some(function) = self.untracked(function, span) else { return ty };
                let quals = self.types.quals(canonical);
                let pointer = self.types.pointer(function);
                self.types.qualified(pointer, quals)
            }
            _ => {
                self.not_a_function("nocf_check", span);
                ty
            }
        }
    }

    /// The type with its function, or the function it points at, made one whose call can come
    /// back by a jump, as [`Self::without_landing_pad`] does for `nocf_check`. Anything else is
    /// left as it is with gcc's warning, which is the one diagnostic: gcc reads the attribute
    /// without `-fcf-protection` in silence, though it then does nothing.
    fn returning_by_jump(&mut self, ty: TypeId, span: Span) -> TypeId {
        let canonical = self.types.canonical(ty);
        match self.types.kind(canonical) {
            TypeKind::Function(function) => self.jumping_back(function),
            TypeKind::Pointer(pointee) => {
                let pointee = self.types.canonical(pointee);
                let TypeKind::Function(function) = self.types.kind(pointee) else {
                    self.not_a_function("indirect_return", span);
                    return ty;
                };
                let function = self.jumping_back(function);
                let quals = self.types.quals(canonical);
                let pointer = self.types.pointer(function);
                self.types.qualified(pointer, quals)
            }
            _ => {
                self.not_a_function("indirect_return", span);
                ty
            }
        }
    }

    /// The same function type with [`FunctionType::indirect_return`] set.
    fn jumping_back(&mut self, function: FunctionId) -> TypeId {
        let current = self.types.signature(function);
        let signature = FunctionType { indirect_return: true, ..current.clone() };
        self.types.function(signature)
    }

    /// The same function type without a landing pad, or nothing where the type stays as written.
    ///
    /// That is a function already without one, and every function when no option asked for the
    /// pads, which gcc warns about and drops: a function with no pad to leave out is an ordinary
    /// function, and a call through a pointer to one has nothing to skip the check of. The
    /// warning comes after the one about a type that is not a function, as gcc's does, and the
    /// attribute is the type's for the reason a convention is, which [`FunctionType::nocf`] says.
    fn untracked(&mut self, function: FunctionId, span: Span) -> Option<TypeId> {
        if !self.cx.landing_pads {
            let what = "'nocf_check' attribute ignored. Use '-fcf-protection' option to enable it";
            self.report(Diagnostic::warning(what, span).with_code("E0703"));
            return None;
        }
        let current = self.types.signature(function);
        if current.nocf {
            return None;
        }
        let signature = FunctionType { nocf: true, ..current.clone() };
        Some(self.types.function(signature))
    }

    /// The convention an attribute list names, with the name that named it and where, after
    /// saying whatever there is to say about the convention attributes in it.
    ///
    /// Three things are said. `ms_abi` or `sysv_abi` on a target other than x86-64 is ignored with
    /// a warning, since there is no second convention there to pick. One of each in the same list
    /// is refused, as gcc refuses it, because a function cannot be both, and the same goes for two
    /// of `stdcall`, `fastcall` and `cdecl` on 32-bit x86, where those are conventions, and for
    /// `fastcall` with `regparm`. And `stdcall`, `cdecl`, `fastcall`, `thiscall`, `regparm` and
    /// `vectorcall` are ignored with a warning on x86-64 outside Windows. The same name twice is
    /// the one convention asked for twice, which is no conflict.
    fn convention_in(&mut self, attrs: AttrList) -> Option<(Convention, String, Span)> {
        let written = self.ast[attrs].to_vec();
        let tuple = self.cx.target.tuple;
        let windows = tuple.os().as_str() == "windows";
        // Where gcc says the 32-bit Windows conventions are ignored. On 32-bit x86 they mean
        // something and on Windows gcc accepts them without a word, and neither is this.
        let foreign_to_them = tuple.arch().as_str() == "x86_64" && !windows;
        let regparm_here = tuple.arch().as_str() == "i686" && !windows;
        let mut asked: Option<(Convention, String, Span)> = None;
        // A `regparm` count is kept apart until the list is read, since outside Windows it goes
        // with `stdcall` rather than against it, and `fastcall` refuses it whichever comes first.
        let mut regparm: Option<(u8, Span)> = None;
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
                // The three 32-bit x86 has, where each is a convention of its own and `cdecl` is
                // the target's. Two different ones in a list are refused as gcc refuses them.
                "stdcall" | "cdecl" | "fastcall" if Convention::asked(tuple, &name).is_some() => {
                    let convention = Convention::asked(tuple, &name).unwrap_or_default();
                    match &asked {
                        Some((_, first, _)) if *first != name => {
                            let what =
                                format!("'{first}' and '{name}' attributes are not compatible");
                            let refused = Diagnostic::error(what, attr.span);
                            self.report(refused.with_code("E0740"));
                        }
                        Some(_) => {}
                        None => asked = Some((convention, name, attr.span)),
                    }
                }
                // How many words go in registers, which outside Windows is the only convention
                // 32-bit x86 has a choice of. A count that is the unit's own is the target's
                // convention, so the attribute written to match `-mregparm=` changes nothing.
                "regparm" if regparm_here => {
                    let Some(registers) = self.regparm_argument(&attr) else { continue };
                    regparm = Some((registers, attr.span));
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
        let Some((registers, span)) = regparm else { return asked };
        let own = registers == self.cx.target.regparm;
        match asked {
            None | Some((Convention::Target, ..)) => {
                let convention =
                    if own { Convention::Target } else { Convention::Regparm(registers) };
                Some((convention, "regparm".to_owned(), span))
            }
            Some((Convention::Fastcall, ..)) => {
                let what = "fastcall and regparm attributes are not compatible";
                self.report(Diagnostic::error(what.to_owned(), span).with_code("E0740"));
                asked
            }
            // A `stdcall` function takes as many words in registers as the unit gives every
            // function, so a count that is the unit's own says nothing more. Another count is a
            // convention gcc has and this compiler does not yet, and it is refused rather than
            // compiled as the wrong one.
            Some((Convention::Stdcall, ..)) if !own => {
                let what = format!(
                    "'stdcall' with 'regparm({registers})' in a unit whose count is {} is not \
                     supported yet",
                    self.cx.target.regparm
                );
                let note = "drop the 'regparm', or build the unit with the same '-mregparm='";
                let refused = Diagnostic::error(what, span).with_code("E0519");
                self.report(refused.note(note, span));
                asked
            }
            Some(_) => asked,
        }
    }

    /// The count a `regparm` attribute gives, or nothing where gcc warns and drops it: an
    /// argument that is not one integer constant, or one above three.
    fn regparm_argument(&mut self, attr: &Attribute) -> Option<u8> {
        let args = self.ast[attr.args].to_vec();
        let number = match args.as_slice() {
            [AttrArg::Expr(expr)] => {
                let value = self.expr(*expr);
                self.eval_integer(value).ok()
            }
            _ => None,
        };
        let what = match number {
            None => "'regparm' attribute requires an integer constant argument".to_owned(),
            Some(number) => match u8::try_from(number) {
                Ok(registers) if registers <= 3 => return Some(registers),
                _ => "argument to 'regparm' attribute larger than 3".to_owned(),
            },
        };
        self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
        None
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
    ///
    /// A variadic function asked to be `stdcall` or `fastcall` stays the target's own, which is
    /// what gcc makes of it without a word: the callee cannot take off the stack what only the
    /// caller knows it pushed.
    ///
    /// A variadic function asked for `regparm` is the target's too, and that is as far as the type
    /// goes: gcc passes every argument of one on the stack whatever the unit's `-mregparm=` says,
    /// and that is worked out where the call is planned, see
    /// `rucc_target::TargetInfo::convention_for`.
    fn function_under(&mut self, function: FunctionId, convention: Convention) -> TypeId {
        let current = self.types.signature(function);
        let convention = if (convention.callee_pops()
            || matches!(convention, Convention::Regparm(_)))
            && current.variadic
        {
            Convention::Target
        } else {
            convention
        };
        let signature = FunctionType { convention, ..current.clone() };
        self.types.function(signature)
    }

    /// gcc's warning for a convention written on something that is not a function.
    pub(in crate::check) fn not_a_function(&mut self, name: &str, span: Span) {
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
        let boolean = matches!(eval::bare(&self.types, elem), TypeKind::Bool)
            || self.types.hardbool_of(elem).is_some();
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

    /// The value of an enumerator an attribute's lone identifier names, and nothing when it
    /// names anything else.
    ///
    /// The parser keeps a lone identifier as one rather than as an expression, since most of the
    /// attributes that take one name a thing outside the ordinary scope. The ones that take a
    /// number take an enumerator as well, and gcc finds it the way an expression would.
    pub(in crate::check) fn enumerator(&self, name: Symbol) -> Option<i128> {
        match self.scopes.lookup(name)? {
            Binding::Enumerator { value, .. } => Some(value),
            Binding::Decl(_) | Binding::Typedef(_) => None,
        }
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
            // `aligned(A)` with `A` an enumerator, which the parser keeps as an identifier
            // because `format(printf, 1, 2)` does. The kernel's blake2s selftest aligns a
            // buffer with an enumerator of its own block.
            Some(&AttrArg::Ident(name)) => {
                let Some(value) = self.enumerator(name) else {
                    let what = "requested alignment is not an integer constant";
                    self.report(Diagnostic::error(what, attr.span).with_code("E0606"));
                    return None;
                };
                value
            }
        };
        // Zero is a warning and the attribute is dropped, as gcc 13 has it for both attributes
        // that read their number here, where any other number that is not a power of two is an
        // error.
        if requested == 0 {
            let what = "requested alignment '0' is not a positive power of 2";
            self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            return None;
        }
        if requested < 0 || requested & (requested - 1) != 0 {
            let what = format!("requested alignment '{requested}' is not a positive power of 2");
            self.report(Diagnostic::error(what, attr.span).with_code("E0607"));
            return None;
        }
        u32::try_from(requested).ok()
    }

    /// What `warn_if_not_aligned` in these lists asked a member of the type to sit at a multiple
    /// of, and where it was written.
    ///
    /// The number is read the way `aligned` reads its own, so written bare it is gcc's biggest
    /// alignment, zero is warned about and dropped, and anything else that is not a power of two
    /// is refused. More than one argument is refused with `E0827`. Where it is written twice the
    /// last one counts.
    pub(in crate::check) fn warn_alignment(
        &mut self,
        lists: &[AttrList],
    ) -> Option<(NonZeroU32, Span)> {
        let ast = self.ast;
        let mut asked = None;
        for &list in lists {
            for &attr in &ast[list] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                    || self.gnu_name(&attr) != "warn_if_not_aligned"
                {
                    continue;
                }
                let count = ast[attr.args].len();
                if count > 1 {
                    let what =
                        "wrong number of arguments specified for 'warn_if_not_aligned' attribute";
                    let note = format!("expected between 0 and 1, found {count}");
                    let refused = Diagnostic::error(what, attr.span).with_code("E0827");
                    self.report(refused.note(note, attr.span));
                    continue;
                }
                if let Some(align) = self.aligned_argument(attr).and_then(NonZeroU32::new) {
                    asked = Some((align, attr.span));
                }
            }
        }
        asked
    }

    /// `warn_if_not_aligned` on the declaration of an object or a function, which gcc refuses in
    /// these words: it is about where a member sits, so only a type or a member can say it.
    pub(in crate::check) fn warn_alignment_misplaced(&mut self, lists: &[AttrList], name: Symbol) {
        if let Some((_, at)) = self.warn_alignment(lists) {
            let what =
                format!("'warn_if_not_aligned' may not be specified for '{}'", self.text(name));
            self.report(Diagnostic::error(what, at).with_code("E0825"));
        }
    }
}

/// Whether a `symver` string is one gas takes: a name, then `@` or `@@`, then a version node,
/// which may be empty. `f@` is how a version script is asked for the base version.
fn is_versioned_name(spelled: &str) -> bool {
    let name = |part: &str| {
        part.chars().next().is_some_and(|first| !first.is_ascii_digit())
            && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$'))
    };
    let Some((bare, node)) = spelled.split_once('@') else { return false };
    let node = node.strip_prefix('@').unwrap_or(node);
    name(bare) && (node.is_empty() || name(node))
}

/// What one option of an `optimize` attribute or a `#pragma GCC optimize` line adds to `flags`.
///
/// An option is written with or without its `-f`. `no-stack-protector` here is what the kernel's
/// `__nostackprotector` was before gcc 11 had the attribute, and what it still falls back to on a
/// compiler that says it does not have it.
fn optimizing(mut flags: DeclFlags, option: &str) -> DeclFlags {
    match option.trim_start_matches('-').trim_start_matches('f') {
        "no-strict-aliasing" => flags |= DeclFlags::NO_STRICT_ALIASING,
        "no-stack-protector" => flags = flags.then(DeclFlags::NO_STACK_PROTECTOR),
        // `-O0` for this one function, which is the level a function can be held to on its own:
        // the passes that run at `-O0` are a subset of the ones at every other level, so the
        // pipeline can leave the rest out for one body without anything else in the unit
        // noticing. A higher level than the unit's would mean running passes the unit never asked
        // for, and those levels are accepted and ignored.
        "O0" => flags |= DeclFlags::OPTIMIZE_NONE,
        // The two options the kernel writes on a function that change what the body is allowed to
        // become rather than how fast it is. `wrapv` is `-fwrapv` for the one body, and the other
        // is what keeps a freestanding `memset` from being compiled into a call to itself.
        "wrapv" => flags |= DeclFlags::WRAPV,
        "no-tree-loop-distribute-patterns" => flags |= DeclFlags::NO_LOOP_IDIOM,
        _ => {}
    }
    flags
}
