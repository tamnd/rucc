//! Reading a file of assembly.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1, which asks for a real assembler with a real
//! directive set rather than a call out to `as`.
//!
//! # What is here and what is not
//!
//! The directives and the labels, and no instruction. That is a smaller thing than an assembler and
//! it is the half that a great many files need and that nothing else in this compiler can do. The
//! probes a configure script writes are almost all of this kind: GMP writes four lines with a
//! `.long` and a label in them to find out how the local assembler spells a thirty two bit word,
//! and the answer it is looking for is the value of a symbol in the object, so nothing but a real
//! object will do and there is no instruction anywhere in it.
//!
//! An instruction is refused by name with its line number. Guessing at one is the failure mode that
//! matters here: an assembler that skipped what it did not recognise would write an object that
//! links, and what would be wrong with it is a run of missing bytes in the middle of a function,
//! which nothing finds until the program runs. Instruction assembly is the rest of the issue.
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

use rucc_object::{
    Array, Assembled, Binding, Held, Name, Part, Reference, Reloc, Shape, Sort, Visibility,
};

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
/// [`Trouble`] for a directive this does not know, an instruction, an expression that does not
/// reduce to something a relocation can say, or a file that is malformed. Every one of them carries
/// the line it was on.
pub fn read(text: &str) -> Result<Assembled, Trouble> {
    let mut reader = Reader::default();
    reader.run(text)?;
    reader.finish()
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
}

/// A place in a section whose bytes are an expression that could not be worked out yet.
#[derive(Debug, Clone)]
struct Fixup {
    part: usize,
    at: u64,
    width: u8,
    sum: Sum,
    line: usize,
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
    /// Which sections have a name pointing into them, so that an empty one that something is
    /// defined in survives and an empty one nothing mentions does not.
    labelled: std::collections::HashSet<usize>,
    fixups: Vec<Fixup>,
    /// `.set` and `.equ`, as the symbol they name and the expression they were given.
    sets: Vec<(usize, Sum, usize)>,
    /// `.size`, the same way.
    sizes: Vec<(usize, Sum, usize)>,
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
        if let Some(directive) = word.strip_prefix('.') {
            return self.directive(directive, rest);
        }
        Err(self.bad(&format!(
            "'{word}' is an instruction, and this compiler assembles the directives of a file of \
             assembly and not yet its instructions"
        )))
    }

    /// A name defined here, at wherever the current section has got to.
    fn label(&mut self, name: &str) -> Result<(), Trouble> {
        let at = self.at();
        let part = self.here;
        let sym = self.sym(name);
        if self.syms[sym].at != Held::Undefined {
            let what = format!("'{name}' is defined twice");
            return Err(self.bad(&what));
        }
        self.syms[sym].at = Held::In { part, offset: at };
        self.labelled.insert(part);
        Ok(())
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
            "local" => self.bind(&args, Binding::Local)?,
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
                let sum = self.expression(&what)?;
                let sym = self.sym(&name);
                self.sets.push((sym, sum, self.line));
            }
            "comm" | "lcomm" => self.common(&args, word == "lcomm")?,

            // Said for a debugger or a reader and holding nothing a link depends on. Passed over
            // rather than refused, because a file that carries them is otherwise readable and
            // refusing would turn a note into a failure.
            "file" | "ident" | "loc" | "loc_mark_labels" | "version" | "arch" | "code64"
            | "att_syntax" | "intel_syntax" | "warning" => {}
            _ if word.starts_with("cfi_") => {}

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
        if let Some(flags) = args.get(1) {
            let letters = unquoted(flags.trim());
            shape = Shape { bits: true, ..Shape::default() };
            for letter in letters.chars() {
                match letter {
                    'a' => shape.alloc = true,
                    'w' => shape.write = true,
                    'x' => shape.exec = true,
                    'T' => shape.thread = true,
                    // Mergeable, with or without strings in it, and part of a group. All three are
                    // about what a linker may do with two copies of the section, and taking them as
                    // an ordinary section of the same bytes is correct and merely larger.
                    'M' | 'S' | 'G' | 'o' | 'e' | 'R' | 'd' => {}
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
            self.fixups.push(Fixup { part, at, width, sum, line: self.line });
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
        let default = if self.parts[self.here].shape.exec { 0x90 } else { 0 };
        let fill = match args.get(1) {
            Some(arg) if !arg.trim().is_empty() => self.byte(arg)?,
            _ => default,
        };
        let at = self.at();
        let over = at % boundary;
        let need = if over == 0 { 0 } else { boundary - over };
        // The third operand is how much padding is worth it. More than that and the alignment is
        // skipped entirely, which is how a file asks for an alignment only where it is cheap.
        if let Some(most) = args.get(2).filter(|arg| !arg.trim().is_empty()) {
            let most = self.number(&most.clone())?;
            if need > self.count(most)? {
                return Ok(());
            }
        }
        let part = &mut self.parts[self.here];
        part.align = part.align.max(boundary);
        self.pad(need, fill)
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
        let here = (self.here, self.at() as i64);
        let mut parser = Parser { text: text.trim(), at: 0, here };
        let sum = parser.whole().map_err(|why| Trouble { line: self.line, why })?;
        // Every name it mentioned gets a symbol table entry, so that a relocation against one has
        // something to point at and so that an undefined one is asked of the linker.
        for term in &sum.terms {
            if let What::Symbol(name) = &term.what {
                let name = name.clone();
                self.sym(&name);
            }
        }
        Ok(sum)
    }

    /// A message about this line.
    fn bad(&self, why: &str) -> Trouble {
        Trouble { line: self.line, why: why.to_owned() }
    }

    /// Work out everything that was waiting for the end of the file.
    fn finish(mut self) -> Result<Assembled, Trouble> {
        self.resolve_sets()?;
        self.resolve_sizes()?;
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
        let mut names = Vec::with_capacity(self.syms.len());
        for sym in self.syms {
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
        Ok(Assembled { parts, names })
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
    fn resolve_fixups(&mut self) -> Result<(), Trouble> {
        for fixup in std::mem::take(&mut self.fixups) {
            let line = fixup.line;
            let residue = self.reduce(&fixup.sum).map_err(|why| Trouble { line, why })?;
            let bad = |why: String| Trouble { line, why };
            let (symbol, kind, addend) = match residue.left.as_slice() {
                [] => {
                    let bytes = residue.constant.to_le_bytes();
                    let at = fixup.at as usize;
                    let part = &mut self.parts[fixup.part];
                    part.bytes[at..at + fixup.width as usize]
                        .copy_from_slice(&bytes[..fixup.width as usize]);
                    continue;
                }
                // The address of something, which is the whole of what a table of pointers holds.
                [Left { coeff: 1, what: What::Symbol(name), .. }] => {
                    let kind = Reference::Address { bytes: fixup.width };
                    (name.clone(), kind, residue.constant)
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
                    let addend = residue.constant + offset - fixup.at as i64;
                    (name.clone(), Reference::Data, addend)
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
            });
        }
        Ok(())
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
/// A colon after a name and nothing else. `.L1:` is one and so is `foo:`, and `1:` is not, because
/// a numbered label is a local one that is referred to as `1b` or `1f` and neither is read here.
fn labelled(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !starts(bytes[0]) {
        return None;
    }
    let end = text.find(|ch: char| !carries_on(ch as u8))?;
    // Not `::`, which is a different thing in gas, and not a bare name with nothing after it.
    if bytes.get(end) != Some(&b':') || bytes.get(end + 1) == Some(&b':') {
        return None;
    }
    Some(text[..end].to_owned())
}

/// Whether a name may start with this.
fn starts(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'.' | b'$')
}

/// Whether a name may go on with this.
fn carries_on(byte: u8) -> bool {
    starts(byte) || byte.is_ascii_digit()
}

/// The text with its quotes taken off, if it had any.
fn unquoted(text: &str) -> String {
    text.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')).unwrap_or(text).to_owned()
}

/// Split on a separator that is outside every string and every bracket.
///
/// The brackets matter as much as the quotes: `.long (1 + 2), 3` is two operands and splitting on
/// every comma would be right here and wrong the moment one turns up inside brackets.
fn split(text: &str, on: char) -> Vec<String> {
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
