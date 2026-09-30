//! Macros, repetitions and conditionals, the part of gas that rewrites the text before any of it
//! is read as a directive or an instruction.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1, and section 7.2.1 of the kernel plan in
//! `tamnd/rucc-kernel`, which is where the list of what is here comes from.
//!
//! # How it sits in front of the reader
//!
//! Every statement of the file goes through [`Reader::feed`] before [`Reader::statement`] sees it.
//! Most of them pass straight through. A `.macro` or a `.rept` starts collecting the statements
//! after it rather than reading them, a conditional that came out false has the statements up to
//! its `.else` or `.endif` dropped, and the name of a macro is replaced by its body with the
//! arguments written in, which is fed back through here one statement at a time so that a macro
//! may call another, define another or open a conditional of its own.
//!
//! It is a layer of the reader rather than a pass over the text before it for one reason:
//! conditionals are decided where they are written, and what they ask about is what the file has
//! said so far. The kernel's `extable_type_reg` counts with `.set` inside an `.irp` and tests the
//! count with `.if` at the end, so the reader has to have read the `.set` lines by the time the
//! `.if` is reached. A separate pass would need a second reader of its own to know that.
//!
//! Inline assembly comes through here as well. A unit whose templates have to be read together is
//! read from its listing, where the `asm` at file scope comes first and every template after it in
//! order, so a macro one of them defines is there for the ones after it, as it is for gas.
//!
//! # What is and is not the same as gas
//!
//! Arguments are split the way gas splits them: at a comma always, and at a blank outside brackets
//! unless the blank sits beside an operator, so `m 1 + 2` is one argument and `m %eax %ebx` is two.
//! gas gets the second half of that from its input scrubber, which squeezes the blanks out around
//! operators before a macro is looked at, and this asks the same question where it splits.
//!
//! `.altmacro` is read, with `<...>` and `%expr` in arguments, parameters named in the body without
//! a backslash, `&` to join one to what follows and `LOCAL`. Nothing in the 7.2 kernel turns it on,
//! so it is here for the programs that do and is not what the tests lean on.

use super::{Reader, Trouble, labelled, split, starts};
use rucc_base::hash::Map;
use rucc_object::Held;

/// How deep one macro may call another before the file is refused.
///
/// A hundred levels, which is far past the twenty or so the kernel reaches. What it is for is a
/// macro that calls itself and never stops, which is refused here rather than left to run the
/// compiler out of stack.
const DEEPEST: usize = 100;

/// How many sets a name may be looked through to find its value, which stops two sets that name
/// each other from being followed round for ever.
const THROUGH: usize = 64;

/// What the macro layer knows while a file is being read.
#[derive(Debug, Default)]
pub(super) struct Macros {
    /// Every macro defined so far, by its name in lower case, since gas looks a macro up without
    /// regard to case.
    defined: Map<String, Macro>,
    /// The `.macro` or repetition whose body is being collected, if there is one.
    collecting: Option<Collecting>,
    /// The conditionals that are open, the innermost last.
    conds: Vec<Cond>,
    /// How many macros have been expanded so far, which is what `\@` is written as.
    expanded: usize,
    /// How many expansions are running inside each other at this moment.
    depth: usize,
    /// Whether `.exitm` has been read and the rest of the innermost expansion is to be skipped.
    exiting: bool,
    /// Whether `.altmacro` is in force.
    alternate: bool,
    /// How many names `LOCAL` has made up so far, which is what keeps each one apart.
    locals: usize,
}

/// One macro, as `.macro` defined it.
#[derive(Debug, Clone)]
struct Macro {
    /// The name as the file spelled it, for a message.
    name: String,
    params: Vec<Param>,
    /// The statements between `.macro` and `.endm`, not yet filled in.
    body: Vec<String>,
}

/// One parameter of a macro.
#[derive(Debug, Clone, Default)]
struct Param {
    name: String,
    /// What it is when the call gives nothing for it, which is empty unless `=` said otherwise.
    default: String,
    /// `:req`, which makes a call that gives nothing for it a mistake.
    required: bool,
    /// `:vararg`, which takes the rest of the call as it was written, commas and all.
    vararg: bool,
}

/// A body being collected, and what it will be once its end is reached.
#[derive(Debug)]
struct Collecting {
    what: Body,
    body: Vec<String>,
    /// How many of the same kind have been opened inside it and not yet closed, which is what
    /// lets a macro define another one or a `.rept` hold another.
    nest: usize,
    /// Where it started, for a file that never closes it.
    line: usize,
}

/// What a collected body is for.
#[derive(Debug)]
enum Body {
    /// `.macro`, to be kept under its name.
    Macro { name: String, params: Vec<Param> },
    /// `.rept`, to be read that many times.
    Rept(u64),
    /// `.irp` and `.irpc`, to be read once for each value with the name standing for it.
    Irp { name: String, values: Vec<String> },
}

impl Body {
    /// Whether a directive opens another body of this kind, which the matching end then closes.
    fn opens(&self, word: &str) -> bool {
        match self {
            Body::Macro { .. } => word == ".macro",
            _ => matches!(word, ".rept" | ".irp" | ".irpc"),
        }
    }

    /// The directive that ends a body of this kind.
    fn end(&self) -> &'static str {
        match self {
            Body::Macro { .. } => ".endm",
            _ => ".endr",
        }
    }
}

/// One open conditional.
#[derive(Debug, Clone, Copy)]
struct Cond {
    /// Whether the text around it is being read at all. When it is not, nothing inside is either,
    /// whatever the condition says, and the condition is not even worked out.
    outer: bool,
    /// Whether one of its branches has been taken already, after which every other is skipped.
    taken: bool,
    /// Whether the branch it is in now is being read.
    now: bool,
    /// Whether `.else` has been read, after which another `.else` or `.elseif` is a mistake.
    otherwise: bool,
}

impl Macros {
    /// Whether the statements here are being read, which is whether every open conditional is in
    /// a branch that was taken.
    fn active(&self) -> bool {
        self.conds.last().is_none_or(|cond| cond.now)
    }
}

/// A statement taken apart: the labels in front of it, the first word after them and the rest.
struct Parts<'a> {
    labels: Vec<String>,
    word: &'a str,
    rest: &'a str,
}

/// The labels, the first word and the rest of one statement.
fn parts(text: &str) -> Parts<'_> {
    let mut labels = Vec::new();
    let mut text = text.trim_start();
    while let Some((name, length)) = labelled(text) {
        text = text[length..].trim_start();
        labels.push(name);
    }
    let (word, rest) = match text.find(char::is_whitespace) {
        Some(cut) => (&text[..cut], text[cut..].trim()),
        None => (text, ""),
    };
    Parts { labels, word, rest }
}

impl Reader {
    /// One line of the file, or of an expansion, with its comments already gone.
    pub(super) fn feed(&mut self, line: &str) -> Result<(), Trouble> {
        for statement in split(line, ';') {
            if self.macros.exiting {
                break;
            }
            self.take(statement.trim())?;
        }
        Ok(())
    }

    /// What is still open at the end of the file, which is a mistake gas reports as well.
    pub(super) fn finish_macros(&mut self) -> Result<(), Trouble> {
        if let Some(collecting) = &self.macros.collecting {
            let why = format!("'{}' is never reached", collecting.what.end());
            return Err(Trouble { line: collecting.line, why });
        }
        if !self.macros.conds.is_empty() {
            return Err(self.bad("a conditional is still open at the end of the file"));
        }
        Ok(())
    }

    /// One statement.
    fn take(&mut self, text: &str) -> Result<(), Trouble> {
        if text.is_empty() {
            return Ok(());
        }
        let Parts { labels, word, rest } = parts(text);
        if let Some(collecting) = &mut self.macros.collecting {
            if collecting.what.opens(word) {
                collecting.nest += 1;
            } else if word == collecting.what.end() {
                if collecting.nest == 0 {
                    let done = self.macros.collecting.take().expect("checked above");
                    return self.collected(done);
                }
                collecting.nest -= 1;
            }
            collecting.body.push(text.to_owned());
            return Ok(());
        }
        if self.conditional(word, rest)? {
            return Ok(());
        }
        if !self.macros.active() {
            return Ok(());
        }
        let control = matches!(
            word,
            ".macro"
                | ".endm"
                | ".exitm"
                | ".purgem"
                | ".rept"
                | ".irp"
                | ".irpc"
                | ".endr"
                | ".altmacro"
                | ".noaltmacro"
                | ".print"
                | ".abort"
        );
        let called = if control || word.is_empty() || rest.starts_with('=') {
            None
        } else {
            self.macros.defined.get(&word.to_ascii_lowercase()).cloned()
        };
        // gas ends the name of a macro at a bracket, so `NAME(arg)` calls it with `(arg)`. The
        // kernel's entry_64.S says `STACK_FRAME_NON_STANDARD(clear_bhb_loop)` that way.
        if let (None, Some(bracket)) = (&called, word.find('(')) {
            let known = bracket > 0 && !control;
            if known && self.macros.defined.contains_key(&word[..bracket].to_ascii_lowercase()) {
                let at = word.as_ptr() as usize - text.as_ptr() as usize + bracket;
                return self.take(&format!("{} {}", &text[..at], &text[at..]));
            }
        }
        if !control && called.is_none() {
            return self.statement(text);
        }
        for label in &labels {
            self.label(label)?;
        }
        if let Some(called) = called {
            return self.expand(&called, rest);
        }
        match word {
            ".macro" => {
                let (name, params) = self.definition(rest)?;
                self.open(Body::Macro { name, params });
            }
            ".endm" => return Err(self.bad("'.endm' with no '.macro' open")),
            ".endr" => return Err(self.bad("'.endr' with no '.rept' or '.irp' open")),
            ".exitm" => {
                if self.macros.depth == 0 {
                    return Err(self.bad("'.exitm' outside a macro"));
                }
                self.macros.exiting = true;
            }
            ".purgem" => {
                // gas only warns about purging a macro that was never defined, and nothing here
                // has anywhere to print a warning to.
                for name in split(rest, ',') {
                    self.macros.defined.remove(&name.to_ascii_lowercase());
                }
            }
            ".rept" => {
                let count = self.value_now(rest)?;
                // A negative count is gas's zero, with a warning.
                self.open(Body::Rept(u64::try_from(count).unwrap_or(0)));
            }
            ".irp" | ".irpc" => {
                let (name, list) = match rest.find([',', ' ', '\t']) {
                    Some(cut) => (rest[..cut].trim(), rest[cut + 1..].trim_start()),
                    None => (rest, ""),
                };
                let list = list.strip_prefix(',').unwrap_or(list).trim();
                if name.is_empty() {
                    return Err(self.bad(&format!("'{word}' with no name to stand for the values")));
                }
                let values = if word == ".irp" {
                    self.arguments(list)?.into_iter().map(|(_, value)| value).collect()
                } else {
                    let list = self.arguments(list)?.into_iter().next().unwrap_or_default().1;
                    list.chars().map(String::from).collect()
                };
                self.open(Body::Irp { name: name.to_owned(), values });
            }
            ".altmacro" => self.macros.alternate = true,
            ".noaltmacro" => self.macros.alternate = false,
            // What gas prints on standard output while it reads, which a compiler has nowhere to
            // put, so it is passed over.
            ".print" => {}
            ".abort" => return Err(self.bad("the file stops itself with '.abort'")),
            _ => unreachable!("every control directive is matched above"),
        }
        Ok(())
    }

    /// Start collecting a body.
    fn open(&mut self, what: Body) {
        self.macros.collecting =
            Some(Collecting { what, body: Vec::new(), nest: 0, line: self.line });
    }

    /// A body whose end has been reached, kept or read as its kind says.
    fn collected(&mut self, done: Collecting) -> Result<(), Trouble> {
        match done.what {
            Body::Macro { name, params } => {
                let key = name.to_ascii_lowercase();
                if self.macros.defined.contains_key(&key) {
                    return Err(self.bad(&format!("macro '{name}' is already defined")));
                }
                self.macros.defined.insert(key, Macro { name, params, body: done.body });
            }
            Body::Rept(count) => {
                for _ in 0..count {
                    for line in &done.body {
                        self.feed(line)?;
                        if self.macros.exiting {
                            return Ok(());
                        }
                    }
                }
            }
            Body::Irp { name, values } => {
                // No values at all is one pass with the name standing for nothing, as in gas.
                let values = if values.is_empty() { vec![String::new()] } else { values };
                for value in values {
                    let names = [(name.clone(), value)];
                    for line in &done.body {
                        let line = self.substitute(line, &names, None);
                        self.feed(&line)?;
                        if self.macros.exiting {
                            return Ok(());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// `.macro name params`, as the name and the parameters.
    ///
    /// Each parameter is a name, then `:req` or `:vararg` if it is one of those, then `=` and what
    /// it is when a call leaves it out. They are separated by commas or by blanks, the way the
    /// arguments of a call are.
    fn definition(&mut self, rest: &str) -> Result<(String, Vec<Param>), Trouble> {
        let end = rest.find([',', ' ', '\t']).unwrap_or(rest.len());
        let name = rest[..end].trim();
        if name.is_empty() {
            return Err(self.bad("'.macro' with no name"));
        }
        let mut text = rest[end..].trim_start();
        text = text.strip_prefix(',').unwrap_or(text).trim_start();
        let mut params: Vec<Param> = Vec::new();
        while !text.is_empty() {
            let end = text
                .find(|ch: char| ch == ':' || ch == '=' || ch == ',' || ch.is_whitespace())
                .unwrap_or(text.len());
            let mut param = Param { name: text[..end].to_owned(), ..Param::default() };
            if param.name.is_empty() {
                return Err(self.bad(&format!("'{text}' is not a parameter of a macro")));
            }
            text = &text[end..];
            if let Some(after) = text.strip_prefix(':') {
                let end = after
                    .find(|ch: char| ch == '=' || ch == ',' || ch.is_whitespace())
                    .unwrap_or(after.len());
                match &after[..end] {
                    "req" => param.required = true,
                    "vararg" => param.vararg = true,
                    other => {
                        let why = format!("':{other}' is not a kind of parameter gas knows");
                        return Err(self.bad(&why));
                    }
                }
                text = &after[end..];
            }
            let blank = text.trim_start();
            if let Some(after) = blank.strip_prefix('=').filter(|after| !after.starts_with('=')) {
                let (value, after) = self.argument(after.trim_start())?;
                param.default = value;
                text = after;
            }
            text = text.trim_start();
            text = text.strip_prefix(',').unwrap_or(text).trim_start();
            if params.iter().any(|other| other.name == param.name) {
                let why = format!("'{}' is a parameter of '{name}' twice", param.name);
                return Err(self.bad(&why));
            }
            params.push(param);
        }
        Ok((name.to_owned(), params))
    }

    /// One call of a macro, which is its body with the arguments written in, read as though it
    /// had been written where the call is.
    fn expand(&mut self, called: &Macro, rest: &str) -> Result<(), Trouble> {
        if self.macros.depth >= DEEPEST {
            let why = format!("'{}' calls macros more than {DEEPEST} deep", called.name);
            return Err(self.bad(&why));
        }
        let mut names = self.bound(called, rest)?;
        let number = self.macros.expanded;
        self.macros.expanded += 1;
        let conds = self.macros.conds.len();
        let alternate = self.macros.alternate;
        self.macros.depth += 1;
        let mut result = Ok(());
        for line in &called.body {
            // `LOCAL a, b` under `.altmacro`, which gives each name a label of its own for this
            // call and is not itself a statement.
            if alternate {
                let Parts { word, rest, .. } = parts(line);
                if word.eq_ignore_ascii_case("local") {
                    for local in split(rest, ',') {
                        self.macros.locals += 1;
                        names.push((local, format!(".LL{:04x}", self.macros.locals)));
                    }
                    continue;
                }
            }
            let line = self.substitute(line, &names, Some(number));
            result = self.feed(&line);
            if result.is_err() || self.macros.exiting {
                break;
            }
        }
        self.macros.depth -= 1;
        if self.macros.exiting {
            // Whatever the macro opened and `.exitm` left open is closed with it, as in gas.
            self.macros.exiting = false;
            self.macros.conds.truncate(conds);
        }
        result.map_err(|trouble| Trouble {
            line: trouble.line,
            why: format!("{}, in the expansion of '{}'", trouble.why, called.name),
        })
    }

    /// What each parameter of a macro stands for in one call of it.
    fn bound(&mut self, called: &Macro, rest: &str) -> Result<Vec<(String, String)>, Trouble> {
        let mut values: Vec<Option<String>> = vec![None; called.params.len()];
        let mut next = 0;
        let mut text = rest.trim();
        while !text.is_empty() {
            if let Some((at, after)) = keyword(text, &called.params) {
                let (value, after) = self.argument(after)?;
                values[at] = Some(value);
                text = after;
            } else {
                let Some(param) = called.params.get(next) else {
                    let why = format!("'{}' is given more arguments than it has", called.name);
                    return Err(self.bad(&why));
                };
                if param.vararg {
                    values[next] = Some(text.to_owned());
                    break;
                }
                let (value, after) = self.argument(text)?;
                values[next] = Some(value);
                next += 1;
                text = after;
            }
            text = text.trim_start();
            text = text.strip_prefix(',').unwrap_or(text).trim_start();
        }
        let mut names = Vec::with_capacity(values.len());
        for (param, value) in called.params.iter().zip(values) {
            let value = value.filter(|value| !value.is_empty());
            if value.is_none() && param.required {
                let why = format!(
                    "'{}' is called with nothing for '{}', which it requires",
                    called.name, param.name
                );
                return Err(self.bad(&why));
            }
            names.push((param.name.clone(), value.unwrap_or_else(|| param.default.clone())));
        }
        Ok(names)
    }

    /// The arguments of a `.irp` or `.irpc`, split as a call's are.
    fn arguments(&mut self, mut text: &str) -> Result<Vec<(String, String)>, Trouble> {
        let mut out = Vec::new();
        text = text.trim();
        while !text.is_empty() {
            let (value, after) = self.argument(text)?;
            out.push((String::new(), value));
            text = after.trim_start();
            text = text.strip_prefix(',').unwrap_or(text).trim_start();
        }
        Ok(out)
    }

    /// One argument off the front of the text, and what is left after it.
    ///
    /// A string in double quotes is what is inside the quotes. Under `.altmacro` so is `<...>`,
    /// with `!` in front of a character that would otherwise end it, and `%expr` is the value of
    /// the expression written as a number. Anything else runs to a comma, or to a blank outside
    /// brackets that is not beside an operator.
    fn argument<'t>(&mut self, text: &'t str) -> Result<(String, &'t str), Trouble> {
        let bytes = text.as_bytes();
        if bytes.first() == Some(&b'"') {
            let mut out = String::new();
            let mut chars = text.char_indices().skip(1);
            while let Some((at, ch)) = chars.next() {
                match ch {
                    '"' => return Ok((out, &text[at + 1..])),
                    // An escaped quote is a quote, and any other escape is left for the directive
                    // that reads the text, which is how gas does it.
                    '\\' => match chars.next() {
                        Some((_, '"')) => out.push('"'),
                        Some((_, next)) => {
                            out.push('\\');
                            out.push(next);
                        }
                        None => out.push('\\'),
                    },
                    _ => out.push(ch),
                }
            }
            return Err(self.bad("an argument in quotes that is never closed"));
        }
        if self.macros.alternate && bytes.first() == Some(&b'<') {
            let mut out = String::new();
            let mut depth = 0;
            let mut chars = text.char_indices().skip(1);
            while let Some((at, ch)) = chars.next() {
                match ch {
                    '!' => {
                        if let Some((_, next)) = chars.next() {
                            out.push(next);
                        }
                    }
                    '<' => {
                        depth += 1;
                        out.push(ch);
                    }
                    '>' if depth == 0 => return Ok((out, &text[at + 1..])),
                    '>' => {
                        depth -= 1;
                        out.push(ch);
                    }
                    _ => out.push(ch),
                }
            }
            return Err(self.bad("an argument in '<' that is never closed with '>'"));
        }
        let end = plain_end(text);
        if self.macros.alternate && bytes.first() == Some(&b'%') {
            let value = self.value_now(&text[1..end])?;
            return Ok((value.to_string(), &text[end..]));
        }
        Ok((text[..end].trim().to_owned(), &text[end..]))
    }

    /// The text with every `\name` of these names replaced by what it stands for.
    ///
    /// `\()` is nothing, and is there to end a name where a letter follows it. `\@` is how many
    /// macros were expanded before this one, when this is a macro's body. A backslash in front of
    /// a name that is not one of these is left as it is, so that an `.irp` inside a macro keeps its
    /// own names for when it is read. Under `.altmacro` a name is replaced without the backslash as
    /// well, outside a string, and `&` next to one is taken out.
    fn substitute(&self, line: &str, names: &[(String, String)], number: Option<usize>) -> String {
        let look = |word: &str| names.iter().find(|(name, _)| name == word).map(|(_, v)| v);
        let alternate = self.macros.alternate;
        let bytes = line.as_bytes();
        let mut out = String::with_capacity(line.len());
        let mut at = 0;
        let mut quoted = false;
        while at < bytes.len() {
            let byte = bytes[at];
            if byte == b'\\' {
                let next = bytes.get(at + 1).copied();
                if line[at + 1..].starts_with("()") {
                    at += 3;
                    continue;
                }
                if next == Some(b'@') {
                    if let Some(number) = number {
                        out.push_str(&number.to_string());
                        at += 2;
                        continue;
                    }
                }
                if next.is_some_and(|next| starts(next) || next.is_ascii_digit()) {
                    let end = name_end(line, at + 1);
                    match look(&line[at + 1..end]) {
                        Some(value) => out.push_str(value),
                        None => out.push_str(&line[at..end]),
                    }
                    at = end;
                    continue;
                }
                // Not something this fills in, so the backslash and the character after it are
                // kept together, which is what stops `\\name` being read as a name.
                out.push('\\');
                at += 1;
                if let Some(next) = line[at..].chars().next() {
                    out.push(next);
                    at += next.len_utf8();
                }
                continue;
            }
            if byte == b'"' {
                quoted = !quoted;
            }
            if alternate && !quoted && starts(byte) && (at == 0 || !word_byte(bytes[at - 1])) {
                let end = name_end(line, at);
                if let Some(value) = look(&line[at..end]) {
                    // `&` on either side of a name joins it to what is there, and goes.
                    if out.ends_with('&') {
                        out.pop();
                    }
                    out.push_str(value);
                    at = end;
                    if bytes.get(at) == Some(&b'&') {
                        at += 1;
                    }
                    continue;
                }
                out.push_str(&line[at..end]);
                at = end;
                continue;
            }
            let ch = line[at..].chars().next().expect("inside the line");
            out.push(ch);
            at += ch.len_utf8();
        }
        out
    }

    /// Read a conditional directive if this is one, and say whether it was.
    ///
    /// These are looked at whether or not the text around them is being read, since a `.endif`
    /// inside a skipped branch closes an `.if` inside it rather than the one outside.
    fn conditional(&mut self, word: &str, rest: &str) -> Result<bool, Trouble> {
        match word {
            ".if" | ".ifdef" | ".ifndef" | ".ifnotdef" | ".ifc" | ".ifnc" | ".ifeqs" | ".ifnes"
            | ".ifb" | ".ifnb" | ".ifeq" | ".ifne" | ".ifgt" | ".ifge" | ".iflt" | ".ifle" => {
                let outer = self.macros.active();
                let now = outer && self.test(word, rest)?;
                self.macros.conds.push(Cond { outer, taken: now, now, otherwise: false });
            }
            ".elseif" => {
                let Some(&cond) = self.macros.conds.last() else {
                    return Err(self.bad("'.elseif' with no '.if' open"));
                };
                if cond.otherwise {
                    return Err(self.bad("'.elseif' after '.else'"));
                }
                let now = cond.outer && !cond.taken && self.test(".if", rest)?;
                let cond = self.macros.conds.last_mut().expect("checked above");
                cond.now = now;
                cond.taken |= now;
            }
            ".else" => {
                let Some(cond) = self.macros.conds.last_mut() else {
                    return Err(self.bad("'.else' with no '.if' open"));
                };
                if cond.otherwise {
                    return Err(self.bad("'.else' twice for one '.if'"));
                }
                cond.now = cond.outer && !cond.taken;
                cond.taken = true;
                cond.otherwise = true;
            }
            ".endif" => {
                if self.macros.conds.pop().is_none() {
                    return Err(self.bad("'.endif' with no '.if' open"));
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Whether the branch after one of the `.if` directives is taken.
    fn test(&mut self, word: &str, rest: &str) -> Result<bool, Trouble> {
        Ok(match word {
            ".if" | ".ifne" => self.value_now(rest)? != 0,
            ".ifeq" => self.value_now(rest)? == 0,
            ".ifgt" => self.value_now(rest)? > 0,
            ".ifge" => self.value_now(rest)? >= 0,
            ".iflt" => self.value_now(rest)? < 0,
            ".ifle" => self.value_now(rest)? <= 0,
            ".ifdef" => self.defined(rest.trim()),
            ".ifndef" | ".ifnotdef" => !self.defined(rest.trim()),
            ".ifb" => rest.trim().is_empty(),
            ".ifnb" => !rest.trim().is_empty(),
            ".ifc" | ".ifnc" => {
                let (first, second) = self.compared(rest)?;
                (first == second) == (word == ".ifc")
            }
            ".ifeqs" | ".ifnes" => {
                let args = split(rest, ',');
                let [first, second] = self.two(&args, word)?;
                let same = self.string(&first)? == self.string(&second)?;
                same == (word == ".ifeqs")
            }
            _ => unreachable!("every conditional is matched by the caller"),
        })
    }

    /// The two strings `.ifc` compares.
    ///
    /// The first runs to the comma and the second to the end, each without the blanks round it.
    /// Either may be in single quotes, which is how one holds a comma, with two quotes inside for
    /// one.
    fn compared(&self, rest: &str) -> Result<(String, String), Trouble> {
        let (first, after) = match rest.strip_prefix('\'') {
            Some(inner) => {
                let (value, after) = single_quoted(inner)
                    .ok_or_else(|| self.bad("a string in '.ifc' that is never closed"))?;
                let after = after.trim_start();
                let after = after.strip_prefix(',').ok_or_else(|| {
                    self.bad("'.ifc' wants two strings with a comma between them")
                })?;
                (value, after)
            }
            None => {
                let Some(comma) = rest.find(',') else {
                    return Err(self.bad("'.ifc' wants two strings with a comma between them"));
                };
                (rest[..comma].trim().to_owned(), &rest[comma + 1..])
            }
        };
        let after = after.trim();
        let second = match after.strip_prefix('\'') {
            Some(inner) => {
                single_quoted(inner)
                    .ok_or_else(|| self.bad("a string in '.ifc' that is never closed"))?
                    .0
            }
            None => after.to_owned(),
        };
        Ok((first, second))
    }

    /// Whether the file has defined this name so far, as a label, a set or a common.
    fn defined(&self, name: &str) -> bool {
        self.current.contains_key(name)
            || self.known.get(name).is_some_and(|&sym| self.syms[sym].at != Held::Undefined)
    }

    /// The value of an expression where it is written, which is what a conditional and a `.rept`
    /// count want.
    ///
    /// gas reads these as soon as it reaches them, so every name in one has to have a value by
    /// then: a set looked through to what it was set to, and two labels in one section taken away
    /// from each other. The comparisons and `&&` and `||` are read here, with the truth values gas
    /// gives them, which is minus one for a comparison that holds and one for the other two. The
    /// arithmetic between them is the reader's own.
    fn value_now(&mut self, text: &str) -> Result<i64, Trouble> {
        let text = text.trim();
        if text.is_empty() {
            return Err(self.bad("an expression that says nothing"));
        }
        if let Some((left, op, right)) = operator(text) {
            // Two registers, which gas lets a conditional compare by name. The kernel's
            // `UNWIND_HINT_REGS` says `.if \base == %rsp` to ask which one it was handed.
            if let (Some(a), Some(b)) = (register(left), register(right)) {
                return match op {
                    "==" => Ok(if a == b { -1 } else { 0 }),
                    "!=" | "<>" => Ok(if a == b { 0 } else { -1 }),
                    _ => Err(self.bad(&format!("'{op}' between two registers"))),
                };
            }
            let left = self.value_now(left)?;
            // gas works out both sides, and so does this, so a name with no value is a mistake on
            // either side of a `&&` whatever the other side says.
            let right = self.value_now(right)?;
            let truth = |holds: bool| if holds { -1 } else { 0 };
            return Ok(match op {
                "||" => i64::from(left != 0 || right != 0),
                "&&" => i64::from(left != 0 && right != 0),
                "==" => truth(left == right),
                "!=" | "<>" => truth(left != right),
                "<=" => truth(left <= right),
                ">=" => truth(left >= right),
                "<" => truth(left < right),
                ">" => truth(left > right),
                _ => unreachable!("every operator is one of these"),
            });
        }
        // A comparison in brackets, which the reader's arithmetic cannot open. Only when the
        // brackets hold the whole of what is left, since `(a) + (b)` is not one pair.
        if let Some(inner) = bracketed(text) {
            if operator(inner).is_some() || bracketed(inner).is_some() {
                return self.value_now(inner);
            }
        }
        if let Some(inner) = text.strip_prefix('!').and_then(bracketed) {
            return Ok(i64::from(self.value_now(inner)? == 0));
        }
        let sum = self.expression(text)?;
        let flat = self.through(sum, 0)?;
        let residue = self.reduce(&flat).map_err(|why| self.bad(&why))?;
        if !residue.left.is_empty() {
            let why =
                format!("'{text}' has to be a number where it is written, and is not one yet");
            return Err(self.bad(&why));
        }
        Ok(residue.constant)
    }

    /// The expression with every set name in it replaced by what it was set to, until what is
    /// left is numbers, places and names that have not been set.
    fn through(&self, sum: super::Sum, depth: usize) -> Result<super::Sum, Trouble> {
        if depth > THROUGH {
            return Err(self.bad("a set that comes back round to itself"));
        }
        let mut out = super::Sum::just(sum.constant);
        for term in sum.terms {
            let set = match &term.what {
                super::What::Symbol(name) => self
                    .known
                    .get(name)
                    .and_then(|sym| self.setting.get(sym))
                    .map(|&index| self.sets[index].1.clone()),
                super::What::Here { .. } => None,
            };
            match set {
                Some(value) => out = out.plus(self.through(value, depth + 1)?.times(term.coeff)),
                None => out.terms.push(term),
            }
        }
        Ok(out)
    }
}

/// The named argument at the front of a call, as which parameter it is and the text after the
/// `=`, when it is one.
fn keyword<'t>(text: &'t str, params: &[Param]) -> Option<(usize, &'t str)> {
    if !text.as_bytes().first().is_some_and(|&byte| starts(byte)) {
        return None;
    }
    let end = name_end(text, 0);
    let after = text[end..].trim_start().strip_prefix('=')?;
    if after.starts_with('=') {
        return None;
    }
    let at = params.iter().position(|param| param.name == text[..end])?;
    Some((at, after.trim_start()))
}

/// Where an argument that is not quoted ends.
///
/// At a comma, or at a blank outside brackets unless an operator is on one side of it, in which
/// case the blank is inside an expression and the argument goes on past it. A minus with a blank
/// after it is taken away from what is in front, and one with none is the sign of the next
/// argument.
fn plain_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'"' => {
                at += 1;
                while at < bytes.len() && bytes[at] != b'"' {
                    at += usize::from(bytes[at] == b'\\') + 1;
                }
            }
            // A character constant, whose character may be the space or the comma that would
            // otherwise end the argument. The kernel's `relocate_kernel_64.S` passes `' '` to a
            // macro. The closing quote is optional, as it is to gas.
            b'\'' => {
                let mut end = at + 1;
                if bytes.get(end) == Some(&b'\\') {
                    end += 1;
                }
                at = if bytes.get(end + 1) == Some(&b'\'') { end + 1 } else { end };
            }
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' => return at,
            b' ' | b'\t' if depth <= 0 => {
                let before = text[..at].trim_end().as_bytes().last().copied();
                let after = text[at..].trim_start();
                let next = after.as_bytes().first().copied();
                let joined = before.is_some_and(|byte| b"+-*/|&^<>=!~(".contains(&byte))
                    || next.is_some_and(|byte| b"+*/|&^<>=)".contains(&byte))
                    || (next == Some(b'-') && after[1..].starts_with([' ', '\t']));
                if !joined {
                    return at;
                }
            }
            _ => {}
        }
        at += 1;
    }
    bytes.len()
}

/// Where a name that starts at `at` ends.
fn name_end(text: &str, at: usize) -> usize {
    text[at..].find(|ch: char| !word_byte(ch as u8)).map_or(text.len(), |end| at + end)
}

/// Whether a name for a macro parameter goes on with this. Not `$`, which on this machine is the
/// front of an immediate rather than part of a name.
fn word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.')
}

/// A string after its opening single quote, as its text and what follows the closing one, with
/// two quotes inside it standing for one.
fn single_quoted(text: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        if ch == '\'' {
            if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                chars.next();
                out.push('\'');
                continue;
            }
            return Some((out, &text[at + 1..]));
        }
        out.push(ch);
    }
    None
}

/// The inside of the text when the whole of it is one pair of brackets.
fn bracketed(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('(')?.strip_suffix(')')?;
    let mut depth = 0i32;
    for byte in inner.bytes() {
        match byte {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    (depth == 0).then_some(inner)
}

/// The loosest operator outside brackets that the reader's arithmetic does not know, as the text
/// on each side of it and the operator.
///
/// `||` binds loosest, then `&&`, then the comparisons, which is gas's order. Among several of the
/// same strength the last is taken, which reads them from the left.
fn operator(text: &str) -> Option<(&str, &'static str, &str)> {
    const LEVELS: [&[&str]; 3] = [&["||"], &["&&"], &["==", "!=", "<>", "<=", ">=", "<", ">"]];
    for ops in LEVELS {
        let bytes = text.as_bytes();
        let mut depth = 0i32;
        let mut found = None;
        let mut at = 0;
        while at < bytes.len() {
            match bytes[at] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                b'\'' => at += 1,
                _ if depth == 0 => {
                    let rest = &text[at..];
                    // `<<` and `>>` are shifts, which the arithmetic reads, and are stepped over
                    // whole so that their second half is not taken for a comparison.
                    if rest.starts_with("<<") || rest.starts_with(">>") {
                        at += 2;
                        continue;
                    }
                    if let Some(op) = ops.iter().find(|op| rest.starts_with(**op)) {
                        found = Some((at, *op));
                        at += op.len();
                        continue;
                    }
                    // A two character comparison is not a one character one followed by
                    // something, so step over the ones this level is not looking for.
                    if ["==", "!=", "<>", "<=", ">=", "||", "&&"]
                        .iter()
                        .any(|op| rest.starts_with(op))
                    {
                        at += 2;
                        continue;
                    }
                }
                _ => {}
            }
            at += 1;
        }
        if let Some((at, op)) = found {
            return Some((&text[..at], op, &text[at + op.len()..]));
        }
    }
    None
}

/// The register a side of a comparison names, in lower case, when it is one.
fn register(text: &str) -> Option<String> {
    let name = text.trim().strip_prefix('%')?;
    let fine = name.starts_with(|ch: char| ch.is_ascii_alphabetic())
        && name.chars().all(|ch| ch.is_ascii_alphanumeric());
    fine.then(|| name.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::super::read;
    use rucc_object::{Assembled, Held};
    use rucc_tuple::Arch;

    /// The file, read, with a failure reported as a panic naming the line it was on.
    fn assembled(text: &str) -> Assembled {
        match read(text, Arch::X86_64) {
            Ok(done) => done,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        }
    }

    /// The bytes of one section.
    fn bytes(assembled: &Assembled, name: &str) -> Vec<u8> {
        assembled
            .parts
            .iter()
            .find(|part| part.name == name)
            .map(|part| part.bytes.clone())
            .unwrap_or_default()
    }

    /// Two files that should come out as the same bytes in `.text`.
    fn same(text: &str, plain: &str) {
        assert_eq!(bytes(&assembled(text), ".text"), bytes(&assembled(plain), ".text"));
    }

    #[test]
    fn a_macro_is_its_body_with_the_arguments_written_in() {
        same(
            ".macro MODRM mod opd1 opd2\n.byte \\mod | (\\opd1 & 7) | ((\\opd2 & 7) << 3)\n.endm\n\
             MODRM 0xc0, 1, 2\nMODRM 0xc0 3 4",
            ".byte 0xc0 | 1 | (2 << 3)\n.byte 0xc0 | 3 | (4 << 3)",
        );
    }

    #[test]
    fn a_character_constant_is_one_argument_even_when_it_is_a_space_or_a_comma() {
        // `relocate_kernel_64.S` prints a register's name a letter at a time, and `r8` is padded
        // with `' '`.
        same(
            ".macro PR a, b, c, d\n.byte \\a, \\b, \\c, \\d\n.endm\nPR 'r', ' ', ',', ':'",
            ".byte 0x72, 0x20, 0x2c, 0x3a",
        );
    }

    #[test]
    fn defaults_names_and_a_required_parameter() {
        // The shape of `UNWIND_HINT_REGS` and `ENCODE_FRAME_POINTER`.
        same(
            ".macro HINT base=%rsp offset=0 signal=1\n.byte \\offset, \\signal\n.endm\n\
             HINT\nHINT offset=8\nHINT base=%rbp, signal=0 offset=16",
            ".byte 0, 1\n.byte 8, 1\n.byte 16, 0",
        );
        let trouble =
            read(".macro JMP_NOSPEC reg:req\njmp *\\reg\n.endm\nJMP_NOSPEC", Arch::X86_64)
                .unwrap_err();
        assert!(trouble.why.contains("requires"), "{}", trouble.why);
    }

    #[test]
    fn a_vararg_parameter_takes_the_rest_and_irp_walks_it() {
        // `_aesenc_loop` in `aes-gcm-vaes-avx2.S` and `CLEAR_REGS` in `kvm/vmenter.h`.
        same(
            ".macro CLEAR_REGS regs:vararg\n.irp reg, \\regs\nxorl \\reg, \\reg\n.endr\n.endm\n\
             CLEAR_REGS %eax, %ecx,%edx",
            "xorl %eax, %eax\nxorl %ecx, %ecx\nxorl %edx, %edx",
        );
    }

    #[test]
    fn a_numbered_expansion_gives_every_call_labels_of_its_own() {
        same(
            ".macro LOOP\n.Lagain\\@:\ndec %eax\njne .Lagain\\@\n.endm\nLOOP\nLOOP",
            ".La0:\ndec %eax\njne .La0\n.La1:\ndec %eax\njne .La1",
        );
    }

    #[test]
    fn a_name_ends_where_the_empty_brackets_say() {
        let done = assembled(".macro F op\n\\op\\()_safe_regs:\nret\n.endm\nF rdmsr");
        assert!(done.names.iter().any(|name| name.name == "rdmsr_safe_regs"));
    }

    #[test]
    fn an_alternative_in_quotes_with_statements_split_by_semicolons() {
        // `ALTERNATIVE` in `alternative.h`, after the preprocessor has joined its body onto one
        // line, called with an empty instruction on one side.
        let text = "\
.macro ALTERNATIVE oldinstr, newinstr, ft_flags
740: \\oldinstr ; 741: .pushsection .altinstructions,\"a\" ; .long 740b - . ; .long 743f - . ; \
.4byte \\ft_flags ; .byte 741b-740b ; .popsection ; .pushsection .altinstr_replacement,\"ax\" ; \
743: \\newinstr ; 744: .popsection
.endm
ALTERNATIVE \"\", \"movl %eax, %ecx\", 3*32+18
ALTERNATIVE \"call foo\", \"\", 1";
        let done = assembled(text);
        assert_eq!(bytes(&done, ".altinstr_replacement"), [0x89, 0xc1]);
        assert_eq!(bytes(&done, ".text"), [0xe8, 0, 0, 0, 0]);
        let table = bytes(&done, ".altinstructions");
        assert_eq!(table.len(), 2 * 13);
        assert_eq!(&table[8..12], &(3 * 32 + 18u32).to_le_bytes());
        assert_eq!(table[12], 0);
        assert_eq!(table[25], 5);
    }

    #[test]
    fn extable_type_reg_counts_with_sets_and_tests_the_count() {
        // `DEFINE_EXTABLE_TYPE_REG` from `asm.h`, as an inline template reads once gcc has turned
        // `%%` into `%`, defined, used and purged the way `_ASM_EXTABLE_TYPE_REG` does.
        let define = "\
.macro extable_type_reg type:req reg:req
.set .Lfound, 0
.set .Lregnr, 0
.irp rs,rax,rcx,rdx,rbx,rsp,rbp,rsi,rdi,r8,r9,r10,r11,r12,r13,r14,r15
.ifc \\reg, %\\rs
.set .Lfound, .Lfound+1
.long \\type + (.Lregnr << 8)
.endif
.set .Lregnr, .Lregnr+1
.endr
.set .Lregnr, 0
.irp rs,eax,ecx,edx,ebx,esp,ebp,esi,edi,r8d,r9d,r10d,r11d,r12d,r13d,r14d,r15d
.ifc \\reg, %\\rs
.set .Lfound, .Lfound+1
.long \\type + (.Lregnr << 8)
.endif
.set .Lregnr, .Lregnr+1
.endr
.if (.Lfound != 1)
.error \"extable_type_reg: bad register argument\"
.endif
.endm
";
        let text = format!(
            "{define}extable_type_reg reg=%rdx, type=5\n.purgem extable_type_reg\n\
             {define}extable_type_reg 7, %r9d\n.purgem extable_type_reg\n"
        );
        let done = assembled(&text);
        let mut want = (5u32 + (2 << 8)).to_le_bytes().to_vec();
        want.extend((7u32 + (9 << 8)).to_le_bytes());
        assert_eq!(bytes(&done, ".text"), want);
        let trouble =
            read(&format!("{define}extable_type_reg 1, %xmm0"), Arch::X86_64).unwrap_err();
        assert!(trouble.why.contains("bad register argument"), "{}", trouble.why);
        assert!(trouble.why.contains("in the expansion of 'extable_type_reg'"), "{}", trouble.why);
    }

    #[test]
    fn a_macro_that_calls_one_that_assigns_and_tests_with_elseif() {
        // `R32_NUM`, `R64_NUM` and `REG_TYPE` from `inst.h`, cut down to a few registers.
        let text = "\
.macro R32_NUM opd r32
\\opd = 100
.ifc \\r32,%eax
\\opd = 0
.endif
.ifc \\r32,%ecx
\\opd = 1
.endif
.endm
.macro R64_NUM opd r64
\\opd = 100
.ifc \\r64,%rax
\\opd = 0
.endif
.ifc \\r64,%r9
\\opd = 9
.endif
.endm
.macro REG_TYPE type reg
R32_NUM reg_type_r32 \\reg
R64_NUM reg_type_r64 \\reg
.if reg_type_r64 <> 100
\\type = 1
.elseif reg_type_r32 <> 100
\\type = 0
.else
\\type = 100
.endif
.endm
.macro PFX_REX opd1 opd2 W=0
.if ((\\opd1 | \\opd2) & 8) || \\W
.byte 0x40 | ((\\opd1 & 8) >> 3) | ((\\opd2 & 8) >> 1) | (\\W << 3)
.endif
.endm
REG_TYPE t1 %ecx
REG_TYPE t2 %r9
REG_TYPE t3 %xmm1
.byte t1, t2, t3
R64_NUM a %r9
R64_NUM b %rax
PFX_REX a b
PFX_REX b b 1
PFX_REX b b
";
        assert_eq!(bytes(&assembled(text), ".text"), [0, 1, 100, 0x41, 0x48]);
    }

    #[test]
    fn a_blank_argument_is_tested_with_ifnb() {
        // `calling.h`'s `.ifnb \save_reg`.
        same(
            ".macro SAVE save_reg\n.ifnb \\save_reg\npush \\save_reg\n.else\nnop\n.endif\n.endm\n\
             SAVE %rax\nSAVE",
            "push %rax\nnop",
        );
    }

    #[test]
    fn rept_and_irpc_repeat_their_bodies() {
        // `efi-mixed.S` walks the digits of `0123` with `.irpc`.
        same(
            ".rept 3\nnop\n.endr\n.irpc l, 0123\n.byte \\l * 8\n.endr",
            "nop\nnop\nnop\n.byte 0, 8, 16, 24",
        );
        same(".irp x\n.byte 1\n.endr\n.rept 0\n.byte 2\n.endr", ".byte 1");
    }

    #[test]
    fn a_macro_may_purge_itself_on_one_line() {
        // `BEGIN_IRQ_SAVE` in `atomic64_386_32.S`, whose `endp` ends a function and then forgets
        // itself, so that the next function may define it again.
        let text = ".macro endp; .size f1, .-f1; .purgem endp; .endm; f1: nop\nendp\n\
                    .macro endp; ret; .purgem endp; .endm; endp";
        let done = assembled(text);
        assert_eq!(bytes(&done, ".text"), [0x90, 0xc3]);
        let f1 = done.names.iter().find(|name| name.name == "f1").expect("f1");
        assert_eq!(f1.size, 1);
    }

    #[test]
    fn exitm_stops_the_expansion_and_closes_what_it_opened() {
        same(
            ".macro COUNT n\n.if \\n == 0\n.exitm\n.endif\n.byte \\n\nCOUNT (\\n-1)\n.endm\nCOUNT 3\nnop",
            ".byte 3\n.byte 2\n.byte 1\nnop",
        );
    }

    #[test]
    fn a_macro_may_define_a_macro() {
        same(
            ".macro MAKE name, byte\n.macro \\name\n.byte \\byte\n.endm\n.endm\nMAKE one, 1\none\none",
            ".byte 1\n.byte 1",
        );
    }

    #[test]
    fn a_skipped_branch_skips_the_conditionals_inside_it() {
        same(
            ".if 0\n.if 1\n.byte 1\n.else\n.byte 2\n.endif\n.error \"no\"\n.else\n.byte 3\n.endif",
            ".byte 3",
        );
        same(".set a, 1\n.ifdef a\n.byte 1\n.endif\n.ifndef b\n.byte 2\n.endif", ".byte 1, 2");
        same(".ifc 'a,b', 'a,b'\n.byte 1\n.endif\n.ifnc a, b\n.byte 2\n.endif", ".byte 1, 2");
        same(".ifeqs \"x\", \"x\"\n.byte 1\n.endif\n.ifgt -1\n.byte 2\n.endif", ".byte 1");
    }

    #[test]
    fn a_comparison_is_minus_one_when_it_holds() {
        same(
            ".if (1 < 2) == -1\n.byte 1\n.endif\n.if !(1 == 2) && 2 >= 2\n.byte 2\n.endif",
            ".byte 1, 2",
        );
    }

    #[test]
    fn a_macro_name_ends_at_a_bracket() {
        same(".macro skip func\n.byte \\func\n.endm\nskip(3)\nx: skip (4)", ".byte 3\nx: .byte 4");
    }

    #[test]
    fn a_label_may_have_spaces_before_its_colon() {
        same("0 :\n.byte 1\nfoo\t:\tnop\njmp 0b", "0:\n.byte 1\nfoo: nop\njmp 0b");
    }

    #[test]
    fn a_conditional_can_ask_which_register_a_macro_was_handed() {
        // The shape of the kernel's `UNWIND_HINT_REGS`, which is used with the default and with
        // another register.
        same(
            ".macro hint base=%rsp\n.if \\base == %rsp\n.byte 1\n.elseif \\base != %RDI\n.byte 2\n.else\n.byte 3\n.endif\n.endm\nhint\nhint base=%rbp\nhint %rdi",
            ".byte 1, 2, 3",
        );
    }

    #[test]
    fn a_difference_of_labels_above_is_a_number_a_conditional_can_test() {
        same("a: nop\nnop\nb:\n.if b - a == 2\n.byte 7\n.endif", "nop\nnop\n.byte 7");
    }

    #[test]
    fn what_is_left_open_is_refused() {
        let open = read(".macro m\nnop", Arch::X86_64).unwrap_err();
        assert!(open.why.contains(".endm"), "{}", open.why);
        assert_eq!(open.line, 1);
        assert!(read(".if 1\nnop", Arch::X86_64).is_err());
        assert!(read(".endif", Arch::X86_64).is_err());
        assert!(read(".if undefined_name\n.endif", Arch::X86_64).is_err());
        let deep = read(".macro m\nm\n.endm\nm", Arch::X86_64).unwrap_err();
        assert!(deep.why.contains("deep"), "{}", deep.why);
        let twice = read(".macro m\n.endm\n.macro M\n.endm", Arch::X86_64).unwrap_err();
        assert!(twice.why.contains("already defined"), "{}", twice.why);
    }

    #[test]
    fn altmacro_reads_angle_brackets_percent_and_local() {
        same(
            ".altmacro\n.macro PAIR a, b\nLOCAL here\nhere: .byte a&0, b\n.endm\nPAIR <1>, %(2+3)\nPAIR 2, 6",
            ".byte 10, 5\n.byte 20, 6",
        );
        let done = assembled(".altmacro\n.macro L\nLOCAL x\nx: nop\n.endm\nL\nL");
        let locals = done
            .names
            .iter()
            .filter(|name| name.name.starts_with(".LL") && name.at != Held::Undefined)
            .count();
        assert_eq!(locals, 2, "each call has a label of its own");
    }

    #[test]
    fn a_template_defines_a_macro_the_next_one_uses() {
        // The listing of a unit whose `asm` at file scope defines a macro that a function's
        // template calls, which is the order gcc writes them in and the order this reads them.
        let text = "\
\t.text
#APP
.macro ANNOTATE type:req
.Lhere_\\@:
.pushsection .discard.annotate_insn,\"M\",@progbits,8
.long .Lhere_\\@ - .
.long \\type
.popsection
.endm
#NO_APP
\t.text
f:
#APP
ANNOTATE 1
nop
#NO_APP
\tret
";
        let done = assembled(text);
        assert_eq!(bytes(&done, ".text"), [0x90, 0xc3]);
        assert_eq!(bytes(&done, ".discard.annotate_insn")[4..], [1, 0, 0, 0]);
    }
}
