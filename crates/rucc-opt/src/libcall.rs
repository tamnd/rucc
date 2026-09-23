//! A call to the library, folded into the call it really is.
//!
//! Section 20.2 of `spec/optimizer/20-idioms-and-libcalls.md`, the half of it that is about a
//! library call rather than about arithmetic. `printf("hello world\n")` writes the same bytes as
//! `puts("hello world")`, and the second one does not read a format string at run time, so gcc
//! rewrites it and has done since 2000. A program that checks which of the two it was left with,
//! which is what `gcc.c-torture/execute/builtins/printf.c` does by defining a `printf` of its own
//! that aborts, fails outright on a compiler that leaves the call alone. tamnd/rucc#1636 is that
//! program and the two beside it.
//!
//! # The rules
//!
//! All of them measured against gcc 16.2.0 on x86-64 rather than read out of its source, and all of
//! them conditional on the format being a string this module holds the bytes of.
//!
//! `strstr(s, "")` is `s`, since the empty string is found at once wherever it is looked for.
//! `strstr(s, "w")` is `strchr(s, 'w')`, which is a search for a character rather than for a string
//! and is worth doing wherever the haystack came from. `strstr` of two strings this module holds is
//! the answer itself, which is a place in the haystack or a null pointer, and nothing is called.
//!
//! `strlen`, `strnlen`, `strcmp`, `strncmp`, `strchr`, `strrchr`, `memchr`, `strspn`, `strcspn` and
//! `strpbrk` of strings this module holds are the answer itself in the same way, which is a number
//! for the first four and the last two of the six that search, and a place in the first argument or
//! a null pointer for the other ones. The number goes in with the width the call was declared to
//! give back, because a program that declared `strlen` as something returning an `int` is a program
//! whose reader of that answer reads an `int`.
//!
//! Three of them have a rule about the shape rather than about the bytes. `strpbrk(s, "")` is a
//! null pointer and `strspn(s, "")` is zero, since nothing at all is in an empty set, and
//! `strcspn(s, "")` is `strlen(s)` for the same reason read the other way. `strpbrk(s, "c")` is
//! `strchr(s, 'c')`, which is the `strstr` rule again for a set of one character instead of a
//! needle of one.
//!
//! Two of them are told how far to read rather than going looking for a terminator, and those two
//! read the object's bytes rather than the string in it. `memchr(s, c, n)` needs the object to have
//! `n` bytes from `s` on, and it is refused where it does not, since a call reading past the end of
//! what the compiler can see is a call whose answer the compiler does not know. `strnlen(s, n)` is
//! the count where nothing terminated the string inside it.
//!
//! `printf` with the format alone: nothing at all when it is empty, `putchar` when it is one
//! character, and `puts` of the format without its last character when the format holds no `%` and
//! ends in a newline. `printf("%s\n", p)` is `puts(p)` and `printf("%c", c)` is `putchar(c)`,
//! whatever `p` and `c` are. `printf("%s", p)` where `p` is a string this module holds is the same
//! question again asked of that string, and where it is not, the call stays: `printf` has no stream
//! argument to hand to `fputs`, and `stdout` is not a name a compiler may invent.
//!
//! `fprintf` is the same list with a stream in hand, so the case `printf` cannot take is the case
//! this one can. A format holding no `%` becomes `fputc` of its one character or `fwrite` of the
//! whole of it, `fprintf(s, "%c", c)` becomes `fputc(c, s)`, and `fprintf(s, "%s", p)` becomes
//! `fputs(p, s)` however little is known about `p`.
//!
//! `fputs(p, s)` needs the length of `p` and nothing else. Zero is nothing at all, one is `fputc`
//! when the character is known as well, and anything longer is `fwrite(p, 1, len, s)`.
//!
//! The `_unlocked` spellings get the one fold that names no function, which is that a call writing
//! nothing is removed. gcc stops in exactly the same place and the reason is in the torture
//! program's own comment: a system need not have a `puts_unlocked` for the compiler to name.
//!
//! The checking spellings `_FORTIFY_SOURCE` writes, `__memcpy_chk` and the thirteen beside it,
//! carry the size of the destination as one more argument and abort when the call would not fit.
//! Where that size is all ones, which is `__builtin_object_size` not knowing, or where what the
//! call writes is known to fit, the check cannot fail and the call is the plain one without it.
//! Where it can fail the call may still get cheaper: a checking `stpcpy` whose answer nothing reads
//! is a checking `strcpy`, and a checking `strcpy` of a known string is a checking `memcpy`. An
//! append of nothing is the destination. The plain name has to be one the module does not declare
//! with some other shape, and a call made plain is looked at again, up to three times.
//!
//! # What a call has to be
//!
//! For the printf family, its result has to be read by nothing. `printf` answers the number of
//! characters written and `puts` answers a non-negative number that is not that count, so a program
//! looking at the answer is a program this may not touch. The str and mem families are the other
//! way round: the answer is the whole point of the call and the fold produces it, so a program
//! reading it is the ordinary case.
//!
//! It has to give back one value of the kind its name says it does. A program that declared
//! `strchr` as something returning two values, or a number, declared a function of its own and a
//! pointer into a string literal is not what it answers.
//!
//! The name has to be the one the source spelled rather than the one the object file will carry.
//! `extern char *strstr (const char *, const char *) __asm ("my_strstr");` is a declaration of
//! `strstr`, and a compiler that reads the symbol alone sees a call to a function it knows nothing
//! about. So the callee is looked up through [`rucc_ir::Func::spelled`], and a call this leaves
//! behind is a call to whatever symbol the module says that name has, which is the rename again
//! read from the other end.
//!
//! The name has to be one this module does not define. A translation unit holding the body of its
//! own `fputs` means that body, which is the rule [`crate::heap`] applies to `malloc` and for the
//! same reason.
//!
//! The function must not carry memory SSA yet, which where this runs it does not. Memory is
//! threaded by [`crate::number`], that pass is in the function pipeline, and this runs before the
//! pipeline starts. The check is here anyway, because a call with a memory operand rewritten into
//! one without would be a use of a value nothing defines.
//!
//! # Where it runs, and why it is not a rule
//!
//! Section 20.2 asks for folds like these to be rules in the rewrite DSL with the callee's identity
//! in the pattern, and most of them can be. These cannot. A rule rewrites one instruction into
//! instructions, and two of the rewrites here need something no rule has: the name `puts` has to be
//! interned before a call can name it, and `printf("hello world\n")` has to leave behind a string
//! that is not in the module yet, because "hello world" with a terminator is not a suffix of
//! "hello world\n" with one. So this is a module at a time transformation beside [`crate::ipcp`]
//! and [`crate::ipasra`], which is where the interner and the module both are.
//!
//! `-O1` and above, which is one level below where those two run. gcc folds these at `-O1`, the
//! torture programs are compiled at every level from `-O1` up, and the fold makes the program
//! smaller as well as faster, so there is no level above `-O0` where declining it is right.
//!
//! Off under `-fno-builtin` and `-ffreestanding`, which is the flag pair section 20.1 describes,
//! and off for one name at a time under `-fno-builtin-<name>`. A freestanding program left with a
//! call to a `puts` it never wrote is a link failure, and that is the whole reason the flag exists.

use std::collections::{HashMap, HashSet};

use rucc_base::{Interner, Symbol};
use rucc_ir::{
    AbiList, CallInfo, Datum, Def, Extra, Func, FuncId, Global, Imm, Inst, InstData, IntPred,
    Linkage, MemInfo, MemOrder, Module, Opcode, Pic, Restrict, Signature, SymbolRef, Type, Value,
};

use crate::extents::vouched;
use crate::{Cfg, Fuel, Stats, uses};

/// What the pass is called in `-fopt-info` and `-fpass-fuel=`.
pub const NAME: &str = "libcall";

/// How many block parameters deep the walk that answers "what string is this" goes.
///
/// A conditional expression whose arms are two literals is one level, which is what
/// `builtins/fputs.c` writes twice. Four is room for that nested three deep and is the bound that
/// stops a walk which would otherwise go round a loop forever. The same number bounds the walk
/// down a chain of `ptr_add`, where one level is one index written in the source.
const DEPTH: u32 = 4;

/// How long a chain of block parameters the walk for a string follows.
///
/// Longer than [`DEPTH`], because an `if` and `else if` chain that picks a string in a loop is one
/// block parameter per arm joining the next, and `builtins/stpcpy-chk.c` has four arms inside the
/// loop on top of the loop's own parameter. A parameter already on the walk is not walked again,
/// so this bounds a chain rather than a loop.
const CHAIN: u32 = 12;

/// How many times the calls in one function are looked at, which is the longest chain of folds
/// where each one leaves a call behind that the next one folds.
const ROUNDS: u32 = 3;

/// The names a fold may leave behind, sorted.
const REPLACEMENTS: [&str; 10] = [
    "__memcpy_chk",
    "fputc",
    "fputs",
    "fwrite",
    "memcpy",
    "putchar",
    "puts",
    "strchr",
    "strcpy",
    "strlen",
];

/// The names a checking call may become once its check cannot fail, sorted.
///
/// These are not in [`REPLACEMENTS`] because what a call to one of them looks like is read off the
/// call it replaces rather than written down here. Half of them are variadic or take a `va_list`,
/// and what a `va_list` is travels with the target, so the one signature that is right is the one
/// the checking call already had with the checking arguments taken out of it.
const UNCHECKED: [&str; 18] = [
    "__memcpy_chk",
    "__strcat_chk",
    "__strcpy_chk",
    "__strncpy_chk",
    "memcpy",
    "memmove",
    "mempcpy",
    "memset",
    "snprintf",
    "sprintf",
    "stpcpy",
    "stpncpy",
    "strcat",
    "strcpy",
    "strncat",
    "strncpy",
    "vsnprintf",
    "vsprintf",
];

/// The names a fold reads, sorted.
const SOURCES: [&str; 47] = [
    "__fprintf_chk",
    "__memcpy_chk",
    "__memmove_chk",
    "__mempcpy_chk",
    "__memset_chk",
    "__printf_chk",
    "__snprintf_chk",
    "__sprintf_chk",
    "__stpcpy_chk",
    "__stpncpy_chk",
    "__strcat_chk",
    "__strcpy_chk",
    "__strncat_chk",
    "__strncpy_chk",
    "__vfprintf_chk",
    "__vprintf_chk",
    "__vsnprintf_chk",
    "__vsprintf_chk",
    "bcopy",
    "fprintf",
    "fprintf_unlocked",
    "fputs",
    "fputs_unlocked",
    "index",
    "memchr",
    "memmove",
    "mempcpy",
    "printf",
    "printf_unlocked",
    "rindex",
    "sprintf",
    "stpcpy",
    "strcat",
    "strchr",
    "strcmp",
    "strcpy",
    "strcspn",
    "strlen",
    "strncat",
    "strncmp",
    "strnlen",
    "strpbrk",
    "strrchr",
    "strspn",
    "strstr",
    "vfprintf",
    "vprintf",
];

/// What the compiler worked out a call writes, which is what it is replaced by.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// It writes nothing, so it goes and nothing takes its place.
    Drop,
    /// The answer is a place in an argument the call was given, or nowhere at all, and that answer
    /// takes the place of the call's result.
    Answer(Answer),
    /// This call takes its place.
    Swap {
        /// The symbol the replacement names, which is what the module calls that function.
        callee: Symbol,
        /// What that function takes and returns.
        signature: Signature,
        /// What to pass it.
        args: Vec<Argument>,
        /// What takes the place of the old call's result, where that is not the new call's
        /// result. `stpcpy` of a string whose length is known is a `memcpy` whose answer is the
        /// start of the copy, and the answer `stpcpy` gives is the end of it.
        answer: Option<Answer>,
    },
    /// The same call to another function, with the arguments at these places left out.
    ///
    /// This is what a call to one of the checking functions `_FORTIFY_SOURCE` writes becomes once
    /// its check cannot fail. What the new call looks like is the old one without those
    /// arguments, including whatever the old one passed beyond its named parameters.
    Unchecked {
        /// The symbol the new call names.
        callee: Symbol,
        /// The places of the arguments that go.
        drop: &'static [usize],
    },
}

impl Plan {
    /// Names what each value was renamed to wherever this plan names the old one.
    fn rename(&mut self, renamed: &HashMap<Value, Value>) {
        if renamed.is_empty() {
            return;
        }
        let answer = match self {
            Plan::Drop | Plan::Unchecked { .. } => None,
            Plan::Answer(answer) => Some(answer),
            Plan::Swap { args, answer, .. } => {
                for arg in args {
                    if let Argument::Have(value) = arg {
                        *value = renamed.get(value).copied().unwrap_or(*value);
                    }
                }
                answer.as_mut()
            }
        };
        let value = match answer {
            Some(Answer::Along(value, _) | Answer::Least { count: value, .. }) => value,
            Some(Answer::Byte { of, .. } | Answer::Less { step: of, .. }) => of,
            Some(Answer::Nowhere | Answer::Number(_)) | None => return,
        };
        *value = renamed.get(value).copied().unwrap_or(*value);
    }
}

/// What a call that answers rather than writes was going to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// That many bytes along from a value the call was handed.
    Along(Value, u64),
    /// Nowhere in it, which is a null pointer.
    Nowhere,
    /// That number, in whatever type the call was declared to give back.
    ///
    /// The type is read off the call rather than worked out from the name, because a program that
    /// declared `strlen` as something returning an `int` gets an `int`, and a constant of the
    /// width the call already had is the only one that can take its place.
    Number(i128),
    /// The smaller of a count the call was given and a length the compiler knows, compared as
    /// unsigned numbers, which is what `strnlen` answers over a string the module holds.
    Least {
        /// The count, which nothing is known about.
        count: Value,
        /// The length of the string, up to its terminator.
        len: u64,
    },
    /// A length the compiler knows less a step nothing is known about but how large it can be,
    /// which is what `strlen` of a string the module holds answers at a place inside it that the
    /// program worked out.
    Less {
        /// The length of the string from where the step is taken, up to its terminator.
        len: u64,
        /// How far along the string the call was handed a pointer to, in bytes.
        step: Value,
    },
    /// One byte of a string the call was given against a byte the compiler knows.
    ///
    /// This is the one answer that is an instruction rather than a constant or an address, because
    /// the byte is in memory and has to be read. Both bytes are `unsigned char` values, which is
    /// what the standard says a comparison compares, and the answer is the difference between them
    /// in whichever order the call wrote its arguments.
    Byte {
        /// The string nothing is known about, whose first byte is read.
        of: Value,
        /// The first byte of the string the module holds, or zero where that string is empty.
        against: u8,
        /// Whether the string the module holds was the call's first argument, which is what says
        /// which way round the difference goes.
        leading: bool,
    },
}

/// Which of the places a character appears in a string a search wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    /// `strchr`.
    First,
    /// `strrchr`.
    Last,
}

/// Which way round the test in a span is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Set {
    /// `strspn`, which walks while the character is one of the set.
    Inside,
    /// `strcspn`, which walks while it is not.
    Outside,
}

/// One argument of a replacement call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Argument {
    /// A value the call being replaced already had.
    Have(Value),
    /// An `int` constant, which in every case here is a character.
    Char(u8),
    /// A `size_t` constant, which in every case here is a count of bytes.
    Count(u64),
    /// The address of a read only object holding these bytes and a terminator.
    Text(Vec<u8>),
}

/// What this module already says about each name a fold may leave behind.
///
/// The verifier holds that a call to a name the module declares carries that name's own signature,
/// so a program that declared `fwrite` through `<stdio.h>` decides what a call to it looks like and
/// a program that declared it as something else stops the fold. The alternative is a fold that
/// produces IR the verifier refuses, which is a compiler that crashes on a program gcc compiles.
struct Shapes {
    /// The symbol a call to that name has to carry and the signature it has to have, and `None`
    /// where no call may name it.
    held: HashMap<&'static str, Option<(Symbol, Signature)>>,
    /// The same for each name in [`UNCHECKED`], where the signature is the one the module declared
    /// and `None` where it declared nothing, since the call a checking call becomes brings its own.
    named: HashMap<&'static str, Option<(Symbol, Option<Signature>)>>,
}

impl Shapes {
    /// Reads the module's answer for each of the names a fold may leave behind.
    fn of(module: &Module, names: &mut Interner) -> Self {
        let mut held: HashMap<&'static str, Option<(Symbol, Signature)>> = REPLACEMENTS
            .iter()
            .map(|&name| (name, Some((names.intern(name), canonical(module, name)))))
            .collect();
        let mut named: HashMap<&'static str, Option<(Symbol, Option<Signature>)>> =
            UNCHECKED.iter().map(|&name| (name, Some((names.intern(name), None)))).collect();
        for id in module.funcs() {
            // The name the source gave it, so that a module which renamed `puts` is left with a
            // call to the symbol it renamed it to rather than one to a `puts` it never declared.
            let func = &module[id];
            let spelled = func.spelled.unwrap_or(func.name);
            if let Some(slot) = named.get_mut(names.resolve(spelled)) {
                *slot = Some((func.name, Some(func.signature().clone())));
            }
            let Some(slot) = held.get_mut(names.resolve(spelled)) else { continue };
            let declared = func.signature();
            let agrees = slot.as_ref().is_some_and(|(_, want)| {
                !declared.variadic
                    && declared.param_types().eq(want.param_types())
                    && declared.return_types().eq(want.return_types())
            });
            *slot = agrees.then(|| (func.name, declared.clone()));
        }
        // A variable or a second name for something else is not a function to call, whatever it is
        // spelled.
        for id in module.globals() {
            let name = names.resolve(module[id].name);
            if let Some(slot) = held.get_mut(name) {
                *slot = None;
            }
            if let Some(slot) = named.get_mut(name) {
                *slot = None;
            }
        }
        for id in module.aliases() {
            let name = names.resolve(module[id].name);
            if let Some(slot) = held.get_mut(name) {
                *slot = None;
            }
            if let Some(slot) = named.get_mut(name) {
                *slot = None;
            }
        }
        Self { held, named }
    }

    /// What a call to that name carries, or `None` where this module does not allow one.
    fn get(&self, name: &'static str) -> Option<(Symbol, Signature)> {
        self.held.get(name)?.clone()
    }

    /// What a call to that name carries, where a call to it with this signature is one the module
    /// allows.
    fn unchecked(&self, name: &str, want: &Signature) -> Option<Symbol> {
        let (symbol, declared) = self.named.get(name)?.as_ref()?;
        let agrees = declared.as_ref().is_none_or(|declared| {
            declared.variadic == want.variadic
                && declared.param_types().eq(want.param_types())
                && declared.return_types().eq(want.return_types())
        });
        agrees.then_some(*symbol)
    }
}

/// The signature a call to that name carries where nothing in the module declared it.
///
/// What the frontend already writes, which is how `__builtin_putchar` reaches `putchar` in a
/// program that never named it.
fn canonical(module: &Module, name: &str) -> Signature {
    let int = int();
    let size = size(module);
    match name {
        "puts" => Signature::new().with_params(&[Type::PTR]).with_returns(&[int]),
        "putchar" => Signature::new().with_params(&[int]).with_returns(&[int]),
        "fputc" => Signature::new().with_params(&[int, Type::PTR]).with_returns(&[int]),
        "fputs" => Signature::new().with_params(&[Type::PTR, Type::PTR]).with_returns(&[int]),
        "strchr" => Signature::new().with_params(&[Type::PTR, int]).with_returns(&[Type::PTR]),
        "strlen" => Signature::new().with_params(&[Type::PTR]).with_returns(&[size]),
        "strcpy" => {
            Signature::new().with_params(&[Type::PTR, Type::PTR]).with_returns(&[Type::PTR])
        }
        "memcpy" => {
            Signature::new().with_params(&[Type::PTR, Type::PTR, size]).with_returns(&[Type::PTR])
        }
        "__memcpy_chk" => Signature::new()
            .with_params(&[Type::PTR, Type::PTR, size, size])
            .with_returns(&[Type::PTR]),
        // `fwrite`, the one that is told how many bytes to write rather than going looking for a
        // terminator, and the only one of the five whose types are the target's rather than fixed.
        _ => {
            Signature::new().with_params(&[Type::PTR, size, size, Type::PTR]).with_returns(&[size])
        }
    }
}

/// The type an `int` is in the IR.
///
/// Thirty two bits on every target this compiler has a back end for, which is why it is written
/// here rather than asked of the layout. The day one of them says otherwise, a fold under this rule
/// would hand `putchar` the wrong width, and this is the one place that would have to change.
const fn int() -> Type {
    Type::int(32)
}

/// The type a `size_t` is on the target this module is for.
fn size(module: &Module) -> Type {
    Type::int(module.datalayout.pointer_bits)
}

/// Folds every call in the module whose output the compiler can work out.
///
/// Gives back the functions that changed and what changed in them, which is what the pipeline turns
/// into `-fopt-info` remarks.
pub fn fold(
    module: &mut Module,
    names: &mut Interner,
    no_builtin: &[String],
    pic: Pic,
    fuel: &mut Fuel,
) -> Vec<(FuncId, Stats)> {
    let shapes = Shapes::of(module, names);
    // What each symbol was called in the source, for the declarations where the two differ. A call
    // names a symbol, and a symbol an assembler name replaced says nothing about which library
    // function it is, so this is what the two names are put back together through.
    let standard: HashMap<Symbol, Symbol> =
        module.funcs().filter_map(|id| Some((module[id].name, module[id].spelled?))).collect();
    // A body the program wrote for one of these names does not stop a call to that name being
    // folded, which is gcc 16's rule as well: only `-fno-builtin` says a standard name is not the
    // standard function. What it does stop is folding inside that body, since a `puts` of the
    // program's own with a `printf` of a newline in it would otherwise be a call to itself.
    let library: HashSet<Symbol> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| module[id].name)
        .filter(|&name| {
            let name = names.resolve(standard.get(&name).copied().unwrap_or(name));
            SOURCES.contains(&name) || REPLACEMENTS.contains(&name)
        })
        .collect();
    // One table for the module rather than one per function, so that two calls folded to the same
    // string share one object instead of each getting one of its own.
    let mut texts: HashMap<Vec<u8>, Symbol> = HashMap::new();
    let mut done = Vec::new();
    for id in module.funcs().collect::<Vec<FuncId>>() {
        // A function with none of these names in it is most of them, and the answer for one is a
        // walk over its instructions that allocates nothing. The two tables below are a vector and
        // a predecessor list per block, which is a cost worth not paying over a module whose
        // functions print nothing.
        if module[id].is_declaration()
            || library.contains(&module[id].name)
            || !mentions(&module[id], names, &standard)
        {
            continue;
        }
        let mut stats = Stats::new();
        // A fold can leave behind a call another fold knows, which is how `__stpcpy_chk` of a
        // string that fits becomes `stpcpy` and then a `memcpy` with the end of the copy as its
        // answer. Each round is one step along a chain like that and none is longer than this.
        for _ in 0..ROUNDS {
            // The whole body is read before any of it changes. A plan names values the body holds,
            // and working the next one out from a body half rewritten is how a pass comes to read
            // a value whose definition it has just taken away.
            let plans = {
                let func = &module[id];
                let site = Site {
                    module,
                    func,
                    cfg: &Cfg::new(func),
                    shapes: &shapes,
                    counts: &uses::count(func),
                    standard: &standard,
                    names,
                    no_builtin,
                    pic,
                };
                site.survey(fuel, &mut stats)
            };
            if plans.is_empty() {
                break;
            }
            // A plan read the body as it was, so a value it names may be the answer of a call an
            // earlier plan in this round took away, which is `mempcpy (mempcpy (p, a, 4), b, 4)`.
            // Every plan is renamed through what the ones before it replaced.
            let mut renamed: HashMap<Value, Value> = HashMap::new();
            for (inst, mut plan) in plans {
                plan.rename(&renamed);
                let made = apply(module, id, names, &mut texts, inst, plan);
                for value in renamed.values_mut() {
                    if let Some(&to) = made.get(value) {
                        *value = to;
                    }
                }
                renamed.extend(made);
            }
        }
        if stats.changed() {
            done.push((id, stats));
        }
    }
    done
}

/// Whether this function calls any of the names a fold reads.
fn mentions(func: &Func, names: &Interner, standard: &HashMap<Symbol, Symbol>) -> bool {
    func.blocks().flat_map(|block| func.insts(block)).any(|inst| {
        let data = &func[inst];
        let Extra::Call(at) = data.extra else { return false };
        data.opcode == Opcode::Call
            && func[at].callee.is_some_and(|callee| {
                let spelled = standard.get(&callee).copied().unwrap_or(callee);
                SOURCES.contains(&names.resolve(spelled))
            })
    })
}

/// One function and everything reading it takes to answer what a call in it writes.
struct Site<'a> {
    /// The module it is in, which is where a string literal's bytes are.
    module: &'a Module,
    /// The function.
    func: &'a Func,
    /// Its shape, which is what a block parameter's arguments are found through.
    cfg: &'a Cfg,
    /// What the module allows a replacement call to look like.
    shapes: &'a Shapes,
    /// How many times each value is read, which is what says a result is ignored.
    counts: &'a [u32],
    /// What each renamed symbol was called in the source.
    standard: &'a HashMap<Symbol, Symbol>,
    /// The spellings, for reading a callee's name.
    names: &'a Interner,
    /// The names `-fno-builtin-<name>` took away.
    no_builtin: &'a [String],
    /// Which definitions something else may replace at load time.
    pic: Pic,
}

impl Site<'_> {
    /// Every call in this function that has a plan, with the plan.
    fn survey(&self, fuel: &mut Fuel, stats: &mut Stats) -> Vec<(Inst, Plan)> {
        let mut plans = Vec::new();
        for block in self.func.blocks().collect::<Vec<_>>() {
            for inst in self.func.insts(block).collect::<Vec<Inst>>() {
                let Some(plan) = self.plan(inst) else { continue };
                if !fuel.take() {
                    stats.missed("call to the library folded");
                    continue;
                }
                stats.optimized(match &plan {
                    Plan::Drop => "call to the library that writes nothing removed",
                    Plan::Answer(_) => "call to the library whose answer is known folded",
                    Plan::Swap { .. } => "call to the library folded",
                    Plan::Unchecked { .. } => "checking call whose check cannot fail made plain",
                });
                plans.push((inst, plan));
            }
        }
        plans
    }

    /// What this call writes, where it is one of the calls this knows and the answer can be worked
    /// out.
    fn plan(&self, inst: Inst) -> Option<Plan> {
        let data = &self.func[inst];
        if data.opcode != Opcode::Call || self.func.mem_in(inst).is_some() {
            return None;
        }
        // A program looking at how many characters went out is a program the count matters to, and
        // no two of the printf family answer the same number. `strstr` is not in that position: its
        // answer is what the call is for and the fold produces the same one.
        let ignored = data.results().all(|result| self.counts[result.index()] == 0);
        let Extra::Call(at) = data.extra else { return None };
        let callee = self.func[at].callee?;
        let name = self.names.resolve(self.standard.get(&callee).copied().unwrap_or(callee));
        if self.no_builtin.iter().any(|it| it == name) {
            return None;
        }
        let args: Vec<Value> = self.func[data.args].to_vec();
        // The locked and the unlocked spellings take the same arguments and differ only in how far
        // the fold may go, so they are an arm each with a flag rather than two bodies.
        match name {
            "printf" if ignored => self.printf(&args, false),
            "printf_unlocked" if ignored => self.printf(&args, true),
            "fprintf" if ignored => self.fprintf(&args, false),
            "fprintf_unlocked" if ignored => self.fprintf(&args, true),
            "fputs" if ignored => self.fputs(&args, false),
            "fputs_unlocked" if ignored => self.fputs(&args, true),
            // The formatted checking calls are the plain ones with a flag, and the flag says only
            // whether a `%n` in a format the program can write to is refused, so what they write is
            // what the plain call writes. A `v` spelling hands its arguments over in a list this
            // cannot read, so it folds only where the format takes none of them.
            "__printf_chk" if ignored => self.printf(args.get(1..)?, false),
            "vprintf" | "__vprintf_chk" if ignored => {
                let format = if name == "vprintf" { 0 } else { 1 };
                self.printf(&[*args.get(format)?], false)
            }
            "__fprintf_chk" if ignored => {
                let mut rest = vec![*args.first()?];
                rest.extend_from_slice(args.get(2..)?);
                self.fprintf(&rest, false)
            }
            "vfprintf" | "__vfprintf_chk" if ignored => {
                let format = if name == "vfprintf" { 1 } else { 2 };
                self.fprintf(&[*args.first()?, *args.get(format)?], false)
            }
            "strstr" => self.strstr(data, &args),
            // `index` and `rindex` are the older spellings of the same two searches, and a
            // program that wrote one of them is asking for the same answer.
            "strchr" | "index" => self.strchr(data, &args, Side::First),
            "strrchr" | "rindex" => self.strchr(data, &args, Side::Last),
            "memchr" => self.memchr(data, &args),
            "strlen" => self.strlen(inst, data, &args),
            "strnlen" => self.strnlen(data, &args),
            "strcmp" => self.strcmp(data, &args),
            "strncmp" => self.strncmp(data, &args),
            "strcspn" => self.span(data, &args, Set::Outside),
            "strspn" => self.span(data, &args, Set::Inside),
            "strpbrk" => self.strpbrk(data, &args),
            "strcpy" | "stpcpy" => self.strcpy(data, name, &args, ignored),
            "strcat" => self.strcat(data, &args, None),
            "strncat" => self.strncat(data, &args),
            "mempcpy" => self.mempcpy(data, &args, ignored),
            "memmove" => self.memmove(data, &args),
            // `bcopy` is `memmove` with the two addresses the other way round and no answer.
            "bcopy" => {
                let [source, dest, count] = *args else { return None };
                if data.results().next().is_some() {
                    return None;
                }
                self.moved(dest, source, count).map(|plan| match plan {
                    Plan::Answer(_) => Plan::Drop,
                    plan => plan,
                })
            }
            "sprintf" => self.sprintf(data, &args, ignored),
            "__memcpy_chk" | "__memmove_chk" | "__mempcpy_chk" | "__memset_chk" => {
                self.memory_chk(data, name, &args, ignored)
            }
            "__strcpy_chk" | "__stpcpy_chk" => self.strcpy_chk(data, name, &args, ignored),
            "__strncpy_chk" | "__stpncpy_chk" => self.strncpy_chk(data, name, &args, ignored),
            "__strcat_chk" => self.strcat_chk(data, &args),
            "__strncat_chk" => self.strncat_chk(data, &args),
            "__sprintf_chk" | "__vsprintf_chk" => self.sprintf_chk(data, name, &args),
            "__snprintf_chk" | "__vsnprintf_chk" => self.snprintf_chk(data, name, &args),
            _ => None,
        }
    }

    /// The type this call's one result has, where it has one and it is an integer.
    ///
    /// A declaration of another shape is a function of the program's own, and a number is not what
    /// it answers.
    fn answers(&self, data: &InstData) -> Option<Type> {
        let mut results = data.results();
        let ty = self.func[results.next()?].ty;
        (results.next().is_none() && ty.is_int() && !ty.is_vector()).then_some(ty)
    }

    /// Whether this call's one result is a pointer, which every search for a place gives back.
    fn places(&self, data: &InstData) -> bool {
        let mut results = data.results();
        results.next().is_some_and(|result| self.func[result].ty == Type::PTR)
            && results.next().is_none()
    }

    /// The character a search was told to look for, which the call carries as an `int` and the
    /// library reads as a `char`.
    fn character(&self, value: Value) -> Option<u8> {
        let (imm, ty) = crate::fold::evaluated(self.func, value, DEPTH)?;
        u8::try_from(imm.signed(ty).rem_euclid(256)).ok()
    }

    /// A count of bytes the call was given, which has to be a constant that fits a `usize`.
    ///
    /// The source writes a small count as an `int` and the call takes a `size_t`, so what the
    /// argument holds is a widening of the constant rather than the constant, and reading only the
    /// argument would miss every count anyone actually writes.
    fn count(&self, value: Value) -> Option<usize> {
        let narrow = self.widened(value);
        let (imm, ty) = crate::fold::evaluated(self.func, narrow, DEPTH)?;
        // A count the source wrote as a negative number is not a count, whatever the conversion
        // makes of it, and folding on one would be reading an object that is not there.
        (narrow == value || imm.signed(ty) >= 0).then_some(())?;
        usize::try_from(imm.unsigned()).ok()
    }

    /// What this value is a widening of, or the value itself where it is not one.
    ///
    /// Both conversions leave a non negative constant alone, so which one it was only matters for
    /// refusing a negative one, and the caller is the one that does that.
    fn widened(&self, value: Value) -> Value {
        let Def::Result { inst, .. } = self.func[value].def else { return value };
        if !matches!(self.func[inst].opcode, Opcode::SExt | Opcode::ZExt) {
            return value;
        }
        self.func[self.func[inst].args].first().copied().unwrap_or(value)
    }

    /// Where a `strchr` or a `strrchr` finds its character.
    ///
    /// A terminator is found at the end of the string rather than not at all, which is what makes
    /// `strchr(s, 0)` the address of the terminator and is the one place the bytes this reads and
    /// the string it is searching are not the same length.
    fn strchr(&self, data: &InstData, args: &[Value], side: Side) -> Option<Plan> {
        if args.len() != 2 || self.func[args[0]].ty != Type::PTR || !self.places(data) {
            return None;
        }
        let wanted = self.character(args[1])?;
        let Some(text) = self.one(args[0]) else {
            // A string has one terminator in it, so looking for that one from the right finds the
            // same place as looking for it from the left, and which end the walk started at stops
            // mattering. That is an answer even where nothing at all is known about the string.
            return match (wanted, side) {
                (0, Side::Last) => {
                    self.call("strchr", vec![Argument::Have(args[0]), Argument::Char(0)])
                }
                _ => None,
            };
        };
        let found = match (wanted, side) {
            (0, _) => Some(text.len()),
            (_, Side::First) => text.iter().position(|&byte| byte == wanted),
            (_, Side::Last) => text.iter().rposition(|&byte| byte == wanted),
        };
        Some(Plan::Answer(match found {
            Some(at) => Answer::Along(args[0], u64::try_from(at).ok()?),
            None => Answer::Nowhere,
        }))
    }

    /// Where a `memchr` finds its character, which is a search over a count rather than up to a
    /// terminator.
    ///
    /// So this reads the object's bytes rather than the string in it, and it refuses a count the
    /// object does not have that many bytes for, since a call that reads past the end of what the
    /// compiler can see is a call whose answer the compiler does not know.
    fn memchr(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        (args.len() == 3).then_some(())?; // not a threshold: `memchr` takes three arguments.
        if self.func[args[0]].ty != Type::PTR || !self.places(data) {
            return None;
        }
        let wanted = self.character(args[1])?;
        let count = self.count(args[2])?;
        let bytes = self.raw(args[0])?;
        let window = bytes.get(..count)?;
        Some(Plan::Answer(match window.iter().position(|&byte| byte == wanted) {
            Some(at) => Answer::Along(args[0], u64::try_from(at).ok()?),
            None => Answer::Nowhere,
        }))
    }

    /// How long a string this module holds is.
    fn strlen(&self, inst: Inst, data: &InstData, args: &[Value]) -> Option<Plan> {
        if args.len() != 1 || self.func[args[0]].ty != Type::PTR {
            return None;
        }
        self.answers(data)?;
        if let Some(len) = self.length(args[0]) {
            return Some(Plan::Answer(Answer::Number(i128::try_from(len).ok()?)));
        }
        if let Some(text) = self.stored(inst, args[0]) {
            return Some(Plan::Answer(Answer::Number(i128::try_from(text.len()).ok()?)));
        }
        // `strlen("hello world" + (x & 7))` is eleven less the step, because a step of no more than
        // the length lands on a byte of the string or on its terminator and there is no other
        // terminator before the end. gcc 16 folds it the same way.
        let Def::Result { inst, .. } = self.func[args[0]].def else { return None };
        if self.func[inst].opcode != Opcode::PtrAdd {
            return None;
        }
        let &[base, step] = &self.func[self.func[inst].args] else { return None };
        let len = u64::try_from(self.literal(base)?.len()).ok()?;
        if self.largest(step)? > u128::from(len) {
            return None;
        }
        Some(Plan::Answer(Answer::Less { len, step }))
    }

    /// The string at this address in a local array, as the stores in front of the call wrote it.
    ///
    /// `builtins/strlen.c` writes "nts" and its terminator into a `char str[8]` a byte at a time
    /// and asks how long it is, and gcc 16 answers that from the stores. The walk goes back from
    /// the call through its own block and keeps the last byte stored at each place, and it stops at
    /// the first thing that could have written the array some other way: a call, a store of more
    /// than a byte, or a store through a pointer that is not this array or another local one. What
    /// it has then is enough only where it reaches a stored terminator without a gap.
    fn stored(&self, call: Inst, value: Value) -> Option<Vec<u8>> {
        let (base, offset) = self.address(value)?;
        if !self.local(base) {
            return None;
        }
        let block = self.func.block_of(call)?;
        let mut bytes: HashMap<i128, u8> = HashMap::new();
        for inst in self.func.insts_backwards(block).skip_while(|&inst| inst != call).skip(1) {
            let data = &self.func[inst];
            if !data.opcode.writes_memory() {
                continue;
            }
            if data.opcode != Opcode::Store {
                break;
            }
            let &[byte, to] = &self.func[data.args] else { break };
            let Some((root, at)) = self.address(to) else { break };
            if root != base {
                // Two locals are two objects, so a store into another one leaves this one alone.
                if self.local(root) {
                    continue;
                }
                break;
            }
            if self.func[byte].ty != Type::int(8) {
                break;
            }
            let Some((imm, _)) = crate::fold::evaluated(self.func, byte, DEPTH) else { break };
            let Ok(byte) = u8::try_from(imm.unsigned()) else { break };
            bytes.entry(at).or_insert(byte);
        }
        let mut text = Vec::new();
        for at in (offset..).take(bytes.len()) {
            match *bytes.get(&at)? {
                0 => return Some(text),
                byte => text.push(byte),
            }
        }
        None
    }

    /// The same, stopping at a count.
    ///
    /// A string with no terminator inside the count is the count, and that needs the object's bytes
    /// rather than the string in it, because there may be no string in it at all.
    fn strnlen(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        if args.len() != 2 || self.func[args[0]].ty != Type::PTR {
            return None;
        }
        let ty = self.answers(data)?;
        // A string with its terminator inside the object is read no further than the terminator
        // whatever the count is, so the answer is the smaller of the two, and any count at all is
        // one the call could have been given. That includes a count the source wrote as a negative
        // number, which is a very large one once it is a `size_t`.
        if let Some(text) = self.literal(args[0]) {
            let len = u64::try_from(text.len()).ok()?;
            if let Some((imm, _)) = crate::fold::evaluated(self.func, args[1], DEPTH) {
                let least = imm.unsigned().min(u128::from(len));
                return Some(Plan::Answer(Answer::Number(i128::try_from(least).ok()?)));
            }
            // An empty string is nothing to count, so the count does not matter.
            if len == 0 {
                return Some(Plan::Answer(Answer::Number(0)));
            }
            // Otherwise the smaller of the two has to be worked out when the program runs, and the
            // count and the answer have to be the same type for that to be one comparison.
            (self.func[args[1]].ty == ty).then_some(())?;
            return Some(Plan::Answer(Answer::Least { count: args[1], len }));
        }
        let count = self.count(args[1])?;
        let bytes = self.raw(args[0])?;
        let window = bytes.get(..count.min(bytes.len()))?;
        let len = match window.iter().position(|&byte| byte == 0) {
            Some(at) => at,
            // Nothing terminated it inside the window, so the answer is the count only where the
            // window was the whole count.
            None if window.len() == count => count,
            None => return None,
        };
        Some(Plan::Answer(Answer::Number(i128::try_from(len).ok()?)))
    }

    /// How two strings this module holds compare.
    fn strcmp(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        (args.len() == 2).then_some(())?;
        self.compared(data, args, usize::MAX)
    }

    /// The same over a count the call was given, which has to be a constant.
    fn strncmp(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        (args.len() == 3).then_some(())?; // not a threshold: `strncmp` takes three arguments.
        let count = self.count(args[2])?;
        self.compared(data, args, count)
    }

    /// The comparison both of them are, over however many bytes each is allowed to read.
    ///
    /// The sign is what the standard promises and the magnitude is not, so where both strings are
    /// known this answers one of minus one, zero and one, which is what gcc leaves behind as well.
    /// Where only one of them is known there is still an answer in the two cases the first byte
    /// settles, and that one is a read rather than a constant.
    fn compared(&self, data: &InstData, args: &[Value], bound: usize) -> Option<Plan> {
        if self.func[args[0]].ty != Type::PTR || self.func[args[1]].ty != Type::PTR {
            return None;
        }
        let ty = self.answers(data)?;
        // A comparison told to read no bytes reads neither string, so the answer is the same
        // whatever the two of them hold, and holds even where neither is there to be read.
        if bound == 0 {
            return Some(Plan::Answer(Answer::Number(0)));
        }
        match (self.one(args[0]), self.one(args[1])) {
            (Some(left), Some(right)) => {
                Some(Plan::Answer(Answer::Number(walk(&left, &right, bound))))
            }
            (Some(known), None) => self.byte(ty, &known, args[1], true, bound),
            (None, Some(known)) => self.byte(ty, &known, args[0], false, bound),
            (None, None) => None,
        }
    }

    /// The comparison a string this module holds makes against one nothing is known about.
    ///
    /// Only the first byte of the other string can be read here, so this is an answer in the two
    /// cases where that byte is the whole comparison: a count of one, which is that byte and
    /// nothing else, and a known string that is empty, whose terminator stops the walk however
    /// many bytes the count allowed.
    fn byte(
        &self,
        ty: Type,
        known: &[u8],
        other: Value,
        leading: bool,
        bound: usize,
    ) -> Option<Plan> {
        (bound == 1 || known.is_empty()).then_some(())?;
        // The answer is a byte widened into the type the call was declared with, and a type no
        // wider than a byte is a declaration this has no room to answer in.
        (ty.bits() > 8).then_some(())?;
        let against = known.first().copied().unwrap_or(0);
        Some(Plan::Answer(Answer::Byte { of: other, against, leading }))
    }

    /// How far into the first string the second one's characters start, or stop.
    ///
    /// `strcspn` walks while the character is outside the set and `strspn` walks while it is
    /// inside, which is one walk with the test turned round, and the shape rules fall out of it:
    /// nothing is outside an empty set, so `strspn(s, "")` is zero, and everything is, so
    /// `strcspn(s, "")` is the length of `s`.
    fn span(&self, data: &InstData, args: &[Value], set: Set) -> Option<Plan> {
        if args.len() != 2
            || self.func[args[0]].ty != Type::PTR
            || self.func[args[1]].ty != Type::PTR
        {
            return None;
        }
        let ty = self.answers(data)?;
        // An empty first string is no bytes to walk over, whatever the set is, and that is the
        // answer `strcspn("", s)` wants where nothing is known about `s`.
        if self.one(args[0]).is_some_and(|text| text.is_empty()) {
            return Some(Plan::Answer(Answer::Number(0)));
        }
        let accept = self.one(args[1])?;
        // Both walks are the same walk with the test turned round, and an empty set needs no arm of
        // its own here, because nothing is inside one and so the walk stops at once or not at all.
        if let Some(text) = self.one(args[0]) {
            let len = text
                .iter()
                .position(|byte| accept.contains(byte) != matches!(set, Set::Inside))
                .unwrap_or(text.len());
            return Some(Plan::Answer(Answer::Number(i128::try_from(len).ok()?)));
        }
        // Nothing is known about the string, so only the shape rules are left.
        accept.is_empty().then_some(())?;
        match set {
            Set::Inside => Some(Plan::Answer(Answer::Number(0))),
            // A number the call gives back and a number `strlen` gives back have to be the same
            // width, since what reads the first is going to read the second and nothing here writes
            // a conversion.
            Set::Outside => {
                let (_, signature) = self.shapes.get("strlen")?;
                signature.return_types().eq([ty]).then_some(())?;
                self.call("strlen", vec![Argument::Have(args[0])])
            }
        }
    }

    /// Where the first character of one string that is in the other is.
    fn strpbrk(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        if args.len() != 2 || self.func[args[0]].ty != Type::PTR || !self.places(data) {
            return None;
        }
        if self.func[args[1]].ty != Type::PTR {
            return None;
        }
        let accept = self.one(args[1])?;
        // Nothing is in an empty set, so the search runs off the end of any string at all.
        if accept.is_empty() {
            return Some(Plan::Answer(Answer::Nowhere));
        }
        match self.one(args[0]) {
            Some(text) => {
                Some(Plan::Answer(match text.iter().position(|byte| accept.contains(byte)) {
                    Some(at) => Answer::Along(args[0], u64::try_from(at).ok()?),
                    None => Answer::Nowhere,
                }))
            }
            // A set of one character is a search for that character, which is the same fold
            // `strstr` of a needle of one character gets.
            None => match accept.as_slice() {
                [one] => self.call("strchr", vec![Argument::Have(args[0]), Argument::Char(*one)]),
                _ => None,
            },
        }
    }

    /// Where a `strstr` finds what it was told to look for.
    ///
    /// The three folds gcc has for it, and the order matters: two strings this module holds are an
    /// answer, and a haystack nothing is known about is a search for a character where the needle is
    /// one character long.
    fn strstr(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        if args.len() != 2 {
            return None;
        }
        let (haystack, needle) = (args[0], args[1]);
        if self.func[haystack].ty != Type::PTR || self.func[needle].ty != Type::PTR {
            return None;
        }
        // A declaration of another shape is a function of the program's own, and the answer this
        // produces is a pointer whatever the program said the call gives back.
        let mut results = data.results();
        if !results.next().is_some_and(|result| self.func[result].ty == Type::PTR)
            || results.next().is_some()
        {
            return None;
        }
        let needle = self.one(needle)?;
        // The empty string is found at once, wherever it is looked for and whatever is there.
        if needle.is_empty() {
            return Some(Plan::Answer(Answer::Along(haystack, 0)));
        }
        match self.one(haystack) {
            Some(hay) => Some(Plan::Answer(match at(&hay, &needle) {
                Some(found) => Answer::Along(haystack, u64::try_from(found).ok()?),
                None => Answer::Nowhere,
            })),
            // A needle of one character is a search for that character, which is a smaller function
            // and is worth doing wherever the haystack came from.
            None => match needle.as_slice() {
                [one] => self.call("strchr", vec![Argument::Have(haystack), Argument::Char(*one)]),
                _ => None,
            },
        }
    }

    /// What a `printf` writes.
    fn printf(&self, args: &[Value], quiet: bool) -> Option<Plan> {
        let format = self.one(*args.first()?)?;
        match args.len() {
            1 => self.plain(&format, None, quiet),
            2 if format == b"%s\n" && !quiet && self.func[args[1]].ty == Type::PTR => {
                self.call("puts", vec![Argument::Have(args[1])])
            }
            2 if format == b"%c" && !quiet && self.func[args[1]].ty == int() => {
                self.call("putchar", vec![Argument::Have(args[1])])
            }
            // The same question asked again of the argument, because what `printf("%s", p)` writes
            // is what `printf(p)` writes for a `p` holding no `%`. A `p` that does hold one is left
            // alone here and folded by gcc, which is a missed fold and not a wrong answer.
            2 if format == b"%s" => self.plain(&self.one(args[1])?, None, quiet),
            _ => None,
        }
    }

    /// What an `fprintf` writes.
    fn fprintf(&self, args: &[Value], quiet: bool) -> Option<Plan> {
        let stream = *args.first()?;
        if self.func[stream].ty != Type::PTR {
            return None;
        }
        let format = self.one(*args.get(1)?)?;
        match args.len() {
            2 => self.plain(&format, Some((args[1], stream)), quiet),
            3 if format == b"%c" && !quiet && self.func[args[2]].ty == int() => {
                self.call("fputc", vec![Argument::Have(args[2]), Argument::Have(stream)])
            }
            // Whatever is known about the argument, which is the fold `printf` cannot have: this
            // one holds the stream, so the call it leaves behind is one the program could have
            // written for itself.
            3 if format == b"%s" && self.func[args[2]].ty == Type::PTR => {
                match self.strings(args[2]) {
                    Some(candidates) => self.string(&candidates, args[2], stream, quiet),
                    None if quiet => None,
                    None => {
                        self.call("fputs", vec![Argument::Have(args[2]), Argument::Have(stream)])
                    }
                }
            }
            _ => None,
        }
    }

    /// What an `fputs` writes.
    fn fputs(&self, args: &[Value], quiet: bool) -> Option<Plan> {
        if args.len() != 2 {
            return None;
        }
        let (text, stream) = (args[0], args[1]);
        if self.func[text].ty != Type::PTR || self.func[stream].ty != Type::PTR {
            return None;
        }
        self.string(&self.strings(text)?, text, stream, quiet)
    }

    /// What a format holding no `%` writes, given the stream to write it to or nothing.
    ///
    /// The empty case comes first and is the one an unlocked spelling is allowed, because a call
    /// writing nothing is removed without naming any function at all.
    fn plain(&self, format: &[u8], stream: Option<(Value, Value)>, quiet: bool) -> Option<Plan> {
        if format.is_empty() {
            return Some(Plan::Drop);
        }
        if quiet || format.contains(&b'%') {
            return None;
        }
        match (format, stream) {
            ([one], Some((_, stream))) => {
                self.call("fputc", vec![Argument::Char(*one), Argument::Have(stream)])
            }
            // Anything longer takes the whole format, since `fwrite` is told how many bytes to
            // write and does not go looking for a terminator.
            (_, Some((text, stream))) => self.fwrite(Argument::Have(text), format.len(), stream),
            ([one], None) => self.call("putchar", vec![Argument::Char(*one)]),
            // `puts` writes a newline of its own, so what it has to be given is the format without
            // its last character, and that is a string the module does not hold yet.
            (_, None) => {
                let (&last, rest) = format.split_last()?;
                match last {
                    b'\n' => self.call("puts", vec![Argument::Text(rest.to_vec())]),
                    _ => None,
                }
            }
        }
    }

    /// What writing this string to this stream is, given every string the pointer may point at.
    ///
    /// The candidates have to agree on their length, because the length is what decides which call
    /// this becomes. They need not agree on their contents unless the length is one, where the
    /// character itself is an argument.
    fn string(
        &self,
        candidates: &[Vec<u8>],
        text: Value,
        stream: Value,
        quiet: bool,
    ) -> Option<Plan> {
        let first = candidates.first()?;
        if candidates.iter().any(|it| it.len() != first.len()) {
            return None;
        }
        if first.is_empty() {
            return Some(Plan::Drop);
        }
        if quiet {
            return None;
        }
        match first.as_slice() {
            [one] if candidates.iter().all(|it| it[0] == *one) => {
                self.call("fputc", vec![Argument::Char(*one), Argument::Have(stream)])
            }
            // A length of one that two candidates disagree about is an `fwrite` of one byte, since
            // the byte itself is not a constant here and the pointer is what has to be written.
            // That is gcc's answer too, and its output for this is a conditional move feeding an
            // `fwrite` of one.
            _ => self.fwrite(Argument::Have(text), first.len(), stream),
        }
    }

    /// An `fwrite` of that many bytes from that address.
    fn fwrite(&self, text: Argument, bytes: usize, stream: Value) -> Option<Plan> {
        let len = u64::try_from(bytes).ok()?;
        self.call(
            "fwrite",
            vec![text, Argument::Count(1), Argument::Count(len), Argument::Have(stream)],
        )
    }

    /// `strcpy` and `stpcpy` of a string whose length is known, which copy that many bytes and a
    /// terminator and are `memcpy` of that many.
    ///
    /// `stpcpy` answers the end of the copy rather than the start, which is the one difference
    /// between the two, so where nothing reads its answer it is `strcpy` whatever the string is.
    fn strcpy(&self, data: &InstData, name: &str, args: &[Value], ignored: bool) -> Option<Plan> {
        let [dest, source] = *args else { return None };
        if !self.places(data) {
            return None;
        }
        let end = name == "stpcpy";
        if end && ignored {
            return self.unchecked(data, "strcpy", &[]);
        }
        let len = self.length(source)?;
        let (callee, signature) = self.shapes.get("memcpy")?;
        let args =
            vec![Argument::Have(dest), Argument::Have(source), Argument::Count(len as u64 + 1)];
        let answer = end.then_some(Answer::Along(dest, len as u64));
        Some(Plan::Swap { callee, signature, args, answer })
    }

    /// `strcat` and `strncat` that append nothing, which answer where they were told to append.
    ///
    /// Nothing is appended where the string is empty or where the count is zero, and neither call
    /// reads the destination before it knows that.
    fn strcat(&self, data: &InstData, args: &[Value], count: Option<Value>) -> Option<Plan> {
        let [dest, source] = *args else { return None };
        let nothing = self.one(source).is_some_and(|text| text.is_empty())
            || count.is_some_and(|count| self.number(count) == Some(0));
        (nothing && self.places(data)).then_some(Plan::Answer(Answer::Along(dest, 0)))
    }

    /// `strncat` whose count is no limit on a string whose length is known, which is `strcat`.
    fn strncat(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        let [dest, source, count] = *args else { return None };
        if let Some(plan) = self.strcat(data, &[dest, source], Some(count)) {
            return Some(plan);
        }
        let len = self.one(source)?.len() as u128;
        (self.number(count)? >= len).then(|| self.unchecked(data, "strcat", &[2]))?
    }

    /// `memmove`, which copies nothing where the count is zero and is `memcpy` where the two
    /// places cannot overlap.
    fn memmove(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        let [dest, source, count] = *args else { return None };
        if !self.places(data) {
            return None;
        }
        self.moved(dest, source, count)
    }

    /// What a move of that many bytes from the source to the destination is, where it is anything
    /// but itself.
    ///
    /// A move that cannot overlap is a copy. That is so where it is one byte, since one byte is
    /// read before it is written; where the source is a read only object, since the destination is
    /// written and a read only object is not; and where either side is a local the other is not,
    /// since two objects do not overlap. `builtins/memmove.c` and `builtins/memmove-2.c` are all
    /// three, and gcc 16 makes a `memcpy` or plain loads and stores of each. A local and a pointer
    /// read from somewhere are not two objects, since the pointer may be the local's address.
    fn moved(&self, dest: Value, source: Value, count: Value) -> Option<Plan> {
        if self.number(count) == Some(0) {
            return Some(Plan::Answer(Answer::Along(dest, 0)));
        }
        let (to, _) = self.address(dest)?;
        let (from, _) = self.address(source)?;
        let apart = self.largest(count).is_some_and(|count| count <= 1)
            || self.fixed(from)
            || (to != from && self.object(to) && self.object(from))
                && (self.local(to) || self.local(from));
        apart.then(|| self.copy(dest, source, count))?
    }

    /// A `memcpy` of that many bytes, answering the destination.
    fn copy(&self, dest: Value, source: Value, count: Value) -> Option<Plan> {
        let (callee, signature) = self.shapes.get("memcpy")?;
        let args = vec![Argument::Have(dest), Argument::Have(source), Argument::Have(count)];
        Some(Plan::Swap { callee, signature, args, answer: None })
    }

    /// Whether this is the address of an object rather than a pointer that could be anywhere.
    fn object(&self, value: Value) -> bool {
        self.local(value)
            || matches!(self.func[value].def, Def::Result { inst, .. } if self.func[inst].opcode == Opcode::GlobalAddr)
    }

    /// Whether this is the address of a local array, which no other object overlaps.
    fn local(&self, value: Value) -> bool {
        matches!(self.func[value].def, Def::Result { inst, .. } if self.func[inst].opcode == Opcode::Alloca)
    }

    /// Whether this is the address of a read only object whose definition the link cannot swap for
    /// one that is not.
    fn fixed(&self, value: Value) -> bool {
        let Def::Result { inst, .. } = self.func[value].def else { return false };
        if self.func[inst].opcode != Opcode::GlobalAddr {
            return false;
        }
        let Extra::Symbol(name) = self.func[inst].extra else { return false };
        let Some(SymbolRef::Global(id)) = self.module.lookup(name) else { return false };
        let global = &self.module[id];
        global.constant && vouched(global, self.pic)
    }

    /// `mempcpy`, which is `memcpy` answering the end of the copy rather than the start.
    ///
    /// Where nothing reads the answer the two are the same call. Where something does, the end is
    /// the start and the count, which is an address this can write only where the count is known.
    fn mempcpy(&self, data: &InstData, args: &[Value], ignored: bool) -> Option<Plan> {
        let [dest, source, count] = *args else { return None };
        if ignored {
            return self.unchecked(data, "memcpy", &[]);
        }
        let along = u64::try_from(self.number(count)?).ok()?;
        if !self.places(data) {
            return None;
        }
        let (callee, signature) = self.shapes.get("memcpy")?;
        let args = vec![Argument::Have(dest), Argument::Have(source), Argument::Have(count)];
        Some(Plan::Swap { callee, signature, args, answer: Some(Answer::Along(dest, along)) })
    }

    /// `sprintf` of a format with nothing to convert, or of `"%s"` and one string, which writes
    /// the string and is `strcpy` of it.
    ///
    /// The count `sprintf` answers is the length of what it wrote and `strcpy` answers something
    /// else, so a program reading it needs that length to be one the compiler knows.
    fn sprintf(&self, data: &InstData, args: &[Value], ignored: bool) -> Option<Plan> {
        let (&dest, &format) = (args.first()?, args.get(1)?);
        let text = self.one(format)?;
        let source = match *args {
            [_, _] if !text.contains(&b'%') => format,
            [_, _, arg] if text == b"%s" && self.func[arg].ty == Type::PTR => arg,
            _ => return None,
        };
        let answer = if ignored {
            None
        } else {
            self.answers(data)?;
            Some(Answer::Number(i128::try_from(self.one(source)?.len()).ok()?))
        };
        let (callee, signature) = self.shapes.get("strcpy")?;
        Some(Plan::Swap {
            callee,
            signature,
            args: vec![Argument::Have(dest), Argument::Have(source)],
            answer,
        })
    }

    /// `__memcpy_chk` and the three beside it, which are the plain call where the count is known to
    /// fit the object.
    ///
    /// `__mempcpy_chk` answers the end of the copy and `__memcpy_chk` the start, so where nothing
    /// reads the answer and the check has to stay, it stays on the call that does not work one out.
    fn memory_chk(
        &self,
        data: &InstData,
        name: &str,
        args: &[Value],
        ignored: bool,
    ) -> Option<Plan> {
        let [_, _, count, size] = *args else { return None };
        if self.fits(count, size) {
            return self.unchecked(data, plain(name), &[3]);
        }
        (name == "__mempcpy_chk" && ignored).then(|| self.unchecked(data, "__memcpy_chk", &[]))?
    }

    /// `__strcpy_chk` and `__stpcpy_chk`, which are the plain call where the string and its
    /// terminator are known to fit.
    ///
    /// Where they are not known to, a string whose length is known is still a count, so
    /// `__strcpy_chk` becomes the `__memcpy_chk` of that many bytes and the library checks a number
    /// rather than walking a string to find one. `__stpcpy_chk` whose answer nothing reads becomes
    /// `__strcpy_chk` for the same reason `stpcpy` becomes `strcpy`.
    fn strcpy_chk(
        &self,
        data: &InstData,
        name: &str,
        args: &[Value],
        ignored: bool,
    ) -> Option<Plan> {
        let [dest, source, size] = *args else { return None };
        let end = name == "__stpcpy_chk";
        let fits = self.unknown(size)
            || self.longest(source).zip(self.number(size)).is_some_and(|(len, size)| len < size);
        if fits {
            return self.unchecked(data, if end && !ignored { "stpcpy" } else { "strcpy" }, &[2]);
        }
        if end {
            return ignored.then(|| self.unchecked(data, "__strcpy_chk", &[]))?;
        }
        let len = self.one(source)?.len() as u64;
        let args = vec![
            Argument::Have(dest),
            Argument::Have(source),
            Argument::Count(len + 1),
            Argument::Have(size),
        ];
        self.call("__memcpy_chk", args)
    }

    /// `__strncpy_chk` and `__stpncpy_chk`, which write exactly as many bytes as they were told to
    /// and are the plain call where that many fit.
    fn strncpy_chk(
        &self,
        data: &InstData,
        name: &str,
        args: &[Value],
        ignored: bool,
    ) -> Option<Plan> {
        let [_, _, count, size] = *args else { return None };
        let end = name == "__stpncpy_chk";
        if self.fits(count, size) {
            return self.unchecked(data, if end && !ignored { "stpncpy" } else { "strncpy" }, &[3]);
        }
        (end && ignored).then(|| self.unchecked(data, "__strncpy_chk", &[]))?
    }

    /// `__strcat_chk`, which appends nothing where the string is empty and is the plain call only
    /// where nothing is known about the object.
    ///
    /// What it appends to is a string whose length is not known here, so no size is enough.
    fn strcat_chk(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        let [dest, source, size] = *args else { return None };
        if let Some(plan) = self.strcat(data, &[dest, source], None) {
            return Some(plan);
        }
        self.unknown(size).then(|| self.unchecked(data, "strcat", &[2]))?
    }

    /// `__strncat_chk`, which is `__strcat_chk` where the count is no limit on a string whose length
    /// is known.
    fn strncat_chk(&self, data: &InstData, args: &[Value]) -> Option<Plan> {
        let [dest, source, count, size] = *args else { return None };
        if let Some(plan) = self.strcat(data, &[dest, source], Some(count)) {
            return Some(plan);
        }
        if self.unknown(size) {
            return self.unchecked(data, "strncat", &[3]);
        }
        let len = self.one(source)?.len() as u128;
        (self.number(count)? >= len).then(|| self.unchecked(data, "__strcat_chk", &[2]))?
    }

    /// `__sprintf_chk` and `__vsprintf_chk`, which are the plain call where what they write is known
    /// to fit.
    ///
    /// What they write is known in two shapes, a format with nothing to convert and `"%s"` of a
    /// string the module holds, and the second is only readable in the variadic one because a
    /// `va_list` is not something this can look inside.
    fn sprintf_chk(&self, data: &InstData, name: &str, args: &[Value]) -> Option<Plan> {
        let (&flag, &size, &format) = (args.get(1)?, args.get(2)?, args.get(3)?);
        let text = self.one(format);
        let len = match (text.as_deref(), args.get(4..)?) {
            (Some(text), rest)
                if !text.contains(&b'%') && (name == "__vsprintf_chk" || rest.is_empty()) =>
            {
                Some(text.len() as u128)
            }
            (Some(b"%s"), &[arg]) if name == "__sprintf_chk" => {
                self.one(arg).map(|arg| arg.len() as u128)
            }
            _ => None,
        };
        let fits =
            self.unknown(size) || len.zip(self.number(size)).is_some_and(|(len, size)| len < size);
        (fits && self.flagless(flag, text.as_deref()))
            .then(|| self.unchecked(data, plain(name), &[1, 2]))?
    }

    /// `__snprintf_chk` and `__vsnprintf_chk`, which write no more than they were told to and are
    /// the plain call where that many fit.
    fn snprintf_chk(&self, data: &InstData, name: &str, args: &[Value]) -> Option<Plan> {
        let (&count, &flag, &size, &format) =
            (args.get(1)?, args.get(2)?, args.get(3)?, args.get(4)?);
        let text = self.one(format);
        (self.fits(count, size) && self.flagless(flag, text.as_deref()))
            .then(|| self.unchecked(data, plain(name), &[2, 3]))?
    }

    /// Whether the flag a checking printf was handed asks for nothing the plain one does not do.
    ///
    /// The flag is set above `_FORTIFY_SOURCE=1` and what it adds is refusing `%n` in a format
    /// that is writable memory, so a format with nothing to convert, or with a `%s` alone, is one
    /// the flag has nothing to say about.
    fn flagless(&self, flag: Value, text: Option<&[u8]>) -> bool {
        self.number(flag) == Some(0)
            || text.is_some_and(|text| !text.contains(&b'%') || text == b"%s")
    }

    /// The same call to the function that name is, with the arguments at those places left out,
    /// where the module allows a call to it that looks like that.
    fn unchecked(&self, data: &InstData, name: &str, drop: &'static [usize]) -> Option<Plan> {
        let Extra::Call(at) = data.extra else { return None };
        let want = without(&self.func[self.func[at].signature], drop)?;
        let callee = self.shapes.unchecked(name, &want)?;
        Some(Plan::Unchecked { callee, drop })
    }

    /// Whether a checking call's size is the one that says nothing is known about the object.
    ///
    /// `__builtin_object_size` answers all ones where it cannot tell, and a check against that is
    /// a check nothing can fail.
    fn unknown(&self, size: Value) -> bool {
        crate::fold::evaluated(self.func, size, DEPTH).is_some_and(|(imm, ty)| imm.signed(ty) == -1)
    }

    /// Whether a count is known to be no more than the size of the object it is a count of.
    fn fits(&self, count: Value, size: Value) -> bool {
        self.unknown(size)
            || self.largest(count).zip(self.number(size)).is_some_and(|(count, size)| count <= size)
    }

    /// The largest number this value may work out to, read as an unsigned one.
    ///
    /// The same walk [`Self::strings`] makes, through block parameters and selects, so
    /// `l1 ? sizeof (buf) : 4` is a count of at most the size of `buf` whichever arm was taken. A
    /// parameter the walk reaches again is a count a loop kept, as `l` is in
    /// `builtins/mempcpy-chk.c`, and adds nothing, for the reason it adds no string.
    fn largest(&self, value: Value) -> Option<u128> {
        self.largest_on(value, CHAIN, &mut Vec::new())
    }

    /// The same, with the block parameters whose largest is being worked out.
    fn largest_on(&self, value: Value, depth: u32, on: &mut Vec<Value>) -> Option<u128> {
        if depth == 0 {
            return None;
        }
        match self.func[value].def {
            Def::Param { block, index } => {
                if on.contains(&value) {
                    return Some(0);
                }
                let preds = self.cfg.predecessors(block);
                on.push(value);
                let mut most = None;
                for &pred in preds {
                    let term = self.func.terminator(pred)?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize)?;
                        most = most.max(Some(self.largest_on(arg, depth - 1, on)?));
                    }
                }
                on.pop();
                most
            }
            Def::Result { inst, .. } if self.func[inst].opcode == Opcode::Select => {
                let args = &self.func[self.func[inst].args];
                let (then, other) = (*args.get(1)?, *args.get(2)?);
                let then = self.largest_on(then, depth - 1, on)?;
                Some(then.max(self.largest_on(other, depth - 1, on)?))
            }
            _ => self.number(value).or_else(|| self.bounded(value, depth, on)),
        }
    }

    /// The largest the arithmetic that made this value says it can be. A mask, a remainder by a
    /// constant and a widening of either are what an index worked out to stay inside an array
    /// looks like, as `x++ & 7` is in `builtins/strlen.c`, and nothing else is looked at.
    fn bounded(&self, value: Value, depth: u32, on: &mut Vec<Value>) -> Option<u128> {
        let Def::Result { inst, .. } = self.func[value].def else { return None };
        let args = &self.func[self.func[inst].args];
        let narrow = *args.first()?;
        match self.func[inst].opcode {
            Opcode::And => self.number(narrow).or_else(|| self.number(*args.get(1)?)),
            Opcode::URem => self.number(*args.get(1)?)?.checked_sub(1),
            Opcode::ZExt => self.largest_on(narrow, depth - 1, on),
            // A widening that copies the sign keeps the bound only where the sign bit cannot be
            // set, which is a bound below the top bit of the narrower type.
            Opcode::SExt => {
                let most = self.largest_on(narrow, depth - 1, on)?;
                let top = 1u128.checked_shl(self.func[narrow].ty.bits().checked_sub(1)?)?;
                (most < top).then_some(most)
            }
            _ => None,
        }
    }

    /// The one length every string this value may point at has, as `foo` has in
    /// `builtins/strlen-3.c` after a loop that picks one of four strings of thirteen characters.
    fn length(&self, value: Value) -> Option<usize> {
        let texts = self.strings(value)?;
        let len = texts.first()?.len();
        texts.iter().all(|text| text.len() == len).then_some(len)
    }

    /// The number this value works out to, read as an unsigned one.
    fn number(&self, value: Value) -> Option<u128> {
        crate::fold::evaluated(self.func, value, DEPTH).map(|(imm, _)| imm.unsigned())
    }

    /// The length of the longest string this value may point at.
    fn longest(&self, value: Value) -> Option<u128> {
        self.strings(value)?.iter().map(|text| text.len() as u128).max()
    }

    /// A call to that name, or nothing where this module does not allow one.
    fn call(&self, callee: &'static str, args: Vec<Argument>) -> Option<Plan> {
        let (callee, signature) = self.shapes.get(callee)?;
        Some(Plan::Swap { callee, signature, args, answer: None })
    }

    /// The one string this value points at, or `None` where there is more than one of them.
    fn one(&self, value: Value) -> Option<Vec<u8>> {
        let mut candidates = self.strings(value)?;
        (candidates.len() == 1).then(|| candidates.pop()).flatten()
    }

    /// Every string this value may point at, or `None` where any of them is not one this module
    /// holds.
    ///
    /// A block parameter is every argument every branch to that block passes, which is how the
    /// conditional expression in `builtins/fputs.c` gets a length without anything having turned it
    /// into a `select` first.
    fn strings(&self, value: Value) -> Option<Vec<Vec<u8>>> {
        self.strings_on(value, CHAIN, &mut Vec::new())
    }

    /// The same, with the block parameters whose strings are being worked out.
    ///
    /// A loop that picks a string on some trips and keeps the one it had on the others is a
    /// parameter that is one of its own arguments, as `l` is in `builtins/stpcpy-chk.c`. Reached
    /// again, it can only be a string one of its other arguments already gave it, so it adds
    /// nothing to the list. `depth` still bounds how long a chain is followed.
    fn strings_on(&self, value: Value, depth: u32, on: &mut Vec<Value>) -> Option<Vec<Vec<u8>>> {
        if depth == 0 {
            return None;
        }
        match self.func[value].def {
            Def::Param { block, index } => {
                if on.contains(&value) {
                    return Some(Vec::new());
                }
                let preds = self.cfg.predecessors(block);
                if preds.is_empty() {
                    return None;
                }
                on.push(value);
                let mut all = Vec::new();
                for &pred in preds {
                    let term = self.func.terminator(pred)?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize)?;
                        all.extend(self.strings_on(arg, depth - 1, on)?);
                    }
                }
                on.pop();
                (!all.is_empty()).then_some(all)
            }
            Def::Result { inst, .. } if self.func[inst].opcode == Opcode::Select => {
                let args = &self.func[self.func[inst].args];
                let (then, other) = (*args.get(1)?, *args.get(2)?);
                let mut all = self.strings_on(then, depth - 1, on)?;
                all.extend(self.strings_on(other, depth - 1, on)?);
                Some(all)
            }
            _ => Some(vec![self.literal(value)?]),
        }
    }

    /// The bytes up to the first terminator at the address this value is, where that address is
    /// inside a read only object this module vouches for.
    fn literal(&self, value: Value) -> Option<Vec<u8>> {
        let bytes = self.raw(value)?;
        let end = bytes.iter().position(|&byte| byte == 0)?;
        Some(bytes[..end].to_vec())
    }

    /// Every byte from the address this value is to the end of the object it is in.
    ///
    /// The same walk as above with nothing stopping it at a terminator, because `memchr` is told
    /// how far to read rather than going looking for one, and `strnlen` may be told to stop before
    /// there is one. A caller that wants a string wants [`Self::literal`] instead.
    fn raw(&self, value: Value) -> Option<Vec<u8>> {
        let (base, offset) = self.address(value)?;
        let Def::Result { inst, .. } = self.func[base].def else { return None };
        if self.func[inst].opcode != Opcode::GlobalAddr {
            return None;
        }
        let Extra::Symbol(name) = self.func[inst].extra else { return None };
        let Some(SymbolRef::Global(id)) = self.module.lookup(name) else { return None };
        let global = &self.module[id];
        if !global.constant || !vouched(global, self.pic) {
            return None;
        }
        let mut bytes = Vec::new();
        for &datum in &self.module[global.init?] {
            match datum {
                Datum::Bytes(range) => bytes.extend_from_slice(&self.module[range]),
                Datum::Zero(count) => {
                    bytes.resize(bytes.len().checked_add(usize::try_from(count).ok()?)?, 0);
                }
                // A number written in the target's byte order, or an address the linker has not
                // filled in. Neither is a byte this can read, and what follows one is at an offset
                // that is right only if this one's width is, so the walk stops.
                Datum::Scalar { .. } | Datum::Addr(_) | Datum::Away(_) => return None,
            }
        }
        // An object whose image stops short of its size is zero from there on, which is what an
        // array with fewer initializers than members is and is a byte a search may reach.
        let size = usize::try_from(global.size).ok()?;
        if bytes.len() < size {
            bytes.resize(size, 0);
        }
        Some(bytes.get(usize::try_from(offset).ok()?..)?.to_vec())
    }

    /// The address this value is, as something it was computed from and a distance in bytes from
    /// it.
    ///
    /// The same walk [`crate::image`] does down a chain of `ptr_add` of a constant, because an
    /// index into a string literal is one of these and the frontend writes one per index.
    fn address(&self, mut value: Value) -> Option<(Value, i128)> {
        let mut offset: i128 = 0;
        for _ in 0..DEPTH {
            let Def::Result { inst, .. } = self.func[value].def else {
                return Some((value, offset));
            };
            if self.func[inst].opcode != Opcode::PtrAdd {
                return Some((value, offset));
            }
            let args = &self.func[self.func[inst].args];
            offset = offset.checked_add(self.step(*args.get(1)?)?)?;
            value = *args.first()?;
        }
        None
    }

    /// The distance in bytes this value is, where it works out to a constant.
    ///
    /// An index into an array is an `int` where the source wrote one, and a pointer is sixty four
    /// bits, so what the frontend leaves in front of a `ptr_add` is a `sext` of a constant rather
    /// than a constant, and an index the source worked out, as in `s + (x & 3)` with `x` known, is
    /// still the arithmetic rather than its answer. This runs before anything has folded either,
    /// since everything that would is one function at a time and the function pipeline has not
    /// started, so the walk above looks underneath both with [`crate::fold::evaluated`], which is
    /// the same arithmetic that pass would do later.
    fn step(&self, value: Value) -> Option<i128> {
        let (imm, ty) = crate::fold::evaluated(self.func, value, DEPTH)?;
        Some(imm.signed(ty))
    }
}

/// Writes one plan into the function.
fn apply(
    module: &mut Module,
    id: FuncId,
    names: &mut Interner,
    texts: &mut HashMap<Vec<u8>, Symbol>,
    inst: Inst,
    plan: Plan,
) -> HashMap<Value, Value> {
    let (callee, signature, args, answer) = match plan {
        Plan::Drop => {
            module[id].remove_inst(inst);
            return HashMap::new();
        }
        Plan::Answer(answer) => {
            let width = size(module);
            return answered(&mut module[id], inst, answer, width);
        }
        Plan::Swap { callee, signature, args, answer } => (callee, signature, args, answer),
        Plan::Unchecked { callee, drop } => {
            let func = &mut module[id];
            let Extra::Call(at) = func[inst].extra else { return HashMap::new() };
            let info = func[at];
            let Some(signature) = without(&func[info.signature], drop) else {
                return HashMap::new();
            };
            let values: Vec<Value> = func[func[inst].args]
                .iter()
                .enumerate()
                .filter(|(index, _)| !drop.contains(index))
                .map(|(_, &value)| value)
                .collect();
            let made = call(func, inst, callee, signature, info.varargs, &values);
            return forward(func, inst, made);
        }
    };
    // The objects first, because a string the fold prints belongs to the module and the module is
    // what the function is reached through.
    let symbols: Vec<Option<Symbol>> = args
        .iter()
        .map(|arg| match arg {
            Argument::Text(bytes) => Some(object(module, names, texts, bytes)),
            _ => None,
        })
        .collect();
    let width = size(module);
    let func = &mut module[id];
    let span = func.span(inst);
    let mut values = Vec::with_capacity(args.len());
    for (arg, symbol) in args.iter().zip(symbols) {
        values.push(match arg {
            Argument::Have(value) => *value,
            Argument::Char(byte) => constant(func, inst, int(), i128::from(*byte)),
            Argument::Count(count) => constant(func, inst, width, i128::from(*count)),
            Argument::Text(_) => {
                let extra = Extra::Symbol(symbol.expect("a text argument has an object"));
                let data = InstData { extra, ..InstData::new(Opcode::GlobalAddr) };
                let made = func.create_inst(data, &[Type::PTR], span);
                func.insert_before(made, inst);
                func[made].results().next().expect("an address is one value")
            }
        });
    }
    let varargs = func.push_abis(&[]);
    let made = call(func, inst, callee, signature, varargs, &values);
    match answer {
        Some(answer) => answered(func, inst, answer, width),
        None => forward(func, inst, made),
    }
}

/// The function a checking call is where the check is taken off, which is its name without the
/// `__` in front and the `_chk` behind.
fn plain(name: &str) -> &str {
    name.strip_prefix("__").and_then(|rest| rest.strip_suffix("_chk")).unwrap_or(name)
}

/// A call to that function with those arguments, put in front of the call being replaced.
fn call(
    func: &mut Func,
    before: Inst,
    callee: Symbol,
    signature: Signature,
    varargs: AbiList,
    values: &[Value],
) -> Inst {
    let span = func.span(before);
    let results: Vec<Type> = signature.return_types().collect();
    let sig = func.add_signature(signature);
    let info = func.add_call(CallInfo { callee: Some(callee), signature: sig, varargs });
    let args = func.push_values(values);
    let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
    let made = func.create_inst(data, &results, span);
    func.insert_before(made, before);
    made
}

/// Hands whoever read the old call's answer the new one's, takes the old call away, and says what
/// was renamed.
fn forward(func: &mut Func, old: Inst, new: Inst) -> HashMap<Value, Value> {
    // Only where the two are the same kind of thing. The printf family is folded only where nothing
    // read it, so the map is empty there and this costs a walk over a function that is about to be
    // walked anyway.
    let forward: HashMap<Value, Value> = func[old]
        .results()
        .zip(func[new].results().collect::<Vec<Value>>())
        .filter(|&(from, to)| func[from].ty == func[to].ty)
        .collect();
    if !forward.is_empty() {
        uses::substitute(func, &forward);
    }
    func.remove_inst(old);
    forward
}

/// That signature with the parameters at those places taken out, or `None` where one of the places
/// is not a parameter it names.
fn without(signature: &Signature, drop: &[usize]) -> Option<Signature> {
    drop.iter().all(|&index| index < signature.params.len()).then_some(())?;
    let params = signature
        .params
        .iter()
        .enumerate()
        .filter(|(index, _)| !drop.contains(index))
        .map(|(_, param)| *param)
        .collect();
    Some(Signature { params, ..signature.clone() })
}

/// Writes the answer a search worked out in place of the call that would have worked it out, and
/// says what was renamed.
fn answered(func: &mut Func, inst: Inst, answer: Answer, width: Type) -> HashMap<Value, Value> {
    let span = func.span(inst);
    let value = match answer {
        // The haystack itself, which is what a search for the empty string finds and what a search
        // that found its needle at the front of one finds. No instruction at all for either.
        Answer::Along(haystack, 0) => haystack,
        Answer::Along(haystack, by) => {
            let step = constant(func, inst, width, i128::from(by));
            let args = func.push_values(&[haystack, step]);
            let data = InstData { args, ..InstData::new(Opcode::PtrAdd) };
            let made = func.create_inst(data, &[Type::PTR], span);
            func.insert_before(made, inst);
            func[made].results().next().expect("an address is one value")
        }
        Answer::Nowhere => {
            let zero = constant(func, inst, width, 0);
            let args = func.push_values(&[zero]);
            let data = InstData { args, ..InstData::new(Opcode::IntToPtr) };
            let made = func.create_inst(data, &[Type::PTR], span);
            func.insert_before(made, inst);
            func[made].results().next().expect("a null pointer is one value")
        }
        Answer::Number(number) => {
            let ty = func[inst]
                .results()
                .next()
                .map(|result| func[result].ty)
                .expect("a call whose answer is a number has one");
            constant(func, inst, ty, number)
        }
        Answer::Least { count, len } => {
            let ty = func[count].ty;
            let len = constant(func, inst, ty, i128::from(len));
            let args = func.push_values(&[count, len]);
            let data = InstData {
                args,
                extra: Extra::IntPred(IntPred::Ult),
                ..InstData::new(Opcode::ICmp)
            };
            let made = func.create_inst(data, &[Type::I1], span);
            func.insert_before(made, inst);
            let shorter = func[made].results().next().expect("a comparison is one value");
            let args = func.push_values(&[shorter, count, len]);
            let data = InstData { args, ..InstData::new(Opcode::Select) };
            let made = func.create_inst(data, &[ty], span);
            func.insert_before(made, inst);
            func[made].results().next().expect("a choice is one value")
        }
        Answer::Less { len, step } => {
            let ty = func[inst]
                .results()
                .next()
                .map(|result| func[result].ty)
                .expect("a call whose answer is a length has one");
            let step = resize(func, inst, step, ty);
            let len = constant(func, inst, ty, i128::from(len));
            let args = func.push_values(&[len, step]);
            let data = InstData { args, ..InstData::new(Opcode::Sub) };
            let made = func.create_inst(data, &[ty], span);
            func.insert_before(made, inst);
            func[made].results().next().expect("a difference is one value")
        }
        Answer::Byte { of, against, leading } => {
            let ty = func[inst]
                .results()
                .next()
                .map(|result| func[result].ty)
                .expect("a call whose answer is a byte has one");
            let read = read(func, inst, of);
            // An `unsigned char` is what the standard says a comparison compares, so the byte goes
            // into the wider type without its top bit being read as a sign.
            let args = func.push_values(&[read]);
            let data = InstData { args, ..InstData::new(Opcode::ZExt) };
            let made = func.create_inst(data, &[ty], span);
            func.insert_before(made, inst);
            let wide = func[made].results().next().expect("a conversion is one value");
            let other = constant(func, inst, ty, i128::from(against));
            let pair = if leading { [other, wide] } else { [wide, other] };
            let args = func.push_values(&pair);
            let data = InstData { args, ..InstData::new(Opcode::Sub) };
            let made = func.create_inst(data, &[ty], span);
            func.insert_before(made, inst);
            func[made].results().next().expect("a difference is one value")
        }
    };
    let forward: HashMap<Value, Value> =
        func[inst].results().map(|result| (result, value)).collect();
    uses::substitute(func, &forward);
    func.remove_inst(inst);
    forward
}

/// How two strings compare, over however many bytes the comparison is allowed to read.
fn walk(left: &[u8], right: &[u8], bound: usize) -> i128 {
    // The terminator is part of the comparison, since it is what stops one string before the other
    // and it is smaller than every byte that could be opposite it.
    for at in 0..bound.min(left.len() + 1).min(right.len() + 1) {
        let (this, that) =
            (left.get(at).copied().unwrap_or(0), right.get(at).copied().unwrap_or(0));
        if this != that {
            return if this < that { -1 } else { 1 };
        }
        if this == 0 {
            break;
        }
    }
    0
}

/// Where the second string is inside the first, in bytes from its front.
fn at(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// The byte at that address, read in front of the call being replaced.
///
/// A comparison reads the first byte of both of its strings before it can answer anything, so this
/// read is one the call was going to make and is safe wherever the call itself was.
fn read(func: &mut Func, before: Inst, from: Value) -> Value {
    let span = func.span(before);
    let mem = func.add_mem(MemInfo {
        size: 1,
        align: 1,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    });
    let args = func.push_values(&[from]);
    let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) };
    let made = func.create_inst(data, &[Type::int(8)], span);
    func.insert_before(made, before);
    func[made].results().next().expect("a load is one value")
}

/// That value in an integer type of another width, put in front of the call being replaced. The
/// value is known not to be negative, so a wider type takes it without its sign.
fn resize(func: &mut Func, before: Inst, value: Value, ty: Type) -> Value {
    let opcode = match func[value].ty.bits().cmp(&ty.bits()) {
        std::cmp::Ordering::Equal => return value,
        std::cmp::Ordering::Less => Opcode::ZExt,
        std::cmp::Ordering::Greater => Opcode::Trunc,
    };
    let span = func.span(before);
    let args = func.push_values(&[value]);
    let made = func.create_inst(InstData { args, ..InstData::new(opcode) }, &[ty], span);
    func.insert_before(made, before);
    func[made].results().next().expect("a conversion is one value")
}

/// An integer constant of that type, put in front of the call being replaced.
fn constant(func: &mut Func, before: Inst, ty: Type, value: i128) -> Value {
    let span = func.span(before);
    let imm = func.add_imm(Imm::int(value, ty.lane()));
    let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, before);
    func[made].results().next().expect("a constant is one value")
}

/// The read only object holding these bytes and a terminator, making it the first time it is asked
/// for.
///
/// `.Lfold` rather than `.Lstr`, so that this numbering and the frontend's cannot meet, and a
/// number past the end of the table in the case where a program has named one of these itself.
fn object(
    module: &mut Module,
    names: &mut Interner,
    texts: &mut HashMap<Vec<u8>, Symbol>,
    bytes: &[u8],
) -> Symbol {
    if let Some(&symbol) = texts.get(bytes) {
        return symbol;
    }
    let mut image = bytes.to_vec();
    image.push(0);
    let mut symbol = names.intern(&format!(".Lfold.{}", texts.len()));
    for next in texts.len().. {
        if module.lookup(symbol).is_none() {
            break;
        }
        symbol = names.intern(&format!(".Lfold.{}", next + 1));
    }
    let mut global = Global::new(symbol, image.len() as u64, 1);
    global.linkage = Linkage::Internal;
    global.constant = true;
    let range = module.push_bytes(&image);
    global.init = Some(module.push_data(&[Datum::Bytes(range)]));
    module.add_global(global);
    texts.insert(bytes.to_vec(), symbol);
    symbol
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What every fixture below starts with, which is the target the counts and widths are of.
    const HEAD: &str = "\
; ModuleID = 't.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
";

    /// The module that text is, folded, printed, and checked by the verifier on the way out.
    ///
    /// Printing it rather than walking it, because what a reader of one of these tests wants to
    /// know is what the function ended up being, and a chain of accessors says that less clearly
    /// than the line it produces.
    fn folded(body: &str) -> String {
        run(body, &[], &mut Fuel::unlimited())
    }

    /// The same, under whatever `-fno-builtin-<name>` and fuel the test wants.
    fn run(body: &str, no_builtin: &[String], fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        fold(&mut module, &mut names, no_builtin, Pic::Executable, fuel);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the fold left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// A `printf` of a format holding no `%` and ending in a newline is a `puts` of the rest of it.
    ///
    /// The rest of it is a string this module did not hold, because "hello world" terminated is not
    /// a suffix of "hello world\n" terminated, so the fold has to leave an object behind as well as
    /// a call.
    #[test]
    fn a_format_that_ends_in_a_newline_is_written_by_puts() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 13 = { bytes "hello world\0a\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @printf(%0) : (ptr, ...) -> i32
    return
}
"#,
        );
        assert!(out.contains("call @puts("), "{out}");
        assert!(!out.contains("call @printf("), "{out}");
        assert!(out.contains(r#"@.Lfold.0 : bytes 12 = { bytes "hello world\00" }"#), "{out}");
    }

    /// A one character format is a `putchar` of that character, and an empty one is nothing at all.
    #[test]
    fn a_short_format_is_written_by_putchar_or_by_nothing() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 2 = { bytes "x\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @printf(%0) : (ptr, ...) -> i32
    %2 = global_addr @.Lstr.1
    %3 = call @printf(%2) : (ptr, ...) -> i32
    return
}
"#,
        );
        assert!(out.contains("iconst.i32 120"), "the character is the argument, {out}");
        assert!(out.contains("call @putchar("), "{out}");
        assert_eq!(out.matches("call @").count(), 1, "the empty one is gone, {out}");
    }

    /// `printf("%s\n", p)` is `puts(p)` and `printf("%c", c)` is `putchar(c)`, whatever the
    /// argument is.
    #[test]
    fn the_two_formats_that_are_a_call_on_their_own_are_folded_for_any_argument() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 4 = { bytes "%s\0a\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 3 = { bytes "%c\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);

func @g(ptr, i32), linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = global_addr @.Lstr.0
    %3 = call @printf(%2, %0) : (ptr, ...) -> i32
    %4 = global_addr @.Lstr.1
    %5 = call @printf(%4, %1) : (ptr, ...) -> i32
    return
}
"#,
        );
        assert!(out.contains("call @puts(%0)"), "{out}");
        assert!(out.contains("call @putchar(%1)"), "{out}");
    }

    /// `printf("%s", p)` where `p` is not a string this module holds stays as it is.
    ///
    /// There is no stream argument to hand to `fputs`, and `stdout` is not a name a compiler may
    /// invent. gcc stops in the same place.
    #[test]
    fn a_string_argument_nothing_is_known_about_is_left_to_printf() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 3 = { bytes "%s\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @printf(%1, %0) : (ptr, ...) -> i32
    return
}
"#,
        );
        assert!(out.contains("call @printf("), "{out}");
    }

    /// The `fprintf` list, which is the `printf` one with a stream in hand.
    ///
    /// A format holding no `%` is an `fwrite` of the whole of it rather than a `puts` of part of
    /// it, since `fwrite` is told how many bytes to write and adds no newline of its own.
    #[test]
    fn a_stream_takes_the_whole_format_through_fwrite() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 13 = { bytes "hello world\0a\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 2 = { bytes "q\00" }, align 1, linkage(internal), constant

func @fprintf(ptr, ptr, ...) -> i32, linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @fprintf(%0, %1) : (ptr, ptr, ...) -> i32
    %3 = global_addr @.Lstr.1
    %4 = call @fprintf(%0, %3) : (ptr, ptr, ...) -> i32
    return
}
"#,
        );
        assert!(out.contains("call @fwrite("), "{out}");
        assert!(out.contains("iconst.i64 12"), "the whole format, newline and all, {out}");
        assert!(out.contains("call @fputc("), "{out}");
        assert!(!out.contains("call @fprintf("), "{out}");
    }

    /// `fprintf(s, "%s", p)` is `fputs(p, s)` however little is known about `p`, which is the fold
    /// `printf` cannot have.
    #[test]
    fn a_string_argument_with_a_stream_beside_it_becomes_fputs() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 3 = { bytes "%s\00" }, align 1, linkage(internal), constant

func @fprintf(ptr, ptr, ...) -> i32, linkage(external);

func @g(ptr, ptr), linkage(external) {
block0(%0: ptr, %1: ptr):
    %2 = global_addr @.Lstr.0
    %3 = call @fprintf(%0, %2, %1) : (ptr, ptr, ...) -> i32
    return
}
"#,
        );
        assert!(out.contains("call @fputs(%1, %0)"), "{out}");
    }

    /// An `fputs` of a string whose length is known is an `fwrite` of that many bytes, one of a
    /// single character is an `fputc`, and one of nothing is nothing.
    #[test]
    fn fputs_of_a_string_this_module_holds_is_folded_by_its_length() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 7 = { bytes "abcdef\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 2 = { bytes "z\00" }, align 1, linkage(internal), constant
global @.Lstr.2 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @fputs(ptr, ptr) -> i32, linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @fputs(%1, %0) : (ptr, ptr) -> i32
    %3 = global_addr @.Lstr.1
    %4 = call @fputs(%3, %0) : (ptr, ptr) -> i32
    %5 = global_addr @.Lstr.2
    %6 = call @fputs(%5, %0) : (ptr, ptr) -> i32
    return
}
"#,
        );
        assert!(out.contains("call @fwrite("), "{out}");
        assert!(out.contains("iconst.i64 6"), "{out}");
        assert!(out.contains("iconst.i32 122"), "{out}");
        assert!(out.contains("call @fputc("), "{out}");
        assert!(!out.contains("call @fputs("), "the empty one is gone too, {out}");
    }

    /// An index into a string literal is a string as well, which is what `fputs(s1 + 6, s)` is.
    ///
    /// The index is an `int` widened to the width of a pointer, because that is what the frontend
    /// writes for an index the source wrote as one and nothing has folded it yet where this runs.
    /// An index landing on the terminator is the empty string, so that call goes altogether.
    #[test]
    fn an_index_into_a_literal_is_a_string_of_its_own() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @fputs(ptr, ptr) -> i32, linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = iconst.i32 6
    %3 = sext.i64 %2
    %4 = ptr_add %1, %3
    %5 = call @fputs(%4, %0) : (ptr, ptr) -> i32
    %6 = iconst.i32 11
    %7 = sext.i64 %6
    %8 = ptr_add %1, %7
    %9 = call @fputs(%8, %0) : (ptr, ptr) -> i32
    return
}
"#,
        );
        assert!(out.contains("iconst.i64 5"), "world without its terminator, {out}");
        assert!(out.contains("call @fwrite("), "{out}");
        assert!(!out.contains("call @fputs("), "and the terminator itself is nothing, {out}");
    }

    /// A conditional expression whose arms are two literals of one length is folded, and one whose
    /// arms are two lengths is not.
    ///
    /// The arms reach the call through a block parameter rather than through a `select`, because
    /// that is what the lowering walk builds for a conditional expression, so the walk that answers
    /// what string a value is has to go up through the branches to find them.
    #[test]
    fn a_choice_between_two_literals_is_folded_when_they_are_the_same_length() {
        let text = r#"
global @.Lstr.0 : bytes 2 = { bytes "f\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 2 = { bytes "x\00" }, align 1, linkage(internal), constant
global @.Lstr.2 : bytes 4 = { bytes "abc\00" }, align 1, linkage(internal), constant

func @fputs(ptr, ptr) -> i32, linkage(external);

func @g(ptr, i1), linkage(external) {
block0(%0: ptr, %1: i1):
    %2 = global_addr @.LEFT
    %3 = global_addr @.Lstr.1
    br_if %1, block1(%2), block1(%3)
block1(%4: ptr):
    %5 = call @fputs(%4, %0) : (ptr, ptr) -> i32
    return
}
"#;
        let same = folded(&text.replace(".LEFT", ".Lstr.0"));
        assert!(same.contains("call @fwrite("), "{same}");
        assert!(same.contains("iconst.i64 1"), "{same}");

        let differing = folded(&text.replace(".LEFT", ".Lstr.2"));
        assert!(differing.contains("call @fputs("), "{differing}");
    }

    /// A call whose result something reads is left alone.
    ///
    /// `printf` answers how many characters it wrote and `puts` answers a number that is not that
    /// count, so a program looking at the answer is a program this may not touch.
    #[test]
    fn a_call_whose_answer_is_read_is_not_folded() {
        let out = folded(
            r#"
global @n : bytes 4 = { zero 4 }, align 4, linkage(external)
global @.Lstr.0 : bytes 3 = { bytes "a\0a\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @printf(%0) : (ptr, ...) -> i32
    %2 = global_addr @n
    store %1 -> %2, align 4
    return
}
"#,
        );
        assert!(out.contains("call @printf("), "{out}");
    }

    /// A body the program wrote for a standard name does not stop a call to that name being folded,
    /// which is what gcc 16 does and what `execute/vprintf-chk-1.c` checks by defining the checking
    /// function above the calls it expects to be folded away. The body itself is left alone, so a
    /// `puts` of the program's own that prints with `printf` does not become a call to itself.
    #[test]
    fn a_body_for_a_standard_name_is_not_folded_inside_and_does_not_stop_the_fold() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 3 = { bytes "a\0a\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);

func @puts(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @printf(%1) : (ptr, ...) -> i32
    %3 = iconst.i32 0
    return %3
}

func @__vprintf_chk(i32, ptr, ptr) -> i32, linkage(external) {
block0(%0: i32, %1: ptr, %2: ptr):
    %3 = iconst.i32 0
    return %3
}

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i32 1
    %2 = global_addr @.Lstr.0
    %3 = call @__vprintf_chk(%1, %2, %0) : (i32, ptr, ptr) -> i32
    return
}
"#,
        );
        assert!(!out.contains("call @__vprintf_chk("), "{out}");
        assert_eq!(out.matches("call @puts(").count(), 1, "{out}");
        assert!(out.contains("call @printf("), "the body of `puts` keeps its own call, {out}");
    }

    /// A program that declared `puts` as something else keeps the call it had.
    ///
    /// The verifier holds that a call to a name this module declares carries that name's signature,
    /// so the fold either agrees with the declaration or does not happen. A fold that went ahead
    /// here would produce IR the compiler itself refuses.
    #[test]
    fn a_declaration_of_another_shape_stops_the_fold() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 13 = { bytes "hello world\0a\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);
func @puts(ptr, i32) -> i32, linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @printf(%0) : (ptr, ...) -> i32
    return
}
"#,
        );
        assert!(out.contains("call @printf("), "{out}");
        assert!(!out.contains("@.Lfold."), "and no object was left behind either, {out}");
    }

    /// A variable by one of those names stops it as well, since a variable is not a function to
    /// call.
    #[test]
    fn a_variable_by_the_name_of_a_replacement_stops_the_fold() {
        let out = folded(
            r#"
global @putchar : bytes 4 = { zero 4 }, align 4, linkage(external)
global @.Lstr.0 : bytes 2 = { bytes "x\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @printf(%0) : (ptr, ...) -> i32
    return
}
"#,
        );
        assert!(out.contains("call @printf("), "{out}");
    }

    /// An `_unlocked` spelling gets the one fold that names no function.
    ///
    /// A system need not have a `puts_unlocked` for the compiler to name, which is the reason the
    /// torture program gives and the place gcc stops too.
    #[test]
    fn the_unlocked_spellings_are_only_removed_when_they_write_nothing() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 13 = { bytes "hello world\0a\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @printf_unlocked(ptr, ...) -> i32, linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @printf_unlocked(%0) : (ptr, ...) -> i32
    %2 = global_addr @.Lstr.1
    %3 = call @printf_unlocked(%2) : (ptr, ...) -> i32
    return
}
"#,
        );
        assert_eq!(out.matches("call @printf_unlocked(").count(), 1, "{out}");
        assert!(!out.contains("call @puts("), "{out}");
    }

    /// `-fno-builtin-printf` takes one name away and leaves the rest of the family folded.
    #[test]
    fn one_name_can_be_taken_away_without_taking_the_family_away() {
        let body = r#"
global @.Lstr.0 : bytes 2 = { bytes "x\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);
func @fputs(ptr, ptr) -> i32, linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @printf(%1) : (ptr, ...) -> i32
    %3 = call @fputs(%1, %0) : (ptr, ptr) -> i32
    return
}
"#;
        let out = run(body, &["printf".to_owned()], &mut Fuel::unlimited());
        assert!(out.contains("call @printf("), "{out}");
        assert!(out.contains("call @fputc("), "and the other one still folded, {out}");
    }

    /// Fuel stops it, which is what a bisection over a miscompilation needs of every transformation
    /// here.
    #[test]
    fn a_run_out_of_fuel_transforms_nothing() {
        let body = r#"
global @.Lstr.0 : bytes 2 = { bytes "x\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @printf(%0) : (ptr, ...) -> i32
    return
}
"#;
        let mut fuel = Fuel::of(0);
        let out = run(body, &[], &mut fuel);
        assert!(out.contains("call @printf("), "{out}");
        assert_eq!(fuel.spent(), 0);
    }

    /// Two calls folded to the same string share one object rather than getting one each.
    #[test]
    fn one_object_serves_every_call_that_prints_the_same_thing() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 4 = { bytes "hi\0a\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @printf(%0) : (ptr, ...) -> i32
    %2 = call @printf(%0) : (ptr, ...) -> i32
    return
}

func @h(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @printf(%0) : (ptr, ...) -> i32
    return
}
"#,
        );
        assert_eq!(out.matches("@.Lfold.0 : bytes").count(), 1, "{out}");
        assert!(!out.contains("@.Lfold.1"), "{out}");
        assert_eq!(out.matches("call @puts(").count(), 3, "{out}");
    }

    /// A search for the empty string finds it at the front of whatever it was given.
    ///
    /// The haystack need not be a string this module holds, because the answer does not depend on
    /// what is in it.
    #[test]
    fn a_search_for_nothing_answers_with_the_haystack_itself() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @strstr(ptr, ptr) -> ptr, linkage(external);

func @g(ptr) -> ptr, linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strstr(%0, %1) : (ptr, ptr) -> ptr
    return %2
}
"#,
        );
        assert!(!out.contains("call @strstr("), "{out}");
        assert!(out.contains("return %0"), "{out}");
    }

    /// Two strings this module holds answer themselves, at a place in the first or nowhere in it.
    #[test]
    fn two_strings_this_module_holds_answer_without_a_call() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 2 = { bytes "w\00" }, align 1, linkage(internal), constant
global @.Lstr.2 : bytes 3 = { bytes "zz\00" }, align 1, linkage(internal), constant

func @strstr(ptr, ptr) -> ptr, linkage(external);
func @use(ptr, ptr), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = global_addr @.Lstr.1
    %2 = call @strstr(%0, %1) : (ptr, ptr) -> ptr
    %3 = global_addr @.Lstr.2
    %4 = call @strstr(%0, %3) : (ptr, ptr) -> ptr
    call @use(%2, %4) : (ptr, ptr)
    return
}
"#,
        );
        assert!(!out.contains("call @strstr("), "{out}");
        assert!(out.contains("ptr_add %0, "), "the w is six bytes along, {out}");
        assert!(out.contains("iconst.i64 6"), "{out}");
        assert!(out.contains("inttoptr"), "and the zz is nowhere in it, {out}");
    }

    /// A one character needle is a search for a character, which `strchr` is the name of.
    #[test]
    fn a_one_character_needle_becomes_a_search_for_that_character() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 2 = { bytes "o\00" }, align 1, linkage(internal), constant

func @strstr(ptr, ptr) -> ptr, linkage(external);

func @g(ptr) -> ptr, linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strstr(%0, %1) : (ptr, ptr) -> ptr
    return %2
}
"#,
        );
        assert!(!out.contains("call @strstr("), "{out}");
        assert!(out.contains("call @strchr(%0, "), "{out}");
        assert!(out.contains("iconst.i32 111"), "{out}");
    }

    /// A module whose `strchr` is something else of that name keeps its `strstr` call.
    #[test]
    fn a_strchr_of_another_shape_is_not_the_one_to_call() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 2 = { bytes "o\00" }, align 1, linkage(internal), constant

func @strstr(ptr, ptr) -> ptr, linkage(external);
func @strchr(ptr, ptr) -> ptr, linkage(external);

func @g(ptr) -> ptr, linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strstr(%0, %1) : (ptr, ptr) -> ptr
    return %2
}
"#,
        );
        assert!(out.contains("call @strstr("), "{out}");
    }

    /// A declaration renamed by an assembler name is the function the standard describes still.
    ///
    /// The call names the symbol the rename asked for, and the fold reads the spelling beside it,
    /// which is what `gcc.c-torture/execute/builtins/strstr-asm.c` is written to catch.
    #[test]
    fn a_renamed_declaration_is_still_the_function_it_was_spelled() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @my_strstr(ptr, ptr) -> ptr, linkage(external), spelled "strstr";

func @g(ptr) -> ptr, linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @my_strstr(%0, %1) : (ptr, ptr) -> ptr
    return %2
}
"#,
        );
        assert!(!out.contains("call @my_strstr("), "{out}");
        assert!(out.contains("return %0"), "{out}");
    }

    /// A module that renamed `strchr` gets a call to the symbol it renamed it to.
    #[test]
    fn a_renamed_replacement_is_called_by_the_symbol_the_rename_asked_for() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 2 = { bytes "o\00" }, align 1, linkage(internal), constant

func @strstr(ptr, ptr) -> ptr, linkage(external);
func @my_strchr(ptr, i32) -> ptr, linkage(external), spelled "strchr";

func @g(ptr) -> ptr, linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strstr(%0, %1) : (ptr, ptr) -> ptr
    return %2
}
"#,
        );
        assert!(out.contains("call @my_strchr(%0, "), "{out}");
        assert!(!out.contains("call @strchr("), "{out}");
    }

    /// `-fno-builtin-strstr` leaves the call alone.
    #[test]
    fn a_strstr_taken_away_is_a_call_like_any_other() {
        let body = r#"
global @.Lstr.0 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @strstr(ptr, ptr) -> ptr, linkage(external);

func @g(ptr) -> ptr, linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strstr(%0, %1) : (ptr, ptr) -> ptr
    return %2
}
"#;
        let out = run(body, &["strstr".to_owned()], &mut Fuel::unlimited());
        assert!(out.contains("call @strstr("), "{out}");
    }

    /// `strlen` of a string this module holds is its length, and of a place inside one is the rest.
    #[test]
    fn strlen_of_a_string_this_module_holds_is_a_number() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @strlen(ptr) -> i64, linkage(external);
func @use(i64, i64), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = call @strlen(%0) : (ptr) -> i64
    %2 = iconst.i64 6
    %3 = ptr_add %0, %2
    %4 = call @strlen(%3) : (ptr) -> i64
    call @use(%1, %4) : (i64, i64)
    return
}
"#,
        );
        assert!(!out.contains("call @strlen("), "{out}");
        assert!(out.contains("iconst.i64 11"), "{out}");
        assert!(out.contains("iconst.i64 5"), "the world on its own, {out}");
    }

    /// `memmove` is `memcpy` where the two sides cannot overlap and the destination where it moves
    /// nothing, and `bcopy` is the same with its addresses the other way round.
    #[test]
    fn a_move_that_cannot_overlap_is_a_copy() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 6 = { bytes "abcde\00" }, align 1, linkage(internal), constant
global @p : bytes 32 = { zero 32 }, align 16, linkage(external)

func @memmove(ptr, ptr, i64) -> ptr, linkage(external);
func @bcopy(ptr, ptr, i64), linkage(external);
func @use(ptr, ptr, ptr, ptr), linkage(external);

func @g(ptr, i64), linkage(external) {
block0(%0: ptr, %1: i64):
    %2 = global_addr @p
    %3 = global_addr @.Lstr.0
    %4 = iconst.i64 6
    %5 = call @memmove(%2, %3, %4) : (ptr, ptr, i64) -> ptr
    %6 = iconst.i64 2
    %7 = ptr_add %2, %6
    %8 = iconst.i64 3
    %9 = ptr_add %2, %8
    %10 = iconst.i64 1
    %11 = call @memmove(%7, %9, %10) : (ptr, ptr, i64) -> ptr
    %12 = iconst.i64 0
    %13 = call @memmove(%7, %0, %12) : (ptr, ptr, i64) -> ptr
    call @bcopy(%9, %7, %10) : (ptr, ptr, i64)
    %14 = alloca, size 8, align 8
    %15 = call @memmove(%14, %0, %1) : (ptr, ptr, i64) -> ptr
    %16 = call @memmove(%7, %9, %1) : (ptr, ptr, i64) -> ptr
    call @use(%5, %11, %13, %16) : (ptr, ptr, ptr, ptr)
    return
}
"#,
        );
        assert_eq!(out.matches("call @memcpy(").count(), 3, "{out}");
        assert!(!out.contains("call @bcopy("), "{out}");
        assert_eq!(
            out.matches("call @memmove(").count(),
            2,
            "a local and a pointer from outside, and two places in one object, stay moves, {out}"
        );
    }

    /// `strcpy` of a pointer that may be any of several strings of one length is a copy of that
    /// many bytes and a terminator.
    #[test]
    fn strcpy_of_a_choice_of_one_length_is_a_copy() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 4 = { bytes "abc\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 4 = { bytes "xyz\00" }, align 1, linkage(internal), constant

func @strcpy(ptr, ptr) -> ptr, linkage(external);
func @use(ptr), linkage(external);

func @g(ptr, i1), linkage(external) {
block0(%0: ptr, %1: i1):
    %2 = global_addr @.Lstr.0
    %3 = global_addr @.Lstr.1
    br_if %1, block1(%2), block1(%3)
block1(%4: ptr):
    %5 = call @strcpy(%0, %4) : (ptr, ptr) -> ptr
    call @use(%5) : (ptr)
    return
}
"#,
        );
        assert!(out.contains("call @memcpy("), "{out}");
        assert!(out.contains("iconst.i64 4"), "{out}");
    }

    /// `strlen` of a local array the block has just written a string into is that string's length,
    /// from the front or from part way along, and a call in between leaves it a call.
    #[test]
    fn strlen_of_what_stores_just_wrote_is_its_length() {
        let text = r#"
func @strlen(ptr) -> i64, linkage(external);
func @use(i64, i64), linkage(external);
func @touch(), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = alloca, size 8, align 1
    %1 = alloca, size 8, align 1
    %2 = iconst.i8 110
    store %2 -> %0, align 1
    %3 = iconst.i64 1
    %4 = ptr_add %0, %3
    %5 = iconst.i8 116
    store %5 -> %4, align 1
    %6 = iconst.i64 2
    %7 = ptr_add %0, %6
    %8 = iconst.i8 0
    store %8 -> %7, align 1
    store %8 -> %1, align 1
    CALL
    %9 = call @strlen(%0) : (ptr) -> i64
    %10 = call @strlen(%4) : (ptr) -> i64
    call @use(%9, %10) : (i64, i64)
    return
}
"#;
        let out = folded(&text.replace("CALL", ""));
        assert!(!out.contains("call @strlen("), "{out}");
        assert!(out.contains("iconst.i64 2"), "{out}");

        let out = folded(&text.replace("CALL", "call @touch() : ()"));
        assert_eq!(out.matches("call @strlen(").count(), 2, "{out}");
    }

    /// `strlen` of a pointer that may be either of two strings is their length where they share
    /// one, and stays a call where they do not.
    #[test]
    fn strlen_of_a_choice_between_literals_of_one_length_is_that_length() {
        let text = r#"
global @.Lstr.0 : bytes 4 = { bytes "abc\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 4 = { bytes "xyz\00" }, align 1, linkage(internal), constant
global @.Lstr.2 : bytes 3 = { bytes "ab\00" }, align 1, linkage(internal), constant

func @strlen(ptr) -> i64, linkage(external);
func @use(i64), linkage(external);

func @g(i1), linkage(external) {
block0(%0: i1):
    %1 = global_addr @.LEFT
    %2 = global_addr @.Lstr.1
    br_if %0, block1(%1), block1(%2)
block1(%3: ptr):
    %4 = call @strlen(%3) : (ptr) -> i64
    call @use(%4) : (i64)
    return
}
"#;
        let same = folded(&text.replace(".LEFT", ".Lstr.0"));
        assert!(!same.contains("call @strlen("), "{same}");
        assert!(same.contains("iconst.i64 3"), "{same}");

        let differing = folded(&text.replace(".LEFT", ".Lstr.2"));
        assert!(differing.contains("call @strlen("), "{differing}");
    }

    /// `strlen` at a place inside a held string the program worked out is the length less the step,
    /// where the step cannot go past the terminator. A mask of seven fits inside eleven and a mask
    /// of fifteen does not, so only the first call goes.
    #[test]
    fn strlen_at_a_bounded_step_into_a_held_string_is_the_rest() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @strlen(ptr) -> i64, linkage(external);
func @use(i64, i64), linkage(external);

func @g(i32), linkage(external) {
block0(%0: i32):
    %1 = global_addr @.Lstr.0
    %2 = iconst.i32 7
    %3 = and %0, %2
    %4 = sext.i64 %3
    %5 = ptr_add %1, %4
    %6 = call @strlen(%5) : (ptr) -> i64
    %7 = iconst.i32 15
    %8 = and %0, %7
    %9 = sext.i64 %8
    %10 = ptr_add %1, %9
    %11 = call @strlen(%10) : (ptr) -> i64
    call @use(%6, %11) : (i64, i64)
    return
}
"#,
        );
        assert_eq!(out.matches("call @strlen(").count(), 1, "{out}");
        assert!(out.contains("sub %6, %4"), "eleven less the step, {out}");
        assert!(out.contains("iconst.i64 11"), "{out}");
    }

    /// `strnlen` stops at its count, and the count is what the answer is where nothing terminated
    /// the string inside it.
    #[test]
    fn strnlen_answers_the_count_where_the_string_runs_past_it() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @strnlen(ptr, i64) -> i64, linkage(external);
func @use(i64, i64), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = iconst.i64 3
    %2 = call @strnlen(%0, %1) : (ptr, i64) -> i64
    %3 = iconst.i64 40
    %4 = call @strnlen(%0, %3) : (ptr, i64) -> i64
    call @use(%2, %4) : (i64, i64)
    return
}
"#,
        );
        assert!(!out.contains("call @strnlen("), "{out}");
        assert!(out.contains("iconst.i64 3"), "the count came first, {out}");
        assert!(out.contains("iconst.i64 11"), "the terminator came first, {out}");
    }

    /// A string the module holds is read no further than its terminator, so a count nothing is
    /// known about makes the answer the smaller of the two, and a count written as a negative number
    /// is a very large one.
    #[test]
    fn strnlen_of_a_string_this_module_holds_takes_any_count() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 4 = { bytes "123\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @strnlen(ptr, i64) -> i64, linkage(external);
func @use(i64, i64, i64), linkage(external);

func @g(i64), linkage(external) {
block0(%0: i64):
    %1 = global_addr @.Lstr.0
    %2 = call @strnlen(%1, %0) : (ptr, i64) -> i64
    %3 = iconst.i32 -2
    %4 = sext.i64 %3
    %5 = call @strnlen(%1, %4) : (ptr, i64) -> i64
    %6 = global_addr @.Lstr.1
    %7 = call @strnlen(%6, %0) : (ptr, i64) -> i64
    call @use(%2, %5, %7) : (i64, i64, i64)
    return
}
"#,
        );
        assert!(!out.contains("call @strnlen("), "{out}");
        assert!(out.contains("icmp ult %0"), "the count against the length, {out}");
        assert!(out.contains("select"), "and the smaller of the two, {out}");
        assert!(out.contains("iconst.i64 3"), "a negative count is past the terminator, {out}");
        assert!(out.contains("iconst.i64 0"), "an empty string is nothing to count, {out}");
    }

    /// A count the source wrote as an `int` reaches the call widened, and the constant is under the
    /// widening rather than in the argument.
    #[test]
    fn a_count_that_was_widened_on_the_way_in_is_still_a_count() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @strnlen(ptr, i64) -> i64, linkage(external);

func @g() -> i64, linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = iconst.i32 4
    %2 = sext.i64 %1
    %3 = call @strnlen(%0, %2) : (ptr, i64) -> i64
    return %3
}
"#,
        );
        assert!(!out.contains("call @strnlen("), "{out}");
        assert!(out.contains("iconst.i64 4"), "{out}");
    }

    /// `memchr` reads a count rather than a string, so it finds a byte past the terminator, and it
    /// answers nowhere where the byte is outside the count.
    #[test]
    fn memchr_searches_the_object_rather_than_the_string_in_it() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @memchr(ptr, i32, i64) -> ptr, linkage(external);
func @use(ptr, ptr), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = iconst.i32 0
    %2 = iconst.i64 12
    %3 = call @memchr(%0, %1, %2) : (ptr, i32, i64) -> ptr
    %4 = iconst.i32 100
    %5 = iconst.i64 10
    %6 = call @memchr(%0, %4, %5) : (ptr, i32, i64) -> ptr
    call @use(%3, %6) : (ptr, ptr)
    return
}
"#,
        );
        assert!(!out.contains("call @memchr("), "{out}");
        assert!(out.contains("iconst.i64 11"), "the terminator is inside the count, {out}");
        assert!(out.contains("inttoptr.ptr "), "the d is one byte past the count, {out}");
    }

    /// A count the object does not have that many bytes for is a read the compiler cannot see the
    /// end of, so the call stays.
    #[test]
    fn a_memchr_that_runs_off_the_object_is_left_alone() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @memchr(ptr, i32, i64) -> ptr, linkage(external);

func @g() -> ptr, linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = iconst.i32 122
    %2 = iconst.i64 13
    %3 = call @memchr(%0, %1, %2) : (ptr, i32, i64) -> ptr
    return %3
}
"#,
        );
        assert!(out.contains("call @memchr("), "{out}");
    }

    /// `strchr` finds the first, `strrchr` the last, and a search for the terminator finds it at the
    /// end rather than not at all.
    #[test]
    fn the_two_character_searches_answer_from_either_end() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @strchr(ptr, i32) -> ptr, linkage(external);
func @strrchr(ptr, i32) -> ptr, linkage(external);
func @use(ptr, ptr, ptr, ptr), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = iconst.i32 111
    %2 = call @strchr(%0, %1) : (ptr, i32) -> ptr
    %3 = call @strrchr(%0, %1) : (ptr, i32) -> ptr
    %4 = iconst.i32 0
    %5 = call @strchr(%0, %4) : (ptr, i32) -> ptr
    %6 = iconst.i32 122
    %7 = call @strchr(%0, %6) : (ptr, i32) -> ptr
    call @use(%2, %3, %5, %7) : (ptr, ptr, ptr, ptr)
    return
}
"#,
        );
        assert!(!out.contains("call @strchr("), "{out}");
        assert!(!out.contains("call @strrchr("), "{out}");
        assert!(out.contains("iconst.i64 4"), "the first o, {out}");
        assert!(out.contains("iconst.i64 7"), "the last o, {out}");
        assert!(out.contains("iconst.i64 11"), "the terminator, {out}");
        assert!(out.contains("inttoptr.ptr "), "there is no z in it, {out}");
    }

    /// The two comparisons answer one of minus one, zero and one, which is the sign the standard
    /// promises and nothing more.
    #[test]
    fn the_two_comparisons_answer_a_sign() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 6 = { bytes "hello\00" }, align 1, linkage(internal), constant

func @strcmp(ptr, ptr) -> i32, linkage(external);
func @strncmp(ptr, ptr, i64) -> i32, linkage(external);
func @use(i32, i32, i32), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = global_addr @.Lstr.1
    %2 = call @strcmp(%0, %1) : (ptr, ptr) -> i32
    %3 = call @strcmp(%1, %0) : (ptr, ptr) -> i32
    %4 = iconst.i64 5
    %5 = call @strncmp(%0, %1, %4) : (ptr, ptr, i64) -> i32
    call @use(%2, %3, %5) : (i32, i32, i32)
    return
}
"#,
        );
        assert!(!out.contains("call @strcmp("), "{out}");
        assert!(!out.contains("call @strncmp("), "{out}");
        assert!(out.contains("iconst.i32 1"), "the longer one is the greater, {out}");
        assert!(out.contains("iconst.i32 -1"), "and the other way round, {out}");
        assert!(out.contains("iconst.i32 0"), "five bytes of each are the same, {out}");
    }

    /// A comparison against the empty string reads the first byte of the other one, whichever side
    /// the empty string was written on.
    #[test]
    fn a_comparison_against_the_empty_string_is_a_read_of_one_byte() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @strcmp(ptr, ptr) -> i32, linkage(external);
func @use(i32, i32), linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strcmp(%0, %1) : (ptr, ptr) -> i32
    %3 = call @strcmp(%1, %0) : (ptr, ptr) -> i32
    call @use(%2, %3) : (i32, i32)
    return
}
"#,
        );
        assert!(!out.contains("call @strcmp("), "{out}");
        assert_eq!(out.matches("load.i8 %0").count(), 2, "one read for each call, {out}");
        assert_eq!(out.matches("zext").count(), 2, "read as an unsigned char, {out}");
        assert_eq!(out.matches("sub").count(), 2, "and the difference each way round, {out}");
    }

    /// A comparison told to read no bytes reads neither string, and one told to read a single byte
    /// against a string this module holds is the difference between two bytes.
    #[test]
    fn a_short_count_settles_a_comparison_without_the_other_string() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 4 = { bytes "ozz\00" }, align 1, linkage(internal), constant

func @strncmp(ptr, ptr, i64) -> i32, linkage(external);
func @use(i32, i32), linkage(external);

func @g(ptr, ptr), linkage(external) {
block0(%0: ptr, %1: ptr):
    %2 = global_addr @.Lstr.0
    %3 = iconst.i32 0
    %4 = sext.i64 %3
    %5 = call @strncmp(%0, %1, %4) : (ptr, ptr, i64) -> i32
    %6 = iconst.i32 1
    %7 = sext.i64 %6
    %8 = call @strncmp(%2, %0, %7) : (ptr, ptr, i64) -> i32
    call @use(%5, %8) : (i32, i32)
    return
}
"#,
        );
        assert!(!out.contains("call @strncmp("), "{out}");
        assert!(out.contains("iconst.i32 0"), "no bytes to read is no difference, {out}");
        assert!(out.contains("load.i8 %0"), "one byte of the other string, {out}");
        assert!(out.contains("iconst.i32 111"), "against the first byte of this one, {out}");
    }

    /// An index and a count the source worked out from constants are still the arithmetic when this
    /// pass looks, because nothing has folded them yet, and they are read as the answer they have.
    #[test]
    fn an_index_and_a_count_worked_out_from_constants_are_constants() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @strncmp(ptr, ptr, i64) -> i32, linkage(external);
func @use(i32), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = iconst.i64 1
    %2 = ptr_add %0, %1
    %3 = iconst.i32 1
    %4 = iconst.i32 3
    %5 = and %3, %4
    %6 = sext.i64 %5
    %7 = ptr_add %0, %6
    %8 = iconst.i32 2
    %9 = add.nsw %8, %3
    %10 = sext.i64 %9
    %11 = call @strncmp(%2, %7, %10) : (ptr, ptr, i64) -> i32
    call @use(%11) : (i32)
    return
}
"#,
        );
        assert!(!out.contains("call @strncmp("), "{out}");
    }

    /// A comparison declared to answer something no wider than the byte it would read is left
    /// alone, because there is no room in it for the answer.
    #[test]
    fn a_comparison_with_no_room_for_a_byte_is_left_alone() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @strcmp(ptr, ptr) -> i8, linkage(external);
func @use(i8), linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strcmp(%0, %1) : (ptr, ptr) -> i8
    call @use(%2) : (i8)
    return
}
"#,
        );
        assert!(out.contains("call @strcmp("), "{out}");
    }

    /// The two spans walk the same string with the test turned round.
    #[test]
    fn the_two_spans_are_one_walk_each_way() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 4 = { bytes "hel\00" }, align 1, linkage(internal), constant
global @.Lstr.2 : bytes 3 = { bytes "wz\00" }, align 1, linkage(internal), constant

func @strspn(ptr, ptr) -> i64, linkage(external);
func @strcspn(ptr, ptr) -> i64, linkage(external);
func @use(i64, i64), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = global_addr @.Lstr.1
    %2 = call @strspn(%0, %1) : (ptr, ptr) -> i64
    %3 = global_addr @.Lstr.2
    %4 = call @strcspn(%0, %3) : (ptr, ptr) -> i64
    call @use(%2, %4) : (i64, i64)
    return
}
"#,
        );
        assert!(!out.contains("call @strspn("), "{out}");
        assert!(!out.contains("call @strcspn("), "{out}");
        assert!(out.contains("iconst.i64 4"), "hello stops at the o, {out}");
        assert!(out.contains("iconst.i64 6"), "the w is six bytes along, {out}");
    }

    /// Nothing is inside an empty set, so `strspn(s, "")` is zero and `strcspn(s, "")` is the length
    /// of `s`, whatever `s` is.
    #[test]
    fn an_empty_set_is_a_span_of_nothing_or_of_all_of_it() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @strspn(ptr, ptr) -> i64, linkage(external);
func @strcspn(ptr, ptr) -> i64, linkage(external);
func @strlen(ptr) -> i64, linkage(external);
func @use(i64, i64), linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strspn(%0, %1) : (ptr, ptr) -> i64
    %3 = call @strcspn(%0, %1) : (ptr, ptr) -> i64
    call @use(%2, %3) : (i64, i64)
    return
}
"#,
        );
        assert!(!out.contains("call @strspn("), "{out}");
        assert!(!out.contains("call @strcspn("), "{out}");
        assert!(out.contains("call @strlen(%0)"), "{out}");
        assert!(out.contains("iconst.i64 0"), "{out}");
    }

    /// A `strcspn` answering a width `strlen` does not answer is a fold that would leave a reader
    /// holding a number of the wrong size, so it does not happen.
    #[test]
    fn a_strcspn_of_another_width_than_strlen_is_left_alone() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @strcspn(ptr, ptr) -> i32, linkage(external);
func @strlen(ptr) -> i64, linkage(external);

func @g(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strcspn(%0, %1) : (ptr, ptr) -> i32
    return %2
}
"#,
        );
        assert!(out.contains("call @strcspn("), "{out}");
    }

    /// `strpbrk` of a set of one character is a search for that character, and of an empty set is
    /// nowhere at all.
    #[test]
    fn strpbrk_of_a_short_set_is_a_search_or_an_answer() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 2 = { bytes "w\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant

func @strpbrk(ptr, ptr) -> ptr, linkage(external);
func @strchr(ptr, i32) -> ptr, linkage(external);
func @use(ptr, ptr), linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = call @strpbrk(%0, %1) : (ptr, ptr) -> ptr
    %3 = global_addr @.Lstr.1
    %4 = call @strpbrk(%0, %3) : (ptr, ptr) -> ptr
    call @use(%2, %4) : (ptr, ptr)
    return
}
"#,
        );
        assert!(!out.contains("call @strpbrk("), "{out}");
        assert!(out.contains("call @strchr(%0, "), "{out}");
        assert!(out.contains("iconst.i32 119"), "{out}");
        assert!(out.contains("inttoptr.ptr "), "the empty set is nowhere, {out}");
    }

    /// Two strings this module holds answer `strpbrk` without either call.
    #[test]
    fn strpbrk_over_two_strings_this_module_holds_is_a_place() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 3 = { bytes "wz\00" }, align 1, linkage(internal), constant
global @.Lstr.2 : bytes 3 = { bytes "qz\00" }, align 1, linkage(internal), constant

func @strpbrk(ptr, ptr) -> ptr, linkage(external);
func @use(ptr, ptr), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = global_addr @.Lstr.1
    %2 = call @strpbrk(%0, %1) : (ptr, ptr) -> ptr
    %3 = global_addr @.Lstr.2
    %4 = call @strpbrk(%0, %3) : (ptr, ptr) -> ptr
    call @use(%2, %4) : (ptr, ptr)
    return
}
"#,
        );
        assert!(!out.contains("call @strpbrk("), "{out}");
        assert!(out.contains("iconst.i64 6"), "the w is six bytes along, {out}");
        assert!(out.contains("inttoptr.ptr "), "there is neither a q nor a z in it, {out}");
    }

    /// `index` and `rindex` are the same two searches under their older names.
    #[test]
    fn the_older_spellings_of_the_two_searches_are_folded_as_well() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @index(ptr, i32) -> ptr, linkage(external);
func @rindex(ptr, i32) -> ptr, linkage(external);
func @use(ptr, ptr), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    %1 = iconst.i32 111
    %2 = call @index(%0, %1) : (ptr, i32) -> ptr
    %3 = call @rindex(%0, %1) : (ptr, i32) -> ptr
    call @use(%2, %3) : (ptr, ptr)
    return
}
"#,
        );
        assert!(!out.contains("call @index("), "{out}");
        assert!(!out.contains("call @rindex("), "{out}");
        assert!(out.contains("iconst.i64 4"), "the first o, {out}");
        assert!(out.contains("iconst.i64 7"), "the last o, {out}");
    }

    /// A search from the right for the terminator is a search from the left for it, because a
    /// string has one terminator and both walks find that one.
    #[test]
    fn a_strrchr_of_the_terminator_is_a_strchr_of_it() {
        let out = folded(
            r#"
func @strrchr(ptr, i32) -> ptr, linkage(external);

func @g(ptr) -> ptr, linkage(external) {
block0(%0: ptr):
    %1 = iconst.i32 0
    %2 = call @strrchr(%0, %1) : (ptr, i32) -> ptr
    return %2
}
"#,
        );
        assert!(!out.contains("call @strrchr("), "{out}");
        assert!(out.contains("call @strchr(%0, "), "{out}");
    }

    /// A search from the right for anything else needs the string, since where the last one is
    /// depends on what is in it.
    #[test]
    fn a_strrchr_of_another_character_needs_the_string() {
        let out = folded(
            r#"
func @strrchr(ptr, i32) -> ptr, linkage(external);

func @g(ptr) -> ptr, linkage(external) {
block0(%0: ptr):
    %1 = iconst.i32 111
    %2 = call @strrchr(%0, %1) : (ptr, i32) -> ptr
    return %2
}
"#,
        );
        assert!(out.contains("call @strrchr("), "{out}");
    }

    /// A declaration of the wrong shape is a function of the program's own, whatever it is called.
    #[test]
    fn a_strlen_that_answers_nothing_is_not_the_one_the_library_has() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 12 = { bytes "hello world\00" }, align 1, linkage(internal), constant

func @strlen(ptr), linkage(external);

func @g(), linkage(external) {
block0:
    %0 = global_addr @.Lstr.0
    call @strlen(%0) : (ptr)
    return
}
"#,
        );
        assert!(out.contains("call @strlen("), "{out}");
    }

    /// A checking copy whose count is known to fit is the plain copy, one that is not known to
    /// fit keeps its check, and `__mempcpy_chk` whose answer nothing reads keeps its check on the
    /// copy that has no answer to work out.
    #[test]
    fn a_checking_copy_that_fits_is_the_plain_copy() {
        let out = folded(
            r#"
func @__memcpy_chk(ptr, ptr, i64, i64) -> ptr, linkage(external);
func @__mempcpy_chk(ptr, ptr, i64, i64) -> ptr, linkage(external);
func @use(ptr, ptr), linkage(external);

func @g(ptr, ptr, i64), linkage(external) {
block0(%0: ptr, %1: ptr, %2: i64):
    %3 = iconst.i64 4
    %4 = iconst.i64 32
    %5 = call @__memcpy_chk(%0, %1, %3, %4) : (ptr, ptr, i64, i64) -> ptr
    %6 = iconst.i64 40
    %7 = call @__memcpy_chk(%0, %1, %6, %4) : (ptr, ptr, i64, i64) -> ptr
    %8 = call @__mempcpy_chk(%0, %1, %2, %4) : (ptr, ptr, i64, i64) -> ptr
    call @use(%5, %7) : (ptr, ptr)
    return
}
"#,
        );
        assert!(out.contains("call @memcpy(%0, %1, %3)"), "four bytes fit in thirty two, {out}");
        assert!(out.contains("call @__memcpy_chk(%0, %1, %6, %4)"), "forty do not, {out}");
        assert!(out.contains("call @__memcpy_chk(%0, %1, %2, %4)"), "nothing read the end, {out}");
        assert!(!out.contains("call @__mempcpy_chk("), "{out}");
    }

    /// `__stpcpy_chk` of a string that fits is `stpcpy`, which is a `memcpy` of the string and its
    /// terminator answering the end of the copy, and of a string nothing is known about it stays,
    /// or becomes `__strcpy_chk` where its answer is not read.
    #[test]
    fn a_checking_string_copy_goes_as_far_as_the_string_is_known() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 6 = { bytes "abcde\00" }, align 1, linkage(internal), constant

func @__stpcpy_chk(ptr, ptr, i64) -> ptr, linkage(external);
func @use(ptr, ptr), linkage(external);

func @g(ptr, ptr), linkage(external) {
block0(%0: ptr, %1: ptr):
    %2 = global_addr @.Lstr.0
    %3 = iconst.i64 32
    %4 = call @__stpcpy_chk(%0, %2, %3) : (ptr, ptr, i64) -> ptr
    %5 = call @__stpcpy_chk(%0, %1, %3) : (ptr, ptr, i64) -> ptr
    %6 = call @__stpcpy_chk(%0, %1, %3) : (ptr, ptr, i64) -> ptr
    call @use(%4, %5) : (ptr, ptr)
    return
}
"#,
        );
        assert!(out.contains("call @memcpy(%0, %2, "), "{out}");
        assert!(out.contains("iconst.i64 6"), "five bytes and a terminator, {out}");
        assert!(out.contains("ptr_add %0"), "the answer is the end of the copy, {out}");
        assert!(out.contains("call @__stpcpy_chk(%0, %1, %3)"), "{out}");
        assert!(out.contains("call @__strcpy_chk(%0, %1, %3)"), "{out}");
    }

    /// A string known not to fit is still a count, so `__strcpy_chk` of it is `__memcpy_chk` of
    /// the string and its terminator, and the library checks a number.
    #[test]
    fn a_checking_string_copy_that_does_not_fit_is_a_checking_copy_of_a_count() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 6 = { bytes "abcde\00" }, align 1, linkage(internal), constant

func @__strcpy_chk(ptr, ptr, i64) -> ptr, linkage(external);
func @use(ptr), linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = iconst.i64 4
    %3 = call @__strcpy_chk(%0, %1, %2) : (ptr, ptr, i64) -> ptr
    call @use(%3) : (ptr)
    return
}
"#,
        );
        assert!(out.contains("call @__memcpy_chk(%0, %1, "), "{out}");
        assert!(out.contains("iconst.i64 6"), "{out}");
    }

    /// Appending the empty string, or no bytes of any string, answers the destination, and
    /// `__strncat_chk` whose count is no limit on the string is `__strcat_chk`.
    #[test]
    fn a_checking_append_of_nothing_is_the_destination() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 1 = { bytes "\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 4 = { bytes "abc\00" }, align 1, linkage(internal), constant

func @__strcat_chk(ptr, ptr, i64) -> ptr, linkage(external);
func @__strncat_chk(ptr, ptr, i64, i64) -> ptr, linkage(external);
func @use(ptr, ptr, ptr, ptr), linkage(external);

func @g(ptr, ptr), linkage(external) {
block0(%0: ptr, %1: ptr):
    %2 = global_addr @.Lstr.0
    %3 = global_addr @.Lstr.1
    %4 = iconst.i64 32
    %5 = call @__strcat_chk(%0, %2, %4) : (ptr, ptr, i64) -> ptr
    %6 = iconst.i64 0
    %7 = call @__strncat_chk(%0, %1, %6, %4) : (ptr, ptr, i64, i64) -> ptr
    %8 = iconst.i64 5
    %9 = call @__strncat_chk(%0, %3, %8, %4) : (ptr, ptr, i64, i64) -> ptr
    %10 = iconst.i64 2
    %11 = call @__strncat_chk(%0, %3, %10, %4) : (ptr, ptr, i64, i64) -> ptr
    call @use(%5, %7, %9, %11) : (ptr, ptr, ptr, ptr)
    return
}
"#,
        );
        assert!(out.contains("call @use(%0, %0, "), "{out}");
        assert_eq!(
            out.matches("call @__strcat_chk(%0, ").count(),
            1,
            "five is no limit on three, {out}"
        );
        assert_eq!(out.matches("call @__strncat_chk(%0, ").count(), 1, "two is, {out}");
    }

    /// `__sprintf_chk` of a format with nothing to convert that fits is `sprintf`, which is
    /// `strcpy` answering the length, which is `memcpy`. A format with a conversion in it writes a
    /// length nothing knows and keeps its check.
    #[test]
    fn a_checking_sprintf_of_a_known_string_is_a_copy() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 6 = { bytes "hello\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 3 = { bytes "%d\00" }, align 1, linkage(internal), constant

func @__sprintf_chk(ptr, i32, i64, ptr, ...) -> i32, linkage(external);
func @use(i32, i32), linkage(external);

func @g(ptr, i32), linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = global_addr @.Lstr.0
    %3 = global_addr @.Lstr.1
    %4 = iconst.i32 0
    %5 = iconst.i64 32
    %6 = call @__sprintf_chk(%0, %4, %5, %2) : (ptr, i32, i64, ptr, ...) -> i32
    %7 = call @__sprintf_chk(%0, %4, %5, %3, %1) : (ptr, i32, i64, ptr, ...) -> i32
    call @use(%6, %7) : (i32, i32)
    return
}
"#,
        );
        assert!(out.contains("call @memcpy(%0, %2, "), "{out}");
        assert!(out.contains("iconst.i32 5"), "the length is the answer, {out}");
        assert!(out.contains("call @__sprintf_chk(%0, %4, %5, %3, %1)"), "{out}");
    }

    /// `__snprintf_chk` whose bound fits is `snprintf` with what it was passed beyond its format
    /// still passed, and a flag asking for more checking keeps the check on a format it would read.
    #[test]
    fn a_checking_snprintf_keeps_its_arguments_and_loses_its_check() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 3 = { bytes "%d\00" }, align 1, linkage(internal), constant

func @__snprintf_chk(ptr, i64, i32, i64, ptr, ...) -> i32, linkage(external);
func @use(i32, i32), linkage(external);

func @g(ptr, i32), linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = global_addr @.Lstr.0
    %3 = iconst.i64 8
    %4 = iconst.i32 0
    %5 = iconst.i64 32
    %6 = call @__snprintf_chk(%0, %3, %4, %5, %2, %1) : (ptr, i64, i32, i64, ptr, ...) -> i32
    %7 = iconst.i32 1
    %8 = call @__snprintf_chk(%0, %3, %7, %5, %2, %1) : (ptr, i64, i32, i64, ptr, ...) -> i32
    call @use(%6, %8) : (i32, i32)
    return
}
"#,
        );
        assert!(
            out.contains("call @snprintf(%0, %3, %2, %1) : (ptr, i64, ptr, ...) -> i32"),
            "{out}"
        );
        assert!(out.contains("call @__snprintf_chk(%0, %3, %7, %5, %2, %1)"), "{out}");
    }

    /// A program that declared the plain function as something else keeps its checking call.
    #[test]
    fn a_plain_function_of_another_shape_keeps_the_check() {
        let out = folded(
            r#"
func @__memcpy_chk(ptr, ptr, i64, i64) -> ptr, linkage(external);
func @memcpy(ptr, ptr, i32) -> ptr, linkage(external);
func @use(ptr), linkage(external);

func @g(ptr, ptr), linkage(external) {
block0(%0: ptr, %1: ptr):
    %2 = iconst.i64 4
    %3 = iconst.i64 32
    %4 = call @__memcpy_chk(%0, %1, %2, %3) : (ptr, ptr, i64, i64) -> ptr
    call @use(%4) : (ptr)
    return
}
"#,
        );
        assert!(out.contains("call @__memcpy_chk("), "{out}");
    }

    /// `mempcpy` is `memcpy` answering the end of the copy, and a `mempcpy` into the end of another
    /// one is the second of two copies whose destination is the first one's answer, which is a
    /// value the first fold took away and the second has to be told about.
    #[test]
    fn a_copy_into_the_end_of_a_copy_names_the_end_the_first_fold_wrote() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 8 = { bytes "abcdEFG\00" }, align 1, linkage(internal), constant
global @.Lstr.1 : bytes 4 = { bytes "efg\00" }, align 1, linkage(internal), constant

func @mempcpy(ptr, ptr, i64) -> ptr, linkage(external);
func @use(ptr), linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = global_addr @.Lstr.1
    %3 = iconst.i64 4
    %4 = call @mempcpy(%0, %1, %3) : (ptr, ptr, i64) -> ptr
    %5 = call @mempcpy(%4, %2, %3) : (ptr, ptr, i64) -> ptr
    call @use(%5) : (ptr)
    return
}
"#,
        );
        assert!(!out.contains("call @mempcpy("), "{out}");
        assert_eq!(out.matches("call @memcpy(").count(), 2, "{out}");
        assert_eq!(out.matches("ptr_add").count(), 2, "{out}");
    }

    /// `strncat` whose count is no limit on a string whose length is known is `strcat`, and one
    /// whose count is short of it keeps the count.
    #[test]
    fn a_counted_append_of_all_of_a_known_string_is_the_uncounted_one() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 4 = { bytes "foo\00" }, align 1, linkage(internal), constant

func @strncat(ptr, ptr, i64) -> ptr, linkage(external);
func @use(ptr, ptr), linkage(external);

func @g(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @.Lstr.0
    %2 = iconst.i64 3
    %3 = call @strncat(%0, %1, %2) : (ptr, ptr, i64) -> ptr
    %4 = iconst.i64 2
    %5 = call @strncat(%0, %1, %4) : (ptr, ptr, i64) -> ptr
    call @use(%3, %5) : (ptr, ptr)
    return
}
"#,
        );
        assert_eq!(out.matches("call @strcat(%0, %1)").count(), 1, "{out}");
        assert_eq!(out.matches("call @strncat(").count(), 1, "{out}");
    }

    /// A count that is one of two numbers fits where the larger of the two does, which is
    /// `l1 ? sizeof (buf) : 4` in `builtins/pr23484-chk.c`.
    #[test]
    fn a_count_fits_where_the_largest_it_may_be_fits() {
        let text = r#"
func @__memcpy_chk(ptr, ptr, i64, i64) -> ptr, linkage(external);
func @use(ptr), linkage(external);

func @g(ptr, ptr, i1), linkage(external) {
block0(%0: ptr, %1: ptr, %2: i1):
    %3 = iconst.i64 8
    %4 = iconst.i64 4
    br_if %2, block1(%3), block1(%4)
block1(%5: i64):
    %6 = iconst.i64 SIZE
    %7 = call @__memcpy_chk(%0, %1, %5, %6) : (ptr, ptr, i64, i64) -> ptr
    call @use(%7) : (ptr)
    return
}
"#;
        let fits = folded(&text.replace("SIZE", "8"));
        assert!(fits.contains("call @memcpy("), "{fits}");
        let short = folded(&text.replace("SIZE", "7"));
        assert!(short.contains("call @__memcpy_chk("), "{short}");
    }
}
