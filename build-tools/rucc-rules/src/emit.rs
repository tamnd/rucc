//! The matcher as Rust, for the compiler to link against.
//!
//! `spec/10-backend.md` section 10.2 asks for a generated automaton rather than a chain of
//! conditionals, and this is the half of that which leaves this crate. A build script reads a
//! rule file, builds the trie next door, and writes what this module produces into the build
//! directory of the crate that matches with it. Nothing here is a copy of anything: the rule
//! file is the only place the rules are written, and the table is regenerated whenever it
//! changes.
//!
//! What comes out does not depend on whether the rules lower or simplify. The kind decides
//! what a replacement is written in and therefore what the crate including the file does with
//! it, and that crate already knows which file it asked for. A table of rewrite rules and a
//! table of lowering rules are the same array of nodes and the same array of replacements.
//!
//! # What comes out
//!
//! One Rust source file, holding the trie as an array of nodes, the rules as an array of
//! replacements, and one function per guard. It is data and not code, except for the guards,
//! which are the one part of a rule that has to be evaluated rather than looked up. The walk
//! over the table lives in the crate that includes the file, because the subject of a match
//! there is the compiler's own IR rather than a term, and because a walk written once is a
//! walk written once however many targets there are.
//!
//! The types the file names are the ones that crate defines, and it refers to them through
//! `super`, which is what makes the file includable and nothing else. That is the whole of the
//! contract between the two, and it is small on purpose.
//!
//! # Guards
//!
//! A guard is a condition on the constants a pattern matched, so it becomes a function of the
//! values the bindings hold. A binding that is not a constant at all makes the guard false
//! rather than an error, because a rule guarded by a claim about a number is a rule that does
//! not fire when the operand is not one.
//!
//! The language a guard may be written in is small and this module is where it ends. A head it
//! does not know is refused with the line it is on, rather than emitted and discovered as a
//! compile error in generated code, which is the sort of message nobody can act on.
//!
//! # Computed numbers
//!
//! A replacement may work a number out of the numbers the pattern matched, and such a term
//! becomes a function of the bindings in exactly the way a guard does. That is what lets a rule
//! be written once per width rather than once per constant: multiplying by a power of two is a
//! shift by the log of it, and the log is a number no rule can write down until it has seen which
//! power it matched.
//!
//! What makes a term in a replacement a computation rather than something to build is its head
//! being one of the arithmetic ones, which is the same closed list a guard is written in. So the
//! two halves of a rule compute in one language, a head this module does not know is refused the
//! same way in both, and the crate that runs the table gets a `Piece::Computed` holding a
//! function rather than a term it would have to evaluate itself.

use std::fmt::Write as _;

use crate::ast::{Rule, Term, TermKind};
use crate::error::Error;
use crate::matcher::Matcher;

/// The helpers a guard can call, and what each one needs emitted with it.
const HELPERS: &[(&str, &str)] = &[
    ("sign_extend", SIGN_EXTEND),
    ("zero_extend", ZERO_EXTEND),
    ("extract", EXTRACT),
    ("power_of_two", POWER_OF_TWO),
    ("trailing_zeros", TRAILING_ZEROS),
    ("shifted", SHIFTED),
    ("low", LOW),
];

/// Turn a rule set and the trie it compiles into into Rust.
///
/// `source` is the rule file as it should be named in the generated file and in anything the
/// compiler says about a rule at run time, so it is the path a person could open rather than
/// wherever the build script happened to find it.
///
/// # Errors
///
/// A guard this module cannot compile, reported with the position of the term that was not
/// understood. Every other way a rule set can be wrong has been reported by the reader or by
/// the trie before anything gets here.
pub fn emit(source: &str, rules: &[Rule], matcher: &Matcher) -> Result<String, Vec<Error>> {
    let mut out = String::new();
    let mut errors = Vec::new();
    let mut wanted: Vec<&'static str> = Vec::new();

    let guards = compile_guards(source, rules, &mut wanted, &mut errors);
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut computed = Vec::new();
    let mut body = String::new();
    replacements(&mut body, source, rules, &guards, &mut wanted, &mut computed, &mut errors);
    if !errors.is_empty() {
        return Err(errors);
    }

    header(&mut out, source, rules, matcher);
    nodes(&mut out, matcher);
    out.push_str(&body);
    out.push_str(&guards.iter().flatten().map(String::as_str).collect::<String>());
    out.push_str(&computed.concat());
    helpers(&mut out, &wanted);
    Ok(out)
}

/// The comment nobody reads until they have to, and the table itself.
fn header(out: &mut String, source: &str, rules: &[Rule], matcher: &Matcher) {
    let shape = matcher.shape();
    let _ = write!(
        out,
        "\
// Generated from {source} by rucc-rules. Do not edit this file: edit the
// rule file and build again. It holds {} rules over {} trie nodes.
//
// The widest node has {} branches. Reading them in the order the rules are written would ask
// that many questions to reach the last of them and to find that none of them matched, and the
// search that is done instead asks {}. {} nodes ask more than one kind of question, which is how
// many of them the order the kinds are tried in decides anything at.
//
// The types are the ones the module that includes this file defines, and the walk over the
// table is there too. What is here is the table.

use super::{{Node, Piece, Rule, Table}};

/// The rule file this table was built from, so that anything said about a rule can name a file
/// somebody can open.
pub const SOURCE: &str = {source:?};

/// The rules of this file, as an automaton over their patterns.
pub static TABLE: Table = Table {{ source: SOURCE, nodes: NODES, rules: RULES }};
",
        rules.len(),
        shape.nodes,
        shape.widest,
        shape.search,
        shape.mixed,
    );
}

/// The trie, one array entry per node, with node zero the root.
fn nodes(out: &mut String, matcher: &Matcher) {
    out.push_str(
        "\n/// The trie over the patterns. A node holds the branches taken on the head of a\n\
         /// term, the branches taken on the value of a constant, the branches taken on a\n\
         /// repeat of an earlier binding, the branch that takes anything, and the rule that\n\
         /// ends here if one does. The first two are sorted, which is what makes finding a\n\
         /// branch a search.\nstatic NODES: &[Node] = &[\n",
    );
    for (index, node) in matcher.nodes.iter().enumerate() {
        let _ = writeln!(out, "    // {index}");
        out.push_str("    Node {\n        heads: &[");
        for (head, arity, next) in &node.heads {
            let _ = write!(out, "\n            ({head:?}, {arity}, {next}),");
        }
        if !node.heads.is_empty() {
            out.push_str("\n        ");
        }
        out.push_str("],\n        ints: &[");
        for (value, next) in &node.ints {
            let _ = write!(out, "\n            ({value}, {next}),");
        }
        if !node.ints.is_empty() {
            out.push_str("\n        ");
        }
        out.push_str("],\n        same: &[");
        for (binding, next) in &node.same {
            let _ = write!(out, "\n            ({binding}, {next}),");
        }
        if !node.same.is_empty() {
            out.push_str("\n        ");
        }
        out.push_str("],\n");
        match &node.wildcard {
            Some((name, next)) => {
                let _ = writeln!(out, "        wildcard: Some(({name:?}, {next})),");
            }
            None => out.push_str("        wildcard: None,\n"),
        }
        let accept: Vec<String> = node.accept.iter().map(ToString::to_string).collect();
        let _ = writeln!(out, "        accept: &[{}],", accept.join(", "));
        out.push_str("    },\n");
    }
    out.push_str("];\n");
}

/// The rules, one array entry each, in the order the file writes them.
#[allow(clippy::too_many_arguments)]
fn replacements(
    out: &mut String,
    source: &str,
    rules: &[Rule],
    guards: &[Option<String>],
    wanted: &mut Vec<&'static str>,
    computed: &mut Vec<String>,
    errors: &mut Vec<Error>,
) {
    out.push_str(
        "\n/// The rules, in the order the rule file writes them, which is the order the\n\
         /// `accept` of a trie node names.\nstatic RULES: &[Rule] = &[\n",
    );
    for (index, rule) in rules.iter().enumerate() {
        let pattern = rule.pattern.to_string();
        let _ = writeln!(out, "    // {source}:{}", rule.line);
        out.push_str("    Rule {\n");
        let _ = writeln!(out, "        pattern: {pattern:?},");
        out.push_str("        replacement: &[");
        let bound = bound_names(&rule.pattern);
        for piece in pieces(source, &rule.replacement, &bound, wanted, computed, errors) {
            let _ = write!(out, "\n            {piece},");
        }
        out.push_str("\n        ],\n");
        match guards[index] {
            Some(_) => {
                let _ = writeln!(out, "        guard: Some(guard_{index}),");
            }
            None => out.push_str("        guard: None,\n"),
        }
        let _ = writeln!(out, "        line: {},", rule.line);
        out.push_str("    },\n");
    }
    out.push_str("];\n");
}

/// The names a pattern binds, in the order the matcher binds them, which is the pre-order it
/// walks the subject in. A replacement names one of them and the table holds the position,
/// because a position is what the match has and a name is what the reader has.
///
/// A name written twice binds once. The second occurrence is a test that the two places hold the
/// same thing rather than a second hole, so it takes no position, and counting it here would put
/// every later name one place along from where the match actually holds it.
fn bound_names(pattern: &Term) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    pattern.walk(&mut |term| {
        if let TermKind::Var(name) = &term.kind {
            if !out.iter().any(|have| have == name) {
                out.push(name.clone());
            }
        }
    });
    out
}

/// One replacement term, flattened into the pieces that build it, in pre-order.
fn pieces(
    source: &str,
    term: &Term,
    bound: &[String],
    wanted: &mut Vec<&'static str>,
    computed: &mut Vec<String>,
    errors: &mut Vec<Error>,
) -> Vec<String> {
    let mut out = Vec::new();
    push_pieces(source, term, bound, wanted, computed, errors, &mut out);
    out
}

#[allow(clippy::too_many_arguments)]
fn push_pieces(
    source: &str,
    term: &Term,
    bound: &[String],
    wanted: &mut Vec<&'static str>,
    computed: &mut Vec<String>,
    errors: &mut Vec<Error>,
    out: &mut Vec<String>,
) {
    match &term.kind {
        TermKind::Var(name) => {
            // The reader has already refused a replacement naming something the pattern never
            // bound, so there is a position for every name that reaches here.
            let index = bound.iter().position(|have| have == name).unwrap_or_default();
            out.push(format!("Piece::Var {{ name: {name:?}, index: {index} }}"));
        }
        TermKind::Int(value) => out.push(format!("Piece::Int({value})")),
        TermKind::App { head, args } if computes(head, args.len()) => {
            // A number the rule works out rather than one it wrote down. What makes it one is its
            // head being arithmetic, and that is the same closed list a guard is written in, so a
            // rule that decides whether to fire and a rule that says what to fire compute in one
            // language rather than in two.
            let index = computed.len();
            let mut used = Vec::new();
            match value(source, term, bound, wanted, &mut used) {
                Ok(text) => {
                    computed.push(computation(index, term, &text, bound, &used));
                    out.push(format!(
                        "Piece::Computed {{ text: {:?}, work: computed_{index} }}",
                        term.to_string()
                    ));
                }
                Err(error) => errors.push(error),
            }
        }
        TermKind::App { head, args } => {
            out.push(format!("Piece::App {{ head: {head:?}, arity: {} }}", args.len()));
            for arg in args {
                push_pieces(source, arg, bound, wanted, computed, errors, out);
            }
        }
    }
}

/// Whether a term in a replacement is arithmetic rather than something to build.
///
/// The heads are the ones [`value`] compiles and the arities are the ones it takes, so a head
/// that is arithmetic at one arity and an instruction at another is read as what it was written
/// as. Nothing in either vocabulary is named this way today and this is what keeps the day one is
/// from turning a rule into a number quietly.
fn computes(head: &str, arity: usize) -> bool {
    matches!((head, arity), ("+" | "-", 2) | ("sign_extend" | "zero_extend" | "extract", 3))
        || (arity == 1 && suffix(head, "ctz").is_some())
}

/// One function per computed piece, which is a guard in every way except what it gives back.
fn computation(index: usize, term: &Term, text: &str, bound: &[String], used: &[usize]) -> String {
    let mut out = format!(
        "\n/// `{term}`, which is a number a replacement works out, written on line {}.\n\
         fn computed_{index}(bound: &[Option<i128>]) -> Option<i128> {{\n",
        term.line
    );
    let mut used = used.to_vec();
    used.sort_unstable();
    used.dedup();
    for at in used {
        let _ = writeln!(
            out,
            "    // {}\n    let Some(Some(v{at})) = {}.copied() else {{ return None }};",
            bound[at],
            reads(at)
        );
    }
    let _ = writeln!(out, "    Some({text})\n}}");
    out
}

/// How a compiled guard or computation reads one of the bindings.
///
/// The first is read by the name for it rather than by its index, because a generated file is
/// linted along with everything else and clippy asks for the name.
fn reads(at: usize) -> String {
    if at == 0 { "bound.first()".to_owned() } else { format!("bound.get({at})") }
}

/// One function per guarded rule, or nothing for a rule with no guard.
fn compile_guards(
    source: &str,
    rules: &[Rule],
    wanted: &mut Vec<&'static str>,
    errors: &mut Vec<Error>,
) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(rules.len());
    for (index, rule) in rules.iter().enumerate() {
        let Some(guard) = &rule.guard else {
            out.push(None);
            continue;
        };
        let bound = bound_names(&rule.pattern);
        let mut used = Vec::new();
        let condition = match condition(source, guard, &bound, wanted, &mut used) {
            Ok(text) => text,
            Err(error) => {
                errors.push(error);
                out.push(None);
                continue;
            }
        };
        // The condition comes out in the order the rule file writes it, so that a reader can hold
        // the two side by side. That is what the lint is turned off for: `(>= k 0)` and `(< k 64)`
        // are two conditions in the rule and `(0..64).contains(&k)` is not either of them.
        let mut text = format!(
            "\n/// `{guard}`, which is the guard of the rule on line {}.\n\
             #[allow(clippy::manual_range_contains)]\nfn guard_{index}(bound: \
             &[Option<i128>]) -> bool {{\n",
            rule.line
        );
        used.sort_unstable();
        used.dedup();
        for at in used {
            let _ = writeln!(
                text,
                "    // {}\n    let Some(Some(v{at})) = {}.copied() else {{ return false }};",
                bound[at],
                reads(at)
            );
        }
        let _ = writeln!(text, "    {}\n}}", bare(&condition));
        out.push(Some(text));
    }
    out
}

/// An expression without the parentheses that wrap the whole of it.
///
/// Every condition is emitted parenthesised, because an operand of one has to be. The outermost
/// one is nobody's operand, and Rust warns about the parentheses around it, which in a generated
/// file is a warning the reader of it can do nothing with.
fn bare(text: &str) -> &str {
    let Some(inner) = text.strip_prefix('(').and_then(|text| text.strip_suffix(')')) else {
        return text;
    };
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        // The pair that opened the string closed before the end of it, so the two ends are not
        // a pair and taking them off would be taking off two different people's parentheses.
        if depth < 0 {
            return text;
        }
    }
    inner
}

/// A guard as a Rust expression of type `bool`.
fn condition(
    source: &str,
    term: &Term,
    bound: &[String],
    wanted: &mut Vec<&'static str>,
    used: &mut Vec<usize>,
) -> Result<String, Error> {
    let TermKind::App { head, args } = &term.kind else {
        return Err(refused(source, term, "a guard is a condition, and this is not one"));
    };
    let arity = args.len();
    // A question about the bits of one number, which has to be asked at a width: whether a
    // constant is a power of two is a different question at eight bits and at sixty four, and
    // the rule that asks it is written at one of them.
    if let Some(bits) = suffix(head, "power_of_two").filter(|_| arity == 1) {
        let inner = value(source, &args[0], bound, wanted, used)?;
        want(wanted, "power_of_two");
        return Ok(format!("power_of_two({bits}, {inner})"));
    }
    match (head.as_str(), arity) {
        ("and" | "or", 1..) => {
            let joint = if head == "and" { " && " } else { " || " };
            let mut parts = Vec::with_capacity(arity);
            for arg in args {
                parts.push(condition(source, arg, bound, wanted, used)?);
            }
            Ok(format!("({})", parts.join(joint)))
        }
        ("not", 1) => Ok(format!("!{}", condition(source, &args[0], bound, wanted, used)?)),
        ("=" | "!=" | "<" | "<=" | ">" | ">=", 2) => {
            let operator = if head == "=" { "==" } else { head.as_str() };
            let left = value(source, &args[0], bound, wanted, used)?;
            let right = value(source, &args[1], bound, wanted, used)?;
            Ok(format!("({left} {operator} {right})"))
        }
        _ => Err(refused(
            source,
            term,
            &format!(
                "`{head}` of {arity} is not a condition a guard can be compiled to. A guard is \
                 `and`, `or`, `not`, `power_of_two.iN`, or a comparison of two numbers"
            ),
        )),
    }
}

/// A term inside a guard that stands for a number.
fn value(
    source: &str,
    term: &Term,
    bound: &[String],
    wanted: &mut Vec<&'static str>,
    used: &mut Vec<usize>,
) -> Result<String, Error> {
    match &term.kind {
        TermKind::Int(number) => Ok(format!("{number}")),
        TermKind::Var(name) => {
            // The reader has already refused a guard naming something the pattern never bound.
            let at = bound.iter().position(|have| have == name).unwrap_or_default();
            used.push(at);
            Ok(format!("v{at}"))
        }
        TermKind::App { head, args } => {
            let arity = args.len();
            // Counting the zero bits a number ends in, at a width, which is the log of it when it
            // is a power of two. This is the one arithmetic here that a replacement needs and a
            // guard does not, and it is what a shift standing in for a multiplication shifts by.
            if let Some(bits) = suffix(head, "ctz").filter(|_| arity == 1) {
                let inner = value(source, &args[0], bound, wanted, used)?;
                want(wanted, "trailing_zeros");
                return Ok(format!("trailing_zeros({bits}, {inner})"));
            }
            match (head.as_str(), arity) {
                // Adding and subtracting, which is what a guard about two offsets into one object
                // is written in.
                //
                // Saturating rather than plain, because a guard is a condition on whatever
                // constants the match happened to hold and there is nothing to stop those being
                // the ends of the type. Plain arithmetic there is a panic in a debug build and a
                // wrap in a release one, and neither is an answer to a question about a rule.
                //
                // Saturating is not the solver's arithmetic either. The solver reads a guard in
                // the width the rule runs at, where adding wraps, and this reads it in `i128`,
                // where it does not. The two agree exactly while the operands stay small, so a
                // rule that adds says how small in the same guard, and one that does not is a
                // rule proved about arithmetic the compiler is not doing.
                ("+" | "-", 2) => {
                    let left = value(source, &args[0], bound, wanted, used)?;
                    let right = value(source, &args[1], bound, wanted, used)?;
                    let name = if head == "+" { "saturating_add" } else { "saturating_sub" };
                    Ok(format!("({left}).{name}({right})"))
                }
                ("sign_extend" | "zero_extend" | "extract", 3) => {
                    let first = width(source, &args[0])?;
                    let second = width(source, &args[1])?;
                    let inner = value(source, &args[2], bound, wanted, used)?;
                    let name = match head.as_str() {
                        "sign_extend" => "sign_extend",
                        "zero_extend" => "zero_extend",
                        _ => "extract",
                    };
                    want(wanted, name);
                    Ok(format!("{name}({first}, {second}, {inner})"))
                }
                _ => Err(refused(
                    source,
                    term,
                    &format!(
                        "`{head}` of {arity} is not a number this can be compiled to. The ones \
                         that are are `+`, `-`, `sign_extend`, `zero_extend`, `extract` and \
                         `ctz.iN`"
                    ),
                )),
            }
        }
    }
}

/// A width, which has to be written out rather than computed, because it is how many bits a
/// machine instruction has room for and not something a program is allowed to vary.
fn width(source: &str, term: &Term) -> Result<String, Error> {
    match &term.kind {
        TermKind::Int(number) if (0..=128).contains(number) => Ok(format!("{number}")),
        _ => Err(refused(source, term, "a width has to be a number from 0 to 128")),
    }
}

/// The width a head names, for the heads that are written once per width as `name.iN`.
///
/// Nothing if the head is some other name, so that a rule writing `ctz` with no width on it is
/// refused with the message about what a number can be rather than compiled at a width nobody
/// chose. The model file says what these mean once per width as well, which is the other half of
/// the reason the width is written rather than inferred.
fn suffix(head: &str, name: &str) -> Option<u32> {
    head.strip_prefix(name)?.strip_prefix(".i")?.parse().ok().filter(|bits| *bits <= 128)
}

/// Remember a helper, and everything it is written in terms of.
fn want(wanted: &mut Vec<&'static str>, name: &'static str) {
    if wanted.contains(&name) {
        return;
    }
    wanted.push(name);
    match name {
        "sign_extend" => want(wanted, "shifted"),
        "zero_extend" | "extract" | "power_of_two" | "trailing_zeros" => want(wanted, "low"),
        _ => {}
    }
}

/// The helpers the guards used, in a fixed order so that the file does not move about between
/// builds for no reason.
fn helpers(out: &mut String, wanted: &[&str]) {
    for (name, text) in HELPERS {
        if wanted.contains(name) {
            out.push_str(text);
        }
    }
}

fn refused(source: &str, term: &Term, message: &str) -> Error {
    Error {
        path: source.to_owned(),
        line: term.line,
        column: term.column,
        message: message.to_owned(),
    }
}

const SIGN_EXTEND: &str = "
/// The low `from` bits of `value`, sign extended to `to` bits.
fn sign_extend(from: u32, to: u32, value: i128) -> i128 {
    shifted(to, shifted(from, value))
}
";

const ZERO_EXTEND: &str = "
/// The low `from` bits of `value`, read as a number and not sign extended.
fn zero_extend(from: u32, to: u32, value: i128) -> i128 {
    low(to, low(from, value))
}
";

const EXTRACT: &str = "
/// The bits from `hi` down to `lo` of `value`, read as a number.
fn extract(hi: u32, lo: u32, value: i128) -> i128 {
    if lo >= 128 || hi < lo {
        return 0;
    }
    low(hi - lo + 1, value >> lo)
}
";

const POWER_OF_TWO: &str = "
/// Whether the low `bits` bits of `value` are one bit set and every other bit clear.
fn power_of_two(bits: u32, value: i128) -> bool {
    let masked = low(bits, value);
    masked > 0 && masked & (masked - 1) == 0
}
";

const TRAILING_ZEROS: &str = "
/// How many zero bits the low `bits` bits of `value` end in, and `bits` when they are all zero.
fn trailing_zeros(bits: u32, value: i128) -> i128 {
    let masked = low(bits, value);
    if masked == 0 { i128::from(bits) } else { i128::from(masked.trailing_zeros()) }
}
";

const SHIFTED: &str = "
/// `value` read as a signed number that many bits wide.
fn shifted(bits: u32, value: i128) -> i128 {
    match 128u32.checked_sub(bits) {
        Some(room) if room > 0 => (value << room) >> room,
        _ => value,
    }
}
";

const LOW: &str = "
/// The low `bits` bits of `value`, read as a number.
fn low(bits: u32, value: i128) -> i128 {
    if bits >= 128 {
        return value;
    }
    #[allow(clippy::cast_possible_wrap)]
    let masked = (value as u128 & ((1u128 << bits) - 1)) as i128;
    masked
}
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn built(text: &str) -> String {
        let rules = parse("rules/test.rules", text).expect("the rules read");
        let matcher = Matcher::build("rules/test.rules", &rules).expect("the matcher builds");
        emit("rules/test.rules", &rules, &matcher).expect("the table is emitted")
    }

    /// The shape of the file, which is what the module that includes it is written against.
    #[test]
    fn a_rule_set_comes_out_as_a_table_of_nodes_and_a_table_of_rules() {
        let out = built(
            "(rule (lower (add.i64 (value.i64 x) (value.i64 y)))\n\
             (x64.add_rr_64 x y)\n\
             (spec (= (bvadd x y) (result))))\n",
        );
        assert!(out.contains("use super::{Node, Piece, Rule, Table};"), "{out}");
        assert!(out.contains("pub const SOURCE: &str = \"rules/test.rules\";"), "{out}");
        assert!(out.contains("(\"add.i64\", 2, 1),"), "{out}");
        assert!(out.contains("wildcard: Some((\"x\", 3)),"), "{out}");
        assert!(out.contains("accept: &[0],"), "{out}");
        assert!(out.contains("Piece::App { head: \"x64.add_rr_64\", arity: 2 }"), "{out}");
        assert!(out.contains("Piece::Var { name: \"x\", index: 0 }"), "{out}");
        assert!(out.contains("Piece::Var { name: \"y\", index: 1 }"), "{out}");
        assert!(out.contains("guard: None,"), "{out}");
    }

    /// A name written twice comes out as a test and not as a second hole, so the positions a
    /// replacement and a guard are written against count it once. Here `k` is binding one, which
    /// it would not be if the second `x` had taken a position of its own.
    #[test]
    fn a_name_written_twice_comes_out_as_a_test_and_takes_no_position() {
        let out = built(
            "(rule (simplify (and.i32 (value.i32 x) (value.i32 x)))\n\
             (value.i32 x)\n\
             (spec (= x (result))))\n\
             (rule (simplify (shl.i32 (value.i32 x) (iconst.i32 k)))\n\
             (if (>= k 0))\n\
             (value.i32 x)\n\
             (spec (= (bvshl x k) (result))))\n",
        );
        assert!(out.contains("same: &[\n            (0, "), "{out}");
        assert!(out.contains("Piece::Var { name: \"x\", index: 0 }"), "{out}");
        assert!(
            out.contains("let Some(Some(v1)) = bound.get(1).copied() else { return false };"),
            "{out}"
        );
    }

    /// A guard becomes a function of the constants the pattern matched, and the helpers it
    /// calls come with it. A binding it reads that is not a constant makes it false, which is
    /// what the `let ... else` in it is for.
    #[test]
    fn a_guard_comes_out_as_a_function_of_the_bindings() {
        let out = built(
            "(rule (lower (shl.i64 (value.i64 x) (iconst.i64 k)))\n\
             (if (and (>= k 0) (< k 64)))\n\
             (x64.shl_ri_64 x k)\n\
             (spec (= (bvshl x k) (result))))\n",
        );
        assert!(out.contains("guard: Some(guard_0),"), "{out}");
        assert!(out.contains("fn guard_0(bound: &[Option<i128>]) -> bool {"), "{out}");
        assert!(
            out.contains("let Some(Some(v1)) = bound.get(1).copied() else { return false };"),
            "{out}"
        );
        assert!(out.contains("(v1 >= 0) && (v1 < 64)"), "{out}");
        // Nothing this guard does not use is emitted, because an unused function in a
        // generated file is a warning in the crate that includes it.
        assert!(!out.contains("fn sign_extend"), "{out}");
        assert!(!out.contains("fn low"), "{out}");
    }

    /// The immediate guard, which is the one that needs the arithmetic helpers, and which is
    /// what pulls `shifted` and `low` in behind them.
    #[test]
    fn a_guard_that_reads_bits_brings_the_helpers_it_needs() {
        let out = built(
            "(rule (lower (add.i64 (value.i64 x) (iconst.i64 k)))\n\
             (if (= k (sign_extend 32 64 (extract 31 0 k))))\n\
             (x64.add_ri_64 x k)\n\
             (spec (= (bvadd x k) (result))))\n",
        );
        assert!(out.contains("v1 == sign_extend(32, 64, extract(31, 0, v1))"), "{out}");
        assert!(out.contains("fn sign_extend(from: u32, to: u32, value: i128) -> i128 {"), "{out}");
        assert!(out.contains("fn shifted(bits: u32, value: i128) -> i128 {"), "{out}");
        assert!(out.contains("fn extract(hi: u32, lo: u32, value: i128) -> i128 {"), "{out}");
        assert!(out.contains("fn low(bits: u32, value: i128) -> i128 {"), "{out}");
        assert!(!out.contains("fn zero_extend"), "{out}");
    }

    /// A guard written in something this module does not compile is refused here, with the
    /// position of the term, rather than emitted and found later as a compile error in a
    /// generated file that nobody wrote.
    #[test]
    fn a_guard_nothing_can_be_made_of_is_refused_where_it_is_written() {
        let rules = parse(
            "rules/test.rules",
            "(rule (lower (add.i64 (value.i64 x) (iconst.i64 k)))\n\
             (if (fits_in_a_byte k))\n\
             (x64.add_ri_64 x k)\n\
             (spec (= (bvadd x k) (result))))\n",
        )
        .expect("the rules read");
        let matcher = Matcher::build("rules/test.rules", &rules).expect("the matcher builds");
        let errors = emit("rules/test.rules", &rules, &matcher).expect_err("the guard is refused");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 2);
        assert!(
            errors[0].message.contains("`fits_in_a_byte` of 1 is not a condition"),
            "{}",
            errors[0]
        );
    }

    /// The rule issue 523 was about, whole: a guard asking whether the matched constant is a
    /// power of two and a replacement shifting by the log of it. Both halves come out as
    /// functions of the bindings and both bring the helper they are written in.
    #[test]
    fn a_replacement_can_work_a_number_out_of_the_one_it_matched() {
        let out = built(
            "(rule (simplify (mul.i32 (value.i32 x) (iconst.i32 k)))\n\
             (if (power_of_two.i32 k))\n\
             (shl.i32 (value.i32 x) (iconst.i32 (ctz.i32 k)))\n\
             (spec (= (bvmul x k) (result))))\n",
        );
        assert!(
            out.contains("Piece::Computed { text: \"(ctz.i32 k)\", work: computed_0 }"),
            "{out}"
        );
        assert!(out.contains("fn computed_0(bound: &[Option<i128>]) -> Option<i128> {"), "{out}");
        assert!(
            out.contains("let Some(Some(v1)) = bound.get(1).copied() else { return None };"),
            "{out}"
        );
        assert!(out.contains("Some(trailing_zeros(32, v1))"), "{out}");
        assert!(out.contains("power_of_two(32, v1)"), "{out}");
        assert!(out.contains("fn power_of_two(bits: u32, value: i128) -> bool {"), "{out}");
        assert!(out.contains("fn trailing_zeros(bits: u32, value: i128) -> i128 {"), "{out}");
        assert!(out.contains("fn low(bits: u32, value: i128) -> i128 {"), "{out}");
    }

    /// The same arithmetic a guard is written in, in a replacement, and the mask a remainder
    /// becomes is what wants it. Nothing about a computed piece is particular to counting bits.
    #[test]
    fn a_replacement_computes_in_the_language_a_guard_computes_in() {
        let out = built(
            "(rule (simplify (urem.i32 (value.i32 x) (iconst.i32 k)))\n\
             (if (power_of_two.i32 k))\n\
             (and.i32 (value.i32 x) (iconst.i32 (- k 1)))\n\
             (spec (= (bvurem x k) (result))))\n",
        );
        assert!(out.contains("Piece::Computed { text: \"(- k 1)\", work: computed_0 }"), "{out}");
        assert!(out.contains("Some((v1).saturating_sub(1))"), "{out}");
        // A subtraction needs no helper, so the only one here is the guard's.
        assert!(!out.contains("fn trailing_zeros"), "{out}");
    }

    /// A computation nothing can be made of is refused where it is written, the same as a guard
    /// is, rather than emitted as a call to a function that does not exist.
    #[test]
    fn a_computed_piece_nothing_can_be_made_of_is_refused_where_it_is_written() {
        let rules = parse(
            "rules/test.rules",
            "(rule (simplify (mul.i32 (value.i32 x) (iconst.i32 k)))\n\
             (shl.i32 (value.i32 x) (iconst.i32 (extract 31 0 (log_of k))))\n\
             (spec (= (bvmul x k) (result))))\n",
        )
        .expect("the rules read");
        let matcher = Matcher::build("rules/test.rules", &rules).expect("the matcher builds");
        let errors =
            emit("rules/test.rules", &rules, &matcher).expect_err("the computation is refused");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 2);
        assert!(errors[0].message.contains("`log_of` of 1 is not a number"), "{}", errors[0]);
    }

    /// A head that only looks like one of the arithmetic ones is built rather than computed. The
    /// width is what says which, so `ctz` with no width on it is a term and not a count.
    #[test]
    fn a_head_with_no_width_on_it_is_not_arithmetic() {
        assert!(computes("ctz.i32", 1));
        assert!(!computes("ctz", 1));
        assert!(!computes("ctz.i32", 2));
        assert!(!computes("ctz.f32", 1));
    }

    /// The first binding is read by the name for it rather than by an index of zero, which is
    /// what the generated file being linted along with the rest of the tree comes to here.
    #[test]
    fn the_first_binding_is_read_by_the_name_for_it() {
        let out = built(
            "(rule (simplify (mul.i32 (iconst.i32 k) (value.i32 x)))\n\
             (if (power_of_two.i32 k))\n\
             (shl.i32 (value.i32 x) (iconst.i32 (ctz.i32 k)))\n\
             (spec (= (bvmul k x) (result))))\n",
        );
        let guard = "let Some(Some(v0)) = bound.first().copied() else { return false };";
        let computed = "let Some(Some(v0)) = bound.first().copied() else { return None };";
        assert!(out.contains(guard), "{out}");
        assert!(out.contains(computed), "{out}");
        assert!(!out.contains("bound.get(0)"), "{out}");
    }
}
