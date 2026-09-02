//! String interning.
//!
//! Identifiers are compared constantly: on every macro lookup, every scope lookup, every
//! typedef disambiguation. Interning turns those comparisons into an integer compare and
//! turns the storage into one arena instead of a `String` per occurrence. The lexer interns
//! during the scan rather than after it, per `spec/06-lexer-and-parser.md`, so an identifier
//! is never materialised as a `String` at all.
//!
//! # Reserved names
//!
//! Every interner starts with the names in [`RESERVED`] already in it, in that order, which is
//! what makes the constants in [`sym`] the symbols they are. The reason is that a pass past the
//! lexer holds the interner through a shared reference and cannot add to it, and a pass that
//! builds a type of its own still has to name it: the members of the target's `va_list` are
//! named by the ABI and never by the source, so the names have to exist before anything is read.
//!
//! # Determinism
//!
//! [`Symbol`] ordering is allocation order, which is the order the source was read in. That
//! is deterministic for a given input, and it is the reason the compiler can sort by symbol
//! anywhere it needs a stable order without reaching for the string. Hashing a `Symbol` must
//! never leak into output ordering, because hash order is not stable across runs, and
//! `spec/02-the-goal.md` makes byte-identical output a requirement rather than a nicety.
//!
//! # Spellings that are not text
//!
//! A source file is UTF-8 and an identifier in it is text, but the body of a string literal is
//! bytes and does not have to be text at all: `"\xff"` may be written as the byte itself, and
//! the object it initialises is one byte long whatever that byte is. So [`Interner::intern_bytes`]
//! takes a spelling that is not UTF-8 and [`Interner::resolve_bytes`] gives it back exactly,
//! while [`Interner::resolve`] still hands back a `&str`, because almost everything that holds a
//! symbol wants to print it. What it hands back for such a symbol is the lossy reading, with the
//! bytes that are not characters replaced, which is right for a message and wrong for an object,
//! and the object is what `resolve_bytes` is for.

use std::collections::HashMap;
use std::fmt;

use crate::index::Idx;

/// Marker for the symbol table, so that `Idx<SymbolTable>` cannot be confused with any
/// other index.
#[derive(Debug)]
pub struct SymbolTable;

/// An interned string.
///
/// Four bytes, `Copy`, and equal exactly when the strings are equal. Resolving one back to
/// text needs the [`Interner`] it came from, which is deliberate: it makes accidentally
/// printing an identifier in a hot path visible at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Symbol(Idx<SymbolTable>);

impl Symbol {
    /// The underlying index, for packing a symbol into a bitfield.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0.raw()
    }

    /// The symbol a [`Symbol::raw`] came from, which is the other half of packing one away.
    ///
    /// # Panics
    ///
    /// Panics if `raw` is not an index this interner could have handed out, which catches a
    /// field holding something other than a symbol rather than resolving to the wrong string.
    #[inline]
    #[must_use]
    pub const fn from_raw(raw: u32) -> Symbol {
        Symbol(Idx::new(raw))
    }
}

/// The names every interner is built with, in the order they are interned.
///
/// The list is short on purpose. A name belongs here when the compiler has to write it down and
/// the source is not the place it comes from, which so far is the target's type for a variable
/// argument list and nothing else.
pub const RESERVED: &[&str] = &[
    "__va_list_tag",
    "gp_offset",
    "fp_offset",
    "overflow_arg_area",
    "reg_save_area",
    "__va_list",
    "__stack",
    "__gr_top",
    "__vr_top",
    "__gr_offs",
    "__vr_offs",
];

/// The symbols for the names in [`RESERVED`].
///
/// Each constant is the position of its name in that list, so the two are one table written
/// twice and a test here holds them together.
pub mod sym {
    use super::Symbol;

    /// `__va_list_tag`, the tag of the record a SysV x86-64 `va_list` is an array of one of.
    pub const VA_LIST_TAG: Symbol = Symbol::from_raw(0);
    /// `gp_offset`, how far into the saved general registers the list has read.
    pub const GP_OFFSET: Symbol = Symbol::from_raw(1);
    /// `fp_offset`, the same for the saved floating point registers.
    pub const FP_OFFSET: Symbol = Symbol::from_raw(2);
    /// `overflow_arg_area`, the arguments that were passed on the stack.
    pub const OVERFLOW_ARG_AREA: Symbol = Symbol::from_raw(3);
    /// `reg_save_area`, where the callee spilled the argument registers.
    pub const REG_SAVE_AREA: Symbol = Symbol::from_raw(4);
    /// `__va_list`, the tag of the record an AAPCS64 `va_list` is.
    pub const VA_LIST: Symbol = Symbol::from_raw(5);
    /// `__stack`, the arguments that were passed on the stack.
    pub const STACK: Symbol = Symbol::from_raw(6);
    /// `__gr_top`, the end of the saved general registers.
    pub const GR_TOP: Symbol = Symbol::from_raw(7);
    /// `__vr_top`, the end of the saved vector registers.
    pub const VR_TOP: Symbol = Symbol::from_raw(8);
    /// `__gr_offs`, how far back from `__gr_top` the list has read, in bytes and negative.
    pub const GR_OFFS: Symbol = Symbol::from_raw(9);
    /// `__vr_offs`, the same for `__vr_top`.
    pub const VR_OFFS: Symbol = Symbol::from_raw(10);
}

/// An append-only set of strings, each mapped to a [`Symbol`].
///
/// Strings are never removed, which is what makes a `Symbol` valid for the lifetime of the
/// compilation and what lets the storage be a plain growing buffer.
pub struct Interner {
    /// Every interned string, concatenated. One allocation that doubles, rather than one
    /// allocation per identifier.
    buf: String,
    /// Where each symbol starts and ends in `buf`.
    spans: Vec<(u32, u32)>,
    /// Lookup from text to symbol. The key is a span into `buf` rather than an owned
    /// `String`, which is why the map is keyed by the string and rebuilt through `resolve`.
    map: HashMap<Box<str>, Symbol>,
    /// The spelling of a symbol whose bytes are not UTF-8, which `buf` cannot hold because
    /// `buf` is a `String`. A map rather than a column beside `spans`, because a compilation
    /// has a handful of these at most and usually none: a raw byte in a string literal is the
    /// only thing that puts one here.
    raw: HashMap<Symbol, Box<[u8]>>,
    /// Lookup from those bytes back to their symbol, so that interning the same spelling twice
    /// is the same symbol. Kept apart from `map` because two spellings that are not text can
    /// read the same lossily and still have to be told apart.
    raw_map: HashMap<Box<[u8]>, Symbol>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    /// An interner holding the reserved names and nothing else.
    pub fn new() -> Self {
        Self::with_capacity(RESERVED.len())
    }

    /// An interner with room for `cap` strings, to avoid regrowing on a large header set.
    pub fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(RESERVED.len());
        let mut interner = Self {
            buf: String::with_capacity(cap * 8),
            spans: Vec::with_capacity(cap),
            map: HashMap::with_capacity(cap),
            raw: HashMap::new(),
            raw_map: HashMap::new(),
        };
        for name in RESERVED {
            interner.intern(name);
        }
        interner
    }

    /// Interns `s`, returning the existing symbol if it has been seen.
    ///
    /// # Panics
    ///
    /// Panics if more than `Idx::MAX` distinct strings are interned.
    pub fn intern(&mut self, s: &str) -> Symbol {
        if let Some(&sym) = self.map.get(s) {
            return sym;
        }
        let start = u32::try_from(self.buf.len()).expect("interner buffer overflow");
        self.buf.push_str(s);
        let end = u32::try_from(self.buf.len()).expect("interner buffer overflow");
        let sym = Symbol(Idx::from_usize(self.spans.len()));
        self.spans.push((start, end));
        self.map.insert(s.into(), sym);
        sym
    }

    /// Interns a spelling that may not be text, returning the existing symbol if it has been seen.
    ///
    /// A spelling that is UTF-8 is interned as itself, so nothing changes for the common case and
    /// a byte spelling equal to a name is the same symbol as that name. One that is not gets a
    /// symbol of its own whose text is the lossy reading, which is what [`Interner::resolve`]
    /// hands back, and whose bytes are kept beside it for [`Interner::resolve_bytes`].
    ///
    /// # Panics
    ///
    /// Panics if more than `Idx::MAX` distinct spellings are interned.
    pub fn intern_bytes(&mut self, bytes: &[u8]) -> Symbol {
        if let Ok(text) = std::str::from_utf8(bytes) {
            return self.intern(text);
        }
        if let Some(&sym) = self.raw_map.get(bytes) {
            return sym;
        }
        // Pushed straight into the buffer rather than through `intern`, because the lossy
        // reading may be a string that is already in there and this spelling is not that one.
        let lossy = String::from_utf8_lossy(bytes);
        let start = u32::try_from(self.buf.len()).expect("interner buffer overflow");
        self.buf.push_str(&lossy);
        let end = u32::try_from(self.buf.len()).expect("interner buffer overflow");
        let sym = Symbol(Idx::from_usize(self.spans.len()));
        self.spans.push((start, end));
        self.raw.insert(sym, bytes.into());
        self.raw_map.insert(bytes.into(), sym);
        sym
    }

    /// The text behind a symbol.
    ///
    /// # Panics
    ///
    /// Panics if the symbol came from a different interner. There is one interner per
    /// compilation, so this is a bug rather than a condition to handle.
    pub fn resolve(&self, sym: Symbol) -> &str {
        let (start, end) = self.spans[sym.0.index()];
        &self.buf[start as usize..end as usize]
    }

    /// The bytes behind a symbol, which is the spelling exactly as it was written.
    ///
    /// The same as `resolve(sym).as_bytes()` for every symbol that came from text, which is all
    /// of them but the ones [`Interner::intern_bytes`] made from bytes that are not UTF-8.
    ///
    /// # Panics
    ///
    /// Panics if the symbol came from a different interner, as [`Interner::resolve`] does.
    pub fn resolve_bytes(&self, sym: Symbol) -> &[u8] {
        match self.raw.get(&sym) {
            Some(bytes) => bytes,
            None => self.resolve(sym).as_bytes(),
        }
    }

    /// How many distinct strings have been interned, the reserved names included.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Whether anything but the reserved names has been interned.
    pub fn is_empty(&self) -> bool {
        self.spans.len() <= RESERVED.len()
    }

    /// Total bytes of interned text, which is the number worth watching on a large build.
    pub fn bytes(&self) -> usize {
        self.buf.len()
    }
}

impl fmt::Debug for Interner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Dumping every identifier in a translation unit is never what anyone wanted from a
        // `{:?}` on the session, so this reports the shape instead.
        f.debug_struct("Interner")
            .field("symbols", &self.spans.len())
            .field("bytes", &self.buf.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_string_gets_the_same_symbol() {
        let mut i = Interner::new();
        let before = i.len();
        let a = i.intern("static_assert");
        let b = i.intern("static_assert");
        assert_eq!(a, b);
        assert_eq!(i.len() - before, 1);
    }

    #[test]
    fn different_strings_get_different_symbols() {
        let mut i = Interner::new();
        let before = i.len();
        assert_ne!(i.intern("int"), i.intern("long"));
        assert_eq!(i.len() - before, 2);
    }

    #[test]
    fn the_reserved_names_are_there_before_anything_is_read() {
        let i = Interner::new();
        assert_eq!(i.len(), RESERVED.len());
        assert!(i.is_empty(), "the reserved names do not count as something having been read");
        assert_eq!(i.resolve(sym::VA_LIST_TAG), "__va_list_tag");
        assert_eq!(i.resolve(sym::GP_OFFSET), "gp_offset");
        assert_eq!(i.resolve(sym::FP_OFFSET), "fp_offset");
        assert_eq!(i.resolve(sym::OVERFLOW_ARG_AREA), "overflow_arg_area");
        assert_eq!(i.resolve(sym::REG_SAVE_AREA), "reg_save_area");
        assert_eq!(i.resolve(sym::VA_LIST), "__va_list");
        assert_eq!(i.resolve(sym::STACK), "__stack");
        assert_eq!(i.resolve(sym::GR_TOP), "__gr_top");
        assert_eq!(i.resolve(sym::VR_TOP), "__vr_top");
        assert_eq!(i.resolve(sym::GR_OFFS), "__gr_offs");
        assert_eq!(i.resolve(sym::VR_OFFS), "__vr_offs");
    }

    #[test]
    fn a_reserved_name_written_in_the_source_is_the_symbol_it_already_had() {
        let mut i = Interner::new();
        let before = i.len();
        assert_eq!(i.intern("__va_list_tag"), sym::VA_LIST_TAG);
        assert_eq!(i.len(), before);
    }

    #[test]
    fn every_interner_agrees_on_where_the_reserved_names_are() {
        let small = Interner::new();
        let large = Interner::with_capacity(4096);
        for (at, name) in RESERVED.iter().enumerate() {
            let sym = Symbol::from_raw(u32::try_from(at).expect("eleven names fit in a u32"));
            assert_eq!(small.resolve(sym), *name);
            assert_eq!(large.resolve(sym), *name);
        }
    }

    #[test]
    fn resolves_back_to_the_text() {
        let mut i = Interner::new();
        let s = i.intern("__builtin_constant_p");
        assert_eq!(i.resolve(s), "__builtin_constant_p");
    }

    #[test]
    fn symbols_are_numbered_in_allocation_order() {
        let mut i = Interner::new();
        let first = i.intern("a");
        let second = i.intern("b");
        assert!(first < second, "symbol order must be allocation order, not hash order");
    }

    #[test]
    fn the_empty_string_is_internable() {
        let mut i = Interner::new();
        let before = i.bytes();
        let s = i.intern("");
        assert_eq!(i.resolve(s), "");
        assert_eq!(i.bytes(), before);
    }

    #[test]
    fn a_spelling_that_is_text_is_the_same_symbol_however_it_was_interned() {
        let mut i = Interner::new();
        let text = i.intern("hello");
        assert_eq!(i.intern_bytes(b"hello"), text);
        assert_eq!(i.resolve_bytes(text), b"hello");
    }

    #[test]
    fn a_spelling_that_is_not_text_keeps_its_bytes() {
        let mut i = Interner::new();
        let raw = i.intern_bytes(b"\"\xff\"");
        assert_eq!(i.resolve_bytes(raw), b"\"\xff\"");
        assert_eq!(i.intern_bytes(b"\"\xff\""), raw, "interning it twice is one symbol");
        // The text is the lossy reading, which is what a message quoting it would print.
        assert_eq!(i.resolve(raw), "\"\u{fffd}\"");
    }

    #[test]
    fn two_spellings_that_read_the_same_lossily_are_still_two_symbols() {
        let mut i = Interner::new();
        let one = i.intern_bytes(b"\xff");
        let other = i.intern_bytes(b"\xfe");
        assert_eq!(i.resolve(one), i.resolve(other), "both read as the replacement character");
        assert_ne!(one, other, "the bytes differ, so the spellings do");
        assert_eq!(i.resolve_bytes(one), b"\xff");
        assert_eq!(i.resolve_bytes(other), b"\xfe");
        // And neither of them is the text that reads the same, which the source may also hold.
        assert_ne!(i.intern("\u{fffd}"), one);
    }

    #[test]
    fn a_symbol_is_four_bytes() {
        assert_eq!(size_of::<Symbol>(), 4);
        assert_eq!(size_of::<Option<Symbol>>(), 4);
    }
}
