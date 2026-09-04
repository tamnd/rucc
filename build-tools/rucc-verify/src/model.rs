//! What the terms in a rule mean, in bitvectors and in floats.
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
//!
//! # Floats
//!
//! A head that ends in `.fN` is a float in the interchange format of that many bits, which is not
//! the bitvector of the same size and is not treated as one: adding two floats is not adding their
//! bits, and a rule that lowered one to the other would be caught here rather than proved. The
//! operations are the [`FLOAT`] heads and they are the ones the floating point standard defines,
//! written with the rounding this file supplies rather than one each rule repeats.
//!
//! A bounded proof does not narrow a float. The formats are the four the standard names rather
//! than a ratio, so a rule about a float is either proved in the format it runs in or not proved,
//! which is what every rule in the shipped set does anyway.
//!
//! The one place a float and the bitvector of the same size are the same thing is [`REINTERPRET`],
//! which is what a load and a store are: neither instruction looks at the bits it moves. Writing
//! that as a head of its own is what keeps it from being the default, so a rule that means to read
//! a float as its bits has to say so and every other way of putting the two together is still an
//! error.
//!
//! The other way across is [`CROSSING`], which is what a conversion instruction does: it reads a
//! number and writes the float nearest to it, or reads a float and writes the number it stands
//! for. Those two are as far from a reinterpretation as they could be, since neither keeps a
//! single bit, and they are written with both widths spelled out for the same reason `sign_extend`
//! is.
//!
//! # Memory
//!
//! A rule with an effect is a claim about memory as well as about a value, so not everything a
//! term computes is a bitvector and [`Sort`] is what says which it is. Memory is one map from an
//! address to a byte, written as an SMT-LIB array, and the three heads that touch it are
//! [`MEMORY`]: `(mem)` is the memory a rule starts from, `select` reads one byte of it and
//! `store` writes one.
//!
//! Nothing wider than a byte is built in, which is deliberate. A load of four bytes is four
//! `select`s put together with `concat` and a store of four bytes is four nested `store`s, both
//! written out in the model file, so the byte order is a thing a reviewer reads rather than a
//! thing this file decides on their behalf. That is the one fact about memory access that no
//! amount of testing on one machine will catch.

use std::collections::{BTreeMap, HashMap};

use rucc_rules::{Error, Term, TermKind, parse_terms};

/// The heads the solver already understands, and what SMT-LIB calls them.
///
/// The comparisons written as symbols are the signed ones. An unsigned comparison in a rule has
/// to be written with the solver's own name for it, which is deliberate: a rule that means the
/// unsigned one should have to say so rather than depend on which way this table happens to read.
/// Both families are here under those names as well, so a rule that would rather be explicit
/// about the signed one can be.
const BUILTIN: [(&str, &str); 32] = [
    ("=", "="),
    ("and", "and"),
    ("or", "or"),
    ("not", "not"),
    ("<", "bvslt"),
    ("<=", "bvsle"),
    (">", "bvsgt"),
    (">=", "bvsge"),
    ("bvslt", "bvslt"),
    ("bvsle", "bvsle"),
    ("bvsgt", "bvsgt"),
    ("bvsge", "bvsge"),
    ("bvult", "bvult"),
    ("bvule", "bvule"),
    ("bvugt", "bvugt"),
    ("bvuge", "bvuge"),
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

/// The heads that work in floats, and how many arguments each takes.
///
/// Not in [`BUILTIN`] because SMT-LIB's float arithmetic takes a rounding mode as its first
/// argument and a rule does not write one. The mode goes in here, once, rather than being a thing
/// every rule repeats and any rule can get wrong.
const FLOAT: [(&str, usize); 4] = [("fp.add", 2), ("fp.sub", 2), ("fp.mul", 2), ("fp.div", 2)];

/// The rounding the solver is told to do, which is the one a C program gets unless it asks for
/// another. `spec/12-abi-and-runtime.md` has the compiler assume the default environment, so the
/// mode a rule is proved under is the mode the program will run in.
const ROUNDING: &str = "RNE";

/// The rounding a conversion to an integer does, which is not [`ROUNDING`].
///
/// C says a float converted to an integer keeps the part before the point and discards the rest,
/// whatever the rounding mode is set to, and that is why the instruction is `cvttsd2si` with two
/// `t`s rather than `cvtsd2si`. A rule proved under the default rounding here would be a rule
/// proved about the instruction we do not select.
const TOWARDS_ZERO: &str = "RTZ";

/// The float formats SMT-LIB has a name for, which are the ones a rule may be written in: how
/// wide each is, then the bits of exponent and the bits of significand SMT-LIB names it by.
///
/// The significand counts the bit the format does not store, which is why the three numbers in a
/// row add up to one more than the width.
///
/// Eighty bit is not among them, and that is an answer rather than a gap: the x87 format is not
/// one of the interchange formats, `crates/rucc-codegen/src/abi.rs` refuses a `long double` on the
/// same grounds, and a rule about one would have to say what it means rather than borrow a name
/// from a standard that does not have it.
const FORMATS: [(u32, u32, u32); 4] = [(16, 5, 11), (32, 8, 24), (64, 11, 53), (128, 15, 113)];

/// The two heads that move between a float and the bits that spell it, which is what a load and a
/// store of one are: neither instruction looks at what it moves.
///
/// Two heads rather than one builtin because they go opposite ways and only one of them is in the
/// standard theory. Reading bits as a float is SMT-LIB's own `to_fp` on a bitvector. Reading a
/// float as its bits is not in the theory at all, and `fp.to_ieee_bv` is what a solver that has it
/// calls it, so the name a rule writes is this file's rather than the solver's for the same reason
/// a rule writes `<` and the query says `bvslt`.
const REINTERPRET: [&str; 2] = ["float_from_bits", "bits_from_float"];

/// The heads that go between a float and the number it stands for, which is the other thing an
/// instruction can do with the two and is the opposite of [`REINTERPRET`]: a conversion keeps the
/// value as far as it can and keeps no bit, and a reinterpretation keeps every bit and no value.
///
/// Each takes the width it comes from, the width it goes to, and the value, in that order, the way
/// `sign_extend` does. Which of the two widths is a float format and which is a number of bits is
/// what the name says, and it is checked rather than guessed: `float_from_signed` handed a float
/// is a rule that has left a conversion out.
///
/// Nothing unsigned is here. The machine has no instruction for it below a hundred and twenty
/// eight bit register, so an unsigned conversion is more than one instruction and belongs in a
/// pass that rewrites it into these rather than in a rule.
const CROSSING: [&str; 3] = ["float_from_float", "float_from_signed", "signed_from_float"];

/// The heads that change width. Their first two arguments are widths rather than values, which
/// is why they are written out here rather than sitting in [`BUILTIN`] with the rest: SMT-LIB
/// spells them as indexed operators and the index is a number this has to work out.
const CONVERSION: [&str; 3] = ["sign_extend", "zero_extend", "extract"];

/// The heads that touch memory, which are not in [`BUILTIN`] because their arguments are not all
/// the same sort and their results are not all the same sort either.
const MEMORY: [&str; 3] = ["mem", "select", "store"];

/// Putting bitvectors end to end, which is how a load of more than one byte is written. Not in
/// [`BUILTIN`] because its arguments are one width and its result is their total.
const CONCAT: &str = "concat";

/// How wide an address is.
///
/// Every target `spec/12-abi-and-runtime.md` implements for 1.0 is sixty four bit, so this is a
/// constant rather than something the model file says. When a thirty two bit target arrives it
/// becomes something the model file says, and the rules that read memory will be the ones that
/// notice.
pub const ADDRESS_WIDTH: u32 = 64;

/// How wide a byte is, which is the element of memory.
pub const BYTE_WIDTH: u32 = 8;

/// What the memory a rule starts from is called in the query.
///
/// A name no rule can bind, because a name in a rule comes out of a pattern and a pattern binds
/// what the selector matched, which is registers and constants and never memory.
pub const MEMORY_CONST: &str = "mem";

/// What kind of thing a term computes.
///
/// Most things are a bitvector, and the two exceptions are the whole point of this type. A rule
/// with an effect relates one memory to another, and a memory is not a number however many bits
/// one is willing to spend on it. A rule about a float relates two floats, and a float is not the
/// number its bits spell either, however much it looks like one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// A bitvector this many bits wide.
    Bits(u32),
    /// A float in the interchange format of this many bits, which is a different kind of thing
    /// from the bitvector of the same size: adding two of them is not adding their bits.
    Float(u32),
    /// The whole of memory, a map from an address to a byte.
    Memory,
}

impl Sort {
    /// How many bits wide it is, or nothing when it is not a bitvector at all.
    ///
    /// A float is not one. Everything that asks this is about to take an extract of it or put it
    /// end to end with something, and neither is a thing to do to a float without saying so.
    #[must_use]
    pub fn bits(self) -> Option<u32> {
        match self {
            Sort::Bits(width) => Some(width),
            Sort::Float(_) | Sort::Memory => None,
        }
    }

    /// What SMT-LIB calls it, at the widths this question is being asked at.
    #[must_use]
    pub fn write(self, widths: &Widths) -> String {
        match self {
            Sort::Bits(width) => format!("(_ BitVec {width})"),
            // `Float32` and the rest are the names the standard gives the interchange formats,
            // and [`FLOAT_WIDTHS`] is what keeps this from being asked for a format it has no
            // name for.
            Sort::Float(width) => format!("Float{width}"),
            Sort::Memory => {
                format!("(Array (_ BitVec {}) (_ BitVec {}))", widths.address(), widths.byte())
            }
        }
    }

    /// How it reads in a message to somebody who has written a rule that does not fit together.
    pub(crate) fn describe(self) -> String {
        match self {
            Sort::Bits(width) => format!("{width} bits wide"),
            Sort::Float(width) => format!("{width} bits of float"),
            Sort::Memory => "the whole of memory".to_owned(),
        }
    }
}

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
    at: BTreeMap<String, Sort>,
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
        widths.bind(pattern, Sort::Bits(asked));
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

    /// Every name the pattern binds and what kind of thing it is, sorted.
    ///
    /// Sorted rather than in the order the pattern binds them, because the query is something a
    /// test pins and a diff is easier to read than it is to regenerate.
    ///
    /// A memory is not among them. Nothing in a pattern binds one, because a name in a rule comes
    /// out of what the selector matched and that is registers and constants.
    pub fn names(&self) -> impl Iterator<Item = (&str, Sort)> {
        self.at
            .iter()
            .filter(|(_, sort)| **sort != Sort::Memory)
            .map(|(name, sort)| (name.as_str(), *sort))
    }

    /// These widths and one more name, which is how the replacement's own meaning gets a width
    /// once it has been substituted into the specification for `(result)`.
    ///
    /// A replacement that computes a memory is recorded as one, so that the specification which
    /// reads it back is checked against a memory rather than against a number of bits nobody
    /// meant.
    #[must_use]
    pub fn with(&self, name: &str, sort: Sort) -> Widths {
        let mut out = self.clone();
        out.at.insert(name.to_owned(), sort);
        out
    }

    /// How wide an address is here, scaled like everything else.
    #[must_use]
    pub fn address(&self) -> u32 {
        self.scale(ADDRESS_WIDTH)
    }

    /// How wide a byte is here, scaled like everything else.
    ///
    /// A bounded proof asks a rule in narrower bitvectors, and a byte narrows with them. It has
    /// to: the bytes a load puts together have to add up to the value the load produces, and a
    /// value that has been scaled and bytes that have not do not add up to anything.
    #[must_use]
    pub fn byte(&self) -> u32 {
        self.scale(BYTE_WIDTH)
    }

    /// What a name stands for, when the pattern bound it.
    fn of_name(&self, name: &str) -> Option<Sort> {
        self.at.get(name).copied()
    }

    /// The kind of thing a head names, scaled.
    ///
    /// A float is not scaled. There is no narrower float to scale to: the formats are the four
    /// the standard names and they are not a ratio of each other, so a bounded proof of a rule
    /// about a float asks about the format the rule runs in. That gives up nothing, because the
    /// claims that need a bounded proof are the ones about wide multiplication and division of
    /// bitvectors.
    fn sort_of(&self, head: &str) -> Option<Sort> {
        match declared(head)? {
            Sort::Bits(width) => Some(Sort::Bits(self.scale(width))),
            other => Some(other),
        }
    }

    /// The width a head names, when it names a number of bits rather than a float.
    fn suffix(&self, head: &str) -> Option<u32> {
        self.sort_of(head).and_then(Sort::bits)
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

    /// Walk the pattern and write down what each name it binds stands for.
    fn bind(&mut self, term: &Term, context: Sort) {
        match &term.kind {
            TermKind::Var(name) => {
                self.at.insert(name.clone(), context);
            }
            TermKind::Int(_) => {}
            TermKind::App { head, args } => {
                let inner = self.sort_of(head).unwrap_or(context);
                for arg in args {
                    self.bind(arg, inner);
                }
            }
        }
    }
}

/// The width a rule works in, taken from the suffix on its pattern's opcode.
///
/// A float rule works in the width of its format, which is the number in the suffix as well.
/// Nothing scales it, so the only thing that number does for a float rule is stand as the width
/// any integer term inside it takes when nothing says otherwise.
#[must_use]
pub fn rule_width(pattern: &Term) -> u32 {
    let TermKind::App { head, .. } = &pattern.kind else {
        return DEFAULT_WIDTH;
    };
    match declared(head) {
        Some(Sort::Bits(width) | Sort::Float(width)) => width,
        Some(Sort::Memory) | None => DEFAULT_WIDTH,
    }
}

/// The kind of thing a head names, if it names one. `add.i32` names a bitvector, `fadd.f32` names
/// a float, and `x64.lea` names neither.
fn declared(head: &str) -> Option<Sort> {
    let (_, suffix) = head.rsplit_once('.')?;
    let number = |kind: char| suffix.strip_prefix(kind).and_then(|bits| bits.parse::<u32>().ok());
    if let Some(bits) = number('i') {
        return Some(Sort::Bits(bits));
    }
    let bits = number('f')?;
    format_of(bits).map(|_| Sort::Float(bits))
}

/// The two numbers SMT-LIB names a float format by, if that width is one of the formats it names.
fn format_of(width: u32) -> Option<(u32, u32)> {
    FORMATS
        .iter()
        .find(|(bits, _, _)| *bits == width)
        .map(|(_, exponent, significand)| (*exponent, *significand))
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
    pub fn write(&self, path: &str, term: &Term, widths: &Widths) -> Result<(String, Sort), Error> {
        self.write_at(path, term, widths.width(), widths, &HashMap::new())
    }

    /// Whether reading this term reaches memory, following every head the model defines.
    ///
    /// A rule that reads memory needs a solver told about arrays and a constant to stand for the
    /// memory it starts from, and neither is worth putting in a query that does not. Nothing in a
    /// rule says `(mem)` directly: a load says `load.i32`, and it is the model entry for that head
    /// which reaches memory, so this expands what the model says rather than reading the surface.
    #[must_use]
    pub fn touches_memory(&self, term: &Term) -> bool {
        match &term.kind {
            TermKind::Var(_) | TermKind::Int(_) => false,
            TermKind::App { head, args } => {
                if MEMORY.contains(&head.as_str()) {
                    return true;
                }
                if args.iter().any(|arg| self.touches_memory(arg)) {
                    return true;
                }
                self.heads.get(head).is_some_and(|meaning| self.touches_memory(&meaning.body))
            }
        }
    }

    /// Whether reading this term reaches a float, following every head the model defines.
    ///
    /// A rule that does needs a solver told about floats, and a solver told about floats is
    /// slower at every rule that has none, so the question is worth asking rather than answering
    /// yes for the whole file. A head is a float either by its own suffix, as `fadd.f32` is, or
    /// by what the model says it means.
    #[must_use]
    pub fn touches_floats(&self, term: &Term) -> bool {
        match &term.kind {
            TermKind::Var(_) | TermKind::Int(_) => false,
            TermKind::App { head, args } => {
                if float_op(head).is_some() || matches!(declared(head), Some(Sort::Float(_))) {
                    return true;
                }
                if REINTERPRET.contains(&head.as_str()) || CROSSING.contains(&head.as_str()) {
                    return true;
                }
                if args.iter().any(|arg| self.touches_floats(arg)) {
                    return true;
                }
                self.heads.get(head).is_some_and(|meaning| self.touches_floats(&meaning.body))
            }
        }
    }

    fn write_at(
        &self,
        path: &str,
        term: &Term,
        context: u32,
        widths: &Widths,
        bound: &HashMap<&str, (String, Sort)>,
    ) -> Result<(String, Sort), Error> {
        match &term.kind {
            TermKind::Var(name) => match bound.get(name.as_str()) {
                Some((already, sort)) => Ok((already.clone(), *sort)),
                None => Ok((name.clone(), widths.of_name(name).unwrap_or(Sort::Bits(context)))),
            },
            TermKind::Int(value) => Ok((literal(*value, context), Sort::Bits(context))),
            TermKind::App { head, args } => {
                if CONVERSION.contains(&head.as_str()) {
                    return self.convert(path, term, head, args, context, widths, bound);
                }
                if MEMORY.contains(&head.as_str()) {
                    return self.reach(path, term, head, args, context, widths, bound);
                }
                if head == CONCAT {
                    return self.join(path, term, args, context, widths, bound);
                }
                if let Some(name) = builtin(head) {
                    return self.combine(path, term, head, name, args, context, widths, bound);
                }
                if let Some(takes) = float_op(head) {
                    return self.rounded(path, term, head, takes, args, context, widths, bound);
                }
                if REINTERPRET.contains(&head.as_str()) {
                    return self.reinterpret(path, term, head, args, widths, bound);
                }
                if CROSSING.contains(&head.as_str()) {
                    return self.crossing(path, term, head, args, widths, bound);
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
                let inner: HashMap<&str, (String, Sort)> =
                    meaning.params.iter().map(String::as_str).zip(written).collect();
                let (text, sort) = self.write_at(path, &meaning.body, own, widths, &inner)?;
                // An opcode that names a width has to mean something that wide. This is the
                // model being held to what the rules say about it: `add.i32` over registers
                // that are sixty four bits wide means an add of their low halves, and a model
                // that leaves the truncation out says so here rather than in a proof that
                // quietly asks the wrong question.
                //
                // A head that means a memory is the one exception, and it is not a hole. The
                // width on `store.i32` is the width of what it wrote rather than of what it
                // computes, and that width is checked all the same, by the extracts in the
                // model entry having to come out of something that wide.
                if let Some(said) = widths.sort_of(head).filter(|_| sort != Sort::Memory) {
                    let agrees = match (said, sort) {
                        (Sort::Bits(a), Sort::Bits(b)) | (Sort::Float(a), Sort::Float(b)) => a == b,
                        _ => false,
                    };
                    if !agrees {
                        let told = match (said, sort) {
                            (Sort::Bits(said), Sort::Bits(width)) => format!(
                                "`{head}` is written for {said} bits and means something {width} \
                                 bits wide"
                            ),
                            _ => format!(
                                "`{head}` is written for something {} and means something {}",
                                said.describe(),
                                sort.describe()
                            ),
                        };
                        return Err(fail(path, term, told));
                    }
                }
                Ok((text, sort))
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
        bound: &HashMap<&str, (String, Sort)>,
    ) -> Result<(String, Sort), Error> {
        // A number has no width of its own and takes the width of what it sits beside. Every
        // rule written before memory arrived had one width throughout, so this changed nothing
        // for them, and it is what lets an offset added to an address in the model be as wide as
        // the address rather than as wide as the value being loaded through it.
        //
        // Not under a head that takes a boolean. What a number sits beside there is a
        // comparison, and a comparison has no width to lend: the one and the zero an `ite`
        // chooses between are as wide as the term the `ite` is in, which is what `context` is.
        let beside = if LOGICAL.contains(&head) {
            context
        } else {
            self.beside(path, args, context, widths, bound)?
        };
        let mut written = Vec::with_capacity(args.len());
        for arg in args {
            let at = if matches!(arg.kind, TermKind::Int(_)) { beside } else { context };
            written.push(self.write_at(path, arg, at, widths, bound)?);
        }
        let Some((_, first)) = written.first() else {
            return Err(fail(path, term, format!("`{head}` needs arguments")));
        };
        let first = *first;
        if !LOGICAL.contains(&head) {
            // A head the solver spells with `bv` is arithmetic on bits, and handing it a float
            // is the mistake a rule makes when it lowers float arithmetic to an integer
            // instruction. The two are the same number of bits and nothing else about them is
            // the same, so this is caught here rather than left to come back as a proof.
            if name.starts_with("bv") && !matches!(first, Sort::Bits(_)) {
                let said = format!("`{head}` works on bitvectors and this is {}", first.describe());
                return Err(fail(path, term, said));
            }
            if let Some((_, other)) = written.iter().find(|(_, sort)| *sort != first) {
                let said = format!(
                    "`{head}` is given something {} and something {}, and those are not the \
                     same kind of thing",
                    first.describe(),
                    other.describe()
                );
                return Err(fail(path, term, said));
            }
        }
        // A comparison computes a boolean and its width is nobody's business, so saying it is
        // as wide as what it compared costs nothing and keeps every term having an answer.
        let sort = if head == "ite" && written.len() > 1 { written[1].1 } else { first };
        let texts: Vec<&str> = written.iter().map(|(text, _)| text.as_str()).collect();
        Ok((format!("({name} {})", texts.join(" ")), sort))
    }

    /// One of the float operations, whose arguments are all one format and whose result is that
    /// format, with the rounding written in on the rule's behalf.
    ///
    /// A number is not one of the things this takes. There is no reading of a bitvector literal
    /// as a float that does not have to say which reading it is, so a rule that wants a constant
    /// float says so with a head of its own rather than by writing a number here.
    #[allow(clippy::too_many_arguments)]
    fn rounded(
        &self,
        path: &str,
        term: &Term,
        head: &str,
        takes: usize,
        args: &[Term],
        context: u32,
        widths: &Widths,
        bound: &HashMap<&str, (String, Sort)>,
    ) -> Result<(String, Sort), Error> {
        if args.len() != takes {
            let said = format!("`{head}` takes {takes} arguments and this gives it {}", args.len());
            return Err(fail(path, term, said));
        }
        let mut written = Vec::with_capacity(args.len());
        for arg in args {
            written.push(self.write_at(path, arg, context, widths, bound)?);
        }
        let first = written[0].1;
        if !matches!(first, Sort::Float(_)) {
            let said = format!("`{head}` works on floats and this is {}", first.describe());
            return Err(fail(path, &args[0], said));
        }
        if let Some((_, other)) = written.iter().find(|(_, sort)| *sort != first) {
            let said = format!(
                "`{head}` is given something {} and something {}, and those are not the same \
                 kind of thing",
                first.describe(),
                other.describe()
            );
            return Err(fail(path, term, said));
        }
        let texts: Vec<&str> = written.iter().map(|(text, _)| text.as_str()).collect();
        Ok((format!("({head} {ROUNDING} {})", texts.join(" ")), first))
    }

    /// A float read as the bits that spell it, or the bits read back as the float, which is the
    /// one place here where the two are the same thing.
    ///
    /// The format is written out rather than taken from what is inside, for the reason
    /// `spec/10-backend.md` gives about every other conversion: a rule that changes what kind of
    /// thing it is holding should say what it is changing it into, and a reader should not have
    /// to work out the answer from somewhere else in the term.
    ///
    /// Nothing here is scaled. A float format is one of four the standard names rather than a
    /// ratio, so the bits that spell one are as fixed as the format is, and a bounded proof of a
    /// rule that read memory into a float would scale the bytes, leave the format alone and be
    /// told the two no longer fit.
    fn reinterpret(
        &self,
        path: &str,
        term: &Term,
        head: &str,
        args: &[Term],
        widths: &Widths,
        bound: &HashMap<&str, (String, Sort)>,
    ) -> Result<(String, Sort), Error> {
        if args.len() != 2 {
            let said =
                format!("`{head}` takes a format and a value, and this gives it {}", args.len());
            return Err(fail(path, term, said));
        }
        let width = number(path, head, &args[0])?;
        let Some((exponent, significand)) = format_of(width) else {
            let said = format!("`{head}` is written at {width} bits, which is not a float format");
            return Err(fail(path, term, said));
        };
        let into_float = head == "float_from_bits";
        let (text, sort) = self.write_at(path, &args[1], width, widths, bound)?;
        let wanted = if into_float { Sort::Bits(width) } else { Sort::Float(width) };
        if sort != wanted {
            let said = format!(
                "`{head}` takes something {} and this is {}",
                wanted.describe(),
                sort.describe()
            );
            return Err(fail(path, &args[1], said));
        }
        if into_float {
            // SMT-LIB's own operator, whose one bitvector argument is the reading that changes
            // no bits. The other readings of `to_fp` take a rounding mode and a value, and this
            // is not one of them.
            let said = format!("((_ to_fp {exponent} {significand}) {text})");
            return Ok((said, Sort::Float(width)));
        }
        Ok((format!("(fp.to_ieee_bv {text})"), Sort::Bits(width)))
    }

    /// A value carried from one format to another, or between a float and the number it stands
    /// for, which is what the conversion instructions do.
    ///
    /// The rounding is not the same on the way in as on the way out. Going to a float rounds to
    /// nearest, which is the mode a C program runs in unless it asks for another. Going to an
    /// integer cuts towards zero whatever the mode says, because that is what C means by the
    /// conversion and it is why the instruction has two `t`s in its name.
    ///
    /// A float too big for the integer it is asked for has no answer here, and that is right
    /// rather than missing. SMT-LIB leaves `fp.to_sbv` unspecified outside the range, C leaves the
    /// conversion undefined there, and the machine writes a value of its own choosing. A rule
    /// about one is proved for every float the conversion is defined for and claims nothing about
    /// the rest, which is the strongest true claim there is.
    fn crossing(
        &self,
        path: &str,
        term: &Term,
        head: &str,
        args: &[Term],
        widths: &Widths,
        bound: &HashMap<&str, (String, Sort)>,
    ) -> Result<(String, Sort), Error> {
        if args.len() != 3 {
            let said = format!(
                "`{head}` takes the width it comes from, the width it goes to and a value, and \
                 this gives it {}",
                args.len()
            );
            return Err(fail(path, term, said));
        }
        let (first, second) = (number(path, head, &args[0])?, number(path, head, &args[1])?);
        let from_float = head != "float_from_signed";
        let into_float = head != "signed_from_float";
        let float_format = |width: u32| {
            format_of(width).ok_or_else(|| {
                let said = format!("`{head}` is written at {width} bits, which is not a format");
                fail(path, term, said)
            })
        };

        // The float side is written at the width the format is, since a format is one of four the
        // standard names rather than a ratio of anything. The number side scales the way every
        // other bitvector in a bounded proof does, so a rule asked at a narrower width is a rule
        // about converting to a narrower integer and is still a rule about a conversion.
        let from = if from_float { first } else { widths.scale(first) };
        let to = if into_float { second } else { widths.scale(second) };
        let wanted = if from_float {
            float_format(from)?;
            Sort::Float(from)
        } else {
            Sort::Bits(from)
        };
        let (text, sort) = self.write_at(path, &args[2], from, widths, bound)?;
        if sort != wanted {
            let said = format!(
                "`{head}` takes something {} and this is {}",
                wanted.describe(),
                sort.describe()
            );
            return Err(fail(path, &args[2], said));
        }
        if into_float {
            let (exponent, significand) = float_format(to)?;
            let said = format!("((_ to_fp {exponent} {significand}) {ROUNDING} {text})");
            return Ok((said, Sort::Float(to)));
        }
        Ok((format!("((_ fp.to_sbv {to}) {TOWARDS_ZERO} {text})"), Sort::Bits(to)))
    }

    /// The width the numbers among a head's arguments should take, which is the width of the
    /// first argument that has one of its own. Nothing when they are all numbers, in which case
    /// the surrounding width is as good an answer as there is.
    fn beside(
        &self,
        path: &str,
        args: &[Term],
        context: u32,
        widths: &Widths,
        bound: &HashMap<&str, (String, Sort)>,
    ) -> Result<u32, Error> {
        if !args.iter().any(|arg| matches!(arg.kind, TermKind::Int(_))) {
            return Ok(context);
        }
        let Some(sized) = args.iter().find(|arg| !matches!(arg.kind, TermKind::Int(_))) else {
            return Ok(context);
        };
        let (_, sort) = self.write_at(path, sized, context, widths, bound)?;
        Ok(sort.bits().unwrap_or(context))
    }

    /// One of the three heads that touch memory.
    #[allow(clippy::too_many_arguments)]
    fn reach(
        &self,
        path: &str,
        term: &Term,
        head: &str,
        args: &[Term],
        context: u32,
        widths: &Widths,
        bound: &HashMap<&str, (String, Sort)>,
    ) -> Result<(String, Sort), Error> {
        // The memory a rule starts from, which is one constant and takes no arguments. It is
        // written `(mem)` for the reason `(result)` is: a head applied to nothing is still an
        // application, because a bare name is a variable.
        if head == "mem" {
            if !args.is_empty() {
                let said = "`mem` is the memory a rule starts from and takes nothing".to_owned();
                return Err(fail(path, term, said));
            }
            return Ok((MEMORY_CONST.to_owned(), Sort::Memory));
        }

        let wanted = if head == "select" { 2 } else { 3 };
        if args.len() != wanted {
            let said =
                format!("`{head}` takes {wanted} arguments and this gives it {}", args.len());
            return Err(fail(path, term, said));
        }
        let mut written = Vec::with_capacity(args.len());
        for arg in args {
            let at = if matches!(arg.kind, TermKind::Int(_)) { widths.address() } else { context };
            written.push(self.write_at(path, arg, at, widths, bound)?);
        }
        // The sorts of the three positions, which is the whole of what an array is: a memory, an
        // address into it, and for a store the byte that goes there.
        let expected = [Sort::Memory, Sort::Bits(widths.address()), Sort::Bits(widths.byte())];
        for (at, (_, got)) in written.iter().enumerate() {
            if *got != expected[at] {
                let said = format!(
                    "`{head}` takes something {} in position {at} and this is {}",
                    expected[at].describe(),
                    got.describe()
                );
                return Err(fail(path, term, said));
            }
        }
        let texts: Vec<&str> = written.iter().map(|(text, _)| text.as_str()).collect();
        let sort = if head == "select" { Sort::Bits(widths.byte()) } else { Sort::Memory };
        Ok((format!("({head} {})", texts.join(" ")), sort))
    }

    /// Bitvectors end to end, which is as wide as all of them together.
    ///
    /// The first argument is the high end, which is how SMT-LIB reads it and is the opposite of
    /// the order the bytes of a little endian load are at in memory. That is why a load in the
    /// model file counts down.
    fn join(
        &self,
        path: &str,
        term: &Term,
        args: &[Term],
        context: u32,
        widths: &Widths,
        bound: &HashMap<&str, (String, Sort)>,
    ) -> Result<(String, Sort), Error> {
        if args.len() < 2 {
            let said = format!("`concat` puts two or more things together and this gives it {}", {
                args.len()
            });
            return Err(fail(path, term, said));
        }
        let mut total = 0;
        let mut texts = Vec::with_capacity(args.len());
        for arg in args {
            let (text, sort) = self.write_at(path, arg, context, widths, bound)?;
            let Some(width) = sort.bits() else {
                let said = "`concat` puts bitvectors together and this is a memory".to_owned();
                return Err(fail(path, arg, said));
            };
            total += width;
            texts.push(text);
        }
        Ok((format!("(concat {})", texts.join(" ")), Sort::Bits(total)))
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
        bound: &HashMap<&str, (String, Sort)>,
    ) -> Result<(String, Sort), Error> {
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
            let (text, sort) = self.write_at(path, &args[2], context, widths, bound)?;
            let of = bits(path, head, &args[2], sort)?;
            if top >= of {
                let said = format!(
                    "`extract` takes bits {top} down to {bottom} of something {of} bits wide"
                );
                return Err(fail(path, term, said));
            }
            return Ok((format!("((_ extract {top} {bottom}) {text})"), Sort::Bits(width)));
        }

        let (from, to) = (widths.scale(first), widths.scale(second));
        if to < from {
            let said = format!("`{head}` goes from {from} bits to {to}, which is narrower");
            return Err(fail(path, term, said));
        }
        let (text, sort) = self.write_at(path, &args[2], from, widths, bound)?;
        let of = bits(path, head, &args[2], sort)?;
        if of != from {
            let said =
                format!("`{head}` goes from {from} bits and is given something {of} bits wide");
            return Err(fail(path, term, said));
        }
        // Extending by nothing is written as nothing rather than as an extension by zero,
        // because a bounded proof can scale two different widths onto the same one.
        if to == from {
            return Ok((text, Sort::Bits(to)));
        }
        Ok((format!("((_ {head} {}) {text})", to - from), Sort::Bits(to)))
    }
}

/// What SMT-LIB calls this head, if it already knows it.
fn builtin(head: &str) -> Option<&'static str> {
    BUILTIN.iter().find(|(name, _)| *name == head).map(|(_, smt)| *smt)
}

/// How many arguments this float operation takes, if it is one.
fn float_op(head: &str) -> Option<usize> {
    FLOAT.iter().find(|(name, _)| *name == head).map(|(_, takes)| *takes)
}

/// Whether the solver already knows this head, and so whether the model may not redefine it.
fn known(head: &str) -> bool {
    builtin(head).is_some()
        || float_op(head).is_some()
        || REINTERPRET.contains(&head)
        || CROSSING.contains(&head)
        || CONVERSION.contains(&head)
        || MEMORY.contains(&head)
        || head == CONCAT
}

/// How wide something is, when it has to be a bitvector and the rule is wrong if it is not.
fn bits(path: &str, head: &str, term: &Term, sort: Sort) -> Result<u32, Error> {
    sort.bits().ok_or_else(|| {
        let said = format!("`{head}` works on bitvectors and this is {}", sort.describe());
        fail(path, term, said)
    })
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
