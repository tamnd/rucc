//! Hide sets: the set of macro names a token refuses to be expanded by again.
//!
//! Design: `spec/05-preprocessor.md` section 5.3.
//!
//! Prosser's algorithm gives every token a set of macro names that are already being
//! expanded around it, and refuses to expand a token by a macro that is in its set. That is
//! what makes mutually recursive macros terminate with the answer the standard asks for
//! rather than with a depth counter's approximation.
//!
//! The obvious implementation, a `HashSet<Symbol>` per token, allocates once per token in
//! the hottest loop of the preprocessor and is not affordable. Hide sets are instead
//! interned: each distinct set is stored once as a sorted slice, and a token carries a
//! four byte [`HideSet`] index into that table. Set equality is an integer compare, and the
//! common case by a wide margin is the empty set, which is always index zero.

use std::collections::HashMap;

use rucc_base::Symbol;

/// An interned set of macro names.
///
/// Only meaningful together with the [`HideSets`] table it was created by. There is one
/// table per translation unit, so mixing two of them is a bug rather than a case to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HideSet(u32);

impl HideSet {
    /// The empty set, which is index zero in every table.
    ///
    /// Available as a constant because a token that came straight from the lexer has an
    /// empty hide set and building one should not need the table.
    pub const EMPTY: HideSet = HideSet(0);

    /// Whether this is the empty set.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The underlying index, for packing a hide set into a token.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// The interning table for hide sets.
///
/// Sets are stored sorted so that membership is a binary search and so that two sets built
/// in different orders intern to the same index.
#[derive(Debug)]
pub struct HideSets {
    /// Every distinct set, sorted, indexed by [`HideSet`]. Index zero is the empty set.
    sets: Vec<Box<[Symbol]>>,
    /// Lookup from contents to index.
    map: HashMap<Box<[Symbol]>, HideSet>,
}

impl Default for HideSets {
    fn default() -> Self {
        Self::new()
    }
}

impl HideSets {
    /// A table containing only the empty set.
    pub fn new() -> Self {
        let empty: Box<[Symbol]> = Box::new([]);
        let mut map = HashMap::new();
        map.insert(empty.clone(), HideSet::EMPTY);
        Self { sets: vec![empty], map }
    }

    /// The members of a set, sorted.
    ///
    /// # Panics
    ///
    /// Panics if the set came from a different table.
    pub fn members(&self, set: HideSet) -> &[Symbol] {
        &self.sets[set.0 as usize]
    }

    /// How many distinct sets exist, which is the number worth watching on a macro heavy
    /// translation unit.
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    /// Always false: the table starts out holding the empty set.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Whether `name` is hidden by `set`.
    ///
    /// # Panics
    ///
    /// Panics if the set came from a different table.
    #[inline]
    pub fn contains(&self, set: HideSet, name: Symbol) -> bool {
        // The empty set is by far the most common, and skipping the bounds check and the
        // binary search for it is worth the branch.
        !set.is_empty() && self.sets[set.0 as usize].binary_search(&name).is_ok()
    }

    /// The set `set` with `name` added.
    ///
    /// # Panics
    ///
    /// Panics if the set came from a different table, or if more than `u32::MAX` distinct
    /// sets are interned, which would need a translation unit no machine can hold.
    pub fn add(&mut self, set: HideSet, name: Symbol) -> HideSet {
        let current = &self.sets[set.0 as usize];
        let Err(at) = current.binary_search(&name) else {
            return set;
        };
        let mut next = Vec::with_capacity(current.len() + 1);
        next.extend_from_slice(&current[..at]);
        next.push(name);
        next.extend_from_slice(&current[at..]);
        self.insert(next)
    }

    /// The union of two sets.
    ///
    /// Substitution unions the hide set being applied into whatever the token already
    /// carried, because an argument token arrives with the hide set it picked up where it
    /// was written and both restrictions have to hold.
    ///
    /// # Panics
    ///
    /// Panics if either set came from a different table.
    pub fn union(&mut self, a: HideSet, b: HideSet) -> HideSet {
        if a == b || b.is_empty() {
            return a;
        }
        if a.is_empty() {
            return b;
        }
        let merged = merge(&self.sets[a.0 as usize], &self.sets[b.0 as usize], Merge::Union);
        self.insert(merged)
    }

    /// The intersection of two sets.
    ///
    /// This is the operation function-like expansion needs: the hide set of the result is
    /// the intersection of the macro name token's set and the closing parenthesis token's
    /// set, per `spec/05-preprocessor.md` section 5.3.
    ///
    /// # Panics
    ///
    /// Panics if either set came from a different table.
    pub fn intersect(&mut self, a: HideSet, b: HideSet) -> HideSet {
        if a == b {
            return a;
        }
        if a.is_empty() || b.is_empty() {
            return HideSet::EMPTY;
        }
        let merged = merge(&self.sets[a.0 as usize], &self.sets[b.0 as usize], Merge::Intersect);
        self.insert(merged)
    }

    /// Interns a sorted set, returning the existing index if it has been seen.
    fn insert(&mut self, sorted: Vec<Symbol>) -> HideSet {
        let sorted: Box<[Symbol]> = sorted.into_boxed_slice();
        if let Some(&found) = self.map.get(&sorted) {
            return found;
        }
        let id = HideSet(u32::try_from(self.sets.len()).expect("too many hide sets"));
        self.sets.push(sorted.clone());
        self.map.insert(sorted, id);
        id
    }
}

/// Which of the two set operations [`merge`] is performing.
#[derive(Clone, Copy)]
enum Merge {
    Union,
    Intersect,
}

/// Merges two sorted slices. Kept out of the methods so that the borrow of the table ends
/// before the result is interned back into it.
fn merge(a: &[Symbol], b: &[Symbol], op: Merge) -> Vec<Symbol> {
    let mut out = Vec::with_capacity(match op {
        Merge::Union => a.len() + b.len(),
        Merge::Intersect => a.len().min(b.len()),
    });
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                if matches!(op, Merge::Union) {
                    out.push(a[i]);
                }
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                if matches!(op, Merge::Union) {
                    out.push(b[j]);
                }
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    if matches!(op, Merge::Union) {
        out.extend_from_slice(&a[i..]);
        out.extend_from_slice(&b[j..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    fn syms(n: usize) -> (Interner, Vec<Symbol>) {
        let mut interner = Interner::new();
        let names = (0..n).map(|i| interner.intern(&format!("M{i}"))).collect();
        (interner, names)
    }

    #[test]
    fn the_empty_set_is_index_zero_and_hides_nothing() {
        let (_i, s) = syms(1);
        let sets = HideSets::new();
        assert_eq!(HideSet::EMPTY.raw(), 0);
        assert!(!sets.contains(HideSet::EMPTY, s[0]));
    }

    #[test]
    fn adding_a_name_makes_it_hidden() {
        let (_i, s) = syms(2);
        let mut sets = HideSets::new();
        let one = sets.add(HideSet::EMPTY, s[0]);
        assert!(sets.contains(one, s[0]));
        assert!(!sets.contains(one, s[1]));
    }

    #[test]
    fn adding_the_same_name_twice_changes_nothing() {
        let (_i, s) = syms(1);
        let mut sets = HideSets::new();
        let one = sets.add(HideSet::EMPTY, s[0]);
        assert_eq!(sets.add(one, s[0]), one);
    }

    #[test]
    fn sets_built_in_different_orders_are_the_same_set() {
        let (_i, s) = syms(3);
        let mut sets = HideSets::new();
        let forward = {
            let a = sets.add(HideSet::EMPTY, s[0]);
            let b = sets.add(a, s[1]);
            sets.add(b, s[2])
        };
        let backward = {
            let a = sets.add(HideSet::EMPTY, s[2]);
            let b = sets.add(a, s[0]);
            sets.add(b, s[1])
        };
        assert_eq!(forward, backward, "interning must not depend on insertion order");
        assert_eq!(sets.members(forward).len(), 3);
    }

    #[test]
    fn union_keeps_everything_from_both() {
        let (_i, s) = syms(3);
        let mut sets = HideSets::new();
        let a = sets.add(HideSet::EMPTY, s[0]);
        let a = sets.add(a, s[1]);
        let b = sets.add(HideSet::EMPTY, s[1]);
        let b = sets.add(b, s[2]);
        let u = sets.union(a, b);
        assert_eq!(sets.members(u), &[s[0], s[1], s[2]]);
    }

    #[test]
    fn intersect_keeps_only_what_is_in_both() {
        let (_i, s) = syms(3);
        let mut sets = HideSets::new();
        let a = sets.add(HideSet::EMPTY, s[0]);
        let a = sets.add(a, s[1]);
        let b = sets.add(HideSet::EMPTY, s[1]);
        let b = sets.add(b, s[2]);
        let x = sets.intersect(a, b);
        assert_eq!(sets.members(x), &[s[1]]);
    }

    #[test]
    fn intersecting_with_the_empty_set_is_empty() {
        let (_i, s) = syms(1);
        let mut sets = HideSets::new();
        let a = sets.add(HideSet::EMPTY, s[0]);
        assert_eq!(sets.intersect(a, HideSet::EMPTY), HideSet::EMPTY);
    }

    #[test]
    fn a_hide_set_is_four_bytes() {
        assert_eq!(size_of::<HideSet>(), 4);
    }
}
