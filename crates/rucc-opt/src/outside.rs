//! The handful of module facts a pass handed one function cannot reach, copied out once.
//!
//! `crate::alias` is written, tested and documented, and until this existed nothing in the pipeline
//! asked it anything. The reason was structural rather than anybody forgetting. A pass is handed
//! one function, `Alias::new` wanted the module the function is in, and the manager hands out
//! `&mut module[id]`, so a pass could not borrow the module even to ask a question about it. That
//! is tamnd/rucc#1467.
//!
//! What the oracle wants from the module is four things and none of them is large.
//!
//! What each name refers to, so that two symbols are known to be two objects. An `alias` or an
//! `ifunc` is exactly a second name for something, so the question is not whether the module has
//! the name but which of the three kinds it is.
//!
//! What a function a call names is declared to do to memory, which is `const`, `pure` and
//! `argmemonly` as the C programmer wrote them.
//!
//! The parent of each type node, which is the whole of what the type based layer walks. The nodes
//! are a flat table on the module and the walk only ever goes up, so a vector of parents indexed
//! the way the table is is the same walk with the module left behind.
//!
//! The data layout, which is two words and is there because the width of a pointer is the target's
//! answer rather than the type's.
//!
//! # Why a copy and not a borrow
//!
//! The same argument `crate::image` makes, and the pipeline does the same thing with the result: one
//! of these is built for the module before any pass runs and each function's analysis cache holds a
//! counted reference to it. A pass then builds its oracle out of the function it was handed and the
//! cache it was handed, and borrows nothing that is being mutated.
//!
//! The size is the reason this is affordable where a borrow would have been free. A symbol table
//! entry is a word and a tag, a parent is an index, and neither is per instruction. The images this
//! sits beside are a copy of the module's read only data and are built anyway.
//!
//! # The empty one
//!
//! [`Outside::default`] knows nothing, and an oracle built on it answers `May` to everything it
//! would have used the module for. That is what a caller with no module gets, and it is the
//! conservative direction, so a test or a tool that builds a cache without a pipeline is slower
//! rather than wrong.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_ir::{Attrs, DataLayout, Meta, Module};

/// What one name in the module refers to, as much of it as the oracle asks about.
#[derive(Clone, Copy, Debug)]
enum Named {
    /// A function, and what it is declared to do to memory.
    Func(Attrs),
    /// A global variable.
    Global,
    /// An alias or an ifunc, which is a second name for an object that has another one.
    Second,
}

/// The module, as much of it as an alias oracle asks about.
#[derive(Clone, Debug, Default)]
pub struct Outside {
    /// What each name the module defines or declares refers to.
    names: HashMap<Symbol, Named>,
    /// The node one level up from each metadata node, indexed the way the module's table is.
    parents: Vec<Option<Meta>>,
    /// What the module was built assuming, or nothing when this was built without a module.
    layout: Option<DataLayout>,
}

impl Outside {
    /// Copies the four facts out of a module.
    #[must_use]
    pub fn of(module: &Module) -> Self {
        let mut names = HashMap::new();
        for id in module.funcs() {
            names.insert(module[id].name, Named::Func(module[id].attrs));
        }
        for id in module.globals() {
            names.insert(module[id].name, Named::Global);
        }
        for id in module.aliases() {
            names.insert(module[id].name, Named::Second);
        }
        // In the order the module has them, so a node's own index is where its parent sits here.
        let parents = module.metadata().map(|node| module[node].parent()).collect();
        Self { names, parents, layout: Some(module.datalayout) }
    }

    /// Whether this symbol is a name for an object no other name in the module also names.
    ///
    /// A name the module does not have at all is treated the way an alias is, because something is
    /// wrong and the conservative answer is the one to be wrong in the direction of.
    #[must_use]
    pub fn one_object(&self, name: Symbol) -> bool {
        matches!(self.names.get(&name), Some(Named::Func(_) | Named::Global))
    }

    /// What a function of that name is declared to do to memory.
    ///
    /// Nothing for a name that is not a function here, which covers an indirect call, a call of
    /// something declared in another module and a call through an alias.
    #[must_use]
    pub fn attrs(&self, name: Symbol) -> Option<Attrs> {
        match self.names.get(&name)? {
            Named::Func(attrs) => Some(*attrs),
            Named::Global | Named::Second => None,
        }
    }

    /// The type node one level up from this one, or nothing at the root.
    #[must_use]
    pub fn parent(&self, node: Meta) -> Option<Meta> {
        self.parents.get(node.index()).copied().flatten()
    }

    /// How many bytes an address takes on the target this module was built for.
    #[must_use]
    pub fn pointer_bytes(&self) -> Option<u64> {
        Some(u64::from(self.layout?.pointer_bits).div_ceil(8))
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Alias, AttrSet, Attrs, Func, Global, MetaNode, Module, Signature, TbaaNode};
    use rucc_target::{TargetInfo, Triple};

    use super::Outside;

    /// An empty module for a sixty four bit Linux.
    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        Module::new(names.intern("t.c"), &target)
    }

    #[test]
    fn a_function_and_a_global_each_name_one_object() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let f = names.intern("f");
        let table = names.intern("table");
        module.add_func(Func::new(f, Signature::new()));
        module.add_global(Global::new(table, 16, 8));
        let outside = Outside::of(&module);
        assert!(outside.one_object(f));
        assert!(outside.one_object(table));
    }

    #[test]
    fn an_alias_and_a_name_nobody_has_do_not() {
        // Two names for one object is the case the rule that two objects do not alias must not
        // reach, and a name this module never saw is the same answer for a different reason.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let here = names.intern("here");
        let second = names.intern("second");
        module.add_func(Func::new(here, Signature::new()));
        module.add_alias(Alias::new(second, here));
        let outside = Outside::of(&module);
        assert!(!outside.one_object(second));
        assert!(!outside.one_object(names.intern("nowhere")));
    }

    #[test]
    fn a_function_carries_what_it_is_declared_to_do_to_memory() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let f = names.intern("f");
        let mut func = Func::new(f, Signature::new());
        func.attrs = Attrs { set: AttrSet::READONLY, ..Attrs::default() };
        module.add_func(func);
        let table = names.intern("table");
        module.add_global(Global::new(table, 16, 8));
        let outside = Outside::of(&module);
        assert!(outside.attrs(f).expect("it is a function").set.contains(AttrSet::READONLY));
        assert!(outside.attrs(table).is_none(), "a global is not something a call names");
    }

    #[test]
    fn a_type_node_knows_the_one_above_it() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let root = module.add_meta(MetaNode::Tbaa(TbaaNode {
            name: names.intern("omnipotent char"),
            parent: None,
            offset: 0,
        }));
        let under = module.add_meta(MetaNode::Tbaa(TbaaNode {
            name: names.intern("int"),
            parent: Some(root),
            offset: 0,
        }));
        let outside = Outside::of(&module);
        assert_eq!(outside.parent(under), Some(root));
        assert_eq!(outside.parent(root), None);
    }

    #[test]
    fn the_empty_one_knows_nothing_and_says_so() {
        let mut names = Interner::new();
        let outside = Outside::default();
        assert!(!outside.one_object(names.intern("f")));
        assert!(outside.attrs(names.intern("f")).is_none());
        assert!(outside.pointer_bytes().is_none());
    }

    #[test]
    fn a_module_says_how_wide_an_address_is() {
        let mut names = Interner::new();
        let module = module(&mut names);
        assert_eq!(Outside::of(&module).pointer_bytes(), Some(8));
    }
}
