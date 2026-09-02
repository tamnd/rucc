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
//!
//! # Widths
//!
//! Every term is some number of bits wide and [`Widths`] is what says how many. A head that ends
//! in `.iN` is N bits wide, anything else is as wide as the term it sits inside, and a name is as
//! wide as the place in the pattern that bound it. That is enough for a rule to convert between
//! widths, which is what `sext`, `zext` and `trunc` all are, and those conversions are written
//! the way `spec/10-backend.md` writes them: `(sign_extend 32 64 x)` and `(extract 31 0 x)`, with
//! the widths spelled out rather than left to be inferred.
//!
//! The widths are checked here rather than left to the solver, because a solver handed two
//! bitvectors of different sorts says so in its own words and at a place in generated text that
//! nobody wants to read.

use std::collections::{BTreeMap, HashMap};

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

/// The builtins that take a boolean somewhere, so their arguments are not all one width and
/// there is nothing to check between them.
const LOGICAL: [&str; 4] = ["and", "or", "not", "ite"];

/// The heads that change width. Their first two arguments are widths rather than values, which
/// is why they are written out here rather than sitting in [`BUILTIN`] with the rest: SMT-LIB
/// spells them as indexed operators and the index is a number this has to work out.
const CONVERSION: [&str; 3] = ["sign_extend", "zero_extend", "extract"];

/// What a rule works in when its opcode does not say. Every opcode in the IR does say, so this
/// is what a hand written test rule gets rather than something the real rule set relies on.
pub const DEFAULT_WIDTH: u32 = 64;

/// How wide each thing in one rule is.
///
/// A rule is written at one width, the one its pattern's opcode names, and the terms inside it
/// may say another: `(value.i64 x)` under an `add.i32` is a thirty two bit add of two sixty four
/// bit registers, which is the shape every `sext`, `zext` and `trunc` in a lowering has. What a
/// name stands at is fixed by the pattern, because the pattern is where a name is bound, and
/// everywhere else reads it from here.
///
/// A bounded proof asks the same rule at a narrower width, and that scales every width in the
/// rule by one ratio rather than flattening them all to one number. A rule that converts between
/// widths still converts between widths when it is asked at eight bits, which it would not do if
/// the narrow width were simply substituted everywhere.
#[derive(Debug, Clone, Default)]
pub struct Widths {
    /// The width the rule is written in.
    natural: u32,
    /// The width it is being asked at, which is the same number unless this is a bounded proof.
    asked: u32,
    /// What each name the pattern binds stands at, already scaled.
    at: BTreeMap<String, u32>,
}

impl Widths {
    /// The widths one rule's pattern fixes, at the width the rule is written in.
    #[must_use]
    pub fn of(pattern: &Term) -> Widths {
        Widths::at(pattern, rule_width(pattern))
    }

    /// The same, scaled to a width somebody asked for. This is what a bounded proof is made of.
    #[must_use]
    pub fn at(pattern: &Term, asked: u32) -> Widths {
        let natural = rule_width(pattern);
        let mut widths = Widths { natural, asked, at: BTreeMap::new() };
        widths.bind(pattern, asked);
        widths
    }

    /// The width a term is at when nothing inside it says otherwise.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.asked
    }

    /// The width the rule is written in, which is the one it will run at.
    #[must_use]
    pub fn natural(&self) -> u32 {
        self.natural
    }

    /// Every name the pattern binds and how wide it is, sorted.
    ///
    /// Sorted rather than in the order the pattern binds them, because the query is something a
    /// test pins and a diff is easier to read than it is to regenerate.
    pub fn names(&self) -> impl Iterator<Item = (&str, u32)> {
        self.at.iter().map(|(name, width)| (name.as_str(), *width))
    }

    /// These widths and one more name, which is how the replacement's own meaning gets a width
    /// once it has been substituted into the specification for `(result)`.
    #[must_use]
    pub fn with(&self, name: &str, width: u32) -> Widths {
        let mut out = self.clone();
        out.at.insert(name.to_owned(), width);
        out
    }

    /// How wide a name is, when the pattern bound it.
    fn of_name(&self, name: &str) -> Option<u32> {
        self.at.get(name).copied()
    }

    /// The width a head names, scaled.
    fn suffix(&self, head: &str) -> Option<u32> {
        declared(head).map(|width| self.scale(width))
    }

    /// A width, in the proportion the question is being asked at. Never nothing: a width that
    /// scales to zero bits is a width the rule cannot be asked about at all.
    fn scale(&self, width: u32) -> u32 {
        if self.asked == self.natural || self.natural == 0 {
            return width;
        }
        self.index(width).max(1)
    }

    /// A bit position, in the same proportion. Zero stays zero, which is what separates this
    /// from [`Widths::scale`].
    fn index(&self, position: u32) -> u32 {
        if self.asked == self.natural || self.natural == 0 {
            return position;
        }
        let scaled = u64::from(position) * u64::from(self.asked) / u64::from(self.natural);
        u32::try_from(scaled).unwrap_or(position)
    }

    /// Walk the pattern and write down what each name it binds stands at.
    fn bind(&mut self, term: &Term, context: u32) {
        match &term.kind {
            TermKind::Var(name) => {
                self.at.insert(name.clone(), context);
            }
            TermKind::Int(_) => {}
            TermKind::App { head, args } => {
                let inner = self.suffix(head).unwrap_or(context);
                for arg in args {
                    self.bind(arg, inner);
                }
            }
        }
    }
}

/// The width a rule works in, taken from the suffix on its pattern's opcode.
#[must_use]
pub fn rule_width(pattern: &Term) -> u32 {
    match &pattern.kind {
        TermKind::App { head, .. } => declared(head).unwrap_or(DEFAULT_WIDTH),
        _ => DEFAULT_WIDTH,
    }
}

/// The width a head names, if it names one. `add.i32` does and `x64.lea` does not.
fn declared(head: &str) -> Option<u32> {
    head.rsplit_once('.')
        .and_then(|(_, suffix)| suffix.strip_prefix('i'))
        .and_then(|bits| bits.parse::<u32>().ok())
}

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
            if known(name) {
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

    /// Write one term out as SMT-LIB, expanding everything the model defines, and say how wide
    /// what it computes is.
    ///
    /// # Errors
    ///
    /// A head that is neither a builtin nor in the model, since that is a term nobody has said
    /// the meaning of, an application of the wrong number of arguments, and anything whose
    /// widths do not fit together.
    pub fn write(&self, path: &str, term: &Term, widths: &Widths) -> Result<(String, u32), Error> {
        self.write_at(path, term, widths.width(), widths, &HashMap::new())
    }

    fn write_at(
        &self,
        path: &str,
        term: &Term,
        context: u32,
        widths: &Widths,
        bound: &HashMap<&str, (String, u32)>,
    ) -> Result<(String, u32), Error> {
        match &term.kind {
            TermKind::Var(name) => match bound.get(name.as_str()) {
                Some((already, width)) => Ok((already.clone(), *width)),
                None => Ok((name.clone(), widths.of_name(name).unwrap_or(context))),
            },
            TermKind::Int(value) => Ok((literal(*value, context), context)),
            TermKind::App { head, args } => {
                if CONVERSION.contains(&head.as_str()) {
                    return self.convert(path, term, head, args, context, widths, bound);
                }
                if let Some(name) = builtin(head) {
                    return self.combine(path, term, head, name, args, context, widths, bound);
                }
                let own = widths.suffix(head).unwrap_or(context);
                let mut written = Vec::with_capacity(args.len());
                for arg in args {
                    written.push(self.write_at(path, arg, own, widths, bound)?);
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
                let inner: HashMap<&str, (String, u32)> =
                    meaning.params.iter().map(String::as_str).zip(written).collect();
                let (text, width) = self.write_at(path, &meaning.body, own, widths, &inner)?;
                // An opcode that names a width has to mean something that wide. This is the
                // model being held to what the rules say about it: `add.i32` over registers
                // that are sixty four bits wide means an add of their low halves, and a model
                // that leaves the truncation out says so here rather than in a proof that
                // quietly asks the wrong question.
                if let Some(said) = widths.suffix(head) {
                    if said != width {
                        let told = format!(
                            "`{head}` is written for {said} bits and means something {width} \
                             bits wide"
                        );
                        return Err(fail(path, term, told));
                    }
                }
                Ok((text, width))
            }
        }
    }

    /// One of the heads the solver already knows, applied to arguments that all have to be the
    /// same width unless a boolean is involved.
    #[allow(clippy::too_many_arguments)]
    fn combine(
        &self,
        path: &str,
        term: &Term,
        head: &str,
        name: &str,
        args: &[Term],
        context: u32,
        widths: &Widths,
        bound: &HashMap<&str, (String, u32)>,
    ) -> Result<(String, u32), Error> {
        let mut written = Vec::with_capacity(args.len());
        for arg in args {
            written.push(self.write_at(path, arg, context, widths, bound)?);
        }
        let Some((_, first)) = written.first() else {
            return Err(fail(path, term, format!("`{head}` needs arguments")));
        };
        let first = *first;
        if !LOGICAL.contains(&head) {
            if let Some((_, other)) = written.iter().find(|(_, width)| *width != first) {
                let said = format!(
                    "`{head}` is given something {first} bits wide and something {other} bits \
                     wide, and those are not the same kind of thing"
                );
                return Err(fail(path, term, said));
            }
        }
        // A comparison computes a boolean and its width is nobody's business, so saying it is
        // as wide as what it compared costs nothing and keeps every term having an answer.
        let width = if head == "ite" && written.len() > 1 { written[1].1 } else { first };
        let texts: Vec<&str> = written.iter().map(|(text, _)| text.as_str()).collect();
        Ok((format!("({name} {})", texts.join(" ")), width))
    }

    /// A conversion between widths, written as `spec/10-backend.md` writes it, with the widths
    /// as arguments rather than inferred from anything.
    #[allow(clippy::too_many_arguments)]
    fn convert(
        &self,
        path: &str,
        term: &Term,
        head: &str,
        args: &[Term],
        context: u32,
        widths: &Widths,
        bound: &HashMap<&str, (String, u32)>,
    ) -> Result<(String, u32), Error> {
        if args.len() != 3 {
            let said = format!("`{head}` takes two numbers and a value, and this gives it {}", {
                args.len()
            });
            return Err(fail(path, term, said));
        }
        // Two numbers, and which two they are depends on the head: the bit positions an extract
        // takes, and the widths an extension goes between.
        let (first, second) = (number(path, head, &args[0])?, number(path, head, &args[1])?);

        if head == "extract" {
            let (high, low) = (first, second);
            if high < low {
                let said = format!("`extract` takes bits {high} down to {low}, which is none");
                return Err(fail(path, term, said));
            }
            let width = widths.scale(high - low + 1);
            let bottom = widths.index(low);
            let top = bottom + width - 1;
            let (text, of) = self.write_at(path, &args[2], context, widths, bound)?;
            if top >= of {
                let said = format!(
                    "`extract` takes bits {top} down to {bottom} of something {of} bits wide"
                );
                return Err(fail(path, term, said));
            }
            return Ok((format!("((_ extract {top} {bottom}) {text})"), width));
        }

        let (from, to) = (widths.scale(first), widths.scale(second));
        if to < from {
            let said = format!("`{head}` goes from {from} bits to {to}, which is narrower");
            return Err(fail(path, term, said));
        }
        let (text, of) = self.write_at(path, &args[2], from, widths, bound)?;
        if of != from {
            let said =
                format!("`{head}` goes from {from} bits and is given something {of} bits wide");
            return Err(fail(path, term, said));
        }
        // Extending by nothing is written as nothing rather than as an extension by zero,
        // because a bounded proof can scale two different widths onto the same one.
        if to == from {
            return Ok((text, to));
        }
        Ok((format!("((_ {head} {}) {text})", to - from), to))
    }
}

/// What SMT-LIB calls this head, if it already knows it.
fn builtin(head: &str) -> Option<&'static str> {
    BUILTIN.iter().find(|(name, _)| *name == head).map(|(_, smt)| *smt)
}

/// Whether the solver already knows this head, and so whether the model may not redefine it.
fn known(head: &str) -> bool {
    builtin(head).is_some() || CONVERSION.contains(&head)
}

/// One of the numbers a conversion is written with.
fn number(path: &str, head: &str, term: &Term) -> Result<u32, Error> {
    match &term.kind {
        TermKind::Int(value) => u32::try_from(*value).map_err(|_| {
            let said = format!("`{head}` is given {value} where it needs a number of bits");
            fail(path, term, said)
        }),
        _ => {
            let said = format!("`{head}` says which widths it goes between, in numbers");
            Err(fail(path, term, said))
        }
    }
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
