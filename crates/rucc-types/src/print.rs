//! Spelling a type the way a person would write it.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.1.
//!
//! Every diagnostic that mentions a type needs one of these, and so does the typed tree's
//! textual form. The rule the type table already follows is that a semantic decision reads the
//! canonical type and a message reads the type as written, so this prints the sugar: a
//! `size_t` prints as `size_t` and not as `unsigned long`, and a caller that wants both prints
//! the canonical form as well, which is what gcc's `size_t {aka long unsigned int}` is doing.
//!
//! A type is not a string of words in C. It is a declaration with a hole in it, and the hole is
//! where the name goes, so `int (*f[3])(char)` is the way an array of pointers to functions is
//! written and there is no other. That is why this is assembled outward from the hole rather
//! than printed left to right, and why the abstract form is the same algorithm with an empty
//! hole, which gives `int (*)[3]` with the parentheses still in it. Both spellings were
//! measured against gcc rather than recalled.
//!
//! The one place the two spellings differ in more than the name is the space in front of the
//! declarator. gcc writes `int[3]` and `int(void)` with nothing between them, because there is
//! nothing there to separate, and writes `int (*)[3]` with a space, because there is. That is
//! what [`Declarator::glued`] carries, and it is why an array of a type and a declaration of
//! one do not look the same.
//!
//! ```
//! use rucc_base::Interner;
//! use rucc_types::{ArrayLen, IntKind, Types, spell};
//!
//! let interner = Interner::new();
//! let mut types = Types::new();
//! let int = types.int(IntKind::Int);
//! let array = types.array(int, ArrayLen::Fixed(3));
//! let pointer = types.pointer(array);
//!
//! assert_eq!(spell(&types, &interner, pointer), "int (*)[3]");
//! ```

use rucc_base::{Interner, Symbol};

use crate::kind::{ArrayLen, FunctionId, Qualifiers, Type, TypeKind};
use crate::types::{TypeId, Types};

/// The type as a type name, which is how it would be written in a cast or in a `sizeof`.
#[must_use]
pub fn spell(types: &Types, names: &Interner, id: TypeId) -> String {
    Speller { types, names }.declaration(id, Declarator::nothing())
}

/// The type as a declaration of `name`, which is how it would be written in the program.
#[must_use]
pub fn declare(types: &Types, names: &Interner, id: TypeId, name: Symbol) -> String {
    Speller { types, names }.declaration(id, Declarator::of(names.resolve(name).to_owned()))
}

/// The part of a declaration that is not the type in front of it, built outward from the hole
/// where the name goes.
#[derive(Debug)]
struct Declarator {
    /// What has been written around the hole so far.
    text: String,
    /// Whether it sits against the type with no space between them. The suffixes a type wears on
    /// its right go hard against it, which is `int[3]` and `int(void)`, and anything with a `*`
    /// in front of them takes the space back, which is `int (*)[3]`. That is gcc's spelling and
    /// it is not a test of the first character, since `(*)[3]` starts with the same bracket a
    /// parameter list does.
    glued: bool,
}

impl Declarator {
    /// The empty declarator, which is what a type name has.
    fn nothing() -> Declarator {
        Declarator { text: String::new(), glued: false }
    }

    /// A declarator that is a name, or a piece of one that has a `*` in it.
    fn of(text: String) -> Declarator {
        Declarator { text, glued: false }
    }

    /// The declarator with a suffix written after it, which keeps it against the type when there
    /// was nothing in front of the suffix to separate them.
    fn suffixed(self, suffix: &str) -> Declarator {
        let glued = self.glued || self.text.is_empty();
        Declarator { text: self.text + suffix, glued }
    }
}

/// What one spelling needs to reach for.
#[derive(Debug)]
struct Speller<'a> {
    types: &'a Types,
    names: &'a Interner,
}

impl Speller<'_> {
    /// The declaration of something named `inner` whose type is `id`.
    ///
    /// `inner` is the declarator built so far, which starts as the name and grows outward. The
    /// three shapes that are written around a name return early and the rest are written in
    /// front of it, which is the whole of C's declarator grammar read backwards.
    fn declaration(&self, id: TypeId, inner: Declarator) -> String {
        let ty = self.types.get(id);
        let base = match ty.kind {
            TypeKind::Pointer(pointee) => return self.pointer(ty, pointee, inner),
            TypeKind::Array { elem, len } => return self.array(ty, elem, len, inner),
            TypeKind::Function(function) => return self.function(function, inner),
            TypeKind::Void => String::from("void"),
            // `_Bool` rather than `bool` in both C17 and C23, which is what gcc 13.3 prints in
            // either dialect and which is unambiguous in a program that has typedefed `bool`.
            TypeKind::Bool => String::from("_Bool"),
            TypeKind::Int(kind) => String::from(kind.as_str()),
            TypeKind::Float(kind) => String::from(kind.as_str()),
            TypeKind::Complex(kind) => format!("_Complex {}", kind.as_str()),
            TypeKind::BitInt { signed, width } => {
                let sign = if signed { "" } else { "unsigned " };
                format!("{sign}_BitInt({width})")
            }
            // `_Atomic(T)` and not `_Atomic T`, which is what gcc writes. The two are the same
            // type and only one of them can be written in front of a pointer without changing
            // what it means, and this crate holds `_Atomic` as a type rather than a qualifier
            // for that reason.
            TypeKind::Atomic(inner) => format!("_Atomic({})", self.spell(inner)),
            // gcc 13.3 writes a vector this way and so does its `{aka}` form, so a program
            // that has a name for the type sees the name and everything else sees this.
            TypeKind::Vector { elem, len } => format!("__vector({len}) {}", self.spell(elem)),
            TypeKind::Record(record) => {
                let info = self.types.record_info(record);
                format!("{} {}", info.kind.as_str(), self.tag(info.tag))
            }
            TypeKind::Enum(enumeration) => {
                let info = self.types.enum_info(enumeration);
                format!("enum {}", self.tag(info.tag))
            }
            TypeKind::Typedef { name, .. } => self.names.resolve(name).to_owned(),
        };

        let mut out = String::new();
        if let Some(quals) = quals_text(ty.quals) {
            out.push_str(quals);
            out.push(' ');
        }
        out.push_str(&base);
        if !inner.text.is_empty() {
            if !inner.glued {
                out.push(' ');
            }
            out.push_str(&inner.text);
        }
        out
    }

    /// A pointer, whose qualifiers are written after the `*` because they are the pointer's own
    /// and not the pointee's.
    fn pointer(&self, ty: Type, pointee: TypeId, inner: Declarator) -> String {
        let mut declarator = String::from("*");
        if let Some(quals) = quals_text(ty.quals) {
            declarator.push_str(quals);
            if !inner.text.is_empty() {
                declarator.push(' ');
            }
        }
        declarator.push_str(&inner.text);
        // An array or a function binds tighter than the `*`, so without these the type would
        // read as an array of pointers or a function returning one. The sugar of a typedef
        // stops the recursion before this can matter, which is why `A *` needs nothing when
        // `A` is an array.
        if matches!(self.types.kind(pointee), TypeKind::Array { .. } | TypeKind::Function(_)) {
            declarator = format!("({declarator})");
        }
        self.declaration(pointee, Declarator::of(declarator))
    }

    /// An array, whose qualifiers have a spelling only inside the brackets.
    fn array(&self, ty: Type, elem: TypeId, len: ArrayLen, inner: Declarator) -> String {
        let mut suffix = String::from("[");
        if let Some(quals) = quals_text(ty.quals) {
            suffix.push_str(quals);
            if !matches!(len, ArrayLen::Unknown) {
                suffix.push(' ');
            }
        }
        match len {
            ArrayLen::Fixed(count) => suffix.push_str(&count.to_string()),
            ArrayLen::Unknown => {}
            // A variable length array's size is an expression the type does not hold, so this
            // says that there is one rather than inventing a name for it.
            ArrayLen::Star | ArrayLen::Variable(_) => suffix.push('*'),
        }
        suffix.push(']');
        self.declaration(elem, inner.suffixed(&suffix))
    }

    /// A function, whose parameters are type names in their own right.
    fn function(&self, function: FunctionId, inner: Declarator) -> String {
        let signature = self.types.signature(function);
        let mut suffix = String::from("(");
        for (index, &param) in signature.params.iter().enumerate() {
            if index > 0 {
                suffix.push_str(", ");
            }
            suffix.push_str(&self.spell(param));
        }
        if signature.variadic {
            if !signature.params.is_empty() {
                suffix.push_str(", ");
            }
            suffix.push_str("...");
        } else if signature.params.is_empty() && signature.prototyped {
            // `()` is a function whose parameters are unknown and `(void)` is one that takes
            // none, and the two are different types in every dialect before C23.
            suffix.push_str("void");
        }
        suffix.push(')');
        self.declaration(signature.ret, inner.suffixed(&suffix))
    }

    /// The type on its own, which is what a parameter and the inside of an `_Atomic` are.
    fn spell(&self, id: TypeId) -> String {
        self.declaration(id, Declarator::nothing())
    }

    /// A tag, or what gcc calls one that was never given.
    fn tag(&self, tag: Option<Symbol>) -> String {
        match tag {
            Some(name) => self.names.resolve(name).to_owned(),
            None => String::from("<anonymous>"),
        }
    }
}

/// The qualifier keywords in the order C writes them, or nothing at all.
fn quals_text(quals: Qualifiers) -> Option<&'static str> {
    // Sixteen combinations of three flags, written out rather than assembled, because the
    // answer is a constant string in every case and this is on the path of every diagnostic.
    match (
        quals.has(Qualifiers::CONST),
        quals.has(Qualifiers::VOLATILE),
        quals.has(Qualifiers::RESTRICT),
    ) {
        (false, false, false) => None,
        (true, false, false) => Some("const"),
        (false, true, false) => Some("volatile"),
        (false, false, true) => Some("restrict"),
        (true, true, false) => Some("const volatile"),
        (true, false, true) => Some("const restrict"),
        (false, true, true) => Some("volatile restrict"),
        (true, true, true) => Some("const volatile restrict"),
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;
    use crate::kind::{ArrayLen, FloatKind, FunctionType, IntKind, RecordKind};

    /// A table with a name in it, which is what every spelling below starts from.
    fn fixture() -> (Types, Interner) {
        (Types::new(), Interner::new())
    }

    #[test]
    fn a_basic_type_is_its_keywords() {
        let (types, names) = fixture();
        let int = types.int(IntKind::Int);
        assert_eq!(spell(&types, &names, int), "int");
        assert_eq!(spell(&types, &names, types.void()), "void");
        assert_eq!(spell(&types, &names, types.boolean()), "_Bool");
        let long_double = types.float(FloatKind::LongDouble);
        assert_eq!(spell(&types, &names, long_double), "long double");
    }

    #[test]
    fn a_qualifier_goes_in_front_of_what_it_qualifies() {
        let (mut types, names) = fixture();
        let int = types.int(IntKind::Int);
        let qualified = types.qualified(int, Qualifiers::CONST.with(Qualifiers::VOLATILE));
        assert_eq!(spell(&types, &names, qualified), "const volatile int");
    }

    #[test]
    fn a_pointers_own_qualifier_goes_after_the_star() {
        let (mut types, names) = fixture();
        let char_type = types.int(IntKind::Char);
        let constant = types.qualified(char_type, Qualifiers::CONST);
        let pointer = types.pointer(constant);
        let constant_pointer = types.qualified(pointer, Qualifiers::CONST);
        assert_eq!(spell(&types, &names, constant_pointer), "const char *const");
    }

    #[test]
    fn a_qualified_pointer_with_a_name_keeps_them_apart() {
        let (mut types, mut names) = fixture();
        let int = types.int(IntKind::Int);
        let pointer = types.pointer(int);
        let restricted = types.qualified(pointer, Qualifiers::RESTRICT);
        let p = names.intern("p");
        assert_eq!(declare(&types, &names, restricted, p), "int *restrict p");
    }

    #[test]
    fn a_declarator_is_written_around_the_name() {
        let (mut types, mut names) = fixture();
        let int = types.int(IntKind::Int);
        let char_type = types.int(IntKind::Char);
        let signature =
            FunctionType { ret: int, params: vec![char_type], variadic: false, prototyped: true };
        let function = types.function(signature);
        let pointer = types.pointer(function);
        let array = types.array(pointer, ArrayLen::Fixed(3));
        let f = names.intern("f");
        assert_eq!(declare(&types, &names, array, f), "int (*f[3])(char)");
    }

    #[test]
    fn an_abstract_declarator_keeps_the_parentheses_the_name_would_have_needed() {
        let (mut types, names) = fixture();
        let int = types.int(IntKind::Int);
        let array = types.array(int, ArrayLen::Fixed(3));
        let pointer = types.pointer(array);
        assert_eq!(spell(&types, &names, pointer), "int (*)[3]");
    }

    /// Measured against gcc 16, which writes all five of these in a `conflicting types` message.
    #[test]
    fn a_suffix_with_nothing_in_front_of_it_goes_against_the_type() {
        let (mut types, names) = fixture();
        let int = types.int(IntKind::Int);
        let array = types.array(int, ArrayLen::Fixed(4));
        let nested = types.array(array, ArrayLen::Fixed(2));
        let takes_an_int =
            FunctionType { ret: int, params: vec![int], variadic: false, prototyped: true };
        let function = types.function(takes_an_int.clone());
        let to_int = types.pointer(int);
        let gives_a_pointer = types.function(FunctionType { ret: to_int, ..takes_an_int });
        let to_array = types.pointer(array);

        assert_eq!(spell(&types, &names, array), "int[4]");
        assert_eq!(spell(&types, &names, nested), "int[2][4]");
        assert_eq!(spell(&types, &names, function), "int(int)");
        assert_eq!(spell(&types, &names, gives_a_pointer), "int *(int)");
        // The `*` is something to separate, so the space comes back.
        assert_eq!(spell(&types, &names, to_array), "int (*)[4]");
    }

    #[test]
    fn an_array_of_arrays_reads_left_to_right() {
        let (mut types, mut names) = fixture();
        let int = types.int(IntKind::Int);
        let inner = types.array(int, ArrayLen::Fixed(3));
        let outer = types.array(inner, ArrayLen::Fixed(2));
        let a = names.intern("a");
        assert_eq!(declare(&types, &names, outer, a), "int a[2][3]");
    }

    #[test]
    fn an_array_without_a_size_says_so_and_a_variable_one_says_only_that_it_has_one() {
        let (mut types, names) = fixture();
        let int = types.int(IntKind::Int);
        let unknown = types.array(int, ArrayLen::Unknown);
        let variable = types.array(int, ArrayLen::Variable(crate::kind::VlaId(0)));
        assert_eq!(spell(&types, &names, unknown), "int[]");
        assert_eq!(spell(&types, &names, variable), "int[*]");
    }

    #[test]
    fn a_prototype_with_no_parameters_is_not_a_function_without_one() {
        let (mut types, names) = fixture();
        let int = types.int(IntKind::Int);
        let prototyped =
            FunctionType { ret: int, params: Vec::new(), variadic: false, prototyped: true };
        let old = FunctionType { ret: int, params: Vec::new(), variadic: false, prototyped: false };
        let prototyped = types.function(prototyped);
        let old = types.function(old);
        let prototyped = types.pointer(prototyped);
        let old = types.pointer(old);
        assert_eq!(spell(&types, &names, prototyped), "int (*)(void)");
        assert_eq!(spell(&types, &names, old), "int (*)()");
    }

    #[test]
    fn a_variadic_function_ends_in_the_ellipsis() {
        let (mut types, names) = fixture();
        let int = types.int(IntKind::Int);
        let char_type = types.int(IntKind::Char);
        let signature =
            FunctionType { ret: int, params: vec![char_type], variadic: true, prototyped: true };
        let function = types.function(signature);
        let pointer = types.pointer(function);
        assert_eq!(spell(&types, &names, pointer), "int (*)(char, ...)");
    }

    #[test]
    fn a_typedef_is_spelled_as_itself_and_its_canonical_form_as_what_it_stands_for() {
        let (mut types, mut names) = fixture();
        let ulong = types.int(IntKind::ULong);
        let name = names.intern("size_t");
        let size_t = types.typedef(name, ulong);
        let pointer = types.pointer(size_t);
        assert_eq!(spell(&types, &names, pointer), "size_t *");
        let canonical = types.canonical(pointer);
        assert_eq!(spell(&types, &names, canonical), "unsigned long *");
    }

    #[test]
    fn a_tag_that_was_never_written_is_named_the_way_gcc_names_it() {
        let (mut types, mut names) = fixture();
        let tag = names.intern("S");
        let named = types.declare_record(RecordKind::Struct, Some(tag));
        let unnamed = types.declare_record(RecordKind::Union, None);
        let named = types.record(named);
        let unnamed = types.record(unnamed);
        assert_eq!(spell(&types, &names, named), "struct S");
        assert_eq!(spell(&types, &names, unnamed), "union <anonymous>");
    }

    #[test]
    fn an_atomic_type_is_written_as_the_type_it_is() {
        let (mut types, names) = fixture();
        let int = types.int(IntKind::Int);
        let atomic = types.atomic(int);
        let pointer = types.pointer(atomic);
        assert_eq!(spell(&types, &names, atomic), "_Atomic(int)");
        assert_eq!(spell(&types, &names, pointer), "_Atomic(int) *");
    }

    #[test]
    fn a_vector_is_written_the_way_gcc_writes_one() {
        let (mut types, names) = fixture();
        let int = types.int(IntKind::Int);
        let vector = types.vector(int, 4);
        assert_eq!(spell(&types, &names, vector), "__vector(4) int");
    }
}
