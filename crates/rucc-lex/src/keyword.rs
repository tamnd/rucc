//! Keywords, and which ones the dialect has.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.1.
//!
//! Phase 7 turns an identifier into a keyword when the active `-std=` says that spelling is
//! one. Doing that with a string comparison, or with a hash lookup on the text, would put a
//! second pass over every identifier in the file right after the scan that already interned
//! it. So the keywords are interned first, before anything else, which makes their symbols one
//! contiguous run at the bottom of the table. Recognition is then a subtraction, a bounds
//! check and a byte load, and every identifier a program actually declares fails the bounds
//! check on the first instruction.
//!
//! The dialect gate is part of the same load. Whether a spelling is a keyword depends on the
//! dialect, and the dialect is fixed for the whole compilation, so [`Keywords::new`] resolves
//! it once: each entry holds the keyword it means in this dialect, or nothing when the
//! spelling is an ordinary identifier here. `restrict` is a keyword from C99 and a variable
//! name in C89, `typeof` is one in C23 and in the GNU dialects and not in `-std=c17`, and
//! `__typeof__` is one everywhere, which is why headers are written with the ugly spelling.
//!
//! Which spelling is a keyword in which dialect was measured rather than read out of the
//! standard, because the standard does not describe the GNU dialects and the underscore
//! spellings are on in dialects that predate them. Every identifier below was compiled as
//! `void f(void) { int KW = 0; (void)KW; }` against gcc 13.3 on x86-64 Linux and against
//! clang, in each of c89, gnu89, c99, gnu99, c11, gnu11, c17, gnu17, c23 and gnu23, with two
//! ordinary identifiers along for the ride to catch a probe that had stopped measuring
//! anything. The two compilers agree except where noted.
//!
//! Three differences from gcc 13.3, one of which gcc 16 has since closed:
//!
//! `_BitInt` is a keyword here in every dialect. That was a difference from gcc 13.3, which
//! does not have the type at all, and is not one from gcc 16: the type arrived in gcc 14, the
//! spelling is a keyword there in every dialect, and `-pedantic` warns about the type before
//! C23 rather than about the spelling. clang does the same. It is in the reserved namespace,
//! so nothing legal can notice.
//!
//! `__float128` and `__bf16` are not keywords. gcc registers them as predefined type names,
//! which a declaration is allowed to shadow, and `void f(void) { int __float128 = 0; }`
//! compiles there. clang makes both of them keywords and rejects it. We follow gcc, so they
//! belong with the other predefined types rather than here.
//!
//! gcc also reserves `_Sat`, `_Fract`, `_Accum`, `__seg_fs` and `__seg_gs` in the GNU
//! dialects. They are left out until the fixed point types and the named address spaces are
//! implemented, because a keyword the parser can only refuse is worse for a program than an
//! identifier it can at least read.

use rucc_base::{Interner, Symbol};
use rucc_session::Std;

/// A keyword, meaning a spelling the grammar knows rather than a name a program chose.
///
/// One variant per meaning, not per spelling. `__inline__` and `inline` are the same keyword
/// because they are the same declaration specifier, and a parser that had to know which of
/// them was written would be carrying the difference all the way to the AST for nothing.
/// Where two spellings mean genuinely different things they stay apart: `__alignof__` is
/// [`Keyword::GnuAlignof`] rather than [`Keyword::Alignof`], because GNU's asks for the
/// alignment the target prefers and C's asks for the one the ABI requires, and on i386 they
/// disagree about `double`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Keyword {
    /// `auto`.
    Auto,
    /// `break`.
    Break,
    /// `case`.
    Case,
    /// `char`.
    Char,
    /// `const`, and the GNU spelling `__const`.
    Const,
    /// `continue`.
    Continue,
    /// `default`.
    Default,
    /// `do`.
    Do,
    /// `double`.
    Double,
    /// `else`.
    Else,
    /// `enum`.
    Enum,
    /// `extern`.
    Extern,
    /// `float`.
    Float,
    /// `for`.
    For,
    /// `goto`.
    Goto,
    /// `if`.
    If,
    /// `int`.
    Int,
    /// `long`.
    Long,
    /// `register`.
    Register,
    /// `return`.
    Return,
    /// `short`.
    Short,
    /// `signed`, and the GNU spelling `__signed__`.
    Signed,
    /// `sizeof`.
    Sizeof,
    /// `static`.
    Static,
    /// `struct`.
    Struct,
    /// `switch`.
    Switch,
    /// `typedef`.
    Typedef,
    /// `union`.
    Union,
    /// `unsigned`.
    Unsigned,
    /// `void`.
    Void,
    /// `volatile`, and the GNU spelling `__volatile__`.
    Volatile,
    /// `while`.
    While,
    /// `inline`, from C99, and the GNU spelling `__inline__`.
    Inline,
    /// `restrict`, from C99, and the GNU spelling `__restrict__`.
    Restrict,
    /// `_Bool`, and `bool` from C23.
    Bool,
    /// `_Complex`, and the GNU spelling `__complex__`.
    Complex,
    /// `_Imaginary`.
    Imaginary,
    /// `_Alignas`, and `alignas` from C23.
    Alignas,
    /// `_Alignof`, and `alignof` from C23.
    Alignof,
    /// `_Atomic`.
    Atomic,
    /// `_Generic`.
    Generic,
    /// `_Noreturn`.
    Noreturn,
    /// `_Static_assert`, and `static_assert` from C23.
    StaticAssert,
    /// `_Thread_local`, `thread_local` from C23, and the GNU spelling `__thread`.
    ThreadLocal,
    /// `_BitInt`.
    BitInt,
    /// `_Decimal32`.
    Decimal32,
    /// `_Decimal64`.
    Decimal64,
    /// `_Decimal128`.
    Decimal128,
    /// `_Float16`.
    Float16,
    /// `_Float32`.
    Float32,
    /// `_Float64`.
    Float64,
    /// `_Float128`.
    Float128,
    /// `_Float32x`.
    Float32x,
    /// `_Float64x`.
    Float64x,
    /// `_Float128x`.
    Float128x,
    /// `constexpr`, from C23.
    Constexpr,
    /// `false`, from C23.
    False,
    /// `nullptr`, from C23.
    Nullptr,
    /// `true`, from C23.
    True,
    /// `typeof`, from C23 and from the GNU dialects, and the spelling `__typeof__`.
    Typeof,
    /// `typeof_unqual`, from C23, and the spelling `__typeof_unqual__`.
    TypeofUnqual,
    /// `asm`, in the GNU dialects, and the spelling `__asm__`.
    Asm,
    /// `__attribute__`.
    Attribute,
    /// `__auto_type`, which is not `auto`: it deduces from an initialiser in every dialect.
    AutoType,
    /// `__alignof__`, which asks for the preferred alignment rather than the required one.
    GnuAlignof,
    /// `__extension__`, which turns off the pedantic diagnostics for one expression.
    Extension,
    /// `__imag__`.
    Imag,
    /// `__real__`.
    Real,
    /// `__int128`.
    Int128,
    /// `__label__`, which declares a local label in a statement expression.
    Label,
    /// `__builtin_offsetof`, which is syntax rather than a function because it takes a type.
    BuiltinOffsetof,
    /// `__builtin_choose_expr`.
    BuiltinChooseExpr,
    /// `__builtin_types_compatible_p`.
    BuiltinTypesCompatibleP,
    /// `__builtin_va_arg`.
    BuiltinVaArg,
}

impl Keyword {
    /// The spelling to print in a diagnostic, which is the standard one where there is one.
    ///
    /// This walks the table, because it is only ever reached while writing a message and a
    /// second array indexed by the enum would be one more place for the two to disagree.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        KEYWORDS
            .iter()
            .find(|entry| entry.keyword == self)
            .map_or("keyword", |entry| entry.spelling)
    }
}

/// The keywords of one dialect, ready to be looked up by symbol.
///
/// Built once per compilation, against the interner that compilation will use, before any
/// source has been read.
#[derive(Debug)]
pub struct Keywords {
    /// The symbol of the first entry. Everything below this is not a keyword, and so is
    /// everything at or past the end of `active`.
    base: u32,
    /// The keyword each spelling means in this dialect, indexed by symbol minus `base`, and
    /// [`None`] for a spelling this dialect leaves as an ordinary identifier.
    active: Box<[Option<Keyword>]>,
}

impl Keywords {
    /// Interns every keyword spelling and resolves which of them this dialect has.
    ///
    /// # Panics
    ///
    /// Panics if `interner` has already been given one of these spellings, since the symbols
    /// would no longer be one run and every lookup after that would be wrong. Build this
    /// first, immediately after the interner itself.
    #[must_use]
    pub fn new(interner: &mut Interner, std: Std, gnu: bool) -> Keywords {
        let dialect = mask(std, gnu);
        let mut base = 0;
        let mut active = Vec::with_capacity(KEYWORDS.len());
        for entry in KEYWORDS {
            let symbol = interner.intern(entry.spelling).raw();
            if active.is_empty() {
                base = symbol;
            }
            let want = base + u32::try_from(active.len()).expect("the table is not that long");
            assert!(
                symbol == want,
                "`{}` was interned before the keyword table was built",
                entry.spelling
            );
            active.push((entry.dialects & dialect != 0).then_some(entry.keyword));
        }
        Keywords { base, active: active.into_boxed_slice() }
    }

    /// The keyword `symbol` is in this dialect, and [`None`] when it is an identifier.
    #[must_use]
    #[inline]
    pub fn get(&self, symbol: Symbol) -> Option<Keyword> {
        let index = symbol.raw().checked_sub(self.base)?;
        // A `usize` cast rather than a conversion: the index is already known to fit, because
        // the slice it indexes was built from symbols this interner handed out.
        *self.active.get(index as usize)?
    }

    /// Whether `symbol` is a keyword in this dialect.
    #[must_use]
    #[inline]
    pub fn contains(&self, symbol: Symbol) -> bool {
        self.get(symbol).is_some()
    }

    /// How many spellings the table holds, active in this dialect or not.
    #[must_use]
    pub fn len(&self) -> usize {
        self.active.len()
    }

    /// Whether the table is empty, which it never is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

/// One bit per dialect, plus one for the GNU extensions.
const C89: u8 = 1 << 0;
const C99: u8 = 1 << 1;
const C11: u8 = 1 << 2;
const C17: u8 = 1 << 3;
const C23: u8 = 1 << 4;
const GNU: u8 = 1 << 5;

/// A spelling that is a keyword in every dialect, GNU or not.
const ALWAYS: u8 = C89 | C99 | C11 | C17 | C23 | GNU;
/// From C99 onwards, and not in `-std=gnu89`. This is `restrict`, and it is the one place the
/// GNU dialects are not a superset: gcc and clang both keep `restrict` out of `gnu89` and
/// offer `__restrict` there instead.
const SINCE_C99: u8 = C99 | C11 | C17 | C23;
/// From C99 onwards, and in every GNU dialect including `gnu89`. This is `inline`.
const SINCE_C99_OR_GNU: u8 = SINCE_C99 | GNU;
/// C23 only. The lowercase spellings of the C11 keywords are here, and so is the rest of what
/// C23 added, and `-std=gnu17` does not have any of them.
const SINCE_C23: u8 = C23;
/// C23, and every GNU dialect. This is `typeof`, which gcc has had for decades and which C23
/// standardised, so `-std=c17` is the only place it is a variable name.
const SINCE_C23_OR_GNU: u8 = C23 | GNU;
/// The GNU dialects only. This is `asm`, which is a keyword in `gnu23` and an identifier in
/// `c23`, where `__asm__` has to be written instead.
const GNU_ONLY: u8 = GNU;

/// A spelling, what it means, and where it is a keyword.
struct Entry {
    /// The spelling as it appears in source.
    spelling: &'static str,
    /// What the grammar makes of it.
    keyword: Keyword,
    /// The dialects it is a keyword in, as a mask of the bits above.
    dialects: u8,
}

/// Shorthand, so that the table below reads as a table rather than a page of struct literals.
const fn e(spelling: &'static str, keyword: Keyword, dialects: u8) -> Entry {
    Entry { spelling, keyword, dialects }
}

/// Every keyword spelling in every dialect we support.
///
/// The order is the interning order and so decides the symbols, which nothing may depend on;
/// it is grouped by where each spelling came from because that is how it is checked against a
/// compiler. The first entry for a keyword is the spelling [`Keyword::as_str`] prints.
static KEYWORDS: &[Entry] = &[
    // The C89 keywords. Nothing has ever removed one, so all of them are unconditional.
    e("auto", Keyword::Auto, ALWAYS),
    e("break", Keyword::Break, ALWAYS),
    e("case", Keyword::Case, ALWAYS),
    e("char", Keyword::Char, ALWAYS),
    e("const", Keyword::Const, ALWAYS),
    e("continue", Keyword::Continue, ALWAYS),
    e("default", Keyword::Default, ALWAYS),
    e("do", Keyword::Do, ALWAYS),
    e("double", Keyword::Double, ALWAYS),
    e("else", Keyword::Else, ALWAYS),
    e("enum", Keyword::Enum, ALWAYS),
    e("extern", Keyword::Extern, ALWAYS),
    e("float", Keyword::Float, ALWAYS),
    e("for", Keyword::For, ALWAYS),
    e("goto", Keyword::Goto, ALWAYS),
    e("if", Keyword::If, ALWAYS),
    e("int", Keyword::Int, ALWAYS),
    e("long", Keyword::Long, ALWAYS),
    e("register", Keyword::Register, ALWAYS),
    e("return", Keyword::Return, ALWAYS),
    e("short", Keyword::Short, ALWAYS),
    e("signed", Keyword::Signed, ALWAYS),
    e("sizeof", Keyword::Sizeof, ALWAYS),
    e("static", Keyword::Static, ALWAYS),
    e("struct", Keyword::Struct, ALWAYS),
    e("switch", Keyword::Switch, ALWAYS),
    e("typedef", Keyword::Typedef, ALWAYS),
    e("union", Keyword::Union, ALWAYS),
    e("unsigned", Keyword::Unsigned, ALWAYS),
    e("void", Keyword::Void, ALWAYS),
    e("volatile", Keyword::Volatile, ALWAYS),
    e("while", Keyword::While, ALWAYS),
    // The two C99 additions that are ordinary words. Everything else C99 and C11 added is
    // spelled with a leading underscore precisely so that it could be turned on in the
    // older dialects without breaking a program that had used the name, and both
    // compilers do exactly that.
    e("inline", Keyword::Inline, SINCE_C99_OR_GNU),
    e("restrict", Keyword::Restrict, SINCE_C99),
    e("_Bool", Keyword::Bool, ALWAYS),
    e("_Complex", Keyword::Complex, ALWAYS),
    e("_Imaginary", Keyword::Imaginary, ALWAYS),
    e("_Alignas", Keyword::Alignas, ALWAYS),
    e("_Alignof", Keyword::Alignof, ALWAYS),
    e("_Atomic", Keyword::Atomic, ALWAYS),
    e("_Generic", Keyword::Generic, ALWAYS),
    e("_Noreturn", Keyword::Noreturn, ALWAYS),
    e("_Static_assert", Keyword::StaticAssert, ALWAYS),
    e("_Thread_local", Keyword::ThreadLocal, ALWAYS),
    e("_BitInt", Keyword::BitInt, ALWAYS),
    e("_Decimal32", Keyword::Decimal32, ALWAYS),
    e("_Decimal64", Keyword::Decimal64, ALWAYS),
    e("_Decimal128", Keyword::Decimal128, ALWAYS),
    e("_Float16", Keyword::Float16, ALWAYS),
    e("_Float32", Keyword::Float32, ALWAYS),
    e("_Float64", Keyword::Float64, ALWAYS),
    e("_Float128", Keyword::Float128, ALWAYS),
    e("_Float32x", Keyword::Float32x, ALWAYS),
    e("_Float64x", Keyword::Float64x, ALWAYS),
    e("_Float128x", Keyword::Float128x, ALWAYS),
    // C23, which spelled the C11 keywords as words and added its own. A program that used
    // `bool` as a variable name still compiles in every earlier dialect, which is the
    // whole reason this table is gated rather than fixed.
    e("alignas", Keyword::Alignas, SINCE_C23),
    e("alignof", Keyword::Alignof, SINCE_C23),
    e("bool", Keyword::Bool, SINCE_C23),
    e("constexpr", Keyword::Constexpr, SINCE_C23),
    e("false", Keyword::False, SINCE_C23),
    e("nullptr", Keyword::Nullptr, SINCE_C23),
    e("static_assert", Keyword::StaticAssert, SINCE_C23),
    e("thread_local", Keyword::ThreadLocal, SINCE_C23),
    e("true", Keyword::True, SINCE_C23),
    e("typeof", Keyword::Typeof, SINCE_C23_OR_GNU),
    e("typeof_unqual", Keyword::TypeofUnqual, SINCE_C23),
    e("asm", Keyword::Asm, GNU_ONLY),
    // The GNU spellings. All of them are in the reserved namespace, so gcc turns them on
    // in every dialect including `-std=c89`, and a header that has to work under `-std=`
    // anything is written with these rather than with the words above.
    e("__asm", Keyword::Asm, ALWAYS),
    e("__asm__", Keyword::Asm, ALWAYS),
    e("__alignof", Keyword::GnuAlignof, ALWAYS),
    e("__alignof__", Keyword::GnuAlignof, ALWAYS),
    e("__attribute", Keyword::Attribute, ALWAYS),
    e("__attribute__", Keyword::Attribute, ALWAYS),
    e("__auto_type", Keyword::AutoType, ALWAYS),
    e("__complex", Keyword::Complex, ALWAYS),
    e("__complex__", Keyword::Complex, ALWAYS),
    e("__const", Keyword::Const, ALWAYS),
    e("__extension__", Keyword::Extension, ALWAYS),
    e("__imag", Keyword::Imag, ALWAYS),
    e("__imag__", Keyword::Imag, ALWAYS),
    e("__inline", Keyword::Inline, ALWAYS),
    e("__inline__", Keyword::Inline, ALWAYS),
    e("__int128", Keyword::Int128, ALWAYS),
    e("__label__", Keyword::Label, ALWAYS),
    e("__real", Keyword::Real, ALWAYS),
    e("__real__", Keyword::Real, ALWAYS),
    e("__restrict", Keyword::Restrict, ALWAYS),
    e("__restrict__", Keyword::Restrict, ALWAYS),
    e("__signed", Keyword::Signed, ALWAYS),
    e("__signed__", Keyword::Signed, ALWAYS),
    // gcc's own diagnostics keep `__thread` and `_Thread_local` apart, but in C they are
    // one storage class with two spellings, so the parser is given one keyword.
    e("__thread", Keyword::ThreadLocal, ALWAYS),
    e("__typeof", Keyword::Typeof, ALWAYS),
    e("__typeof__", Keyword::Typeof, ALWAYS),
    e("__typeof_unqual", Keyword::TypeofUnqual, ALWAYS),
    e("__typeof_unqual__", Keyword::TypeofUnqual, ALWAYS),
    e("__volatile", Keyword::Volatile, ALWAYS),
    e("__volatile__", Keyword::Volatile, ALWAYS),
    // The builtins that are syntax rather than functions, because an argument of theirs is
    // a type name. Everything else called `__builtin_` is an ordinary identifier that
    // resolves to a declaration, and belongs nowhere near this table.
    e("__builtin_offsetof", Keyword::BuiltinOffsetof, ALWAYS),
    e("__builtin_choose_expr", Keyword::BuiltinChooseExpr, ALWAYS),
    e("__builtin_types_compatible_p", Keyword::BuiltinTypesCompatibleP, ALWAYS),
    e("__builtin_va_arg", Keyword::BuiltinVaArg, ALWAYS),
];

/// The bits a dialect matches, which is its own and the GNU one when the extensions are on.
const fn mask(std: Std, gnu: bool) -> u8 {
    let dialect = match std {
        Std::C89 => C89,
        Std::C99 => C99,
        Std::C11 => C11,
        Std::C17 => C17,
        Std::C23 => C23,
    };
    if gnu { dialect | GNU } else { dialect }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keywords of one dialect, and an interner that has them and nothing else.
    fn build(std: Std, gnu: bool) -> (Keywords, Interner) {
        let mut interner = Interner::new();
        let keywords = Keywords::new(&mut interner, std, gnu);
        (keywords, interner)
    }

    /// What `text` means in this dialect, having been interned the way the scanner would.
    fn lookup(std: Std, gnu: bool, text: &str) -> Option<Keyword> {
        let (keywords, mut interner) = build(std, gnu);
        keywords.get(interner.intern(text))
    }

    #[test]
    fn a_word_the_language_has_always_had_is_a_keyword_in_every_dialect() {
        for std in [Std::C89, Std::C99, Std::C11, Std::C17, Std::C23] {
            for gnu in [false, true] {
                assert_eq!(lookup(std, gnu, "int"), Some(Keyword::Int));
                assert_eq!(lookup(std, gnu, "sizeof"), Some(Keyword::Sizeof));
                assert_eq!(lookup(std, gnu, "_Complex"), Some(Keyword::Complex));
            }
        }
    }

    #[test]
    fn a_name_a_program_chose_is_never_a_keyword() {
        // Including one that only just misses, and one that reads like a keyword and is not.
        for name in ["x", "intx", "in", "INT", "fortran", "ordinary", "__builtin_expect"] {
            assert_eq!(lookup(Std::C23, true, name), None, "{name} is not a keyword");
        }
    }

    #[test]
    fn restrict_arrived_in_c99_and_gnu89_did_not_get_it_early() {
        // Measured: gcc and clang both leave `restrict` out of `-std=gnu89`, which is the one
        // place the GNU dialect is not a superset of the standard one it is based on.
        assert_eq!(lookup(Std::C89, false, "restrict"), None);
        assert_eq!(lookup(Std::C89, true, "restrict"), None);
        assert_eq!(lookup(Std::C99, false, "restrict"), Some(Keyword::Restrict));
        // `__restrict__` is how a header written for both says it, and it works in c89.
        assert_eq!(lookup(Std::C89, false, "__restrict__"), Some(Keyword::Restrict));
    }

    #[test]
    fn inline_arrived_in_c99_and_gnu89_did_get_it_early() {
        assert_eq!(lookup(Std::C89, false, "inline"), None);
        assert_eq!(lookup(Std::C89, true, "inline"), Some(Keyword::Inline));
        assert_eq!(lookup(Std::C99, false, "inline"), Some(Keyword::Inline));
    }

    #[test]
    fn typeof_is_a_gnu_extension_that_c23_made_standard() {
        assert_eq!(lookup(Std::C17, false, "typeof"), None);
        assert_eq!(lookup(Std::C17, true, "typeof"), Some(Keyword::Typeof));
        assert_eq!(lookup(Std::C23, false, "typeof"), Some(Keyword::Typeof));
        // `typeof_unqual` is the C23 half only, which is what both compilers do.
        assert_eq!(lookup(Std::C17, true, "typeof_unqual"), None);
        assert_eq!(lookup(Std::C23, false, "typeof_unqual"), Some(Keyword::TypeofUnqual));
        assert_eq!(lookup(Std::C17, false, "__typeof__"), Some(Keyword::Typeof));
    }

    #[test]
    fn asm_is_the_one_word_c23_still_does_not_have() {
        assert_eq!(lookup(Std::C23, false, "asm"), None);
        assert_eq!(lookup(Std::C23, true, "asm"), Some(Keyword::Asm));
        assert_eq!(lookup(Std::C89, false, "__asm__"), Some(Keyword::Asm));
    }

    #[test]
    fn the_c23_words_are_variable_names_in_every_earlier_dialect() {
        let added = [
            ("alignas", Keyword::Alignas),
            ("alignof", Keyword::Alignof),
            ("bool", Keyword::Bool),
            ("constexpr", Keyword::Constexpr),
            ("false", Keyword::False),
            ("nullptr", Keyword::Nullptr),
            ("static_assert", Keyword::StaticAssert),
            ("thread_local", Keyword::ThreadLocal),
            ("true", Keyword::True),
        ];
        for (spelling, keyword) in added {
            assert_eq!(lookup(Std::C17, true, spelling), None, "{spelling} in gnu17");
            assert_eq!(lookup(Std::C23, false, spelling), Some(keyword), "{spelling} in c23");
        }
        // The underscore spellings they replaced go on working, which is what lets one header
        // serve both.
        assert_eq!(lookup(Std::C17, false, "_Static_assert"), Some(Keyword::StaticAssert));
        assert_eq!(lookup(Std::C23, false, "_Static_assert"), Some(Keyword::StaticAssert));
    }

    #[test]
    fn two_spellings_of_one_thing_are_one_keyword() {
        for spelling in ["const", "__const"] {
            assert_eq!(lookup(Std::C23, true, spelling), Some(Keyword::Const));
        }
        for spelling in ["_Thread_local", "thread_local", "__thread"] {
            assert_eq!(lookup(Std::C23, true, spelling), Some(Keyword::ThreadLocal));
        }
        // And the one pair that looks like two spellings and is not. GNU's `__alignof__`
        // reports the preferred alignment, C's `_Alignof` the required one.
        assert_ne!(
            lookup(Std::C23, true, "__alignof__"),
            lookup(Std::C23, true, "_Alignof"),
            "the two alignments are different questions"
        );
    }

    #[test]
    fn every_keyword_prints_a_spelling_that_is_that_keyword() {
        for entry in KEYWORDS {
            let printed = entry.keyword.as_str();
            let found = KEYWORDS
                .iter()
                .find(|other| other.spelling == printed)
                .unwrap_or_else(|| panic!("{printed} is not in the table"));
            assert_eq!(found.keyword, entry.keyword, "{printed} prints for the wrong keyword");
        }
    }

    #[test]
    fn no_spelling_is_in_the_table_twice() {
        // A repeat would be interned once, the run of symbols would be short by one, and
        // `Keywords::new` would refuse to build at all. Better to say why here.
        let mut seen: Vec<&str> = KEYWORDS.iter().map(|entry| entry.spelling).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "a spelling appears twice in the table");
    }

    #[test]
    fn recognition_does_not_depend_on_what_was_interned_afterwards() {
        // The property the whole design rests on: the keywords are one run at the bottom of
        // the table, so an identifier interned later cannot land inside it however many there
        // are.
        let (keywords, mut interner) = build(Std::C23, true);
        for i in 0..1000 {
            let symbol = interner.intern(&format!("name{i}"));
            assert_eq!(keywords.get(symbol), None);
        }
        assert_eq!(keywords.get(interner.intern("while")), Some(Keyword::While));
    }

    #[test]
    #[should_panic(expected = "`static` was interned before the keyword table was built")]
    fn an_interner_that_already_has_a_keyword_in_it_is_refused() {
        // Silently building a table whose symbols are not one run would mean a compiler that
        // recognised the wrong words, which is not a failure anybody would find quickly.
        let mut interner = Interner::new();
        interner.intern("static");
        let _ = Keywords::new(&mut interner, Std::C23, true);
    }

    #[test]
    fn a_lookup_is_a_bounds_check_on_one_run_of_symbols() {
        let (keywords, _) = build(Std::C23, true);
        assert!(!keywords.is_empty());
        assert_eq!(keywords.len(), KEYWORDS.len());
    }
}
