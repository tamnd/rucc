//! The vocabulary the type plane is written in, and the number each entry travels as.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` section 9.1.
//!
//! The plane says what every byte was last stored through. The front end already names the type of
//! every access it lowers, which is the aliasing node on the access's payload, and this is the
//! other side of that name: the entry a store records, and the small integer the runtime holds for
//! it.
//!
//! # Why a plane entry is not an aliasing node
//!
//! Three of the things a byte can say are not types. A byte nothing has stored through says so, a
//! byte stored through a character lvalue says that instead of saying `char`, and a byte of a
//! pointer shaped word says which byte of one it is. There is no aliasing node for any of those,
//! so the plane has a node kind of its own that points at an aliasing node when it is a type and
//! says one of the three when it is not.
//!
//! # Why the number is a hash of the name
//!
//! The number is what ends up in the plane, so two objects that were compiled separately have to
//! agree about it or a store in one and a read in the other disagree about a type they both spell
//! the same way. A counter cannot do that: every object numbers its own types from zero, so `int`
//! is 3 in one file and 7 in the next, and the disagreement is a refusal of a program that is
//! correct.
//!
//! The name can. `rucc_lower::aliasing` interns one canonical spelling per type for exactly this
//! reason, so a function of the spelling is the same number in every object anybody builds, with no
//! table to keep and nothing to merge at link time. What it costs is that two spellings can collide
//! and be treated as one type, which is a check that is not made rather than a wrong answer, and
//! the test called `no_two_types_the_front_end_can_spell_share_a_number` is the whole of today's
//! vocabulary saying it does not happen.
//!
//! What is given up is that the plane holds a number nothing can turn back into a name, so a report
//! cannot say `int` yet. It is worth saying that document 09 section 9.1 asks for the opposite, an
//! interned universe the runtime can name a type out of. That wants a table per object and a merge
//! at load time, and neither the reporter that would read it nor the merge exists. The names are
//! recoverable when it does: a hash of a spelling is a spelling the compiler still has.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_ir::{Meta, MetaNode, Module, PlaneNode};

/// The type of a byte nothing has stored through, which `rucc_safe_rt::types::UNTYPED` fixes at
/// zero because a fresh shadow reservation reads as zeroes.
const UNTYPED: u32 = 0;

/// The type of a byte stored through a character lvalue, which is compatible with everything.
///
/// `rucc_safe_rt::types::CHARACTER`.
const CHARACTER: u32 = 1;

/// The first of the eight ids that name a byte of a pointer shaped word.
///
/// `rucc_safe_rt::types::POINTER_FIRST`, which is private there, so the two are kept the same by
/// the same reading a person does for [`FIRST_INTERNED`].
const POINTER_FIRST: u32 = 2;

/// The first id the runtime has not spent on a value of its own.
///
/// `rucc_safe_rt::types::FIRST_INTERNED`, which is the two above plus the eight pointer bytes. It
/// is written out here rather than shared, because the runtime is compiled for the target and this
/// crate is compiled for the host, and there is no crate below both of them to put one constant in.
/// The pair that would break is a store recording 10 and a read asking about 11, which is the same
/// kind of quiet disagreement `xtask interpose` exists for, and there are four numbers here rather
/// than a table of a hundred names.
const FIRST_INTERNED: u32 = POINTER_FIRST + 8;

/// The plane entries one module needs, and the aliasing node each of them is for.
///
/// Built once per module rather than per function, because a metadata node belongs to the module
/// and two stores through the same type have to record the same entry or the plane holds two
/// numbers for one type.
#[derive(Debug)]
pub struct Plane {
    /// What a store that names no type at all records.
    untyped: Meta,
    /// What a store through a character type records.
    character: Meta,
    /// The entry for each aliasing node that is a type.
    types: HashMap<Meta, Meta>,
}

impl Plane {
    /// Gives a module a plane entry for every type its accesses name.
    ///
    /// The aliasing nodes are already in the module, put there by the walk that lowered the
    /// accesses, so this reads them rather than working the types out again. The root of the
    /// aliasing tree is the character type and is the one node that does not become an entry of
    /// its own: C says a store through a character lvalue does not set an effective type, so what
    /// it records is the distinguished value that is compatible with everything.
    ///
    /// The two distinguished entries go in whether or not anything records them. They cost two
    /// nodes in a table that is printed and never loaded, and having them unconditionally means
    /// [`Self::entry`] answers without needing the module back.
    pub fn build(module: &mut Module) -> Self {
        let character = module.add_meta(MetaNode::Plane(PlaneNode::Character));
        let untyped = module.add_meta(MetaNode::Plane(PlaneNode::NoType));
        let named: Vec<Meta> = module
            .metadata()
            .filter(|&meta| module[meta].tbaa().is_some_and(|node| node.parent.is_some()))
            .collect();
        let types = named
            .into_iter()
            .map(|node| (node, module.add_meta(MetaNode::Plane(PlaneNode::Type(node)))))
            .collect();
        Self { untyped, character, types }
    }

    /// The entry a store through `tbaa` records.
    ///
    /// A store that names no type records that the bytes are untyped, which is not the same as
    /// recording nothing: the bytes may have said something before, and a store that left an
    /// earlier type standing over what it just wrote is how a plane comes to describe memory that
    /// is no longer there. Untyped is compatible with every access, so what this costs is a check
    /// that is not made, which is the direction a plane is allowed to be wrong in.
    #[must_use]
    pub fn entry(&self, tbaa: Option<Meta>) -> Meta {
        match tbaa.and_then(|node| self.types.get(&node)) {
            Some(&entry) => entry,
            // The root of the aliasing tree, or a node from some module this one did not build.
            None if tbaa.is_some() => self.character,
            None => self.untyped,
        }
    }
}

/// What each plane entry in a module is called in the plane itself.
///
/// One walk of the metadata table rather than a lookup per instruction, because the table is a
/// handful of nodes and the instructions are every store in the module.
pub(crate) fn numbers(module: &Module, names: &Interner) -> HashMap<Meta, u32> {
    module
        .metadata()
        .filter_map(|meta| Some((meta, number(module, names, module[meta].plane()?))))
        .collect()
}

/// The number one plane entry travels as.
fn number(module: &Module, names: &Interner, node: PlaneNode) -> u32 {
    match node {
        PlaneNode::NoType => UNTYPED,
        PlaneNode::Character => CHARACTER,
        PlaneNode::PointerSlot(k) => POINTER_FIRST + u32::from(k),
        // A plane entry that names a type points at an aliasing node, and the verifier has already
        // said that it does, so a node that is not one is a module nobody could have built.
        PlaneNode::Type(ty) => match module[ty].tbaa() {
            Some(node) => identifier(names.resolve(node.name)),
            None => UNTYPED,
        },
    }
}

/// The number the type spelled `name` travels as.
///
/// Biased past what the runtime has spent on the values that are not types, so that the type whose
/// spelling happens to hash to zero is not read back as a byte nobody has stored through.
fn identifier(name: &str) -> u32 {
    FIRST_INTERNED + fnv(name) % (u32::MAX - FIRST_INTERNED)
}

/// FNV-1a over the bytes of a name.
///
/// Chosen because it is eight lines and the two ends of it have to agree forever. Anything with a
/// seed, a table or a word size that depends on the host is a thing that can be built two ways, and
/// the number this produces is written into memory one object reads and another one wrote.
fn fnv(name: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &byte in name.as_bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use rucc_ir::TbaaNode;
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;

    /// Every spelling `rucc_lower::aliasing` can produce today.
    ///
    /// Written out rather than reached for, because that crate is the same rank as this one and
    /// neither can depend on the other. A spelling added there and not here is not a failure: it
    /// hashes to a number of its own without anything here knowing about it, which is the point of
    /// there being no table. This list is what the collision test is run against, and a type whose
    /// spelling is missing from it is a type nobody has checked for a collision.
    const VOCABULARY: &[&str] = &[
        "bool",
        "short",
        "int",
        "long",
        "long long",
        "__int128",
        "_Float16",
        "float",
        "_Float32",
        "double",
        "_Float32x",
        "_Float64",
        "long double",
        "_Float64x",
        "_Float128",
        "_Complex float",
        "_Complex double",
        "_Complex long double",
        "pointer",
        "_BitInt(2)",
        "_BitInt(7)",
        "_BitInt(64)",
        "_BitInt(128)",
    ];

    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        Module::new(names.intern("plane.c"), &target)
    }

    /// The aliasing tree the front end builds: a root called `char` with the types under it.
    fn tree(module: &mut Module, names: &mut Interner, under: &[&str]) -> (Meta, Vec<Meta>) {
        let name = names.intern("char");
        let root = module.add_meta(MetaNode::Tbaa(TbaaNode { name, parent: None, offset: 0 }));
        let nodes = under
            .iter()
            .map(|spelling| {
                let name = names.intern(spelling);
                module.add_meta(MetaNode::Tbaa(TbaaNode { name, parent: Some(root), offset: 0 }))
            })
            .collect();
        (root, nodes)
    }

    #[test]
    fn no_two_types_the_front_end_can_spell_share_a_number() {
        // The one thing the hash has to do. Two types sharing a number is a check that is not
        // made, which is why this is a test of today's vocabulary rather than a proof, but a
        // collision inside a list this short would be bad luck worth knowing about.
        let mut seen: Vec<u32> = VOCABULARY.iter().map(|name| identifier(name)).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two of {VOCABULARY:?} hash to one number");
    }

    #[test]
    fn no_type_is_numbered_as_one_of_the_values_that_is_not_a_type() {
        // The bias. `UNTYPED` in particular is what untouched shadow memory reads as, so a type
        // that landed on it would be a type every access was permitted against.
        for name in VOCABULARY {
            assert!(identifier(name) >= FIRST_INTERNED, "{name} is numbered as a reserved value");
        }
    }

    #[test]
    fn the_same_spelling_is_the_same_number_in_two_modules() {
        // What a counter cannot do, and the whole reason this is a hash. The two modules here
        // stand in for two objects built at different times by different runs of the compiler.
        let mut names = Interner::new();
        let mut one = module(&mut names);
        let (_, first) = tree(&mut one, &mut names, &["long", "int"]);
        let mut two = module(&mut names);
        let (_, second) = tree(&mut two, &mut names, &["int"]);

        let one_plane = Plane::build(&mut one);
        let two_plane = Plane::build(&mut two);
        let first = numbers(&one, &names)[&one_plane.entry(Some(first[1]))];
        let second = numbers(&two, &names)[&two_plane.entry(Some(second[0]))];
        assert_eq!(first, second);
    }

    #[test]
    fn a_store_through_a_character_type_records_that_and_not_the_type() {
        // C 6.5: a store through a character lvalue does not set an effective type. The root of
        // the aliasing tree is the character type, so this is the node the front end hands over
        // for `*(char *)p = 0` and it has to come back as the distinguished value rather than as
        // a type called `char`.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (root, _) = tree(&mut module, &mut names, &["int"]);
        let plane = Plane::build(&mut module);
        assert_eq!(module[plane.entry(Some(root))], MetaNode::Plane(PlaneNode::Character));
        assert_eq!(numbers(&module, &names)[&plane.entry(Some(root))], CHARACTER);
    }

    #[test]
    fn a_store_that_names_no_type_records_that_the_bytes_are_untyped() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        tree(&mut module, &mut names, &["int"]);
        let plane = Plane::build(&mut module);
        assert_eq!(module[plane.entry(None)], MetaNode::Plane(PlaneNode::NoType));
        assert_eq!(numbers(&module, &names)[&plane.entry(None)], UNTYPED);
    }

    #[test]
    fn two_stores_through_one_type_record_one_entry() {
        // The reason the entries are built per module. Two entries for one type would be two
        // numbers for one type, and the second store would make the first one's bytes unreadable.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (_, nodes) = tree(&mut module, &mut names, &["int"]);
        let plane = Plane::build(&mut module);
        assert_eq!(plane.entry(Some(nodes[0])), plane.entry(Some(nodes[0])));
    }

    #[test]
    fn every_type_in_the_module_gets_an_entry_that_points_back_at_it() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (_, nodes) = tree(&mut module, &mut names, &["int", "float"]);
        let plane = Plane::build(&mut module);
        for node in nodes {
            assert_eq!(module[plane.entry(Some(node))], MetaNode::Plane(PlaneNode::Type(node)));
        }
    }
}
