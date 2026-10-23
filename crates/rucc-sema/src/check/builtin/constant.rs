//! The builtins whose answer is a constant, which is what a static initializer needs.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! Most of the library builtins mean the library function of the same name, and a call to one is
//! the whole of what they are. A handful are not like that, because a program writes them where
//! nothing may be called at all: `double n = __builtin_nan("");` at file scope is an initializer
//! for an object with static storage duration, and C 6.7.10p4 says such a thing has to be a
//! constant expression, so a compiler that leaves it as a call refuses a program gcc accepts.
//!
//! So these are answered here, in the front end, before the callee is looked up. Each is a value
//! nothing has to compute: an infinity and a nan are bit patterns, and the length or the order of
//! two string literals is known as soon as the literals are.
//!
//! # When it does not fold
//!
//! Every row wants its arguments written out. `__builtin_nan(p)` for a pointer that came from
//! somewhere else has nothing here to read, and `__builtin_strlen(s)` for a real string is a call
//! to `strlen` and always was. Nothing is reported for either: this hands back nothing, the call
//! is checked and lowered the ordinary way, and the answer is the library function, which is
//! what gcc does with the same program.
//!
//! That is also why the caller has to know which declarations this compiler made for itself. A
//! file with `__builtin_nan(p)` in it leaves a declaration of `__builtin_nan` at file scope, and
//! a `__builtin_nan("1")` later in the same file has to fold anyway.
//!
//! # What is not here
//!
//! The names whose answer is built out of an operand rather than written down.
//! `__builtin_copysign` and `__builtin_fabs` are answered as well and for the same reason, but
//! what they answer depends on what they were handed, so they have a node and a lowering and are
//! in `check/builtin/sign.rs` next door. This file is only the values that are constants however
//! they are reached.

use rucc_ast::{self as ast, BinaryOp, UnaryOp};
use rucc_base::Symbol;
use rucc_base::float::Float;
use rucc_diag::Span;
use rucc_lex::Encoding;
use rucc_types::{
    ArrayLen, FloatKind, Qualifiers, TypeKind, float_format, is_pointer, layout, pointee,
};

use crate::check::Checker;
use crate::decl::{DeclFlags, DeclId, DeclKind, Linkage, StorageDuration};
use crate::eval::bare;
use crate::expr::{Conversion, ExprId, ExprKind};
use crate::scope::Binding;
use crate::tast::Const;

/// How many `const` pointer objects deep a length follows one initializer into the next, which
/// only stops `static const char *const p = p;`.
const OBJECT_DEPTH: u32 = 8;

/// What one name answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// An infinity, of the type the name says. Takes nothing.
    Infinity(FloatKind),
    /// A nan, quiet or signalling, whose payload is written out as a string.
    Nan(FloatKind, bool),
    /// How long a string literal is, not counting its terminator.
    Length,
    /// How two string literals are ordered.
    Compare,
}

impl Answer {
    /// How many arguments the name takes, which is also how many string literals it wants.
    const fn arity(self) -> usize {
        match self {
            Answer::Infinity(_) => 0,
            Answer::Nan(..) => 1,
            Answer::Length => 1,
            Answer::Compare => 2,
        }
    }
}

/// Every name that is answered here.
///
/// `huge_val` and `inf` are two names for one thing. C89 had `HUGE_VAL` as the value `math.h`
/// returns on overflow, which need not have been an infinity on a machine without one, and gcc
/// gives the two builtins the same answer on every target it has.
const TABLE: &[(&str, Answer)] = &[
    ("__builtin_inf", Answer::Infinity(FloatKind::Double)),
    ("__builtin_inff", Answer::Infinity(FloatKind::Float)),
    ("__builtin_infl", Answer::Infinity(FloatKind::LongDouble)),
    ("__builtin_huge_val", Answer::Infinity(FloatKind::Double)),
    ("__builtin_huge_valf", Answer::Infinity(FloatKind::Float)),
    ("__builtin_huge_vall", Answer::Infinity(FloatKind::LongDouble)),
    ("__builtin_nan", Answer::Nan(FloatKind::Double, true)),
    ("__builtin_nanf", Answer::Nan(FloatKind::Float, true)),
    ("__builtin_nanl", Answer::Nan(FloatKind::LongDouble, true)),
    ("__builtin_nans", Answer::Nan(FloatKind::Double, false)),
    ("__builtin_nansf", Answer::Nan(FloatKind::Float, false)),
    ("__builtin_nansl", Answer::Nan(FloatKind::LongDouble, false)),
    // The decimal names, which gcc has on the targets that have the types. A name for a type the
    // target lacks is refused where its prototype is, the same as any other unknown builtin.
    ("__builtin_infd32", Answer::Infinity(FloatKind::Decimal32)),
    ("__builtin_infd64", Answer::Infinity(FloatKind::Decimal64)),
    ("__builtin_infd128", Answer::Infinity(FloatKind::Decimal128)),
    ("__builtin_nand32", Answer::Nan(FloatKind::Decimal32, true)),
    ("__builtin_nand64", Answer::Nan(FloatKind::Decimal64, true)),
    ("__builtin_nand128", Answer::Nan(FloatKind::Decimal128, true)),
    ("__builtin_nansd32", Answer::Nan(FloatKind::Decimal32, false)),
    ("__builtin_nansd64", Answer::Nan(FloatKind::Decimal64, false)),
    ("__builtin_nansd128", Answer::Nan(FloatKind::Decimal128, false)),
    // The `_FloatN` and `_FloatNx` names, which glibc's `bits/floatn-common.h` spells for
    // `HUGE_VAL_F128`, `INFINITY` and the rest when the compiler says it is gcc 7 or later. A
    // name for a type the target lacks is refused, as for the decimal names.
    ("__builtin_inff16", Answer::Infinity(FloatKind::Float16)),
    ("__builtin_huge_valf16", Answer::Infinity(FloatKind::Float16)),
    ("__builtin_nanf16", Answer::Nan(FloatKind::Float16, true)),
    ("__builtin_nansf16", Answer::Nan(FloatKind::Float16, false)),
    ("__builtin_inff32", Answer::Infinity(FloatKind::Float32)),
    ("__builtin_huge_valf32", Answer::Infinity(FloatKind::Float32)),
    ("__builtin_nanf32", Answer::Nan(FloatKind::Float32, true)),
    ("__builtin_nansf32", Answer::Nan(FloatKind::Float32, false)),
    ("__builtin_inff64", Answer::Infinity(FloatKind::Float64)),
    ("__builtin_huge_valf64", Answer::Infinity(FloatKind::Float64)),
    ("__builtin_nanf64", Answer::Nan(FloatKind::Float64, true)),
    ("__builtin_nansf64", Answer::Nan(FloatKind::Float64, false)),
    ("__builtin_inff128", Answer::Infinity(FloatKind::Float128)),
    ("__builtin_huge_valf128", Answer::Infinity(FloatKind::Float128)),
    ("__builtin_nanf128", Answer::Nan(FloatKind::Float128, true)),
    ("__builtin_nansf128", Answer::Nan(FloatKind::Float128, false)),
    ("__builtin_inff32x", Answer::Infinity(FloatKind::Float32x)),
    ("__builtin_huge_valf32x", Answer::Infinity(FloatKind::Float32x)),
    ("__builtin_nanf32x", Answer::Nan(FloatKind::Float32x, true)),
    ("__builtin_nansf32x", Answer::Nan(FloatKind::Float32x, false)),
    ("__builtin_inff64x", Answer::Infinity(FloatKind::Float64x)),
    ("__builtin_huge_valf64x", Answer::Infinity(FloatKind::Float64x)),
    ("__builtin_nanf64x", Answer::Nan(FloatKind::Float64x, true)),
    ("__builtin_nansf64x", Answer::Nan(FloatKind::Float64x, false)),
    ("__builtin_strlen", Answer::Length),
    ("__builtin_strcmp", Answer::Compare),
];

/// The plain names that are answered here too, which are the ones `string.h` declares.
const LIBRARY: &[(&str, Answer)] = &[("strlen", Answer::Length), ("strcmp", Answer::Compare)];

impl Checker<'_> {
    /// Answers a call to one of these, if the name is one and the arguments are written out.
    pub(super) fn constant_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        let spelled = self.text(name);
        let &(_, answer) = TABLE.iter().find(|(row, _)| *row == spelled)?;
        if let Answer::Infinity(kind) | Answer::Nan(kind, _) = answer {
            if !self.has_float_kind(kind) {
                return None;
            }
        }
        self.constant_answer(answer, args, span)
    }

    /// Answers a call to `strlen` or `strcmp` under its plain name, if the arguments are written
    /// out and the name still means the library function.
    ///
    /// A program reaches these through `string.h`, which in glibc spells neither prefixed name,
    /// and gcc folds them all the same: `.n = strlen("AB")` in a static array is accepted there
    /// with builtins on and refused with `-fno-builtin`. LTP's `TST_KCONFIG_INIT` is written that
    /// way. The declaration has to be the library's in shape too, an external function with a
    /// prototype that returns what the library one returns and takes as many arguments, since a
    /// program that declared its own `strlen` meant that one.
    pub(super) fn constant_library_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        let spelled = self.text(name);
        let &(_, answer) = LIBRARY.iter().find(|(row, _)| *row == spelled)?;
        if !self.library_shaped(name, answer) {
            return None;
        }
        self.constant_answer(answer, args, span)
    }

    /// Whether a plain name still means the library function, in what it was declared as too.
    fn library_shaped(&self, name: Symbol, answer: Answer) -> bool {
        if !self.cx.means_the_library(self.text(name)) {
            return false;
        }
        let Some(Binding::Decl(decl)) = self.scopes.lookup(name) else { return false };
        let decl = &self.tast[decl];
        if decl.kind != DeclKind::Function || decl.linkage != Linkage::External {
            return false;
        }
        let TypeKind::Function(id) = self.types.kind(self.types.canonical(decl.ty)) else {
            return false;
        };
        let signature = self.types.signature(id);
        let ret = match answer {
            Answer::Length => self.size_type(),
            _ => self.int(),
        };
        signature.prototyped
            && !signature.variadic
            && signature.params.len() == answer.arity()
            && self.types.canonical(signature.ret) == self.types.canonical(ret)
    }

    /// Answers a checked call to `strlen` whose argument points into a string gcc's front end
    /// can read, which is more than a literal written in the call.
    ///
    /// gcc folds the length of a `const` character array that was initialized with a string at
    /// every level, and from `-O1` up it reads through a `const` pointer object as well, inside a
    /// function and not in a static initializer. That makes the answer an integer constant
    /// expression, and the kernel's `fortify_kunit.c` asks exactly that of
    /// `__is_constexpr(__builtin_strlen(unchanging_12))` with `unchanging_12` a `static const
    /// char * const`. Measured on gcc 13, 14 and 16, which agree.
    pub(in crate::check) fn constant_length(
        &mut self,
        function: Option<Symbol>,
        checked: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let means = match self.text(name) {
            "__builtin_strlen" => self.declared_builtins.contains(&name),
            "strlen" => self.library_shaped(name, Answer::Length),
            _ => false,
        };
        let [arg] = checked else { return None };
        if !means {
            return None;
        }
        let (bytes, at) = self.pointed_string(*arg, 0)?;
        let rest = bytes.get(usize::try_from(at).ok()?..)?;
        let length = rest.iter().position(|byte| *byte == 0)? as u64;
        let ty = self.size_type();
        Some(self.constant(Const::Int(i128::from(length)), ty, span))
    }

    /// The bytes of the string object a pointer points into and how far into it, where gcc reads
    /// them too.
    fn pointed_string(&mut self, expr: ExprId, depth: u32) -> Option<(Vec<u8>, u64)> {
        if depth > OBJECT_DEPTH {
            return None;
        }
        match self.tast[expr].kind {
            ExprKind::Convert { kind: Conversion::ArrayDecay, operand } => {
                Some((self.string_object(operand)?, 0))
            }
            ExprKind::Convert { kind: Conversion::Lvalue, operand } => {
                let ExprKind::Decl(decl) = self.tast[operand].kind else { return None };
                self.pointer_object(decl, depth)
            }
            ExprKind::Cast(inner)
            | ExprKind::Convert { kind: Conversion::Pointer, operand: inner } => {
                if !is_pointer(&self.types, self.tast[inner].ty) {
                    return None;
                }
                self.pointed_string(inner, depth)
            }
            ExprKind::Binary { op: op @ (BinaryOp::Add | BinaryOp::Sub), lhs, rhs } => {
                let (pointer, index) = match is_pointer(&self.types, self.tast[lhs].ty) {
                    true => (lhs, rhs),
                    false if op == BinaryOp::Add => (rhs, lhs),
                    false => return None,
                };
                let step = self.step(pointer, index)?;
                let step = if op == BinaryOp::Sub { -step } else { step };
                let (bytes, at) = self.pointed_string(pointer, depth)?;
                Some((bytes, u64::try_from(i128::from(at) + step).ok()?))
            }
            ExprKind::Unary { op: UnaryOp::AddrOf, operand } => match self.tast[operand].kind {
                ExprKind::Subscript { base, index } => {
                    let step = self.step(base, index)?;
                    let (bytes, at) = self.pointed_string(base, depth)?;
                    Some((bytes, u64::try_from(i128::from(at) + step).ok()?))
                }
                _ => Some((self.string_object(operand)?, 0)),
            },
            _ => None,
        }
    }

    /// How many bytes adding a constant index to a pointer moves it.
    fn step(&self, pointer: ExprId, index: ExprId) -> Option<i128> {
        let target = pointee(&self.types, self.tast[pointer].ty)?;
        let size = layout(&self.types, target, self.cx.target).ok()?.size;
        let mut eval = self.eval();
        let value = eval.constant(index);
        let _ = eval.finish();
        let Ok(Const::Int(value)) = value else { return None };
        value.checked_mul(i128::from(size))
    }

    /// What a `const` pointer object was initialized to point at, which gcc reads once it
    /// optimizes and only inside a function.
    fn pointer_object(&mut self, decl: DeclId, depth: u32) -> Option<(Vec<u8>, u64)> {
        if !self.cx.optimizing || self.defining.is_none() {
            return None;
        }
        let node = &self.tast[decl];
        let quals = self.types.quals(self.types.canonical(node.ty));
        if node.kind != DeclKind::Object
            || node.flags.contains(DeclFlags::WEAK)
            || !quals.has(Qualifiers::CONST)
            || quals.has(Qualifiers::VOLATILE)
            || !is_pointer(&self.types, node.ty)
        {
            return None;
        }
        let entries = self.tast[node.init?].to_vec();
        let [entry] = entries.as_slice() else { return None };
        if entry.offset != 0 || entry.bit_width != 0 {
            return None;
        }
        self.pointed_string(entry.value, depth + 1)
    }

    /// The bytes of a narrow string literal, or of a `const` character array that was
    /// initialized with one, as many as the object holds.
    ///
    /// The array has to be one whose initializer nothing can change and that the linker cannot
    /// swap for another: not `volatile` and not weak. gcc leaves out a `static` one inside a
    /// function as well, which keeps its initializer somewhere the front end does not read.
    fn string_object(&self, expr: ExprId) -> Option<Vec<u8>> {
        let (elem, len) = match bare(&self.types, self.tast[expr].ty) {
            TypeKind::Array { elem, len: ArrayLen::Fixed(len) } => (elem, len),
            _ => return None,
        };
        if layout(&self.types, elem, self.cx.target).ok()?.size != 1 {
            return None;
        }
        let mut bytes = match self.tast[expr].kind {
            ExprKind::Str(id) => {
                let literal = &self.tast[id];
                if !matches!(literal.encoding, Encoding::Plain | Encoding::Utf8) {
                    return None;
                }
                literal.bytes(self.cx.target)
            }
            ExprKind::Decl(decl) => {
                let node = &self.tast[decl];
                let quals = self.types.quals(self.types.canonical(elem));
                if node.kind != DeclKind::Object
                    || node.flags.contains(DeclFlags::WEAK)
                    || !quals.has(Qualifiers::CONST)
                    || quals.has(Qualifiers::VOLATILE)
                    || (node.duration != StorageDuration::Automatic
                        && node.linkage == Linkage::None)
                {
                    return None;
                }
                let entries = self.tast[node.init?].to_vec();
                let [entry] = entries.as_slice() else { return None };
                if entry.offset != 0 || !matches!(self.tast[entry.value].kind, ExprKind::Str(_)) {
                    return None;
                }
                self.string_object(entry.value)?
            }
            _ => return None,
        };
        bytes.resize(usize::try_from(len).ok()?, 0);
        Some(bytes)
    }

    /// The answer itself, once the name has been found to be one of these.
    fn constant_answer(
        &mut self,
        answer: Answer,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        if written.len() != answer.arity() {
            // A call with the wrong number of arguments is a mistake, and the ordinary path is
            // where it is reported, against the prototype the table gives the name.
            return None;
        }
        match answer {
            Answer::Infinity(kind) => {
                let format = float_format(kind, self.cx.target);
                Some(self.folded_float(kind, Float::infinity(format, false), span))
            }
            Answer::Nan(kind, quiet) => {
                let payload = payload(&self.literal(written[0])?)?;
                let format = float_format(kind, self.cx.target);
                Some(self.folded_float(kind, Float::nan_with(format, false, quiet, payload), span))
            }
            Answer::Length => {
                let bytes = self.literal(written[0])?;
                let length = length(&bytes);
                let ty = self.size_type();
                Some(self.constant(Const::Int(i128::from(length)), ty, span))
            }
            Answer::Compare => {
                let left = self.literal(written[0])?;
                let right = self.literal(written[1])?;
                let order = compare(&left, &right);
                let ty = self.int();
                Some(self.constant(Const::Int(i128::from(order)), ty, span))
            }
        }
    }

    /// One floating constant, of the type the name says.
    fn folded_float(&mut self, kind: FloatKind, value: Float, span: Span) -> ExprId {
        let ty = self.types.float(kind);
        self.constant(Const::Float(value), ty, span)
    }

    /// The bytes of an argument that is written as a narrow string literal, terminator included.
    ///
    /// Nothing else counts. A wide literal is the wrong type for every name in the table, and an
    /// expression that merely folds to the address of a literal is not what gcc asks for either:
    /// `__builtin_strlen(p)` for a `const char *p = "abc";` is a call there and is a call here.
    fn literal(&self, arg: ast::ExprId) -> Option<Vec<u8>> {
        let ast::Expr::Str(id) = self.ast[arg] else { return None };
        let literal = &self.ast[id];
        matches!(literal.encoding, Encoding::Plain | Encoding::Utf8)
            .then(|| literal.bytes(self.cx.target))
    }
}

/// How long a string is, which is where its first zero byte is.
///
/// A literal may have one written inside it, and `__builtin_strlen("a\0b")` is one for the same
/// reason `strlen` of it would be.
fn length(bytes: &[u8]) -> u64 {
    bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len()) as u64
}

/// How two strings are ordered, in the way `strcmp` orders them.
///
/// The comparison is on `unsigned char`, C 7.24.4p1, whatever the sign of a plain `char` is on
/// the target. That is what makes `strcmp("X", "X\376")` negative: the second string has a two
/// hundred and fifty four where the first has its terminator.
///
/// The answer is minus one, zero or one. C promises only the sign, and those are the three values
/// gcc folds to.
fn compare(left: &[u8], right: &[u8]) -> i32 {
    for (left, right) in left.iter().zip(right) {
        if left != right {
            return if left < right { -1 } else { 1 };
        }
        // Both are the terminator, so neither string goes any further whatever is written after
        // it in the literal.
        if *left == 0 {
            return 0;
        }
    }
    0
}

/// The payload a nan is written with, or nothing when the string is not one.
///
/// The string is read the way `strtol` reads one with a base of zero, which is what gcc does with
/// it: leading space, then an optional sign, then a base taken from the prefix, and every
/// character after that has to be a digit of that base or the whole thing is not a payload. An
/// empty string is a payload of zero, which is the plain quiet nan and is what `math.h` writes
/// `NAN` as.
///
/// A value too large for the format is cut down to size rather than refused, which is
/// [`Float::nan_with`]'s doing and is gcc's answer as well.
fn payload(bytes: &[u8]) -> Option<u128> {
    // The string as the library function would see it, which stops at the first zero however
    // many bytes the literal has after it.
    let text = std::str::from_utf8(&bytes[..length(bytes) as usize]).ok()?;
    let text = text.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    // The sign is read and thrown away. A payload is a bit pattern and has no sign, and gcc
    // answers `__builtin_nan("-1")` and `__builtin_nan("1")` with the same number.
    let text = text.strip_prefix(['+', '-']).unwrap_or(text);
    if text.is_empty() {
        return Some(0);
    }
    let (radix, digits) = match text.as_bytes() {
        [b'0', b'x' | b'X', rest @ ..] => (16, rest),
        // A lone zero is a zero in any base, which is what makes this reachable with nothing
        // left to read.
        [b'0', rest @ ..] => (8, rest),
        rest => (10, rest),
    };
    if digits.is_empty() {
        return Some(0);
    }
    let mut value: u128 = 0;
    for &byte in digits {
        let digit = char::from(byte).to_digit(radix)?;
        value = value.wrapping_mul(u128::from(radix)).wrapping_add(u128::from(digit));
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every name here has to be a row of the roster, or it is a builtin `__has_builtin` has
    /// never heard of and a header that asks takes its fallback path for no reason.
    #[test]
    fn every_name_answered_here_is_a_row_of_the_table_and_says_it_is_done() {
        for (name, _) in TABLE {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            // These have a type, unlike the families that take whatever they are handed, because
            // the call that does not fold is a call and has to be checked against something.
            assert!(!feature.signature.is_empty(), "{name} is called when it does not fold");
        }
    }

    /// The names are what the lookup searches, so a name written twice is a second row nothing
    /// can ever reach.
    #[test]
    fn no_name_is_in_the_table_twice() {
        let mut names: Vec<&str> = TABLE.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let all = names.len();
        names.dedup();
        assert_eq!(names.len(), all, "a name is in the table twice");
    }

    /// A string as the table's own code sees one, with the terminator the object would have.
    fn text(spelled: &str) -> Vec<u8> {
        let mut bytes = spelled.as_bytes().to_vec();
        bytes.push(0);
        bytes
    }

    /// Each of these is what gcc 16 answers for the same string.
    #[test]
    fn a_payload_is_read_the_way_strtol_reads_a_number() {
        for (spelled, value) in [
            ("", 0),
            ("0", 0),
            ("1", 1),
            ("0x1", 1),
            ("0X1", 1),
            ("010", 8),
            ("0xff", 255),
            ("8", 8),
            ("-1", 1),
            ("+2", 2),
            (" 1", 1),
            ("\t 0x10", 16),
            // A prefix with nothing after it, which gcc reads as the zero it starts with.
            ("0x", 0),
        ] {
            assert_eq!(payload(&text(spelled)), Some(value), "for {spelled:?}");
        }
        // Every one of these is a string gcc leaves as a call rather than answering, which is
        // the same set: a payload has to be the whole of what is written.
        for spelled in ["abc", "1x", "1 ", "08", "0x1g", "0x1p3", "--1"] {
            assert_eq!(payload(&text(spelled)), None, "for {spelled:?}");
        }
        // A literal with a zero written inside it stops there, the way the library would.
        assert_eq!(payload(b"1\x002\0"), Some(1));
    }

    #[test]
    fn a_length_stops_at_the_first_zero_and_an_order_is_over_unsigned_bytes() {
        assert_eq!(length(&text("")), 0);
        assert_eq!(length(&text("abc")), 3);
        assert_eq!(length(b"a\0bc\0"), 1);
        assert_eq!(compare(&text("X"), &text("X")), 0);
        assert_eq!(compare(&text("a"), &text("b")), -1);
        assert_eq!(compare(&text("b"), &text("a")), 1);
        assert_eq!(compare(&text("a"), &text("ab")), -1);
        // The byte is two hundred and fifty four and not minus two, which is the whole of what
        // `execute/921007-1.c` in the torture suite is about.
        assert_eq!(compare(&text("X"), b"X\xfe\0"), -1);
        // Nothing past the terminator is part of either string.
        assert_eq!(compare(b"a\0x\0", b"a\0y\0"), 0);
    }
}
