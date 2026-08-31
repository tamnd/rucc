//! The `#if` expression evaluator.
//!
//! Design: `spec/05-preprocessor.md` section 5.4.
//!
//! `#if` expressions are a small language of their own: integer constants and character
//! constants, the usual operators, and nothing else. Every value is `intmax_t` or
//! `uintmax_t`, there are no variables, and an identifier that survived macro expansion is
//! zero. That last rule is why `#if FOO` works on a project that never defined `FOO`, and it
//! is also why a typo in a feature test silently takes the wrong branch, which is what
//! `-Wundef` is for.
//!
//! Short circuiting is not an optimisation here, it is required. `#if defined(X) && X > 2`
//! has to not evaluate `X > 2` when `X` is undefined, and `#if 1 ? 2 : 1/0` has to not
//! divide. The evaluator threads a `live` flag through so that a dead branch is still parsed
//! for syntax but reports nothing.

use rucc_base::Interner;
use rucc_diag::{Diagnostic, Span};
use rucc_lex::{PpTokenKind, Punct};

use crate::token::Tok;

/// A value in a `#if` expression.
///
/// Sixty four bits plus a signedness flag, which is `intmax_t` and `uintmax_t` on every
/// target we have. The bits are kept as unsigned and interpreted on use, so that the
/// wrapping behaviour is the same whichever operand is signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Val {
    bits: u64,
    unsigned: bool,
}

impl Val {
    const ZERO: Val = Val { bits: 0, unsigned: false };

    fn signed(v: i64) -> Val {
        Val { bits: v as u64, unsigned: false }
    }

    fn boolean(b: bool) -> Val {
        Val::signed(i64::from(b))
    }

    fn as_signed(self) -> i64 {
        self.bits as i64
    }

    fn is_true(self) -> bool {
        self.bits != 0
    }
}

/// Evaluates a `#if` expression, returning whether the branch is taken.
///
/// `tokens` is the line after the directive name, with `defined` already resolved and macros
/// already expanded. Anything wrong is reported and the expression evaluates to false, which
/// is the recovery that produces the fewest follow-on errors: a file that fails to configure
/// itself is better than one that takes every branch.
pub(crate) fn evaluate(
    tokens: &[Tok],
    interner: &Interner,
    diagnostics: &mut Vec<Diagnostic>,
    line: Span,
) -> bool {
    if tokens.is_empty() {
        diagnostics.push(Diagnostic::error("`#if` with no expression", line).with_code("E0320"));
        return false;
    }
    let mut eval = Eval { tokens, at: 0, interner, diagnostics, line };
    let value = eval.conditional(true);
    if eval.at < eval.tokens.len() {
        let span = eval.tokens[eval.at].report_span();
        eval.diagnostics.push(
            Diagnostic::error("extra tokens after the `#if` expression", span).with_code("E0321"),
        );
    }
    value.is_true()
}

struct Eval<'a> {
    tokens: &'a [Tok],
    at: usize,
    interner: &'a Interner,
    diagnostics: &'a mut Vec<Diagnostic>,
    /// The whole directive line, for errors that are about something missing rather than
    /// about a token that is present.
    line: Span,
}

impl Eval<'_> {
    fn peek(&self) -> Option<Tok> {
        self.tokens.get(self.at).copied()
    }

    fn peek_punct(&self) -> Option<Punct> {
        self.peek().and_then(|t| t.punct())
    }

    fn eat(&mut self, p: Punct) -> bool {
        if self.peek_punct() == Some(p) {
            self.at += 1;
            return true;
        }
        false
    }

    fn span(&self) -> Span {
        self.peek().map_or(self.line, |t| t.report_span())
    }

    fn error(&mut self, message: impl Into<String>, code: &'static str, live: bool) {
        if live {
            let span = self.span();
            self.diagnostics.push(Diagnostic::error(message, span).with_code(code));
        }
    }

    /// `a ? b : c`, right associative, and the only place where evaluation and parsing come
    /// apart: both arms are parsed, one of them is evaluated.
    fn conditional(&mut self, live: bool) -> Val {
        let cond = self.binary(0, live);
        if !self.eat(Punct::Question) {
            return cond;
        }
        let taken = cond.is_true();
        let then = self.conditional(live && taken);
        if !self.eat(Punct::Colon) {
            self.error("expected `:` in a conditional expression", "E0322", live);
            return Val::ZERO;
        }
        let otherwise = self.conditional(live && !taken);
        let mut result = if taken { then } else { otherwise };
        // The result type comes from both arms whichever one is chosen, so `#if 1 ? -1 : 0u`
        // is unsigned, the same as it would be in C.
        result.unsigned = then.unsigned || otherwise.unsigned;
        result
    }

    /// Precedence climbing over the binary operators.
    fn binary(&mut self, min: u8, live: bool) -> Val {
        let mut lhs = self.unary(live);
        loop {
            let Some((punct, prec)) = self.peek_punct().and_then(|p| Some((p, binary_op(p)?)))
            else {
                return lhs;
            };
            if prec < min {
                return lhs;
            }
            self.at += 1;
            // `&&` and `||` decide whether the right side is evaluated at all, which matters
            // because `#if defined(X) && X > 2` must not look at `X` when it is undefined.
            let right_live = match punct {
                Punct::AmpAmp => live && lhs.is_true(),
                Punct::PipePipe => live && !lhs.is_true(),
                _ => live,
            };
            let rhs = self.binary(prec + 1, right_live);
            lhs = self.apply(punct, lhs, rhs, live);
        }
    }

    fn unary(&mut self, live: bool) -> Val {
        let Some(tok) = self.peek() else {
            self.error("expected a value in a `#if` expression", "E0323", live);
            return Val::ZERO;
        };
        if let Some(p) = tok.punct() {
            match p {
                Punct::Plus => {
                    self.at += 1;
                    return self.unary(live);
                }
                Punct::Minus => {
                    self.at += 1;
                    let v = self.unary(live);
                    return Val { bits: (v.bits as i64).wrapping_neg() as u64, ..v };
                }
                Punct::Tilde => {
                    self.at += 1;
                    let v = self.unary(live);
                    return Val { bits: !v.bits, ..v };
                }
                Punct::Bang => {
                    self.at += 1;
                    let v = self.unary(live);
                    return Val::boolean(!v.is_true());
                }
                Punct::LParen => {
                    self.at += 1;
                    let v = self.conditional(live);
                    if !self.eat(Punct::RParen) {
                        self.error("expected `)`", "E0322", live);
                    }
                    return v;
                }
                _ => {}
            }
        }
        self.at += 1;
        match tok.kind {
            PpTokenKind::Number => self.number(tok, live),
            PpTokenKind::CharConst => self.char_const(tok, live),
            // Everything that survived expansion and is not a number is zero. C23 spells two
            // of them `true` and `false` and means 1 and 0.
            PpTokenKind::Ident => {
                let text = tok.value.map(|v| self.interner.resolve(v)).unwrap_or_default();
                match text {
                    "true" => Val::signed(1),
                    _ => Val::ZERO,
                }
            }
            _ => {
                self.at -= 1;
                self.error("expected a value in a `#if` expression", "E0323", live);
                self.at += 1;
                Val::ZERO
            }
        }
    }

    fn number(&mut self, tok: Tok, live: bool) -> Val {
        let text = tok.value.map(|v| self.interner.resolve(v)).unwrap_or_default();
        match parse_integer(text) {
            Ok(v) => v,
            Err(problem) => {
                if live {
                    self.diagnostics
                        .push(Diagnostic::error(problem, tok.report_span()).with_code("E0324"));
                }
                Val::ZERO
            }
        }
    }

    fn char_const(&mut self, tok: Tok, live: bool) -> Val {
        let text = tok.value.map(|v| self.interner.resolve(v)).unwrap_or_default();
        match parse_char(text) {
            Ok(v) => v,
            Err(problem) => {
                if live {
                    self.diagnostics
                        .push(Diagnostic::error(problem, tok.report_span()).with_code("E0325"));
                }
                Val::ZERO
            }
        }
    }

    fn apply(&mut self, op: Punct, lhs: Val, rhs: Val, live: bool) -> Val {
        // The usual arithmetic conversions, which here reduce to one rule: if either side is
        // unsigned the whole operation is unsigned. This is the rule that makes
        // `#if -1 < 0u` false, and it catches people out in `#if` exactly as it does in C.
        let unsigned = lhs.unsigned || rhs.unsigned;
        let (a, b) = (lhs.bits, rhs.bits);
        let (sa, sb) = (lhs.as_signed(), rhs.as_signed());
        let arith = |bits: u64| Val { bits, unsigned };

        match op {
            Punct::Star => arith(a.wrapping_mul(b)),
            Punct::Slash | Punct::Percent => {
                if b == 0 {
                    self.error("division by zero in a `#if` expression", "E0326", live);
                    return Val::ZERO;
                }
                match (op, unsigned) {
                    (Punct::Slash, true) => arith(a / b),
                    (Punct::Slash, false) => arith(sa.wrapping_div(sb) as u64),
                    (_, true) => arith(a % b),
                    (_, false) => arith(sa.wrapping_rem(sb) as u64),
                }
            }
            Punct::Plus => arith(a.wrapping_add(b)),
            Punct::Minus => arith(a.wrapping_sub(b)),
            // A shift takes its type from the left operand, not from both, and a shift count
            // at or past the width is undefined in C. GCC produces zero and so do we.
            Punct::Shl | Punct::Shr => {
                let count = if rhs.unsigned || sb >= 0 { b } else { return Val::ZERO };
                let out = if count >= 64 {
                    if op == Punct::Shl || lhs.unsigned || lhs.as_signed() >= 0 {
                        0
                    } else {
                        u64::MAX
                    }
                } else if op == Punct::Shl {
                    a << count
                } else if lhs.unsigned {
                    a >> count
                } else {
                    (sa >> count) as u64
                };
                Val { bits: out, unsigned: lhs.unsigned }
            }
            Punct::Lt => Val::boolean(if unsigned { a < b } else { sa < sb }),
            Punct::Gt => Val::boolean(if unsigned { a > b } else { sa > sb }),
            Punct::Le => Val::boolean(if unsigned { a <= b } else { sa <= sb }),
            Punct::Ge => Val::boolean(if unsigned { a >= b } else { sa >= sb }),
            Punct::EqEq => Val::boolean(a == b),
            Punct::Ne => Val::boolean(a != b),
            Punct::Amp => arith(a & b),
            Punct::Caret => arith(a ^ b),
            Punct::Pipe => arith(a | b),
            Punct::AmpAmp => Val::boolean(lhs.is_true() && rhs.is_true()),
            Punct::PipePipe => Val::boolean(lhs.is_true() || rhs.is_true()),
            Punct::Comma => rhs,
            _ => Val::ZERO,
        }
    }
}

/// Precedence of a binary operator, higher binds tighter.
fn binary_op(p: Punct) -> Option<u8> {
    let prec = match p {
        Punct::Comma => 1,
        Punct::PipePipe => 2,
        Punct::AmpAmp => 3,
        Punct::Pipe => 4,
        Punct::Caret => 5,
        Punct::Amp => 6,
        Punct::EqEq | Punct::Ne => 7,
        Punct::Lt | Punct::Gt | Punct::Le | Punct::Ge => 8,
        Punct::Shl | Punct::Shr => 9,
        Punct::Plus | Punct::Minus => 10,
        Punct::Star | Punct::Slash | Punct::Percent => 11,
        _ => return None,
    };
    Some(prec)
}

/// Turns a preprocessing number into a value.
///
/// A pp-number is looser than an integer constant on purpose, so this is where `1.5` and
/// `0x1p3` are finally rejected. Digit separators from C23 are stripped first.
fn parse_integer(text: &str) -> Result<Val, String> {
    let cleaned: String = text.chars().filter(|&c| c != '\'').collect();
    let lower = cleaned.to_ascii_lowercase();
    let (radix, digits) = if let Some(rest) = lower.strip_prefix("0x") {
        (16, rest)
    } else if let Some(rest) = lower.strip_prefix("0b") {
        (2, rest)
    // A leading zero only means octal when a digit follows it. `0u` and `0L` are decimal zero
    // with a suffix, and reading them as octal leaves an empty body and a spurious error.
    } else if lower.starts_with('0') && lower.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
        (8, &lower[1..])
    } else {
        (10, lower.as_str())
    };

    let end = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    let (body, suffix) = digits.split_at(end);
    if body.is_empty() {
        return Err(format!("`{text}` is not an integer constant"));
    }
    let mut unsigned = false;
    let mut longs = 0;
    let mut seen_u = false;
    let mut rest = suffix;
    while !rest.is_empty() {
        if let Some(next) = rest.strip_prefix("ll") {
            longs += 2;
            rest = next;
        } else if let Some(next) = rest.strip_prefix('l') {
            longs += 1;
            rest = next;
        } else if let Some(next) = rest.strip_prefix('u') {
            if seen_u {
                return Err(format!("`{text}` is not an integer constant"));
            }
            seen_u = true;
            unsigned = true;
            rest = next;
        } else if let Some(next) = rest.strip_prefix("wb") {
            // C23 bit-precise constants. In a `#if` they are just integers.
            rest = next;
        } else if let Some(next) = rest.strip_prefix('z') {
            rest = next;
        } else if rest.starts_with('.') || rest.starts_with('e') || rest.starts_with('p') {
            return Err("a floating constant cannot appear in a `#if` expression".to_string());
        } else {
            return Err(format!("`{text}` is not an integer constant"));
        }
        if longs > 2 {
            return Err(format!("`{text}` is not an integer constant"));
        }
    }

    let bits = u64::from_str_radix(body, radix)
        .map_err(|_| format!("`{text}` does not fit in the widest integer type"))?;
    // A decimal constant with no `u` that does not fit in a signed 64 bit value is unsigned
    // in every real compiler, and warning about it is `-Wpedantic` territory rather than an
    // error that stops a build.
    if bits > i64::MAX as u64 {
        unsigned = true;
    }
    Ok(Val { bits, unsigned })
}

/// Turns a character constant into a value.
///
/// A narrow character constant is `int` and signed on every target we support. A multi
/// character constant is implementation defined and we do what GCC does, packing the
/// characters big end first, because the only code that uses them expects that.
fn parse_char(text: &str) -> Result<Val, String> {
    let body = text
        .trim_start_matches(['L', 'u', 'U', '8'])
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .ok_or_else(|| format!("`{text}` is not a character constant"))?;
    let wide = !text.starts_with('\'');

    let mut chars = body.chars().peekable();
    let mut value: u64 = 0;
    let mut count = 0;
    while let Some(c) = chars.next() {
        let scalar = if c == '\\' { escape(&mut chars)? } else { u64::from(c as u32) };
        value = if count == 0 { scalar } else { (value << 8) | (scalar & 0xff) };
        count += 1;
    }
    if count == 0 {
        return Err("empty character constant".to_string());
    }
    if wide {
        return Ok(Val { bits: value, unsigned: false });
    }
    if count == 1 {
        // Plain `char` is signed on x86-64 Linux and unsigned on AArch64, which changes what
        // `#if '\xff' < 0` means. The target answer belongs to `rucc-target` and arrives with
        // the session plumbing; until then this is the x86-64 answer.
        return Ok(Val::signed(value as u8 as i8 as i64));
    }
    Ok(Val::signed(value as u32 as i32 as i64))
}

/// Reads one escape sequence, the backslash already consumed.
fn escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<u64, String> {
    let c = chars.next().ok_or_else(|| "incomplete escape sequence".to_string())?;
    let simple = match c {
        'n' => Some(b'\n'),
        't' => Some(b'\t'),
        'r' => Some(b'\r'),
        '0'..='7' => None,
        'a' => Some(7),
        'b' => Some(8),
        'f' => Some(12),
        'v' => Some(11),
        'e' => Some(27),
        '\\' | '\'' | '"' | '?' => Some(c as u8),
        'x' | 'u' | 'U' => None,
        _ => return Err(format!("unknown escape sequence `\\{c}`")),
    };
    if let Some(byte) = simple {
        return Ok(u64::from(byte));
    }
    if c == 'x' {
        let mut value: u64 = 0;
        let mut any = false;
        while let Some(&d) = chars.peek() {
            let Some(digit) = d.to_digit(16) else { break };
            value = value.wrapping_mul(16).wrapping_add(u64::from(digit));
            any = true;
            chars.next();
        }
        if !any {
            return Err("`\\x` with no hexadecimal digits".to_string());
        }
        return Ok(value);
    }
    if c == 'u' || c == 'U' {
        let width = if c == 'u' { 4 } else { 8 };
        let mut value: u64 = 0;
        for _ in 0..width {
            let d = chars.next().and_then(|d| d.to_digit(16));
            let Some(digit) = d else {
                return Err("incomplete universal character name".to_string());
            };
            value = value * 16 + u64::from(digit);
        }
        return Ok(value);
    }
    // Octal, up to three digits including the one already read.
    let mut value = u64::from(c.to_digit(8).unwrap_or(0));
    for _ in 0..2 {
        let Some(digit) = chars.peek().and_then(|d| d.to_digit(8)) else { break };
        value = value * 8 + u64::from(digit);
        chars.next();
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_hex_octal_and_binary_all_parse() {
        assert_eq!(parse_integer("42").expect("decimal").bits, 42);
        assert_eq!(parse_integer("0x2a").expect("hex").bits, 42);
        assert_eq!(parse_integer("052").expect("octal").bits, 42);
        assert_eq!(parse_integer("0b101010").expect("binary").bits, 42);
        assert_eq!(parse_integer("0").expect("zero").bits, 0);
    }

    #[test]
    fn digit_separators_are_stripped() {
        assert_eq!(parse_integer("1'000'000").expect("separated").bits, 1_000_000);
    }

    #[test]
    fn suffixes_set_the_type() {
        assert!(parse_integer("1u").expect("unsigned").unsigned);
        assert!(!parse_integer("1ll").expect("long long").unsigned);
        assert!(parse_integer("1ull").expect("both").unsigned);
    }

    #[test]
    fn a_floating_constant_is_rejected() {
        assert!(parse_integer("1.5").is_err());
        assert!(parse_integer("1e3").is_err());
    }

    #[test]
    fn a_value_too_large_for_intmax_becomes_unsigned() {
        let v = parse_integer("18446744073709551615").expect("fits in uintmax");
        assert!(v.unsigned);
        assert_eq!(v.bits, u64::MAX);
    }

    #[test]
    fn character_constants_and_escapes() {
        assert_eq!(parse_char("'a'").expect("plain").bits, 97);
        assert_eq!(parse_char("'\\n'").expect("newline").bits, 10);
        assert_eq!(parse_char("'\\0'").expect("nul").bits, 0);
        assert_eq!(parse_char("'\\x41'").expect("hex").bits, 65);
        assert_eq!(parse_char("'\\101'").expect("octal").bits, 65);
    }

    #[test]
    fn a_narrow_character_constant_is_signed() {
        assert_eq!(parse_char("'\\xff'").expect("high bit set").as_signed(), -1);
        assert_eq!(parse_char("L'\\xff'").expect("wide").as_signed(), 255);
    }

    #[test]
    fn a_multi_character_constant_packs_big_end_first() {
        assert_eq!(parse_char("'ab'").expect("two chars").bits, (97 << 8) | 98);
    }
}
