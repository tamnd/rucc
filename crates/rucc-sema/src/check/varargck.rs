//! kencc's format checking, which is `-F` and `#pragma varargck`, from `cc/dpchk.c`.
//!
//! A header names the functions that take a format with `argpos`, and the verbs with `type`
//! and `flag`. 9front's `libc.h` has `#pragma varargck argpos print 1` and
//! `#pragma varargck type "lld" vlong`, for example. At each call of a named function whose
//! format is a string literal, kencc reads the format as `checkargs` does, and gives a warning
//! where an argument has no `type` line for its verb.
//!
//! Every rule here is the rule of kencc, quirks included, because `r9 warn-diff` compares the
//! warnings of rucc with those of 6c:
//! - A verb is a rune that is not a flag. Each new verb gets the next number, and the flags
//!   before it are a set of bits, so `%lld` and `%llx` are different verbs and `%5d` and `%d`
//!   are the same.
//! - A type is the same as another as `sametype` says, so a `void *` argument matches the verb
//!   of any pointer.
//! - The type of an argument is the type that `tcoma` gives it: a prototyped argument has the
//!   type of the parameter, an argument after `...` has the default promotions, and `float`
//!   becomes `double`.
//! - A line is read where it is in the file. A call in a function sees the lines before the end
//!   of the function, because kencc types the body of a function only at its end.
//!
//! The warnings have the words of kencc, so that the two lists can be compared.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{FloatKind, IntKind, TypeId, TypeKind, kencc_same_type, kencc_type_text};

use super::Checker;
use crate::expr::{Conversion, ExprId, ExprKind};
use crate::scope::Binding;

/// The code of every warning of the format check. It is not the code of the gcc format check,
/// which is quiet until `-Wformat` asks for it, because kencc gives these under `-F` alone.
const FORMAT: &str = "E0884";
/// The code of a `#pragma varargck` line that kencc cannot read.
const PRAGMA: &str = "E0798";

/// One `#pragma varargck` line, as the parser read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarargckLine {
    /// What the line says.
    pub pragma: VarargckPragma,
    /// Where the line is.
    pub span: Span,
    /// How many external declarations end before the line.
    pub decls: u32,
}

/// What a `#pragma varargck` line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarargckPragma {
    /// `argpos NAME N`: the argument `N` of a call of `NAME` is the format, counted from one.
    Argpos {
        /// The function.
        name: Symbol,
        /// The place of the format.
        param: u32,
    },
    /// `type "FMT" TYPE`, with `stars` levels of pointer after the type.
    Type {
        /// The verb with its flags, such as `lld`.
        format: Vec<u8>,
        /// The name of the type.
        ty: VarargckType,
        /// The number of `*`.
        stars: u32,
    },
    /// `flag 'c'`: the rune is a flag that the verbs ignore.
    Flag(u32),
    /// A line that kencc cannot read.
    Bad,
}

/// The name of the type of a `type` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarargckType {
    /// An ordinary name, whose type is the type of what it names, as `s->type` is in kencc.
    Name(Symbol),
    /// A keyword, with its spelling. Only `char`, `double`, `float`, `int`, `long`, `short` and
    /// `void` have a type in kencc.
    Keyword(&'static str),
}

/// What a rune is in `flagbits`.
const FNONE: u32 = 0;
const FL: u32 = 1;
const FVL: u32 = 2;
const FIGNOR: u32 = 3;
const FSTAR: u32 = 4;
/// The number of the first verb.
const FVERB: u32 = 10;
/// The length of `flagbits`. A rune at or above it ends a verb.
const NFLAGBITS: u32 = 512;
/// The length of `fmtbuf`, which holds one byte less.
const FMTBUF: usize = 100;

/// A set of flag and verb numbers, kencc's `Bits`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Flags([u64; 9]);

impl Flags {
    fn set(&mut self, bit: u32) {
        if let Some(word) = self.0.get_mut(bit as usize / 64) {
            *word |= 1 << (bit % 64);
        }
    }

    fn has(&self, bit: u32) -> bool {
        self.0.get(bit as usize / 64).is_some_and(|word| word & (1 << (bit % 64)) != 0)
    }
}

/// The state of `dpchk.c` for one unit.
#[derive(Debug)]
pub(in crate::check) struct Varargck {
    /// `flagbits`.
    flagbits: Vec<u32>,
    /// `lastverb`.
    lastverb: u32,
    /// `tprot`, with the newest line last.
    tprot: Vec<(TypeId, Flags)>,
    /// `tname`, with the newest line last.
    tname: Vec<(Symbol, u32)>,
    /// The lines not read yet, with the first line last.
    lines: Vec<VarargckLine>,
}

impl Varargck {
    /// `arginit`.
    pub(in crate::check) fn new(target_vlong_is_pointer: bool) -> Varargck {
        let mut state = Varargck {
            flagbits: vec![FNONE; NFLAGBITS as usize],
            lastverb: FVERB,
            tprot: Vec::new(),
            tname: Vec::new(),
            lines: Vec::new(),
        };
        for flag in b"0123456789.#uh+-" {
            state.flagbits[usize::from(*flag)] = FIGNOR;
        }
        state.flagbits[usize::from(b'*')] = FSTAR;
        state.flagbits[usize::from(b'l')] = FL;
        state.flagbits[usize::from(b'z')] = if target_vlong_is_pointer { FVL } else { FL };
        state.new_verb(u32::from(b'o'));
        state.flagbits[usize::from(b'x')] = state.flagbits[usize::from(b'o')];
        state.flagbits[usize::from(b'X')] = state.flagbits[usize::from(b'o')];
        state
    }

    /// `argflag(c, Fverb)`.
    fn new_verb(&mut self, rune: u32) {
        self.flagbits[rune as usize] = self.lastverb;
        self.lastverb += 1;
    }

    /// `getflag`: the flags and the verb at the start of `s`, the number of `*`, and the text
    /// that was read, which is `fmtbuf`.
    fn getflag(&mut self, mut s: &[u8]) -> (Flags, u32, String) {
        let mut flags = Flags::default();
        let mut stars = 0;
        let mut text = String::new();
        while text.len() < FMTBUF - 1 {
            let (rune, len) = chartorune(s);
            s = &s[len.min(s.len())..];
            if rune == 0 || rune >= NFLAGBITS {
                break;
            }
            text.push(char::from_u32(rune).unwrap_or(char::REPLACEMENT_CHARACTER));
            let mut f = self.flagbits[rune as usize];
            match f {
                FNONE => {
                    self.new_verb(rune);
                    f = self.flagbits[rune as usize];
                }
                FSTAR => {
                    stars += 1;
                    continue;
                }
                FIGNOR => continue,
                FL if flags.has(FL) => flags.set(FVL),
                _ => {}
            }
            flags.set(f);
            if f >= FVERB {
                break;
            }
        }
        (flags, stars, text)
    }
}

/// `chartorune` of Plan 9: the rune at the start of `s` and its length. A sequence that is not
/// UTF-8 is `Runeerror` and one byte long.
fn chartorune(s: &[u8]) -> (u32, usize) {
    let Some(&first) = s.first() else { return (0, 0) };
    let len = match first {
        0x00..=0x7f => return (u32::from(first), 1),
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return (0xfffd, 1),
    };
    match s.get(..len).and_then(|bytes| std::str::from_utf8(bytes).ok()) {
        Some(text) => (text.chars().next().map_or(0xfffd, u32::from), len),
        None => (0xfffd, 1),
    }
}

impl Checker<'_> {
    /// Gives the checker the `#pragma varargck` lines of the unit, in the order they were
    /// written. Call this before [`Checker::check_unit`]. The lines are read only under `-F`.
    pub fn set_varargck_lines(&mut self, lines: &[VarargckLine]) {
        if !self.cx.kencc_format {
            return;
        }
        let vlong_is_pointer = self.cx.target.pointer_width == 64;
        let mut state = Varargck::new(vlong_is_pointer);
        state.lines = lines.iter().rev().cloned().collect();
        self.varargck = Some(state);
    }

    /// Reads each line that comes before `place`, as `pragvararg` does.
    pub(in crate::check) fn read_varargck_before(&mut self, decl: u32) {
        loop {
            let Some(state) = self.varargck.as_mut() else { return };
            if !state.lines.last().is_some_and(|line| line.decls <= decl) {
                return;
            }
            let Some(line) = state.lines.pop() else { return };
            self.read_varargck(line);
        }
    }

    /// One line, with the messages of `pragvararg`, `newname` and `newprot`.
    fn read_varargck(&mut self, line: VarargckLine) {
        match line.pragma {
            VarargckPragma::Bad => {
                let what = "syntax in #pragma varargck";
                self.report(Diagnostic::error(what, line.span).with_code(PRAGMA));
            }
            VarargckPragma::Argpos { name, param } => {
                let Some(state) = self.varargck.as_mut() else { return };
                match state.tname.iter().find(|&&(known, _)| known == name) {
                    Some(&(_, known)) if known != param => {
                        let what = format!("vargck {} already defined", self.text(name));
                        self.report(Diagnostic::error(what, line.span).with_code(PRAGMA));
                    }
                    Some(_) => {}
                    None => state.tname.push((name, param)),
                }
            }
            VarargckPragma::Flag(rune) => {
                if let Some(state) = self.varargck.as_mut()
                    && let Some(bits) = state.flagbits.get_mut(rune as usize)
                {
                    *bits = FIGNOR;
                }
            }
            VarargckPragma::Type { format, ty, stars } => {
                let (spelled, found) = match ty {
                    VarargckType::Name(name) => {
                        let found = match self.scopes.lookup(name) {
                            Some(Binding::Typedef(ty)) => Some(ty),
                            Some(Binding::Decl(decl)) => Some(self.tast[decl].ty),
                            Some(Binding::Enumerator { ty, .. }) => Some(ty),
                            None => None,
                        };
                        (self.text(name).to_owned(), found)
                    }
                    VarargckType::Keyword(word) => (word.to_owned(), self.keyword_type(word)),
                };
                let Some(mut found) = found else {
                    // kencc's `warn`, which has no node and so the line of the pragma.
                    let what = format!("{spelled}: newprot: type not defined");
                    self.report(Diagnostic::warning(what, line.span).with_code(FORMAT));
                    return;
                };
                for _ in 0..stars {
                    found = self.types.pointer(found);
                }
                let Some(state) = self.varargck.as_mut() else { return };
                let (flags, _, _) = state.getflag(&format);
                let tprot = std::mem::take(&mut state.tprot);
                let known = tprot
                    .iter()
                    .rev()
                    .any(|&(ty, bits)| bits == flags && self.kencc_same(found, ty));
                let Some(state) = self.varargck.as_mut() else { return };
                state.tprot = tprot;
                if !known {
                    state.tprot.push((found, flags));
                }
            }
        }
    }

    /// The type of a keyword, as `cinit` gives `s->type` for the keywords in `itab`.
    fn keyword_type(&self, word: &str) -> Option<TypeId> {
        Some(match word {
            "char" => self.types.int(IntKind::Char),
            "short" => self.types.int(IntKind::Short),
            "int" => self.types.int(IntKind::Int),
            "long" => self.types.int(IntKind::Long),
            "float" => self.types.float(FloatKind::Float),
            "double" => self.types.float(FloatKind::Double),
            "void" => self.types.void(),
            _ => return None,
        })
    }

    /// kencc's `sametype` under the `-V` of the unit.
    fn kencc_same(&self, t1: TypeId, t2: TypeId) -> bool {
        kencc_same_type(&self.types, self.cx.target, self.cx.kencc_strict_void, t1, t2)
    }

    /// A type as kencc's `%T` prints it.
    fn kencc_text(&self, ty: TypeId) -> String {
        kencc_type_text(&self.types, self.cx.target, ty, &|tag| self.text(tag).to_owned())
    }

    /// `dpcheck`, at a call whose callee and arguments are checked. `params` and `prototyped`
    /// are of the prototype of the callee.
    pub(in crate::check) fn heed_varargck(
        &mut self,
        callee: ExprId,
        params: &[TypeId],
        prototyped: bool,
        args: &[ExprId],
        call: Span,
    ) {
        let Some(state) = self.varargck.as_ref() else { return };
        if state.tname.is_empty() {
            return;
        }
        // kencc looks only at a call of a name, and not at a call through a pointer.
        let Some(decl) = self.called_decl(callee) else { return };
        let ty = self.types.canonical(self.tast[decl].ty);
        if !matches!(self.types.kind(ty), TypeKind::Function(_)) {
            return;
        }
        let Some(name) = self.tast[decl].name else { return };
        let Some(&(_, param)) = state.tname.iter().rev().find(|&&(known, _)| known == name) else {
            return;
        };
        let Some(index) = (param as usize).checked_sub(1) else { return };
        // kencc gives an error for a call with too few arguments, and does not check the format.
        if prototyped && args.len() < params.len() {
            return;
        }
        let types: Vec<TypeId> = args
            .iter()
            .enumerate()
            .map(|(at, &arg)| self.kencc_argument_type(arg, params.get(at).filter(|_| prototyped)))
            .collect();
        let Some(&format) = args.get(index) else {
            self.report(Diagnostic::warning("cant find format arg", call).with_code(FORMAT));
            return;
        };
        let indchar = self.types.pointer(self.types.int(IntKind::Char));
        if !self.kencc_same(indchar, types[index]) {
            let what = format!("format arg type {}", self.kencc_text(types[index]));
            self.report(Diagnostic::warning(what, call).with_code(FORMAT));
            return;
        }
        let Some(text) = self.format_literal(format) else { return };
        self.checkargs(&text, &types[index + 1..], param, call);
    }

    /// The bytes of a format that is a string literal, up to the first zero, as kencc's
    /// `cstring` is. Anything else is not read.
    fn format_literal(&self, format: ExprId) -> Option<Vec<u8>> {
        let mut at = format;
        while let ExprKind::Convert { operand, .. } = self.tast[at].kind {
            at = operand;
        }
        let ExprKind::Str(id) = self.tast[at].kind else { return None };
        let literal = &self.tast[id];
        if literal.encoding != rucc_lex::Encoding::Plain {
            return None;
        }
        let bytes = literal.elements.iter().map(|&element| element as u8);
        Some(bytes.take_while(|&byte| byte != 0).collect())
    }

    /// The type of an argument after `tcoma`. `param` is the type of the parameter, when the
    /// prototype has one for the argument.
    fn kencc_argument_type(&self, arg: ExprId, param: Option<&TypeId>) -> TypeId {
        // The conversions of the call and the reading of an object are not nodes in kencc. The
        // decay of an array or a function is, and it gives a pointer as `addaddr` does.
        let mut at = arg;
        while let ExprKind::Convert { kind, operand } = self.tast[at].kind {
            if matches!(kind, Conversion::ArrayDecay | Conversion::FunctionDecay) {
                break;
            }
            at = operand;
        }
        let ty = self.tast[at].ty;
        let (int, uint) = (self.types.int(IntKind::Int), self.types.int(IntKind::UInt));
        let promoted = |kind: TypeKind, float: bool| match kind {
            TypeKind::Int(IntKind::Char | IntKind::SChar | IntKind::Short) => Some(int),
            TypeKind::Int(IntKind::UChar | IntKind::UShort) => Some(uint),
            TypeKind::Float(FloatKind::Float) if float => Some(self.types.float(FloatKind::Double)),
            _ => None,
        };
        let to = match param {
            Some(&param) => {
                let kind = self.types.kind(self.kencc_resolve(param));
                Some(promoted(kind, false).unwrap_or(param))
            }
            None => promoted(self.types.kind(self.kencc_resolve(ty)), true),
        };
        match to {
            Some(to) if !self.kencc_same(to, ty) => to,
            _ => ty,
        }
    }

    /// The type behind typedefs and enumerations, which kencc does not keep apart.
    fn kencc_resolve(&self, ty: TypeId) -> TypeId {
        let ty = self.types.canonical(ty);
        match self.types.kind(ty) {
            TypeKind::Enum(id) => {
                self.types.enum_info(id).underlying.unwrap_or_else(|| self.types.int(IntKind::Int))
            }
            _ => ty,
        }
    }

    /// `checkargs`: the format `s` against the types of the arguments after it. `pos` is the
    /// place of the format, counted from one.
    fn checkargs(&mut self, format: &[u8], args: &[TypeId], mut pos: u32, call: Span) {
        let int = self.types.int(IntKind::Int);
        let uint = self.types.int(IntKind::UInt);
        let void = self.types.void();
        let mut next = args.iter().copied();
        let mut s = 0;
        loop {
            let Some(found) = format[s..].iter().position(|&byte| byte == b'%') else {
                if let Some(extra) = next.next() {
                    let what = format!("more arguments than format {}", self.kencc_text(extra));
                    self.report(Diagnostic::warning(what, call).with_code(FORMAT));
                }
                return;
            };
            s += found + 1;
            let Some(state) = self.varargck.as_mut() else { return };
            let (flags, stars, text) = state.getflag(format.get(s..).unwrap_or(&[]));
            for _ in 0..stars {
                pos += 1;
                let Some(arg) = next.next() else {
                    let what = format!("more format than arguments {text}");
                    self.report(Diagnostic::warning(what, call).with_code(FORMAT));
                    return;
                };
                if !self.kencc_same(int, arg) && !self.kencc_same(uint, arg) {
                    let what = format!(
                        "format mismatch '*' in {text} {}, arg {pos}",
                        self.kencc_text(arg)
                    );
                    self.report(Diagnostic::warning(what, call).with_code(FORMAT));
                }
            }
            let tprot = self.varargck.as_ref().map(|state| state.tprot.clone()).unwrap_or_default();
            let takes_none =
                tprot.iter().rev().any(|&(ty, bits)| self.kencc_same(void, ty) && bits == flags);
            if takes_none {
                s += 1;
                continue;
            }
            pos += 1;
            let Some(arg) = next.next() else {
                let what = format!("more format than arguments {text}");
                self.report(Diagnostic::warning(what, call).with_code(FORMAT));
                return;
            };
            let known =
                tprot.iter().rev().any(|&(ty, bits)| self.kencc_same(arg, ty) && bits == flags);
            if !known {
                let what = format!("format mismatch {text} {}, arg {pos}", self.kencc_text(arg));
                self.report(Diagnostic::warning(what, call).with_code(FORMAT));
            }
        }
    }
}
