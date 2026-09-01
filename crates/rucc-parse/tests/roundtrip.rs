//! The printer against the parser, which is the check that neither of them is inventing things.
//!
//! Each case is parsed, printed, parsed again and printed again. The second parse has to be as
//! clean as the first, the two texts have to be the same, and the two trees have to be the same
//! size. Between them those catch a printer that drops a node, a printer that writes something
//! the parser will not take back, and a printer whose output means something else than what it
//! was given, which is the failure that reads correctly and is not.
//!
//! What is not checked is that the output looks like the input. It does not: the layout is the
//! printer's, floating constants come out in hexadecimal, and the keywords come out in the
//! spellings that every dialect has. That is the printer's contract, described on
//! `rucc_ast::Printer`.

use rucc_ast::print;
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
        let mut interner = Interner::new();
        let keywords = Keywords::new(&mut interner, std, true);
        let target =
            TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
        Fixture { interner, keywords, target, std }
    }

    fn parse(&mut self, src: &str) -> Parsed {
        let (pp, diagnostics) = tokenize(src.as_bytes(), 0, Options::new(), &mut self.interner);
        assert!(diagnostics.is_empty(), "the scanner disliked this:\n{src}");
        let cx = Convert {
            keywords: &self.keywords,
            interner: &self.interner,
            target: &self.target,
            std: self.std,
            pedantic: false,
        };
        let (tokens, diagnostics) = convert(&pp, &cx);
        assert!(diagnostics.is_empty(), "phase 7 disliked this:\n{src}");
        parse(&tokens, Context::new(&self.interner, self.std))
    }
}

/// Parses `src`, prints it, parses what came out and prints that, and insists the two agree.
///
/// The printed text is given back, so that a case can also say what it expects to see.
fn round_trip(std: Std, src: &str) -> String {
    let mut fixture = Fixture::new(std);

    let first = fixture.parse(src);
    let complaints: Vec<&str> = first.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(complaints.is_empty(), "expected this to parse:\n{src}\ngot {complaints:?}");
    let printed = print(&first.ast, &fixture.interner);

    let second = fixture.parse(&printed);
    let complaints: Vec<&str> = second.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(complaints.is_empty(), "the printer wrote this:\n{printed}\ngot {complaints:?}");
    let again = print(&second.ast, &fixture.interner);

    assert_eq!(printed, again, "printing twice gave two different programs");
    assert_eq!(
        first.ast.counts(),
        second.ast.counts(),
        "the tree changed size on the way through:\n{printed}"
    );
    printed
}

/// The same, in C23 with the GNU extensions on, which is what everything defaults to here.
fn printed(src: &str) -> String {
    round_trip(Std::C23, src)
}

#[test]
fn declarations_and_declarators_come_back() {
    printed(
        "int a;
         static int b = 1;
         extern const char *const p;
         int (*f[3])(char);
         int (*g)(void);
         typedef unsigned long long ull;
         ull h;
         int arr[10][20];
         void k(int a[static const 4]);
         int *restrict q;
         _Atomic(int) at;
         _Alignas(16) char buf[64];
         _Thread_local int tls;
         int unnamed(int, char *, ...);
         void nothing();",
    );
}

#[test]
fn a_pointer_to_an_array_keeps_its_parentheses() {
    let text = printed("int (*p)[4]; int *q[4];");
    assert!(text.contains("int (*p)[4];"), "{text}");
    assert!(text.contains("int *q[4];"), "{text}");
}

#[test]
fn tags_and_their_bodies_come_back() {
    printed(
        "struct S {
             int x, y;
             unsigned b : 3, : 0;
             struct { int inner; };
             union U { int a; float f; } u;
             static_assert(1, \"a struct may hold one\");
         };
         struct Incomplete;
         enum E : unsigned char { A, B = 3, C };
         enum Plain { D };
         struct __attribute__((packed)) P { char c; };
         union { int one; } anonymous;",
    );
}

#[test]
fn members_declared_together_stay_together() {
    // Splitting them would make the anonymous structure two anonymous structures, which is a
    // different program that looks like the same one.
    let text = printed("struct S { struct { int x; } a, b; };");
    assert_eq!(text.matches("struct {").count(), 1, "{text}");
}

#[test]
fn statements_come_back() {
    printed(
        "int main(void) {
             int i = 0;
             for (int j = 0; j < 10; j++) { i += j; }
             for (;;) break;
             while (i) { i--; }
             do { i++; } while (i < 3);
             if (i) i = 1; else if (i == 2) i = 2; else i = 3;
             switch (i) { case 1: case 2 ... 4: i = 5; break; default: break; }
             { int shadowed = i; (void)shadowed; }
             ;
             goto done;
         done:
             return i;
         }",
    );
}

#[test]
fn an_else_is_kept_away_from_the_wrong_if() {
    let text = printed("void f(int a, int b) { if (a) { if (b) a = 1; } else a = 2; }");
    // The braces the printer writes are not the ones the source had, and they have to be there:
    // without them the `else` would read as the inner `if`'s.
    let inner = text.find("if (b)").expect("the inner if");
    let alternative = text.find("else").expect("the else");
    assert!(text[inner..alternative].contains('}'), "{text}");
}

#[test]
fn expressions_come_back() {
    printed(
        "int f(int a, int b) {
             int c = a + b * 2 - (a - b);
             c = a ? b : c;
             c = a ?: b;
             c += a << 2 >> 1;
             c = (a, b);
             c = a > b == b < a;
             c = a & b | b ^ a;
             c = a && b || !a;
             c = sizeof(int) + sizeof a;
             c = _Alignof(int) + __alignof__ a;
             c = a++ + ++b;
             c = -a + +b + ~a;
             c = *&a;
             c = (char)a;
             c = __extension__ a;
             c = __builtin_choose_expr(1, a, b);
             c = __builtin_types_compatible_p(int, long);
             c = _Generic(a, int: 1, default: 0);
             c = ({ int t = a; t + 1; });
             return c;
         }",
    );
}

#[test]
fn grouping_that_the_tree_does_not_hold_is_worked_out_again() {
    let text = printed("int f(int a, int b, int c) { return (a + b) * c - a * (b - c); }");
    assert!(text.contains("return (a + b) * c - a * (b - c);"), "{text}");
}

#[test]
fn calls_and_members_and_subscripts_come_back() {
    printed(
        "struct S { int x; struct S *next; };
         int g(int, int);
         int f(struct S *s, int *a) {
             return g(s->x, a[1]) + (*s).x + s->next->x;
         }",
    );
}

#[test]
fn constants_come_back_as_the_same_numbers() {
    printed(
        "int i = 1;
         unsigned u = 4294967295u;
         long l = 1l;
         unsigned long long ull = 18446744073709551615ull;
         int hex = 0x7fff;
         int oct = 0777;
         int bin = 0b1011;
         double d = 1.5;
         float f = 0.1f;
         long double ld = 1e300l;
         char c = 'a';
         char nul = '\\0';
         char esc = '\\xff';
         int wide = L'x';
         char *s = \"hello\\n\";
         char *quoted = \"a \\\"b\\\" c\";
         int *n = nullptr;
         bool t = true;",
    );
}

#[test]
fn a_floating_constant_is_written_so_that_it_reads_back_exactly() {
    let text = printed("double d = 0.1;");
    assert!(text.contains("0x"), "a decimal spelling would not be exact: {text}");
}

#[test]
fn initializers_come_back() {
    printed(
        "struct Point { int x; int y; };
         struct Point pt = { .x = 1, .y = 2 };
         struct Point old = { x: 1, y: 2 };
         int nums[5] = { [0] = 1, [1 ... 3] = 2, 4 };
         int nested[2][2] = { { 1, 2 }, { 3, 4 } };
         char msg[] = \"hello\";
         int none[] = {};
         struct Point made = (struct Point){ .x = 1 };",
    );
}

#[test]
fn attributes_come_back_in_the_spelling_they_were_written_in() {
    let text = printed(
        "[[deprecated]] int old_thing(void);
         __attribute__((noreturn)) void die(void);
         int packed_var __attribute__((aligned(16)));
         [[gnu::hot]] void hot_fn(void);
         int *__attribute__((may_alias)) aliasing;
         void labelled(void) { [[gnu::hot]] here: return; }",
    );
    assert!(text.contains("[[deprecated]] int old_thing(void);"), "{text}");
    assert!(text.contains("__attribute__((noreturn)) void die(void);"), "{text}");
    assert!(text.contains("int packed_var __attribute__((aligned(16)));"), "{text}");
    assert!(text.contains("[[gnu::hot]] void hot_fn(void);"), "{text}");
}

#[test]
fn assembly_comes_back() {
    printed(
        "__asm__(\".text\");
         int aliased __asm__(\"real_name\");
         void f(int x, int *y) {
             __asm__ volatile (\"nop\");
             __asm__ (\"mov %1, %0\" : \"=r\" (*y) : \"r\" (x) : \"memory\");
             __asm__ (\"nop\" : [out] \"=r\" (*y) : [in] \"r\" (x));
             __asm__ goto (\"jmp %l0\" : : : : there);
         there:
             return;
         }",
    );
}

#[test]
fn the_c23_type_specifiers_come_back() {
    printed(
        "int tq;
         __typeof__(tq) tq2;
         __typeof_unqual__(tq) tq3;
         typeof(int) tq4;
         _BitInt(37) big;
         constexpr int cx = 5;
         static_assert(1 == 1, \"arithmetic still works\");
         static_assert(1);",
    );
}

#[test]
fn a_sign_written_after_a_bit_int_comes_back_in_front_of_it() {
    // The two spellings are one type, so the printer has one spelling for them, and it is the
    // one that reads: the width has to stay next to the keyword it belongs to.
    let text = printed("unsigned _BitInt(8) a; _BitInt(8) unsigned b; signed _BitInt(9) c;");
    assert!(text.contains("unsigned _BitInt(8) a;"), "{text}");
    assert!(text.contains("unsigned _BitInt(8) b;"), "{text}");
    assert!(text.contains("signed _BitInt(9) c;"), "{text}");
}

#[test]
fn the_gnu_statement_shapes_come_back() {
    printed(
        "void f(int a) {
             __label__ again, once;
             void *where = &&again;
         again:
             once:
             if (a) goto *where;
             return;
         }",
    );
}

#[test]
fn an_old_style_definition_comes_back() {
    // C17, because C23 removed the form and diagnoses it.
    let text = round_trip(
        Std::C17,
        "int old(a, b)
             int a;
             int b;
         {
             return a + b;
         }",
    );
    assert!(text.contains("int old(a, b)"), "{text}");
}

#[test]
fn a_wide_string_that_needs_an_escape_comes_back() {
    printed("int f(void) { return L\"\\x1234\" L\"abc\"[0]; }");
}

#[test]
fn an_empty_translation_unit_prints_as_nothing() {
    assert_eq!(printed(""), "");
}
