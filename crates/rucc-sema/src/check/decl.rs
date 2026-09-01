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

use rucc_ast::{self as ast, AlignSpec, FuncSpecs, StorageClass};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{ArrayLen, TypeId, TypeKind};
use rucc_types::{compatible, composite, is_complete, is_function, is_void, layout};

use crate::check::Checker;
use crate::decl::{
    Decl, DeclId, DeclKind, DeclList, Definition, InitList, Linkage, StorageDuration,
};
use crate::scope::Binding;

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
    /// What `alignas` asked for, once it has been folded and checked.
    alignment: Option<u32>,
    /// Whether `extern` was written, which is not the same as having external linkage. A file
    /// scope `int x;` has external linkage and no keyword, and the difference between the two is
    /// what makes `static int x; extern int x;` legal and `static int x; int x;` not.
    is_extern: bool,
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
        let items = self.ast[declarators].to_vec();
        if items.is_empty() {
            self.empty_declaration(specs);
            return self.tast.add_decl_refs(&[]);
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
        // An old-style definition takes the types of its parameters from the declarations between
        // the parenthesis and the body, which is a second way to declare a parameter and not
        // something the type builder was asked to do. Nothing in rung 0 is written this way.
        if kind == ast::ParamKind::Identifiers && !(params.is_empty() && declarations.is_empty()) {
            self.declaration_unsupported("an old-style function definition", span);
            return None;
        }
        let ty = self.declared_type(specs, declarator);
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
        let declared = Declared {
            name,
            ty,
            kind: DeclKind::Function,
            linkage,
            duration,
            state: Definition::Defined,
            alignment,
            is_extern: specs.storage == Some(StorageClass::Extern),
            span,
        };
        let id = self.merge(declared);
        let stmt = self.function_body(ty, span, params, body);
        let mut node = self.tast[id].clone();
        node.body = Some(stmt);
        self.tast.set_decl(id, node);
        Some(id)
    }

    /// The body of a function definition, in a scope holding its parameters.
    ///
    /// One scope and not two. C 6.2.1p4 puts the parameters in the block scope of the body, which
    /// is why `void f(int a) { int a; }` is a redeclaration and `void f(int a) { { int a; } }` is
    /// not, so the body's own compound statement is walked here rather than through the statement
    /// that would open a scope of its own.
    fn function_body(
        &mut self,
        ty: TypeId,
        span: Span,
        params: ast::ParamList,
        body: ast::StmtId,
    ) -> crate::stmt::StmtId {
        let ret = match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Function(signature) => self.types.signature(signature).ret,
            // A definition of something that is not a function has been reported by the merge,
            // and checking the body against `int` is what keeps the rest of it worth reading.
            _ => self.int(),
        };
        self.scopes.push();
        for decl in self.prototype_params(params) {
            if let Some(name) = self.tast[decl].name {
                self.scopes.declare(name, Binding::Decl(decl));
            }
        }
        let previous = self.open_body(ret, span);
        let stmt = self.body_block(body);
        self.close_body(previous);
        self.scopes.pop();
        stmt
    }

    /// A declaration with no declarator, which declares a tag or nothing at all.
    ///
    /// The type is built either way, because `struct S { int x; };` is how every structure in
    /// every header is declared and the body is where the members are checked. What is diagnosed
    /// is the case where a type was named and there was nothing for it to be the type of.
    fn empty_declaration(&mut self, specs: ast::DeclSpecsId) {
        self.declared_specs(specs);
        let node = self.ast[specs];
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

    /// One declarator of a declaration, with whatever initializer it was given.
    fn init_declarator(
        &mut self,
        specs: ast::DeclSpecsId,
        item: ast::InitDeclarator,
    ) -> Option<DeclId> {
        let ty = self.declared_type(specs, item.declarator);
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
        let mut declared = Declared {
            name,
            ty,
            kind,
            linkage,
            duration,
            state,
            alignment,
            is_extern: specs.storage == Some(StorageClass::Extern),
            span,
        };
        let id = self.merge(declared);
        // An initializer that did not work out leaves the object without a size, and saying so
        // a second time helps nobody, so what it did decides whether the size is asked about.
        let mut worked = true;
        if let Some(init) = item.init {
            let constant = specs.storage == Some(StorageClass::Constexpr);
            match self.initializer(id, init, constant, span) {
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
                Some(StorageClass::Auto | StorageClass::Register | StorageClass::Constexpr) => true,
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
                Some(StorageClass::Static | StorageClass::Constexpr) => Linkage::Internal,
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
        if specs.storage == Some(StorageClass::Constexpr) && !has_init {
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
            None if declared.linkage != Linkage::None => self.scopes.lookup(declared.name),
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
            linkage: if declared.is_extern { node.linkage } else { declared.linkage },
            state: stronger(node.state, declared.state),
            alignment: node.alignment.max(declared.alignment),
            ..node
        };
        self.tast.set_decl(previous, merged);
        self.scopes.declare(declared.name, Binding::Decl(previous));
        previous
    }

    /// Whether the two declarations agree about who can see the name.
    fn check_linkage(&mut self, node: &Decl, declared: &Declared, previous: DeclId) -> bool {
        let spelled = self.text(declared.name).to_owned();
        let message = match (node.linkage, declared.linkage) {
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
            (Linkage::Internal, Linkage::External) if !declared.is_extern => {
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
            init: None,
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
        self.init_object(ty, name, is_static, init, constant, span)
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
    fn is_unsized_array(&self, ty: TypeId) -> bool {
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
    use rucc_diag::Span;
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
            specs.ty =
                TypeSpec::Record { kind: RecordKind::Struct, tag, fields, attrs: AttrList::EMPTY };
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
            let declarator = self.declarator(name, derived);
            let specs = self.specs(specs);
            let params = self.ast.add_decl_list(&[]);
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

        assert_eq!(dump(&c, id), "decl #0 a : int [3] object external static tentative\n");
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
            "decl #0 a : int [] object external static tentative\n"
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
        assert_eq!(dump(&c, only(&c, list)), "decl #0 f : int (void) function internal declared\n");
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
            "decl #0 f : void (void) function external defined\n  body\n    block\n"
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
            "decl #1 f : int (int) function external defined\n  body\n    block\n      return\n \
             \x20      convert lvalue : int\n          decl #0 n : int lvalue\n"
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
            "decl #1 f : void (int *) function external defined\n  body\n    block\n      expr\n \
             \x20      convert lvalue : int *\n          decl #0 a : int * lvalue\n"
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
            "decl #0 f : void (void) function external defined\n  body\n    block\n"
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
    fn an_old_style_definition_is_recognised_and_not_checked_yet() {
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

        assert_eq!(message(&c), "an old-style function definition is not supported yet");
    }
}
