//! The type based aliasing tree, built from the C types the walk already has in hand.
//!
//! Design: `spec/optimizer/08-alias-analysis.md` section 8.2.
//!
//! Layer 3 of the alias analysis asks whether two accesses go through types that can describe the
//! same byte, and it answers by walking a tree of nodes from the module's metadata table. This
//! file is where that tree comes from. Every load and every store the walk builds carries the node
//! for the type it is an access through, and two accesses whose nodes are in different parts of
//! the tree are two accesses the optimizer may reorder.
//!
//! # The tree
//!
//! `char` is the root, so an access through a character type conflicts with everything. That is C
//! 6.5 paragraph 7's last clause and it is the rule the whole scheme rests on: a program is
//! allowed to read any object as bytes, so the byte reader has to be treated as being able to
//! reach any object.
//!
//! Every other scalar hangs directly under the root, one node per type, so two different scalar
//! types never conflict and either of them conflicts with `char`. The tree is one level deep on
//! purpose. A deeper tree is what puts a struct member under the struct it is in, which is worth
//! having and is the offset field on the node, but it needs the access to know which member of
//! which aggregate it landed on and the walk does not carry that today. One level is the part
//! that is both correct and cheap, and it is where the great majority of the disambiguations are:
//! a loop over an `int` array that also writes through a `double *` is the shape this answers.
//!
//! # What shares a node
//!
//! A signed type and its unsigned counterpart share one node, because 6.5 paragraph 7 lets an
//! object be accessed through either. An enumeration shares with the integer type it is
//! represented in, for the same reason. All three character types are the root rather than three
//! nodes under it, since each of them is a character type and the clause names them all.
//!
//! Every object pointer is one node, which is what gcc's C front end does in
//! `c_common_get_alias_set`: the pointed-to type is dropped and every pointer lands in one set.
//! It costs the disambiguation between an `int *` and a `char *` held in memory and it avoids
//! being wrong about the many programs that store one pointer type and read another back.
//!
//! # What gets no node at all
//!
//! An aggregate, an array, a vector, a function and `void`. No node means the access conflicts
//! with everything, because layer 3 only answers when both sides carry one, so leaving a type out
//! costs speed and never correctness. An aggregate is the interesting one: an access to a struct
//! as a whole is a copy, a copy's members have types of their own that are what the later accesses
//! carry, and putting the struct itself at the root is the conservative answer C requires anyway
//! for a type that contains a character type somewhere in it.
//!
//! # What a member of a union gets
//!
//! The root, and `crate::body` is where that is decided rather than here, because the type of the
//! member says nothing about what it is a member of. Every member of a union starts at the same
//! byte, so an access to one of them is an access to bytes another member may have been written
//! through, and C 6.5.2.3 permits reading them back that way. The root is the node that conflicts
//! with everything, which is the answer that keeps the punning working.
//!
//! Layer 3 would have been right without it, because layer 4 settles two accesses to one object on
//! their offsets before the types are looked at. The type plane of `spec/safe-memory` has no layer
//! 4 to run first: what it holds is a number per byte of the program's own memory, and a store
//! through one member followed by a read through another is exactly the shape a check against it
//! would refuse. So the answer is given once, on the access, and both readers get the same one.
//!
//! # Why the name is the identity
//!
//! A node is found by a canonical spelling of the type and never by the order the walk met it.
//! Two accesses through `int` in two different functions get one node because they spell the same
//! string, not because one of them was lowered first. Section 8.2 asks for this and the reason is
//! document 35's link time optimization: merging two modules is merging two metadata tables, and a
//! table keyed on the order a walk happened to visit types gives the merge no way to tell that
//! this module's `int` and that module's `int` are the same thing. Merging by name is a merge
//! anybody can write, and a disambiguation that only appears when the two halves of a program are
//! compiled separately is the kind of bug nobody finds.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_ir::{Meta, MetaNode, Module, TbaaNode};
use rucc_types::{FloatKind, IntKind, TypeId, TypeKind, Types};

/// What the root is called, which is the type every access through it may reach any object with.
const ROOT: &str = "char";

/// What every object pointer is called, since they all share one node.
const POINTER: &str = "pointer";

/// The nodes built so far, by the spelling that names them.
///
/// Per translation unit rather than per function, because the tree lives in the module's metadata
/// table and a node per function would make two accesses to the same type look unrelated.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    /// The root, built the first time anything asks for a node, so a module whose accesses are all
    /// through types with no node has no metadata table at all.
    root: Option<Meta>,
    /// Everything under it.
    nodes: HashMap<String, Meta>,
}

impl Tree {
    /// The node for an access through `ty`, and [`None`] for a type that has no node.
    pub(crate) fn node(
        &mut self,
        module: &mut Module,
        names: &mut Interner,
        types: &Types,
        ty: TypeId,
    ) -> Option<Meta> {
        match spelling(types, ty)? {
            Name::Character => Some(self.root(module, names)),
            Name::Distinct(name) => {
                if let Some(&found) = self.nodes.get(&name) {
                    return Some(found);
                }
                let parent = self.root(module, names);
                let symbol = names.intern(&name);
                let node = module.add_meta(MetaNode::Tbaa(TbaaNode {
                    name: symbol,
                    parent: Some(parent),
                    offset: 0,
                }));
                self.nodes.insert(name, node);
                Some(node)
            }
        }
    }

    /// The root, built on first use.
    pub(crate) fn root(&mut self, module: &mut Module, names: &mut Interner) -> Meta {
        match self.root {
            Some(root) => root,
            None => {
                let name = names.intern(ROOT);
                let root =
                    module.add_meta(MetaNode::Tbaa(TbaaNode { name, parent: None, offset: 0 }));
                self.root = Some(root);
                root
            }
        }
    }
}

/// Which node a type wants, as a name rather than as a node, so that the walk over the type is
/// separate from the table that hands out nodes.
enum Name {
    /// The root, which is what a character type gets.
    Character,
    /// A node of its own, under the root, called this.
    Distinct(String),
}

/// The canonical spelling of the type an access through `ty` is an access through.
///
/// Typedefs are resolved first and qualifiers are never looked at, so `const size_t` and
/// `unsigned long` are one name on a target where they are one type.
fn spelling(types: &Types, ty: TypeId) -> Option<Name> {
    match types.kind(types.canonical(ty)) {
        TypeKind::Bool => Some(Name::Distinct("bool".to_string())),
        TypeKind::Int(kind) => Some(match integer(kind) {
            Some(name) => Name::Distinct(name.to_string()),
            None => Name::Character,
        }),
        // Signed and unsigned share, the same way the standard types above do, and the width is
        // part of the name because a `_BitInt(17)` and a `_BitInt(18)` are two types.
        TypeKind::BitInt { width, .. } => Some(Name::Distinct(format!("_BitInt({width})"))),
        TypeKind::Float(kind) => Some(Name::Distinct(floating(kind).to_string())),
        TypeKind::Complex(kind) => Some(Name::Distinct(format!("_Complex {}", floating(kind)))),
        TypeKind::Pointer(_) => Some(Name::Distinct(POINTER.to_string())),
        // `_Atomic int` is not `int`, and an object of one accessed as the other conflicts, which
        // is what sharing the name says. Whether the access is atomic is a separate field on the
        // instruction and no layer of the alias analysis reads this one for it.
        TypeKind::Atomic(inner) => spelling(types, inner),
        // Whatever the enumeration is represented in. An enumerated type and its compatible
        // integer type may each be used to access an object of the other.
        TypeKind::Enum(id) => spelling(types, types.enum_info(id).underlying?),
        _ => None,
    }
}

/// What an integer type is called, with the unsigned types folded into the signed ones, and
/// [`None`] for a character type, which is the root rather than anything under it.
const fn integer(kind: IntKind) -> Option<&'static str> {
    match kind {
        IntKind::Char | IntKind::SChar | IntKind::UChar => None,
        IntKind::Short | IntKind::UShort => Some("short"),
        IntKind::Int | IntKind::UInt => Some("int"),
        IntKind::Long | IntKind::ULong => Some("long"),
        IntKind::LongLong | IntKind::ULongLong => Some("long long"),
        IntKind::Int128 | IntKind::UInt128 => Some("__int128"),
    }
}

/// What a real floating type is called.
///
/// One name per kind and not one per format. `float` and `_Float32` are the same bits everywhere
/// and are still two types, `long double` is a `double` on Apple and on Windows and is still a
/// type of its own, and an object of one may not be accessed through the other.
const fn floating(kind: FloatKind) -> &'static str {
    match kind {
        FloatKind::Float16 => "_Float16",
        FloatKind::Float => "float",
        FloatKind::Float32 => "_Float32",
        FloatKind::Double => "double",
        FloatKind::Float32x => "_Float32x",
        FloatKind::Float64 => "_Float64",
        FloatKind::LongDouble => "long double",
        FloatKind::Float64x => "_Float64x",
        FloatKind::Float128 => "_Float128",
    }
}

#[cfg(test)]
mod tests {
    use rucc_ir::MetaNode;
    use rucc_target::TargetInfo;
    use rucc_types::{ArrayLen, Qualifiers};

    use super::*;

    /// A module to hang nodes off, an interner to name them with, and an empty type table.
    struct Fixture {
        names: Interner,
        types: Types,
        module: Module,
        tree: Tree,
    }

    impl Fixture {
        fn new() -> Self {
            let mut names = Interner::new();
            let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().expect("a triple"));
            let module = Module::new(names.intern("test"), &target);
            Self { names, types: Types::new(), module, tree: Tree::default() }
        }

        fn node(&mut self, ty: TypeId) -> Option<Meta> {
            self.tree.node(&mut self.module, &mut self.names, &self.types, ty)
        }

        /// What a node is called, which is what the tree is keyed on.
        fn name(&self, node: Meta) -> &str {
            match self.module[node] {
                MetaNode::Tbaa(tbaa) => self.names.resolve(tbaa.name),
                MetaNode::Plane(_) => panic!("a plane node in the aliasing tree"),
            }
        }

        /// What a node hangs under.
        fn parent(&self, node: Meta) -> Option<Meta> {
            match self.module[node] {
                MetaNode::Tbaa(tbaa) => tbaa.parent,
                MetaNode::Plane(_) => panic!("a plane node in the aliasing tree"),
            }
        }
    }

    #[test]
    fn every_scalar_hangs_under_the_character_type() {
        let mut fix = Fixture::new();
        let int = fix.types.int(IntKind::Int);
        let node = fix.node(int).expect("a node for int");
        assert_eq!(fix.name(node), "int");
        let root = fix.parent(node).expect("a parent for int");
        assert_eq!(fix.name(root), "char");
        assert_eq!(fix.parent(root), None, "the character type is the root");
    }

    #[test]
    fn a_character_type_is_the_root_itself_rather_than_a_node_under_it() {
        let mut fix = Fixture::new();
        let plain = fix.types.int(IntKind::Char);
        let signed = fix.types.int(IntKind::SChar);
        let unsigned = fix.types.int(IntKind::UChar);
        let root = fix.node(plain).expect("a node for char");
        assert_eq!(fix.parent(root), None);
        assert_eq!(fix.node(signed), Some(root));
        assert_eq!(fix.node(unsigned), Some(root));
    }

    #[test]
    fn the_unsigned_version_of_an_integer_type_is_the_same_node() {
        let mut fix = Fixture::new();
        let signed = fix.types.int(IntKind::Long);
        let unsigned = fix.types.int(IntKind::ULong);
        assert_eq!(fix.node(signed), fix.node(unsigned));
        // And it is not the same node as the next type up, which is the whole point.
        let wider = fix.types.int(IntKind::LongLong);
        assert_ne!(fix.node(signed), fix.node(wider));
    }

    #[test]
    fn an_enumeration_shares_with_what_it_is_represented_in() {
        let mut fix = Fixture::new();
        let int = fix.types.int(IntKind::Int);
        let id = fix.types.declare_enum(None);
        let enumeration = fix.types.enumeration(id);
        // Nothing is known about it until its definition has been seen, and a node that named a
        // width nobody has decided yet would be a guess.
        assert_eq!(fix.node(enumeration), None);
        fix.types.complete_enum(id, int, false);
        assert_eq!(fix.node(enumeration), fix.node(int));
    }

    #[test]
    fn a_typedef_and_a_qualifier_are_the_type_underneath_them() {
        let mut fix = Fixture::new();
        let long = fix.types.int(IntKind::ULong);
        let name = fix.names.intern("size_t");
        let sugar = fix.types.typedef(name, long);
        let konst = fix.types.qualified(sugar, Qualifiers::CONST);
        assert_eq!(fix.node(sugar), fix.node(long));
        assert_eq!(fix.node(konst), fix.node(long));
    }

    #[test]
    fn every_object_pointer_is_one_node() {
        let mut fix = Fixture::new();
        let int = fix.types.int(IntKind::Int);
        let double = fix.types.float(FloatKind::Double);
        let one = fix.types.pointer(int);
        let other = fix.types.pointer(double);
        let node = fix.node(one).expect("a node for a pointer");
        assert_eq!(fix.name(node), "pointer");
        assert_eq!(fix.node(other), Some(node));
    }

    #[test]
    fn two_floating_types_with_the_same_format_are_still_two_nodes() {
        let mut fix = Fixture::new();
        let real = fix.types.float(FloatKind::Float);
        let named = fix.types.float(FloatKind::Float32);
        assert_ne!(fix.node(real), fix.node(named));
    }

    #[test]
    fn the_types_that_are_reached_by_address_get_no_node() {
        let mut fix = Fixture::new();
        let int = fix.types.int(IntKind::Int);
        let array = fix.types.array(int, ArrayLen::Fixed(4));
        assert_eq!(fix.node(array), None);
        let void = fix.types.void();
        assert_eq!(fix.node(void), None);
    }

    #[test]
    fn asking_twice_for_one_type_adds_one_node() {
        let mut fix = Fixture::new();
        let int = fix.types.int(IntKind::Int);
        let short = fix.types.int(IntKind::Short);
        let first = fix.node(int);
        assert_eq!(fix.node(int), first);
        fix.node(short);
        // The root, `int` and `short`, and nothing else.
        assert_eq!(fix.module.metadata().count(), 3);
    }

    #[test]
    fn a_module_whose_accesses_name_nothing_has_no_tree_at_all() {
        let mut fix = Fixture::new();
        let void = fix.types.void();
        assert_eq!(fix.node(void), None);
        assert_eq!(fix.module.metadata().count(), 0, "not even the root");
    }
}
