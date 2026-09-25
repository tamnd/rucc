//! Reading a file of assembly.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1, which asks for a real assembler with a real
//! directive set rather than a call out to `as`.
//!
//! # What is here and what is not
//!
//! The directives, the labels and the expressions. The instructions are [`crate::instruction`],
//! which this hands each line that is one and which hands back the bytes of it and the places in
//! those bytes that name something. The names are the reason the split falls there: what an
//! instruction is is a question about one line, and what it refers to is a question about the
//! whole file, because the label a jump goes to is usually further down than the jump is.
//!
//! A mnemonic with no bytes behind it is refused by name with its line number, and so is an
//! operand this cannot read. Guessing at either is the failure mode that matters here: an
//! assembler that skipped what it did not recognise would write an object that links, and what
//! would be wrong with it is a run of missing bytes in the middle of a function, which nothing
//! finds until the program runs.
//!
//! # Why expressions are worth this much of the file
//!
//! Because `.size foo, .-foo` is on the end of nearly every function gas ever wrote, and because a
//! table of addresses is `.quad` of a name. An expression here is kept as a constant plus a list of
//! names with coefficients, rather than collapsed to a number as it is parsed, for two reasons. A
//! name may not be defined yet when it is used, so nothing can be collapsed until the whole file has
//! been read. And two names in the same section have a difference even when neither has an address,
//! which is the whole of what `.-foo` is asking, so the pair has to survive as a pair to be
//! subtracted at the end. What is left over after the subtractions is what the linker is asked
//! about, and the shape of what is left is what says which relocation it is.

use std::collections::{BTreeMap, HashMap};

use rucc_mir::CfiOp;
use rucc_object::{
    Array, Assembled, Binding, Extent, Held, Name, Part, Reference, Reloc, Shape, Sort, Visibility,
};
use rucc_target::ObjectFormat;
use rucc_target::x86_64::{SYSV, gpr_named, nops};

/// What an instruction says about the place in it that names something, under a name that does not
/// collide with the [`Sort`] an ELF symbol has.
use crate::instruction::Sort as Reach;

/// A file this could not read, and where in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trouble {
    /// Which line, counting from one, so that it can be put in front of a message the way every
    /// other diagnostic in this compiler is.
    pub line: usize,
    /// What was wrong with it, already formatted and without the line number in it.
    pub why: String,
}

impl std::fmt::Display for Trouble {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.line, self.why)
    }
}

impl std::error::Error for Trouble {}

/// What a file of assembly says, as the sections and names an object file is written from.
///
/// # Errors
///
/// [`Trouble`] for a directive this does not know, an instruction it has no bytes for, an operand
/// it cannot read, an expression that does not reduce to something a relocation can say, or a file
/// that is malformed. Every one of them carries the line it was on.
pub fn read(text: &str) -> Result<Assembled, Trouble> {
    // Every branch starts out in its two byte form and the file is read again with the ones that
    // did not reach written long, until none is left over. A branch made long never goes back, so
    // each pass has more long ones than the last and there are only so many branches, which is how
    // gas does it and why the two come out the same size.
    let mut long = std::collections::HashSet::new();
    loop {
        let mut reader = Reader { long: long.clone(), ..Reader::default() };
        reader.run(text)?;
        match reader.finish()? {
            Ok(done) => return Ok(done),
            Err(grow) => long.extend(grow),
        }
    }
}

/// One name, while the file is still being read.
///
/// Held apart from [`Name`] because two of its fields are not answers yet. A `.set` is an expression
/// that may name something further down the file, and so is the second operand of `.size`, and both
/// have to wait for the end.
#[derive(Debug, Clone)]
struct Sym {
    name: String,
    at: Held,
    size: u64,
    sort: Sort,
    binding: Binding,
    visibility: Visibility,
    /// Whether this is a numbered local label, which is a place in the file rather than a name and
    /// so is resolved like one and then left out of the symbol table.
    numbered: bool,
}

/// A place in a section whose bytes are an expression that could not be worked out yet.
#[derive(Debug, Clone)]
struct Fixup {
    part: usize,
    at: u64,
    width: u8,
    sum: Sum,
    /// Which of the four things these bytes are, since a jump is allowed to go through a stub and a
    /// load of a datum is not, and a name reached through a table is a relocation however near it
    /// turns out to be. A directive writes [`Reach::Near`], which is the plain one.
    reach: Reach,
    /// Which relocation a reach through the global offset table asks for, which is decided by the
    /// instruction the hole is in and so is worked out while its bytes are still at hand.
    slot: Reference,
    /// Which branch of the file this is, counting every one that has a two byte form, when it was
    /// written in that form and so may turn out not to reach.
    branch: Option<usize>,
    /// Whether this is a jump at all, short or long, which gas works out to a global name where it
    /// leaves a call to one for the linker.
    jump: bool,
    line: usize,
}

/// An alignment, as this pass laid it out, for the next pass's branches to be judged across.
#[derive(Debug, Clone, Copy)]
struct Aligned {
    part: usize,
    /// Where the padding starts.
    at: u64,
    boundary: u64,
    /// The most padding the file allowed, past which there is none.
    most: Option<u64>,
    /// How much padding there is.
    need: u64,
}

/// One function's frame rules, as `.cfi_` directives said them.
#[derive(Debug)]
struct Frame {
    part: usize,
    start: u64,
    len: u64,
    /// The entry the record points at, made where `.cfi_startproc` was written, since that is the
    /// first instruction the rules are about whether or not a label is there.
    sym: usize,
    rows: crate::unwind::Rows,
    /// How far the end of the frame is from the register it is counted from, which a directive
    /// that says a slot relative to that register or adjusts the distance has to know.
    cfa: i32,
    /// What `.cfi_remember_state` put away, for `.cfi_restore_state` to bring back.
    remembered: Vec<i32>,
}

/// The file, as it is being read.
#[derive(Debug, Default)]
struct Reader {
    parts: Vec<Part>,
    /// Which index each section name is at, so that a second `.text` continues the first one.
    named: HashMap<String, usize>,
    /// The section being written to.
    here: usize,
    /// What `.pushsection` stacked up.
    stack: Vec<usize>,
    /// What `.previous` goes back to.
    before: Option<usize>,
    syms: Vec<Sym>,
    known: HashMap<String, usize>,
    /// How many times each numbered local label has been written so far, which is what `1b` counts
    /// back from and what `1f` counts forward from.
    counts: HashMap<String, usize>,
    /// Which sections have a name pointing into them, so that an empty one that something is
    /// defined in survives and an empty one nothing mentions does not.
    labelled: std::collections::HashSet<usize>,
    fixups: Vec<Fixup>,
    /// `.set` and `.equ`, as the symbol they name and the expression they were given.
    sets: Vec<(usize, Sum, usize)>,
    /// `.size`, the same way.
    sizes: Vec<(usize, Sum, usize)>,
    /// Which entry a name that has been set means from here on. A file may set one name as many
    /// times as it likes, and each use means the value it had where the use was written, so a
    /// second setting is a second entry and this says which one is current.
    current: HashMap<String, String>,
    /// The numbered entries a relocation names, which are kept in the symbol table so that the
    /// relocation has something to point at. That is a numbered local label or a set name reached
    /// from another section, and is rare.
    relocated: std::collections::HashSet<usize>,
    /// The names `.local` was said of, which a `.comm` after it makes room for here rather than
    /// asking the linker, the way `.lcomm` does. Every name is local until something says
    /// otherwise, so the binding alone cannot tell these apart.
    said_local: std::collections::HashSet<usize>,
    /// The function whose frame rules are being read, between `.cfi_startproc` and `.cfi_endproc`.
    frame: Option<Frame>,
    /// Every function that has had its frame rules read, in the order the file wrote them.
    frames: Vec<Frame>,
    /// Whether `.cfi_sections` left the unwind table out, which a file does when it wants the rules
    /// for a debugger only.
    no_unwind: bool,
    /// What the file said it was called. Kept apart from the rest because it is not a name anything
    /// refers to, and a file whose own name is also the name of something in it would otherwise be
    /// one symbol where it should be two.
    files: Vec<String>,
    /// The branches an earlier pass found out of reach of two bytes, which this one writes long.
    long: std::collections::HashSet<usize>,
    /// How many branches with a two byte form have been read so far.
    branches: usize,
    /// Every alignment in the file, in the order it was written.
    aligns: Vec<Aligned>,
    line: usize,
}

impl Reader {
    /// Read the whole file.
    fn run(&mut self, text: &str) -> Result<(), Trouble> {
        // Before anything else, so that a file which never names a section still has one and a
        // stray directive has somewhere to go. gas starts in `.text` and so does this.
        self.section(".text", Shape::of(".text"));
        let mut commenting = false;
        for (index, raw) in text.lines().enumerate() {
            self.line = index + 1;
            let line = self.strip(raw, &mut commenting)?;
            for statement in split(&line, ';') {
                self.statement(statement.trim())?;
            }
        }
        if commenting {
            return Err(self.bad("a block comment was opened and never closed"));
        }
        Ok(())
    }

    /// One line without its comments.
    ///
    /// Three kinds, because gas takes three on this machine: `/* */` which may run over the end of
    /// a line, `//` to the end of one, and `#` to the end of one. The last is why the output of the
    /// preprocessor can be read directly: a `# 42 "foo.h"` line marker is a comment and nothing has
    /// to know it is one.
    fn strip(&self, raw: &str, commenting: &mut bool) -> Result<String, Trouble> {
        let mut out = String::with_capacity(raw.len());
        let bytes = raw.as_bytes();
        let mut i = 0;
        let mut quote = None;
        while i < bytes.len() {
            let rest = &raw[i..];
            if *commenting {
                if let Some(end) = rest.find("*/") {
                    *commenting = false;
                    // A space, because a comment between two words is a separator and pasting the
                    // two together would make one word out of them.
                    out.push(' ');
                    i += end + 2;
                } else {
                    return Ok(out);
                }
                continue;
            }
            let ch = bytes[i] as char;
            if let Some(mark) = quote {
                out.push(ch);
                if ch == '\\' && i + 1 < bytes.len() {
                    out.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                if ch == mark {
                    quote = None;
                }
                i += 1;
                continue;
            }
            if ch == '"' {
                quote = Some('"');
                out.push(ch);
                i += 1;
                continue;
            }
            if rest.starts_with("/*") {
                *commenting = true;
                i += 2;
                continue;
            }
            if rest.starts_with("//") || ch == '#' {
                return Ok(out);
            }
            out.push(ch);
            i += 1;
        }
        if quote.is_some() {
            return Err(self.bad("a string was opened and the line ended before it closed"));
        }
        Ok(out)
    }

    /// One statement, which is any number of labels and then at most one directive.
    fn statement(&mut self, mut text: &str) -> Result<(), Trouble> {
        loop {
            text = text.trim_start();
            let Some(name) = labelled(text) else { break };
            self.label(&name)?;
            text = &text[name.len() + 1..];
        }
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let (word, rest) = match text.find(char::is_whitespace) {
            Some(cut) => (&text[..cut], text[cut..].trim()),
            None => (text, ""),
        };
        if let Some((name, what)) = assigned(text) {
            return self.assign(name, what);
        }
        if let Some(directive) = word.strip_prefix('.') {
            return self.directive(directive, rest);
        }
        if let Some((word, rest)) = repeated(word, rest) {
            return self.instruction(&word, rest);
        }
        self.instruction(word, rest)
    }

    /// One instruction, as the bytes of it.
    ///
    /// What an instruction is is [`crate::instruction`]'s business and what it refers to is this
    /// one's, which is the same division as everywhere else in this file: the bytes come back with
    /// the places in them that name something, and a name is the whole file's question because the
    /// label a jump goes to is usually further down than the jump is.
    ///
    /// Each of those places becomes the same kind of fixup `.long foo - .` makes, written as the
    /// name minus where the instruction ends, since that is what the machine counts a branch and a
    /// rip-relative address from. Then the arithmetic already here does the rest: a target in this
    /// section cancels down to a number and is written into the bytes, and one that does not is a
    /// relocation with the right addend on it. A branch says so, because a call to a name another
    /// object defines is allowed to go through a stub and a load of a datum is not.
    fn instruction(&mut self, word: &str, rest: &str) -> Result<(), Trouble> {
        let args = if rest.is_empty() { Vec::new() } else { split(rest, ',') };
        let mut written = crate::instruction::one(word, &args).map_err(|why| self.bad(&why))?;
        // `jmp .+10` has been given its short form already, where the distance is known.
        let mut branch = None;
        let short = crate::instruction::short(&written).filter(|_| written.holes[0].name != ".");
        let jump = short.is_some();
        if let Some(short) = short {
            if !self.long.contains(&self.branches) {
                branch = Some(self.branches);
                written = short;
            }
            self.branches += 1;
        }
        let part = self.here;
        let at = self.at();
        self.put(&written.bytes)?;
        let end = at + written.bytes.len() as u64;
        let slot = crate::bytes::slot(&written.bytes);
        for hole in written.holes {
            // `.` in an instruction is where the instruction starts, which is what gas means by it
            // and what `mov .-4(%rip), %eax` counts back from.
            let here = (part, at as i64);
            let sum = if hole.sort == Reach::Value {
                // The number itself, with nothing taken off for where the instruction ends.
                self.expression_at(&hole.name, here)?
            } else {
                let what = if hole.name == "." {
                    What::Here { part, at: here.1 }
                } else {
                    // Written down as a name the file mentions, which is what a call to something
                    // in another object is and the only way it gets into the symbol table at all.
                    let name = self.named(&hole.name)?;
                    self.sym(&name);
                    What::Symbol(name)
                };
                Sum {
                    constant: hole.addend,
                    terms: vec![
                        Term { coeff: 1, what },
                        Term { coeff: -1, what: What::Here { part, at: end as i64 } },
                    ],
                }
            };
            self.fixups.push(Fixup {
                part,
                at: at + hole.at as u64,
                width: hole.width,
                sum,
                reach: hole.sort,
                slot,
                branch,
                jump,
                line: self.line,
            });
        }
        Ok(())
    }

    /// A name defined here, at wherever the current section has got to.
    fn label(&mut self, name: &str) -> Result<(), Trouble> {
        let at = self.at();
        let part = self.here;
        // A numbered one is a place and not a name, so each writing of it is its own entry and
        // writing the same number again is what the file is for rather than a mistake.
        let numbered = name.bytes().all(|byte| byte.is_ascii_digit());
        let held = if numbered {
            let count = self.counts.entry(name.to_owned()).or_insert(0);
            *count += 1;
            counted(name, *count)
        } else {
            name.to_owned()
        };
        let sym = self.sym(&held);
        if self.syms[sym].at != Held::Undefined {
            let what = format!("'{name}' is defined twice");
            return Err(self.bad(&what));
        }
        self.syms[sym].at = Held::In { part, offset: at };
        self.labelled.insert(part);
        Ok(())
    }

    /// The place `1b` or `2f` means, if the word is one of those.
    ///
    /// Backwards is the last writing of that number above this line and forwards is the next one
    /// below it, which is why a file can use the same number over and over and why neither spelling
    /// says anything on its own. Backwards with nothing above it is refused here. Forwards with
    /// nothing below it cannot be seen yet, so it is refused where the places are worked out.
    fn numbered(&self, word: &str) -> Result<Option<String>, Trouble> {
        let Some(number) = word.strip_suffix(['b', 'f']) else {
            return Ok(None);
        };
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok(None);
        }
        let count = self.counts.get(number).copied().unwrap_or(0);
        if word.ends_with('b') {
            if count == 0 {
                let what =
                    format!("'{word}' goes back to a '{number}:' and there is none above it");
                return Err(self.bad(&what));
            }
            return Ok(Some(counted(number, count)));
        }
        Ok(Some(counted(number, count + 1)))
    }

    /// The entry a name the file wrote means where it was written.
    ///
    /// That is the place a numbered label refers to, the current setting of a name that has been
    /// set more than once, and otherwise the name.
    fn named(&self, word: &str) -> Result<String, Trouble> {
        if let Some(place) = self.numbered(word)? {
            return Ok(place);
        }
        Ok(self.current.get(word).cloned().unwrap_or_else(|| word.to_owned()))
    }

    /// `name = value`, and `.set` and `.equ` which say the same thing.
    ///
    /// The first setting is the name itself, so that a use further up the file which reached
    /// forward to it finds it. A setting after that is a new entry, because a use written between
    /// the two means the value the name had then: gas does the same by copying the symbol when it
    /// is set again, and a file can count on it. The value is read before the new entry is made,
    /// so `x = x + 1` means the one before.
    fn assign(&mut self, name: &str, what: &str) -> Result<(), Trouble> {
        let sum = self.expression(what)?;
        let held = match self.current.get(name) {
            Some(_) => format!("{name}\u{1}={}", self.syms.len()),
            None => name.to_owned(),
        };
        let sym = self.sym(&held);
        if self.syms[sym].at != Held::Undefined {
            let what = format!("'{name}' is defined twice");
            return Err(self.bad(&what));
        }
        self.current.insert(name.to_owned(), held);
        self.sets.push((sym, sum, self.line));
        Ok(())
    }

    /// A frame rule, which says what an unwinder standing at this instruction should believe.
    ///
    /// What the rules say is the same [`CfiOp`] the compiler's own functions are described with,
    /// and the table is written from them by the same code, so a function read from text and the
    /// same function compiled straight to an object unwind the same way. The directives that say
    /// something this table has no row for, a personality routine and the rest, are passed over as
    /// they were before any of this was read, which leaves those functions described as well as a
    /// C function needs.
    fn cfi(&mut self, word: &str, args: &[String]) -> Result<(), Trouble> {
        match word {
            "cfi_startproc" => {
                if self.frame.is_some() {
                    return Err(self.bad("a '.cfi_startproc' inside another one"));
                }
                let sym = self.sym(&format!("\u{1}frame{}", self.frames.len()));
                let (part, start) = (self.here, self.at());
                self.syms[sym].at = Held::In { part, offset: start };
                // Where every function starts, which is what the table's header says: the frame
                // ends one word above the stack pointer because the call pushed a return address.
                let frame = Frame {
                    part,
                    start,
                    len: 0,
                    sym,
                    rows: Vec::new(),
                    cfa: 8,
                    remembered: Vec::new(),
                };
                self.frame = Some(frame);
                return Ok(());
            }
            "cfi_sections" => {
                self.no_unwind = !args.iter().any(|arg| arg.trim() == ".eh_frame");
                return Ok(());
            }
            "cfi_endproc"
            | "cfi_def_cfa"
            | "cfi_def_cfa_offset"
            | "cfi_adjust_cfa_offset"
            | "cfi_def_cfa_register"
            | "cfi_offset"
            | "cfi_rel_offset"
            | "cfi_restore"
            | "cfi_remember_state"
            | "cfi_restore_state" => {}
            _ => return Ok(()),
        }
        let (here, at) = (self.here, self.at());
        let line = self.line;
        let bad = |why: &str| Trouble { line, why: why.to_owned() };
        let Some(mut frame) = self.frame.take() else {
            return Err(bad("a frame rule outside '.cfi_startproc' and '.cfi_endproc'"));
        };
        if frame.part != here {
            return Err(bad("a frame rule in another section from the function it is about"));
        }
        let op = match word {
            "cfi_endproc" => {
                frame.len = at - frame.start;
                self.frames.push(frame);
                return Ok(());
            }
            "cfi_def_cfa" => {
                let [reg, offset] = self.two(args, ".cfi_def_cfa")?;
                frame.cfa = self.distance(&offset)?;
                CfiOp::DefCfa { reg: self.dwarf(&reg)?, offset: frame.cfa }
            }
            "cfi_def_cfa_offset" | "cfi_adjust_cfa_offset" => {
                let by = self.distance(args.first().map_or("", |arg| arg.as_str()))?;
                frame.cfa = if word == "cfi_def_cfa_offset" { by } else { frame.cfa + by };
                CfiOp::DefCfaOffset(frame.cfa)
            }
            "cfi_def_cfa_register" => {
                CfiOp::DefCfaRegister(self.dwarf(args.first().map_or("", |arg| arg.as_str()))?)
            }
            "cfi_offset" | "cfi_rel_offset" => {
                let [reg, offset] = self.two(args, &format!(".{word}"))?;
                let mut offset = self.distance(&offset)?;
                // Counted from the register the frame is counted from rather than from the end of
                // the frame, which is the same slot once the distance between the two is taken off.
                if word == "cfi_rel_offset" {
                    offset -= frame.cfa;
                }
                if offset >= 0 || offset % 8 != 0 {
                    return Err(bad(
                        "a register saved somewhere that is not a whole slot below the end of the \
                         frame, which is the only place this writes a rule for",
                    ));
                }
                CfiOp::Offset { reg: self.dwarf(&reg)?, offset }
            }
            "cfi_restore" => {
                CfiOp::Restore(self.dwarf(args.first().map_or("", |arg| arg.as_str()))?)
            }
            "cfi_remember_state" => {
                frame.remembered.push(frame.cfa);
                CfiOp::RememberState
            }
            "cfi_restore_state" => {
                frame.cfa = frame.remembered.pop().ok_or_else(|| {
                    bad("a '.cfi_restore_state' with nothing remembered to restore")
                })?;
                CfiOp::RestoreState
            }
            _ => unreachable!("every other word returned above"),
        };
        frame.rows.push(((at - frame.start) as usize, op));
        self.frame = Some(frame);
        Ok(())
    }

    /// A distance in a frame rule, which is a number and not negative for the end of the frame.
    fn distance(&mut self, text: &str) -> Result<i32, Trouble> {
        let value = self.number(text)?;
        i32::try_from(value).map_err(|_| self.bad(&format!("{value} is not a distance in a frame")))
    }

    /// The number DWARF gives a register a frame rule names, which a file may write either way.
    fn dwarf(&self, text: &str) -> Result<u16, Trouble> {
        let text = text.trim();
        if let Ok(number) = text.parse::<u16>() {
            return Ok(number);
        }
        let name = text.strip_prefix('%').unwrap_or(text);
        if name == "rip" {
            return Ok(SYSV.dwarf_return_address);
        }
        gpr_named(name)
            .and_then(|(reg, _)| SYSV.dwarf(SYSV.int_class, reg))
            .ok_or_else(|| self.bad(&format!("'{text}' is not a register a frame rule can name")))
    }

    /// Everything that starts with a dot.
    #[allow(clippy::too_many_lines)]
    fn directive(&mut self, word: &str, rest: &str) -> Result<(), Trouble> {
        let args = split(rest, ',');
        match word {
            "text" | "data" | "bss" | "rodata" => {
                self.plain(word, rest)?;
            }
            "section" => self.section_directive(&args)?,
            "pushsection" => {
                self.stack.push(self.here);
                self.section_directive(&args)?;
            }
            "popsection" => {
                let Some(back) = self.stack.pop() else {
                    return Err(self.bad(".popsection with nothing pushed"));
                };
                self.go(back);
            }
            "previous" => {
                let Some(back) = self.before else {
                    return Err(self.bad(".previous with no section before this one"));
                };
                self.go(back);
            }

            "byte" => self.data(&args, 1)?,
            "short" | "word" | "hword" | "value" | "2byte" => self.data(&args, 2)?,
            "long" | "int" | "4byte" => self.data(&args, 4)?,
            "quad" | "8byte" => self.data(&args, 8)?,

            "ascii" => self.text_bytes(&args, false)?,
            "asciz" | "string" => self.text_bytes(&args, true)?,

            "space" | "skip" | "zero" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(self.bad(&format!(".{word} wants a size and an optional fill")));
                }
                let size = self.number(&args[0])?;
                let size = self.count(size)?;
                let fill = match args.get(1) {
                    Some(arg) => self.byte(arg)?,
                    None => 0,
                };
                self.pad(size, fill)?;
            }
            "fill" => {
                // The middle operand is the width of one item and the last is its value, and the
                // default width is one byte, which is why `.fill 8` is eight zero bytes and not
                // eight of anything else.
                if args.is_empty() || args.len() > 3 {
                    return Err(self.bad(".fill wants a count and an optional width and value"));
                }
                let count = self.number(&args[0])?;
                let count = self.count(count)?;
                let width = match args.get(1) {
                    Some(arg) => {
                        let width = self.number(arg)?;
                        self.count(width)?
                    }
                    None => 1,
                };
                let value = match args.get(2) {
                    Some(arg) => self.number(arg)?,
                    None => 0,
                };
                if width > 8 {
                    return Err(self.bad(".fill of items wider than eight bytes is not written"));
                }
                let one = value.to_le_bytes();
                for _ in 0..count {
                    self.put(&one[..width as usize])?;
                }
            }

            "align" | "balign" | "p2align" => self.align(word, &args)?,
            "org" => {
                let Some(first) = args.first() else {
                    return Err(self.bad(".org with nothing after it"));
                };
                let to = self.number(first)?;
                let to = self.count(to)?;
                let fill = match args.get(1) {
                    Some(arg) => self.byte(arg)?,
                    None => 0,
                };
                let at = self.at();
                if to < at {
                    let what = format!(".org back to {to} from {at}, which would overwrite bytes");
                    return Err(self.bad(&what));
                }
                self.pad(to - at, fill)?;
            }

            "globl" | "global" => self.bind(&args, Binding::Global)?,
            "weak" => self.bind(&args, Binding::Weak)?,
            "local" => {
                self.bind(&args, Binding::Local)?;
                for arg in &args {
                    let sym = self.sym(arg.trim());
                    self.said_local.insert(sym);
                }
            }
            "hidden" => self.sight(&args, Visibility::Hidden)?,
            "protected" => self.sight(&args, Visibility::Protected)?,
            // Hidden and not in any dynamic table at all. Nothing this writes can say the second
            // half, and the first half is the part a link depends on.
            "internal" => self.sight(&args, Visibility::Hidden)?,

            "type" => self.type_directive(&args)?,
            "err" | "error" => {
                let what = unquoted(args.first().map_or("", |arg| arg.trim()));
                return Err(self.bad(&format!("the file says so itself: {what}")));
            }
            "size" => {
                let [name, what] = self.two(&args, ".size")?;
                let sum = self.expression(&what)?;
                let sym = self.sym(&name);
                self.sizes.push((sym, sum, self.line));
            }
            "set" | "equ" | "equiv" => {
                let [name, what] = self.two(&args, &format!(".{word}"))?;
                self.assign(&name, &what)?;
            }
            "comm" | "lcomm" => self.common(&args, word == "lcomm")?,

            // Two directives under one name. `.file "foo.c"` says what this was assembled from and
            // becomes a symbol, and `.file 1 "foo.c"` is a line table entry which says the same
            // thing to a debugger and does not. The number in front is the whole difference.
            "file" => {
                let what = args.first().map_or("", |arg| arg.trim());
                if what.starts_with('"') {
                    self.files.push(unquoted(what));
                }
            }

            // Said for a debugger or a reader and holding nothing a link depends on. Passed over
            // rather than refused, because a file that carries them is otherwise readable and
            // refusing would turn a note into a failure.
            "ident" | "loc" | "loc_mark_labels" | "version" | "arch" | "code64" | "att_syntax"
            | "intel_syntax" | "warning" => {}
            _ if word.starts_with("cfi_") => self.cfi(word, &args)?,

            _ => {
                let what = format!(
                    "'.{word}' is a directive this compiler does not know, so nothing was written \
                     for it"
                );
                return Err(self.bad(&what));
            }
        }
        Ok(())
    }

    /// `.text`, `.data`, `.bss` and `.rodata`, which name a section this already knows the flags of.
    fn plain(&mut self, word: &str, rest: &str) -> Result<(), Trouble> {
        // A number after one of these is a subsection, and gas lays the numbered ones out after the
        // unnumbered one at the end of the file rather than where they were written. Refused rather
        // than merged in place, because merging is right only for a file that never goes back to a
        // lower number and wrong silently for one that does.
        if !rest.trim().is_empty() && rest.trim() != "0" {
            let what =
                format!("'.{word} {}' is a subsection, which is not written yet", rest.trim());
            return Err(self.bad(&what));
        }
        let name = format!(".{word}");
        let shape = Shape::of(&name);
        self.section(&name, shape);
        Ok(())
    }

    /// `.section name[, "flags"[, @type]]`.
    fn section_directive(&mut self, args: &[String]) -> Result<(), Trouble> {
        let Some(name) = args.first() else {
            return Err(self.bad(".section with no name"));
        };
        let name = unquoted(name.trim());
        if name.is_empty() {
            return Err(self.bad(".section with no name"));
        }
        // No flags means the name decides, which is what makes `.section .text` the same section as
        // `.text` rather than an unallocated one that happens to share its name.
        let mut shape = Shape::of(&name);
        let (mut merge, mut strings) = (false, false);
        if let Some(flags) = args.get(1) {
            let letters = unquoted(flags.trim());
            shape = Shape { bits: true, ..Shape::default() };
            for letter in letters.chars() {
                match letter {
                    'a' => shape.alloc = true,
                    'w' => shape.write = true,
                    'x' => shape.exec = true,
                    'T' => shape.thread = true,
                    'M' => merge = true,
                    'S' => strings = true,
                    // Part of a group, and the rest. They are about what a linker may do with two
                    // copies of the section, and taking them as an ordinary section of the same
                    // bytes is correct and merely larger.
                    'G' | 'o' | 'e' | 'R' | 'd' => {}
                    _ => {
                        let what = format!("'{letter}' is not a section flag this compiler knows");
                        return Err(self.bad(&what));
                    }
                }
            }
        }
        if let Some(kind) = args.get(2) {
            let kind = kind.trim().trim_start_matches(['@', '%']);
            let kind = unquoted(kind);
            match kind.as_str() {
                "progbits" => shape.bits = true,
                "nobits" => shape.bits = false,
                "init_array" => shape.array = Some(Array::Init),
                "fini_array" => shape.array = Some(Array::Fini),
                "preinit_array" => shape.array = Some(Array::Preinit),
                "note" => shape.bits = true,
                _ => {
                    let what = format!("'{kind}' is not a section type this compiler writes");
                    return Err(self.bad(&what));
                }
            }
        }
        // How long an entry is follows the type, and a section with `M` and no length, or one this
        // cannot read, is taken as an ordinary one, which is correct and merely larger.
        if merge {
            shape.merge = args.get(3).and_then(|entry| entry.trim().parse().ok()).unwrap_or(0);
            shape.strings = strings;
        }
        self.section(&name, shape);
        Ok(())
    }

    /// Go to a section, making it if this is the first time the file has named it.
    ///
    /// The flags are taken from the first mention. A second `.section .text,"ax"` after a plain
    /// `.text` says the same thing gas already worked out, and a file that really does contradict
    /// itself is one gas warns about and keeps the first answer for.
    fn section(&mut self, name: &str, shape: Shape) {
        if let Some(&at) = self.named.get(name) {
            self.go(at);
            return;
        }
        let at = self.parts.len();
        self.parts.push(Part {
            name: name.to_owned(),
            bytes: Vec::new(),
            size: 0,
            align: 1,
            shape,
            relocs: Vec::new(),
        });
        self.named.insert(name.to_owned(), at);
        self.go(at);
    }

    /// Go to a section that exists, remembering where this came from for `.previous`.
    fn go(&mut self, at: usize) {
        if at != self.here {
            self.before = Some(self.here);
            self.here = at;
        }
    }

    /// `.byte`, `.long` and the rest, at the width each of them means.
    fn data(&mut self, args: &[String], width: u8) -> Result<(), Trouble> {
        if args.is_empty() {
            return Err(self.bad("a data directive with nothing after it"));
        }
        for arg in args {
            let sum = self.expression(arg)?;
            let at = self.at();
            if let Some(value) = sum.flat() {
                self.put(&value.to_le_bytes()[..width as usize])?;
                continue;
            }
            // A name, so the bytes are the linker's answer and not this one's. Zeroes go down to
            // hold the place, which is what the addend of the relocation is counted from.
            let part = self.here;
            if !self.parts[part].shape.bits {
                let what = format!(
                    "'{}' holds no bytes and this asks the linker to write some into it",
                    self.parts[part].name
                );
                return Err(self.bad(&what));
            }
            self.put(&vec![0u8; width as usize])?;
            self.fixups.push(Fixup {
                part,
                at,
                width,
                sum,
                reach: Reach::Near,
                slot: Reference::Got,
                branch: None,
                jump: false,
                line: self.line,
            });
        }
        Ok(())
    }

    /// `.ascii` and the two that add the terminator.
    fn text_bytes(&mut self, args: &[String], terminated: bool) -> Result<(), Trouble> {
        for arg in args {
            let mut bytes = self.string(arg.trim())?;
            if terminated {
                bytes.push(0);
            }
            self.put(&bytes)?;
        }
        Ok(())
    }

    /// `.align`, `.balign` and `.p2align`, which differ only in what the first number means.
    ///
    /// On this machine `.align` counts bytes, which is the trap: on some other machines the same
    /// directive counts bits, and a file written for one read by the other is off by a factor it
    /// never says out loud.
    fn align(&mut self, word: &str, args: &[String]) -> Result<(), Trouble> {
        let Some(head) = args.first() else {
            return Err(self.bad(&format!(".{word} with nothing after it")));
        };
        let first = self.number(head)?;
        let first = self.count(first)?;
        let boundary = if word == "p2align" {
            if first > 31 {
                return Err(self.bad(".p2align of more than two gigabytes"));
            }
            1u64 << first
        } else {
            first
        };
        if boundary == 0 || !boundary.is_power_of_two() {
            let what = format!("an alignment of {boundary}, which is not a power of two");
            return Err(self.bad(&what));
        }
        // The default filling is a no-op instruction in a section that holds instructions, because
        // what is being aligned there is the next instruction and the processor may walk into the
        // padding from the one before it.
        let exec = self.parts[self.here].shape.exec;
        let fill = match args.get(1) {
            Some(arg) if !arg.trim().is_empty() => Some(self.byte(arg)?),
            _ => None,
        };
        let at = self.at();
        // The third operand is how much padding is worth it. More than that and the alignment is
        // skipped entirely, which is how a file asks for an alignment only where it is cheap.
        let most = match args.get(2).filter(|arg| !arg.trim().is_empty()) {
            Some(most) => {
                let most = self.number(&most.clone())?;
                Some(self.count(most)?)
            }
            None => None,
        };
        let need = padding(at, boundary, most);
        self.aligns.push(Aligned { part: self.here, at, boundary, most, need });
        if need == 0 && most.is_some_and(|most| padding(at, boundary, None) > most) {
            return Ok(());
        }
        let part = &mut self.parts[self.here];
        part.align = part.align.max(boundary);
        match fill {
            Some(fill) => self.pad(need, fill),
            // Not one byte at a time, which is what gas does as well: the padding in front of a
            // loop is fallen into, and a few long nops are fewer instructions than many short ones.
            None if exec => {
                let mut bytes = Vec::new();
                nops(usize::try_from(need).unwrap_or(usize::MAX), &mut bytes);
                self.put(&bytes)
            }
            None => self.pad(need, 0),
        }
    }

    /// `.globl` and the two others that say who can see a name.
    fn bind(&mut self, args: &[String], binding: Binding) -> Result<(), Trouble> {
        for arg in args {
            let sym = self.sym(arg.trim());
            self.syms[sym].binding = binding;
        }
        Ok(())
    }

    /// `.hidden` and the rest of how far one reaches.
    fn sight(&mut self, args: &[String], visibility: Visibility) -> Result<(), Trouble> {
        for arg in args {
            let sym = self.sym(arg.trim());
            self.syms[sym].visibility = visibility;
        }
        Ok(())
    }

    /// `.type name,@function` and the other spellings of the same thing.
    fn type_directive(&mut self, args: &[String]) -> Result<(), Trouble> {
        let [name, what] = self.two(args, ".type")?;
        let what = unquoted(what.trim().trim_start_matches(['@', '%']));
        let sort = match what.trim_start_matches("STT_").to_ascii_lowercase().as_str() {
            "func" | "function" => Sort::Func,
            "object" | "gnu_unique_object" => Sort::Object,
            "tls_object" | "tls" => Sort::Thread,
            "notype" | "" => Sort::Untyped,
            other => {
                let what = format!("'{other}' is not a symbol type this compiler writes");
                return Err(self.bad(&what));
            }
        };
        let sym = self.sym(name.trim());
        self.syms[sym].sort = sort;
        Ok(())
    }

    /// `.comm` and `.lcomm`, which are two different things under names that look alike.
    ///
    /// `.comm` asks the linker for the space and lets every object that asks for the same name
    /// share one piece of it, which is what a tentative definition in C becomes. `.lcomm` asks for
    /// nothing of the kind: it puts the bytes in this file's own `.bss` under a name nothing outside
    /// can see, and two files that use it for the same name get two pieces of storage.
    fn common(&mut self, args: &[String], local: bool) -> Result<(), Trouble> {
        if !(2..=3).contains(&args.len()) {
            return Err(
                self.bad("a common directive wants a name, a size and an optional alignment")
            );
        }
        let name = args[0].trim().to_owned();
        let size = self.number(&args[1])?;
        let size = self.count(size)?;
        let align = match args.get(2) {
            Some(arg) => {
                let align = self.number(&arg.clone())?;
                self.count(align)?.max(1)
            }
            // What gas picks when nothing said: the natural boundary for something that size, up to
            // a machine word.
            None => size.next_power_of_two().clamp(1, 16),
        };
        if !align.is_power_of_two() {
            let what = format!("an alignment of {align}, which is not a power of two");
            return Err(self.bad(&what));
        }
        let sym = self.sym(&name);
        // `.local` and then `.comm` is how gcc writes a `static` variable it leaves in common, and
        // gas takes it as `.lcomm`. Taken as common it would be a global the linker merges with
        // every other file's variable of the same name.
        let local = local || self.said_local.contains(&sym);
        // Both spellings ask for storage, so both name data, and gas records that whether or not
        // the file also wrote a `.type` for it. A `.type` afterwards still overrides this, since
        // this is only what the directive itself says.
        self.syms[sym].sort = Sort::Object;
        if local {
            let was = self.here;
            self.section(".bss", Shape::of(".bss"));
            let part = &mut self.parts[self.here];
            part.align = part.align.max(align);
            let over = part.size % align;
            if over != 0 {
                part.size += align - over;
            }
            let offset = self.parts[self.here].size;
            self.parts[self.here].size += size;
            let at = self.here;
            self.syms[sym].at = Held::In { part: at, offset };
            self.syms[sym].size = size;
            self.syms[sym].binding = Binding::Local;
            self.go(was);
        } else {
            self.syms[sym].at = Held::Common { size, align };
            self.syms[sym].size = size;
            self.syms[sym].binding = Binding::Global;
        }
        Ok(())
    }

    /// How far into the current section the file has got.
    fn at(&self) -> u64 {
        let part = &self.parts[self.here];
        if part.shape.bits { part.bytes.len() as u64 } else { part.size }
    }

    /// Bytes into the current section.
    fn put(&mut self, bytes: &[u8]) -> Result<(), Trouble> {
        let part = &mut self.parts[self.here];
        if !part.shape.bits {
            if bytes.iter().all(|byte| *byte == 0) {
                // A run of zeroes is exactly what such a section holds, so asking for one is not a
                // mistake and there is nothing to write down but the length.
                part.size += bytes.len() as u64;
                return Ok(());
            }
            let what = format!("'{}' holds no bytes and this puts some in it", part.name);
            return Err(Trouble { line: self.line, why: what });
        }
        part.bytes.extend_from_slice(bytes);
        part.size = part.bytes.len() as u64;
        Ok(())
    }

    /// That many copies of one byte.
    fn pad(&mut self, count: u64, fill: u8) -> Result<(), Trouble> {
        let part = &mut self.parts[self.here];
        if !part.shape.bits {
            part.size += count;
            return Ok(());
        }
        part.bytes.resize(part.bytes.len() + usize::try_from(count).unwrap_or(usize::MAX), fill);
        part.size = part.bytes.len() as u64;
        Ok(())
    }

    /// The index of a name, making the entry if this is the first time the file has said it.
    fn sym(&mut self, name: &str) -> usize {
        if let Some(&at) = self.known.get(name) {
            return at;
        }
        let at = self.syms.len();
        self.syms.push(Sym {
            name: name.to_owned(),
            at: Held::Undefined,
            size: 0,
            sort: Sort::Untyped,
            // Local until something says otherwise, which is what a plain label is. A name that
            // turns out to be undefined is made global at the end, since a local one the linker is
            // asked to find is a contradiction.
            binding: Binding::Local,
            visibility: Visibility::Default,
            // Read off the name, since the one byte no source file can write is exactly what says
            // this entry came from a numbered local label rather than from something a file named.
            numbered: name.contains('\u{1}'),
        });
        self.known.insert(name.to_owned(), at);
        at
    }

    /// Two operands, said the same way wherever a directive wants exactly two.
    fn two(&self, args: &[String], what: &str) -> Result<[String; 2], Trouble> {
        if args.len() != 2 {
            let why = format!("{what} wants two operands and was given {}", args.len());
            return Err(Trouble { line: self.line, why });
        }
        Ok([args[0].trim().to_owned(), args[1].trim().to_owned()])
    }

    /// An expression whose value has to be known now rather than at the end.
    fn number(&mut self, text: &str) -> Result<i64, Trouble> {
        let sum = self.expression(text)?;
        sum.flat().ok_or_else(|| Trouble {
            line: self.line,
            why: format!("'{}' has to be a number here and it names something", text.trim()),
        })
    }

    /// One of those that has to fit in a byte.
    fn byte(&mut self, text: &str) -> Result<u8, Trouble> {
        let value = self.number(text)?;
        u8::try_from(value & 0xff).map_err(|_| Trouble {
            line: self.line,
            why: format!("{value} does not fit in a byte"),
        })
    }

    /// One of those that has to be a length rather than a negative number.
    fn count(&self, value: i64) -> Result<u64, Trouble> {
        u64::try_from(value).map_err(|_| Trouble {
            line: self.line,
            why: format!("{value} is negative and this is a length"),
        })
    }

    /// Parse one, with `.` meaning where the file has got to.
    fn expression(&mut self, text: &str) -> Result<Sum, Trouble> {
        self.expression_at(text, (self.here, self.at() as i64))
    }

    /// The same, with `.` meaning `here`.
    fn expression_at(&mut self, text: &str, here: (usize, i64)) -> Result<Sum, Trouble> {
        let mut parser = Parser { text: text.trim(), at: 0, here };
        let mut sum = parser.whole().map_err(|why| Trouble { line: self.line, why })?;
        // Every name it mentioned gets a symbol table entry, so that a relocation against one has
        // something to point at and so that an undefined one is asked of the linker.
        for term in &mut sum.terms {
            if let What::Symbol(name) = &term.what {
                let name = self.named(name)?;
                self.sym(&name);
                term.what = What::Symbol(name);
            }
        }
        Ok(sum)
    }

    /// A message about this line.
    fn bad(&self, why: &str) -> Trouble {
        Trouble { line: self.line, why: why.to_owned() }
    }

    /// Work out everything that was waiting for the end of the file.
    ///
    /// Or the branches written short that do not reach, when there are any, for the file to be read
    /// again with those long.
    fn finish(mut self) -> Result<Result<Assembled, Vec<usize>>, Trouble> {
        if self.frame.is_some() {
            return Err(self.bad("a '.cfi_startproc' that is never ended"));
        }
        self.unwind_table();
        self.resolve_sets()?;
        self.resolve_sizes()?;
        let grow = self.too_far()?;
        if !grow.is_empty() {
            return Ok(Err(grow));
        }
        self.resolve_fixups()?;
        // A section the file only ever mentioned is dropped, so that a `.section` in a macro that
        // turned out to be unused does not put an empty header in the object. `.text` at the top is
        // the common case of one.
        let keep: Vec<bool> = self
            .parts
            .iter()
            .enumerate()
            .map(|(at, part)| {
                part.size > 0 || !part.relocs.is_empty() || self.labelled.contains(&at)
            })
            .collect();
        let mut moved = vec![0usize; self.parts.len()];
        let mut parts = Vec::with_capacity(self.parts.len());
        for (at, part) in self.parts.into_iter().enumerate() {
            if keep[at] {
                moved[at] = parts.len();
                parts.push(part);
            }
        }
        let mut names = Vec::with_capacity(self.syms.len() + self.files.len());
        // In front, which is where gas puts them and where a reader expects the name of the file to
        // be before anything that is in it.
        for file in self.files {
            names.push(Name {
                name: file,
                at: Held::Absolute(0),
                size: 0,
                sort: Sort::File,
                binding: Binding::Local,
                visibility: Visibility::Default,
            });
        }
        for (index, sym) in self.syms.into_iter().enumerate() {
            // A numbered local label is a place and not a name. Everything that went to one has been
            // resolved to a number in the bytes by now, and gas writes no symbol for one either, so
            // an object this assembles has the same table as an object gas assembles from the same
            // file rather than a table with a made up name in it.
            if sym.numbered && !self.relocated.contains(&index) {
                continue;
            }
            let at = match sym.at {
                Held::In { part, offset } => Held::In { part: moved[part], offset },
                other => other,
            };
            let binding = match (at, sym.binding) {
                (Held::Undefined, Binding::Local) => Binding::Global,
                (_, binding) => binding,
            };
            names.push(Name {
                name: sym.name,
                at,
                size: sym.size,
                sort: sym.sort,
                binding,
                visibility: sym.visibility,
            });
        }
        Ok(Ok(Assembled { parts, names }))
    }

    /// The unwind table the frame rules describe, as a section of its own.
    ///
    /// Written only when a file said some rules, which is every function the compiler emits and
    /// every function gcc does. A file of assembly written by hand with none gets no table, the same
    /// as it does from gas.
    fn unwind_table(&mut self) {
        if self.frames.is_empty() || self.no_unwind {
            return;
        }
        let funcs: Vec<Extent> = self
            .frames
            .iter()
            .map(|frame| Extent {
                name: self.syms[frame.sym].name.clone(),
                start: frame.start as usize,
                len: frame.len as usize,
                align: 1,
                binding: Binding::Local,
                visibility: Visibility::Default,
                patch: None,
            })
            .collect();
        let rows: Vec<_> = self.frames.iter().map(|frame| frame.rows.clone()).collect();
        let Ok(table) = crate::unwind::table(&funcs, &rows, &SYSV, ObjectFormat::Elf) else {
            return;
        };
        for frame in &self.frames {
            self.relocated.insert(frame.sym);
        }
        let size = table.bytes.len() as u64;
        self.parts.push(Part {
            name: ".eh_frame".to_owned(),
            bytes: table.bytes,
            size,
            align: 8,
            shape: Shape { alloc: true, bits: true, ..Shape::default() },
            relocs: table.relocs,
        });
    }

    /// `.set` and its spellings, which may name each other and so are worked at until they stop
    /// moving rather than in the order they were written.
    fn resolve_sets(&mut self) -> Result<(), Trouble> {
        while !self.sets.is_empty() {
            let mut done = Vec::new();
            for (at, (sym, sum, line)) in self.sets.iter().enumerate() {
                if let Ok(residue) = self.reduce(sum) {
                    done.push((at, *sym, self.settled(&residue, *line)?));
                }
            }
            if done.is_empty() {
                let (sym, _, line) = &self.sets[0];
                let why = format!(
                    "'{}' is set to something that is set to it, so neither has a value",
                    self.syms[*sym].name
                );
                return Err(Trouble { line: *line, why });
            }
            for (_, sym, held) in &done {
                self.syms[*sym].at = *held;
            }
            // Backwards, so that removing one does not move the next one out from under its index.
            for (at, _, _) in done.iter().rev() {
                self.sets.remove(*at);
            }
        }
        Ok(())
    }

    /// What one `.set` came out as.
    fn settled(&self, residue: &Residue, line: usize) -> Result<Held, Trouble> {
        match residue.left.as_slice() {
            [] => Ok(Held::Absolute(residue.constant as u64)),
            // `.set alias, real`, which is how a file gives something a second name without a
            // second copy of it. The two end up at the same place in the same section.
            [Left { coeff: 1, at: Some((part, offset)), .. }] => {
                Ok(Held::In { part: *part, offset: (*offset + residue.constant) as u64 })
            }
            _ => Err(Trouble {
                line,
                why: "a set to something that is neither a number nor a place in this file"
                    .to_owned(),
            }),
        }
    }

    /// `.size`, which has to come out as a number because that is what ELF records.
    fn resolve_sizes(&mut self) -> Result<(), Trouble> {
        for (sym, sum, line) in std::mem::take(&mut self.sizes) {
            let residue = self.reduce(&sum).map_err(|why| Trouble { line, why })?;
            if !residue.left.is_empty() {
                let why = format!(
                    "the size of '{}' is not a number, and a size has to be one",
                    self.syms[sym].name
                );
                return Err(Trouble { line, why });
            }
            let size = self.count(residue.constant).map_err(|_| Trouble {
                line,
                why: format!("'{}' is given a negative size", self.syms[sym].name),
            })?;
            self.syms[sym].size = size;
        }
        Ok(())
    }

    /// The places whose bytes name something.
    /// The branches written in two bytes that two bytes do not reach.
    ///
    /// That is one whose distance is not a number in this section, or goes to a weak name, which
    /// another object may replace and so is a relocation wherever it is defined, or is a number past
    /// a signed byte. The first two are long whatever the layout is, and when there are any they
    /// are the only ones grown on this pass. gas makes them long before it lays anything out, and a
    /// jump grown by three bytes moves the padding behind it, so judging the distances of the
    /// others before that has happened would grow some that gas leaves short.
    ///
    /// The rest are judged the way gas judges them, which is not quite by the distances this pass
    /// laid out. gas walks a section in order and keeps count of how far what it has grown so far
    /// has pushed everything behind it, and an alignment takes some of that back by padding less.
    /// A jump back is judged by where its target has already moved to. A jump forward to somewhere
    /// past an alignment is judged as though the alignment will take up all the growth in front of
    /// it, and one to somewhere before the next alignment as though the target moves with it. The
    /// first of those is a guess, and it matters: guessing the other way grows jumps that gas
    /// leaves short, and each one grown moves the padding behind it and the file comes out
    /// different. Whatever is guessed wrong is put right on the next pass, as it is in gas.
    fn too_far(&self) -> Result<Vec<usize>, Trouble> {
        let mut away = Vec::new();
        // Where each jump ends, how far it goes, and which it is.
        let mut jumps: Vec<(usize, i64, i64, usize)> = Vec::new();
        for fixup in &self.fixups {
            let Some(nth) = fixup.branch else { continue };
            let residue = self
                .reduce_kept(&fixup.sum, true)
                .map_err(|why| Trouble { line: fixup.line, why })?;
            if !residue.left.is_empty() {
                away.push(nth);
            } else {
                jumps.push((fixup.part, fixup.at as i64 + 1, residue.constant, nth));
            }
        }
        if !away.is_empty() {
            return Ok(away);
        }
        jumps.sort_unstable();
        let mut far = Vec::new();
        let mut jumps = jumps.into_iter().peekable();
        while let Some(&(part, ..)) = jumps.peek() {
            let aligns: Vec<Aligned> =
                self.aligns.iter().filter(|align| align.part == part).copied().collect();
            let mut aligns_left = aligns.iter().peekable();
            let mut stretch = 0i64;
            // How far everything from each place on has moved, in order, for a jump back to read.
            let mut moved: Vec<(i64, i64)> = Vec::new();
            while let Some(&(_, end, distance, nth)) = jumps.peek().filter(|jump| jump.0 == part) {
                jumps.next();
                while let Some(align) = aligns_left.next_if(|align| align.at as i64 <= end - 2) {
                    let now =
                        padding((align.at as i64 + stretch) as u64, align.boundary, align.most);
                    stretch += now as i64 - align.need as i64;
                    moved.push(((align.at + align.need) as i64, stretch));
                }
                let target = end + distance;
                let judged = if distance < 0 {
                    let there = moved.iter().rev().find(|(from, _)| *from <= target);
                    distance + there.map_or(0, |(_, by)| *by) - stretch
                } else if stretch > 0
                    && aligns.iter().any(|align| {
                        end <= align.at as i64 && (align.at + align.need) as i64 <= target
                    })
                {
                    distance - stretch
                } else {
                    distance
                };
                // A target forward that the guess puts behind the jump is a guess gone wrong, and
                // gas leaves the jump as it is for this pass rather than grow it on the strength
                // of one.
                if distance >= 0 && judged < -2 {
                    continue;
                }
                if i8::try_from(judged).is_err() {
                    far.push(nth);
                    stretch += if self.parts[part].bytes[end as usize - 2] == 0xEB { 3 } else { 4 };
                    moved.push((end, stretch));
                }
            }
        }
        Ok(far)
    }

    fn resolve_fixups(&mut self) -> Result<(), Trouble> {
        for fixup in std::mem::take(&mut self.fixups) {
            let line = fixup.line;
            let bad = |why: String| Trouble { line, why };
            // A name reached through the global offset table, or through the one entry of it a
            // thread-local variable has, is a relocation whatever else is true of it. What goes in
            // the bytes is the distance to a word the linker makes, and the linker only knows where
            // it put that word, so working the sum out here would answer a different question. The
            // sum is the one the instruction made two paragraphs up, which is the name minus the
            // end of the instruction, so the addend comes out the way it does for every other
            // rip-relative reference and is minus four.
            if matches!(fixup.reach, Reach::Table | Reach::Thread) {
                let [
                    Term { coeff: 1, what: What::Symbol(name) },
                    Term { coeff: -1, what: What::Here { at: end, .. } },
                ] = fixup.sum.terms.as_slice()
                else {
                    return Err(bad(
                        "a reach through the global offset table in something other than an \
                         instruction, which is not an expression this compiler writes"
                            .to_owned(),
                    ));
                };
                let kind = if fixup.reach == Reach::Table { fixup.slot } else { Reference::Thread };
                self.parts[fixup.part].relocs.push(Reloc {
                    at: fixup.at as usize,
                    symbol: name.clone(),
                    kind,
                    addend: fixup.sum.constant + fixup.at as i64 - end,
                    after: (end - fixup.at as i64 - 4).max(0) as u8,
                });
                continue;
            }
            let residue =
                self.reduce_kept(&fixup.sum, fixup.jump).map_err(|why| Trouble { line, why })?;
            if fixup.reach == Reach::Value && !residue.left.is_empty() {
                return Err(bad(
                    "a number in an instruction that names something outside this section, \
                     which wants a relocation this compiler does not write yet"
                        .to_owned(),
                ));
            }
            let (symbol, kind, addend, after) = match residue.left.as_slice() {
                [] => {
                    // A distance a branch carries is signed and nothing else, so a byte of it
                    // reaches a hundred and twenty seven forwards and a hundred and twenty eight
                    // back. A number a directive writes down is counted both ways, because a byte
                    // holds two hundred and fifty five as well as minus one and a file writing
                    // either means it. Either way what does not fit is refused: a branch out of
                    // reach cut down to its low byte goes somewhere nobody wrote, and so does a
                    // table of offsets whose entries were quietly truncated.
                    let width = fixup.width as usize;
                    let room = 8 * width as u32;
                    let low = -(1i64 << (room - 1));
                    let high = if fixup.reach == Reach::Branch {
                        (1i64 << (room - 1)) - 1
                    } else {
                        (1i64 << room) - 1
                    };
                    if width < 8 && (residue.constant < low || residue.constant > high) {
                        return Err(bad(format!(
                            "{} written into {width} bytes, which does not reach it",
                            residue.constant
                        )));
                    }
                    let bytes = residue.constant.to_le_bytes();
                    let at = fixup.at as usize;
                    let part = &mut self.parts[fixup.part];
                    part.bytes[at..at + width].copy_from_slice(&bytes[..width]);
                    continue;
                }
                // The address of something, which is the whole of what a table of pointers holds.
                [Left { coeff: 1, what: What::Symbol(name), .. }] => {
                    let kind = Reference::Address { bytes: fixup.width };
                    (name.clone(), kind, residue.constant, 0)
                }
                // The distance from these bytes to something, which is what a position independent
                // table of offsets holds and what `.long foo - .` is asking for. The subtracted
                // side has to be these bytes or somewhere else in the same section, because a
                // distance to another section is not a number until the linker has laid both out.
                [
                    Left { coeff: 1, what: What::Symbol(name), .. },
                    Left { coeff: -1, at: Some((part, offset)), .. },
                ]
                | [
                    Left { coeff: -1, at: Some((part, offset)), .. },
                    Left { coeff: 1, what: What::Symbol(name), .. },
                ] => {
                    if *part != fixup.part {
                        return Err(bad(
                            "a distance that is subtracted from somewhere in another section"
                                .to_owned(),
                        ));
                    }
                    if fixup.width != 4 {
                        return Err(bad(format!(
                            "a distance written into {} bytes, and four is the only width a \
                             relocation says one at",
                            fixup.width
                        )));
                    }
                    // A linker writes `symbol + addend - here`, and what was asked for is
                    // `symbol + constant - there`, so the addend is the constant plus however far
                    // these bytes are past the place the distance is counted from. That is zero
                    // for `.long foo - .`, which is why the two are easy to write down the wrong
                    // way round, and it is minus four for a call, whose four bytes are counted
                    // from the end of the instruction they are the last of.
                    let addend = residue.constant + fixup.at as i64 - offset;
                    // A static name defined here needs no stub whichever section it is in, and
                    // gas says so by asking for the plain distance to it rather than a call.
                    let near = self.known.get(name).is_some_and(|&sym| {
                        self.syms[sym].binding == Binding::Local
                            && matches!(self.syms[sym].at, Held::In { .. })
                    });
                    let kind = if fixup.reach == Reach::Branch && !near {
                        Reference::Call
                    } else {
                        Reference::Data
                    };
                    // The same distance said the other way, for the format that wants it apart
                    // from the addend rather than folded into it. See `rucc_object::Reloc`.
                    let after = (offset - fixup.at as i64 - 4).max(0);
                    (name.clone(), kind, addend, after as u8)
                }
                [Left { coeff: 1, what: What::Here { .. }, .. }] => {
                    return Err(bad(
                        "the address of these bytes themselves, which has no symbol to be \
                         relocated against"
                            .to_owned(),
                    ));
                }
                _ => {
                    return Err(bad(
                        "an expression that does not come out as a number, an address, or a \
                         distance, and those are what a relocation can say"
                            .to_owned(),
                    ));
                }
            };
            // A numbered local label that got this far was never written, which for `1f` is the one
            // way of getting it wrong that nothing above can see: the file said go to the next `1:`
            // and there was no next one. It is not a name, so there is nothing to ask the linker.
            if let Some(&sym) = self.known.get(&symbol) {
                if self.syms[sym].numbered && self.syms[sym].at != Held::Undefined {
                    self.relocated.insert(sym);
                } else if self.syms[sym].numbered {
                    let number = symbol.split('\u{1}').next().unwrap_or(&symbol);
                    return Err(bad(format!(
                        "'{number}f' goes on to a '{number}:' and there is none below it"
                    )));
                }
            }
            if matches!(kind, Reference::Address { bytes } if bytes != 4 && bytes != 8) {
                return Err(bad(format!(
                    "the address of '{symbol}' written into {} bytes, and this machine relocates \
                     an address at four or eight",
                    fixup.width
                )));
            }
            self.parts[fixup.part].relocs.push(Reloc {
                at: fixup.at as usize,
                symbol,
                kind,
                addend,
                after,
            });
        }
        Ok(())
    }

    /// The same as `reduce`, except that a weak name defined here is left for the linker, and so
    /// is a global one unless this is a jump.
    ///
    /// Another object can put its own definition in front of one of those, a weak one by being
    /// strong and a global one by being in the executable when this is a shared library, so a
    /// place that reaches it is a relocation even though the distance is known here. That is what
    /// gas does for a call and a `lea`. A jump to a global name gas judges the way it judges one to
    /// a label and works out, and only a weak name makes it long and a relocation. It only holds
    /// when the name is the one thing counted from, since `f - g` is a distance whichever `f` the
    /// linker picks and gas works that out too.
    fn reduce_kept(&self, sum: &Sum, jump: bool) -> Result<Residue, String> {
        let mut named =
            sum.terms.iter().enumerate().filter(|(_, term)| matches!(term.what, What::Symbol(_)));
        let (Some((nth, Term { coeff: 1, what: What::Symbol(name) })), None) =
            (named.next(), named.next())
        else {
            return self.reduce(sum);
        };
        let kept = self.known.get(name).is_some_and(|&sym| {
            (self.syms[sym].binding == Binding::Weak
                || !jump && self.syms[sym].binding == Binding::Global)
                && matches!(self.syms[sym].at, Held::In { .. })
        });
        if !kept {
            return self.reduce(sum);
        }
        let mut rest = sum.clone();
        rest.terms.remove(nth);
        let mut residue = self.reduce(&rest)?;
        residue.left.push(Left { coeff: 1, what: What::Symbol(name.clone()), at: None });
        Ok(residue)
    }

    /// Take an expression down to a constant and whatever names would not cancel.
    ///
    /// The algebra is the ordinary one and worth saying once. A sum of terms over the same section
    /// is `sum(c * x)`, every `x` is that section's address plus a known offset, and the section's
    /// address is the only unknown in it. Rewriting each term as its distance from one chosen term
    /// in the group leaves `sum(c * (offset - chosen))`, which is a number, plus `sum(c)` times the
    /// chosen one. So a group whose coefficients add to zero disappears into the constant however
    /// many terms it had, which is what makes `.-foo` a number.
    fn reduce(&self, sum: &Sum) -> Result<Residue, String> {
        let mut constant = sum.constant;
        let mut placed: BTreeMap<usize, Vec<(i64, What, i64)>> = BTreeMap::new();
        let mut outside: Vec<(i64, String)> = Vec::new();
        for term in &sum.terms {
            match &term.what {
                What::Here { part, at } => {
                    placed.entry(*part).or_default().push((term.coeff, term.what.clone(), *at));
                }
                What::Symbol(name) => {
                    let Some(&at) = self.known.get(name) else {
                        return Err(format!("'{name}' is named and never said"));
                    };
                    match self.syms[at].at {
                        Held::Absolute(value) => constant += term.coeff * value as i64,
                        Held::In { part, offset } => placed.entry(part).or_default().push((
                            term.coeff,
                            term.what.clone(),
                            offset as i64,
                        )),
                        // Not defined here and not a place here, so nothing about it cancels with
                        // anything and the linker is the one that knows.
                        Held::Undefined | Held::Common { .. } => {
                            if !self.sets.iter().any(|(sym, _, _)| *sym == at) {
                                outside.push((term.coeff, name.clone()));
                            } else {
                                return Err(format!("'{name}' is not worked out yet"));
                            }
                        }
                    }
                }
            }
        }
        let mut left: Vec<Left> = Vec::new();
        for (part, terms) in placed {
            let (_, chosen, base) = terms[0].clone();
            let mut net = 0;
            for (coeff, _, offset) in &terms {
                net += coeff;
                constant += coeff * (offset - base);
            }
            if net != 0 {
                left.push(Left { coeff: net, what: chosen, at: Some((part, base)) });
            }
        }
        let mut together: BTreeMap<String, i64> = BTreeMap::new();
        for (coeff, name) in outside {
            *together.entry(name).or_default() += coeff;
        }
        for (name, coeff) in together {
            if coeff != 0 {
                left.push(Left { coeff, what: What::Symbol(name), at: None });
            }
        }
        Ok(Residue { constant, left })
    }
}

/// What an expression came out as: a number, and the names that would not cancel.
#[derive(Debug, Clone)]
struct Residue {
    constant: i64,
    left: Vec<Left>,
}

/// One name an expression would not get rid of.
#[derive(Debug, Clone)]
struct Left {
    /// How many times it is counted, which is one for everything a relocation can say.
    coeff: i64,
    /// Which name it is, which is what a relocation points at.
    what: What,
    /// Which section it is in and how far into it, when this file is the one that knows. Nothing
    /// for a name the linker has to find, which has no place here to be at.
    at: Option<(usize, i64)>,
}

/// An expression, kept as a sum so that it survives until the names in it have values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Sum {
    constant: i64,
    terms: Vec<Term>,
}

/// One name in one, and how many times it is counted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Term {
    coeff: i64,
    what: What,
}

/// What a term is about.
#[derive(Debug, Clone, PartialEq, Eq)]
enum What {
    /// A name, which may or may not turn out to be in this file.
    Symbol(String),
    /// `.`, which is a place and never a name. Worked out as the expression is parsed, because it
    /// means where the file had got to when it was written and not where it got to in the end.
    Here { part: usize, at: i64 },
}

impl Sum {
    /// A plain number, and nothing for one that names something.
    fn flat(&self) -> Option<i64> {
        self.terms.is_empty().then_some(self.constant)
    }

    /// One name on its own.
    fn of(what: What) -> Sum {
        Sum { constant: 0, terms: vec![Term { coeff: 1, what }] }
    }

    /// A number on its own.
    fn just(value: i64) -> Sum {
        Sum { constant: value, terms: Vec::new() }
    }

    /// Two of them added, which is the one operation that always works.
    fn plus(mut self, other: Sum) -> Sum {
        self.constant = self.constant.wrapping_add(other.constant);
        self.terms.extend(other.terms);
        self
    }

    /// One of them counted backwards.
    fn minus(self) -> Sum {
        Sum {
            constant: self.constant.wrapping_neg(),
            terms: self
                .terms
                .into_iter()
                .map(|term| Term { coeff: term.coeff.wrapping_neg(), what: term.what })
                .collect(),
        }
    }

    /// One of them counted a number of times, which only means anything when the number is one.
    fn times(self, factor: i64) -> Sum {
        Sum {
            constant: self.constant.wrapping_mul(factor),
            terms: self
                .terms
                .into_iter()
                .map(|term| Term { coeff: term.coeff.wrapping_mul(factor), what: term.what })
                .collect(),
        }
    }
}

/// One expression, being read.
struct Parser<'a> {
    text: &'a str,
    at: usize,
    here: (usize, i64),
}

impl Parser<'_> {
    /// The whole of it, and nothing left over.
    fn whole(&mut self) -> Result<Sum, String> {
        let sum = self.bitwise()?;
        self.space();
        if self.at < self.text.len() {
            return Err(format!(
                "'{}' is left over at the end of an expression",
                &self.text[self.at..]
            ));
        }
        Ok(sum)
    }

    /// The loosest binding of them, which is why it is the outermost.
    fn bitwise(&mut self) -> Result<Sum, String> {
        let mut left = self.shift()?;
        loop {
            self.space();
            let Some(op) = self.one_of(&["|", "^", "&"]) else { return Ok(left) };
            let right = self.shift()?;
            left = self.arithmetic(left, right, op)?;
        }
    }

    /// Shifts, which bind tighter than the bitwise operators and looser than addition.
    fn shift(&mut self) -> Result<Sum, String> {
        let mut left = self.sum()?;
        loop {
            self.space();
            let Some(op) = self.one_of(&["<<", ">>"]) else { return Ok(left) };
            let right = self.sum()?;
            left = self.arithmetic(left, right, op)?;
        }
    }

    /// Addition and subtraction, which are the two that keep working when names are involved.
    fn sum(&mut self) -> Result<Sum, String> {
        let mut left = self.product()?;
        loop {
            self.space();
            // Not the start of `<<` or `>>`, and not a `-` that belongs to nothing.
            let Some(op) = self.one_of(&["+", "-"]) else { return Ok(left) };
            let right = self.product()?;
            left = if op == "+" { left.plus(right) } else { left.plus(right.minus()) };
        }
    }

    /// Multiplication and the two that go with it.
    fn product(&mut self) -> Result<Sum, String> {
        let mut left = self.unary()?;
        loop {
            self.space();
            let Some(op) = self.one_of(&["*", "/", "%"]) else { return Ok(left) };
            let right = self.unary()?;
            // A name times a number is still a name counted that many times, which is worth keeping
            // because `foo*2 - foo` is a thing a macro produces. Everything else here wants two
            // numbers, and a name in one of them is a mistake rather than something to guess at.
            left = match (op, left.flat(), right.flat()) {
                ("*", _, Some(factor)) => left.times(factor),
                ("*", Some(factor), _) => right.times(factor),
                (_, Some(a), Some(b)) => Sum::just(self.arithmetic_number(a, b, op)?),
                _ => return Err(format!("'{op}' of something that names a symbol")),
            };
        }
    }

    /// A sign or a complement in front of something.
    fn unary(&mut self) -> Result<Sum, String> {
        self.space();
        if self.eat("-") {
            return Ok(self.unary()?.minus());
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("~") {
            let inner = self.unary()?;
            let value = inner
                .flat()
                .ok_or_else(|| "a complement of something that names a symbol".to_owned())?;
            return Ok(Sum::just(!value));
        }
        if self.eat("!") {
            let inner = self.unary()?;
            let value = inner
                .flat()
                .ok_or_else(|| "a negation of something that names a symbol".to_owned())?;
            return Ok(Sum::just(i64::from(value == 0)));
        }
        self.primary()
    }

    /// A number, a name, a character, `.`, or the whole thing again in brackets.
    fn primary(&mut self) -> Result<Sum, String> {
        self.space();
        let rest = &self.text[self.at..];
        if rest.is_empty() {
            return Err("an expression that stops before it says anything".to_owned());
        }
        if self.eat("(") {
            let inner = self.bitwise()?;
            self.space();
            if !self.eat(")") {
                return Err("a bracket that was opened and never closed".to_owned());
            }
            return Ok(inner);
        }
        let first = rest.as_bytes()[0];
        if first == b'\'' {
            return self.character();
        }
        if first.is_ascii_digit() {
            // `1b` and `2f`, which are a numbered label above and below rather than a number.
            // Told apart from `0b1010` by what comes after the letter, which ends a label and
            // carries on a binary number.
            let end = rest.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(rest.len());
            let bytes = rest.as_bytes();
            if matches!(bytes.get(end), Some(b'b' | b'f'))
                && !bytes.get(end + 1).is_some_and(|byte| carries_on(*byte))
            {
                self.at += end + 1;
                return Ok(Sum::of(What::Symbol(rest[..=end].to_owned())));
            }
            return self.digits();
        }
        if starts(first) {
            let name = self.word();
            // `.` on its own is where the file has got to, and `.L1` is a name that starts with one.
            if name == "." {
                let (part, at) = self.here;
                return Ok(Sum::of(What::Here { part, at }));
            }
            // What follows an `@` says which table the linker should reach the name through, and
            // none of them is a thing a directive can hold, so one here is a file that wants the
            // instruction assembler rather than this.
            if self.text[self.at..].starts_with('@') {
                return Err(format!(
                    "'{name}@' asks for a relocation only an instruction can carry"
                ));
            }
            return Ok(Sum::of(What::Symbol(name)));
        }
        Err(format!("'{rest}' is not the start of an expression"))
    }

    /// A number in any of the bases a file may write one in.
    fn digits(&mut self) -> Result<Sum, String> {
        let rest = &self.text[self.at..];
        let (radix, skip) = if rest.starts_with("0x") || rest.starts_with("0X") {
            (16, 2)
        } else if rest.starts_with("0b") || rest.starts_with("0B") {
            (2, 2)
        } else if rest.len() > 1 && rest.starts_with('0') {
            (8, 1)
        } else {
            (10, 0)
        };
        let body = &rest[skip..];
        let end = body.find(|ch: char| !ch.is_digit(radix) && ch != '_').unwrap_or(body.len());
        if end == 0 {
            return Err(format!("'{rest}' starts like a number and is not one"));
        }
        let text: String = body[..end].chars().filter(|ch| *ch != '_').collect();
        // Wrapping round rather than refusing, because a file writes `0xffffffffffffffff` for a word
        // of ones and means the bits rather than the value.
        let value = u64::from_str_radix(&text, radix)
            .map_err(|_| format!("'{text}' does not fit in sixty four bits"))?;
        self.at += skip + end;
        // A suffix, which a file written for more than one assembler carries and which says nothing
        // this needs: the width is the directive's business here.
        while self.text[self.at..].starts_with(['u', 'U', 'l', 'L']) {
            self.at += 1;
        }
        Ok(Sum::just(value as i64))
    }

    /// `'a'` or `'a`, which are both a character and both what gas takes.
    fn character(&mut self) -> Result<Sum, String> {
        self.at += 1;
        let rest = &self.text[self.at..];
        let mut chars = rest.chars();
        let Some(first) = chars.next() else {
            return Err("a quote with no character after it".to_owned());
        };
        let (value, used) = if first == '\\' {
            let (value, used) = escape(&rest[1..])?;
            (value, used + 1)
        } else {
            (first as u8, first.len_utf8())
        };
        self.at += used;
        // The closing quote is optional in gas and a file written by hand often leaves it out, so
        // one is taken when it is there and not asked for when it is not.
        if self.text[self.at..].starts_with('\'') {
            self.at += 1;
        }
        Ok(Sum::just(i64::from(value)))
    }

    /// An operator on two things that both have to be numbers.
    fn arithmetic(&self, left: Sum, right: Sum, op: &str) -> Result<Sum, String> {
        let (Some(a), Some(b)) = (left.flat(), right.flat()) else {
            return Err(format!("'{op}' of something that names a symbol"));
        };
        Ok(Sum::just(self.arithmetic_number(a, b, op)?))
    }

    /// The same, once both are numbers.
    fn arithmetic_number(&self, a: i64, b: i64, op: &str) -> Result<i64, String> {
        Ok(match op {
            "|" => a | b,
            "^" => a ^ b,
            "&" => a & b,
            "<<" => a.wrapping_shl(shift(b)?),
            ">>" => a.wrapping_shr(shift(b)?),
            "*" => a.wrapping_mul(b),
            "/" if b == 0 => return Err("a division by zero".to_owned()),
            "%" if b == 0 => return Err("a remainder of a division by zero".to_owned()),
            "/" => a.wrapping_div(b),
            "%" => a.wrapping_rem(b),
            _ => return Err(format!("'{op}' is not an operator this compiler knows")),
        })
    }

    /// One name, as far as it runs.
    fn word(&mut self) -> String {
        let body = &self.text[self.at..];
        let end = body.find(|ch: char| !carries_on(ch as u8)).unwrap_or(body.len());
        let word = body[..end].to_owned();
        self.at += end;
        word
    }

    /// Whichever of these is next, and nothing if none of them is.
    ///
    /// In the order given, which matters: `<<` has to be looked for in front of anything that starts
    /// with `<`, or the second half of it is left behind as an operator of its own.
    fn one_of(&mut self, ops: &[&'static str]) -> Option<&'static str> {
        for op in ops {
            if self.text[self.at..].starts_with(op) {
                self.at += op.len();
                return Some(op);
            }
        }
        None
    }

    /// One exact string, if it is next.
    fn eat(&mut self, what: &str) -> bool {
        if self.text[self.at..].starts_with(what) {
            self.at += what.len();
            return true;
        }
        false
    }

    /// Past any blanks.
    fn space(&mut self) {
        while self.text[self.at..].starts_with([' ', '\t']) {
            self.at += 1;
        }
    }
}

impl Reader {
    /// A quoted string, as its bytes.
    fn string(&self, text: &str) -> Result<Vec<u8>, Trouble> {
        let bad = |why: &str| Trouble { line: self.line, why: why.to_owned() };
        let body = text
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or_else(|| bad("a string directive whose operand is not in quotes"))?;
        let mut out = Vec::with_capacity(body.len());
        let mut at = 0;
        while at < body.len() {
            let rest = &body[at..];
            let first = rest.as_bytes()[0];
            if first == b'\\' {
                let (value, used) =
                    escape(&rest[1..]).map_err(|why| Trouble { line: self.line, why })?;
                out.push(value);
                at += used + 1;
                continue;
            }
            let ch = rest.chars().next().unwrap_or('\0');
            let mut buffer = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            at += ch.len_utf8();
        }
        Ok(out)
    }
}

/// How far to shift by, which has to be a count and not a number that happens to be negative.
fn shift(by: i64) -> Result<u32, String> {
    u32::try_from(by).map_err(|_| "a shift by a negative amount".to_owned())
}

/// What one backslash and what follows it mean, and how much of the text that took.
///
/// The count is of what came after the backslash, so a caller adds one for the backslash itself.
fn escape(rest: &str) -> Result<(u8, usize), String> {
    let bytes = rest.as_bytes();
    let Some(&first) = bytes.first() else {
        return Err("a backslash with nothing after it".to_owned());
    };
    let simple = match first {
        b'n' => Some(b'\n'),
        b't' => Some(b'\t'),
        b'r' => Some(b'\r'),
        b'f' => Some(0x0c),
        b'b' => Some(0x08),
        b'v' => Some(0x0b),
        b'a' => Some(0x07),
        b'e' => Some(0x1b),
        b'\\' => Some(b'\\'),
        b'"' => Some(b'"'),
        b'\'' => Some(b'\''),
        _ => None,
    };
    if let Some(value) = simple {
        return Ok((value, 1));
    }
    if first == b'x' || first == b'X' {
        let end = bytes[1..]
            .iter()
            .position(|byte| !byte.is_ascii_hexdigit())
            .map_or(bytes.len(), |at| at + 1);
        if end == 1 {
            return Err("a hex escape with no digits in it".to_owned());
        }
        // Only the last two digits, which is what gas keeps: the escape is one byte however many
        // digits were written.
        let text = &rest[1..end];
        let text = &text[text.len().saturating_sub(2)..];
        let value =
            u8::from_str_radix(text, 16).map_err(|_| "a hex escape that is not one".to_owned())?;
        return Ok((value, end));
    }
    if (b'0'..=b'7').contains(&first) {
        let end = bytes.iter().take(3).take_while(|byte| (b'0'..=b'7').contains(byte)).count();
        let value = u32::from_str_radix(&rest[..end], 8)
            .map_err(|_| "an octal escape that is not one".to_owned())?;
        return Ok(((value & 0xff) as u8, end));
    }
    // gas takes an unknown escape as the character itself and warns. Refused here, because the two
    // things it is likely to be are a typo and a file meant for another assembler, and both are
    // better said than guessed.
    Err(format!("'\\{}' is not an escape this compiler knows", first as char))
}

/// The name of the label at the start of this text, if it starts with one.
///
/// A colon after a name and nothing else. `.L1:` is one, so is `foo:`, and so is `1:`, which is a
/// numbered local label and is a place rather than a name: it may be written as many times in a file
/// as the file likes and what refers to it is `1b` for the last one above and `1f` for the next one
/// below.
fn labelled(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !(starts(bytes[0]) || bytes[0].is_ascii_digit()) {
        return None;
    }
    let end = text.find(|ch: char| !carries_on(ch as u8))?;
    // Not `::`, which is a different thing in gas, and not a bare name with nothing after it.
    if bytes.get(end) != Some(&b':') || bytes.get(end + 1) == Some(&b':') {
        return None;
    }
    Some(text[..end].to_owned())
}

/// `name = value`, as the name and the value, when the statement is one.
///
/// Not `==`, which is a comparison, and not a label, which was taken off before this is asked.
fn assigned(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !starts(bytes[0]) {
        return None;
    }
    let end = text.find(|ch: char| !carries_on(ch as u8)).unwrap_or(text.len());
    let rest = text[end..].trim_start().strip_prefix('=')?;
    if rest.starts_with('=') {
        return None;
    }
    Some((&text[..end], rest.trim()))
}

/// Whether a name may start with this.
fn starts(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'.' | b'$')
}

/// Whether a name may go on with this.
fn carries_on(byte: u8) -> bool {
    starts(byte) || byte.is_ascii_digit()
}

/// The name a numbered local label is kept under while the file is being read.
///
/// A file writes `1:` over and over and each one is a different place, so what goes in the table has
/// to say which of them this is. The byte in the middle is one no name in a source file can hold, so
/// nothing a file writes its own way can collide with one of these, and none of them reaches the
/// symbol table at the end.
/// How much padding an alignment takes at `at`, which is none when it would be more than `most`.
fn padding(at: u64, boundary: u64, most: Option<u64>) -> u64 {
    let over = at % boundary;
    let need = if over == 0 { 0 } else { boundary - over };
    if most.is_some_and(|most| need > most) { 0 } else { need }
}

fn counted(number: &str, nth: usize) -> String {
    format!("{number}\u{1}{nth}")
}

/// The text with its quotes taken off, if it had any.
fn unquoted(text: &str) -> String {
    text.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')).unwrap_or(text).to_owned()
}

/// Split on a separator that is outside every string and every bracket.
///
/// The brackets matter as much as the quotes: `.long (1 + 2), 3` is two operands and splitting on
/// every comma would be right here and wrong the moment one turns up inside brackets.
pub(crate) fn split(text: &str, on: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut piece = String::new();
    let mut depth = 0i32;
    let mut quote = None;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if let Some(mark) = quote {
            piece.push(ch);
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    piece.push(next);
                }
                continue;
            }
            if ch == mark {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' => {
                quote = Some(ch);
                piece.push(ch);
            }
            '(' => {
                depth += 1;
                piece.push(ch);
            }
            ')' => {
                depth -= 1;
                piece.push(ch);
            }
            _ if ch == on && depth == 0 => {
                out.push(std::mem::take(&mut piece));
            }
            _ => piece.push(ch),
        }
    }
    if !piece.trim().is_empty() || !out.is_empty() {
        out.push(piece);
    }
    out.into_iter().map(|piece| piece.trim().to_owned()).collect()
}

/// A repeat prefix and the string instruction behind it, as the one mnemonic the encoder knows the
/// pair by, and what is left of the line after the two.
///
/// Five spellings for two bytes. `rep`, `repe` and `repz` are one byte, which is spelled `repe` in
/// front of a scan or a comparison and `rep` in front of anything else, and `repne` and `repnz` are
/// the other. A prefix in front of anything that is not a string instruction is left alone here,
/// so it reaches the encoder as the word it was and is refused there as a mnemonic nobody knows.
///
/// `notrack` is read the same way, joined to the `jmp` or `call` behind it, since the encoder has
/// rows for the pair and none for the prefix alone. So is `rep bsf`, which is `tzcnt`.
fn repeated<'a>(word: &str, rest: &'a str) -> Option<(String, &'a str)> {
    let (next, after) = match rest.find(char::is_whitespace) {
        Some(cut) => (&rest[..cut], rest[cut..].trim()),
        None => (rest, ""),
    };
    if word == "notrack" {
        return match next {
            "jmp" | "jmpq" => Some(("notrack jmp".to_owned(), after)),
            "call" | "callq" => Some(("notrack call".to_owned(), after)),
            _ => None,
        };
    }
    // `rep bsf` is how gcc writes `tzcnt` for a machine that may not have it: the bytes are the
    // same, and a processor without the instruction ignores the prefix and runs the `bsf`.
    if matches!(word, "rep" | "repe" | "repz") {
        if let Some(width) = next.strip_prefix("bsf") {
            if matches!(width, "" | "w" | "l" | "q") {
                return Some((format!("tzcnt{width}"), after));
            }
        }
    }
    let unequal = match word {
        "rep" | "repe" | "repz" => false,
        "repne" | "repnz" => true,
        _ => return None,
    };
    let string = next.len() == 5 && next.ends_with(['b', 'w', 'l', 'q']);
    let which = if string { &next[..4] } else { "" };
    let prefix = match (unequal, which) {
        (false, "movs" | "stos") => "rep",
        (false, "scas" | "cmps") => "repe",
        (true, "scas" | "cmps") => "repne",
        _ => return None,
    };
    Some((format!("{prefix} {next}"), after))
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_object::Reference;

    /// The file, read, with a failure reported as a panic naming the line it was on.
    fn assembled(text: &str) -> Assembled {
        match read(text) {
            Ok(assembled) => assembled,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        }
    }

    /// The bytes of the section of that name.
    fn bytes(assembled: &Assembled, name: &str) -> Vec<u8> {
        let part = assembled
            .parts
            .iter()
            .find(|part| part.name == name)
            .unwrap_or_else(|| panic!("there is no section called '{name}'"));
        part.bytes.clone()
    }

    /// The name of that name.
    fn name<'a>(assembled: &'a Assembled, want: &str) -> &'a Name {
        assembled
            .names
            .iter()
            .find(|name| name.name == want)
            .unwrap_or_else(|| panic!("there is no name called '{want}'"))
    }

    /// What a file this could not read said about it.
    fn refused(text: &str) -> Trouble {
        read(text).err().unwrap_or_else(|| panic!("this was read and should not have been"))
    }

    #[test]
    fn a_repeat_prefix_is_read_with_the_string_instruction_behind_it() {
        let assembled =
            assembled("\t.text\n\trep movsl\n\trepnz scasb\n\trepz cmpsb\n\trep stosq\n");
        assert_eq!(
            bytes(&assembled, ".text"),
            [0xF3, 0xA5, 0xF2, 0xAE, 0xF3, 0xA6, 0xF3, 0x48, 0xAB]
        );
    }

    #[test]
    fn notrack_is_read_with_the_jump_behind_it() {
        let assembled = assembled("\t.text\n\tnotrack jmp\t*%rax\n\tnotrack jmp *%r8\n\tleave\n");
        assert_eq!(bytes(&assembled, ".text"), [0x3E, 0xFF, 0xE0, 0x3E, 0x41, 0xFF, 0xE0, 0xC9]);
    }

    #[test]
    fn rep_bsf_is_read_as_tzcnt() {
        let assembled = assembled("\t.text\n\trep bsfq\t-8(%rbp), %rax\n\trep bsfl %edi, %eax\n");
        assert_eq!(
            bytes(&assembled, ".text"),
            [0xF3, 0x48, 0x0F, 0xBC, 0x45, 0xF8, 0xF3, 0x0F, 0xBC, 0xC7]
        );
    }

    /// A numbered local label, which is a place a file may write as often as it likes.
    ///
    /// `1:` three times is three places and the jumps between them say which by counting, so `1b`
    /// is the one above and `1f` is the one below. None of the three is a name, which is why the
    /// symbol table at the end holds the one thing this file actually called something.
    #[test]
    fn a_number_is_a_label_a_file_may_write_as_many_times_as_it_likes() {
        let out =
            assembled("\t.text\nfoo:\n1:\tnop\n\tjmp 1b\n1:\tnop\n\tjmp 1f\n\tnop\n1:\tret\n");
        let text = bytes(&out, ".text");
        // `nop`, then a jump back over both of them, then `nop`, then a jump forward over the
        // `nop` behind it, then that `nop`, then `ret`.
        assert_eq!(text, vec![0x90, 0xeb, 0xfd, 0x90, 0xeb, 0x01, 0x90, 0xc3]);
        assert!(out.parts[0].relocs.is_empty(), "{:?}", out.parts[0].relocs);
        // One name, and it is the one the file wrote as a name.
        let written: Vec<&str> = out.names.iter().map(|name| name.name.as_str()).collect();
        assert_eq!(written, vec!["foo"]);
    }

    #[test]
    fn a_numbered_label_with_nothing_on_the_side_it_names_is_refused() {
        let back = refused("\t.text\n\tjmp 1b\n1:\tret\n");
        assert!(back.why.contains("none above it"), "{}", back.why);
        let forward = refused("\t.text\n1:\tnop\n\tjmp 1f\n\tret\n");
        assert!(forward.why.contains("none below it"), "{}", forward.why);
    }

    /// A prefix written on a line of its own, which is how gas takes one and how GMP writes them.
    ///
    /// `rep;bsf %rdx, %rcx` is two statements on one line, and the first of them is an instruction
    /// with no operands whose whole encoding is the byte that goes in front of the next one. The
    /// reader needs nothing for this beyond the rows, because a statement is already a statement
    /// whether a semicolon or a newline ended the one before it.
    #[test]
    fn a_prefix_is_a_statement_of_its_own_and_the_byte_goes_in_front() {
        let out = assembled("\t.text\n\trep;bsf %rdx, %rcx\n");
        assert_eq!(bytes(&out, ".text"), vec![0xf3, 0x48, 0x0f, 0xbc, 0xca]);
        let split = assembled("\t.text\n\trep\n\tmovsq\n");
        assert_eq!(bytes(&split, ".text"), vec![0xf3, 0x48, 0xa5]);
        let lock = assembled("\t.text\n\tlock;incl (%rdi)\n");
        assert_eq!(bytes(&lock, ".text"), vec![0xf0, 0xff, 0x07]);
    }

    /// A name reached through the global offset table, which is a relocation however near it is.
    ///
    /// What the four bytes hold is the distance to a slot the linker makes, so there is nothing for
    /// the reader to work out even when the name is defined three lines further down. That is the
    /// difference from a plain rip-relative reference, which cancels to a number whenever both ends
    /// are in the same section.
    #[test]
    fn a_reach_through_the_table_is_a_relocation_even_when_this_file_defines_the_name() {
        let out = assembled("\t.text\n\tmovq table@GOTPCREL(%rip), %rdx\ntable:\n\t.quad 0\n");
        let relocs = &out.parts[0].relocs;
        assert_eq!(relocs.len(), 1);
        assert_eq!(relocs[0].symbol, "table");
        assert_eq!(relocs[0].kind, Reference::Got);
        // The four bytes are the last four of the instruction and the machine counts them from the
        // end of it, so the addend is minus four.
        assert_eq!(relocs[0].addend, -4);
        let out = assembled("\t.text\n\tmovq counter@GOTTPOFF(%rip), %rax\n");
        assert_eq!(out.parts[0].relocs[0].kind, Reference::Thread);
    }

    /// Which of the three table relocations an instruction asks for, as gas picks them: the one
    /// the linker may rewrite for the few instructions it knows, split by whether there is a REX
    /// prefix, and the plain one for everything else. cJSON reads `malloc` into `%xmm0` this way.
    #[test]
    fn only_an_instruction_the_linker_can_rewrite_asks_it_to() {
        for (line, kind) in [
            ("movq f@GOTPCREL(%rip), %rax", Reference::Got),
            ("cmpq f@GOTPCREL(%rip), %rdx", Reference::Got),
            ("addq f@GOTPCREL(%rip), %rdx", Reference::Got),
            ("movl f@GOTPCREL(%rip), %eax", Reference::GotBare),
            ("call *f@GOTPCREL(%rip)", Reference::GotBare),
            ("jmp *f@GOTPCREL(%rip)", Reference::GotBare),
            ("movq f@GOTPCREL(%rip), %xmm0", Reference::GotKept),
            ("movhps f@GOTPCREL(%rip), %xmm0", Reference::GotKept),
            ("movq %rax, f@GOTPCREL(%rip)", Reference::GotKept),
            ("movw f@GOTPCREL(%rip), %ax", Reference::GotKept),
        ] {
            let out = assembled(&format!("\t.text\n\t{line}\n"));
            assert_eq!(out.parts[0].relocs[0].kind, kind, "{line}");
        }
    }

    /// A name reached with something added to it, which is a table indexed by a value that does not
    /// start at zero.
    ///
    /// The number belongs to the linker along with the name, so it lands in the addend rather than
    /// in the bytes, and the minus four the machine already wanted is on top of it.
    #[test]
    fn a_number_beside_a_name_in_a_displacement_is_part_of_what_the_linker_is_asked_for() {
        let out = assembled("\t.text\n\tleaq -512+table(%rip), %r8\n\t.globl table\n");
        let relocs = &out.parts[0].relocs;
        assert_eq!(relocs.len(), 1);
        assert_eq!(relocs[0].symbol, "table");
        assert_eq!(relocs[0].addend, -516);
        // And the name is the name, rather than the whole of what was written in front of the
        // bracket, which is what a symbol table full of things nothing defines used to look like.
        let named: Vec<&str> = out.names.iter().map(|name| name.name.as_str()).collect();
        assert_eq!(named, ["table"]);
    }

    #[test]
    fn a_name_taken_away_from_something_in_a_displacement_is_refused() {
        // There is no relocation for the distance back from something, so this is a mistake rather
        // than a thing to hand on to the linker.
        refused("\t.text\n\tleaq 512-table(%rip), %r8\n");
    }

    #[test]
    fn the_probe_gmp_writes() {
        // The case the whole crate exists for. Four lines, no instruction, and the answer configure
        // is after is the value of the symbol: four, because the `.long` in front of it took four
        // bytes. It seds that number out of `nm` and writes it into a header.
        let out = assembled("\t.data\n\t.globl foo\n\t.long 0\nfoo:\n\t.byte 0\n");
        assert_eq!(bytes(&out, ".data"), vec![0, 0, 0, 0, 0]);
        let foo = name(&out, "foo");
        assert_eq!(foo.at, Held::In { part: 0, offset: 4 });
        assert_eq!(foo.binding, Binding::Global);
    }

    #[test]
    fn every_width_of_number_is_the_bytes_it_says_it_is() {
        let out = assembled(
            "\t.data\n\t.byte 1\n\t.short 2\n\t.long 3\n\t.quad 4\n\t.byte 0x7f, 0377, 'a', '\\n'\n",
        );
        let mut want = vec![1, 2, 0, 3, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0];
        want.extend_from_slice(&[0x7f, 0xff, b'a', b'\n']);
        assert_eq!(bytes(&out, ".data"), want);
    }

    #[test]
    fn a_number_that_is_negative_is_written_as_the_width_asked_for() {
        // Two's complement in that many bytes, not a refusal, because `.short -1` is how a file
        // says two bytes of ones and every table of small offsets somewhere has one in it.
        let out = assembled("\t.data\n\t.short -1\n\t.long -2\n");
        assert_eq!(bytes(&out, ".data"), vec![0xff, 0xff, 0xfe, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn the_three_kinds_of_string_differ_only_in_the_zero_on_the_end() {
        let out = assembled("\t.data\n\t.ascii \"ab\"\n\t.asciz \"cd\"\n\t.string \"e\\tf\"\n");
        assert_eq!(bytes(&out, ".data"), b"abcd\0e\tf\0".to_vec());
    }

    #[test]
    fn space_and_fill_put_that_many_bytes_there() {
        let out = assembled("\t.data\n\t.byte 1\n\t.zero 3\n\t.space 2, 0x41\n\t.fill 2, 1, 7\n");
        assert_eq!(bytes(&out, ".data"), vec![1, 0, 0, 0, 0x41, 0x41, 7, 7]);
    }

    #[test]
    fn aligning_moves_on_to_the_boundary_and_no_further() {
        // `.align` on this machine is a byte count and `.p2align` is a power of two, which is the
        // one thing about them somebody porting a file from another assembler gets wrong.
        let out = assembled("\t.data\n\t.byte 1\n\t.align 8\n\t.byte 2\n\t.p2align 4\n\t.byte 3\n");
        let data = bytes(&out, ".data");
        assert_eq!(data.len(), 17);
        assert_eq!(data[0], 1);
        assert_eq!(data[8], 2);
        assert_eq!(data[16], 3);
        assert_eq!(out.parts[0].align, 16, "the section has to start where the widest ask does");
    }

    #[test]
    fn a_section_that_holds_no_bytes_counts_them_rather_than_carrying_them() {
        let out = assembled("\t.bss\n\t.globl room\nroom:\n\t.zero 4096\n");
        let part = &out.parts[0];
        assert_eq!(part.name, ".bss");
        assert_eq!(part.size, 4096);
        assert!(part.bytes.is_empty(), "the zeroes were carried after all");
        assert!(!part.shape.bits);
    }

    #[test]
    fn what_a_section_directive_said_about_a_section_is_what_it_is() {
        let out = assembled("\t.section .init.text,\"ax\",@progbits\n\t.byte 0x90\n");
        let part = out.parts.iter().find(|part| part.name == ".init.text").expect("the section");
        assert!(part.shape.alloc && part.shape.exec && part.shape.bits);
        assert!(!part.shape.write, "nothing said it was writable");
    }

    #[test]
    fn the_same_section_named_twice_is_one_section_and_the_bytes_run_on() {
        let out = assembled("\t.data\n\t.byte 1\n\t.text\n\t.byte 0x90\n\t.data\n\t.byte 2\n");
        assert_eq!(bytes(&out, ".data"), vec![1, 2]);
        assert_eq!(bytes(&out, ".text"), vec![0x90]);
    }

    #[test]
    fn pushing_a_section_and_coming_back_leaves_the_first_one_where_it_was() {
        let out = assembled(
            "\t.data\n\t.byte 1\n\t.pushsection .rodata\n\t.byte 9\n\t.popsection\n\t.byte 2\n",
        );
        assert_eq!(bytes(&out, ".data"), vec![1, 2]);
        assert_eq!(bytes(&out, ".rodata"), vec![9]);
    }

    #[test]
    fn a_size_that_counts_from_here_back_to_a_label_is_a_number() {
        // `.size foo, .-foo` is on the end of nearly every function gas ever wrote. Both ends are in
        // the same section, so the difference is known here and there is nothing to ask the linker.
        let out = assembled(
            "\t.text\n\t.globl f\n\t.type f, @function\nf:\n\t.byte 0,0,0,0,0\n\t.size f, .-f\n",
        );
        let f = name(&out, "f");
        assert_eq!(f.size, 5);
        assert_eq!(f.sort, Sort::Func);
    }

    #[test]
    fn a_set_may_name_something_further_down_the_file() {
        // Nothing can be worked out as it is parsed, which is why an expression is kept as a sum
        // until the end. `table_end` does not exist yet on the line that subtracts it.
        let out = assembled(
            "\t.data\ntable:\n\t.long 1, 2, 3\ntable_end:\n\t.globl width\n\t.set width, \
             table_end - table\n",
        );
        assert_eq!(name(&out, "width").at, Held::Absolute(12));
    }

    #[test]
    fn a_set_that_names_another_set_is_worked_at_until_it_stops_moving() {
        let out = assembled("\t.set a, b + 1\n\t.set b, c * 2\n\t.set c, 5\n");
        assert_eq!(name(&out, "a").at, Held::Absolute(11));
        assert_eq!(name(&out, "b").at, Held::Absolute(10));
    }

    #[test]
    fn two_sets_that_name_each_other_are_refused_rather_than_looped_over() {
        let why = refused("\t.set a, b\n\t.set b, a\n");
        assert!(why.why.contains("neither has a value"), "{why}");
    }

    #[test]
    fn a_pointer_to_something_else_is_a_relocation_for_the_whole_address() {
        let out = assembled("\t.data\n\t.quad message\n");
        let reloc = &out.parts[0].relocs[0];
        assert_eq!(reloc.at, 0);
        assert_eq!(reloc.symbol, "message");
        assert_eq!(reloc.kind, Reference::Address { bytes: 8 });
        assert_eq!(reloc.addend, 0);
        assert_eq!(name(&out, "message").at, Held::Undefined);
    }

    #[test]
    fn a_distance_from_here_to_something_else_is_a_relocation_relative_to_here() {
        // The other shape a reduced expression can have, and the one whose addend is not zero: the
        // four bytes sit at offset four, and a relocation counts from where it starts.
        let out = assembled("\t.data\n\t.quad 0\n\t.long message - .\n");
        let reloc = &out.parts[0].relocs[0];
        assert_eq!(reloc.at, 8);
        assert_eq!(reloc.symbol, "message");
        assert_eq!(reloc.kind, Reference::Data);
        assert_eq!(reloc.addend, 0);
    }

    #[test]
    fn a_distance_counted_from_somewhere_that_is_not_here_carries_the_difference() {
        // The case that says which way round the addend goes, which `message - .` cannot because
        // both halves of it are the same number. A linker writes `symbol + addend - here`, and
        // what was asked for is `symbol - start`, so the addend is how far these bytes are past
        // the label rather than how far the label is behind them.
        let out = assembled("\t.data\nstart:\n\t.quad 0\n\t.long message - start\n");
        let reloc = &out.parts[0].relocs[0];
        assert_eq!(reloc.at, 8);
        assert_eq!(reloc.kind, Reference::Data);
        assert_eq!(reloc.addend, 8);
    }

    #[test]
    fn a_number_added_to_a_name_rides_along_in_the_addend() {
        let out = assembled("\t.data\n\t.quad message + 16\n");
        assert_eq!(out.parts[0].relocs[0].addend, 16);
    }

    #[test]
    fn comm_and_lcomm_ask_the_linker_for_room_rather_than_carrying_it() {
        let out = assembled("\t.comm shared, 8, 8\n\t.lcomm mine, 32, 16\n");
        assert_eq!(name(&out, "shared").at, Held::Common { size: 8, align: 8 });
        assert_eq!(name(&out, "shared").binding, Binding::Global);
        // `.lcomm` is space in `.bss` under a local name, which is a different thing from `.comm`
        // however much the two names look alike.
        assert_eq!(name(&out, "mine").binding, Binding::Local);
        assert!(matches!(name(&out, "mine").at, Held::In { .. }));
    }

    #[test]
    fn comm_of_a_name_said_to_be_local_is_room_here_as_lcomm_is() {
        let out = assembled("\t.local mine\n\t.comm mine, 8, 8\n");
        assert_eq!(name(&out, "mine").binding, Binding::Local);
        assert!(matches!(name(&out, "mine").at, Held::In { .. }));
    }

    #[test]
    fn what_a_file_says_about_who_can_see_a_name_is_kept() {
        let out = assembled(
            "\t.text\n\t.globl seen\n\t.weak maybe\n\t.hidden inside\n\t.globl \
             inside\nseen:\nmaybe:\ninside:\n\t.byte 0\n",
        );
        assert_eq!(name(&out, "seen").binding, Binding::Global);
        assert_eq!(name(&out, "maybe").binding, Binding::Weak);
        assert_eq!(name(&out, "inside").visibility, Visibility::Hidden);
    }

    #[test]
    fn the_name_of_the_file_is_a_symbol_of_its_own() {
        // And not one that can collide with something in the file, which is why it is kept apart
        // from the rest until the end.
        let out = assembled("\t.file \"big.s\"\n\t.data\nbig:\n\t.byte 0\n");
        assert_eq!(out.names[0].name, "big.s");
        assert_eq!(out.names[0].sort, Sort::File);
        assert_eq!(out.names[0].binding, Binding::Local);
        assert!(out.names.iter().any(|name| name.name == "big"), "the label was lost");
    }

    #[test]
    fn a_numbered_file_is_a_note_for_a_debugger_and_not_a_name() {
        // `.file 1 "foo.c"` is the DWARF form and names an entry in a line table, which is a
        // different directive wearing the same word.
        let out = assembled("\t.file 1 \"foo.c\"\n\t.data\n\t.byte 0\n");
        assert!(out.names.is_empty(), "{:?}", out.names);
    }

    #[test]
    fn an_instruction_this_has_no_bytes_for_is_refused_by_name_and_by_line() {
        // The failure this crate is written to prevent. An assembler that skipped what it did not
        // recognise would write an object that links, and what would be wrong with it is a run of
        // missing bytes in the middle of a function.
        let why = refused("\t.text\nf:\n\tmovq %rdi, %rax\n\tpopcnt %rax, %rdx\n\tret\n");
        assert_eq!(why.line, 4);
        assert!(why.why.contains("popcnt"), "{why}");
    }

    #[test]
    fn a_function_of_instructions_is_its_bytes_and_its_size() {
        // The whole of what a hand written file is, end to end: a section, a name, three
        // instructions and a size counted back to the label.
        let out = assembled(
            "\t.text\n\t.globl id\n\t.type id, @function\nid:\n\tmovq %rdi, %rax\n\tret\n\t.size \
             id, .-id\n",
        );
        assert_eq!(bytes(&out, ".text"), vec![0x48, 0x89, 0xf8, 0xc3]);
        assert_eq!(name(&out, "id").size, 4);
        assert_eq!(name(&out, "id").at, Held::In { part: 0, offset: 0 });
    }

    #[test]
    fn a_jump_to_a_label_in_this_section_is_a_number_and_not_a_relocation() {
        // Because both ends are here, so there is nothing for a linker to work out. The distance
        // is counted from the end of the jump, which is why jumping over nothing is zero and not
        // minus two.
        let out = assembled("\t.text\n\tjmp over\nover:\n\tret\n");
        assert_eq!(bytes(&out, ".text"), vec![0xeb, 0, 0xc3]);
        assert!(out.parts[0].relocs.is_empty(), "{:?}", out.parts[0].relocs);
    }

    #[test]
    fn a_jump_backwards_is_the_negative_distance_to_it() {
        let out = assembled("\t.text\nagain:\n\tjmp again\n");
        assert_eq!(bytes(&out, ".text"), vec![0xeb, 0xfe]);
    }

    #[test]
    fn a_branch_is_as_short_as_the_distance_lets_it_be() {
        // A hundred and twenty seven bytes forward still fits in one, and one more does not, which
        // is where gas moves to the long form too. The conditional one keeps its condition.
        let out = assembled("\tjne far\n\t.zero 127\nfar:\n\tret\n");
        assert_eq!(bytes(&out, ".text")[..2], [0x75, 127]);
        let out = assembled("\tjne far\n\t.zero 128\nfar:\n\tret\n");
        assert_eq!(bytes(&out, ".text")[..6], [0x0f, 0x85, 128, 0, 0, 0]);
        let out = assembled("back:\n\t.zero 126\n\tjmp back\n");
        assert_eq!(bytes(&out, ".text")[126..], [0xeb, 0x80]);
        let out = assembled("back:\n\t.zero 127\n\tjmp back\n");
        assert_eq!(bytes(&out, ".text")[127..], [0xe9, 0x7c, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn a_branch_made_long_can_push_another_one_out_of_reach() {
        // The first jump fits only while the second is short, and the second does not fit at all.
        // Once the second is long the first is three bytes further from its label and has to be
        // long as well, which is the pass after the one that found the second.
        let out = assembled("\tjmp a\n\t.zero 125\n\tjmp b\na:\n\t.zero 128\nb:\n\tret\n");
        let text = bytes(&out, ".text");
        assert_eq!(text[..5], [0xe9, 130, 0, 0, 0]);
        assert_eq!(text[130..135], [0xe9, 128, 0, 0, 0]);
    }

    #[test]
    fn a_jump_past_an_alignment_is_judged_the_way_gas_judges_it() {
        // Laid out with every jump short, the third one is a hundred and thirty bytes from its
        // label. The two in front of it are long, which is seven bytes, and the alignment gives
        // those seven back, so where it ends up it is a hundred and twenty three and fits. gas
        // counts it that way on its first pass and so does this, and the bytes are the ones gas
        // writes. Judged by the first layout alone it would be long, and three bytes further on
        // everything behind it would be too.
        let out = assembled(
            "\tjmp far1\n\tje far1\n\tje far2\n\t.zero 123\n\t.p2align 3\nfar2:\n\tret\n\t.zero \
             200\nfar1:\n\tret\n",
        );
        let text = bytes(&out, ".text");
        assert_eq!(text[..13], [0xe9, 0x4c, 1, 0, 0, 0x0f, 0x84, 0x46, 1, 0, 0, 0x74, 123]);
        assert_eq!(text.len(), 0x152);
    }

    #[test]
    fn a_branch_that_leaves_the_section_or_goes_to_a_weak_name_is_long() {
        // Both are relocations, and a relocation is four bytes whatever the distance comes to.
        let out = assembled("\tjmp elsewhere\n\tjz maybe\n\t.weak maybe\nmaybe:\n\tret\n");
        assert_eq!(bytes(&out, ".text")[..1], [0xe9]);
        assert_eq!(bytes(&out, ".text")[5..7], [0x0f, 0x84]);
    }

    #[test]
    fn a_section_of_constants_says_how_long_each_one_is() {
        let out = assembled(
            "\t.section .rodata.str1.1,\"aMS\",@progbits,1\n\t.string \"hi\"\n\t\
             .section .rodata.cst8,\"aM\",@progbits,8\n\t.quad 1\n\t.section .rodata.x,\"aM\"\n\t.byte 1\n",
        );
        let shapes: Vec<_> =
            out.parts.iter().map(|part| (part.shape.merge, part.shape.strings)).collect();
        assert_eq!(shapes, [(1, true), (8, false), (0, false)]);
    }

    #[test]
    fn a_global_name_defined_here_is_still_left_to_the_linker() {
        // Another object may define it first, so the call and the address are relocations with
        // zeros in the bytes, the same as gas writes. A jump to it is worked out the way gas works
        // it out, and so is a call to a static name and a distance from one global to another.
        let out = assembled(
            "\t.globl f\nf:\n\tcall f\n\tjmp f\n\tleaq f(%rip), %rax\n\tcall g\n\t\
             .long f - g\ng:\n\tret\n",
        );
        let relocs = &out.parts[0].relocs;
        let kinds: Vec<_> = relocs.iter().map(|r| (r.at, r.symbol.as_str(), r.kind)).collect();
        assert_eq!(kinds, [(1, "f", Reference::Call), (10, "f", Reference::Data)]);
        assert!(relocs.iter().all(|r| r.addend == -4));
        let text = bytes(&out, ".text");
        assert_eq!(text[..7], [0xe8, 0, 0, 0, 0, 0xeb, 0xf9]);
        assert_eq!(text[14..19], [0xe8, 4, 0, 0, 0]);
        assert_eq!(text[19..23], (-23i32).to_le_bytes());
    }

    #[test]
    fn a_call_to_a_static_name_in_another_section_needs_no_stub() {
        let out = assembled("\t.text\n\tcall cold\n\t.section .text.unlikely\ncold:\n\tret\n");
        let reloc = &out.parts[0].relocs[0];
        assert_eq!((reloc.symbol.as_str(), reloc.kind), ("cold", Reference::Data));
    }

    #[test]
    fn a_call_to_a_name_this_file_does_not_define_may_go_through_a_stub() {
        // Which is the whole difference between this and the test below it. A call is allowed to
        // reach further than four bytes by way of something the linker writes, and a load of a
        // datum is not, so they are two relocations and the shape of the instruction is what says
        // which. The addend is minus four because the four bytes are the last of the instruction
        // and the machine counts them from the end of it.
        let out = assembled("\t.text\n\tcall puts\n");
        let reloc = &out.parts[0].relocs[0];
        assert_eq!(reloc.at, 1);
        assert_eq!(reloc.symbol, "puts");
        assert_eq!(reloc.kind, Reference::Call);
        assert_eq!(reloc.addend, -4);
    }

    #[test]
    fn a_datum_reached_from_the_instruction_pointer_is_a_relocation_that_may_not() {
        let out = assembled("\t.text\n\tmovq message(%rip), %rax\n");
        let reloc = &out.parts[0].relocs[0];
        assert_eq!(reloc.symbol, "message");
        assert_eq!(reloc.kind, Reference::Data);
        // Three bytes of opcode and addressing in front of the four, and nothing after them.
        assert_eq!(reloc.at, 3);
        assert_eq!(reloc.addend, -4);
    }

    #[test]
    fn a_branch_with_one_byte_of_reach_is_filled_in_at_one_byte() {
        // `jrcxz` has no longer form, so what goes in is a byte and the byte is all there is. A
        // fixup that assumed four would write over the two instructions behind this one.
        let out = assembled("\t.text\nagain:\n\tdec %rcx\n\tjrcxz again\n\tret\n");
        assert_eq!(bytes(&out, ".text"), vec![0x48, 0xff, 0xc9, 0xe3, 0xfb, 0xc3]);
    }

    #[test]
    fn a_branch_to_somewhere_the_bytes_it_has_cannot_reach_is_refused() {
        // The other half of the same thing. There is no relaxing a `jrcxz` into something longer,
        // so a destination out of its reach is a mistake in the file, and quietly keeping the low
        // byte of the distance would send the program somewhere nobody wrote.
        let why = refused("\t.text\n\tjrcxz away\n\t.zero 200\naway:\n\tret\n");
        assert_eq!(why.line, 2);
        assert!(why.why.contains("does not reach"), "{why}");
    }

    #[test]
    fn a_number_too_big_for_the_bytes_it_is_written_into_is_refused() {
        // Not about instructions at all, and found on the way to the two above: a distance between
        // two labels written into a `.byte` was being cut down to its low eight bits. Counted both
        // ways, so a byte takes anything from minus a hundred and twenty eight to two hundred and
        // fifty five and refuses what is outside that.
        let out = assembled("\t.data\nhere:\n\t.zero 200\nthere:\n\t.byte there - here\n");
        assert_eq!(bytes(&out, ".data")[200], 200);
        let why = refused("\t.data\nhere:\n\t.zero 300\nthere:\n\t.byte there - here\n");
        assert!(why.why.contains("does not reach"), "{why}");
    }

    #[test]
    fn an_instruction_in_a_section_that_holds_no_bytes_is_refused() {
        let why = refused("\t.bss\n\tret\n");
        assert!(why.why.contains("holds no bytes"), "{why}");
    }

    #[test]
    fn a_directive_this_does_not_know_is_refused_by_name_and_by_line() {
        let why = refused("\t.text\n\t.byte 0\n\t.reloc 0, R_X86_64_NONE, f\n");
        assert_eq!(why.line, 3);
        assert!(why.why.contains(".reloc"), "{why}");
    }

    #[test]
    fn the_comments_the_three_ways_of_writing_one_make_are_not_read() {
        // The `#` one is why the output of the preprocessor can be handed straight to this: a
        // `# 42 "foo.h"` line marker is a comment and nothing has to know it is one.
        let out = assembled(
            "# 1 \"foo.S\"\n\t.data\n\t.byte 1 # one\n\t.byte 2 // two\n\t/* a\n\tcomment */\t.byte \
             3\n",
        );
        assert_eq!(bytes(&out, ".data"), vec![1, 2, 3]);
    }

    #[test]
    fn a_comment_left_open_at_the_end_of_the_file_is_said_rather_than_ignored() {
        let why = refused("\t.data\n\t/* and then nothing\n");
        assert!(why.why.contains("never closed"), "{why}");
    }

    #[test]
    fn a_string_with_a_comment_character_in_it_is_a_string() {
        let out = assembled("\t.data\n\t.ascii \"a#b/*c\"\n");
        assert_eq!(bytes(&out, ".data"), b"a#b/*c".to_vec());
    }

    #[test]
    fn several_statements_on_one_line_are_several_statements() {
        let out = assembled("\t.data; .byte 1; .byte 2\n");
        assert_eq!(bytes(&out, ".data"), vec![1, 2]);
    }

    #[test]
    fn a_section_nothing_was_ever_put_in_is_dropped() {
        // Every file starts in `.text` whether or not it says so, and a `.section` inside a macro
        // that turned out to be unused should not leave a header behind either.
        let out = assembled("\t.data\n\t.byte 1\n");
        assert_eq!(out.parts.len(), 1);
        assert_eq!(out.parts[0].name, ".data");
    }

    #[test]
    fn a_section_with_nothing_in_it_but_a_name_is_kept() {
        // Because the name has to point somewhere, and dropping the section under it would leave a
        // symbol pointing at a section that is not there.
        let out = assembled("\t.text\n\t.globl marker\nmarker:\n");
        assert_eq!(out.parts.len(), 1);
        assert_eq!(name(&out, "marker").at, Held::In { part: 0, offset: 0 });
    }

    #[test]
    fn an_error_directive_is_the_file_saying_it_refuses_itself() {
        let why = refused("\t.error \"this is not the machine for it\"\n");
        assert!(why.why.contains("not the machine for it"), "{why}");
    }

    #[test]
    fn a_jump_counted_from_itself_is_the_short_one_gas_writes() {
        // What tcc's own tests do: jump over four bytes of data and load them back by counting
        // from the load. Both only land where they mean to if the jump is two bytes long.
        let out = assembled("\tjmp .+6\n\t.int 123\n\tmov .-4(%rip), %eax\n");
        assert_eq!(
            bytes(&out, ".text"),
            vec![0xeb, 0x04, 123, 0, 0, 0, 0x8b, 0x05, 0xf6, 0xff, 0xff, 0xff]
        );
    }

    #[test]
    fn a_numbered_label_in_an_expression_is_a_place() {
        let out =
            assembled("2:\n\tjmp .+6\n1:\n\t.pushsection .data\n\t.long 1b - 2b\n\t.popsection\n");
        assert_eq!(bytes(&out, ".data"), vec![2, 0, 0, 0]);
        // And a binary number is still a number, because the digits go on after the letter.
        let out = assembled("\t.data\n\t.byte 0b101\n");
        assert_eq!(bytes(&out, ".data"), vec![5]);
    }

    #[test]
    fn a_number_an_instruction_carries_may_be_an_expression_over_labels() {
        let out = assembled("3:\tmov $4f-3b, %eax\n4:\n");
        assert_eq!(bytes(&out, ".text"), vec![0xb8, 5, 0, 0, 0]);
    }

    #[test]
    fn a_number_an_instruction_carries_may_not_name_something_elsewhere() {
        let why = refused("\tmov $elsewhere, %eax\n");
        assert!(why.why.contains("relocation"), "{why}");
    }

    #[test]
    fn a_name_set_twice_means_what_it_was_where_it_is_used() {
        let out = assembled(
            "\t.data\n\t.byte early\n\tearly = 3\n\tx = 1\n\t.byte x\n\tx = x + 1\n\t.byte x\n",
        );
        assert_eq!(bytes(&out, ".data"), vec![3, 1, 2]);
    }

    #[test]
    fn a_place_set_twice_and_reached_from_another_section_is_relocated_against() {
        let out = assembled(
            "\t.data\n\tx = .\n\t.int 1\n\tx = .\n\t.int 2\n\t.text\n\tmov x(%rip), %eax\n",
        );
        let reloc = &out.parts.iter().find(|part| part.name == ".text").unwrap().relocs[0];
        let target = name(&out, &reloc.symbol);
        let data = out.parts.iter().position(|part| part.name == ".data").unwrap();
        assert_eq!(target.at, Held::In { part: data, offset: 4 });
    }

    #[test]
    fn frame_rules_are_an_unwind_table_pointing_at_the_function() {
        let out = assembled(
            "f:\n\t.cfi_startproc\n\tpush %rbp\n\t.cfi_def_cfa_offset 16\n\t.cfi_offset %rbp, \
             -16\n\tpop %rbp\n\t.cfi_def_cfa_offset 8\n\tret\n\t.cfi_endproc\n",
        );
        let table = out.parts.iter().find(|part| part.name == ".eh_frame").expect("a table");
        // One byte in, the push: the frame is sixteen deep and the caller's rbp is at the bottom.
        // One byte later, the pop, and it is eight deep again.
        let rows = [0x41, 0x0e, 0x10, 0x86, 0x02, 0x41, 0x0e, 0x08];
        assert!(table.bytes.windows(rows.len()).any(|at| at == rows), "{:x?}", table.bytes);
        let [reloc] = table.relocs.as_slice() else { panic!("one record, one relocation") };
        let text = out.parts.iter().position(|part| part.name == ".text").unwrap();
        assert_eq!(name(&out, &reloc.symbol).at, Held::In { part: text, offset: 0 });
    }

    #[test]
    fn a_frame_rule_relative_to_the_register_is_the_same_slot() {
        let out = assembled(
            "\t.cfi_startproc\n\tpush %rbx\n\t.cfi_adjust_cfa_offset 8\n\t.cfi_rel_offset \
             %rbx, 0\n\t.cfi_endproc\n",
        );
        let table = out.parts.iter().find(|part| part.name == ".eh_frame").expect("a table");
        let rows = [0x41, 0x0e, 0x10, 0x83, 0x02];
        assert!(table.bytes.windows(rows.len()).any(|at| at == rows), "{:x?}", table.bytes);
    }

    #[test]
    fn frame_rules_for_a_debugger_only_are_no_unwind_table() {
        let out =
            assembled("\t.cfi_sections .debug_frame\n\t.cfi_startproc\n\tret\n\t.cfi_endproc\n");
        assert!(out.parts.iter().all(|part| part.name != ".eh_frame"));
    }

    #[test]
    fn a_frame_rule_outside_a_function_or_a_function_never_ended_is_refused() {
        let why = refused("\t.cfi_def_cfa_offset 16\n");
        assert!(why.why.contains("outside"), "{why}");
        let why = refused("\t.cfi_startproc\n\tret\n");
        assert!(why.why.contains("never ended"), "{why}");
    }
}
