//! The type signatures of kencc, which `signof` gives and `-T` writes in an object.
//!
//! Design: tamnd/rucc#4157, and 03.13 and 07.4 of the rucc-plan9 plan.
//!
//! `signat` in `cc/dcl.c` hashes a type as a walk over kencc's type tree. Each node adds its
//! kind, an array adds nothing for its length, a function adds each parameter, and a structure
//! adds each member. A structure seen before in the same walk adds its place in a table instead,
//! which is how a list that points to itself ends. The table is `sigind`, and it finds a structure
//! with `sametype`, which compares the members and not the tags. So two structures with the same
//! members give the same index, and that must be the same here.
//!
//! The linkers `6l` and `7l` compare the signatures of a symbol in two objects and stop when they
//! differ. So every rule here is bit for bit the rule of kencc, quirks included:
//! - a pointer to `void` is the same as every pointer in `sametype`;
//! - `sametype` stops at a depth of five and calls what is below the same;
//! - an enumeration is its integer type;
//! - `(void)` is one `void` parameter, `...` is one more parameter, and `()` is none;
//! - a structure named in `#pragma incomplete` adds its kind and stops.
//!
//! rucc's types are not kencc's tree, so [`Tree`] gives the same view over them. A node is a type
//! together with the member or the parameter it is, because in kencc each member and each
//! parameter is a node of its own, and the next one is found from it.

use rucc_base::Symbol;
use rucc_target::TargetInfo;

use crate::{
    FloatKind, FunctionId, IntKind, Qualifiers, RecordId, RecordKind, TypeId, TypeKind, Types,
};

/// The multiplier before the kind of each node, `thash1` in `cc/sub.c`.
const THASH1: u32 = 0x2eda_b8c9;
/// The multiplier before a length or a table index, `thash2`.
const THASH2: u32 = 0x1dc7_4fb8;
/// The multiplier before a parameter or a member, `thash3`.
const THASH3: u32 = 0x1f24_1331;

/// What `sign` in `cc/dcl.c` gives a symbol whose signature is zero, so that zero always means
/// that an object has no signature for it. It is `1729*325*1729`.
pub const KENCC_SIGN_INTERN: u32 = 1729 * 325 * 1729;

/// The kind of a node, kencc's `etype`, with the hash `thashinit` in `cc/sub.c` gives each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Et {
    Xxx,
    Char,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    VLong,
    UVLong,
    Float,
    Double,
    Ind,
    Func,
    Array,
    Void,
    Struct,
    Union,
    Old,
    Dot,
}

impl Et {
    /// `tnames[etype]` in `cc/sub.c`, which `%T` prints.
    fn name(self) -> &'static str {
        match self {
            Et::Xxx => "TXXX",
            Et::Char => "CHAR",
            Et::UChar => "UCHAR",
            Et::Short => "SHORT",
            Et::UShort => "USHORT",
            Et::Int => "INT",
            Et::UInt => "UINT",
            Et::Long => "LONG",
            Et::ULong => "ULONG",
            Et::VLong => "VLONG",
            Et::UVLong => "UVLONG",
            Et::Float => "FLOAT",
            Et::Double => "DOUBLE",
            Et::Ind => "IND",
            Et::Func => "FUNC",
            Et::Array => "ARRAY",
            Et::Void => "VOID",
            Et::Struct => "STRUCT",
            Et::Union => "UNION",
            Et::Old => "OLD",
            Et::Dot => "DOT",
        }
    }

    /// `thash[etype]`.
    fn hash(self) -> u32 {
        match self {
            Et::Xxx => 0x1752_7bbd,
            Et::Char => 0x5ced_d32b,
            Et::UChar => 0x552c_4454,
            Et::Short => 0x6304_0b4b,
            Et::UShort => 0x32a4_5878,
            Et::Int => 0x4151_d5bd,
            Et::UInt => 0x5ae7_07d6,
            Et::Long => 0x5ef2_0f47,
            Et::ULong => 0x36d8_eb8f,
            Et::VLong => 0x6e5e_9590,
            Et::UVLong => 0x7591_0105,
            Et::Float => 0x25fd_7af1,
            Et::Double => 0x7c40_a1b2,
            Et::Ind => 0x1b83_2357,
            Et::Func => 0x6bab_c9cb,
            Et::Array => 0x7c50_986d,
            Et::Void => 0x4411_2eff,
            Et::Struct => 0x7c2d_a3bf,
            Et::Union => 0x3eb2_5e98,
            Et::Old => 0x22b1_5988,
            Et::Dot => 0x0204_f6b3,
        }
    }
}

/// What a node is: a type, or one of the three marks kencc puts in a parameter list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    Type(TypeId),
    /// The one parameter of `(void)`.
    Void,
    /// The first parameter of a function defined in the old style, `TOLD`.
    Old,
    /// `...`, `TDOT`.
    Dot,
}

/// Which list a node is in, so that its `down` can be found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    None,
    Member(RecordId, usize),
    Param(FunctionId, usize),
}

/// One node of kencc's tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Node {
    what: What,
    next: Next,
}

impl Node {
    fn of(ty: TypeId) -> Node {
        Node { what: What::Type(ty), next: Next::None }
    }
}

/// kencc's view of rucc's types.
struct Tree<'a> {
    types: &'a Types,
    target: &'a TargetInfo,
    /// `debug['V']`, which is `-V`.
    strict_void: bool,
    incomplete: &'a dyn Fn(RecordId) -> bool,
}

impl Tree<'_> {
    /// The type behind typedefs, `_Atomic` and enumerations, which kencc does not keep apart.
    fn resolve(&self, mut ty: TypeId) -> TypeId {
        loop {
            ty = self.types.canonical(ty);
            ty = match self.types.kind(ty) {
                TypeKind::Atomic(inner) => inner,
                TypeKind::Enum(id) => match self.types.enum_info(id).underlying {
                    Some(underlying) => underlying,
                    None => return self.types.int(IntKind::Int),
                },
                _ => return ty,
            };
        }
    }

    fn kind(&self, node: Node) -> Option<TypeKind> {
        match node.what {
            What::Type(ty) => Some(self.types.kind(self.resolve(ty))),
            _ => None,
        }
    }

    fn et(&self, node: Node) -> Et {
        let kind = match node.what {
            What::Type(_) => self.kind(node),
            What::Void => return Et::Void,
            What::Old => return Et::Old,
            What::Dot => return Et::Dot,
        };
        match kind {
            Some(TypeKind::Void) => Et::Void,
            Some(TypeKind::Int(int)) => match int {
                IntKind::Char | IntKind::SChar => Et::Char,
                IntKind::UChar => Et::UChar,
                IntKind::Short => Et::Short,
                IntKind::UShort => Et::UShort,
                IntKind::Int => Et::Int,
                IntKind::UInt => Et::UInt,
                IntKind::Long => Et::Long,
                IntKind::ULong => Et::ULong,
                IntKind::LongLong => Et::VLong,
                IntKind::ULongLong => Et::UVLong,
                IntKind::Int128 | IntKind::UInt128 => Et::Xxx,
            },
            Some(TypeKind::Float(float)) => match float {
                FloatKind::Float | FloatKind::Float32 => Et::Float,
                // kencc reads `long double` as `double`.
                FloatKind::Double | FloatKind::Float64 | FloatKind::LongDouble => Et::Double,
                _ => Et::Xxx,
            },
            Some(TypeKind::Pointer(_)) => Et::Ind,
            Some(TypeKind::Array { .. }) => Et::Array,
            Some(TypeKind::Function(_)) => Et::Func,
            Some(TypeKind::Record(id)) => match self.types.record_info(id).kind {
                RecordKind::Struct => Et::Struct,
                RecordKind::Union => Et::Union,
            },
            // kencc has no `_Bool`, no complex type, no `_BitInt` and no vector.
            _ => Et::Xxx,
        }
    }

    /// `t->link`: what a pointer points to, the element of an array, what a function returns, and
    /// the first member of a structure.
    fn link(&self, node: Node) -> Option<Node> {
        match self.kind(node)? {
            TypeKind::Pointer(to) => Some(Node::of(to)),
            TypeKind::Array { elem, .. } => Some(Node::of(elem)),
            TypeKind::Function(id) => Some(Node::of(self.types.signature(id).ret)),
            TypeKind::Record(id) => self.member(id, 0),
            _ => None,
        }
    }

    /// `t->down` of a member or a parameter, which is the next one.
    fn down(&self, node: Node) -> Option<Node> {
        match node.next {
            Next::None => None,
            Next::Member(id, index) => self.member(id, index + 1),
            Next::Param(id, index) => self.param(id, index + 1),
        }
    }

    /// `t->down` of a function, which is its first parameter.
    fn params(&self, node: Node) -> Option<Node> {
        match self.kind(node)? {
            TypeKind::Function(id) => self.param(id, 0),
            _ => None,
        }
    }

    fn member(&self, id: RecordId, index: usize) -> Option<Node> {
        let field = self.types.record_info(id).fields.get(index)?;
        Some(Node { what: What::Type(field.ty), next: Next::Member(id, index) })
    }

    /// The parameter at `index` of the list kencc builds in `fnproto` and `argmark`.
    fn param(&self, id: FunctionId, index: usize) -> Option<Node> {
        let sig = self.types.signature(id);
        let what = if !sig.prototyped {
            // `()` has no list. A definition in the old style has `TOLD` and then its parameters.
            match index {
                _ if sig.params.is_empty() => return None,
                0 => What::Old,
                _ => What::Type(*sig.params.get(index - 1)?),
            }
        } else if sig.params.is_empty() && !sig.variadic {
            match index {
                0 => What::Void,
                _ => return None,
            }
        } else if let Some(&ty) = sig.params.get(index) {
            What::Type(ty)
        } else if sig.variadic && index == sig.params.len() {
            What::Dot
        } else {
            return None;
        };
        Some(Node { what, next: Next::Param(id, index) })
    }

    /// `t->width` of an array, which is zero when the length is not known.
    fn width(&self, node: Node) -> u64 {
        match node.what {
            What::Type(ty) => crate::layout(self.types, ty, self.target).map_or(0, |l| l.size),
            _ => 0,
        }
    }

    fn record(&self, node: Node) -> Option<RecordId> {
        match self.kind(node)? {
            TypeKind::Record(id) => Some(id),
            _ => None,
        }
    }

    /// Whether `#pragma incomplete` named the structure, `GINCOMPLETE` in `t->garb`.
    fn marked(&self, node: Node) -> bool {
        self.record(node).is_some_and(|id| (self.incomplete)(id))
    }

    /// `signat` in `cc/dcl.c`.
    fn signat(&self, mut t: Option<Node>, table: &mut Vec<Node>) -> u32 {
        let mut s: u32 = 0;
        while let Some(node) = t {
            let et = self.et(node);
            s = s.wrapping_mul(THASH1).wrapping_add(et.hash());
            if self.marked(node) {
                return s;
            }
            match et {
                // The length was in the hash once, and kencc took it out.
                Et::Array => s = s.wrapping_mul(THASH2),
                Et::Func => {
                    let mut param = self.params(node);
                    while let Some(p) = param {
                        s = s.wrapping_mul(THASH3).wrapping_add(self.signat(Some(p), table));
                        param = self.down(p);
                    }
                }
                Et::Struct | Et::Union => {
                    if let Some(index) = self.sigind(node, table) {
                        return s.wrapping_mul(THASH2).wrapping_add(index);
                    }
                    let mut member = self.link(node);
                    while let Some(m) = member {
                        s = s.wrapping_mul(THASH3).wrapping_add(self.signat(Some(m), table));
                        member = self.down(m);
                    }
                    return s;
                }
                Et::Ind => {}
                _ => return s,
            }
            t = self.link(node);
        }
        s
    }

    /// `sigind` in `cc/dcl.c`: the place of a structure seen before in this walk, or nothing
    /// after it is added.
    fn sigind(&self, t: Node, table: &mut Vec<Node>) -> Option<u32> {
        if let Some(index) = table.iter().position(|&seen| self.sametype(seen, t)) {
            return u32::try_from(index).ok();
        }
        table.push(t);
        None
    }

    /// `sametype` in `cc/dcl.c`.
    fn sametype(&self, t1: Node, t2: Node) -> bool {
        t1 == t2 || self.rsametype(Some(t1), Some(t2), 5, true)
    }

    /// `rsametype` in `cc/dcl.c`. A node in rucc is the same node only where kencc's would be
    /// the same too or would compare the same.
    ///
    /// `top` is kencc's `f`, which is set only in the first call. A pointer to `void` is the same
    /// as every pointer there, and below it only without `-V`.
    fn rsametype(&self, mut t1: Option<Node>, mut t2: Option<Node>, n: i32, top: bool) -> bool {
        let n = n - 1;
        loop {
            if t1 == t2 {
                return true;
            }
            let (Some(a), Some(b)) = (t1, t2) else { return false };
            if n <= 0 {
                return true;
            }
            let et = self.et(a);
            if et != self.et(b) {
                return false;
            }
            if et == Et::Func {
                if !self.rsametype(self.link(a), self.link(b), n, false) {
                    return false;
                }
                let (mut p1, mut p2) = (self.params(a), self.params(b));
                while let (Some(x), Some(y)) = (p1, p2) {
                    if self.et(x) == Et::Old {
                        p1 = self.down(x);
                        continue;
                    }
                    if self.et(y) == Et::Old {
                        p2 = self.down(y);
                        continue;
                    }
                    while p1.is_some() || p2.is_some() {
                        if !self.rsametype(p1, p2, n, false) {
                            return false;
                        }
                        p1 = p1.and_then(|p| self.down(p));
                        p2 = p2.and_then(|p| self.down(p));
                    }
                    break;
                }
                return true;
            }
            if et == Et::Array {
                let (w1, w2) = (self.width(a), self.width(b));
                if w1 != w2 && w1 != 0 && w2 != 0 {
                    return false;
                }
            }
            if matches!(et, Et::Struct | Et::Union) {
                let (m1, m2) = (self.link(a), self.link(b));
                if m1.is_none() && m2.is_none() {
                    // Structures with no members compare by tag, and one with no tag is
                    // different from all others.
                    let tag =
                        |node| self.record(node).and_then(|id| self.types.record_info(id).tag);
                    if tag(a).is_none() || tag(a) != tag(b) {
                        return false;
                    }
                }
                let (mut m1, mut m2) = (m1, m2);
                loop {
                    if m1 == m2 {
                        return true;
                    }
                    if !self.rsametype(m1, m2, n, false) {
                        return false;
                    }
                    m1 = m1.and_then(|m| self.down(m));
                    m2 = m2.and_then(|m| self.down(m));
                }
            }
            t1 = self.link(a);
            t2 = self.link(b);
            if (top || !self.strict_void)
                && et == Et::Ind
                && [t1, t2].iter().flatten().any(|&pointee| self.et(pointee) == Et::Void)
            {
                return true;
            }
        }
    }

    /// `Tconv` in `cc/lex.c`, which prints a type in the messages of kencc as the kinds of the
    /// nodes, such as `IND CONST CHAR`.
    fn text(&self, mut t: Node, tag: &dyn Fn(Symbol) -> String, out: &mut String) {
        loop {
            if let What::Type(ty) = t.what {
                let quals = self.types.quals(self.types.canonical(ty));
                let garb = (quals.has(Qualifiers::CONST), quals.has(Qualifiers::VOLATILE));
                out.push_str(match garb {
                    (true, true) => "CONST-VOLATILE ",
                    (true, false) => "CONST ",
                    (false, true) => "VOLATILE ",
                    (false, false) => "",
                });
            }
            let et = self.et(t);
            out.push_str(et.name());
            if let (Et::Func, Some(first)) = (et, self.params(t)) {
                out.push('(');
                self.text(first, tag, out);
                let mut param = self.down(first);
                while let Some(p) = param {
                    out.push_str(", ");
                    self.text(p, tag, out);
                    param = self.down(p);
                }
                out.push(')');
            }
            if et == Et::Array {
                let elem = self.link(t).map_or(0, |elem| self.width(elem));
                let width = self.width(t);
                let n = width.checked_div(elem).unwrap_or(width);
                out.push_str(&format!("[{n}]"));
            }
            if matches!(et, Et::Struct | Et::Union) {
                let name = self.record(t).and_then(|id| self.types.record_info(id).tag);
                out.push(' ');
                out.push_str(&name.map_or_else(|| "{}".to_owned(), tag));
                return;
            }
            let Some(next) = self.link(t) else { return };
            out.push(' ');
            t = next;
        }
    }
}

/// The signature of a type, as `signof` and `signature` in `cc/dcl.c` give it.
///
/// `strict_void` is kencc's `-V`, which `sametype` reads when two structures are compared.
/// `incomplete` says whether `#pragma incomplete` named a structure or a union. Array lengths
/// in `sametype` come from the layout of the type on `target`.
#[must_use]
pub fn kencc_signature(
    types: &Types,
    ty: TypeId,
    target: &TargetInfo,
    strict_void: bool,
    incomplete: &dyn Fn(RecordId) -> bool,
) -> u32 {
    let tree = Tree { types, target, strict_void, incomplete };
    tree.signat(Some(Node::of(ty)), &mut Vec::new())
}

/// Whether kencc's `sametype` in `cc/dcl.c` calls two types the same. `strict_void` is `-V`.
#[must_use]
pub fn kencc_same_type(
    types: &Types,
    target: &TargetInfo,
    strict_void: bool,
    t1: TypeId,
    t2: TypeId,
) -> bool {
    let tree = Tree { types, target, strict_void, incomplete: &|_| false };
    tree.sametype(Node::of(t1), Node::of(t2))
}

/// A type as kencc's `%T` prints it, such as `IND CONST CHAR` or `STRUCT Point`. `tag` gives
/// the text of a tag.
#[must_use]
pub fn kencc_type_text(
    types: &Types,
    target: &TargetInfo,
    ty: TypeId,
    tag: &dyn Fn(Symbol) -> String,
) -> String {
    let tree = Tree { types, target, strict_void: false, incomplete: &|_| false };
    let mut out = String::new();
    tree.text(Node::of(ty), tag, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_target::{Convention, Triple};

    use super::*;
    use crate::{ArrayLen, FieldDecl, FunctionType, RecordOptions, Types, layout_record};

    /// The values come from `signat`, `sigind` and `rsametype` of `cc/dcl.c` and the table of
    /// `cc/sub.c`, compiled as they are with `long` as 32 bits, over the same trees built by hand.
    /// See tamnd/rucc#4157 for the program.
    struct Fixture {
        types: Types,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        fn new() -> Fixture {
            let triple = "x86_64-unknown-plan9".parse::<Triple>().expect("the Plan 9 triple");
            Fixture { types: Types::new(), names: Interner::new(), target: TargetInfo::new(triple) }
        }

        fn int(&self, kind: IntKind) -> TypeId {
            self.types.int(kind)
        }

        fn function(
            &mut self,
            ret: TypeId,
            params: &[TypeId],
            variadic: bool,
            proto: bool,
        ) -> TypeId {
            self.types.function(FunctionType {
                ret,
                params: params.to_vec(),
                variadic,
                prototyped: proto,
                convention: Convention::default(),
                nocf: false,
                indirect_return: false,
                return_pointer_popped: None,
                sse_regparm: false,
                sme: crate::Sme::NONE,
            })
        }

        fn declare(&mut self, kind: RecordKind, tag: &str) -> RecordId {
            let tag = self.names.intern(tag);
            self.types.declare_record(kind, Some(tag))
        }

        fn complete(&mut self, id: RecordId, members: &[TypeId]) -> TypeId {
            let kind = self.types.record_info(id).kind;
            let fields: Vec<FieldDecl> =
                members.iter().map(|&ty| FieldDecl::new(None, ty)).collect();
            let laid_out =
                layout_record(&self.types, kind, &fields, &RecordOptions::default(), &self.target)
                    .expect("the members have a layout");
            self.types.complete_record(id, laid_out);
            self.types.record(id)
        }

        fn structure(&mut self, tag: &str, members: &[TypeId]) -> TypeId {
            let id = self.declare(RecordKind::Struct, tag);
            self.complete(id, members)
        }

        fn sign(&self, ty: TypeId) -> u32 {
            kencc_signature(&self.types, ty, &self.target, false, &|_| false)
        }
    }

    #[test]
    fn a_basic_type_is_its_own_hash() {
        let f = Fixture::new();
        let cases = [
            (IntKind::Int, 0x4151_d5bd),
            (IntKind::Char, 0x5ced_d32b),
            (IntKind::SChar, 0x5ced_d32b),
            (IntKind::UChar, 0x552c_4454),
            (IntKind::Short, 0x6304_0b4b),
            (IntKind::UShort, 0x32a4_5878),
            (IntKind::UInt, 0x5ae7_07d6),
            (IntKind::Long, 0x5ef2_0f47),
            (IntKind::ULong, 0x36d8_eb8f),
            (IntKind::LongLong, 0x6e5e_9590),
            (IntKind::ULongLong, 0x7591_0105),
        ];
        for (kind, want) in cases {
            assert_eq!(f.sign(f.int(kind)), want, "{kind:?}");
        }
        assert_eq!(f.sign(f.types.float(FloatKind::Float)), 0x25fd_7af1);
        assert_eq!(f.sign(f.types.float(FloatKind::Double)), 0x7c40_a1b2);
        assert_eq!(f.sign(f.types.float(FloatKind::LongDouble)), 0x7c40_a1b2);
        assert_eq!(f.sign(f.types.void()), 0x4411_2eff);
    }

    #[test]
    fn pointers_arrays_and_functions_walk_down_the_tree() {
        let mut f = Fixture::new();
        let (int, char, void) = (f.int(IntKind::Int), f.int(IntKind::Char), f.types.void());
        let char_ptr = f.types.pointer(char);
        assert_eq!(f.sign(char_ptr), 0xf261_1a7a);
        let int_ptr = f.types.pointer(int);
        let int_ptr_ptr = f.types.pointer(int_ptr);
        assert_eq!(f.sign(int_ptr_ptr), 0xf4ce_e213);
        let void_ptr = f.types.pointer(void);
        assert_eq!(f.sign(void_ptr), 0xd984_764e);
        let array = f.types.array(int, ArrayLen::Fixed(10));
        assert_eq!(f.sign(array), 0xb3a1_d3d5);

        let no_params = f.function(int, &[], false, true);
        assert_eq!(f.sign(no_params), 0x49c2_47e7);
        let two = f.function(int, &[int, char_ptr], false, true);
        assert_eq!(f.sign(two), 0x33cd_455f);
        let print = f.function(int, &[char_ptr], true, true);
        assert_eq!(f.sign(print), 0x0091_8875);
        let unprototyped = f.function(int, &[], false, false);
        assert_eq!(f.sign(unprototyped), 0x951b_2e20);
        let old = f.function(int, &[int, char_ptr], false, false);
        assert_eq!(f.sign(old), 0x82e3_25b7);
        let callback = f.function(void, &[int], false, true);
        let callback_ptr = f.types.pointer(callback);
        assert_eq!(f.sign(callback_ptr), 0xc563_57ae);
    }

    #[test]
    fn a_structure_adds_its_members_and_a_second_visit_adds_its_index() {
        let mut f = Fixture::new();
        let (int, char) = (f.int(IntKind::Int), f.int(IntKind::Char));
        let char_ptr = f.types.pointer(char);
        let s = f.structure("S", &[int, char_ptr]);
        assert_eq!(f.sign(s), 0xe717_0706);

        let id = f.declare(RecordKind::Struct, "List");
        let list = f.types.record(id);
        let next = f.types.pointer(list);
        let list = f.complete(id, &[next, int]);
        assert_eq!(f.sign(list), 0xba1b_092c);

        let id = f.declare(RecordKind::Union, "U");
        let double = f.types.float(FloatKind::Double);
        let union = f.complete(id, &[int, double]);
        assert_eq!(f.sign(union), 0x7762_5177);

        let name = f.types.array(char, ArrayLen::Fixed(28));
        let vlong = f.int(IntKind::LongLong);
        let named = f.structure("Named", &[name, vlong]);
        assert_eq!(f.sign(named), 0xa9c2_98c2);
    }

    #[test]
    fn the_table_finds_a_structure_by_its_members_and_not_its_tag() {
        let mut f = Fixture::new();
        let (int, char, void) = (f.int(IntKind::Int), f.int(IntKind::Char), f.types.void());
        let a = f.structure("A", &[int]);
        let b = f.structure("B", &[int]);
        let p = f.structure("P", &[a, b]);
        assert_eq!(f.sign(p), 0xa71f_8b34);

        // A pointer to `void` is the same as a pointer to anything.
        let void_ptr = f.types.pointer(void);
        let int_ptr = f.types.pointer(int);
        let q = f.structure("Q", &[void_ptr]);
        let r = f.structure("R", &[int_ptr]);
        let qr = f.structure("QR", &[q, r]);
        assert_eq!(f.sign(qr), 0xae1f_09f5);

        // Five levels down the comparison stops and calls the rest the same.
        let v1 = f.structure("V1", &[int]);
        let z1 = f.structure("Z1", &[v1]);
        let y1 = f.structure("Y1", &[z1]);
        let x1 = f.structure("X1", &[y1]);
        let w1 = f.structure("W1", &[x1]);
        let v2 = f.structure("V2", &[char]);
        let z2 = f.structure("Z2", &[v2]);
        let y2 = f.structure("Y2", &[z2]);
        let x2 = f.structure("X2", &[y2]);
        let w2 = f.structure("W2", &[x2]);
        let deep = f.structure("Deep", &[w1, w2]);
        assert_eq!(f.sign(deep), 0xeae1_1600);
        // Less deep, the difference is seen.
        let x3 = f.structure("X3", &[v2]);
        let shallow = f.structure("Shallow", &[z1, x3]);
        assert_eq!(f.sign(shallow), 0xd058_c193);
    }

    #[test]
    fn a_structure_with_no_members_is_found_by_its_tag() {
        let mut f = Fixture::new();
        let opaque = f.declare(RecordKind::Struct, "Opaque");
        let opaque = f.types.record(opaque);
        let opaque_ptr = f.types.pointer(opaque);
        assert_eq!(f.sign(opaque_ptr), 0x11a0_eb0e);

        let o1 = f.declare(RecordKind::Struct, "O1");
        let o1 = f.types.record(o1);
        let o2 = f.declare(RecordKind::Struct, "O2");
        let o2 = f.types.record(o2);
        let (p1, p2) = (f.types.pointer(o1), f.types.pointer(o2));
        let oo = f.structure("OO", &[p1, p2, p1]);
        assert_eq!(f.sign(oo), 0x169f_093c);
    }

    #[test]
    fn a_structure_named_in_pragma_incomplete_stops_the_walk() {
        let mut f = Fixture::new();
        let int = f.int(IntKind::Int);
        let id = f.declare(RecordKind::Struct, "Inc");
        let inc = f.complete(id, &[int]);
        let inc_ptr = f.types.pointer(inc);
        let marked = kencc_signature(&f.types, inc_ptr, &f.target, false, &|record| record == id);
        assert_eq!(marked, 0x11a0_eb0e);
        assert_ne!(f.sign(inc_ptr), marked);
    }

    #[test]
    fn the_intern_value_is_the_one_kencc_writes() {
        assert_eq!(KENCC_SIGN_INTERN, 971_568_325);
    }

    #[test]
    fn strict_void_keeps_a_pointer_to_void_apart_only_below_the_top() {
        let mut f = Fixture::new();
        let (void, char) = (f.types.void(), f.int(IntKind::Char));
        let (pv, pc) = (f.types.pointer(void), f.types.pointer(char));
        let a = f.structure("A", &[pv]);
        let b = f.structure("B", &[pc]);
        for strict in [false, true] {
            assert!(kencc_same_type(&f.types, &f.target, strict, pv, pc));
        }
        assert!(kencc_same_type(&f.types, &f.target, false, a, b));
        assert!(!kencc_same_type(&f.types, &f.target, true, a, b));
        // The table of `sigind` finds `B` as `A` only without `-V`.
        let (pa, pb) = (f.types.pointer(a), f.types.pointer(b));
        let ab = f.structure("AB", &[pa, pb]);
        let loose = kencc_signature(&f.types, ab, &f.target, false, &|_| false);
        let strict = kencc_signature(&f.types, ab, &f.target, true, &|_| false);
        assert_ne!(loose, strict);
    }

    #[test]
    fn a_type_prints_as_the_kinds_of_its_nodes() {
        let mut f = Fixture::new();
        let char = f.int(IntKind::Char);
        let konst = f.types.qualified(char, Qualifiers::CONST);
        let string = f.types.pointer(konst);
        let int = f.int(IntKind::Int);
        let point = f.structure("Point", &[int]);
        let list = f.types.array(int, ArrayLen::Fixed(4));
        let void = f.types.void();
        let func = f.function(void, &[int, string], true, true);
        let point_ptr = f.types.pointer(point);
        let names = &f.names;
        let tag = |name| names.resolve(name).to_owned();
        let text = |ty| kencc_type_text(&f.types, &f.target, ty, &tag);
        assert_eq!(text(string), "IND CONST CHAR");
        assert_eq!(text(point_ptr), "IND STRUCT Point");
        assert_eq!(text(list), "ARRAY[4] INT");
        assert_eq!(text(func), "FUNC(INT, IND CONST CHAR, DOT) VOID");
    }
}
