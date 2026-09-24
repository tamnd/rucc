//! Read only data a function pass has come to need, on its way to the module.
//!
//! A [`crate::Pass`] is handed one function and nothing else, which is what lets it be reasoned
//! about alone and is why it cannot add a global: the globals are the module's, and the module is
//! the thing the pass is not given. Switch conversion to a lookup table is the one pass that needs
//! one anyway. Section 24.4 of `spec/optimizer/24-switch-lowering.md` puts it in the middle end so
//! that what it writes is an ordinary load every pass after it can read, and a load has to load
//! from somewhere.
//!
//! So the pass asks this for a name, writes its load against that name, and leaves the table here.
//! The pipeline adds every table to the module as soon as the pass has finished with the function,
//! before the verifier looks at it, so no function is ever seen naming a table the module does not
//! have. The same shape as `crate::libcall`, which is handed the module whole because it needs
//! one, but narrower: a pass that goes through this can add a constant array and do nothing else
//! to the module, which is what keeps it a function pass.

use std::collections::HashSet;

use rucc_base::{Interner, Symbol};
use rucc_ir::Type;

/// One array a pass has asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The name its load was written against.
    pub name: Symbol,
    /// The type of every cell, which is an integer of a whole number of bytes.
    pub ty: Type,
    /// The cells in order, each read with its own sign and held at the type's width when written.
    pub cells: Vec<i128>,
    /// What each cell also holds the distance to, in the same order, or nothing at all when every
    /// cell is only a number. A cell with a name here is that name's address less the address of
    /// the cell's own table, plus its number, which the linker works out.
    pub to: Vec<Option<Symbol>>,
}

/// Where a pass puts the tables it asks for, until the pipeline takes them.
#[derive(Debug)]
pub struct ReadOnly<'a> {
    names: &'a mut Interner,
    taken: &'a HashSet<Symbol>,
    pointer_bits: u32,
    measures: bool,
    next: u32,
    tables: Vec<Table>,
}

impl<'a> ReadOnly<'a> {
    /// A place for tables in a module where `taken` are the names already in use.
    ///
    /// `next` is the number the next name is made from. The pipeline makes one of these for every
    /// function a pass runs over, so the count is handed in and read back out with
    /// [`ReadOnly::next`], and two functions never get the same name.
    #[must_use]
    pub fn new(
        names: &'a mut Interner,
        taken: &'a HashSet<Symbol>,
        pointer_bits: u32,
        next: u32,
    ) -> Self {
        Self { names, taken, pointer_bits, measures: false, next, tables: Vec::new() }
    }

    /// The same place, able to hold a table of distances when `measures` is true.
    #[must_use]
    pub const fn measuring(mut self, measures: bool) -> Self {
        self.measures = measures;
        self
    }

    /// Whether a cell may be how far a name is from its table, which [`ReadOnly::distances`]
    /// makes.
    ///
    /// That needs a four byte relocation measured from where it is written. x86-64 ELF has one,
    /// and it is the only target the pipeline says yes for.
    #[must_use]
    pub const fn measures(&self) -> bool {
        self.measures
    }

    /// The width of an address on the target, which is how wide an index into a table is made.
    #[must_use]
    pub const fn pointer_bits(&self) -> u32 {
        self.pointer_bits
    }

    /// Asks for a table and gets back the name to load from it by.
    ///
    /// The name is gcc's, `CSWTCH.` and a number, which nothing written in C can spell because of
    /// the dot and which reads the same in a disassembly of either compiler. A name the module
    /// already has is stepped over rather than trusted not to be there, since an `asm` label can
    /// spell anything.
    pub fn table(&mut self, ty: Type, cells: Vec<i128>) -> Symbol {
        let name = self.name();
        self.tables.push(Table { name, ty, cells, to: Vec::new() });
        name
    }

    /// Asks for a table of how far names are from it and gets back the name to load from it by.
    ///
    /// Cell `k` is four bytes holding the address of the name in `to[k]` plus the bytes beside it,
    /// less the address of the table, and zero where `to[k]` is `None`, which is a hole nothing
    /// reads. Only asked for where [`ReadOnly::measures`] says it may be, and named the way
    /// [`ReadOnly::table`] names one.
    pub fn distances(&mut self, to: &[Option<(Symbol, i128)>]) -> Symbol {
        let name = self.name();
        let cells = to.iter().map(|cell| cell.map_or(0, |(_, bytes)| bytes)).collect();
        let to = to.iter().map(|cell| cell.map(|(name, _)| name)).collect();
        self.tables.push(Table { name, ty: Type::int(32), cells, to });
        name
    }

    /// A name for the next table, which is one the module does not have.
    fn name(&mut self) -> Symbol {
        loop {
            let name = self.names.intern(&format!("CSWTCH.{}", self.next));
            self.next += 1;
            if !self.taken.contains(&name) {
                return name;
            }
        }
    }

    /// The number the next name will be made from.
    #[must_use]
    pub const fn next(&self) -> u32 {
        self.next
    }

    /// Every table asked for so far, in the order they were asked for.
    #[must_use]
    pub fn into_tables(self) -> Vec<Table> {
        self.tables
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rucc_base::Interner;
    use rucc_ir::Type;

    use super::ReadOnly;

    #[test]
    fn two_tables_get_two_names_and_a_taken_name_is_stepped_over() {
        let mut names = Interner::new();
        let taken: HashSet<_> = [names.intern("CSWTCH.1")].into_iter().collect();
        let mut data = ReadOnly::new(&mut names, &taken, 64, 0);
        let first = data.table(Type::int(8), vec![1, 2]);
        let second = data.table(Type::int(8), vec![3]);
        assert_eq!(data.next(), 3);
        let tables = data.into_tables();
        assert_eq!(tables.len(), 2);
        assert_eq!(names.resolve(first), "CSWTCH.0");
        assert_eq!(names.resolve(second), "CSWTCH.2");
    }

    #[test]
    fn a_table_of_distances_is_four_byte_cells_named_like_any_other() {
        let mut names = Interner::new();
        let taken = HashSet::new();
        let to = [names.intern("a"), names.intern("b")].map(Some);
        let mut data = ReadOnly::new(&mut names, &taken, 64, 0).measuring(true);
        assert!(data.measures());
        let name = data.distances(&[to[0].map(|it| (it, 0)), None, to[1].map(|it| (it, 8))]);
        let tables = data.into_tables();
        assert_eq!(names.resolve(name), "CSWTCH.0");
        assert_eq!(tables[0].ty, Type::int(32));
        assert_eq!(tables[0].cells, [0, 0, 8]);
        assert_eq!(tables[0].to, [to[0], None, to[1]]);
    }
}
