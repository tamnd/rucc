//! What the terms in a rule mean, in bitvectors.
//!
//! A rule relates an IR term to a machine term and claims the two compute the same thing. A
//! solver cannot check that claim without being told what the terms are, so every head a rule
//! uses needs an entry here. `spec/10-backend.md` calls this Crocus's stated tax and says to pay
//! it from the first rule rather than retrofitting it, which is why a head with no entry is an
//! error rather than an unchecked assumption.
//!
//! The model is written in the same language as the rules:
//!
//! ```text
//! (semantics (amode_base_index_scale base index scale) (bvadd base (bvmul index scale)))
//! (semantics (x64.lea address) address)
//! ```
//!
//! Anything the solver already knows is not written down. Those are the [`BUILTIN`] heads, and
//! they are spelled the way SMT-LIB spells them except for the comparisons, where a rule writes
//! `<` and the solver wants `bvslt`.

use std::collections::HashMap;

use rucc_rules::{Error, Term, TermKind, parse_terms};

/// The heads the solver already understands, and what SMT-LIB calls them.
///
/// The comparisons are the signed ones. An unsigned comparison in a rule has to be written with
/// the solver's own name for it, which is deliberate: a rule that means the unsigned one should
/// have to say so rather than depend on which way this table happens to read.
const BUILTIN: [(&str, &str); 24] = [
    ("=", "="),
    ("and", "and"),
    ("or", "or"),
    ("not", "not"),
    ("<", "bvslt"),
    ("<=", "bvsle"),
    (">", "bvsgt"),
    (">=", "bvsge"),
    ("bvadd", "bvadd"),
    ("bvsub", "bvsub"),
    ("bvmul", "bvmul"),
    ("bvneg", "bvneg"),
    ("bvnot", "bvnot"),
    ("bvand", "bvand"),
    ("bvor", "bvor"),
    ("bvxor", "bvxor"),
    ("bvshl", "bvshl"),
    ("bvlshr", "bvlshr"),
    ("bvashr", "bvashr"),
    ("bvsdiv", "bvsdiv"),
    ("bvudiv", "bvudiv"),
    ("bvsrem", "bvsrem"),
    ("bvurem", "bvurem"),
    ("ite", "ite"),
];

/// What one head means.
#[derive(Debug, Clone)]
struct Meaning {
    /// The names the body is written in terms of.
    params: Vec<String>,
    /// What it computes.
    body: Term,
}

/// Everything the rules are allowed to say, and what each of it means.
#[derive(Debug, Default)]
pub struct Model {
    heads: HashMap<String, Meaning>,
}

impl Model {
    /// Read a model from text.
    ///
    /// # Errors
    ///
    /// Anything that is not a well formed `(semantics (head params) body)` form, and any head
    /// given a meaning twice.
    pub fn read(path: &str, text: &str) -> Result<Model, Vec<Error>> {
        let terms = parse_terms(path, text)?;
        let mut model = Model::default();
        let mut errors = Vec::new();

        for term in terms {
            let TermKind::App { head, args } = &term.kind else {
                errors.push(fail(path, &term, "expected a `(semantics ...)` form".to_owned()));
                continue;
            };
            if head != "semantics" || args.len() != 2 {
                errors.push(fail(path, &term, "expected a `(semantics ...)` form".to_owned()));
                continue;
            }
            let TermKind::App { head: name, args: params } = &args[0].kind else {
                errors.push(fail(path, &args[0], "expected a head and its parameters".to_owned()));
                continue;
            };
            let mut names = Vec::new();
            for param in params {
                match &param.kind {
                    TermKind::Var(name) => names.push(name.clone()),
                    _ => errors.push(fail(path, param, "a parameter has to be a name".to_owned())),
                }
            }
            if builtin(name).is_some() {
                let said = format!("`{name}` is something the solver already knows");
                errors.push(fail(path, &args[0], said));
                continue;
            }
            let meaning = Meaning { params: names, body: args[1].clone() };
            if model.heads.insert(name.clone(), meaning).is_some() {
                let said = format!("`{name}` is given a meaning twice");
                errors.push(fail(path, &args[0], said));
            }
        }

        if errors.is_empty() { Ok(model) } else { Err(errors) }
    }

    /// Write one term out as SMT-LIB, expanding everything the model defines.
    ///
    /// `width` is the bitvector width the rule works in, which is what a literal is written at.
    ///
    /// # Errors
    ///
    /// A head that is neither a builtin nor in the model, since that is a term nobody has said
    /// the meaning of, and an application of the wrong number of arguments.
    pub fn write(&self, path: &str, term: &Term, width: u32) -> Result<String, Error> {
        self.write_with(path, term, width, &HashMap::new())
    }

    fn write_with(
        &self,
        path: &str,
        term: &Term,
        width: u32,
        bound: &HashMap<&str, String>,
    ) -> Result<String, Error> {
        match &term.kind {
            TermKind::Var(name) => match bound.get(name.as_str()) {
                Some(already) => Ok(already.clone()),
                None => Ok(name.clone()),
            },
            TermKind::Int(value) => Ok(literal(*value, width)),
            TermKind::App { head, args } => {
                let mut written = Vec::with_capacity(args.len());
                for arg in args {
                    written.push(self.write_with(path, arg, width, bound)?);
                }
                if let Some(name) = builtin(head) {
                    if written.is_empty() {
                        return Err(fail(path, term, format!("`{head}` needs arguments")));
                    }
                    return Ok(format!("({name} {})", written.join(" ")));
                }
                let Some(meaning) = self.heads.get(head) else {
                    let said = format!("nothing in the model says what `{head}` means");
                    return Err(fail(path, term, said));
                };
                if meaning.params.len() != written.len() {
                    let said = format!(
                        "`{head}` means something with {} arguments and this gives it {}",
                        meaning.params.len(),
                        written.len()
                    );
                    return Err(fail(path, term, said));
                }
                let inner: HashMap<&str, String> =
                    meaning.params.iter().map(String::as_str).zip(written).collect();
                self.write_with(path, &meaning.body, width, &inner)
            }
        }
    }
}

/// What SMT-LIB calls this head, if it already knows it.
fn builtin(head: &str) -> Option<&'static str> {
    BUILTIN.iter().find(|(name, _)| *name == head).map(|(_, smt)| *smt)
}

/// A literal at the rule's width. Negative values are written as the bit pattern they are, since
/// SMT-LIB has no sign on a bitvector literal.
fn literal(value: i128, width: u32) -> String {
    let wrapped =
        if width >= 128 { value as u128 } else { (value as u128) & ((1u128 << width) - 1) };
    format!("(_ bv{wrapped} {width})")
}

fn fail(path: &str, term: &Term, message: String) -> Error {
    Error { path: path.to_owned(), line: term.line, column: term.column, message }
}
