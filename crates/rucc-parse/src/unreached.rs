//! Which `static inline` functions of the headers nothing in the unit reaches.
//!
//! A kernel unit includes a few thousand inline functions from its headers and calls a few
//! hundred of them. No code is written for the rest, by gcc or by us, but checking their bodies
//! was about a third of the checker's time in an `allmodconfig` build, which is most of what kept
//! the build short of twice gcc's speed. tamnd/rucc-kernel#244.
//!
//! The question is answered from the tokens, before anything is checked, so it has to be asked
//! in a way that can only err on the side of a function being reached. A function is reached
//! when its name is written anywhere outside the definitions that are not reached, the function's
//! own definition included in those. That counts a name that only happens to be spelled the same,
//! a member or a label, and a name in `sizeof` that nothing calls, and all that costs is a body
//! checked that did not need to be. A name in `alias("...")` and the other attributes that name a
//! symbol in a string is counted too, and so is a name in `#pragma weak` and in
//! `#pragma redefine_extname`.

use rucc_ast::{Decl, FuncSpecs, StorageClass};
use rucc_base::hash::Map;
use rucc_base::{Interner, Symbol};
use rucc_diag::Span;
use rucc_lex::{Punct, TokenKind, Tokens};

use crate::Parsed;

/// The attributes that keep a function in the object whether or not it is called.
const KEPT: [&str; 4] = ["used", "constructor", "destructor", "retain"];

/// The attributes whose argument is a string naming a symbol, in both of gcc's spellings, since
/// the tokens are read before an attribute's name has its underscores taken off.
const NAMING: [&str; 8] =
    ["alias", "__alias__", "ifunc", "__ifunc__", "weakref", "__weakref__", "symver", "__symver__"];

/// For each external declaration, whether it is the definition of a `static inline` function in
/// a header that nothing in the unit reaches.
///
/// `in_header` says whether a declaration that starts at a span is in a file other than the one
/// being compiled. A function the file itself defines is always checked, so that what is wrong
/// in it is reported whether or not it is called.
#[must_use]
pub fn unreached(
    tokens: &Tokens,
    parsed: &Parsed,
    interner: &Interner,
    in_header: impl Fn(Span) -> bool,
) -> Vec<bool> {
    let ast = &parsed.ast;
    let top = ast.top_level();
    // An attribute's name is as it was written, so `__used__` is taken to be `used` here.
    let keeps = |name: Symbol| {
        let name = interner.resolve(name);
        let bare = name.strip_prefix("__").and_then(|name| name.strip_suffix("__"));
        KEPT.contains(&bare.unwrap_or(name))
    };
    // The candidates by name. A name defined twice is an error the checker reports, and both
    // definitions are checked then.
    let mut by_name: Map<Symbol, Option<usize>> = Map::default();
    for (index, &decl) in top.iter().enumerate() {
        let Decl::Function { specs, declarator, .. } = ast[decl] else { continue };
        let specs = &ast[specs];
        let Some(name) = ast[declarator].name else { continue };
        let candidate = specs.storage == Some(StorageClass::Static)
            && specs.func.has(FuncSpecs::INLINE)
            && !ast[specs.attrs].iter().any(|attr| keeps(attr.name))
            && in_header(ast.decl_span(decl));
        if candidate {
            by_name.entry(name).and_modify(|seen| *seen = None).or_insert(Some(index));
        }
    }
    let mut unreached = vec![false; top.len()];
    if by_name.is_empty() {
        return unreached;
    }
    for &index in by_name.values().flatten() {
        unreached[index] = true;
    }

    let naming: Vec<Symbol> = NAMING.iter().filter_map(|name| interner.find(name)).collect();
    let ends = &parsed.ends;
    let range = |index: usize| {
        let lo = if index == 0 { 0 } else { ends[index - 1] as usize };
        lo..ends[index] as usize
    };
    let mut work = Vec::new();
    let reach = |name: Symbol, unreached: &mut Vec<bool>, work: &mut Vec<usize>| {
        if let Some(&Some(index)) = by_name.get(&name) {
            if unreached[index] {
                unreached[index] = false;
                work.push(index);
            }
        }
    };
    let toks = &tokens.tokens;
    let walk = |lo: usize, hi: usize, unreached: &mut Vec<bool>, work: &mut Vec<usize>| {
        for at in lo..hi.min(toks.len()) {
            let tok = toks[at];
            match tok.kind {
                TokenKind::Ident => reach(Symbol::from_raw(tok.value), unreached, work),
                TokenKind::Str
                    if at >= 2 && toks[at - 1].kind == TokenKind::Punct(Punct::LParen) =>
                {
                    let before = toks[at - 2];
                    let names = before.kind == TokenKind::Ident
                        && naming.contains(&Symbol::from_raw(before.value));
                    if names {
                        let text: String = tokens.strings[tok.value as usize]
                            .elements
                            .iter()
                            .filter_map(|&c| char::from_u32(c))
                            .collect();
                        if let Some(name) = interner.find(&text) {
                            reach(name, unreached, work);
                        }
                    }
                }
                _ => {}
            }
        }
    };
    // What is not a candidate is reached, and so is whatever it names.
    let candidates = unreached.clone();
    let mut from = 0;
    for (index, _) in candidates.iter().enumerate().filter(|&(_, &candidate)| candidate) {
        let r = range(index);
        walk(from, r.start, &mut unreached, &mut work);
        from = r.end;
    }
    walk(from, toks.len(), &mut unreached, &mut work);
    for pragma in &parsed.weak_pragmas {
        reach(pragma.name, &mut unreached, &mut work);
        if let Some(target) = pragma.target {
            reach(target, &mut unreached, &mut work);
        }
    }
    for pragma in &parsed.extname_pragmas {
        reach(pragma.old, &mut unreached, &mut work);
        reach(pragma.new, &mut unreached, &mut work);
    }
    while let Some(index) = work.pop() {
        let r = range(index);
        walk(r.start, r.end, &mut unreached, &mut work);
    }
    unreached
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_lex::{Convert, Keywords, Options, convert, tokenize};
    use rucc_session::Std;
    use rucc_target::{TargetInfo, Triple};

    use crate::{Context, parse};

    /// The flags for `source`, every declaration of it taken to be in a header.
    fn unreached(source: &str) -> Vec<bool> {
        let std = Std::C23;
        let mut interner = Interner::new();
        let keywords = Keywords::new(&mut interner, std, true);
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let (pp, _) = tokenize(source.as_bytes(), 0, Options::new(), &mut interner);
        let cx = Convert {
            keywords: &keywords,
            interner: &interner,
            target: &target,
            std,
            gnu: true,
            pedantic: false,
            kencc: false,
        };
        let (tokens, _) = convert(&pp, &cx);
        let mut parse_cx = Context::new(&interner, std);
        parse_cx.gnu = true;
        let parsed = parse(&tokens, parse_cx);
        assert!(!parsed.failed());
        super::unreached(&tokens, &parsed, &interner, |_| true)
    }

    #[test]
    fn an_inline_function_nothing_calls_is_not_reached_nor_what_only_it_calls() {
        let flags = unreached(
            "static inline int a(void) { return 1; }\n\
             static inline int b(void) { return a(); }\n\
             int c(void) { return 0; }\n",
        );
        assert_eq!(flags, [true, true, false]);
    }

    #[test]
    fn a_call_from_a_function_that_is_written_reaches_everything_under_it() {
        let flags = unreached(
            "static inline int a(void) { return 1; }\n\
             static inline int b(void) { return a(); }\n\
             int c(void) { return b(); }\n",
        );
        assert_eq!(flags, [false, false, false]);
    }

    #[test]
    fn a_call_written_after_the_definition_reaches_it() {
        let flags = unreached(
            "static inline int a(void) { return a(); }\n\
             int (*p)(void) = a;\n",
        );
        assert_eq!(flags, [false, false]);
    }

    #[test]
    fn a_function_that_only_calls_itself_is_not_reached() {
        let flags = unreached("static inline int a(int n) { return n ? a(n - 1) : 0; }\n");
        assert_eq!(flags, [true]);
    }

    #[test]
    fn a_function_kept_by_an_attribute_and_one_that_is_not_inline_are_checked() {
        let flags = unreached(
            "static inline __attribute__((__used__)) int a(void) { return 1; }\n\
             static int b(void) { return 2; }\n\
             inline int c(void) { return 3; }\n",
        );
        assert_eq!(flags, [false, false, false]);
    }

    #[test]
    fn a_name_in_an_alias_reaches_the_function() {
        let flags = unreached(
            "static inline int a(void) { return 1; }\n\
             int d(void) __attribute__((__alias__(\"a\")));\n",
        );
        assert_eq!(flags, [false, false]);
    }
}
