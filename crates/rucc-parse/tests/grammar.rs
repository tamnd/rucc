//! The grammar, tested through the public entry point on real source text.
//!
//! The source goes through the lexer and phase 7 rather than being written as tokens by hand,
//! because a token stream written by hand is a token stream that agrees with the parser's
//! assumptions rather than with the lexer's output. That is the bug this catches and the reason
//! these are integration tests: they see what a driver sees.

use rucc_ast::{
    ArraySize, BinaryOp, BuiltinSet, Decl, Deduction, Derived, Expr, ForInit, Init, Member,
    ParamKind, Stmt, StorageClass, TypeSpec, UnaryOp,
};
use rucc_base::Interner;
use rucc_lex::{Convert, Keywords, Options, convert, tokenize};
use rucc_parse::{Context, Parsed, parse};
use rucc_session::Std;
use rucc_target::{TargetInfo, Triple};

/// Everything a parse needs, built the way a driver builds it.
struct Fixture {
    interner: Interner,
    keywords: Keywords,
    target: TargetInfo,
    std: Std,
}

impl Fixture {
    fn new(std: Std) -> Fixture {
        // The keyword table is interned before any source is read, which `Keywords::new`
        // insists on.
        let mut interner = Interner::new();
        let keywords = Keywords::new(&mut interner, std, true);
        let target =
            TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
        Fixture { interner, keywords, target, std }
    }

    fn parse(&mut self, src: &str) -> Parsed {
        let (pp, diagnostics) = tokenize(src.as_bytes(), 0, Options::new(), &mut self.interner);
        assert!(diagnostics.is_empty(), "the scanner disliked the source: {src}");
        let cx = Convert {
            keywords: &self.keywords,
            interner: &self.interner,
            target: &self.target,
            std: self.std,
            gnu: false,
            pedantic: false,
        };
        let (tokens, diagnostics) = convert(&pp, &cx);
        assert!(diagnostics.is_empty(), "phase 7 disliked the source: {src}");
        parse(&tokens, Context::new(&self.interner, self.std))
    }
}

/// Parses `src` as C23 with the GNU extensions on, and insists that it parsed.
fn parsed(src: &str) -> Parsed {
    let out = Fixture::new(Std::C23).parse(src);
    let complaints: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(!out.failed(), "expected {src} to parse, got {complaints:?}");
    out
}

/// What the parse of `src` complained about.
fn complaints(src: &str) -> Vec<String> {
    let out = Fixture::new(Std::C23).parse(src);
    out.diagnostics.iter().map(|d| d.message.clone()).collect()
}

/// The one declaration of a unit that has one.
fn only_decl(out: &Parsed) -> Decl {
    let top = out.ast.top_level();
    assert_eq!(top.len(), 1, "expected one declaration");
    out.ast[top[0]]
}

/// The statements of a function's body.
fn body_of(out: &Parsed, decl: Decl) -> Vec<Stmt> {
    let Decl::Function { body, .. } = decl else { panic!("expected a function definition") };
    let Stmt::Compound(items) = out.ast[body] else { panic!("expected a block") };
    out.ast[items].iter().map(|&id| out.ast[id]).collect()
}

/// The expression of `int f(void) { return EXPR; }`.
fn returned(out: &Parsed) -> Expr {
    let decl = only_decl(out);
    let stmts = body_of(out, decl);
    let Stmt::Return(Some(value)) = stmts[0] else { panic!("expected a return") };
    out.ast[value]
}

/// The expression of `int f(void) { return EXPR; }` for the source `EXPR`.
fn expression(src: &str) -> Expr {
    let out = parsed(&format!("int f(void) {{ return {src}; }}"));
    returned(&out)
}

#[test]
fn a_translation_unit_is_a_list_of_declarations() {
    let out = parsed("int x; int y = 1; void f(void) {}");
    assert_eq!(out.ast.top_level().len(), 3);
    assert!(matches!(out.ast[out.ast.top_level()[0]], Decl::Var { .. }));
    assert!(matches!(out.ast[out.ast.top_level()[2]], Decl::Function { .. }));
}

#[test]
fn one_declaration_holds_all_of_its_declarators() {
    let out = parsed("int a, *b = 0, c[4];");
    let Decl::Var { declarators, .. } = only_decl(&out) else { panic!("expected a declaration") };
    assert_eq!(declarators.len(), 3);
    let second = out.ast[declarators][1];
    assert!(second.init.is_some());
    assert!(out.ast[declarators][0].init.is_none());
}

#[test]
fn a_declarator_reads_outward_from_the_name() {
    // Array of three pointers to functions taking char and returning int.
    let out = parsed("int (*f[3])(char);");
    let Decl::Var { declarators, .. } = only_decl(&out) else { panic!("expected a declaration") };
    let declarator = out.ast[out.ast[declarators][0].declarator];
    let derived: Vec<Derived> = out.ast[declarator.derived].to_vec();
    assert!(matches!(derived[0], Derived::Array { .. }), "{derived:?}");
    assert!(matches!(derived[1], Derived::Pointer { .. }), "{derived:?}");
    assert!(matches!(derived[2], Derived::Function { .. }), "{derived:?}");
    assert_eq!(derived.len(), 3);
}

#[test]
fn an_empty_parameter_list_is_not_a_void_one() {
    let out = parsed("int f(); int g(void); int h(int, ...);");
    let kinds: Vec<ParamKind> = out
        .ast
        .top_level()
        .iter()
        .map(|&id| {
            let Decl::Var { declarators, .. } = out.ast[id] else {
                panic!("expected a declaration")
            };
            let declarator = out.ast[out.ast[declarators][0].declarator];
            let Derived::Function { kind, .. } = out.ast[declarator.derived][0] else {
                panic!("expected a function declarator")
            };
            kind
        })
        .collect();
    assert_eq!(kinds, vec![ParamKind::Empty, ParamKind::Void, ParamKind::Prototype]);
}

#[test]
fn a_typedef_name_is_a_type_and_then_a_variable() {
    // The declarator ends before the name is declared, so the second `T` is the variable.
    let out = parsed("typedef int T; T T;");
    let second = out.ast.top_level()[1];
    let Decl::Var { specs, declarators } = out.ast[second] else { panic!("expected one") };
    assert!(matches!(out.ast[specs].ty, TypeSpec::Typedef(_)));
    assert_eq!(declarators.len(), 1);
    assert!(out.ast[out.ast[declarators][0].declarator].name.is_some());
}

#[test]
fn a_parameter_takes_the_name_away_from_the_typedef() {
    // `T x` is not a declaration here, because the first parameter renamed `T`.
    let complaints = complaints("typedef int T; void f(int T, T x);");
    assert!(!complaints.is_empty(), "expected the second parameter to be rejected");
}

#[test]
fn the_typedef_decides_the_cast_from_the_multiplication() {
    let cast = expression("(A)*b");
    assert!(matches!(cast, Expr::Binary { op: BinaryOp::Mul, .. }), "{cast:?}");

    let out = parsed("typedef int A; int f(void) { return (A)*b; }");
    let stmts = body_of(&out, out.ast[out.ast.top_level()[1]]);
    let Stmt::Return(Some(value)) = stmts[0] else { panic!("expected a return") };
    assert!(matches!(out.ast[value], Expr::Cast { .. }), "{:?}", out.ast[value]);
}

#[test]
fn the_binding_powers_are_the_ones_c_has() {
    // Assignment binds loosest, so the whole sum is its right operand, and the product binds
    // tighter than the sum, so the sum is what is on top of it.
    let out = parsed("int f(void) { return a = b + c * d; }");
    let Expr::Assign { op: None, rhs, .. } = returned(&out) else { panic!("expected `=`") };
    let Expr::Binary { op: BinaryOp::Add, rhs, .. } = out.ast[rhs] else { panic!("expected `+`") };
    assert!(matches!(out.ast[rhs], Expr::Binary { op: BinaryOp::Mul, .. }));

    // The same tree either way round, which is what says the shape came from the operators and
    // not from the order they were written in.
    let out = parsed("int f(void) { return a * b + c; }");
    assert!(matches!(returned(&out), Expr::Binary { op: BinaryOp::Add, .. }));

    assert!(matches!(expression("(a, b)"), Expr::Comma { .. }));
}

#[test]
fn assignment_is_right_associative_and_comparison_is_not() {
    let out = parsed("int f(void) { return a = b = c; }");
    let Expr::Assign { rhs, .. } = returned(&out) else { panic!("expected an assignment") };
    assert!(matches!(out.ast[rhs], Expr::Assign { .. }), "{:?}", out.ast[rhs]);

    let out = parsed("int f(void) { return a < b < c; }");
    let Expr::Binary { op: BinaryOp::Lt, lhs, .. } = returned(&out) else {
        panic!("expected a comparison")
    };
    assert!(matches!(out.ast[lhs], Expr::Binary { op: BinaryOp::Lt, .. }));
}

#[test]
fn the_conditional_middle_may_be_left_out() {
    let full = expression("a ? b : c");
    let Expr::Cond { then, .. } = full else { panic!("expected a conditional") };
    assert!(then.is_some());

    let gnu = expression("a ?: c");
    let Expr::Cond { then, .. } = gnu else { panic!("expected a conditional") };
    assert!(then.is_none());
}

#[test]
fn sizeof_of_a_type_is_not_sizeof_of_an_expression() {
    assert!(matches!(expression("sizeof(int)"), Expr::SizeofType(_)));
    assert!(matches!(expression("sizeof x"), Expr::SizeofExpr(_)));
    assert!(matches!(expression("sizeof(x)"), Expr::SizeofExpr(_)));
    // A braced initializer after the type makes it a compound literal, not a type name.
    assert!(matches!(expression("sizeof(int){0}"), Expr::SizeofExpr(_)));
    assert!(matches!(expression("_Alignof(int)"), Expr::AlignofType(_)));
}

#[test]
fn a_compound_literal_is_an_object_and_a_cast_is_a_conversion() {
    assert!(matches!(expression("(int){1}"), Expr::CompoundLiteral { .. }));
    assert!(matches!(expression("(int)1"), Expr::Cast { .. }));
}

#[test]
fn the_postfix_operators_chain() {
    let out = parsed("int f(void) { return a[1].b->c(2)++; }");
    let Expr::Unary { op: UnaryOp::PostInc, operand } = returned(&out) else {
        panic!("expected a postfix increment")
    };
    let Expr::Call { callee, args } = out.ast[operand] else { panic!("expected a call") };
    assert_eq!(args.len(), 1);
    let Expr::Member { arrow: true, base, .. } = out.ast[callee] else { panic!("expected `->`") };
    let Expr::Member { arrow: false, base, .. } = out.ast[base] else { panic!("expected `.`") };
    assert!(matches!(out.ast[base], Expr::Index { .. }));
}

#[test]
fn the_gnu_expression_extensions_are_here() {
    assert!(matches!(expression("({ 1; })"), Expr::StmtExpr(_)));
    assert!(matches!(expression("&&there"), Expr::LabelAddr(_)));
    assert!(matches!(expression("__builtin_offsetof(struct s, m)"), Expr::Offsetof { .. }));
    assert!(matches!(expression("__builtin_choose_expr(1, 2, 3)"), Expr::ChooseExpr { .. }));
    assert!(matches!(
        expression("__builtin_types_compatible_p(int, long)"),
        Expr::TypesCompatible { .. }
    ));
    assert!(matches!(expression("_Generic(1, int: 2, default: 3)"), Expr::Generic { .. }));
}

#[test]
fn the_variable_argument_family_is_syntax_and_not_four_calls() {
    assert!(matches!(expression("__builtin_va_arg(ap, int)"), Expr::VaArg { .. }));
    assert!(matches!(expression("__builtin_va_end(ap)"), Expr::VaEnd { .. }));
    assert!(matches!(expression("__builtin_va_copy(a, b)"), Expr::VaCopy { .. }));
    // The second argument of `va_start` is optional here and required later, so that a program
    // that leaves it out is told about the argument rather than about a parenthesis.
    let Expr::VaStart { last: Some(_), .. } = expression("__builtin_va_start(ap, n)") else {
        panic!("expected a second argument")
    };
    let Expr::VaStart { last: None, .. } = expression("__builtin_va_start(ap)") else {
        panic!("expected no second argument")
    };
}

#[test]
fn the_statements_are_all_here() {
    let out = parsed(
        "void f(int x) {
             ;
             if (x) x++; else --x;
             while (x) break;
             do x--; while (x);
             for (int i = 0; i < x; i++) continue;
             switch (x) { case 1: case 2 ... 4: break; default: break; }
             { int y = x; (void)y; }
             there: goto there;
             return;
         }",
    );
    let stmts = body_of(&out, only_decl(&out));
    assert!(matches!(stmts[0], Stmt::Empty));
    assert!(matches!(stmts[1], Stmt::If { otherwise: Some(_), .. }));
    assert!(matches!(stmts[2], Stmt::While { .. }));
    assert!(matches!(stmts[3], Stmt::DoWhile { .. }));
    assert!(matches!(stmts[4], Stmt::For { init: ForInit::Decl(_), .. }));
    assert!(matches!(stmts[5], Stmt::Switch { .. }));
    assert!(matches!(stmts[6], Stmt::Compound(_)));
    assert!(matches!(stmts[7], Stmt::Label { .. }));
    assert!(matches!(stmts[8], Stmt::Return(None)));
}

#[test]
fn an_else_binds_to_the_nearest_if() {
    let out = parsed("void f(int a, int b) { if (a) if (b) b++; else a++; }");
    let stmts = body_of(&out, only_decl(&out));
    let Stmt::If { then, otherwise: None, .. } = stmts[0] else { panic!("expected an outer if") };
    assert!(matches!(out.ast[then], Stmt::If { otherwise: Some(_), .. }));
}

#[test]
fn a_case_range_keeps_both_ends() {
    let out = parsed("void f(int x) { switch (x) { case 1 ... 9: break; } }");
    let stmts = body_of(&out, only_decl(&out));
    let Stmt::Switch { body, .. } = stmts[0] else { panic!("expected a switch") };
    let Stmt::Compound(items) = out.ast[body] else { panic!("expected a block") };
    let Stmt::Case { hi: Some(_), body: Some(_), .. } = out.ast[out.ast[items][0]] else {
        panic!("expected a case range")
    };
}

#[test]
fn a_long_run_of_labels_does_not_recurse() {
    // Generated dispatch tables really do look like this, and a stack frame per label is how a
    // parser overflows on one.
    let mut src = String::from("void f(int x) { switch (x) { ");
    for value in 0..2000 {
        src.push_str(&format!("case {value}: "));
    }
    src.push_str("break; } }");
    let out = parsed(&src);
    assert!(out.ast.counts().stmts > 2000);
}

#[test]
fn a_declaration_may_follow_a_label_since_c23() {
    let out = parsed("void f(void) { there: int x = 1; (void)x; }");
    let stmts = body_of(&out, only_decl(&out));
    let Stmt::Label { body: Some(body), .. } = stmts[0] else { panic!("expected a label") };
    assert!(matches!(out.ast[body], Stmt::Decl(_)));
}

#[test]
fn a_label_may_end_a_block() {
    let out = parsed("void f(void) { there: }");
    let stmts = body_of(&out, only_decl(&out));
    assert!(matches!(stmts[0], Stmt::Label { body: None, .. }));
}

#[test]
fn the_gnu_statements_are_here() {
    let out = parsed(
        "void f(void *p) {
             __label__ again;
         again:
             asm volatile (\"nop\" ::: \"memory\");
             asm goto (\"jmp %l0\" :: \"r\"(p) : : done);
         done:
             goto *p;
         }",
    );
    let stmts = body_of(&out, only_decl(&out));
    assert!(matches!(stmts[0], Stmt::LocalLabels(_)));
    let Stmt::Label { body: Some(body), .. } = stmts[1] else { panic!("expected a label") };
    assert!(matches!(out.ast[body], Stmt::Asm(_)));
}

#[test]
fn an_asm_statement_keeps_its_operands_in_order() {
    let out = parsed("void f(int a, int b) { asm (\"add\" : \"=r\"(a) : \"r\"(b) : \"cc\"); }");
    let stmts = body_of(&out, only_decl(&out));
    let Stmt::Asm(asm) = stmts[0] else { panic!("expected an asm statement") };
    let asm = out.ast[asm];
    assert_eq!(asm.outputs.len(), 1);
    assert_eq!(asm.inputs.len(), 1);
    assert_eq!(asm.clobbers.len(), 1);
    assert!(asm.labels.is_empty());
    assert!(asm.quals.is_none());
}

#[test]
fn an_asm_operand_may_be_named() {
    let out = parsed("void f(int a) { asm (\"\" : [out] \"=r\"(a)); }");
    let stmts = body_of(&out, only_decl(&out));
    let Stmt::Asm(asm) = stmts[0] else { panic!("expected an asm statement") };
    let outputs = out.ast[asm].outputs;
    assert!(out.ast[outputs][0].name.is_some());
}

#[test]
fn a_struct_holds_its_members_in_source_order() {
    let out = parsed(
        "struct s {
             int a;
             unsigned b : 3, : 0;
             static_assert(1, \"ok\");
             struct { int c; };
         };",
    );
    let Decl::Var { specs, declarators } = only_decl(&out) else { panic!("expected one") };
    assert!(declarators.is_empty());
    let TypeSpec::Record { fields: Some(members), .. } = out.ast[specs].ty else {
        panic!("expected a record body")
    };
    // Five, because `b : 3, : 0` declares two of them and the list is flat: members declared
    // together share their specifiers and are otherwise unrelated.
    let members: Vec<Member> = out.ast[members].to_vec();
    assert_eq!(members.len(), 5);
    let Member::Field(bits) = members[1] else { panic!("expected a bit-field") };
    assert!(bits.declarator.is_some() && bits.bits.is_some());
    let Member::Field(anonymous) = members[2] else { panic!("expected an unnamed bit-field") };
    assert!(anonymous.declarator.is_none() && anonymous.bits.is_some());
    assert!(matches!(members[3], Member::StaticAssert { .. }));
    let Member::Field(nested) = members[4] else { panic!("expected an anonymous struct") };
    assert!(nested.declarator.is_none() && nested.bits.is_none());
}

#[test]
fn a_bit_int_takes_a_sign_on_either_side_of_it() {
    for src in ["unsigned _BitInt(8) x;", "_BitInt(8) unsigned x;"] {
        let out = parsed(src);
        let Decl::Var { specs, .. } = only_decl(&out) else { panic!("expected one") };
        let TypeSpec::Builtin(builtin) = out.ast[specs].ty else { panic!("expected the keywords") };
        assert!(builtin.set.has(BuiltinSet::BIT_INT), "{src}");
        assert!(builtin.set.has(BuiltinSet::UNSIGNED), "{src}");
        assert!(builtin.width.is_some(), "{src}");
    }
}

#[test]
fn a_bit_int_written_twice_is_two_types() {
    assert_eq!(
        complaints("_BitInt(8) _BitInt(8) x;"),
        ["two or more data types in declaration specifiers"]
    );
    assert_eq!(
        complaints("struct s _BitInt(8) x;"),
        ["two or more data types in declaration specifiers"]
    );
}

#[test]
fn an_enumeration_may_name_its_underlying_type() {
    let out = parsed("enum e : unsigned char { a, b = 2 };");
    let Decl::Var { specs, .. } = only_decl(&out) else { panic!("expected one") };
    let TypeSpec::Enum { enumerators: Some(list), underlying: Some(_), .. } = out.ast[specs].ty
    else {
        panic!("expected an enumeration with an underlying type")
    };
    assert_eq!(list.len(), 2);
}

#[test]
fn an_initializer_keeps_the_designations_it_was_written_with() {
    let out = parsed("int a[8] = { [0] = 1, [2 ... 4] = 2, 3 };");
    let Decl::Var { declarators, .. } = only_decl(&out) else { panic!("expected a declaration") };
    let init = out.ast[declarators][0].init.expect("an initializer");
    let Init::List(items) = out.ast[init] else { panic!("expected a braced initializer") };
    assert_eq!(items.len(), 3);
    assert_eq!(out.ast[items][0].designators.len(), 1);
    assert!(out.ast[items][2].designators.is_empty());
}

#[test]
fn an_array_size_may_be_absent_or_a_star_or_an_expression() {
    let out = parsed("void f(int n, int a[], int b[*], int c[static 4]);");
    let Decl::Var { declarators, .. } = only_decl(&out) else { panic!("expected a declaration") };
    let declarator = out.ast[out.ast[declarators][0].declarator];
    let Derived::Function { params, .. } = out.ast[declarator.derived][0] else {
        panic!("expected a function declarator")
    };
    let sizes: Vec<ArraySize> = out.ast[params][1..]
        .iter()
        .map(|param| {
            let derived = out.ast[param.declarator].derived;
            let Derived::Array { size, .. } = out.ast[derived][0] else {
                panic!("expected an array parameter")
            };
            size
        })
        .collect();
    assert!(matches!(sizes[0], ArraySize::Unspecified));
    assert!(matches!(sizes[1], ArraySize::Star));
    assert!(matches!(sizes[2], ArraySize::Expr(_)));
}

#[test]
fn the_attribute_syntaxes_are_both_accepted() {
    let out = parsed(
        "[[gnu::hot]] void f(void);
         __attribute__((noreturn, format(printf, 1, 2))) void g(const char *, ...);
         [[deprecated(\"no\")]];",
    );
    assert_eq!(out.ast.top_level().len(), 3);
    assert!(matches!(out.ast[out.ast.top_level()[2]], Decl::Attributes(_)));
    let Decl::Var { specs, .. } = out.ast[out.ast.top_level()[0]] else { panic!("expected one") };
    assert_eq!(out.ast[specs].attrs.len(), 1);
}

#[test]
fn an_attribute_after_a_declarator_does_not_start_a_definition() {
    // Everything a specifier can start here is an old-style definition's parameters, except an
    // attribute, which is a specifier keyword and is not one of those.
    let out = parsed("int packed_var __attribute__((aligned(16)));");
    let Decl::Var { declarators, .. } = only_decl(&out) else { panic!("expected a declaration") };
    assert_eq!(out.ast[declarators][0].attrs.len(), 1);
}

#[test]
fn an_assembler_name_is_not_an_assembly_statement() {
    let out = parsed("extern int errno __asm__(\"__errno_location\") __attribute__((const));");
    let Decl::Var { declarators, .. } = only_decl(&out) else { panic!("expected a declaration") };
    let item = out.ast[declarators][0];
    assert!(item.asm_label.is_some());
    assert_eq!(item.attrs.len(), 1);
}

#[test]
fn a_file_scope_assembly_statement_is_a_declaration() {
    let out = parsed("asm(\".text\");");
    assert!(matches!(only_decl(&out), Decl::Asm(_)));
}

#[test]
fn a_static_assertion_is_a_declaration_and_a_block_item() {
    let out = parsed("static_assert(1, \"ok\"); void f(void) { static_assert(1); }");
    assert!(matches!(out.ast[out.ast.top_level()[0]], Decl::StaticAssert { message: Some(_), .. }));
    let stmts = body_of(&out, out.ast[out.ast.top_level()[1]]);
    let Stmt::Decl(decl) = stmts[0] else { panic!("expected a declaration") };
    assert!(matches!(out.ast[decl], Decl::StaticAssert { message: None, .. }));
}

#[test]
fn a_definition_is_told_from_a_declaration_after_the_declarator() {
    let out = parsed("static inline int (*f(int a))[4] { return 0; } int g(int a);");
    assert!(matches!(out.ast[out.ast.top_level()[0]], Decl::Function { .. }));
    assert!(matches!(out.ast[out.ast.top_level()[1]], Decl::Var { .. }));
    let Decl::Function { specs, .. } = out.ast[out.ast.top_level()[0]] else { panic!("one") };
    assert_eq!(out.ast[specs].storage, Some(StorageClass::Static));
}

#[test]
fn auto_is_a_type_when_nothing_else_names_one_and_a_storage_class_when_something_does() {
    // The keyword is settled after the whole list has been read, because until then there is
    // no telling which of its two meanings it has.
    let out = parsed("static auto x = 1;");
    let Decl::Var { specs, .. } = only_decl(&out) else { panic!("expected a declaration") };
    assert_eq!(out.ast[specs].ty, TypeSpec::Auto(Deduction::Auto));
    assert_eq!(out.ast[specs].storage, Some(StorageClass::Static));

    let out = parsed("constexpr auto y = 2;");
    let Decl::Var { specs, .. } = only_decl(&out) else { panic!("expected a declaration") };
    assert_eq!(out.ast[specs].ty, TypeSpec::Auto(Deduction::Auto));
    assert_eq!(out.ast[specs].storage, Some(StorageClass::Constexpr));

    let mut fixture = Fixture::new(Std::C17);
    let out = fixture.parse("auto int z = 3;");
    assert!(!out.failed(), "{:?}", out.diagnostics);
    let Decl::Var { specs, .. } = only_decl(&out) else { panic!("expected a declaration") };
    assert!(matches!(out.ast[specs].ty, TypeSpec::Builtin(_)));
    assert_eq!(out.ast[specs].storage, Some(StorageClass::Auto));
}

/// Declaring the loop variable in the `for` is C99, and gcc refuses it in gnu89 as well, which
/// is one of the places a GNU dialect is not the iso one with more allowed. The header is
/// parsed anyway, so the body is still checked and the answer is one complaint.
#[test]
fn a_declaration_in_a_for_header_needs_c99() {
    let mut fixture = Fixture::new(Std::C89);
    let out = fixture.parse("void f(void) { for (int i = 0; i < 4; i++) ; }");
    let said: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(said, vec!["`for` loop initial declarations are only allowed in C99 or C11 mode"]);
    let stmts = body_of(&out, only_decl(&out));
    assert!(matches!(stmts[0], Stmt::For { init: ForInit::Decl(_), .. }));

    let mut fixture = Fixture::new(Std::C99);
    let out = fixture.parse("void f(void) { for (int i = 0; i < 4; i++) ; }");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
}

#[test]
fn the_gnu_spelling_of_a_deduced_type_is_kept_apart_from_the_c23_one() {
    let out = parsed("__auto_type x = 1;");
    let Decl::Var { specs, .. } = only_decl(&out) else { panic!("expected a declaration") };
    assert_eq!(out.ast[specs].ty, TypeSpec::Auto(Deduction::AutoType));
    assert_eq!(out.ast[specs].storage, None);
}

#[test]
fn a_deduced_type_is_deduced_from_an_expression_and_not_from_a_braced_list() {
    // There is no object yet for a list to be laid out in, so the parser asks for the one
    // thing that can be deduced from and the reader is told about the expression.
    let c23 = complaints("auto x = {1};");
    assert!(c23.iter().any(|m| m.contains("expected an expression")), "{c23:?}");

    let gnu = complaints("__auto_type x = {1};");
    assert!(gnu.iter().any(|m| m.contains("expected an expression")), "{gnu:?}");

    // A list is still a list where nothing is deduced.
    let out = parsed("int x[] = {1};");
    let Decl::Var { declarators, .. } = only_decl(&out) else { panic!("expected a declaration") };
    let init = out.ast[declarators][0].init.expect("an initializer");
    assert!(matches!(out.ast[init], Init::List(_)), "{:?}", out.ast[init]);
}

#[test]
fn neither_spelling_of_a_deduced_type_is_a_type_name() {
    // A type name has no declarator to deduce from, so there is nothing for either of them to
    // mean there, which is what gcc and clang both say.
    let size = complaints("int f(void) { return sizeof(__auto_type); }");
    assert!(!size.is_empty(), "expected the type name to be refused");

    let cast = complaints("int f(void) { return (__auto_type)1; }");
    assert!(!cast.is_empty(), "expected the cast to be refused");
}

#[test]
fn a_second_auto_is_a_duplicate_and_another_storage_class_is_a_conflict() {
    let duplicate = complaints("auto auto x = 1;");
    assert!(duplicate.iter().any(|m| m.contains("duplicate `auto`")), "{duplicate:?}");

    // A `typedef` names a type rather than deducing one, so the `auto` beside it is the
    // storage class and the two of them are two storage classes.
    let combined = complaints("typedef auto T;");
    assert!(
        combined.iter().any(|m| m == "`auto` cannot be combined with `typedef`"),
        "{combined:?}"
    );
}

#[test]
fn an_old_style_definition_parses_and_c23_rejects_it() {
    let mut fixture = Fixture::new(Std::C17);
    let out = fixture.parse("int f(a, b) int a; char *b; { return a; }");
    assert!(!out.failed(), "{:?}", out.diagnostics);
    let Decl::Function { params, .. } = out.ast[out.ast.top_level()[0]] else { panic!("one") };
    assert_eq!(params.len(), 2);

    let complaints = complaints("int f(a, b) int a; char *b; { return a; }");
    assert!(complaints.iter().any(|m| m.contains("old style")), "{complaints:?}");
}

#[test]
fn a_parameter_is_in_scope_in_the_body() {
    // If the parameter were not re-declared in the body's scope, `T` would still be a type name
    // and `T * x` would be a declaration rather than a multiplication.
    let out = parsed("typedef int T; int f(int T) { return T * 2; }");
    let stmts = body_of(&out, out.ast[out.ast.top_level()[1]]);
    let Stmt::Return(Some(value)) = stmts[0] else { panic!("expected a return") };
    assert!(matches!(out.ast[value], Expr::Binary { op: BinaryOp::Mul, .. }));
}

#[test]
fn a_broken_declaration_does_not_cost_the_next_one() {
    let out = Fixture::new(Std::C23).parse("int x = ; int y = 2;");
    assert!(out.failed());
    assert_eq!(out.ast.top_level().len(), 2);
    assert!(matches!(out.ast[out.ast.top_level()[1]], Decl::Var { .. }));
}

#[test]
fn a_broken_statement_does_not_cost_the_rest_of_the_block() {
    let out = Fixture::new(Std::C23).parse("void f(void) { int a = 1 +; a++; return; }");
    assert!(out.failed());
    let stmts = body_of(&out, only_decl(&out));
    assert_eq!(stmts.len(), 3);
    assert!(matches!(stmts[2], Stmt::Return(None)));
}

#[test]
fn a_missing_semicolon_is_reported_once() {
    let complaints = complaints("void f(void) { int a = 1 return; }");
    assert_eq!(complaints.len(), 1, "{complaints:?}");
    assert!(complaints[0].contains("expected `;`"), "{complaints:?}");
}

#[test]
fn nesting_deeper_than_the_cap_is_reported_and_not_a_crash() {
    let depth = 400;
    let src = format!("int x = {}1{};", "(".repeat(depth), ")".repeat(depth));
    let out = Fixture::new(Std::C23).parse(&src);
    assert!(out.failed());
    let deep = out.diagnostics.iter().filter(|d| d.message.contains("nested more deeply")).count();
    assert_eq!(deep, 1, "the cap is reported once");
}

#[test]
fn the_error_limit_stops_the_parse() {
    let src = "int a = ;".repeat(40);
    let out = Fixture::new(Std::C23).parse(&src);
    let notes = out.diagnostics.iter().filter(|d| d.message.contains("too many errors")).count();
    assert_eq!(notes, 1);
}

#[test]
fn a_declaration_and_a_statement_are_told_apart_at_block_scope() {
    let out = parsed("typedef int T; void f(void) { T x; x * 2; T * y; }");
    let stmts = body_of(&out, out.ast[out.ast.top_level()[1]]);
    assert!(matches!(stmts[0], Stmt::Decl(_)));
    assert!(matches!(stmts[1], Stmt::Expr(_)));
    assert!(matches!(stmts[2], Stmt::Decl(_)));
}

/// `__extension__` is written in front of a declaration as often as in front of an expression,
/// and at block scope the same keyword begins both. What tells them apart is the token after it,
/// which is why the decision is a lookahead and not a keyword test.
#[test]
fn the_extension_keyword_begins_a_declaration_as_well_as_an_expression() {
    let out = parsed("__extension__ typedef struct { long long q; } lldiv_t;");
    let Decl::Var { specs, .. } = out.ast[out.ast.top_level()[0]] else { panic!("a declaration") };
    assert_eq!(out.ast[specs].storage, Some(StorageClass::Typedef));

    let out = parsed("struct s { int a; __extension__ long long b; };");
    let Decl::Var { specs, .. } = out.ast[out.ast.top_level()[0]] else { panic!("a declaration") };
    let TypeSpec::Record { fields: Some(fields), .. } = out.ast[specs].ty else {
        panic!("a struct with a body")
    };
    assert_eq!(out.ast[fields].len(), 2, "the second member is still a member");

    let out = parsed("void f(int n) { __extension__ long long a = 1; __extension__ (n + 1); }");
    let stmts = body_of(&out, out.ast[out.ast.top_level()[0]]);
    assert!(matches!(stmts[0], Stmt::Decl(_)), "a declaration");
    assert!(matches!(stmts[1], Stmt::Expr(_)), "an expression, from the same keyword");
}
