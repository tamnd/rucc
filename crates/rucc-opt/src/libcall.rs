//! The printf family, folded into the call its output really is.
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
//! # What a call has to be
//!
//! Its result has to be read by nothing. `printf` answers the number of characters written and
//! `puts` answers a non-negative number that is not that count, so a program looking at the answer
//! is a program this may not touch.
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
    CallInfo, Datum, Def, Extra, Func, FuncId, Global, Imm, Inst, InstData, Linkage, Module,
    Opcode, Pic, Signature, SymbolRef, Type, Value,
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

/// The names a fold may leave behind, sorted.
const REPLACEMENTS: [&str; 5] = ["fputc", "fputs", "fwrite", "putchar", "puts"];

/// The names a fold reads, sorted.
const SOURCES: [&str; 6] =
    ["fprintf", "fprintf_unlocked", "fputs", "fputs_unlocked", "printf", "printf_unlocked"];

/// What the compiler worked out a call writes, which is what it is replaced by.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// It writes nothing, so it goes and nothing takes its place.
    Drop,
    /// This call takes its place.
    Swap {
        /// The function the replacement names.
        callee: &'static str,
        /// What that function takes and returns.
        signature: Signature,
        /// What to pass it.
        args: Vec<Argument>,
    },
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
    /// The signature a call to that name has to carry, and `None` where no call may name it.
    held: HashMap<&'static str, Option<Signature>>,
}

impl Shapes {
    /// Reads the module's answer for each of the five names.
    fn of(module: &Module, names: &Interner) -> Self {
        let mut held: HashMap<&'static str, Option<Signature>> =
            REPLACEMENTS.iter().map(|&name| (name, Some(canonical(module, name)))).collect();
        for id in module.funcs() {
            let Some(slot) = held.get_mut(names.resolve(module[id].name)) else { continue };
            let declared = module[id].signature();
            let agrees = slot.as_ref().is_some_and(|want| {
                !declared.variadic
                    && declared.param_types().eq(want.param_types())
                    && declared.return_types().eq(want.return_types())
            });
            *slot = agrees.then(|| declared.clone());
        }
        // A variable or a second name for something else is not a function to call, whatever it is
        // spelled.
        for id in module.globals() {
            if let Some(slot) = held.get_mut(names.resolve(module[id].name)) {
                *slot = None;
            }
        }
        for id in module.aliases() {
            if let Some(slot) = held.get_mut(names.resolve(module[id].name)) {
                *slot = None;
            }
        }
        Self { held }
    }

    /// What a call to that name carries, or `None` where this module does not allow one.
    fn get(&self, name: &'static str) -> Option<Signature> {
        self.held.get(name)?.clone()
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
    // A module that defines one of these names itself is where that function comes from, and what a
    // function called `fputs` does in there is whatever it was written to do.
    let defined: HashSet<Symbol> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| module[id].name)
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
        if module[id].is_declaration() || !mentions(&module[id], names) {
            continue;
        }
        let mut stats = Stats::new();
        // The whole body is read before any of it changes. A plan names values the body holds, and
        // working the next one out from a body half rewritten is how a pass comes to read a value
        // whose definition it has just taken away.
        let plans = {
            let func = &module[id];
            let site = Site {
                module,
                func,
                cfg: &Cfg::new(func),
                shapes: &shapes,
                counts: &uses::count(func),
                defined: &defined,
                names,
                no_builtin,
                pic,
            };
            site.survey(fuel, &mut stats)
        };
        for (inst, plan) in plans {
            apply(module, id, names, &mut texts, inst, plan);
        }
        if stats.changed() {
            done.push((id, stats));
        }
    }
    done
}

/// Whether this function calls any of the names a fold reads.
fn mentions(func: &Func, names: &Interner) -> bool {
    func.blocks().flat_map(|block| func.insts(block)).any(|inst| {
        let data = &func[inst];
        let Extra::Call(at) = data.extra else { return false };
        data.opcode == Opcode::Call
            && func[at].callee.is_some_and(|callee| SOURCES.contains(&names.resolve(callee)))
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
    /// The names this module defines bodies for.
    defined: &'a HashSet<Symbol>,
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
                    Plan::Swap { .. } => "call to the library folded",
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
        // no two of these functions answer the same number.
        if data.results().any(|result| self.counts[result.index()] != 0) {
            return None;
        }
        let Extra::Call(at) = data.extra else { return None };
        let callee = self.func[at].callee?;
        if self.defined.contains(&callee) {
            return None;
        }
        let name = self.names.resolve(callee);
        if self.no_builtin.iter().any(|it| it == name) {
            return None;
        }
        let args: Vec<Value> = self.func[data.args].to_vec();
        // The locked and the unlocked spellings take the same arguments and differ only in how far
        // the fold may go, so they are an arm each with a flag rather than two bodies.
        match name {
            "printf" => self.printf(&args, false),
            "printf_unlocked" => self.printf(&args, true),
            "fprintf" => self.fprintf(&args, false),
            "fprintf_unlocked" => self.fprintf(&args, true),
            "fputs" => self.fputs(&args, false),
            "fputs_unlocked" => self.fputs(&args, true),
            _ => None,
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
                match self.strings(args[2], DEPTH) {
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
        self.string(&self.strings(text, DEPTH)?, text, stream, quiet)
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

    /// A call to that name, or nothing where this module does not allow one.
    fn call(&self, callee: &'static str, args: Vec<Argument>) -> Option<Plan> {
        Some(Plan::Swap { callee, signature: self.shapes.get(callee)?, args })
    }

    /// The one string this value points at, or `None` where there is more than one of them.
    fn one(&self, value: Value) -> Option<Vec<u8>> {
        let mut candidates = self.strings(value, DEPTH)?;
        (candidates.len() == 1).then(|| candidates.pop()).flatten()
    }

    /// Every string this value may point at, or `None` where any of them is not one this module
    /// holds.
    ///
    /// A block parameter is every argument every branch to that block passes, which is how the
    /// conditional expression in `builtins/fputs.c` gets a length without anything having turned it
    /// into a `select` first. `depth` is what stops the walk on a loop, where a parameter's
    /// argument is the parameter.
    fn strings(&self, value: Value, depth: u32) -> Option<Vec<Vec<u8>>> {
        if depth == 0 {
            return None;
        }
        match self.func[value].def {
            Def::Param { block, index } => {
                let preds = self.cfg.predecessors(block);
                if preds.is_empty() {
                    return None;
                }
                let mut all = Vec::new();
                for &pred in preds {
                    let term = self.func.terminator(pred)?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize)?;
                        all.extend(self.strings(arg, depth - 1)?);
                    }
                }
                (!all.is_empty()).then_some(all)
            }
            Def::Result { inst, .. } if self.func[inst].opcode == Opcode::Select => {
                let args = &self.func[self.func[inst].args];
                let (then, other) = (*args.get(1)?, *args.get(2)?);
                let mut all = self.strings(then, depth - 1)?;
                all.extend(self.strings(other, depth - 1)?);
                Some(all)
            }
            _ => Some(vec![self.literal(value)?]),
        }
    }

    /// The bytes up to the first terminator at the address this value is, where that address is
    /// inside a read only object this module vouches for.
    fn literal(&self, value: Value) -> Option<Vec<u8>> {
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
        let rest = bytes.get(usize::try_from(offset).ok()?..)?;
        let end = rest.iter().position(|&byte| byte == 0)?;
        Some(rest[..end].to_vec())
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

    /// The constant this value is, looking through a widening of one.
    ///
    /// An index into an array is an `int` where the source wrote one, and a pointer is sixty four
    /// bits, so what the frontend leaves in front of a `ptr_add` is a `sext` of a constant rather
    /// than a constant. This runs before anything has folded that, since everything that would is
    /// one function at a time and the function pipeline has not started, so the walk above would
    /// stop at the first index written in the source without this.
    fn step(&self, mut value: Value) -> Option<i128> {
        for _ in 0..DEPTH {
            if let Some((imm, ty)) = crate::fold::constant(self.func, value) {
                return Some(imm.signed(ty));
            }
            let Def::Result { inst, .. } = self.func[value].def else { return None };
            match self.func[inst].opcode {
                // The narrow value read the way the widening reads it, which for the signed one is
                // the same number and for the unsigned one is the same number only where it was
                // not negative.
                Opcode::SExt => value = *self.func[self.func[inst].args].first()?,
                Opcode::ZExt => {
                    let arg = *self.func[self.func[inst].args].first()?;
                    let (imm, _) = crate::fold::constant(self.func, arg)?;
                    return i128::try_from(imm.unsigned()).ok();
                }
                _ => return None,
            }
        }
        None
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
) {
    let Plan::Swap { callee, signature, args } = plan else {
        module[id].remove_inst(inst);
        return;
    };
    let callee = names.intern(callee);
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
    let results: Vec<Type> = signature.return_types().collect();
    let sig = func.add_signature(signature);
    let varargs = func.push_abis(&[]);
    let info = func.add_call(CallInfo { callee: Some(callee), signature: sig, varargs });
    let args = func.push_values(&values);
    let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
    let made = func.create_inst(data, &results, span);
    func.insert_before(made, inst);
    func.remove_inst(inst);
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

    /// A module holding the body of its own `printf` means that body.
    #[test]
    fn a_name_this_module_defines_is_that_definition() {
        let out = folded(
            r#"
global @.Lstr.0 : bytes 3 = { bytes "a\0a\00" }, align 1, linkage(internal), constant

func @printf(ptr, ...) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = iconst.i32 0
    return %1
}

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
}
