//! The functions the compiler declares for itself.
//!
//! Design: `spec/13-gnu-compat.md`.
//!
//! A program calls `__builtin_clzll` without declaring it and without including anything, and
//! it is right to: no header declares one, and the reserved prefix is what says the name
//! belongs to the implementation. So the implementation has to declare it, and what it declares
//! it as comes out of `features.toml`, which is the same table `__has_builtin` answers from.
//! One table rather than two, because two lists of the same builtins are two lists that
//! disagree.
//!
//! # Why the declaration is made on demand
//!
//! gcc declares all of them before it reads the first line, which puts a couple of thousand
//! names in the symbol table of every translation unit whether or not one is used. Here a name
//! is declared the first time it is looked up and not found, which reaches the same place for
//! any program that could tell the difference. The declaration goes in the file scope, so it is
//! visible for the rest of the unit and does not disappear when the block that first called it
//! closes, and a program that declares a builtin itself never gets here at all, because its own
//! declaration is what the lookup finds.
//!
//! # What is not here
//!
//! The builtins whose type depends on what they are handed. `__builtin_constant_p` takes
//! anything, `__builtin_add_overflow` takes three types that have to agree, and the atomics are
//! a family rather than a function. Those have no signature in the table, nothing here answers
//! for them, and they are decided from their arguments in `check/builtin/generic.rs`.
//!
//! Anything the call then does, except for the one family where the call is the whole answer.
//! A declared builtin is called like any other function, which is what makes the type checking
//! right and the code wrong: for most of them the call reaches the IR as a call to a name no
//! object file defines. Folding `__builtin_inf()` to an infinity and turning `__builtin_clzll`
//! into an instruction are the next piece, and until they land those rows stay `unimplemented`
//! in the table, so `__has_builtin` still answers no and a header that asks takes its fallback
//! path.
//!
//! The family where the call is the whole answer is the library builtins, the ones whose row
//! carries a `library`. `__builtin_abort` means `abort` and a call to the one is a call to the
//! other, so the declaration made here is all the front end owes them and the only other thing
//! anything needs is the name to put on the call, which [`library_name`] answers. gcc folds
//! several of them when the arguments allow it, and folding one of these is an optimization on
//! top of a call that was already right, which is why they can be implemented before anything
//! folds anything.

use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_gnu::{Kind, Status};
use rucc_types::{FloatKind, FunctionType, IntKind, Qualifiers, TypeId, int_width};

use crate::check::Checker;
use crate::decl::{Decl, DeclId, DeclKind, DeclList, Definition, Linkage, StorageDuration};
use crate::scope::Binding;

mod abs;
mod bswap;
mod classify;
mod constant;
mod count;
mod expect;
mod generic;
mod sign;
mod unreachable;

/// The name in the object file for a function declared under this spelling, when the two are
/// not the same name.
///
/// The library builtins are the family where they are not: `__builtin_abort` is a call to
/// `abort`, and a program writes the prefixed spelling to reach the function the C library
/// promises where a macro or a definition of its own has taken the plain name. So the walk to
/// the IR asks this for every name it is about to put in an object file, and the answer for
/// everything that is not one of that family is that the name stands.
///
/// The question is asked of the spelling rather than of the declaration because the answer is a
/// fact about the name and not about that particular declaration of it, and because the alternative
/// is a field on every declaration in the program to carry something almost none of them have.
#[must_use]
pub fn library_name(spelled: &str) -> Option<&'static str> {
    // Every name this can answer for starts with the prefix, and every other name in the
    // program is asked too, so the test that costs nothing goes first.
    if !spelled.starts_with("__builtin_") {
        return None;
    }
    let feature = rucc_gnu::lookup(Kind::Builtin, spelled)?;
    (!feature.library.is_empty()).then_some(feature.library)
}

/// Whether this is a builtin that nothing implements and that nothing in the C library would
/// answer for either, which is the set that has to be refused rather than called.
///
/// A builtin the walk to the IR does not recognise becomes a call to the name the program wrote,
/// and no object file defines a name with that prefix, so the program fails at the linker on a
/// name its author never typed. That is the worst place for the news to arrive, because a builtin
/// is the one thing a programmer does not expect to have to provide.
///
/// The table already knows. A row whose status is not `implemented` is one nothing here does
/// anything with, and a row with no `library` is one that has no function of its own to fall back
/// on, so the two together are exactly the set. That is also what `__has_builtin` answers no for,
/// which means the compiler has been saying the right thing about these all along and only the
/// call was wrong.
///
/// Asked of the spelling for the same reason [`library_name`] is: the answer is a fact about the
/// name rather than about a particular declaration of it.
#[must_use]
pub fn unimplemented_builtin(spelled: &str) -> bool {
    // The `__sync_` and `__atomic_` families are in here too, so the test is the two underscores
    // rather than the whole prefix. Every name in the program is asked, so it goes first.
    if !spelled.starts_with("__") {
        return false;
    }
    let Some(feature) = rucc_gnu::lookup(Kind::Builtin, spelled) else {
        return false;
    };
    feature.status != Status::Implemented && feature.library.is_empty()
}

impl Checker<'_> {
    /// Declares a name the program used and nothing declared, if it is a builtin we know the
    /// type of. Answers the declaration, or nothing when the name is not one.
    pub(in crate::check) fn declare_builtin(&mut self, name: Symbol, span: Span) -> Option<DeclId> {
        let spelled = self.text(name);
        // Every name this can answer for starts with two underscores, and every other undeclared
        // name in the program reaches here too. The test costs a byte and saves the search.
        if !spelled.starts_with("__") {
            return None;
        }
        let feature = rucc_gnu::lookup(Kind::Builtin, spelled)?;
        if feature.signature.is_empty() {
            return None;
        }
        let ty = self.signature_type(feature.signature)?;
        let decl = self.tast.decl(
            Decl {
                name: Some(name),
                ty,
                kind: DeclKind::Function,
                // External and declared, which is what it is: a name every translation unit
                // that uses it shares, and one nothing here defines.
                linkage: Linkage::External,
                duration: StorageDuration::Static,
                state: Definition::Declared,
                alignment: None,
                constant: false,
                retained: false,
                asm_label: None,
                init: None,
                params: DeclList::EMPTY,
                body: None,
            },
            span,
        );
        // Not added to the top level, which is the list of what the translation unit declares.
        // The program declared nothing, and a `--emit=tast` that showed a declaration the source
        // does not contain would be answering a question nobody asked.
        self.scopes.declare_at_file_scope(name, Binding::Decl(decl));
        self.declared_builtins.push(name);
        Some(decl)
    }

    /// The type a signature from the table names.
    ///
    /// Answers nothing only if the table is wrong, which `build.rs` and the test at the bottom
    /// of this file are there to stop. Nothing is reported either way: a builtin whose type
    /// cannot be built is a builtin the program hears about in the ordinary words, which is
    /// that the name is undeclared.
    fn signature_type(&mut self, signature: &str) -> Option<TypeId> {
        let (result, rest) = signature.split_once('(')?;
        let params = rest.strip_suffix(')')?;
        let ret = self.written_type(result)?;
        let mut types = Vec::new();
        let mut variadic = false;
        for param in params.split(',').map(str::trim).filter(|param| !param.is_empty()) {
            if param == "..." {
                variadic = true;
                continue;
            }
            let ty = self.written_type(param)?;
            // `void` alone is how C spells a parameter list with nothing in it, and it is the
            // only place a parameter is allowed that type.
            if rucc_types::is_void(&self.types, ty) {
                continue;
            }
            types.push(ty);
        }
        Some(self.types.function(FunctionType { ret, params: types, variadic, prototyped: true }))
    }

    /// One type, written the way the table writes one.
    fn written_type(&mut self, text: &str) -> Option<TypeId> {
        let stars = text.bytes().filter(|byte| *byte == b'*').count();
        let words = text.trim_end_matches(['*', ' ']).split_whitespace();
        let mut quals = Qualifiers::NONE;
        let mut base = Vec::new();
        for word in words {
            match word {
                "const" => quals = quals.with(Qualifiers::CONST),
                "volatile" => quals = quals.with(Qualifiers::VOLATILE),
                other => base.push(other),
            }
        }
        // A qualifier in a signature is always on the thing pointed at, since every one in the
        // table is there to promise that the callee does not write through the pointer.
        let mut ty = self.base_type(&base.join(" "))?;
        ty = self.types.qualified(ty, quals);
        for _ in 0..stars {
            ty = self.types.pointer(ty);
        }
        Some(ty)
    }

    /// The type a base type's words name, with no pointers and no qualifiers on it.
    fn base_type(&mut self, words: &str) -> Option<TypeId> {
        let kind = match words {
            "void" => return Some(self.types.void()),
            // What a builtin that asks a yes or no question answers with. gcc gives these the C
            // `_Bool` and not an `int`, and a header that puts one straight into a `_Bool` field
            // would take a conversion it does not need if this were wrong.
            "_Bool" => return Some(self.types.boolean()),
            "float" => return Some(self.types.float(FloatKind::Float)),
            "double" => return Some(self.types.float(FloatKind::Double)),
            "long double" => return Some(self.types.float(FloatKind::LongDouble)),
            // Not a fixed kind on any target. Whichever unsigned type is wide enough to hold a
            // pointer is what a header would have made `size_t`, and it has to be the one
            // `sizeof` produces or a program that hands one to the other converts for nothing.
            "size_t" => return Some(self.size_type()),
            "uint16_t" => return Some(self.exact_unsigned(16)),
            "uint32_t" => return Some(self.exact_unsigned(32)),
            "uint64_t" => return Some(self.exact_unsigned(64)),
            "char" => IntKind::Char,
            "signed char" => IntKind::SChar,
            "unsigned char" => IntKind::UChar,
            "short" | "signed short" | "short int" => IntKind::Short,
            "unsigned short" => IntKind::UShort,
            "int" | "signed" | "signed int" => IntKind::Int,
            "unsigned" | "unsigned int" => IntKind::UInt,
            "long" | "signed long" | "long int" => IntKind::Long,
            "unsigned long" => IntKind::ULong,
            "long long" | "signed long long" => IntKind::LongLong,
            "unsigned long long" => IntKind::ULongLong,
            _ => return None,
        };
        Some(self.types.int(kind))
    }

    /// The unsigned integer type that is exactly this many bits wide on this target.
    ///
    /// The table writes `uint16_t` rather than `unsigned short` because that is what gcc's
    /// `__builtin_bswap16` takes, and the two are the same type on every target this compiles
    /// for and need not be on one it does not have yet.
    fn exact_unsigned(&self, bits: u32) -> TypeId {
        let kinds = [IntKind::UChar, IntKind::UShort, IntKind::UInt, IntKind::ULong];
        let kind = kinds.into_iter().find(|&kind| int_width(kind, self.cx.target) == bits);
        self.types.int(kind.unwrap_or(IntKind::ULongLong))
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_gnu::Kind;
    use rucc_session::Std;
    use rucc_target::{TargetInfo, Triple};
    use rucc_types::TypeKind;

    use super::*;
    use crate::check::Context;

    /// What a checker needs to exist, since it borrows all of it for as long as it lives.
    struct Fixture {
        ast: rucc_ast::Ast,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        fn new(triple: &str) -> Fixture {
            let target = TargetInfo::new(triple.parse::<Triple>().expect("a triple"));
            Fixture { ast: rucc_ast::Ast::new(), names: Interner::new(), target }
        }

        fn checker(&self) -> Checker<'_> {
            Checker::new(&self.ast, Context::new(&self.names, &self.target, Std::C23))
        }
    }

    /// How a built type is written, which is what these compare against.
    fn spelled(checker: &Checker<'_>, ty: TypeId) -> String {
        rucc_types::spell(&checker.types, checker.cx.names, ty)
    }

    #[test]
    fn a_signature_becomes_the_type_it_is_written_as() {
        let fixture = Fixture::new("x86_64-unknown-linux-gnu");
        let mut c = fixture.checker();
        let cases = [
            ("int(unsigned int)", "int(unsigned int)"),
            ("long(long, long)", "long(long, long)"),
            ("void(void)", "void(void)"),
            ("size_t(const char *)", "unsigned long(const char *)"),
            ("void *(void *, const void *, size_t)", "void *(void *, const void *, unsigned long)"),
            ("void(const void *, ...)", "void(const void *, ...)"),
            ("double(double)", "double(double)"),
            ("long double(long double)", "long double(long double)"),
        ];
        for (signature, written) in cases {
            let ty = c.signature_type(signature).expect("a type");
            assert_eq!(spelled(&c, ty), written, "for {signature}");
        }
    }

    /// `void` in a parameter list is a list with nothing in it rather than a parameter of no
    /// type, and the difference shows up in the count and nowhere else.
    #[test]
    fn a_void_parameter_list_is_an_empty_one() {
        let fixture = Fixture::new("x86_64-unknown-linux-gnu");
        let mut c = fixture.checker();
        let ty = c.signature_type("void(void)").expect("a type");
        let TypeKind::Function(id) = c.types.kind(ty) else { panic!("a function") };
        let signature = c.types.signature(id);
        assert!(signature.params.is_empty());
        assert!(signature.prototyped, "or a call would not be checked against it");
        assert!(!signature.variadic);
    }

    /// The width the table asks for is a fact about the target, so the type it comes out as is
    /// one too. `size_t` is the one that moves.
    #[test]
    fn the_target_decides_which_type_a_width_names() {
        for (triple, written) in [
            ("x86_64-unknown-linux-gnu", "unsigned long(const char *)"),
            ("x86_64-pc-windows-msvc", "unsigned long long(const char *)"),
        ] {
            let fixture = Fixture::new(triple);
            let mut c = fixture.checker();
            let ty = c.signature_type("size_t(const char *)").expect("a type");
            assert_eq!(spelled(&c, ty), written, "on {triple}");
        }
    }

    #[test]
    fn a_signature_the_reader_cannot_make_sense_of_is_no_type_rather_than_a_wrong_one() {
        let fixture = Fixture::new("x86_64-unknown-linux-gnu");
        let mut c = fixture.checker();
        assert_eq!(c.signature_type("int"), None, "no parameter list at all");
        assert_eq!(c.signature_type("int(unsigned int"), None, "unclosed");
        assert_eq!(c.signature_type("struct tm *(void)"), None, "a word that is not in the set");
    }

    /// Every signature in the table is written out of the words the reader above takes. The
    /// build checks the alphabet already, and this checks that the two agree on it, which is
    /// the failure the build cannot see: a word spelled correctly that means nothing here.
    #[test]
    fn every_signature_in_the_table_builds_a_type() {
        let fixture = Fixture::new("x86_64-unknown-linux-gnu");
        let mut c = fixture.checker();
        let mut built = 0;
        for feature in rucc_gnu::features() {
            if feature.kind != Kind::Builtin || feature.signature.is_empty() {
                continue;
            }
            let ty = c.signature_type(feature.signature);
            assert!(ty.is_some(), "{} has a signature this cannot read", feature.name);
            built += 1;
        }
        assert!(built > 40, "the table lost its signatures, only {built} left");
    }

    /// The lookup is by the whole name including the prefix, because that prefix is part of the
    /// name of a builtin rather than the armour a header puts around an attribute.
    #[test]
    fn only_a_name_the_table_has_is_declared() {
        let mut fixture = Fixture::new("x86_64-unknown-linux-gnu");
        let known = fixture.names.intern("__builtin_clzll");
        let ordinary = fixture.names.intern("printf");
        // In the table, and deliberately without a signature, since what it takes is whatever
        // it was handed.
        let untyped = fixture.names.intern("__builtin_constant_p");
        let mut c = fixture.checker();
        let decl = c.declare_builtin(known, Span::DUMMY).expect("in the table");
        assert_eq!(spelled(&c, c.tast[decl].ty), "int(unsigned long long)");
        assert_eq!(c.tast[decl].linkage, Linkage::External);
        assert_eq!(c.tast[decl].state, Definition::Declared);
        assert_eq!(c.declare_builtin(ordinary, Span::DUMMY), None);
        assert_eq!(c.declare_builtin(untyped, Span::DUMMY), None);
    }

    /// The three widths of one of these are three names and not one, because a builtin is an
    /// ordinary function as far as the type checker is concerned and C has no overloading.
    #[test]
    fn a_rounding_builtin_has_a_name_for_each_width() {
        let mut fixture = Fixture::new("x86_64-unknown-linux-gnu");
        let names: Vec<_> = ["__builtin_ceil", "__builtin_ceilf", "__builtin_ceill"]
            .map(|name| fixture.names.intern(name))
            .into();
        let mut c = fixture.checker();
        let spellings: Vec<_> = names
            .into_iter()
            .map(|name| {
                let decl = c.declare_builtin(name, Span::DUMMY).expect("in the table");
                spelled(&c, c.tast[decl].ty)
            })
            .collect();
        assert_eq!(spellings, ["double(double)", "float(float)", "long double(long double)"]);
    }

    /// The library builtins are the family whose whole answer is the library function of the
    /// same name, so what has to be right is both names at once: the one the program wrote,
    /// which is what the declaration is looked up by and what a diagnostic will say, and the one
    /// the library defines, which is what the call has to carry.
    #[test]
    fn every_library_builtin_is_declared_under_its_own_name_and_called_by_the_library_one() {
        let mut fixture = Fixture::new("x86_64-unknown-linux-gnu");
        let family: Vec<_> = rucc_gnu::features()
            .iter()
            .filter(|feature| !feature.library.is_empty())
            .map(|feature| (fixture.names.intern(feature.name), feature))
            .collect();
        assert!(family.len() > 30, "the table lost the family, {} rows left", family.len());
        // One that is a builtin and not a library function, to show that the answer comes from
        // the row rather than from everything going through here.
        let trap = fixture.names.intern("__builtin_trap");
        let mut c = fixture.checker();
        for (name, feature) in family {
            let decl = c.declare_builtin(name, Span::DUMMY).expect("in the table");
            let node = &c.tast[decl];
            assert_eq!(node.name, Some(name), "{}", feature.name);
            assert_eq!(node.kind, DeclKind::Function, "{}", feature.name);
            assert_eq!(node.linkage, Linkage::External, "{}", feature.name);
            assert_eq!(node.state, Definition::Declared, "{}", feature.name);
            assert_eq!(library_name(feature.name), Some(feature.library), "{}", feature.name);
        }
        assert!(c.declare_builtin(trap, Span::DUMMY).is_some(), "in the table");
        assert_eq!(library_name("__builtin_trap"), None, "nothing in the library is called that");
    }

    /// The question is asked of every name the walk to the IR is about to write down, so the
    /// answer for a name that has nothing to do with the family has to be that it stands.
    #[test]
    fn an_ordinary_name_keeps_the_one_the_program_gave_it() {
        assert_eq!(library_name("abort"), None, "the program's own function of that name");
        assert_eq!(library_name("main"), None);
        assert_eq!(library_name("__builtin_nonesuch"), None, "the prefix is not a promise");
        assert_eq!(library_name("__builtin_va_start"), None, "a builtin and not a function");
    }

    /// What each one is declared as, spelled out for the shapes the rest of the family is made
    /// of, since the walk above compares the table against itself and this compares it against
    /// what the C library actually promises.
    #[test]
    fn a_library_builtin_is_declared_with_the_type_the_library_gives_the_function() {
        let mut fixture = Fixture::new("x86_64-unknown-linux-gnu");
        let cases = [
            ("__builtin_abort", "void(void)"),
            ("__builtin_malloc", "void *(unsigned long)"),
            ("__builtin_abs", "int(int)"),
            ("__builtin_memcmp", "int(const void *, const void *, unsigned long)"),
            ("__builtin_strchr", "char *(const char *, int)"),
            ("__builtin_printf", "int(const char *, ...)"),
        ];
        let names: Vec<_> =
            cases.iter().map(|(name, _)| fixture.names.intern(name)).collect::<Vec<_>>();
        let mut c = fixture.checker();
        for (name, (spelling, written)) in names.into_iter().zip(cases) {
            let decl = c.declare_builtin(name, Span::DUMMY).expect("in the table");
            assert_eq!(spelled(&c, c.tast[decl].ty), written, "for {spelling}");
        }
    }

    /// The declaration a program did not write goes where C says the implementation made it,
    /// which is the file scope, however deep in the program the first call was.
    #[test]
    fn a_builtin_first_called_inside_a_block_is_still_declared_at_the_file_scope() {
        let mut fixture = Fixture::new("x86_64-unknown-linux-gnu");
        let name = fixture.names.intern("__builtin_trap");
        let mut c = fixture.checker();
        c.scopes.push();
        c.scopes.push();
        let decl = c.declare_builtin(name, Span::DUMMY).expect("in the table");
        c.scopes.pop();
        c.scopes.pop();
        assert_eq!(c.scopes.lookup(name), Some(Binding::Decl(decl)));
        assert!(c.tast.top_level().is_empty(), "the program declared nothing");
    }
}
