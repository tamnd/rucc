//! String interning.
//!
//! Identifiers are compared constantly: on every macro lookup, every scope lookup, every
//! typedef disambiguation. Interning turns those comparisons into an integer compare and
//! turns the storage into one arena instead of a `String` per occurrence. The lexer interns
//! during the scan rather than after it, per `spec/06-lexer-and-parser.md`, so an identifier
//! is never materialised as a `String` at all.
//!
//! # Determinism
//!
//! [`Symbol`] ordering is allocation order, which is the order the source was read in. That
//! is deterministic for a given input, and it is the reason the compiler can sort by symbol
//! anywhere it needs a stable order without reaching for the string. Hashing a `Symbol` must
//! never leak into output ordering, because hash order is not stable across runs, and
//! `spec/02-the-goal.md` makes byte-identical output a requirement rather than a nicety.

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
}

/// An append-only set of strings, each mapped to a [`Symbol`].
///
/// Strings are never removed, which is what makes a `Symbol` valid for the lifetime of the
/// compilation and what lets the storage be a plain growing buffer.
#[derive(Default)]
pub struct Interner {
    /// Every interned string, concatenated. One allocation that doubles, rather than one
    /// allocation per identifier.
    buf: String,
    /// Where each symbol starts and ends in `buf`.
    spans: Vec<(u32, u32)>,
    /// Lookup from text to symbol. The key is a span into `buf` rather than an owned
    /// `String`, which is why the map is keyed by the string and rebuilt through `resolve`.
    map: HashMap<Box<str>, Symbol>,
}

impl Interner {
    /// An empty interner.
    pub fn new() -> Self {
        Self::default()
    }

    /// An interner with room for `cap` strings, to avoid regrowing on a large header set.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: String::with_capacity(cap * 8),
            spans: Vec::with_capacity(cap),
            map: HashMap::with_capacity(cap),
        }
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

    /// How many distinct strings have been interned.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Whether anything has been interned.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
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
        let a = i.intern("static_assert");
        let b = i.intern("static_assert");
        assert_eq!(a, b);
        assert_eq!(i.len(), 1);
    }

    #[test]
    fn different_strings_get_different_symbols() {
        let mut i = Interner::new();
        assert_ne!(i.intern("int"), i.intern("long"));
        assert_eq!(i.len(), 2);
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
        let s = i.intern("");
        assert_eq!(i.resolve(s), "");
        assert_eq!(i.bytes(), 0);
    }

    #[test]
    fn a_symbol_is_four_bytes() {
        assert_eq!(size_of::<Symbol>(), 4);
        assert_eq!(size_of::<Option<Symbol>>(), 4);
    }
}
