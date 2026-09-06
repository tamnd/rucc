//! Declarations: what a name means, how long the thing it names lives, and who else sees it.
//!
//! Design: `spec/07-types-and-semantics.md`.
//!
//! The type builder in `check/ty.rs` answers what a declarator says. This answers everything
//! else a declaration decides, which is four things about each name and one relation between the
//! declarations that share it. The four are what kind of thing it is, what linkage it has, how
//! long it lives and how much of a definition it is, and not one of them is written down: `int
//! x;` at file scope is an external, static, tentative definition, and the same three words in a
//! block are a local automatic one, and the only difference between them is where they are.
//!
//! # Why the states are kept apart
//!
//! A tentative definition is not a definition and not a plain declaration, and collapsing it into
//! either is what makes `int x; int x;` come out as an error or as two objects in a compiler that
//! got it wrong. It is a definition only if nothing else in the translation unit defines the
//! name, so the answer is not known where it is read, which is why [`Definition`] has three
//! values rather than a boolean.
//!
//! # What is not here yet
//!
//! A function definition, which needs statements. A braced initializer, which is the piece after
//! this one and which is where the string literals and the designators go.
//!
//! Two checks wait on something that does not exist rather than on effort. The end of the
//! translation unit is where a tentative `int a[];` is given its one element and where an object
//! that is still incomplete is reported, so neither is done here, since a declaration in the
//! middle of a file has no way to know what comes after it. And a file-scope initializer is not
//! required to be constant here, because the constant folding has no address constants yet and
//! `int *p = &x;` is the ordinary case rather than the exotic one, so the check would be wrong
//! far more often than it would be right. What is checked is the `constexpr` case, which is
//! arithmetic and which the folding does answer.

use std::num::NonZeroU32;

use rucc_ast::{self as ast, AlignSpec, AttrList, FuncSpecs, StorageClass};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{ArrayLen, Qualifiers, TypeId, TypeKind};
use rucc_types::{compatible, composite, is_complete, is_function, is_void, layout};

use crate::check::Checker;
use crate::check::stmt::Enclosing;
use crate::decl::{
    Decl, DeclId, DeclKind, DeclList, Definition, InitList, Linkage, StorageDuration,
};
use crate::scope::Binding;
use crate::tast::StrId;

/// What one declarator declares, before anything already declared under the name is consulted.
#[derive(Debug, Clone, Copy)]
struct Declared {
    /// The name, which a declaration that reaches this point always has.
    name: Symbol,
    /// The type the declarator built.
    ty: TypeId,
    /// Whether it is an object or a function.
    kind: DeclKind,
    /// Who else can see the name.
    linkage: Linkage,
    /// How long it lives.
    duration: StorageDuration,
    /// How much of a definition it is.
    state: Definition,
    /// Whether an initializer was written. A block-scope object defines itself whether or not one
    /// was, so [`Definition`] does not answer this, and the wording of two declarations meeting in
    /// a block turns on it.
    initialized: bool,
    /// What `alignas` asked for, once it has been folded and checked.
    alignment: Option<u32>,
    /// Whether `constexpr` was written, which makes the object a named constant.
    constant: bool,
    /// Whether an attribute asks for it to exist where nothing refers to it.
    retained: bool,
    /// The assembler name this declaration wrote, which is the symbol the name stands for.
    asm_label: Option<StrId>,
    /// Whether the declaration says nothing about which linkage it wants and so takes whatever the
    /// declaration before it had. This is not the same as having external linkage. A file scope
    /// `int x;` has external linkage and no keyword, and the difference between the two is what
    /// makes `static int x; extern int x;` legal and `static int x; int x;` not.
    takes_prior_linkage: bool,
    /// The name, for the diagnostics that point at one.
    span: Span,
}

impl Checker<'_> {
    /// Checks one declaration and gives back the objects and functions it declared.
    ///
    /// The run is empty for a declaration that declares neither, which is a `typedef`, a tag, a
    /// static assertion, or one of the mistakes that leaves nothing behind.
    pub fn check_decl(&mut self, id: ast::DeclId) -> DeclList {
        let span = self.ast.decl_span(id);
        match self.ast[id] {
            ast::Decl::Error => self.tast.add_decl_refs(&[]),
            ast::Decl::Var { specs, declarators } => self.var(specs, declarators),
            ast::Decl::StaticAssert { cond, message } => {
                self.static_assert(cond, message, span);
                self.tast.add_decl_refs(&[])
            }
            ast::Decl::Function { specs, declarator, params, body } => {
                match self.function(specs, declarator, params, body) {
                    Some(id) => self.tast.add_decl_refs(&[id]),
                    None => self.tast.add_decl_refs(&[]),
                }
            }
            ast::Decl::Asm(_) => {
                self.declaration_unsupported("an assembler statement at file scope", span);
                self.tast.add_decl_refs(&[])
            }
            // An attribute declaration appertains to nothing by definition, so there is nothing
            // to check and nothing to declare.
            ast::Decl::Attributes(_) => self.tast.add_decl_refs(&[]),
        }
    }

    /// A specifier list and its declarators, which is what most declarations are.
    fn var(&mut self, specs: ast::DeclSpecsId, declarators: ast::InitDeclaratorList) -> DeclList {
        let mut items = self.ast[declarators].to_vec();
        if items.is_empty() {
            self.empty_declaration(specs);
            return self.tast.add_decl_refs(&[]);
        }
        let node = self.ast[specs];
        if let Some(which) = node.deduces() {
            // One initializer deduces one type, and there is nothing to say two declarators of
            // one list should deduce the same one. gcc allows the one and refuses the rest,
            // which is what happens here: the message is said once and the first declarator is
            // checked so that its name still means something.
            if items.len() > 1 {
                let spelled = which.spelling();
                self.report(
                    Diagnostic::error(
                        format!("'{spelled}' may only be used with a single declarator"),
                        node.span,
                    )
                    .with_code("E0651"),
                );
                items.truncate(1);
            }
        }
        let mut declared = Vec::with_capacity(items.len());
        for item in items {
            if let Some(id) = self.init_declarator(specs, item) {
                declared.push(id);
            }
        }
        self.tast.add_decl_refs(&declared)
    }

    /// A function definition, which is a declaration with a body under it.
    ///
    /// The parameters are declared once, by the type builder, when it read the prototype. They
    /// are bound again here rather than declared again, so that the declaration the prototype
    /// resolved `n` to in `void f(int n, int a[n])` is the one the body assigns to.
    fn function(
        &mut self,
        specs: ast::DeclSpecsId,
        declarator: ast::DeclaratorId,
        declarations: ast::DeclList,
        body: ast::StmtId,
    ) -> Option<DeclId> {
        let node = self.ast[declarator];
        let span = node.name_span;
        let (params, kind) = match self.ast[node.derived].first() {
            Some(&ast::Derived::Function { params, kind, .. }) => (params, kind),
            // A definition whose declarator does not end in a parameter list is a parse that did
            // not work out, and the parser has already said so.
            _ => return None,
        };
        // GNU's nested function, a definition inside a block. `spec/13-gnu-compat.md` section 13.2
        // settles this one: a call to a nested function goes through a trampoline written on the
        // stack, and a stack that can be executed is not something to add to a compiler being
        // written now. The row in `features.toml` says the same. It is turned down here rather
        // than left to the lowering, which had no name to give one and built a module with two
        // symbols named `nested` out of a file that had two of them.
        //
        // The declaration is kept, without the body, so that the calls below it resolve. One error
        // for the definition reads better than that error and one more for every call under it.
        let nested = !self.scopes.at_file_scope();
        if nested {
            let note = "a nested function is called through a trampoline written on the stack, \
                        which no target that enforces an unexecutable stack allows, so this \
                        compiler does not have them and will not";
            self.report(
                Diagnostic::error("a function definition inside a function", span)
                    .with_code("E0676")
                    .note(note, span),
            );
        }
        // A definition is a declarator with a function type, so it is never the plain identifier
        // a deduced type needs, and the deduction never gets as far as an initializer to deduce
        // from. gcc says the same thing about it as about `auto *p = q;`.
        if let Some(which) = self.ast[specs].deduces() {
            self.not_plain(which, span);
            return None;
        }
        let ty = self.declared_type(specs, declarator);
        // An old-style definition's identifier list said nothing about types, so the type built
        // from the declarator has no parameters in it. The declarations under the list are where
        // they are, and they are read here because the type builder never saw them: they are not
        // part of the declarator at all. The type is then made again with them in it.
        let ty = if kind == ast::ParamKind::Identifiers {
            self.identifier_list(params, declarations);
            let taking = self.old_style_signature(node.name, params, span);
            self.function_taking(ty, taking)
        } else {
            ty
        };
        let name = node.name?;
        let specs = self.ast[specs];
        if specs.is_typedef() {
            self.report(
                Diagnostic::error("function definition declared 'typedef'", span)
                    .with_code("E0589"),
            );
            return None;
        }
        let (linkage, duration) = self.placement(&specs, DeclKind::Function, name, span);
        let alignment = match specs.align {
            Some(align) => self.alignment(align, ty, DeclKind::Function, name, span),
            None => None,
        };
        // A definition has no declarator of its own to hang an attribute off, the way `retained`
        // below reads only the specifiers for the same reason. The usual place to write one on a
        // function is the declaration above the definition anyway, and the merge keeps it.
        let alignment = alignment.max(self.attribute_alignment(specs.attrs, AttrList::EMPTY, ty));
        let declared = Declared {
            name,
            ty,
            kind: DeclKind::Function,
            linkage,
            duration,
            state: if nested { Definition::Declared } else { Definition::Defined },
            initialized: false,
            alignment,
            constant: false,
            retained: self.retains(specs.attrs),
            // A definition has no declarator of its own to write one after, which is a rule of
            // the grammar rather than of this compiler: gcc stops at the brace as well. The
            // declaration above the definition is where one goes and the merge keeps it.
            asm_label: None,
            takes_prior_linkage: takes_prior_linkage(&specs, DeclKind::Function),
            span,
        };
        let id = self.merge(declared);
        if nested {
            return Some(id);
        }
        let (stmt, params) = self.function_body(ty, name, span, params, body);
        let mut node = self.tast[id].clone();
        node.params = params;
        node.body = Some(stmt);
        self.tast.set_decl(id, node);
        Some(id)
    }

    /// What an old-style definition's function type takes.
    ///
    /// The promoted types of the identifiers, which is what a caller hands over and what
    /// 6.7.6.3p15 compares against a prototype, unless there is a prototype in scope already. A
    /// prototype overrules them, because the alternative is refusing the pairing that all the
    /// code written this way relies on: a header says `int f(char);` and the file defines `f` in
    /// the old style, and `char` promotes to `int`, so the standard's own rule makes the two
    /// incompatible and gcc has always taken them. gcc compares each parameter as written rather
    /// than as promoted, so a `short` where the prototype says `char` is still the mistake it
    /// looks like, and this does the same.
    fn old_style_signature(
        &mut self,
        name: Option<Symbol>,
        params: ast::ParamList,
        span: Span,
    ) -> Vec<TypeId> {
        let objects = self.prototype_params(params);
        let written: Vec<TypeId> = objects.iter().map(|&id| self.tast[id].ty).collect();
        let Some(prototype) = name.and_then(|name| self.prototype_in_scope(name)) else {
            return written.iter().map(|&ty| self.default_promoted(ty)).collect();
        };
        if prototype.len() != written.len() {
            // The counts disagree, so there is no pairing to check and no reason to prefer one
            // list over the other. The merge reports it as the conflict it is.
            return written.iter().map(|&ty| self.default_promoted(ty)).collect();
        }
        for (index, (&declared, &wanted)) in written.iter().zip(&prototype).enumerate() {
            let promoted = self.default_promoted(declared);
            if compatible(&self.types, declared, wanted)
                || compatible(&self.types, promoted, wanted)
            {
                continue;
            }
            let at = objects.get(index).map_or(span, |&id| self.tast.decl_span(id));
            let what = match objects.get(index).and_then(|&id| self.tast[id].name) {
                Some(name) => format!("argument '{}' doesn't match prototype", self.text(name)),
                None => "argument doesn't match prototype".to_string(),
            };
            self.report(Diagnostic::error(what, at).with_code("E0683"));
        }
        prototype
    }

    /// The parameter types of the prototype this name already has, if it has one.
    ///
    /// A variadic one is not one of these. An old-style definition cannot be the definition of a
    /// variadic function, so there is nothing for its identifiers to line up against.
    fn prototype_in_scope(&mut self, name: Symbol) -> Option<Vec<TypeId>> {
        let Some(Binding::Decl(id)) = self.scopes.lookup(name) else { return None };
        let canonical = self.types.canonical(self.tast[id].ty);
        let TypeKind::Function(signature) = self.types.kind(canonical) else { return None };
        let signature = self.types.signature(signature);
        (signature.prototyped && !signature.variadic).then(|| signature.params.clone())
    }

    /// The body of a function definition, in a scope holding its parameters, and the parameters
    /// themselves.
    ///
    /// One scope and not two. C 6.2.1p4 puts the parameters in the block scope of the body, which
    /// is why `void f(int a) { int a; }` is a redeclaration and `void f(int a) { { int a; } }` is
    /// not, so the body's own compound statement is walked here rather than through the statement
    /// that would open a scope of its own.
    fn function_body(
        &mut self,
        ty: TypeId,
        name: Symbol,
        span: Span,
        params: ast::ParamList,
        body: ast::StmtId,
    ) -> (crate::stmt::StmtId, DeclList) {
        let (ret, variadic) = match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Function(signature) => {
                let signature = self.types.signature(signature);
                (signature.ret, signature.variadic)
            }
            // A definition of something that is not a function has been reported by the merge,
            // and checking the body against `int` is what keeps the rest of it worth reading.
            _ => (self.int(), false),
        };
        self.scopes.push();
        let params = self.prototype_params(params);
        for &decl in &params {
            if let Some(name) = self.tast[decl].name {
                self.scopes.declare(name, Binding::Decl(decl));
            }
        }
        let last_param = params.last().copied();
        let params = self.tast.add_decl_refs(&params);
        let name = Some(name);
        let previous =
            self.open_body(Enclosing { ret, at: span, variadic, last_param, params, name });
        let stmt = self.body_block(body);
        self.close_body(previous);
        self.scopes.pop();
        (stmt, params)
    }

    /// A declaration with no declarator, which declares a tag or nothing at all.
    ///
    /// The type is built either way, because `struct S { int x; };` is how every structure in
    /// every header is declared and the body is where the members are checked. What is diagnosed
    /// is the case where a type was named and there was nothing for it to be the type of.
    fn empty_declaration(&mut self, specs: ast::DeclSpecsId) {
        let node = self.ast[specs];
        if matches!(node.ty, ast::TypeSpec::None) {
            // No type was named, so there is none to build and nothing to say about what it
            // would have been. `;` on its own gets here from every macro that ends in one,
            // and the type builder would otherwise report the missing type as if the
            // declaration had a declarator that needed one.
            self.specifiers_alone(node);
            return;
        }
        if let ast::TypeSpec::Auto(which) = node.ty {
            // A deduced type with nothing to deduce from. Not a useless type name, because
            // there is no name here that could have been useful, and not a missing initializer
            // either, because there is no declarator to have given one to.
            let word = which.spelling();
            self.report(
                Diagnostic::error(format!("`{word}` in empty declaration"), node.span)
                    .with_code("E0669"),
            );
            return;
        }
        self.declared_specs(specs);
        match node.ty {
            // A record with no tag and no declarator names a type nothing can ever refer to,
            // which is a different mistake from naming a type and forgetting the variable.
            ast::TypeSpec::Record { tag: None, fields: Some(_), .. } => {
                self.report(
                    Diagnostic::warning(
                        "unnamed struct/union that defines no instances",
                        node.span,
                    )
                    .with_code("E0612"),
                );
            }
            ast::TypeSpec::Record { .. } | ast::TypeSpec::Enum { .. } => {
                if !node.quals.is_none() {
                    self.report(
                        Diagnostic::warning(
                            "useless type qualifier in empty declaration",
                            node.span,
                        )
                        .with_code("E0611"),
                    );
                }
            }
            _ => {
                self.report(
                    Diagnostic::warning("useless type name in empty declaration", node.span)
                        .with_code("E0610"),
                );
            }
        }
    }

    /// A declaration that named no type, no declarator and no tag, which is a `;` and whatever
    /// specifiers were written before it.
    ///
    /// Nothing here declares anything, so all of it is dead, and what is said about it is which
    /// specifier was the useless one. Only the first is named, because a reader who deletes the
    /// specifier deletes the rest of them with it. A bare `;` is silent, since the parser has
    /// already given it the one warning it deserves and every project has one.
    fn specifiers_alone(&mut self, node: ast::DeclSpecs) {
        // `inline` and `_Noreturn` are errors rather than warnings because neither has a
        // meaning at all away from a function, where the other two do have one and are merely
        // being ignored.
        if node.func.has(FuncSpecs::INLINE) {
            self.report(
                Diagnostic::error("`inline` in empty declaration", node.span).with_code("E0665"),
            );
            return;
        }
        if node.func.has(FuncSpecs::NORETURN) {
            self.report(
                Diagnostic::error("`_Noreturn` in empty declaration", node.span).with_code("E0666"),
            );
            return;
        }
        // At file scope these two name a storage duration that file scope does not have, so
        // there is no reading of them under which the declaration would have meant something.
        let automatic = matches!(node.storage, Some(StorageClass::Auto | StorageClass::Register));
        if automatic && self.scopes.at_file_scope() {
            let word = if node.storage == Some(StorageClass::Auto) { "auto" } else { "register" };
            self.report(
                Diagnostic::error(format!("`{word}` in file-scope empty declaration"), node.span)
                    .with_code("E0667"),
            );
            return;
        }
        let useless = if node.storage.is_some() {
            "useless storage class specifier in empty declaration"
        } else if node.thread_local {
            "useless `_Thread_local` in empty declaration"
        } else if !node.quals.is_none() {
            "useless type qualifier in empty declaration"
        } else {
            return;
        };
        self.report(Diagnostic::warning(useless, node.span).with_code("E0611"));
        self.report(Diagnostic::warning("empty declaration", node.span).with_code("E0668"));
    }

    /// One declarator of a declaration, with whatever initializer it was given.
    fn init_declarator(
        &mut self,
        specs: ast::DeclSpecsId,
        item: ast::InitDeclarator,
    ) -> Option<DeclId> {
        // A deduced type is not known until its initializer is checked, and until then the
        // declaration is made with `int` so that everything else about it is still checked.
        let deduces = self.ast[specs].deduces();
        let deducible = deduces.is_some_and(|which| self.deducible(which, item));
        let ty =
            if deduces.is_some() { self.int() } else { self.declared_type(specs, item.declarator) };
        // `constexpr` implies `const`, which C23 6.7.2p6 says and which is what makes taking the
        // address of one and writing through it the diagnostic gcc gives it rather than silence.
        // An array is qualified through its element, so `constexpr int a[3];` is an array of
        // `const int` and not a `const` array, which is 6.7.3p10 and what the qualifier does.
        let ty = if self.ast[specs].constexpr {
            self.types.qualified(ty, Qualifiers::CONST)
        } else {
            ty
        };
        // `vector_size` changes which type this declares rather than how that type is laid out,
        // so it is applied before anything else reads the type, and before the typedef below
        // takes its early exit: a typedef is where the attribute is nearly always written.
        let ty = self.vectorized(ty, self.ast[specs].attrs);
        let ty = self.vectorized(ty, item.attrs);
        let node = self.ast[item.declarator];
        // A declarator with no name in a declaration is a parse that did not work out, and the
        // parser has already said so.
        let name = node.name?;
        let span = node.name_span;
        let specs = self.ast[specs];
        if specs.is_typedef() {
            self.typedef(name, ty, &specs, item, span);
            return None;
        }
        let kind = if is_function(&self.types, ty) { DeclKind::Function } else { DeclKind::Object };
        let (linkage, duration) = self.placement(&specs, kind, name, span);
        let state = self.definition_state(&specs, kind, item.init.is_some());
        self.check_initializer_placement(&specs, item.init.is_some(), name, span);
        self.check_specifiers(&specs, kind, name, span);
        let alignment = match specs.align {
            Some(align) => self.alignment(align, ty, kind, name, span),
            None => None,
        };
        let alignment = alignment.max(self.attribute_alignment(specs.attrs, item.attrs, ty));
        let mut declared = Declared {
            name,
            ty,
            kind,
            linkage,
            duration,
            state,
            initialized: item.init.is_some(),
            alignment,
            constant: specs.constexpr,
            // Written on the specifiers it is shared with the declarators beside this one, and
            // written after the declarator it is this declaration's alone. Either place asks for
            // the same thing, so either place is read.
            retained: self.retains(specs.attrs) || self.retains(item.attrs),
            asm_label: self.declared_label(item, &specs, duration, name, span),
            takes_prior_linkage: takes_prior_linkage(&specs, kind),
            span,
        };
        let id = self.merge(declared);
        // An initializer that did not work out leaves the object without a size, and saying so
        // a second time helps nobody, so what it did decides whether the size is asked about.
        let mut worked = true;
        // A declaration that deduces a type and is not written so that it can has been reported
        // and leaves nothing here for its initializer to be checked against.
        let init = if deduces.is_some() && !deducible { None } else { item.init };
        if let Some(init) = init {
            let constant = specs.constexpr;
            // A declaration that has no type or no value of its own until its initializer is
            // checked is what C23 calls underspecified, and its name being in scope inside that
            // initializer is what makes a reference to it something to report rather than a use.
            if deduces.is_some() || constant {
                self.underspecified.push(id);
            }
            let deduce = deduces.map(|_| specs.quals);
            let result = self.initializer(id, init, constant, deduce, span);
            if deduces.is_some() || constant {
                self.underspecified.pop();
            }
            match result {
                Some((entries, ty)) => {
                    // The type comes back because an array whose length nobody wrote takes the
                    // one its initializer implies, and this is where it becomes the type.
                    let mut node = self.tast[id].clone();
                    node.init = Some(entries);
                    node.ty = ty;
                    self.tast.set_decl(id, node);
                }
                None => worked = false,
            }
        }
        if kind == DeclKind::Object && worked {
            // After the initializer, because `int a[] = { 1, 2 }` has a size and an `int a[]`
            // with nothing after it does not, and the initializer is what tells them apart.
            declared.ty = self.tast[id].ty;
            self.check_storage_size(&declared);
        }
        Some(id)
    }

    /// A `typedef`, which declares a name for a type and nothing that exists at run time.
    fn typedef(
        &mut self,
        name: Symbol,
        ty: TypeId,
        specs: &ast::DeclSpecs,
        item: ast::InitDeclarator,
        span: Span,
    ) {
        if item.init.is_some() {
            let spelled = self.text(name).to_owned();
            self.report(
                Diagnostic::error(
                    format!("typedef '{spelled}' is initialized (use '__typeof__' instead)"),
                    span,
                )
                .with_code("E0600"),
            );
        }
        if specs.align.is_some() {
            let spelled = self.text(name).to_owned();
            self.report(
                Diagnostic::error(format!("alignment specified for typedef '{spelled}'"), span)
                    .with_code("E0604"),
            );
        }
        // `_Alignas` on a typedef is the error above and `aligned` on one is not, which is not
        // an inconsistency: C23 6.7.5p2 says the alignment specifier may not appear in a typedef
        // and gcc's attribute in this position is how a program says the same thing anyway. It
        // is the alignment the type has rather than a floor on it, so unlike the attribute on a
        // declaration it may lower one, and `typedef int L __attribute__((aligned(2)))` is an
        // `int` at a multiple of two.
        let asked = self.packing(specs.attrs).align.or(self.packing(item.attrs).align);
        let ty = match asked.and_then(NonZeroU32::new) {
            Some(align) => self.types.aligned_typedef(name, ty, align),
            None => ty,
        };
        match self.scopes.lookup_here(name) {
            // A typedef may be written twice for the same type, which is what lets two headers
            // that both define `size_t` be included by one file.
            Some(Binding::Typedef(previous)) if compatible(&self.types, previous, ty) => {}
            Some(Binding::Typedef(_)) => {
                self.conflicting_types(name, ty, None, span);
                return;
            }
            Some(Binding::Decl(previous)) => {
                self.different_kind(name, Some(previous), span);
                return;
            }
            Some(Binding::Enumerator { .. }) => {
                self.different_kind(name, None, span);
                return;
            }
            None => {}
        }
        self.declare_typedef(name, ty);
    }

    /// The linkage and the storage duration, which the scope and the keyword decide together.
    fn placement(
        &mut self,
        specs: &ast::DeclSpecs,
        kind: DeclKind,
        name: Symbol,
        span: Span,
    ) -> (Linkage, StorageDuration) {
        let file_scope = self.scopes.at_file_scope();
        let storage = specs.storage;
        if kind == DeclKind::Function {
            // A function is never automatic and never lives in a block, so the only storage
            // class it takes is `static`, and that only where there is a file for it to be
            // static to.
            let invalid = match storage {
                Some(StorageClass::Auto | StorageClass::Register) => true,
                Some(StorageClass::Static) => !file_scope,
                _ => false,
            };
            if invalid {
                let spelled = self.text(name).to_owned();
                self.report(
                    Diagnostic::error(
                        format!("invalid storage class for function '{spelled}'"),
                        span,
                    )
                    .with_code("E0596"),
                );
            }
            let linkage = if storage == Some(StorageClass::Static) && file_scope {
                Linkage::Internal
            } else {
                Linkage::External
            };
            return (linkage, StorageDuration::Static);
        }
        if file_scope {
            let spelled = self.text(name).to_owned();
            match storage {
                Some(StorageClass::Auto) => {
                    self.report(
                        Diagnostic::error(
                            format!("file-scope declaration of '{spelled}' specifies 'auto'"),
                            span,
                        )
                        .with_code("E0594"),
                    );
                }
                // gcc words this after the GNU extension that ties a register variable to a
                // named register, since that is the only thing `register` at file scope could
                // mean.
                Some(StorageClass::Register) => {
                    self.report(
                        Diagnostic::error(
                            format!("register name not specified for '{spelled}'"),
                            span,
                        )
                        .with_code("E0595"),
                    );
                }
                _ => {}
            }
            let linkage = match storage {
                Some(StorageClass::Static) => Linkage::Internal,
                _ if specs.constexpr => Linkage::Internal,
                _ => Linkage::External,
            };
            let duration =
                if specs.thread_local { StorageDuration::Thread } else { StorageDuration::Static };
            return (linkage, duration);
        }
        // A block-scope object has no linkage unless it says `extern`, in which case it names
        // whatever the rest of the program named and lives as long as the program does.
        let stored =
            if specs.thread_local { StorageDuration::Thread } else { StorageDuration::Static };
        match storage {
            Some(StorageClass::Extern) => (Linkage::External, stored),
            Some(StorageClass::Static) => (Linkage::None, stored),
            _ if specs.thread_local => {
                let spelled = self.text(name).to_owned();
                self.report(
                    Diagnostic::error(
                        format!(
                            "function-scope '{spelled}' implicitly auto and declared \
                             '_Thread_local'"
                        ),
                        span,
                    )
                    .with_code("E0597"),
                );
                (Linkage::None, StorageDuration::Thread)
            }
            _ => (Linkage::None, StorageDuration::Automatic),
        }
    }

    /// How much of a definition this declaration is.
    fn definition_state(
        &mut self,
        specs: &ast::DeclSpecs,
        kind: DeclKind,
        has_init: bool,
    ) -> Definition {
        if kind == DeclKind::Function {
            return Definition::Declared;
        }
        if has_init {
            return Definition::Defined;
        }
        if specs.storage == Some(StorageClass::Extern) {
            return Definition::Declared;
        }
        // A file-scope object with no initializer defines the object only if nothing else in the
        // translation unit does, which is not known here and is what tentative means.
        if self.scopes.at_file_scope() { Definition::Tentative } else { Definition::Defined }
    }

    /// What may and may not carry an initializer, which the storage class decides.
    fn check_initializer_placement(
        &mut self,
        specs: &ast::DeclSpecs,
        has_init: bool,
        name: Symbol,
        span: Span,
    ) {
        let spelled = self.text(name).to_owned();
        if specs.storage == Some(StorageClass::Extern) && has_init {
            // At file scope this is a definition written oddly, and gcc lets it through with a
            // warning. In a block there is no object here to initialize, so it is an error.
            let diagnostic = if self.scopes.at_file_scope() {
                Diagnostic::warning(format!("'{spelled}' initialized and declared 'extern'"), span)
                    .with_code("E0599")
            } else {
                Diagnostic::error(format!("'{spelled}' has both 'extern' and initializer"), span)
                    .with_code("E0598")
            };
            self.report(diagnostic);
        }
        if specs.constexpr && !has_init {
            self.report(
                Diagnostic::error("'constexpr' requires an initialized data declaration", span)
                    .with_code("E0617"),
            );
        }
    }

    /// The specifiers that mean something on a function and nothing on an object.
    fn check_specifiers(
        &mut self,
        specs: &ast::DeclSpecs,
        kind: DeclKind,
        name: Symbol,
        span: Span,
    ) {
        if kind == DeclKind::Function || specs.func.is_none() {
            return;
        }
        let spelled = self.text(name).to_owned();
        let word = if specs.func.has(FuncSpecs::INLINE) { "inline" } else { "_Noreturn" };
        self.report(
            Diagnostic::warning(format!("variable '{spelled}' declared '{word}'"), span)
                .with_code("E0609"),
        );
    }

    /// Whether there is enough of the type to make an object of it.
    fn check_storage_size(&mut self, declared: &Declared) {
        // An `extern` declaration makes no object, so it is allowed to name a type whose size
        // only the definition elsewhere knows.
        if declared.state == Definition::Declared {
            return;
        }
        let spelled = self.text(declared.name).to_owned();
        if self.is_variable_length(declared.ty) {
            if declared.duration != StorageDuration::Automatic {
                self.report(
                    Diagnostic::error(
                        format!("storage size of '{spelled}' isn't constant"),
                        declared.span,
                    )
                    .with_code("E0602"),
                );
            }
            return;
        }
        if is_complete(&self.types, declared.ty) {
            return;
        }
        // A tentative definition is allowed to be an array of no size, since a later declaration
        // may give it one and the end of the translation unit gives it one element if none does.
        if declared.state == Definition::Tentative && self.is_unsized_array(declared.ty) {
            return;
        }
        // gcc words the `void` case after the type in a block and after the size at file scope,
        // which is not a distinction anyone would design and is what it prints.
        let void = is_void(&self.types, declared.ty) && !self.scopes.at_file_scope();
        let diagnostic = if void {
            Diagnostic::error(format!("variable or field '{spelled}' declared void"), declared.span)
                .with_code("E0603")
        } else {
            Diagnostic::error(format!("storage size of '{spelled}' isn't known"), declared.span)
                .with_code("E0601")
        };
        self.report(diagnostic);
    }

    /// What the `aligned` attribute on a declaration asks for, which is a raise and never a lower.
    ///
    /// `alignas` and `aligned` ask the same thing in two spellings and do not follow the same
    /// rule. C23 6.7.5p5 makes an `alignas` below the type's own alignment a constraint violation,
    /// which is the diagnostic [`Self::alignment`] gives it, and gcc's attribute below it is
    /// ignored without a word: the attribute raises an alignment and never lowers one. So one
    /// below what the type already has is not an error here and is not an override either, and
    /// the declaration keeps the alignment it would have had.
    ///
    /// Written on the specifiers it is shared with the declarators beside this one, and written
    /// after the declarator it is this declaration's alone. Either place asks for the same thing,
    /// so either place is read, which is how `retained` reads them.
    fn attribute_alignment(&mut self, specs: AttrList, item: AttrList, ty: TypeId) -> Option<u32> {
        let on_specs = self.packing(specs).align;
        let on_item = self.packing(item).align;
        let asked = [on_specs, on_item].into_iter().flatten().max()?;
        let natural = layout(&self.types, ty, self.cx.target).map_or(1, |l| l.align);
        (i128::from(asked) > i128::from(natural)).then_some(asked)
    }

    /// What `alignas` asked for, folded and checked against what the type already has.
    fn alignment(
        &mut self,
        align: AlignSpec,
        ty: TypeId,
        kind: DeclKind,
        name: Symbol,
        span: Span,
    ) -> Option<u32> {
        let spelled = self.text(name).to_owned();
        if kind == DeclKind::Function {
            self.report(
                Diagnostic::error(format!("alignment specified for function '{spelled}'"), span)
                    .with_code("E0605"),
            );
            return None;
        }
        let requested = match align {
            AlignSpec::Type(named) => {
                let named = self.type_name(named);
                i128::from(layout(&self.types, named, self.cx.target).ok()?.align)
            }
            AlignSpec::Expr(expr) => {
                let value = self.expr(expr);
                match self.eval_integer(value) {
                    Ok(value) => value,
                    Err(failed) => {
                        if !failed.poisoned {
                            self.report(
                                Diagnostic::error(
                                    "requested alignment is not an integer constant",
                                    self.tast.expr_span(failed.at),
                                )
                                .with_code("E0606"),
                            );
                        }
                        return None;
                    }
                }
            }
        };
        // C23 6.7.5p4 says `alignas(0)` has no effect, which is the one value below one that is
        // not a mistake.
        if requested == 0 {
            return None;
        }
        if requested < 0 || requested & (requested - 1) != 0 {
            self.report(
                Diagnostic::error(
                    format!("requested alignment '{requested}' is not a positive power of 2"),
                    span,
                )
                .with_code("E0607"),
            );
            return None;
        }
        let natural = layout(&self.types, ty, self.cx.target).map_or(1, |l| l.align);
        if requested < i128::from(natural) {
            self.report(
                Diagnostic::error(
                    format!("'_Alignas' specifiers cannot reduce alignment of '{spelled}'"),
                    span,
                )
                .with_code("E0608"),
            );
            return None;
        }
        u32::try_from(requested).ok()
    }

    /// The declaration this one names, which may be one that was already made.
    fn merge(&mut self, declared: Declared) -> DeclId {
        // A declaration with linkage answers to one anywhere in sight, since it names the same
        // object, and one without linkage answers only to this scope, which is what lets a
        // function declare a local called `printf`.
        let binding = match self.scopes.lookup_here(declared.name) {
            Some(binding) => Some(binding),
            // The one in sight is the innermost that has a linkage of its own. 6.2.2p4 gives
            // `extern` the linkage of a visible prior declaration only where that declaration
            // has one, so a local of the same name in the block outside is walked past rather
            // than stopped at: `int v = 4; { extern int v; }` leaves the inner one naming the
            // object at file scope, and gcc reads it the same way. The same pair written in one
            // block is caught above, by the lookup that asks about this scope alone.
            None if declared.linkage != Linkage::None => {
                self.scopes.lookup_where(declared.name, |binding| match binding {
                    Binding::Decl(id) => self.tast[id].linkage != Linkage::None,
                    Binding::Typedef(_) | Binding::Enumerator { .. } => true,
                })
            }
            None => None,
        };
        let previous = match binding {
            Some(Binding::Decl(id)) => id,
            Some(Binding::Typedef(_)) => {
                self.different_kind(declared.name, None, declared.span);
                return self.declare(declared);
            }
            Some(Binding::Enumerator { .. }) => {
                self.different_kind(declared.name, None, declared.span);
                return self.declare(declared);
            }
            None => return self.declare(declared),
        };
        let node = self.tast[previous].clone();
        if node.kind != declared.kind {
            self.different_kind(declared.name, Some(previous), declared.span);
            return self.declare(declared);
        }
        if !self.check_linkage(&node, &declared, previous) {
            return self.declare(declared);
        }
        if !compatible(&self.types, node.ty, declared.ty) {
            self.conflicting_types(declared.name, declared.ty, Some(previous), declared.span);
            return previous;
        }
        if node.state == Definition::Defined && declared.state == Definition::Defined {
            let spelled = self.text(declared.name).to_owned();
            let (note, at) = self.previous_note(previous);
            self.report(
                Diagnostic::error(format!("redefinition of '{spelled}'"), declared.span)
                    .with_code("E0588")
                    .note(note, at),
            );
            return previous;
        }
        let ty = composite(&mut self.types, node.ty, declared.ty).unwrap_or(declared.ty);
        let merged = Decl {
            ty,
            // `extern` after `static` keeps the internal linkage the first declaration gave the
            // name, which is 6.2.2p4 and what every library that hides a symbol relies on.
            linkage: if declared.takes_prior_linkage { node.linkage } else { declared.linkage },
            state: stronger(node.state, declared.state),
            alignment: node.alignment.max(declared.alignment),
            // One declaration of a name asking for it to be kept is enough, which is what lets a
            // header write `used` on the declaration and the file define it without.
            retained: node.retained || declared.retained,
            asm_label: self.merged_label(&node, &declared, previous),
            ..node
        };
        self.tast.set_decl(previous, merged);
        self.scopes.declare(declared.name, Binding::Decl(previous));
        previous
    }

    /// The assembler name one declarator wrote, where there is a symbol for it to name.
    ///
    /// An object that lives on the stack has none. It is a slot at an offset rather than
    /// something with a name, so gcc warns and carries on and this says what gcc says. A
    /// `register` one is gcc's other reading of the same syntax, where the string is a machine
    /// register rather than a symbol and the object is kept in it. That is a feature of its own
    /// and is not here yet, so it is warned about in its own words rather than passed off as the
    /// first case: a program that writes one is writing assembly around it and would otherwise
    /// be told nothing at all.
    fn declared_label(
        &mut self,
        item: ast::InitDeclarator,
        specs: &ast::DeclSpecs,
        duration: StorageDuration,
        name: Symbol,
        span: Span,
    ) -> Option<StrId> {
        let label = self.asm_label(item.asm_label?, span);
        if duration != StorageDuration::Automatic {
            return Some(label);
        }
        let spelled = self.text(name).to_owned();
        let (what, code) = match specs.storage {
            Some(StorageClass::Register) => {
                (format!("'asm' specifier for register variable '{spelled}' ignored"), "E0692")
            }
            _ => (
                format!("ignoring 'asm' specifier for non-static local variable '{spelled}'"),
                "E0693",
            ),
        };
        self.report(Diagnostic::warning(what, span).with_code(code));
        None
    }

    /// The assembler name a declaration wrote, copied into the typed tree.
    ///
    /// A wide one is refused where the assembly statement's strings are refused and in the same
    /// words, since a symbol is bytes and there is nothing an assembler could be handed here.
    fn asm_label(&mut self, id: ast::StrId, span: Span) -> StrId {
        let literal = self.ast[id].clone();
        self.asm_narrow(&literal, span);
        self.tast.add_string(literal)
    }

    /// The assembler name of a name that has been declared before, which is the first one
    /// written.
    ///
    /// A second one that disagrees is ignored rather than taken, because the earlier name may
    /// already have been used and every use of a name is the same symbol. gcc warns and keeps
    /// the first, and a program that hits this has a header disagreeing with itself.
    fn merged_label(
        &mut self,
        node: &Decl,
        declared: &Declared,
        previous: DeclId,
    ) -> Option<StrId> {
        let (Some(before), Some(now)) = (node.asm_label, declared.asm_label) else {
            return node.asm_label.or(declared.asm_label);
        };
        if self.tast[before].elements != self.tast[now].elements {
            let (note, at) = self.previous_note(previous);
            self.report(
                Diagnostic::warning(
                    "'asm' declaration ignored due to conflict with previous rename",
                    declared.span,
                )
                .with_code("E0691")
                .note(note, at),
            );
        }
        Some(before)
    }

    /// Whether the two declarations agree about who can see the name.
    fn check_linkage(&mut self, node: &Decl, declared: &Declared, previous: DeclId) -> bool {
        let spelled = self.text(declared.name).to_owned();
        let message = match (node.linkage, declared.linkage) {
            // Two declarations in a block that both give the name a value are a redefinition, and
            // `merge` says so in the same words it uses for a name at file scope. The linkage is
            // the interesting part only when at least one of them stops short of saying what the
            // object holds, which is where gcc draws the line as well.
            (Linkage::None, Linkage::None) if node.init.is_some() && declared.initialized => {
                return true;
            }
            (Linkage::None, Linkage::None) => {
                format!("redeclaration of '{spelled}' with no linkage")
            }
            (Linkage::None, _) => {
                format!("extern declaration of '{spelled}' follows declaration with no linkage")
            }
            (_, Linkage::None) => {
                format!("declaration of '{spelled}' with no linkage follows extern declaration")
            }
            (Linkage::External, Linkage::Internal) => {
                format!("static declaration of '{spelled}' follows non-static declaration")
            }
            // `extern` says nothing about which linkage it wants and takes what is there, so the
            // contradiction is only with a declaration that says nothing at all.
            (Linkage::Internal, Linkage::External) if !declared.takes_prior_linkage => {
                format!("non-static declaration of '{spelled}' follows static declaration")
            }
            _ => return true,
        };
        let (note, at) = self.previous_note(previous);
        self.report(Diagnostic::error(message, declared.span).with_code("E0592").note(note, at));
        false
    }

    /// Puts a declaration in the tree and binds the name to it.
    fn declare(&mut self, declared: Declared) -> DeclId {
        let node = Decl {
            name: Some(declared.name),
            ty: declared.ty,
            kind: declared.kind,
            linkage: declared.linkage,
            duration: declared.duration,
            state: declared.state,
            alignment: declared.alignment,
            constant: declared.constant,
            retained: declared.retained,
            asm_label: declared.asm_label,
            init: None,
            params: DeclList::EMPTY,
            body: None,
        };
        let id = self.tast.decl(node, declared.span);
        self.scopes.declare(declared.name, Binding::Decl(id));
        if self.scopes.at_file_scope() {
            self.tast.add_top_level(id);
        }
        id
    }

    /// The values an initializer stores, and the type the object ended up with.
    ///
    /// The walk itself is in `check/init.rs`. What is here is the one thing about an
    /// initializer that is a fact about the declaration rather than about the object: a
    /// function cannot have one.
    fn initializer(
        &mut self,
        decl: DeclId,
        init: ast::InitId,
        constant: bool,
        deduce: Option<ast::Quals>,
        span: Span,
    ) -> Option<(InitList, TypeId)> {
        let node = &self.tast[decl];
        let (ty, kind, name, duration) = (node.ty, node.kind, node.name, node.duration);
        if kind == DeclKind::Function {
            let spelled = name.map_or_else(String::new, |name| self.text(name).to_owned());
            self.report(
                Diagnostic::error(
                    format!("function '{spelled}' is initialized like a variable"),
                    span,
                )
                .with_code("E0615"),
            );
            return None;
        }
        let is_static = duration != StorageDuration::Automatic;
        match deduce {
            Some(quals) => self.init_deduced(name, is_static, init, constant, quals, span),
            None => self.init_object(ty, name, is_static, init, constant, span),
        }
    }

    /// The message for a deduced type whose declarator is more than the name it has to be.
    ///
    /// gcc words it differently for the two spellings, since only C23's takes the attributes
    /// that its wording mentions.
    fn not_plain(&mut self, which: ast::Deduction, span: Span) {
        let spelled = which.spelling();
        let allowed = match which {
            ast::Deduction::Auto => ", possibly with attributes,",
            ast::Deduction::AutoType => "",
        };
        self.report(
            Diagnostic::error(
                format!("'{spelled}' requires a plain identifier{allowed} as declarator"),
                span,
            )
            .with_code("E0651"),
        );
    }

    /// Whether a declarator that deduces its type is written so that it can.
    ///
    /// Both constraints are gcc's. A deduced type comes from an initializer, so there has to be
    /// one. It is the whole type, so there is nothing left for a declarator to add to it and it
    /// has to be a name and no more than a name: `auto *p = q;` names no type, however obvious
    /// what it was meant to mean.
    fn deducible(&mut self, which: ast::Deduction, item: ast::InitDeclarator) -> bool {
        let spelled = which.spelling();
        let node = self.ast[item.declarator];
        let span = if node.name.is_some() { node.name_span } else { node.span };
        if !self.ast[node.derived].is_empty() {
            self.not_plain(which, span);
            return false;
        }
        if item.init.is_none() {
            self.report(
                Diagnostic::error(
                    format!("'{spelled}' requires an initialized data declaration"),
                    span,
                )
                .with_code("E0651"),
            );
            return false;
        }
        true
    }

    /// `static_assert`, which is the one declaration whose whole purpose is to be checked.
    fn static_assert(&mut self, cond: ast::ExprId, message: Option<ast::StrId>, span: Span) {
        let cond = self.expr(cond);
        let cond = self.value(cond);
        if self.is_poisoned(cond) {
            return;
        }
        let value = match self.eval_integer(cond) {
            Ok(value) => value,
            Err(failed) => {
                if !failed.poisoned {
                    self.report(
                        Diagnostic::error(
                            "expression in static assertion is not constant",
                            self.tast.expr_span(failed.at),
                        )
                        .with_code("E0614"),
                    );
                }
                return;
            }
        };
        if value != 0 {
            return;
        }
        let message = match message {
            Some(id) => format!("static assertion failed: {}", self.quoted(id)),
            None => "static assertion failed".to_owned(),
        };
        self.report(Diagnostic::error(message, span).with_code("E0613"));
    }

    /// The diagnostic for two declarations of one name that do not describe the same thing.
    fn conflicting_types(
        &mut self,
        name: Symbol,
        ty: TypeId,
        previous: Option<DeclId>,
        span: Span,
    ) {
        let spelled = self.text(name).to_owned();
        let written = self.spell(ty);
        let mut diagnostic =
            Diagnostic::error(format!("conflicting types for '{spelled}'; have '{written}'"), span)
                .with_code("E0586");
        if let Some(previous) = previous {
            let (note, at) = self.previous_note(previous);
            diagnostic = diagnostic.note(note, at);
        }
        self.report(diagnostic);
    }

    /// The diagnostic for a name that already means something of another kind.
    fn different_kind(&mut self, name: Symbol, previous: Option<DeclId>, span: Span) {
        let spelled = self.text(name).to_owned();
        let mut diagnostic =
            Diagnostic::error(format!("'{spelled}' redeclared as different kind of symbol"), span)
                .with_code("E0587");
        if let Some(previous) = previous {
            let (note, at) = self.previous_note(previous);
            diagnostic = diagnostic.note(note, at);
        }
        self.report(diagnostic);
    }

    /// What the note under a redeclaration says, and where it points.
    ///
    /// A `typedef` and an enumerator get no note, because a binding is a type or a value and
    /// neither remembers where it was written. That is a smaller loss than it sounds: the note is
    /// a courtesy and the error above it names the same identifier.
    fn previous_note(&self, id: DeclId) -> (String, Span) {
        let node = &self.tast[id];
        let word = if node.state == Definition::Defined { "definition" } else { "declaration" };
        let name = node.name.map_or("", |name| self.text(name));
        let ty = self.spell(node.ty);
        (format!("previous {word} of '{name}' with type '{ty}'"), self.tast.decl_span(id))
    }

    /// A string literal written back out, for the assertion that quotes its message.
    fn quoted(&self, id: ast::StrId) -> String {
        let mut out = String::from("\"");
        for &element in &self.ast[id].elements {
            match char::from_u32(element) {
                Some('"') => out.push_str("\\\""),
                Some('\\') => out.push_str("\\\\"),
                Some('\n') => out.push_str("\\n"),
                Some(c) if !c.is_control() => out.push(c),
                _ => out.push_str(&format!("\\x{element:x}")),
            }
        }
        out.push('"');
        out
    }

    /// Whether a type is an array whose length nobody has said yet.
    pub(in crate::check) fn is_unsized_array(&self, ty: TypeId) -> bool {
        matches!(
            self.types.kind(self.types.canonical(ty)),
            TypeKind::Array { len: ArrayLen::Unknown, .. }
        )
    }

    /// A declaration form that is recognised and not checked yet.
    fn declaration_unsupported(&mut self, what: &str, span: Span) {
        self.report(
            Diagnostic::error(format!("{what} is not supported yet"), span).with_code("E0519"),
        );
    }
}

/// Whether a declaration says nothing about which linkage it wants, and so takes whatever the
/// declaration before it had.
///
/// `extern` is the spelling that does this for an object. A function that says nothing is the
/// other one: C 6.2.2p5 gives a function declared with no storage class the linkage `extern`
/// would have given it, which is what makes `static int f(void); int f(void) { return 1; }` a
/// pair of declarations of one static function where the same pair written on an object is two
/// declarations that disagree. gcc draws the line in the same place, and the idiom of declaring
/// a static function ahead of its definition and leaving the keyword off the definition is
/// common enough in real code that this is not a corner.
fn takes_prior_linkage(specs: &ast::DeclSpecs, kind: DeclKind) -> bool {
    match specs.storage {
        Some(StorageClass::Extern) => true,
        None => kind == DeclKind::Function,
        _ => false,
    }
}

/// The stronger of two definition states, which is what a redeclaration leaves behind.
fn stronger(a: Definition, b: Definition) -> Definition {
    let rank = |state| match state {
        Definition::Declared => 0,
        Definition::Tentative => 1,
        Definition::Defined => 2,
    };
    if rank(a) >= rank(b) { a } else { b }
}

#[cfg(test)]
mod tests {
    use rucc_ast::{
        ArraySize, AttrList, Builtin, BuiltinSet, DeclSpecs, DeclSpecsId, Declarator, DeclaratorId,
        Derived, ParamKind, ParamList, Quals, RecordKind, TypeSpec,
    };
    use rucc_base::Interner;
    use rucc_diag::{Severity, Span};
    use rucc_lex::{Encoding, IntConstant, IntConstantType, Remarks, StringLiteral};
    use rucc_session::Std;
    use rucc_target::{TargetInfo, Triple};
    use rucc_types::IntKind;

    use super::*;
    use crate::check::Context;
    use crate::print::Printer;

    /// The untyped tree a test checks, built by hand.
    ///
    /// Everything is built before the checker exists, because the checker borrows the interner
    /// for as long as it lives and a test that has started cannot invent another name.
    struct Fixture {
        ast: rucc_ast::Ast,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        fn new() -> Fixture {
            let target =
                TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
            Fixture { ast: rucc_ast::Ast::new(), names: Interner::new(), target }
        }

        fn name(&mut self, text: &str) -> Symbol {
            self.names.intern(text)
        }

        /// `int`, as a specifier list the test can add words to before it is added.
        fn int_specs(&self) -> DeclSpecs {
            self.builtin(BuiltinSet::INT)
        }

        /// `auto` or `__auto_type`, as the specifier list that deduces a type.
        fn deduced(&self, which: ast::Deduction) -> DeclSpecs {
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            specs.ty = TypeSpec::Auto(which);
            specs
        }

        fn builtin(&self, keyword: BuiltinSet) -> DeclSpecs {
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            let builtin = Builtin::NONE.add(keyword).expect("a keyword written once");
            specs.ty = TypeSpec::Builtin(builtin);
            specs
        }

        /// `struct S`, either as a mention of the tag or as a definition of it.
        fn record(&mut self, tag: Option<&str>, fields: bool) -> DeclSpecs {
            let tag = tag.map(|tag| self.name(tag));
            let fields = fields.then(|| self.ast.add_member_list(&[]));
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            specs.ty = TypeSpec::Record {
                kind: RecordKind::Struct,
                tag,
                fields,
                attrs: AttrList::EMPTY,
                pack: None,
            };
            specs
        }

        fn declarator(&mut self, name: &str, derived: &[Derived]) -> DeclaratorId {
            let name = self.name(name);
            let derived = self.ast.add_derived_list(derived);
            self.ast.add_declarator(Declarator {
                name: Some(name),
                name_span: Span::DUMMY,
                derived,
                span: Span::DUMMY,
            })
        }

        fn int(&mut self, value: u128) -> ast::ExprId {
            let ty = IntConstantType::Standard(IntKind::Int);
            let id = self.ast.add_int(IntConstant { value, ty, remarks: Remarks::default() });
            self.ast.expr(ast::Expr::Int(id), Span::DUMMY)
        }

        fn use_name(&mut self, text: &str) -> ast::ExprId {
            let name = self.name(text);
            self.ast.expr(ast::Expr::Name(name), Span::DUMMY)
        }

        fn string(&mut self, text: &str) -> ast::StrId {
            let elements = text.chars().map(|c| c as u32).collect();
            self.ast.add_string(StringLiteral {
                elements,
                encoding: Encoding::Plain,
                remarks: Remarks::default(),
            })
        }

        /// A declaration of one name, from the specifiers, the derivations and the initializer.
        fn var(
            &mut self,
            specs: DeclSpecs,
            name: &str,
            derived: &[Derived],
            init: Option<ast::ExprId>,
        ) -> ast::DeclId {
            let declarator = self.declarator(name, derived);
            let init = init.map(|expr| self.ast.add_init(ast::Init::Expr(expr)));
            let item = ast::InitDeclarator {
                declarator,
                init,
                asm_label: None,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            };
            let declarators = self.ast.add_init_declarator_list(&[item]);
            let specs = self.specs(specs);
            self.ast.decl(ast::Decl::Var { specs, declarators }, Span::DUMMY)
        }

        /// `int x;` and the like, which is what most of these are.
        fn object(&mut self, specs: DeclSpecs, name: &str) -> ast::DeclId {
            self.var(specs, name, &[], None)
        }

        /// The same declaration with an assembler name written after the declarator.
        fn labelled(
            &mut self,
            specs: DeclSpecs,
            name: &str,
            derived: &[Derived],
            label: &str,
        ) -> ast::DeclId {
            let declarator = self.declarator(name, derived);
            let asm_label = Some(self.string(label));
            let item = ast::InitDeclarator {
                declarator,
                init: None,
                asm_label,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            };
            let declarators = self.ast.add_init_declarator_list(&[item]);
            let specs = self.specs(specs);
            self.ast.decl(ast::Decl::Var { specs, declarators }, Span::DUMMY)
        }

        /// A declaration with no declarator at all.
        fn bare(&mut self, specs: DeclSpecs) -> ast::DeclId {
            let specs = self.specs(specs);
            let declarators = self.ast.add_init_declarator_list(&[]);
            self.ast.decl(ast::Decl::Var { specs, declarators }, Span::DUMMY)
        }

        fn specs(&mut self, specs: DeclSpecs) -> DeclSpecsId {
            self.ast.add_specs(specs)
        }

        /// One parameter of a prototype.
        fn param(
            &mut self,
            specs: DeclSpecs,
            name: Option<&str>,
            derived: &[Derived],
        ) -> ast::Param {
            let declarator = match name {
                Some(name) => self.declarator(name, derived),
                None => {
                    let derived = self.ast.add_derived_list(derived);
                    self.ast.add_declarator(Declarator {
                        name: None,
                        name_span: Span::DUMMY,
                        derived,
                        span: Span::DUMMY,
                    })
                }
            };
            let specs = self.specs(specs);
            ast::Param { specs: Some(specs), declarator, attrs: AttrList::EMPTY, span: Span::DUMMY }
        }

        /// `(a, b)`, as the derivation that makes a declarator a function.
        fn takes(&mut self, params: &[ast::Param]) -> Derived {
            let params = self.ast.add_param_list(params);
            Derived::Function { params, variadic: false, kind: ParamKind::Prototype }
        }

        fn stmt(&mut self, stmt: ast::Stmt) -> ast::StmtId {
            self.ast.stmt(stmt, Span::DUMMY)
        }

        /// `{ ... }`, from the statements it holds.
        fn block(&mut self, body: &[ast::StmtId]) -> ast::StmtId {
            let body = self.ast.add_stmt_list(body);
            self.stmt(ast::Stmt::Compound(body))
        }

        /// A function definition, from its specifiers, its name, its derivations and its body.
        fn define(
            &mut self,
            specs: DeclSpecs,
            name: &str,
            derived: &[Derived],
            body: ast::StmtId,
        ) -> ast::DeclId {
            self.define_taking(specs, name, derived, &[], body)
        }

        /// The same, with the declarations an old-style definition writes under its identifier
        /// list. Every other definition has none.
        fn define_taking(
            &mut self,
            specs: DeclSpecs,
            name: &str,
            derived: &[Derived],
            declarations: &[ast::DeclId],
            body: ast::StmtId,
        ) -> ast::DeclId {
            let declarator = self.declarator(name, derived);
            let specs = self.specs(specs);
            let params = self.ast.add_decl_list(declarations);
            self.ast.decl(ast::Decl::Function { specs, declarator, params, body }, Span::DUMMY)
        }

        fn checker(&self) -> Checker<'_> {
            Checker::new(&self.ast, Context::new(&self.names, &self.target, Std::C23))
        }
    }

    /// `*`, which is the one derivation written often enough to be worth a name.
    fn pointer() -> Derived {
        Derived::Pointer { quals: Quals::NONE, attrs: AttrList::EMPTY }
    }

    /// `(void)`, which is what makes a declarator a function.
    fn function() -> Derived {
        Derived::Function { params: ParamList::EMPTY, variadic: false, kind: ParamKind::Void }
    }

    /// `[n]`, from whatever expression was written between the brackets.
    fn array(size: ast::ExprId) -> Derived {
        Derived::Array { size: ArraySize::Expr(size), quals: Quals::NONE, has_static: false }
    }

    /// The one declaration a checked declaration declared.
    fn only(checker: &Checker<'_>, list: DeclList) -> DeclId {
        let declared = &checker.tast[list];
        assert_eq!(declared.len(), 1, "expected exactly one declaration, got {declared:?}");
        declared[0]
    }

    /// One declaration and whatever hangs under it, which is what most assertions here are about.
    fn dump(checker: &Checker<'_>, id: DeclId) -> String {
        let mut printer = Printer::new(&checker.tast, &checker.types, checker.cx.names);
        printer.decl(id);
        printer.finish()
    }

    /// What was reported, as the messages alone, notes included.
    fn messages(checker: &Checker<'_>) -> Vec<String> {
        checker
            .errors
            .diagnostics()
            .iter()
            .flat_map(|d| {
                std::iter::once(d.message.clone())
                    .chain(d.children.iter().map(|n| n.message.clone()))
            })
            .collect()
    }

    /// The one message that was reported, which is what most of these tests expect.
    /// How bad each reported diagnostic was, which is the whole difference between the two
    /// halves of the empty declaration family.
    fn severities(checker: &Checker<'_>) -> Vec<Severity> {
        checker.errors.diagnostics().iter().map(|d| d.severity).collect()
    }

    fn message(checker: &Checker<'_>) -> String {
        let mut reported = messages(checker);
        assert_eq!(reported.len(), 1, "expected exactly one diagnostic, got {reported:?}");
        reported.pop().expect("one message")
    }

    #[test]
    fn a_file_scope_object_is_external_and_static_and_defines_nothing_by_itself() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let decl = f.object(specs, "x");

        let mut c = f.checker();
        let list = c.check_decl(decl);

        let id = only(&c, list);
        assert_eq!(dump(&c, id), "decl #0 x : int object external static tentative\n");
        assert_eq!(c.tast.top_level(), [id]);
        assert!(c.errors.is_empty());
    }

    #[test]
    fn the_same_three_words_in_a_block_are_a_local_with_no_linkage() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let decl = f.object(specs, "x");

        let mut c = f.checker();
        c.scopes.push();
        let list = c.check_decl(decl);

        let id = only(&c, list);
        assert_eq!(dump(&c, id), "decl #0 x : int object automatic defined\n");
        assert!(c.tast.top_level().is_empty());
        assert!(c.errors.is_empty());
    }

    #[test]
    fn static_at_file_scope_hides_the_name_and_in_a_block_only_lengthens_the_lifetime() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Static);
        let outer = f.object(specs, "x");
        let inner = f.object(specs, "y");

        let mut c = f.checker();
        let list = c.check_decl(outer);
        let outer = only(&c, list);
        c.scopes.push();
        let list = c.check_decl(inner);
        let inner = only(&c, list);

        assert_eq!(dump(&c, outer), "decl #0 x : int object internal static tentative\n");
        assert_eq!(dump(&c, inner), "decl #1 y : int object static defined\n");
        assert!(c.errors.is_empty());
    }

    #[test]
    fn extern_in_a_block_names_the_object_the_file_scope_declaration_named() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let outer = f.object(specs, "x");
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Extern);
        let inner = f.object(specs, "x");

        let mut c = f.checker();
        let list = c.check_decl(outer);
        let first = only(&c, list);
        c.scopes.push();
        let list = c.check_decl(inner);

        assert_eq!(only(&c, list), first, "the block-scope declaration is the same object");
        assert_eq!(dump(&c, first), "decl #0 x : int object external static tentative\n");
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_declaration_and_a_definition_of_one_name_are_one_declaration() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Extern);
        let declared = f.object(specs, "x");
        let specs = f.int_specs();
        let one = f.int(1);
        let defined = f.var(specs, "x", &[], Some(one));

        let mut c = f.checker();
        let list = c.check_decl(declared);
        let first = only(&c, list);
        let list = c.check_decl(defined);

        assert_eq!(only(&c, list), first);
        assert_eq!(
            dump(&c, first),
            "decl #0 x : int object external static defined\n  init\n    +0\n      const 1 : int\n"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn two_definitions_of_one_name_are_refused_and_the_first_one_is_pointed_at() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let one = f.int(1);
        let first = f.var(specs, "x", &[], Some(one));
        let two = f.int(2);
        let second = f.var(specs, "x", &[], Some(two));

        let mut c = f.checker();
        c.check_decl(first);
        c.check_decl(second);

        assert_eq!(
            messages(&c),
            ["redefinition of 'x'", "previous definition of 'x' with type 'int'"]
        );
    }

    #[test]
    fn a_redeclaration_with_another_type_says_which_type_this_one_has() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let first = f.object(specs, "x");
        let specs = f.builtin(BuiltinSet::CHAR);
        let second = f.object(specs, "x");

        let mut c = f.checker();
        c.check_decl(first);
        c.check_decl(second);

        assert_eq!(
            messages(&c),
            [
                "conflicting types for 'x'; have 'char'",
                "previous declaration of 'x' with type 'int'"
            ]
        );
    }

    #[test]
    fn two_declarations_of_an_array_leave_the_one_that_gave_a_bound() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let first = f.var(
            specs,
            "a",
            &[Derived::Array {
                size: ArraySize::Unspecified,
                quals: Quals::NONE,
                has_static: false,
            }],
            None,
        );
        let three = f.int(3);
        let second = f.var(specs, "a", &[array(three)], None);

        let mut c = f.checker();
        let list = c.check_decl(first);
        let id = only(&c, list);
        c.check_decl(second);

        assert_eq!(dump(&c, id), "decl #0 a : int[3] object external static tentative\n");
        assert!(c.errors.is_empty());
    }

    #[test]
    fn static_and_non_static_declarations_of_one_name_contradict_each_other_both_ways() {
        let mut f = Fixture::new();
        let plain = f.int_specs();
        let mut hidden = f.int_specs();
        hidden.storage = Some(StorageClass::Static);
        let (a, b) = (f.object(plain, "x"), f.object(hidden, "x"));
        let (c1, d) = (f.object(hidden, "y"), f.object(plain, "y"));

        let mut c = f.checker();
        c.check_decl(a);
        c.check_decl(b);
        c.check_decl(c1);
        c.check_decl(d);

        assert_eq!(
            messages(&c),
            [
                "static declaration of 'x' follows non-static declaration",
                "previous declaration of 'x' with type 'int'",
                "non-static declaration of 'y' follows static declaration",
                "previous declaration of 'y' with type 'int'",
            ]
        );
    }

    #[test]
    fn extern_after_static_keeps_the_linkage_the_first_declaration_gave_the_name() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Static);
        let first = f.object(specs, "x");
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Extern);
        let second = f.object(specs, "x");

        let mut c = f.checker();
        let list = c.check_decl(first);
        let id = only(&c, list);
        c.check_decl(second);

        assert_eq!(dump(&c, id), "decl #0 x : int object internal static tentative\n");
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_function_defined_with_no_keyword_after_a_static_declaration_is_still_static() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Static);
        let first = f.var(specs, "f", &[function()], None);
        let body = f.block(&[]);
        let specs = f.int_specs();
        let second = f.define(specs, "f", &[function()], body);

        let mut c = f.checker();
        let list = c.check_decl(first);
        let id = only(&c, list);
        c.check_decl(second);

        // The keyword is left off the definition all over real code, and C 6.2.2p5 says the
        // function keeps the linkage the declaration before it gave the name rather than
        // contradicting it. The same pair written on an object does contradict.
        assert_eq!(
            dump(&c, id),
            "decl #0 f : int(void) function internal defined\n  body\n    block\n"
        );
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn an_assembler_name_written_after_a_declarator_is_the_symbol_the_name_stands_for() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Extern);
        let decl = f.labelled(specs, "open", &[function()], "open64");

        let mut c = f.checker();
        let list = c.check_decl(decl);

        // Which is how the C library redirects a name, and the declaration is the only place
        // that says so: nothing about the type or the linkage of `open` has changed and every
        // use of it reaches `open64`.
        let id = only(&c, list);
        assert_eq!(
            dump(&c, id),
            "decl #0 open : int(void) function external declared asm \"open64\"\n"
        );
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn the_definition_under_a_declaration_that_renamed_the_name_is_the_renamed_symbol() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Static);
        let first = f.labelled(specs, "f", &[function()], "g");
        let body = f.block(&[]);
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Static);
        let second = f.define(specs, "f", &[function()], body);

        let mut c = f.checker();
        let list = c.check_decl(first);
        let id = only(&c, list);
        c.check_decl(second);

        // A definition has no declarator to write a name after, so the declaration above it is
        // where the program said what the symbol is and the merge has to keep it. Losing it
        // here would emit the body under one symbol and every call to it under another.
        assert_eq!(
            dump(&c, id),
            "decl #0 f : int(void) function internal defined asm \"g\"\n  body\n    block\n"
        );
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn a_second_assembler_name_that_disagrees_is_ignored_and_the_first_one_stands() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Extern);
        let first = f.labelled(specs, "f", &[function()], "g");
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Extern);
        let second = f.labelled(specs, "f", &[function()], "h");

        let mut c = f.checker();
        let list = c.check_decl(first);
        let id = only(&c, list);
        c.check_decl(second);

        // The name may already have been used by the time the second one is read, and every use
        // of a name is the same symbol, so there is nothing to do with the second but say it was
        // dropped. gcc keeps the first one as well.
        assert_eq!(dump(&c, id), "decl #0 f : int(void) function external declared asm \"g\"\n");
        assert_eq!(severities(&c), [Severity::Warning]);
        assert_eq!(
            messages(&c)[0],
            "'asm' declaration ignored due to conflict with previous rename"
        );
    }

    #[test]
    fn an_assembler_name_on_an_object_that_lives_on_the_stack_is_dropped_with_a_word_about_it() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let decl = f.labelled(specs, "x", &[], "g");

        let mut c = f.checker();
        c.scopes.push();
        let list = c.check_decl(decl);

        // There is no symbol for the name to stand for: what a local is, is an offset from the
        // frame pointer. gcc warns about it in these words rather than refusing, and a header
        // that writes one has done no harm, so the object is declared and the name is dropped.
        let id = only(&c, list);
        assert_eq!(dump(&c, id), "decl #0 x : int object automatic defined\n");
        assert_eq!(severities(&c), [Severity::Warning]);
        assert_eq!(messages(&c)[0], "ignoring 'asm' specifier for non-static local variable 'x'");
    }

    #[test]
    fn a_local_kept_in_a_named_register_says_that_the_register_is_not_honoured() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Register);
        let decl = f.labelled(specs, "x", &[], "r12");

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(decl);

        // Which is gcc's other reading of this syntax: the string is a machine register and the
        // object is kept in it, which programs that write assembly around a variable depend on.
        // It is a feature of its own and is not here, and saying so is the least that is owed to
        // a program whose next line hands that register to an `asm` statement.
        assert_eq!(severities(&c), [Severity::Warning]);
        assert_eq!(messages(&c)[0], "'asm' specifier for register variable 'x' ignored");
    }

    #[test]
    fn two_locals_of_one_name_in_one_block_are_refused_as_having_no_linkage() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let first = f.object(specs, "x");
        let second = f.object(specs, "x");

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(first);
        c.check_decl(second);

        assert_eq!(
            messages(&c),
            ["redeclaration of 'x' with no linkage", "previous definition of 'x' with type 'int'"]
        );
    }

    #[test]
    fn an_extern_declaration_looks_past_a_local_of_the_same_name_in_the_block_outside_it() {
        // C 6.2.2p4 hands `extern` the linkage of a visible prior declaration only where that
        // declaration has a linkage of its own. The local in the block outside has none, so the
        // inner declaration is of the object at file scope rather than a second declaration of
        // the local, and it is not the contradiction the pair written in one block is. gcc reads
        // it the same way, and this used to report the pair in two blocks as well.
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let file = f.object(specs, "v");
        let specs = f.int_specs();
        let local = f.object(specs, "v");
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Extern);
        let outward = f.object(specs, "v");

        let mut c = f.checker();
        let list = c.check_decl(file);
        let first = only(&c, list);
        c.scopes.push();
        c.check_decl(local);
        c.scopes.push();
        let list = c.check_decl(outward);

        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
        assert_eq!(only(&c, list), first, "it names the object at file scope");
    }

    #[test]
    fn a_name_that_already_means_a_type_is_redeclared_as_a_different_kind_of_symbol() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Typedef);
        let named = f.object(specs, "T");
        let specs = f.int_specs();
        let object = f.object(specs, "T");

        let mut c = f.checker();
        c.check_decl(named);
        c.check_decl(object);

        assert_eq!(message(&c), "'T' redeclared as different kind of symbol");
    }

    #[test]
    fn a_typedef_may_be_written_twice_for_one_type_and_not_for_two() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Typedef);
        let first = f.object(specs, "T");
        let again = f.object(specs, "T");
        let mut specs = f.builtin(BuiltinSet::CHAR);
        specs.storage = Some(StorageClass::Typedef);
        let other = f.object(specs, "T");

        let mut c = f.checker();
        let declared = c.check_decl(first);
        assert!(c.tast[declared].is_empty(), "a typedef declares nothing at run time");
        c.check_decl(again);
        assert!(c.errors.is_empty(), "the same type twice is what two headers do");
        c.check_decl(other);

        assert_eq!(message(&c), "conflicting types for 'T'; have 'char'");
    }

    #[test]
    fn a_typedef_with_an_initializer_names_the_operator_that_was_wanted_instead() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Typedef);
        let one = f.int(1);
        let decl = f.var(specs, "T", &[], Some(one));

        let mut c = f.checker();
        c.check_decl(decl);

        assert_eq!(message(&c), "typedef 'T' is initialized (use '__typeof__' instead)");
    }

    #[test]
    fn an_object_of_a_type_with_no_size_is_refused_and_a_local_void_is_worded_apart() {
        let mut f = Fixture::new();
        let incomplete = f.record(Some("S"), false);
        let hidden = f.object(incomplete, "s");
        let void = f.builtin(BuiltinSet::VOID);
        let nothing = f.object(void, "x");

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(hidden);
        c.check_decl(nothing);

        assert_eq!(
            messages(&c),
            ["storage size of 's' isn't known", "variable or field 'x' declared void"]
        );
    }

    #[test]
    fn an_array_with_no_bound_at_file_scope_waits_for_a_declaration_that_gives_one() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let decl = f.var(
            specs,
            "a",
            &[Derived::Array {
                size: ArraySize::Unspecified,
                quals: Quals::NONE,
                has_static: false,
            }],
            None,
        );

        let mut c = f.checker();
        let list = c.check_decl(decl);

        assert_eq!(
            dump(&c, only(&c, list)),
            "decl #0 a : int[] object external static tentative\n"
        );
        assert!(c.errors.is_empty(), "the end of the translation unit is what decides this one");
    }

    #[test]
    fn a_variable_length_array_may_be_automatic_and_may_not_outlive_the_block() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let n = f.use_name("n");
        let automatic = f.var(specs, "a", &[array(n)], None);
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Static);
        let n = f.use_name("n");
        let stored = f.var(specs, "b", &[array(n)], None);
        let n = f.name("n");

        let mut c = f.checker();
        c.scopes.push();
        let int = c.types.int(IntKind::Int);
        c.declare_object(n, int, Span::DUMMY);
        c.check_decl(automatic);
        assert!(c.errors.is_empty());
        c.check_decl(stored);

        assert_eq!(message(&c), "storage size of 'b' isn't constant");
    }

    #[test]
    fn auto_and_register_at_file_scope_are_each_refused_in_the_words_gcc_uses() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Auto);
        let automatic = f.object(specs, "x");
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Register);
        let in_a_register = f.object(specs, "y");

        let mut c = f.checker();
        c.check_decl(automatic);
        c.check_decl(in_a_register);

        assert_eq!(
            messages(&c),
            [
                "file-scope declaration of 'x' specifies 'auto'",
                "register name not specified for 'y'",
            ]
        );
    }

    #[test]
    fn a_function_takes_static_at_file_scope_and_no_storage_class_anywhere_else() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Static);
        let hidden = f.var(specs, "f", &[function()], None);
        let inner = f.var(specs, "g", &[function()], None);

        let mut c = f.checker();
        let list = c.check_decl(hidden);
        assert_eq!(dump(&c, only(&c, list)), "decl #0 f : int(void) function internal declared\n");
        assert!(c.errors.is_empty());
        c.scopes.push();
        c.check_decl(inner);

        assert_eq!(message(&c), "invalid storage class for function 'g'");
    }

    #[test]
    fn a_scalar_initializer_is_converted_to_the_type_of_the_object_it_initializes() {
        let mut f = Fixture::new();
        let specs = f.builtin(BuiltinSet::DOUBLE);
        let one = f.int(1);
        let decl = f.var(specs, "d", &[], Some(one));

        let mut c = f.checker();
        c.scopes.push();
        let list = c.check_decl(decl);

        assert_eq!(
            dump(&c, only(&c, list)),
            "decl #0 d : double object automatic defined\n  init\n    +0\n      \
             convert arithmetic : double\n        const 1 : int\n"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn an_initializer_of_the_wrong_kind_names_the_conversion_it_would_have_taken() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let one = f.int(1);
        let from_an_integer = f.var(specs, "p", &[pointer()], None.or(Some(one)));
        let specs = f.builtin(BuiltinSet::CHAR);
        let q = f.use_name("q");
        let from_a_pointer = f.var(specs, "r", &[pointer()], Some(q));
        let q = f.name("q");

        let mut c = f.checker();
        c.scopes.push();
        let int = c.types.int(IntKind::Int);
        let to_int = c.types.pointer(int);
        c.declare_object(q, to_int, Span::DUMMY);
        c.check_decl(from_an_integer);
        c.check_decl(from_a_pointer);

        assert_eq!(
            messages(&c),
            [
                "initialization of 'int *' from 'int' makes pointer from integer without a cast",
                "initialization of 'char *' from incompatible pointer type 'int *'",
            ]
        );
    }

    #[test]
    fn an_array_and_a_structure_each_refuse_a_value_as_an_initializer() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let two = f.int(2);
        let one = f.int(1);
        let an_array = f.var(specs, "a", &[array(two)], Some(one));
        let specs = f.record(Some("S"), true);
        let one = f.int(1);
        let a_record = f.var(specs, "s", &[], Some(one));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(an_array);
        c.check_decl(a_record);

        assert_eq!(messages(&c), ["invalid initializer", "invalid initializer"]);
    }

    #[test]
    fn extern_with_an_initializer_is_an_error_in_a_block_and_a_warning_at_file_scope() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Extern);
        let one = f.int(1);
        let outer = f.var(specs, "x", &[], Some(one));
        let one = f.int(1);
        let inner = f.var(specs, "y", &[], Some(one));

        let mut c = f.checker();
        c.check_decl(outer);
        c.scopes.push();
        c.check_decl(inner);

        assert_eq!(
            messages(&c),
            ["'x' initialized and declared 'extern'", "'y' has both 'extern' and initializer"]
        );
    }

    #[test]
    fn alignas_raises_the_alignment_and_refuses_to_lower_it_or_to_take_a_number_that_is_not_one() {
        let mut f = Fixture::new();
        let sixteen = f.int(16);
        let mut specs = f.int_specs();
        specs.align = Some(AlignSpec::Expr(sixteen));
        let raised = f.object(specs, "x");
        let one = f.int(1);
        let mut specs = f.int_specs();
        specs.align = Some(AlignSpec::Expr(one));
        let lowered = f.object(specs, "y");
        let three = f.int(3);
        let mut specs = f.int_specs();
        specs.align = Some(AlignSpec::Expr(three));
        let odd = f.object(specs, "z");

        let mut c = f.checker();
        let list = c.check_decl(raised);
        assert_eq!(
            dump(&c, only(&c, list)),
            "decl #0 x : int object external static tentative alignas 16\n"
        );
        c.check_decl(lowered);
        c.check_decl(odd);

        assert_eq!(
            messages(&c),
            [
                "'_Alignas' specifiers cannot reduce alignment of 'y'",
                "requested alignment '3' is not a positive power of 2",
            ]
        );
    }

    #[test]
    fn alignment_asked_for_on_a_typedef_and_on_a_function_is_refused_on_each() {
        let mut f = Fixture::new();
        let sixteen = f.int(16);
        let mut specs = f.int_specs();
        specs.align = Some(AlignSpec::Expr(sixteen));
        specs.storage = Some(StorageClass::Typedef);
        let named = f.object(specs, "T");
        let mut specs = f.int_specs();
        specs.align = Some(AlignSpec::Expr(sixteen));
        let called = f.var(specs, "g", &[function()], None);

        let mut c = f.checker();
        c.check_decl(named);
        c.check_decl(called);

        assert_eq!(
            messages(&c),
            ["alignment specified for typedef 'T'", "alignment specified for function 'g'"]
        );
    }

    #[test]
    fn a_static_assertion_that_holds_says_nothing_and_one_that_fails_quotes_its_message() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let holds = f.ast.decl(ast::Decl::StaticAssert { cond: one, message: None }, Span::DUMMY);
        let zero = f.int(0);
        let boom = f.string("boom");
        let fails =
            f.ast.decl(ast::Decl::StaticAssert { cond: zero, message: Some(boom) }, Span::DUMMY);

        let mut c = f.checker();
        c.check_decl(holds);
        assert!(c.errors.is_empty());
        c.check_decl(fails);

        assert_eq!(message(&c), "static assertion failed: \"boom\"");
    }

    #[test]
    fn an_empty_declaration_says_what_about_it_was_useless() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let a_type_name = f.bare(specs);
        let mut specs = f.record(Some("S"), false);
        specs.quals = Quals::CONST;
        let a_qualifier = f.bare(specs);
        let unnamed = f.record(None, true);
        let no_instances = f.bare(unnamed);

        let mut c = f.checker();
        c.check_decl(a_type_name);
        c.check_decl(a_qualifier);
        c.check_decl(no_instances);

        assert_eq!(
            messages(&c),
            [
                "useless type name in empty declaration",
                "useless type qualifier in empty declaration",
                "unnamed struct/union that defines no instances",
            ]
        );
    }

    /// The `;` a macro leaves behind. Every project has one, so it has to cost nothing: the
    /// parser gives it the one pedantic warning gcc gives it and nothing here adds to that.
    #[test]
    fn a_semicolon_on_its_own_is_a_declaration_of_nothing_and_says_nothing() {
        let mut f = Fixture::new();
        let nothing = f.bare(DeclSpecs::empty(Span::DUMMY));

        let mut c = f.checker();
        c.check_decl(nothing);

        assert!(messages(&c).is_empty(), "{:?}", messages(&c));
    }

    /// A specifier written with no declarator after it declares nothing, so what is worth
    /// saying is which specifier it was. gcc has a separate sentence for each and these are
    /// its words, since a program that hits one is usually a macro that expanded oddly and the
    /// reader is going to search for the message.
    #[test]
    fn a_specifier_with_nothing_to_apply_to_is_named_in_the_message() {
        let mut f = Fixture::new();
        let mut specs = DeclSpecs::empty(Span::DUMMY);
        specs.storage = Some(StorageClass::Extern);
        let storage = f.bare(specs);
        let mut specs = DeclSpecs::empty(Span::DUMMY);
        specs.thread_local = true;
        let thread = f.bare(specs);
        let mut specs = DeclSpecs::empty(Span::DUMMY);
        specs.quals = Quals::CONST;
        let qualifier = f.bare(specs);

        let mut c = f.checker();
        c.check_decl(storage);
        c.check_decl(thread);
        c.check_decl(qualifier);

        assert_eq!(
            messages(&c),
            [
                "useless storage class specifier in empty declaration",
                "empty declaration",
                "useless `_Thread_local` in empty declaration",
                "empty declaration",
                "useless type qualifier in empty declaration",
                "empty declaration",
            ]
        );
        assert!(severities(&c).iter().all(|s| *s == Severity::Warning), "all six are warnings");
    }

    /// The four that are errors rather than warnings. `inline` and `_Noreturn` have no meaning
    /// at all away from a function, `auto` and `register` name a storage duration that file
    /// scope does not have, and a deduced type has nothing to deduce from.
    #[test]
    fn the_specifiers_that_cannot_be_ignored_in_an_empty_declaration_are_errors() {
        let mut f = Fixture::new();
        let mut specs = DeclSpecs::empty(Span::DUMMY);
        specs.func = FuncSpecs::INLINE;
        let inline = f.bare(specs);
        let mut specs = DeclSpecs::empty(Span::DUMMY);
        specs.func = FuncSpecs::NORETURN;
        let noreturn = f.bare(specs);
        let mut specs = DeclSpecs::empty(Span::DUMMY);
        specs.storage = Some(StorageClass::Register);
        let register = f.bare(specs);
        let specs = f.deduced(ast::Deduction::Auto);
        let deduced = f.bare(specs);

        let mut c = f.checker();
        c.check_decl(inline);
        c.check_decl(noreturn);
        c.check_decl(register);
        c.check_decl(deduced);

        assert_eq!(
            messages(&c),
            [
                "`inline` in empty declaration",
                "`_Noreturn` in empty declaration",
                "`register` in file-scope empty declaration",
                "`auto` in empty declaration",
            ]
        );
        assert!(severities(&c).iter().all(|s| *s == Severity::Error), "all four are errors");
    }

    #[test]
    fn a_specifier_that_only_a_function_takes_is_warned_about_on_a_variable() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.func = FuncSpecs::INLINE;
        let decl = f.object(specs, "x");

        let mut c = f.checker();
        c.check_decl(decl);

        assert_eq!(message(&c), "variable 'x' declared 'inline'");
    }

    #[test]
    fn thread_local_in_a_block_needs_a_storage_class_that_gives_it_somewhere_to_live() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.thread_local = true;
        let alone = f.object(specs, "x");
        let mut specs = f.int_specs();
        specs.thread_local = true;
        specs.storage = Some(StorageClass::Static);
        let stored = f.object(specs, "y");

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(alone);
        assert_eq!(message(&c), "function-scope 'x' implicitly auto and declared '_Thread_local'");
        let list = c.check_decl(stored);

        assert_eq!(dump(&c, only(&c, list)), "decl #1 y : int object thread defined\n");
    }

    #[test]
    fn a_function_definition_is_a_declaration_with_its_body_under_it() {
        let mut f = Fixture::new();
        let specs = f.builtin(BuiltinSet::VOID);
        let body = f.block(&[]);
        let decl = f.define(specs, "f", &[function()], body);

        let mut c = f.checker();
        let list = c.check_decl(decl);

        let id = only(&c, list);
        assert_eq!(
            dump(&c, id),
            "decl #0 f : void(void) function external defined\n  body\n    block\n"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_parameter_is_declared_once_and_the_body_names_that_declaration() {
        let mut f = Fixture::new();
        let int = f.int_specs();
        let n = f.param(int, Some("n"), &[]);
        let takes = f.takes(&[n]);
        let use_n = f.use_name("n");
        let ret = f.stmt(ast::Stmt::Return(Some(use_n)));
        let body = f.block(&[ret]);
        let specs = f.int_specs();
        let decl = f.define(specs, "f", &[takes], body);

        let mut c = f.checker();
        let list = c.check_decl(decl);

        let id = only(&c, list);
        assert_eq!(
            dump(&c, id),
            "decl #1 f : int(int) function external defined\n  params\n    decl #0 n : int \
             object automatic defined\n  body\n    block\n      return\n        convert lvalue \
             : int\n          decl #0 n : int lvalue\n"
        );
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn a_parameter_a_definition_left_unnamed_is_still_one_of_the_parameters() {
        let mut f = Fixture::new();
        let int = f.int_specs();
        let a = f.param(int, Some("a"), &[]);
        let int = f.int_specs();
        let anonymous = f.param(int, None, &[]);
        let takes = f.takes(&[a, anonymous]);
        let use_a = f.use_name("a");
        let ret = f.stmt(ast::Stmt::Return(Some(use_a)));
        let body = f.block(&[ret]);
        let specs = f.int_specs();
        let decl = f.define(specs, "f", &[takes], body);

        let mut c = f.checker();
        let list = c.check_decl(decl);

        // C23 6.7.7.4p1 lets the name be left out, and the object is passed either way. The list
        // is what says what the function takes and in what order, so leaving it out of the list
        // would say the function takes one thing when its type says two.
        let id = only(&c, list);
        assert_eq!(
            dump(&c, id),
            "decl #2 f : int(int, int) function external defined\n  params\n    decl #0 a : int \
             object automatic defined\n    decl #1 : int object automatic defined\n  body\n    \
             block\n      return\n        convert lvalue : int\n          decl #0 a : int \
             lvalue\n"
        );
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn a_name_declared_in_the_body_meets_the_parameter_of_the_same_name() {
        let mut f = Fixture::new();
        let int = f.int_specs();
        let a = f.param(int, Some("a"), &[]);
        let takes = f.takes(&[a]);
        let specs = f.int_specs();
        let shadow = f.object(specs, "a");
        let shadow = f.stmt(ast::Stmt::Decl(shadow));
        let body = f.block(&[shadow]);
        let specs = f.builtin(BuiltinSet::VOID);
        let decl = f.define(specs, "f", &[takes], body);

        let mut c = f.checker();
        c.check_decl(decl);

        assert_eq!(
            messages(&c),
            ["redeclaration of 'a' with no linkage", "previous definition of 'a' with type 'int'"]
        );
    }

    #[test]
    fn a_block_inside_the_body_may_shadow_a_parameter() {
        let mut f = Fixture::new();
        let int = f.int_specs();
        let a = f.param(int, Some("a"), &[]);
        let takes = f.takes(&[a]);
        let specs = f.int_specs();
        let shadow = f.object(specs, "a");
        let shadow = f.stmt(ast::Stmt::Decl(shadow));
        let inner = f.block(&[shadow]);
        let body = f.block(&[inner]);
        let specs = f.builtin(BuiltinSet::VOID);
        let decl = f.define(specs, "f", &[takes], body);

        let mut c = f.checker();
        c.check_decl(decl);

        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn an_array_parameter_is_a_pointer_in_the_body_as_well_as_in_the_type() {
        let mut f = Fixture::new();
        let int = f.int_specs();
        let three = f.int(3);
        let a = f.param(int, Some("a"), &[array(three)]);
        let takes = f.takes(&[a]);
        let use_a = f.use_name("a");
        let stmt = f.stmt(ast::Stmt::Expr(use_a));
        let body = f.block(&[stmt]);
        let specs = f.builtin(BuiltinSet::VOID);
        let decl = f.define(specs, "f", &[takes], body);

        let mut c = f.checker();
        let list = c.check_decl(decl);

        let id = only(&c, list);
        assert_eq!(
            dump(&c, id),
            "decl #1 f : void(int *) function external defined\n  params\n    decl #0 a : \
             int * object automatic defined\n  body\n    block\n      expr\n        convert \
             lvalue : int *\n          decl #0 a : int * lvalue\n"
        );
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn a_qualifier_on_a_parameter_is_the_object_s_and_not_the_function_type_s() {
        let mut f = Fixture::new();
        let mut int = f.int_specs();
        int.quals = Quals::CONST;
        let a = f.param(int, Some("a"), &[]);
        let takes = f.takes(&[a]);
        let use_a = f.use_name("a");
        let stmt = f.stmt(ast::Stmt::Expr(use_a));
        let body = f.block(&[stmt]);
        let specs = f.builtin(BuiltinSet::VOID);
        let decl = f.define(specs, "f", &[takes], body);

        let mut c = f.checker();
        let list = c.check_decl(decl);

        // The type says `int` and the object says `const int`, which is the whole of the rule:
        // a caller is told nothing by the `const` and the body is bound by it.
        let id = only(&c, list);
        assert_eq!(
            dump(&c, id),
            "decl #1 f : void(int) function external defined\n  params\n    decl #0 a : \
             const int object automatic defined\n  body\n    block\n      expr\n        \
             convert lvalue : int\n          decl #0 a : const int lvalue\n"
        );
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn the_body_answers_to_the_return_type_the_definition_was_written_with() {
        let mut f = Fixture::new();
        let ret = f.stmt(ast::Stmt::Return(None));
        let body = f.block(&[ret]);
        let specs = f.int_specs();
        let decl = f.define(specs, "f", &[function()], body);

        let mut c = f.checker();
        c.check_decl(decl);

        assert_eq!(
            messages(&c),
            ["'return' with no value, in function returning non-void", "declared here"]
        );
    }

    #[test]
    fn a_declaration_and_a_definition_of_one_function_are_one_declaration() {
        let mut f = Fixture::new();
        let specs = f.builtin(BuiltinSet::VOID);
        let declared = f.var(specs, "f", &[function()], None);
        let body = f.block(&[]);
        let specs = f.builtin(BuiltinSet::VOID);
        let defined = f.define(specs, "f", &[function()], body);

        let mut c = f.checker();
        let list = c.check_decl(declared);
        let first = only(&c, list);
        let list = c.check_decl(defined);

        assert_eq!(only(&c, list), first);
        assert_eq!(
            dump(&c, first),
            "decl #0 f : void(void) function external defined\n  body\n    block\n"
        );
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn a_function_definition_declared_typedef_is_an_error() {
        let mut f = Fixture::new();
        let mut specs = f.builtin(BuiltinSet::VOID);
        specs.storage = Some(StorageClass::Typedef);
        let body = f.block(&[]);
        let decl = f.define(specs, "f", &[function()], body);

        let mut c = f.checker();
        c.check_decl(decl);

        assert_eq!(message(&c), "function definition declared 'typedef'");
    }

    #[test]
    fn a_function_definition_inside_a_function_is_refused_and_the_name_still_declared() {
        let mut f = Fixture::new();
        let empty = f.block(&[]);
        let specs = f.builtin(BuiltinSet::VOID);
        let nested = f.define(specs, "g", &[function()], empty);
        let nested = f.stmt(ast::Stmt::Decl(nested));
        let call = f.use_name("g");
        let call = f.stmt(ast::Stmt::Expr(call));
        let body = f.block(&[nested, call]);
        let specs = f.builtin(BuiltinSet::VOID);
        let decl = f.define(specs, "f", &[function()], body);

        let mut c = f.checker();
        c.check_decl(decl);

        // The second message is the note, and it is here so that a rewrite of it that leaves the
        // continuation of a line in the text is a test failure rather than something a reader of
        // the output notices later.
        assert_eq!(
            messages(&c),
            [
                "a function definition inside a function",
                "a nested function is called through a trampoline written on the stack, which no \
                 target that enforces an unexecutable stack allows, so this compiler does not \
                 have them and will not"
            ],
            "the mention of 'g' under the definition has to resolve, so that one definition is \
             one error"
        );
    }

    /// `void f(a) char a; {}`, which takes its parameter type from the declaration under the
    /// identifier list and then promotes it, since a promoted type is what a caller of an
    /// unprototyped function hands over.
    #[test]
    fn an_old_style_definition_reads_the_declarations_written_under_its_list() {
        let mut f = Fixture::new();
        let int = f.int_specs();
        let a = f.param(int, Some("a"), &[]);
        let params = f.ast.add_param_list(&[a]);
        let old = Derived::Function { params, variadic: false, kind: ParamKind::Identifiers };
        let char_specs = f.builtin(BuiltinSet::CHAR);
        let written = f.object(char_specs, "a");
        let body = f.block(&[]);
        let specs = f.builtin(BuiltinSet::VOID);
        let decl = f.define_taking(specs, "f", &[old], &[written], body);

        let mut c = f.checker();
        let list = c.check_decl(decl);

        assert_eq!(messages(&c), Vec::<String>::new());
        let text = dump(&c, only(&c, list));
        assert!(text.contains("f : void(int) function"), "{text}");
        // And the body sees the `char` it was declared as, whatever the caller hands over.
        assert!(text.contains("a : char object automatic defined"), "{text}");
    }

    /// The same definition with nothing declaring `a`. C89 made it an `int` and every dialect
    /// after it made the line a diagnostic, and the fixture is C23.
    #[test]
    fn a_name_in_an_identifier_list_that_nothing_declares_says_so() {
        let mut f = Fixture::new();
        let int = f.int_specs();
        let a = f.param(int, Some("a"), &[]);
        let params = f.ast.add_param_list(&[a]);
        let old = Derived::Function { params, variadic: false, kind: ParamKind::Identifiers };
        let body = f.block(&[]);
        let specs = f.builtin(BuiltinSet::VOID);
        let decl = f.define(specs, "f", &[old], body);

        let mut c = f.checker();
        c.check_decl(decl);

        assert_eq!(message(&c), "type of 'a' defaults to 'int'");
    }

    #[test]
    fn a_deduced_type_is_the_type_the_initializer_would_have_where_it_is_used() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let decl = f.var(f.deduced(ast::Deduction::Auto), "x", &[], Some(one));

        let mut c = f.checker();
        let list = c.check_decl(decl);

        let id = only(&c, list);
        assert_eq!(
            dump(&c, id),
            "decl #0 x : int object external static defined\n  init\n    +0\n      const 1 : int\n"
        );
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn a_deduced_type_is_the_one_a_use_has_so_an_array_deduces_a_pointer() {
        let mut f = Fixture::new();
        let three = f.int(3);
        let ints = f.int_specs();
        let array = f.var(ints, "a", &[array(three)], None);
        let a = f.use_name("a");
        let decl = f.var(f.deduced(ast::Deduction::AutoType), "p", &[], Some(a));

        let mut c = f.checker();
        c.check_decl(array);
        c.scopes.push();
        let list = c.check_decl(decl);

        let id = only(&c, list);
        assert!(dump(&c, id).starts_with("decl #1 p : int *"), "{}", dump(&c, id));
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn a_deduced_type_drops_the_initializers_qualifiers_and_takes_the_declarations() {
        let mut f = Fixture::new();
        let mut ints = f.int_specs();
        ints.quals = Quals::CONST;
        let one = f.int(1);
        let source = f.var(ints, "c", &[], Some(one));
        // What is put into the new object is a value, and a value is not `const`.
        let c1 = f.use_name("c");
        let plain = f.var(f.deduced(ast::Deduction::Auto), "x", &[], Some(c1));
        let c2 = f.use_name("c");
        let mut qualified = f.deduced(ast::Deduction::Auto);
        qualified.quals = Quals::CONST;
        let kept = f.var(qualified, "y", &[], Some(c2));

        let mut c = f.checker();
        c.check_decl(source);
        c.scopes.push();
        let list = c.check_decl(plain);
        let plain = only(&c, list);
        let list = c.check_decl(kept);
        let kept = only(&c, list);

        assert!(dump(&c, plain).starts_with("decl #1 x : int "), "{}", dump(&c, plain));
        assert!(dump(&c, kept).starts_with("decl #2 y : const int "), "{}", dump(&c, kept));
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn a_deduced_type_needs_a_declarator_that_is_no_more_than_a_name() {
        let mut f = Fixture::new();
        // The deduction is the whole type, so there is nothing left for a `*` to add to it.
        let one = f.int(1);
        let c23 = f.var(f.deduced(ast::Deduction::Auto), "p", &[pointer()], Some(one));
        let two = f.int(2);
        let gnu = f.var(f.deduced(ast::Deduction::AutoType), "q", &[pointer()], Some(two));

        let mut c = f.checker();
        c.check_decl(c23);
        c.check_decl(gnu);

        // gcc words the two differently, since only C23's takes the attributes it mentions.
        assert_eq!(
            messages(&c),
            [
                "'auto' requires a plain identifier, possibly with attributes, as declarator",
                "'__auto_type' requires a plain identifier as declarator",
            ]
        );
    }

    #[test]
    fn a_deduced_type_needs_something_to_deduce_from() {
        let mut f = Fixture::new();
        let decl = f.var(f.deduced(ast::Deduction::AutoType), "x", &[], None);

        let mut c = f.checker();
        c.check_decl(decl);

        assert_eq!(message(&c), "'__auto_type' requires an initialized data declaration");
    }

    #[test]
    fn one_initializer_deduces_one_type_so_a_second_declarator_is_refused() {
        let mut f = Fixture::new();
        // Said once and about the declaration, and the first declarator is still checked so
        // that its name means something for the rest of the unit.
        let one = f.int(1);
        let two = f.int(2);
        let x = f.declarator("x", &[]);
        let y = f.declarator("y", &[]);
        let items: Vec<ast::InitDeclarator> = [(x, one), (y, two)]
            .into_iter()
            .map(|(declarator, value)| ast::InitDeclarator {
                declarator,
                init: Some(f.ast.add_init(ast::Init::Expr(value))),
                asm_label: None,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            })
            .collect();
        let declarators = f.ast.add_init_declarator_list(&items);
        let specs = f.specs(f.deduced(ast::Deduction::Auto));
        let decl = f.ast.decl(ast::Decl::Var { specs, declarators }, Span::DUMMY);

        let mut c = f.checker();
        let list = c.check_decl(decl);

        let id = only(&c, list);
        assert_eq!(
            dump(&c, id),
            "decl #0 x : int object external static defined\n  init\n    +0\n      const 1 : int\n"
        );
        assert_eq!(message(&c), "'auto' may only be used with a single declarator");
    }

    #[test]
    fn a_function_definition_deduces_nothing_because_it_has_no_initializer() {
        let mut f = Fixture::new();
        let body = f.block(&[]);
        let decl = f.define(f.deduced(ast::Deduction::Auto), "f", &[function()], body);

        let mut c = f.checker();
        let list = c.check_decl(decl);

        assert!(c.tast[list].is_empty());
        assert_eq!(
            message(&c),
            "'auto' requires a plain identifier, possibly with attributes, as declarator"
        );
    }

    #[test]
    fn a_name_with_no_type_until_its_initializer_is_checked_may_not_be_used_in_it() {
        let mut f = Fixture::new();
        // The name is in scope inside its own initializer, which is what makes this a
        // reference to report rather than a use of an undeclared name.
        let x = f.use_name("x");
        let deduced = f.var(f.deduced(ast::Deduction::Auto), "x", &[], Some(x));
        let y = f.use_name("y");
        let mut ints = f.int_specs();
        ints.constexpr = true;
        let constant = f.var(ints, "y", &[], Some(y));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(deduced);
        c.check_decl(constant);

        // A `constexpr` has a type before its initializer and no value until after it, which
        // C23 calls underspecified for the same reason and gcc reports the same way.
        assert_eq!(
            messages(&c),
            [
                "underspecified 'x' referenced in its initializer",
                "underspecified 'y' referenced in its initializer",
            ]
        );
    }
}
