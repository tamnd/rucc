//! The format checks: what `-Wformat` says about a call to `printf`, `scanf` or `strftime`, or to a
//! function a program marked `format`, whose format string is a literal.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! The format string is a small program the library runs over the arguments, and a mistake in it
//! is a mistake the compiler can see: `printf("%ld", 1)` reads a `long` where an `int` was passed,
//! which works on one target and prints garbage on the next. gcc reads the string and says so, and
//! a program that builds warning free under gcc's `-Wall` expects to here as well, so what is said
//! and where is gcc 16's, measured one case at a time:
//!
//! * The warnings are off until `-Wformat` or `-Wall` asks for them, which is gcc's default, and
//!   are filed under the four names gcc files them under, so `-Wno-format-extra-args` quiets the
//!   one about arguments left over and nothing else.
//! * The functions checked are the ones a declaration marked `format(archetype, string, first)`
//!   and, without any attribute, the C library's own `printf`, `scanf` and `strftime` families,
//!   which gcc knows by name unless `-fno-builtin` took the name away. A Windows target is left
//!   out of the second half, because whether `printf` there is msvcrt's or mingw's C99 one is a
//!   macro's to say, and of the archetypes only the `gnu_` ones are read there.
//! * Only a literal is read. A format that is a variable is the program's to vouch for, and gcc
//!   says nothing about it below `-Wformat-security`. Both arms of a conditional are read, and a
//!   call to a function marked `format_arg`, which is how `gettext` hands a translation back, is
//!   read through to the literal it was handed.
//! * Each argument is judged after the default argument promotions, so a `float` is a `double`
//!   and a `char` is an `int`, and signedness is not judged at all, since that is
//!   `-Wformat-signedness`. `long` and `long long` are two types even where they are one width,
//!   because a program that passes one for the other is wrong on the targets where they are not.
//! * The types are spelled the way gcc spells them, `long unsigned int` and not `unsigned long`,
//!   with a typedef followed by what it stands for, so the message reads the same here as there.
//!
//! What is not here: the checks `-Wformat=2` adds, `-Wformat-overflow` and `-Wformat-truncation`,
//! which need the values and not only the types, the `ms_` archetypes, `strfmon`, and the
//! modifiers of `strftime` used with a conversion that has no use for them.

use rucc_ast::{AttrArg, Attribute};
use rucc_diag::{Diagnostic, Span};
use rucc_lex::Encoding;
use rucc_types::{FloatKind, IntKind, Qualifiers, TypeId, TypeKind, int_width};

use crate::check::Checker;
use crate::decl::DeclId;
use crate::expr::{ExprId, ExprKind};

/// The code of the format warnings, which answer to `-Wformat`.
const FORMAT: &str = "E0785";

/// The code of the warning about arguments the format does not read, which answers to
/// `-Wformat-extra-args`.
const EXTRA_ARGS: &str = "E0786";

/// The code of the warning about a zero inside a format, which answers to `-Wformat-contains-nul`.
const CONTAINS_NUL: &str = "E0787";

/// The code of the warning about an empty format, which answers to `-Wformat-zero-length`.
const ZERO_LENGTH: &str = "E0788";

/// Which language a format string is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::check) enum Family {
    /// `printf`'s.
    Printf,
    /// `scanf`'s.
    Scanf,
    /// `strftime`'s, which reads no arguments.
    Strftime,
}

impl Family {
    /// The archetype gcc names in a message, which on every target this checks is the `gnu_` one.
    fn archetype(self) -> &'static str {
        match self {
            Family::Printf => "gnu_printf",
            Family::Scanf => "gnu_scanf",
            Family::Strftime => "gnu_strftime",
        }
    }
}

/// What a `format` attribute said: the language, which parameter is the string, and which
/// argument is the first the string reads, counting from one, with nought for a function that
/// is handed a `va_list` and so has nothing to check the conversions against.
#[derive(Debug, Clone, Copy)]
pub(in crate::check) struct Format {
    family: Family,
    string: usize,
    first: usize,
}

/// The C library's functions gcc checks without any attribute, with the same three numbers.
const LIBRARY: &[(&str, Family, usize, usize)] = &[
    ("dprintf", Family::Printf, 2, 3),
    ("fprintf", Family::Printf, 2, 3),
    ("fscanf", Family::Scanf, 2, 3),
    ("printf", Family::Printf, 1, 2),
    ("scanf", Family::Scanf, 1, 2),
    ("snprintf", Family::Printf, 3, 4),
    ("sprintf", Family::Printf, 2, 3),
    ("sscanf", Family::Scanf, 2, 3),
    ("strftime", Family::Strftime, 3, 0),
    ("vdprintf", Family::Printf, 2, 0),
    ("vfprintf", Family::Printf, 2, 0),
    ("vfscanf", Family::Scanf, 2, 0),
    ("vprintf", Family::Printf, 1, 0),
    ("vscanf", Family::Scanf, 1, 0),
    ("vsnprintf", Family::Printf, 3, 0),
    ("vsprintf", Family::Printf, 2, 0),
    ("vsscanf", Family::Scanf, 2, 0),
];

/// The length modifier of one directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    None,
    Hh,
    H,
    L,
    Ll,
    /// `L`, which is `long double` on a floating conversion and `long long` on an integer one.
    BigL,
    J,
    Z,
    T,
    /// C23's `w32` and `wf32`, whose types are read no further.
    W,
    /// `H`, `D` and `DD`, the decimal floating ones, whose types are read no further.
    Decimal,
}

/// What one conversion reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Signed,
    Unsigned,
    Floating,
    Char,
    WideChar,
    String,
    WideString,
    Pointer,
    Count,
    Errno,
}

/// What an argument is matched against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Base {
    /// An integer of this rank, either signedness, from [`rank`].
    Rank(u8),
    /// Exactly this floating type.
    Float(FloatKind),
    /// Any of the three character types.
    AnyChar,
    /// `wchar_t`, which is an integer as wide as the target says.
    Wide,
    /// Anything at all, which is what `%p` takes from under its pointer.
    Anything,
}

/// The type a directive wants, as a thing to match and as the words gcc says it in.
#[derive(Debug, Clone)]
struct Want {
    base: Base,
    /// How many pointers are above the base.
    depth: u8,
    name: String,
}

/// Where in the string the caret goes, and the literal it is in.
struct Text {
    chars: Vec<u32>,
    /// The offset in the source of each element of the literal, where the spelling could be
    /// worked out from the elements, and nothing where it could not.
    spots: Option<Vec<u32>>,
    literal: Span,
}

impl Text {
    /// The span of the element at `at`, or of the whole literal where the spelling is unknown.
    fn at(&self, at: usize) -> Span {
        match &self.spots {
            Some(spots) if at < spots.len() => {
                let lo = self.literal.lo + spots[at];
                Span::new(lo, lo + 1)
            }
            _ => self.literal,
        }
    }

    fn get(&self, at: usize) -> Option<char> {
        self.chars.get(at).map(|&unit| char::from_u32(unit).unwrap_or('\u{fffd}'))
    }

    /// The digits from `at`, as the number they spell and where they end.
    fn digits(&self, mut at: usize) -> (Option<usize>, usize) {
        let mut value: Option<usize> = None;
        while let Some(digit) = self.get(at).and_then(|c| c.to_digit(10)) {
            let so_far = value.unwrap_or(0);
            value = Some(so_far.saturating_mul(10).saturating_add(digit as usize));
            at += 1;
        }
        (value, at)
    }

    /// An operand number, `3$`, at `at`, as the number and where it ends.
    fn operand(&self, at: usize) -> Option<(usize, usize)> {
        let (value, end) = self.digits(at);
        let value = value?;
        (self.get(end) == Some('$')).then_some((value, end + 1))
    }
}

/// What the arguments of one call are and how far the format has read into them.
struct Reading<'a> {
    family: Family,
    /// The arguments from the first the format reads, empty for a `va_list` one.
    args: &'a [ExprId],
    /// Whether there are arguments to read at all, which there are not for a `va_list` one.
    checked: bool,
    /// The number of `args[0]` among the call's arguments, counting from one.
    base: usize,
    call: Span,
    next: usize,
    used: Vec<bool>,
    /// Whether the directives so far were written with `$` operand numbers.
    numbered: Option<bool>,
}

/// A directive asked for something that ended the reading of the format, and has said so.
struct Stop;

impl Checker<'_> {
    /// Reads a `format(archetype, string, first)` attribute, for an archetype this checks.
    pub(in crate::check) fn format_attribute(&mut self, attr: Attribute) -> Option<Format> {
        let args = self.ast[attr.args].to_vec();
        let [AttrArg::Ident(archetype), string, first] = args.as_slice() else { return None };
        let name = rucc_gnu::unarmour(self.text(*archetype));
        let windows = self.cx.target.tuple.os().as_str() == "windows";
        let family = match name {
            "gnu_printf" => Family::Printf,
            "gnu_scanf" => Family::Scanf,
            "gnu_strftime" => Family::Strftime,
            "printf" if !windows => Family::Printf,
            "scanf" if !windows => Family::Scanf,
            "strftime" if !windows => Family::Strftime,
            _ => return None,
        };
        let string = self.attribute_number(*string).filter(|&n| n > 0)?;
        let first = self.attribute_number(*first)?;
        Some(Format { family, string, first })
    }

    /// A number an attribute was handed, when it was written as one.
    ///
    /// Only an integer constant is read, because the expression is checked for the reading and
    /// anything more than a constant could say something about itself that the attribute's own
    /// checks have said already.
    pub(in crate::check) fn attribute_number(&mut self, arg: AttrArg) -> Option<usize> {
        let AttrArg::Expr(expr) = arg else { return None };
        if !matches!(self.ast[expr], rucc_ast::Expr::Int(_)) {
            return None;
        }
        let value = self.expr(expr);
        let value = self.eval_integer(value).ok()?;
        usize::try_from(value).ok()
    }

    /// The function a call calls by name, when it does.
    pub(in crate::check) fn called_decl(&self, callee: ExprId) -> Option<DeclId> {
        let mut callee = callee;
        while let ExprKind::Convert { operand, .. } = self.tast[callee].kind {
            callee = operand;
        }
        match self.tast[callee].kind {
            ExprKind::Decl(decl) => Some(decl),
            _ => None,
        }
    }

    /// The name of the C library function a call to `decl` is a call to, without the `__builtin_`
    /// it may have been written with, when the flags let the name mean the library's function.
    pub(in crate::check) fn library_function(&self, decl: DeclId) -> Option<&str> {
        let name = self.text(self.tast[decl].name?);
        if !self.cx.means_the_library(name) {
            return None;
        }
        Some(name.strip_prefix("__builtin_").unwrap_or(name))
    }

    /// Checks the format of a call, once its arguments have been converted and promoted.
    pub(in crate::check) fn heed_format(
        &mut self,
        callee: ExprId,
        params: &[TypeId],
        args: &[ExprId],
        call: Span,
    ) {
        let Some(decl) = self.called_decl(callee) else { return };
        let format = match self.advice.format.get(&decl) {
            Some(&format) => format,
            None => {
                if self.cx.target.tuple.os().as_str() == "windows" {
                    return;
                }
                let Some(name) = self.library_function(decl) else { return };
                let Some(&(_, family, string, first)) = LIBRARY.iter().find(|row| row.0 == name)
                else {
                    return;
                };
                // A program that declared its own function under the name, with something other
                // than a string where the library has one, is not calling the library's.
                let Some(&param) = params.get(string - 1) else { return };
                let param = self.types.canonical(param);
                if !matches!(self.types.kind(param), TypeKind::Pointer(_)) {
                    return;
                }
                Format { family, string, first }
            }
        };
        let Some(&string) = args.get(format.string - 1) else { return };
        let (rest, checked) = match format.first {
            0 => (&[][..], false),
            first => (args.get(first - 1..).unwrap_or(&[]), true),
        };
        self.format_string(string, format, rest, checked, call);
    }

    /// Finds the literal a format argument is, through a conditional and a `format_arg` call.
    fn format_string(
        &mut self,
        string: ExprId,
        format: Format,
        args: &[ExprId],
        checked: bool,
        call: Span,
    ) {
        let mut at = string;
        while let ExprKind::Convert { operand, .. } | ExprKind::Cast(operand) = self.tast[at].kind {
            at = operand;
        }
        match self.tast[at].kind {
            ExprKind::Str(id) => {
                let literal = &self.tast[id];
                let prefix = match literal.encoding {
                    Encoding::Plain => 0,
                    Encoding::Utf8 => 2,
                    _ => return,
                };
                let chars = literal.elements.clone();
                let span = self.tast.expr_span(at);
                let spots = spots(&chars, prefix, span);
                let text = Text { chars, spots, literal: span };
                let mut reading = Reading {
                    family: format.family,
                    args,
                    checked,
                    base: format.first,
                    call,
                    next: 0,
                    used: vec![false; args.len()],
                    numbered: None,
                };
                self.read_format(text, &mut reading);
            }
            ExprKind::Cond { then, otherwise, .. } => {
                self.format_string(then, format, args, checked, call);
                self.format_string(otherwise, format, args, checked, call);
            }
            ExprKind::Call { callee, args: inner } => {
                let Some(decl) = self.called_decl(callee) else { return };
                let Some(&number) = self.advice.format_arg.get(&decl) else { return };
                let inner = self.tast[inner].to_vec();
                if let Some(&arg) = number.checked_sub(1).and_then(|at| inner.get(at)) {
                    self.format_string(arg, format, args, checked, call);
                }
            }
            _ => {}
        }
    }

    /// Reads one literal format, then says what is left over.
    fn read_format(&mut self, mut text: Text, reading: &mut Reading<'_>) {
        if text.chars.is_empty() {
            let what = format!("zero-length {} format string", reading.family.archetype());
            self.report(Diagnostic::warning(what, text.literal).with_code(ZERO_LENGTH));
            return;
        }
        if let Some(nul) = text.chars.iter().position(|&unit| unit == 0) {
            let said = Diagnostic::warning("embedded '\\0' in format", text.at(nul));
            self.report(said.with_code(CONTAINS_NUL));
            text.chars.truncate(nul);
        }
        let mut at = 0;
        while at < text.chars.len() {
            if text.get(at) != Some('%') {
                at += 1;
                continue;
            }
            let read = match reading.family {
                Family::Printf => self.printf_directive(&text, at, reading),
                Family::Scanf => self.scanf_directive(&text, at, reading),
                Family::Strftime => Ok(self.strftime_directive(&text, at)),
            };
            match read {
                Ok(next) => at = next,
                Err(Stop) => return,
            }
        }
        self.left_over(&text, reading);
    }

    /// Says what the format did not read, once it has all been read.
    fn left_over(&mut self, text: &Text, reading: &Reading<'_>) {
        if !reading.checked {
            return;
        }
        if reading.numbered == Some(true) {
            let Some(last) = reading.used.iter().rposition(|&used| used) else { return };
            for unused in (0..last).filter(|&at| !reading.used[at]) {
                let what = format!(
                    "format argument {} unused before used argument {} in '$'-style format",
                    unused + 1,
                    last + 1
                );
                self.report(Diagnostic::warning(what, text.literal).with_code(FORMAT));
            }
            if reading.args.len() > last + 1 {
                let said =
                    Diagnostic::warning("unused arguments in '$'-style format", text.literal);
                self.report(said.with_code(EXTRA_ARGS));
            }
        } else if reading.next < reading.args.len() {
            let said = Diagnostic::warning("too many arguments for format", text.literal);
            self.report(said.with_code(EXTRA_ARGS));
        }
    }

    /// Says that a directive ran into the end of the string, or into another `%` that starts
    /// the next one, which gcc says in the same words.
    fn lacks_type(&mut self, text: &Text, percent: usize) {
        let at = text.at(percent + 1);
        let said = Diagnostic::warning("conversion lacks type at end of format", at);
        self.report(said.with_code(FORMAT));
    }

    /// Says that a format ends in a `%` with nothing after it.
    fn spurious(&mut self, text: &Text, percent: usize) {
        let said = Diagnostic::warning("spurious trailing '%' in format", text.at(percent));
        self.report(said.with_code(FORMAT));
    }

    /// Says that a conversion character is not one the family has.
    fn unknown(&mut self, text: &Text, at: usize) {
        let unit = text.chars[at];
        let shown = match char::from_u32(unit) {
            Some(c) if (' '..='~').contains(&c) => c.to_string(),
            _ => format!("\\x{unit:02x}"),
        };
        let what = format!("unknown conversion type character '{shown}' in format");
        self.report(Diagnostic::warning(what, text.at(at)).with_code(FORMAT));
    }

    /// Settles whether a directive's being numbered or not agrees with the ones before it.
    fn numbering(
        &mut self,
        text: &Text,
        numbered: bool,
        reading: &mut Reading<'_>,
    ) -> Result<(), Stop> {
        match reading.numbered {
            None => reading.numbered = Some(numbered),
            Some(true) if !numbered => {
                let said = Diagnostic::warning("missing $ operand number in format", text.literal);
                self.report(said.with_code(FORMAT));
                return Err(Stop);
            }
            Some(false) if numbered => {
                let what = "'$'operand number used after format without operand number";
                self.report(Diagnostic::warning(what, reading.call).with_code(FORMAT));
                return Err(Stop);
            }
            Some(_) => {}
        }
        Ok(())
    }

    /// The next argument a directive reads, or the numbered one, with its number among the
    /// call's arguments, or nothing where the call has run out of them.
    fn take(
        &mut self,
        number: Option<usize>,
        reading: &mut Reading<'_>,
    ) -> Result<Option<(ExprId, usize)>, Stop> {
        let at = match number {
            Some(number) => {
                if number == 0 || number > reading.args.len() {
                    let said =
                        Diagnostic::warning("operand number out of range in format", reading.call);
                    self.report(said.with_code(FORMAT));
                    return Err(Stop);
                }
                number - 1
            }
            None => {
                reading.next += 1;
                reading.next - 1
            }
        };
        let Some(&arg) = reading.args.get(at) else { return Ok(None) };
        reading.used[at] = true;
        Ok(Some((arg, reading.base + at)))
    }

    /// One `printf` directive, from its `%`, as where the next one may start.
    fn printf_directive(
        &mut self,
        text: &Text,
        percent: usize,
        reading: &mut Reading<'_>,
    ) -> Result<usize, Stop> {
        let mut at = percent + 1;
        let Some(first) = text.get(at) else {
            self.spurious(text, percent);
            return Ok(at);
        };
        if first == '%' {
            return Ok(at + 1);
        }
        let number = text.operand(at).map(|(number, end)| {
            at = end;
            number
        });
        let mut flags: Vec<char> = Vec::new();
        while let Some(flag) = text.get(at).filter(|c| "-+ #0'I".contains(*c)) {
            if flags.contains(&flag) {
                let what = format!("repeated '{flag}' flag in format");
                self.report(Diagnostic::warning(what, text.at(at)).with_code(FORMAT));
            } else {
                flags.push(flag);
            }
            at += 1;
        }
        // A star, with where it is and the number it was given, or digits.
        let mut width: Option<Option<(usize, Option<usize>)>> = None;
        if text.get(at) == Some('*') {
            let star = at;
            at += 1;
            let number = text.operand(at).map(|(number, end)| {
                at = end;
                number
            });
            width = Some(Some((star, number)));
        } else if text.get(at).is_some_and(|c| c.is_ascii_digit()) {
            at = text.digits(at).1;
            width = Some(None);
        }
        let mut precision: Option<Option<(usize, Option<usize>)>> = None;
        if text.get(at) == Some('.') {
            at += 1;
            if text.get(at) == Some('*') {
                let star = at;
                at += 1;
                let number = text.operand(at).map(|(number, end)| {
                    at = end;
                    number
                });
                precision = Some(Some((star, number)));
            } else {
                at = text.digits(at).1;
                precision = Some(None);
            }
        }
        let length_at = at;
        let length = printf_length(text, &mut at);
        let length_text: String = (length_at..at).filter_map(|i| text.get(i)).collect();
        let Some(conversion) = text.get(at) else {
            self.lacks_type(text, percent);
            return Ok(at);
        };
        if conversion == '%' {
            self.lacks_type(text, percent);
            return Ok(at);
        }
        let Some((allowed, class)) = printf_conversion(conversion) else {
            self.unknown(text, at);
            return Ok(at + 1);
        };
        let directive = format!("%{length_text}{conversion}");
        let caret = text.at(at);
        let archetype = reading.family.archetype();
        for &flag in &flags {
            if !allowed.contains(flag) {
                let what = format!("'{flag}' flag used with '{directive}' {archetype} format");
                self.report(Diagnostic::warning(what, caret).with_code(FORMAT));
            }
        }
        if width.is_some() && !allowed.contains('w') {
            let what = format!("field width used with '{directive}' {archetype} format");
            self.report(Diagnostic::warning(what, caret).with_code(FORMAT));
        }
        if precision.is_some() && !allowed.contains('p') {
            let what = format!("precision used with '{directive}' {archetype} format");
            self.report(Diagnostic::warning(what, caret).with_code(FORMAT));
        }
        let has = |flag: char| flags.contains(&flag) && allowed.contains(flag);
        if has(' ') && has('+') {
            let what = format!("' ' flag ignored with '+' flag in {archetype} format");
            self.report(Diagnostic::warning(what, text.literal).with_code(FORMAT));
        }
        if has('0') && has('-') {
            let what = format!("'0' flag ignored with '-' flag in {archetype} format");
            self.report(Diagnostic::warning(what, text.literal).with_code(FORMAT));
        }
        let integer = matches!(class, Class::Signed | Class::Unsigned);
        if has('0') && precision.is_some() && integer {
            let what =
                format!("'0' flag ignored with precision and '{directive}' {archetype} format");
            self.report(Diagnostic::warning(what, text.literal).with_code(FORMAT));
        }
        let want = self.printf_want(class, length);
        if want.is_none() {
            let what = format!(
                "use of '{length_text}' length modifier with '{conversion}' type character has \
                 either no effect or undefined behavior"
            );
            self.report(Diagnostic::warning(what, caret).with_code(FORMAT));
        }
        if class == Class::Errno {
            return Ok(at + 1);
        }
        let stars =
            [(width, "field width specifier '*'"), (precision, "field precision specifier '.*'")];
        let numbered = number.is_some()
            || stars.iter().any(|(star, _)| matches!(star, Some(Some((_, Some(_))))));
        self.numbering(text, numbered, reading)?;
        if !reading.checked {
            return Ok(at + 1);
        }
        for (star, what) in stars {
            let Some(Some((star, number))) = star else { continue };
            if numbered && number.is_none() {
                self.numbering(text, false, reading)?;
            }
            let int =
                Want { base: Base::Rank(rank(IntKind::Int)), depth: 0, name: "int".to_owned() };
            match self.take(number, reading)? {
                None => {
                    let what = format!("{what} expects a matching 'int' argument");
                    self.report(Diagnostic::warning(what, text.at(star)).with_code(FORMAT));
                }
                Some((arg, index)) => {
                    if !self.fits(arg, &int) {
                        let given = self.gcc_quoted(self.tast[arg].ty);
                        let what = format!(
                            "{what} expects argument of type 'int', but argument {index} has type \
                             {given}"
                        );
                        self.report(Diagnostic::warning(what, text.at(star)).with_code(FORMAT));
                    }
                }
            }
        }
        let taken = self.take(number, reading)?;
        let want = want.flatten();
        match (taken, want) {
            (None, Some(want)) => {
                let what =
                    format!("format '{directive}' expects a matching '{}' argument", want.name);
                self.report(Diagnostic::warning(what, caret).with_code(FORMAT));
            }
            (Some((arg, index)), Some(want)) => {
                if class == Class::Count && self.written_badly(arg, index, reading.call) {
                    return Ok(at + 1);
                }
                self.judge(arg, index, &want, &directive, caret);
            }
            _ => {}
        }
        Ok(at + 1)
    }

    /// One `scanf` directive, from its `%`, as where the next one may start.
    fn scanf_directive(
        &mut self,
        text: &Text,
        percent: usize,
        reading: &mut Reading<'_>,
    ) -> Result<usize, Stop> {
        let mut at = percent + 1;
        let Some(first) = text.get(at) else {
            self.spurious(text, percent);
            return Ok(at);
        };
        if first == '%' {
            return Ok(at + 1);
        }
        let number = text.operand(at).map(|(number, end)| {
            at = end;
            number
        });
        let suppressed = text.get(at) == Some('*');
        if suppressed {
            at += 1;
        }
        at = text.digits(at).1;
        let allocated = text.get(at) == Some('m');
        if allocated {
            at += 1;
        }
        let length_at = at;
        let length = printf_length(text, &mut at);
        let length_text: String = (length_at..at).filter_map(|i| text.get(i)).collect();
        let Some(conversion) = text.get(at) else {
            self.lacks_type(text, percent);
            return Ok(at);
        };
        if conversion == '%' {
            self.lacks_type(text, percent);
            return Ok(at);
        }
        let Some(class) = scanf_conversion(conversion) else {
            self.unknown(text, at);
            return Ok(at + 1);
        };
        let allocation = if allocated { "m" } else { "" };
        let mut directive = format!("%{allocation}{length_text}{conversion}");
        let mut caret = text.at(at);
        let mut end = at + 1;
        if conversion == '[' {
            // The set runs to the first `]` after the one place a `]` belongs to the set, which
            // is first, or first after a `^`.
            let mut scan = at + 1;
            if text.get(scan) == Some('^') {
                scan += 1;
            }
            if text.get(scan) == Some(']') {
                scan += 1;
            }
            while text.get(scan).is_some_and(|c| c != ']') {
                scan += 1;
            }
            if text.get(scan).is_none() {
                let last = text.chars.len() - 1;
                let said = Diagnostic::warning("no closing ']' for '%[' format", text.at(last));
                self.report(said.with_code(FORMAT));
            }
            directive.extend((at + 1..scan).filter_map(|i| text.get(i)));
            caret = text.at(scan - 1);
            end = (scan + 1).min(text.chars.len());
        }
        let want = self.scanf_want(class, length, allocated);
        if want.is_none() {
            let what = format!(
                "use of '{length_text}' length modifier with '{conversion}' type character has \
                 either no effect or undefined behavior"
            );
            self.report(Diagnostic::warning(what, caret).with_code(FORMAT));
        }
        if suppressed {
            return Ok(end);
        }
        self.numbering(text, number.is_some(), reading)?;
        if !reading.checked {
            return Ok(end);
        }
        let taken = self.take(number, reading)?;
        let want = want.flatten();
        match (taken, want) {
            (None, Some(want)) => {
                let what =
                    format!("format '{directive}' expects a matching '{}' argument", want.name);
                self.report(Diagnostic::warning(what, caret).with_code(FORMAT));
            }
            (Some((arg, index)), Some(want)) if !self.written_badly(arg, index, reading.call) => {
                self.judge(arg, index, &want, &directive, caret);
            }
            _ => {}
        }
        Ok(end)
    }

    /// One `strftime` directive, from its `%`, as where the next one may start.
    fn strftime_directive(&mut self, text: &Text, percent: usize) -> usize {
        let mut at = percent + 1;
        if text.get(at).is_none() {
            self.spurious(text, percent);
            return at;
        }
        while text.get(at).is_some_and(|c| "_-0^#".contains(c)) {
            at += 1;
        }
        at = text.digits(at).1;
        if matches!(text.get(at), Some('E' | 'O')) {
            at += 1;
        }
        match text.get(at) {
            None => self.lacks_type(text, percent),
            Some(c) if "ABZabcxHIMSUWdmwjpXyY%CDeVuFRTntrgGhzklsP".contains(c) => {}
            Some(_) => self.unknown(text, at),
        }
        at + 1
    }

    /// Says that a conversion that writes was handed something it may not write through, and
    /// whether it said so, which is the end of what is said about that argument.
    fn written_badly(&mut self, arg: ExprId, index: usize, call: Span) -> bool {
        let ty = self.types.canonical(self.tast[arg].ty);
        let TypeKind::Pointer(pointee) = self.types.kind(ty) else { return false };
        if self.conv().is_null_pointer_constant(arg) {
            let what = format!("writing through null pointer (argument {index})");
            self.report(Diagnostic::warning(what, call).with_code(FORMAT));
            return true;
        }
        if self.types.object_quals(pointee).has(Qualifiers::CONST) {
            let what = format!("writing into constant object (argument {index})");
            self.report(Diagnostic::warning(what, call).with_code(FORMAT));
            return true;
        }
        false
    }

    /// Says that an argument is not the type its directive reads, where it is not.
    fn judge(&mut self, arg: ExprId, index: usize, want: &Want, directive: &str, caret: Span) {
        if self.is_poisoned(arg) || self.fits(arg, want) {
            return;
        }
        let given = self.gcc_quoted(self.tast[arg].ty);
        let what = format!(
            "format '{directive}' expects argument of type '{}', but argument {index} has type \
             {given}",
            want.name
        );
        self.report(Diagnostic::warning(what, caret).with_code(FORMAT));
    }

    /// Whether an argument is what a directive reads.
    fn fits(&self, arg: ExprId, want: &Want) -> bool {
        let mut ty = self.types.canonical(self.tast[arg].ty);
        for _ in 0..want.depth {
            let TypeKind::Pointer(pointee) = self.types.kind(ty) else { return false };
            ty = self.types.canonical(pointee);
        }
        match want.base {
            Base::Anything => true,
            Base::Rank(wanted) => self.rank_of(ty) == Some(wanted),
            Base::Float(kind) => self.types.kind(ty) == TypeKind::Float(kind),
            Base::AnyChar => self.rank_of(ty) == Some(rank(IntKind::Char)),
            Base::Wide => match self.types.kind(ty) {
                TypeKind::Int(kind) => {
                    rank(kind) > rank(IntKind::Char)
                        && int_width(kind, self.cx.target) == self.cx.target.wchar_width
                }
                _ => false,
            },
        }
    }

    /// The rank of an integer type, an enumeration's being its underlying type's.
    fn rank_of(&self, ty: TypeId) -> Option<u8> {
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Int(kind) => Some(rank(kind)),
            TypeKind::Enum(id) => {
                self.types.enum_info(id).underlying.and_then(|under| self.rank_of(under))
            }
            _ => None,
        }
    }

    /// What a `printf` conversion reads with a length modifier, nothing for a pair that has
    /// either no effect or undefined behaviour, and a want of nothing for one whose type is not
    /// read here.
    fn printf_want(&self, class: Class, length: Length) -> Option<Option<Want>> {
        let int = |name: &str, kind: IntKind| Some(Some(scalar(Base::Rank(rank(kind)), name)));
        let ptr =
            |base: Base, name: &str| Some(Some(Want { base, depth: 1, name: format!("{name} *") }));
        match (class, length) {
            (_, Length::W) if matches!(class, Class::Signed | Class::Unsigned | Class::Count) => {
                Some(None)
            }
            (Class::Floating, Length::Decimal) => Some(None),
            (Class::Signed | Class::Unsigned, Length::None | Length::Hh | Length::H) => {
                let name = match (class, length) {
                    (Class::Unsigned, Length::None) => "unsigned int",
                    _ => "int",
                };
                int(name, IntKind::Int)
            }
            (Class::Signed | Class::Unsigned, _) => {
                let (signed, unsigned, kind) = self.wide_names(length)?;
                int(if class == Class::Signed { signed } else { unsigned }, kind)
            }
            (Class::Count, Length::BigL) => None,
            (Class::Count, _) => {
                let (name, kind) = self.count_name(length)?;
                ptr(Base::Rank(rank(kind)), name)
            }
            (Class::Floating, Length::None | Length::L) => {
                Some(Some(scalar(Base::Float(FloatKind::Double), "double")))
            }
            (Class::Floating, Length::BigL) => {
                Some(Some(scalar(Base::Float(FloatKind::LongDouble), "long double")))
            }
            (Class::Char, Length::None) => int("int", IntKind::Int),
            (Class::Char, Length::L) | (Class::WideChar, Length::None) => {
                int("wint_t", IntKind::Int)
            }
            (Class::String, Length::None) => ptr(Base::AnyChar, "char"),
            (Class::String, Length::L) | (Class::WideString, Length::None) => {
                ptr(Base::Wide, "wchar_t")
            }
            (Class::Pointer, Length::None) => ptr(Base::Anything, "void"),
            (Class::Errno, Length::None) => Some(None),
            _ => None,
        }
    }

    /// What a `scanf` conversion writes through, the same way [`Self::printf_want`] answers.
    fn scanf_want(&self, class: Class, length: Length, allocated: bool) -> Option<Option<Want>> {
        let ptr =
            |base: Base, name: &str| Some(Some(Want { base, depth: 1, name: format!("{name} *") }));
        let deeper = |base: Base, name: &str| {
            Some(Some(Want { base, depth: 2, name: format!("{name} **") }))
        };
        match (class, length) {
            (_, Length::W) if matches!(class, Class::Signed | Class::Unsigned | Class::Count) => {
                Some(None)
            }
            (Class::Floating, Length::Decimal) => Some(None),
            (Class::Signed | Class::Count, Length::None) => {
                ptr(Base::Rank(rank(IntKind::Int)), "int")
            }
            (Class::Unsigned, Length::None) => ptr(Base::Rank(rank(IntKind::Int)), "unsigned int"),
            (Class::Signed | Class::Count, Length::Hh) => ptr(Base::AnyChar, "signed char"),
            (Class::Unsigned, Length::Hh) => ptr(Base::AnyChar, "unsigned char"),
            (Class::Signed | Class::Count, Length::H) => {
                ptr(Base::Rank(rank(IntKind::Short)), "short int")
            }
            (Class::Unsigned, Length::H) => {
                ptr(Base::Rank(rank(IntKind::Short)), "short unsigned int")
            }
            (Class::Count, Length::BigL) => None,
            (Class::Signed | Class::Count, _) => {
                let (name, _, kind) = self.wide_names(length)?;
                ptr(Base::Rank(rank(kind)), name)
            }
            (Class::Unsigned, _) => {
                let (_, name, kind) = self.wide_names(length)?;
                ptr(Base::Rank(rank(kind)), name)
            }
            (Class::Floating, Length::None) => ptr(Base::Float(FloatKind::Float), "float"),
            (Class::Floating, Length::L) => ptr(Base::Float(FloatKind::Double), "double"),
            (Class::Floating, Length::BigL) => {
                ptr(Base::Float(FloatKind::LongDouble), "long double")
            }
            (Class::String, Length::None) if allocated => deeper(Base::AnyChar, "char"),
            (Class::String, Length::None) => ptr(Base::AnyChar, "char"),
            (Class::String, Length::L) | (Class::WideString, Length::None) if allocated => {
                deeper(Base::Wide, "wchar_t")
            }
            (Class::String, Length::L) | (Class::WideString, Length::None) => {
                ptr(Base::Wide, "wchar_t")
            }
            (Class::Pointer, Length::None) => deeper(Base::Anything, "void"),
            _ => None,
        }
    }

    /// The signed and unsigned names of an integer length wider than `int`, and the kind of the
    /// signed one on this target.
    fn wide_names(&self, length: Length) -> Option<(&'static str, &'static str, IntKind)> {
        Some(match length {
            Length::L => ("long int", "long unsigned int", IntKind::Long),
            Length::Ll | Length::BigL => {
                ("long long int", "long long unsigned int", IntKind::LongLong)
            }
            Length::J => ("intmax_t", "uintmax_t", self.widest_integer(true)),
            Length::Z => ("signed size_t", "size_t", self.int_kind(self.size_type())),
            Length::T => ("ptrdiff_t", "unsigned ptrdiff_t", self.int_kind(self.ptrdiff())),
            _ => return None,
        })
    }

    /// What `%n` writes through with a length modifier, and the kind it is on this target.
    fn count_name(&self, length: Length) -> Option<(&'static str, IntKind)> {
        Some(match length {
            Length::None => ("int", IntKind::Int),
            Length::Hh => ("signed char", IntKind::SChar),
            Length::H => ("short int", IntKind::Short),
            Length::Z => ("signed size_t", self.int_kind(self.size_type())),
            _ => {
                let (signed, _, kind) = self.wide_names(length)?;
                (signed, kind)
            }
        })
    }

    /// The integer kind of an integer type.
    fn int_kind(&self, ty: TypeId) -> IntKind {
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Int(kind) => kind,
            _ => IntKind::Long,
        }
    }

    /// A type the way gcc's format messages quote it, with what a typedef stands for after it.
    pub(in crate::check) fn gcc_quoted(&self, ty: TypeId) -> String {
        let written = self.gcc_type(ty, false);
        let plain = self.gcc_type(ty, true);
        if written == plain {
            format!("'{written}'")
        } else {
            format!("'{written}' {{aka '{plain}'}}")
        }
    }

    /// A type in gcc's words, `long unsigned int` and `char *`, through its typedefs or not.
    ///
    /// The integer types and the pointers to them are where gcc's words are not this compiler's,
    /// and everything else is spelled the way every other message here spells it.
    fn gcc_type(&self, ty: TypeId, plain: bool) -> String {
        let quals = self.types.quals(ty);
        let mut prefix = String::new();
        if quals.has(Qualifiers::CONST) {
            prefix.push_str("const ");
        }
        if quals.has(Qualifiers::VOLATILE) {
            prefix.push_str("volatile ");
        }
        let fallback = || {
            let shown = if plain { self.types.canonical(ty) } else { ty };
            self.spell(shown)
        };
        let named = match self.types.kind(ty) {
            TypeKind::Typedef { name, underlying, .. } => {
                if plain {
                    return format!("{prefix}{}", self.gcc_type(underlying, true));
                }
                self.text(name).to_owned()
            }
            TypeKind::Int(kind) => match int_name(kind) {
                Some(name) => name.to_owned(),
                None => return fallback(),
            },
            TypeKind::Float(FloatKind::Float) => "float".to_owned(),
            TypeKind::Float(FloatKind::Double) => "double".to_owned(),
            TypeKind::Float(FloatKind::LongDouble) => "long double".to_owned(),
            TypeKind::Void => "void".to_owned(),
            TypeKind::Pointer(pointee) => {
                let target = self.types.canonical(pointee);
                if matches!(self.types.kind(target), TypeKind::Function(_) | TypeKind::Array { .. })
                {
                    return fallback();
                }
                let inner = self.gcc_type(pointee, plain);
                let star =
                    if inner.ends_with('*') { format!("{inner}*") } else { format!("{inner} *") };
                let mut after = String::new();
                if quals.has(Qualifiers::CONST) {
                    after.push_str(" const");
                }
                return format!("{star}{after}");
            }
            _ => return fallback(),
        };
        format!("{prefix}{named}")
    }
}

/// A want of a value rather than of a pointer.
fn scalar(base: Base, name: &str) -> Want {
    Want { base, depth: 0, name: name.to_owned() }
}

/// The length modifier at `at`, moving `at` past it.
fn printf_length(text: &Text, at: &mut usize) -> Length {
    let next = |offset: usize| text.get(*at + offset);
    let (length, width) = match (next(0), next(1)) {
        (Some('h'), Some('h')) => (Length::Hh, 2),
        (Some('h'), _) => (Length::H, 1),
        (Some('l'), Some('l')) => (Length::Ll, 2),
        (Some('l'), _) => (Length::L, 1),
        (Some('q'), _) => (Length::Ll, 1),
        (Some('L'), _) => (Length::BigL, 1),
        (Some('j'), _) => (Length::J, 1),
        (Some('z' | 'Z'), _) => (Length::Z, 1),
        (Some('t'), _) => (Length::T, 1),
        (Some('H'), _) => (Length::Decimal, 1),
        (Some('D'), Some('D')) => (Length::Decimal, 2),
        (Some('D'), _) => (Length::Decimal, 1),
        (Some('w'), _) => {
            let mut end = *at + 1;
            if text.get(end) == Some('f') {
                end += 1;
            }
            let (_, end) = text.digits(end);
            *at = end;
            return Length::W;
        }
        _ => (Length::None, 0),
    };
    *at += width;
    length
}

/// The flags a `printf` conversion takes, with `w` and `p` for a width and a precision, and what
/// it reads. gcc 16's table.
fn printf_conversion(c: char) -> Option<(&'static str, Class)> {
    Some(match c {
        'd' | 'i' => ("-wp0 +'I", Class::Signed),
        'o' | 'x' | 'X' | 'b' | 'B' => ("-wp0#", Class::Unsigned),
        'u' => ("-wp0'I", Class::Unsigned),
        'f' | 'F' | 'g' | 'G' => ("-wp0 +#'I", Class::Floating),
        'e' | 'E' => ("-wp0 +#I", Class::Floating),
        'a' | 'A' => ("-wp0 +#", Class::Floating),
        'c' => ("-w", Class::Char),
        'C' => ("-w", Class::WideChar),
        's' => ("-wp", Class::String),
        'S' => ("-wp", Class::WideString),
        'p' => ("-w", Class::Pointer),
        'n' => ("", Class::Count),
        'm' => ("-wp", Class::Errno),
        _ => return None,
    })
}

/// What a `scanf` conversion writes.
fn scanf_conversion(c: char) -> Option<Class> {
    Some(match c {
        'd' | 'i' => Class::Signed,
        'o' | 'u' | 'x' | 'X' | 'b' => Class::Unsigned,
        'n' => Class::Count,
        'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'a' | 'A' => Class::Floating,
        'c' | 's' | '[' => Class::String,
        'C' | 'S' => Class::WideString,
        'p' => Class::Pointer,
        _ => return None,
    })
}

/// Where an integer kind sits among the others, with the two signednesses of one rank together,
/// since the format checks do not judge signedness.
fn rank(kind: IntKind) -> u8 {
    match kind {
        IntKind::Char | IntKind::SChar | IntKind::UChar => 0,
        IntKind::Short | IntKind::UShort => 1,
        IntKind::Int | IntKind::UInt => 2,
        IntKind::Long | IntKind::ULong => 3,
        IntKind::LongLong | IntKind::ULongLong => 4,
        IntKind::Int128 | IntKind::UInt128 => 5,
    }
}

/// gcc's name for an integer kind.
fn int_name(kind: IntKind) -> Option<&'static str> {
    Some(match kind {
        IntKind::Char => "char",
        IntKind::SChar => "signed char",
        IntKind::UChar => "unsigned char",
        IntKind::Short => "short int",
        IntKind::UShort => "short unsigned int",
        IntKind::Int => "int",
        IntKind::UInt => "unsigned int",
        IntKind::Long => "long int",
        IntKind::ULong => "long unsigned int",
        IntKind::LongLong => "long long int",
        IntKind::ULongLong => "long long unsigned int",
        _ => return None,
    })
}

/// The offset in the source of each element of a literal, when the literal is spelled the one
/// way its elements would be written back.
///
/// The source is not to hand here, so the spelling is worked out: each element is one character
/// or a two character escape, and if that comes to the length of the span the offsets are right.
/// A literal written another way, with an octal escape or across several pieces, comes to some
/// other length, and the caret goes at the start of the literal instead, which is where gcc puts
/// it for anything it cannot place inside.
fn spots(chars: &[u32], prefix: u32, span: Span) -> Option<Vec<u32>> {
    let mut spots = Vec::with_capacity(chars.len());
    let mut at = prefix + 1;
    for &unit in chars {
        spots.push(at);
        at += match unit {
            0 | 0x07..=0x0d | 0x22 | 0x5c => 2,
            0x20..=0x7e => 1,
            _ => return None,
        };
    }
    (at + 1 == span.hi.checked_sub(span.lo)?).then_some(spots)
}
