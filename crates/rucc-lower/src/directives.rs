//! An `asm` at file scope whose template is directives, read into the objects it defines.
//!
//! Design: `spec/11-asm-objects-debug.md` sections 11.1 and 11.2.
//!
//! # What this is for
//!
//! A template with no instructions in it is the case to build first, and 11.2 says so about the
//! `asm` inside a function. The same sentence is true of the one outside a function and for the
//! same reason: what a program writes there is nearly always a run of directives that puts bytes
//! and names in the output, and none of it needs the assembler 11.1 is waiting for. `.incbin` is
//! the whole of what the incbin header does, an alias table is `.globl` and a label, and a
//! version script's worth of `.set` is the same shape again.
//!
//! So a template made of directives is read here and becomes the globals it defines, and a
//! template with an instruction in it is refused by name until there is something that can turn
//! one into bytes. That is the same line drawn in the same place as for the `asm` in a function,
//! which refuses a template with anything in it today.
//!
//! # Why globals rather than text
//!
//! This compiler writes object files itself and does not print assembly for somebody else to
//! read, so there is no output for a template to be copied into. What the directives say is that
//! a section holds these bytes under these names with these alignments, and a global is exactly
//! that, so the reading is a translation and not an approximation. It also means `-S` and `-c`
//! cannot disagree about what the file contains, because both are written from the same globals.
//!
//! # Why the labels land next to each other
//!
//! A block usually names more than one thing and means them to be adjacent. The incbin header
//! writes a label, the file's bytes, a second label, and a size worked out as the distance
//! between them, and a program that reads the size and walks to the end symbol is reading a
//! promise about where the two sit. One global per label keeps that promise because the object
//! writer lays globals out in the order the module holds them, padding each to its own
//! alignment, which is the same walk an assembler makes over the same directives. What it takes
//! is for nothing to be placed between them, and that is why [`crate::unit`] reads the file
//! scope blocks before it walks the declarations: a block's globals are added together, so they
//! are a run, and a later declaration of one of the names finds the definition already there.
//!
//! The offsets this computes are counted from the start of the block rather than from the start
//! of the section, so the distance between two labels is right only if the block starts at an
//! offset every alignment inside it divides. That is arranged rather than hoped for: the first
//! global of a section is given the largest alignment anything in that section asked for.
//!
//! Bytes written before any label are a run of their own and are given a name nothing else takes,
//! because a global has to have a name and nothing outside the template can name these. They are
//! still a global in the same section written before the label that follows them, so the label is
//! where an assembler would have put it and the bytes are where a program reading backwards from
//! it expects. A label written as a number is not one of these runs at all: it is local, it may be
//! written again further down, and what it is for is the distance from somewhere else in the same
//! block, so it is recorded as a position and leaves the run it stands inside alone.
//!
//! One difference from an assembler survives this and is deliberate. An alignment written after
//! the last byte of a section pads the section out in `gas` and does nothing here, because nothing
//! has been written for the padding to go in front of. Nothing can read those bytes: the section
//! still records the alignment it asked for, so the linker puts whatever follows it at the same
//! place either way, and every symbol in the file is at the same offset it would be.

use std::collections::HashMap;

use rucc_ir::{Linkage, Visibility};

/// What could not be done with a template, and which kind of answer that is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failed {
    /// Something a reader of directives does not do, named so the message can say which.
    ///
    /// The string completes a sentence that ends in "is not supported yet", so it reads as a
    /// noun phrase and names the construct rather than describing the reader.
    Unsupported(String),
    /// A file `.incbin` named that could not be read, with the reason the operating system gave.
    Missing(String, String),
}

/// Which section the bytes of one global go in.
///
/// The four the directives name by themselves and the one they name with a string. They are kept
/// apart rather than turned into a section name here because what the object writer wants is what
/// kind of thing the global is: `.rodata` is the section a constant with no address in it lands
/// in anyway, and asking for it by name would produce a writable section with the same spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Section {
    /// `.text`. Nothing may be emitted into it here, since bytes in it would be instructions.
    Text,
    /// `.data`, which is what a section directive naming nothing else gets.
    Data,
    /// `.rodata`, and on a Mach-O target `.const_data`.
    ReadOnly,
    /// `.bss`, which carries no image, so only zeros may be put in it.
    Bss,
    /// The section the template named, for anything the four above do not cover.
    Named(String),
}

/// One piece of an image, kept as what it is rather than as bytes.
///
/// An integer is not bytes yet because which end of it comes first is the target's answer and
/// not this reader's, and the module already writes a scalar the way the target wants it. Zeros
/// are not bytes either, so that a section carrying no image can say how much of it there is
/// without the compiler holding that many bytes to say it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    /// Literal bytes, in the order they go in the image.
    Bytes(Vec<u8>),
    /// One integer that many bytes wide.
    Int {
        /// How many bytes of image it is.
        width: u8,
        /// Its value, which is what the expression worked out to.
        value: i64,
    },
    /// That many zero bytes, and no image for them.
    Zero(u64),
    /// How far one of this template's own places is from these four bytes, which is what
    /// `.long 661b - .` writes. The place is named as the global it is inside and how far into
    /// that global it sits, because a relocation names a symbol and the label it was written as
    /// may be a local one that never becomes a symbol at all.
    Away {
        /// Which of the template's globals holds the place, as an index into the list this
        /// reader gives back, since the name of one written under no label is minted later.
        piece: usize,
        /// How far into that global the place is.
        addend: i64,
    },
}

impl Item {
    /// How many bytes of image it is.
    fn size(&self) -> u64 {
        match self {
            Item::Bytes(bytes) => bytes.len() as u64,
            Item::Int { width, .. } => u64::from(*width),
            Item::Zero(bytes) => *bytes,
            Item::Away { .. } => 4,
        }
    }
}

/// What a template said, which is the globals it defines and the names it equates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Assembled {
    /// The globals, in the order the template defined them.
    pub pieces: Vec<Piece>,
    /// The names a `.set` gave to something else, in the order they were written.
    pub sets: Vec<Set>,
}

/// One global a template defines, which is one label and everything written under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    /// The name the label gave it, and nothing for bytes written under no label.
    ///
    /// A template may write bytes before it writes any label, which is a thing with no name that
    /// a global has to have one for, so the walk mints one that no C program can collide with.
    /// The bytes are still part of the run the template is laying out, and they land where they
    /// were written because the globals of one template are added to the module in order.
    pub name: Option<String>,
    /// Which section it goes in.
    pub section: Section,
    /// What it has to be aligned to, which is what the last alignment directive in front of the
    /// label asked for.
    pub align: u32,
    /// Its image, in order.
    pub items: Vec<Item>,
    /// How many bytes the image adds up to.
    pub size: u64,
    /// How the linker sees the name, which is internal unless a directive exported it.
    pub linkage: Linkage,
    /// How far outside a shared library the name reaches.
    pub visibility: Visibility,
}

/// One `.set`, which is one name standing for another.
///
/// A name rather than anything else, because a name is what the thing on the right of one of
/// these is in every program that writes one: a second symbol at the first one's address, which
/// is an alias and is the one shape of `.set` the module has somewhere to put. The two other
/// shapes, a number and a name with a distance on it, are refused where the template is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Set {
    /// The name being given.
    pub name: String,
    /// The name it stands for.
    pub target: String,
    /// How the linker sees the name being given, which is internal unless a directive said more.
    pub linkage: Linkage,
    /// How far outside a shared library that name reaches.
    pub visibility: Visibility,
}

/// Reads a template and gives back the globals it defines, in the order it defined them.
///
/// `read` is asked for the bytes of a file `.incbin` names. It is a function rather than a list
/// of directories so that the walk over a template stays something that can be tested without a
/// file system, and so that where an assembler looks for a file is decided in one place by the
/// caller.
///
/// # Errors
///
/// [`Failed::Unsupported`] for a template holding anything this does not read, which is an
/// instruction, a directive not in the set below, or a use of a name it cannot work out.
/// [`Failed::Missing`] for a file `.incbin` names that `read` could not give.
pub(crate) fn assemble(
    template: &str,
    read: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<Assembled, Failed> {
    let mut asm = Assembler::new(read);
    for statement in statements(template)? {
        asm.statement(&statement)?;
    }
    asm.finish()
}

/// Where a section is up to, and which section that is.
#[derive(Debug)]
struct Where {
    /// Which section this is.
    section: Section,
    /// How many bytes of it the block has written, which is where the next label lands.
    at: u64,
}

/// The global being filled in, which is the last label and what has been written since.
#[derive(Debug)]
struct Open {
    /// The label's name, and nothing when the bytes were written under no label.
    name: Option<String>,
    /// Which of [`Assembler::sections`] it is in.
    section: usize,
    /// How far into that section it starts, which is where the label stood.
    start: u64,
    /// What the label was aligned to.
    align: u32,
    /// Its image so far.
    items: Vec<Item>,
    /// How many bytes that is.
    size: u64,
}

/// What one of the directives that talk about a name says about it.
///
/// The five of them differ in the one word they put on the name and in nothing else, so they are
/// one function taking this rather than five that each walk a list of names.
#[derive(Debug, Clone, Copy)]
enum Said {
    /// How the linker sees the name. `.globl`, `.local` and `.weak`.
    Linkage(Linkage),
    /// How far outside a shared library it reaches. `.hidden` and `.protected`.
    Reach(Visibility),
}

/// What an expression worked out to, which is a number and the section it counts from.
///
/// A label is a number only once the linker has placed the section it is in, so what is known
/// here is the offset and which section it is an offset into. Two labels of one section subtract
/// to a number that does not depend on where the section went, which is the whole reason a
/// program writes `end - start`, and this is the record that makes the subtraction possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Value {
    /// Which of [`Assembler::sections`] the number counts from, and nothing for a plain number.
    section: Option<usize>,
    /// The number itself.
    offset: i64,
    /// Whether this is the position itself rather than a label standing at it, which is what `.`
    /// reads as. It matters because a label minus the position is a distance the linker can work
    /// out even across sections, and a label minus another label is not.
    here: bool,
    /// Whether this is how far something is from where it is being written rather than where the
    /// something is. The section and the offset are then the target's, and the position this is
    /// counted from is wherever the bytes holding it land.
    away: bool,
}

impl Value {
    /// A plain number, which counts from nothing.
    const fn number(offset: i64) -> Value {
        Value { section: None, offset, here: false, away: false }
    }

    /// A place, which is an offset into one of the sections the template has named.
    const fn place(section: usize, offset: i64) -> Value {
        Value { section: Some(section), offset, here: false, away: false }
    }
}

/// The walk over a template, and everything it has worked out so far.
struct Assembler<'a> {
    /// The sections the template has named, in the order it first named them.
    sections: Vec<Where>,
    /// Which of them is being written to.
    current: usize,
    /// The sections a `.pushsection` left to come back to, the last one first.
    stack: Vec<usize>,
    /// The globals finished so far.
    pieces: Vec<Piece>,
    /// How far into its section each of them starts, which is what a distance to a place inside
    /// one is worked out from. Beside the list rather than in a [`Piece`] because it is the
    /// reader's own bookkeeping and says nothing about the global itself.
    starts: Vec<u64>,
    /// The names a `.set` equated to another name, in the order they were written.
    sets: Vec<(String, String)>,
    /// The one being filled in.
    open: Option<Open>,
    /// Where every label of this block is.
    labels: HashMap<String, Value>,
    /// Where the last local label of each number is, which is what a reference ending in `b`
    /// asks for.
    locals: HashMap<i64, Value>,
    /// The names a directive exported, and how.
    linkage: HashMap<String, Linkage>,
    /// The names a directive said how far they reach.
    visibility: HashMap<String, Visibility>,
    /// What `.size` claimed each name is, to be held against what was written under it.
    sizes: HashMap<String, i64>,
    /// What the last alignment directive asked for and nothing has been aligned to yet.
    pending: u32,
    /// Where the bytes of a file come from.
    read: &'a mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
}

impl<'a> Assembler<'a> {
    /// A walk that has read nothing, standing in `.text` the way an assembler starts.
    fn new(read: &'a mut dyn FnMut(&str) -> Result<Vec<u8>, String>) -> Assembler<'a> {
        Assembler {
            sections: vec![Where { section: Section::Text, at: 0 }],
            current: 0,
            stack: Vec::new(),
            pieces: Vec::new(),
            starts: Vec::new(),
            sets: Vec::new(),
            open: None,
            labels: HashMap::new(),
            locals: HashMap::new(),
            linkage: HashMap::new(),
            visibility: HashMap::new(),
            sizes: HashMap::new(),
            pending: 1,
            read,
        }
    }

    /// Reads one statement, which is a label, a directive, or the end of the line.
    fn statement(&mut self, statement: &str) -> Result<(), Failed> {
        let rest = statement.trim();
        if rest.is_empty() {
            return Ok(());
        }
        let word = word_of(rest);
        if let Some(after) = rest[word.len()..].strip_prefix(':') {
            if word.is_empty() {
                return Err(unsupported("a label with no name at file scope".to_owned()));
            }
            self.label(word)?;
            return self.statement(after);
        }
        let operands = rest[word.len()..].trim();
        if !word.starts_with('.') {
            return Err(unsupported(format!("the instruction '{word}'")));
        }
        self.directive(word, operands)
    }

    /// One label, which closes the global above it and opens the one under it.
    ///
    /// A label written as a number is a local one and does none of that. It is a name that may be
    /// written again further down, so nothing outside the template can be given it and no symbol
    /// is wanted for it: what it is for is the distance from it to somewhere else in the same
    /// block, which is a number this reader works out. So it records where the position is and
    /// leaves the global being filled in alone, which is what keeps the bytes either side of one
    /// in the same global the way an assembler keeps them in the same section.
    fn label(&mut self, name: &str) -> Result<(), Failed> {
        if let Ok(number) = name.parse::<i64>() {
            // The padding an alignment asks for is written when the next byte is, so where this
            // one stands is not settled yet and the distance from it would be the one before the
            // padding. Refused rather than answered with the number that is to hand.
            if self.pending > 1 {
                return Err(unsupported(format!("an alignment in front of the label '{name}'")));
            }
            self.locals.insert(number, self.here());
            return Ok(());
        }
        if self.labels.contains_key(name) {
            return Err(unsupported(format!("the label '{name}' written twice")));
        }
        self.close();
        let align = std::mem::replace(&mut self.pending, 1);
        let at = round_up(self.sections[self.current].at, u64::from(align));
        self.sections[self.current].at = at;
        self.labels
            .insert(name.to_owned(), Value::place(self.current, i64::try_from(at).unwrap_or(0)));
        self.open = Some(Open {
            name: Some(name.to_owned()),
            section: self.current,
            start: at,
            align,
            items: Vec::new(),
            size: 0,
        });
        Ok(())
    }

    /// Where the position is, which is an offset into the section being written to.
    fn here(&self) -> Value {
        let at = self.sections[self.current].at;
        Value { here: true, ..Value::place(self.current, i64::try_from(at).unwrap_or(0)) }
    }

    /// One directive and the text after its name.
    fn directive(&mut self, name: &str, operands: &str) -> Result<(), Failed> {
        match name {
            ".text" => self.section(Section::Text),
            ".data" => self.section(Section::Data),
            ".bss" => self.section(Section::Bss),
            ".section" => self.section(self.named(operands)),
            // The same directive with a stack behind it, which is how a template writes something
            // into another section without having to know which section it was standing in.
            ".pushsection" => {
                self.stack.push(self.current);
                self.section(self.named(operands));
            }
            ".popsection" => {
                let Some(back) = self.stack.pop() else {
                    return Err(unsupported("a '.popsection' with nothing pushed".to_owned()));
                };
                self.move_to(back);
            }
            ".globl" | ".global" => self.says(operands, Said::Linkage(Linkage::External)),
            ".local" => self.says(operands, Said::Linkage(Linkage::Internal)),
            ".weak" => self.says(operands, Said::Linkage(Linkage::Weak)),
            ".hidden" | ".internal" => self.says(operands, Said::Reach(Visibility::Hidden)),
            ".protected" => self.says(operands, Said::Reach(Visibility::Protected)),
            // What a name is is recorded by the object writer from what the global holds, so
            // there is nothing here to keep. It is accepted because every exported symbol in
            // real assembly is written with one and refusing it would refuse the whole file.
            ".type" | ".ident" | ".file" | ".cfi_sections" => {}
            ".set" | ".equ" => self.equate(operands)?,
            ".size" => self.size(operands)?,
            ".balign" | ".align" => self.align(operands, false)?,
            ".p2align" => self.align(operands, true)?,
            ".byte" => self.ints(operands, 1)?,
            ".short" | ".hword" | ".word" | ".value" => self.ints(operands, 2)?,
            ".long" | ".int" => self.ints(operands, 4)?,
            ".quad" => self.ints(operands, 8)?,
            ".ascii" => self.ascii(operands, false)?,
            ".asciz" | ".string" => self.ascii(operands, true)?,
            ".zero" | ".space" | ".skip" => self.space(operands)?,
            ".incbin" => self.incbin(operands)?,
            _ => return Err(unsupported(format!("the '{name}' directive at file scope"))),
        }
        Ok(())
    }

    /// Which section a directive's operands named, which is the first of them.
    fn named(&self, operands: &str) -> Section {
        let named = split(operands).into_iter().next().unwrap_or_default();
        section_named(named.trim_matches('"'))
    }

    /// Moves to a section, which closes whatever was being written to the last one.
    fn section(&mut self, section: Section) {
        let index = match self.sections.iter().position(|held| held.section == section) {
            Some(index) => index,
            None => {
                self.sections.push(Where { section, at: 0 });
                self.sections.len() - 1
            }
        };
        self.move_to(index);
    }

    /// The same, to a section this template has already been in.
    fn move_to(&mut self, index: usize) {
        self.close();
        self.pending = 1;
        self.current = index;
    }

    /// A directive that says one thing about each of the names it lists.
    fn says(&mut self, operands: &str, said: Said) {
        for name in split(operands) {
            let name = name.trim().to_owned();
            match said {
                Said::Linkage(linkage) => {
                    self.linkage.insert(name, linkage);
                }
                Said::Reach(visibility) => {
                    self.visibility.insert(name, visibility);
                }
            }
        }
    }

    /// `.set name, thing` and `.equ`, which are one directive under two names.
    ///
    /// Only a name on the right, which is what an alias is. A number there is an absolute symbol,
    /// which is a thing the object writers have no way to say, and a name with a distance on it is
    /// an address rather than a symbol, which is a thing an alias has no room for. Both are
    /// refused with what was written in the message, so a program that needs one says so itself.
    fn equate(&mut self, operands: &str) -> Result<(), Failed> {
        let parts = split(operands);
        let [name, thing] = parts.as_slice() else {
            return Err(unsupported("a '.set' that is not a name and a thing".to_owned()));
        };
        let (name, thing) = (name.trim(), thing.trim());
        if !is_a_name(name) {
            return Err(unsupported(format!("a '.set' of '{name}'")));
        }
        if !is_a_name(thing) {
            return Err(unsupported(format!("a '.set' of '{name}' to '{thing}'")));
        }
        self.sets.push((name.to_owned(), thing.to_owned()));
        Ok(())
    }

    /// `.size name, expression`, kept so it can be held against what was written.
    fn size(&mut self, operands: &str) -> Result<(), Failed> {
        let parts = split(operands);
        let [name, expression] = parts.as_slice() else {
            return Err(unsupported(
                "a '.size' directive that is not a name and a size".to_owned(),
            ));
        };
        let value = self.value(expression)?;
        if value.section.is_some() {
            return Err(unsupported(format!("a '.size' of '{name}' that is not a number")));
        }
        self.sizes.insert(name.trim().to_owned(), value.offset);
        Ok(())
    }

    /// An alignment directive, whose operand is a count of bytes or a power of two.
    fn align(&mut self, operands: &str, power: bool) -> Result<(), Failed> {
        let parts = split(operands);
        let first = parts.first().map_or("", |text| text.trim());
        if first.is_empty() {
            return Ok(());
        }
        let asked = self.value(first)?;
        if asked.section.is_some() {
            return Err(unsupported("an alignment that is not a number".to_owned()));
        }
        let Ok(asked) = u32::try_from(asked.offset) else {
            return Err(unsupported(format!("an alignment of {}", asked.offset)));
        };
        let bytes = if power {
            if asked >= 32 {
                return Err(unsupported(format!("an alignment of two to the {asked}")));
            }
            1u32 << asked
        } else {
            asked
        };
        if bytes == 0 || !bytes.is_power_of_two() {
            return Err(unsupported(format!("an alignment of {bytes}")));
        }
        self.pending = self.pending.max(bytes);
        Ok(())
    }

    /// A run of integers of one width, which is what the data directives are.
    fn ints(&mut self, operands: &str, width: u8) -> Result<(), Failed> {
        for operand in split(operands) {
            let value = self.value(&operand)?;
            if value.away {
                self.distance(&operand, value, width)?;
                continue;
            }
            if value.section.is_some() {
                return Err(unsupported(format!(
                    "the address of '{}' in an image",
                    operand.trim()
                )));
            }
            self.emit(Item::Int { width, value: value.offset })?;
        }
        Ok(())
    }

    /// How far one of the template's own places is from these bytes, which is written rather than
    /// worked out because where the bytes land is the linker's answer and not this reader's.
    ///
    /// Four bytes wide only, which is the width the relocation for it has on this target and the
    /// width every program that writes one of these uses. The offset of the position the
    /// expression subtracted is not wanted and was dropped: what the relocation is resolved
    /// against is where the four bytes themselves end up, which is the same place.
    fn distance(&mut self, operand: &str, value: Value, width: u8) -> Result<(), Failed> {
        let operand = operand.trim();
        if width != 4 {
            return Err(unsupported(format!("a distance from here {width} bytes wide")));
        }
        let Some(section) = value.section else {
            return Err(unsupported(format!("the distance '{operand}'")));
        };
        let Some((piece, addend)) = self.spot(section, value.offset) else {
            return Err(unsupported(format!(
                "the distance '{operand}', which measures to nothing this block wrote"
            )));
        };
        self.emit(Item::Away { piece, addend })
    }

    /// Which global of a section holds that offset into it, and how far into that global it is.
    ///
    /// A relocation names a symbol, and the place a distance is measured to is usually a local
    /// label, which is no symbol at all. So the label is turned into the global it stands inside
    /// and the distance from the front of that global, which names the same place in terms the
    /// object file has a symbol for. Only the globals already finished are walked, because the one
    /// still open is in the section being written to and a place in that section is a plain number
    /// rather than a distance.
    fn spot(&self, section: usize, offset: i64) -> Option<(usize, i64)> {
        let wanted = &self.sections[section].section;
        let mut found = None;
        for (index, piece) in self.pieces.iter().enumerate() {
            if piece.section != *wanted {
                continue;
            }
            let start = i64::try_from(self.starts[index]).unwrap_or(i64::MAX);
            if start > offset {
                break;
            }
            found = Some((index, offset - start));
        }
        found
    }

    /// A run of strings, with a terminator after each one under `.asciz`.
    fn ascii(&mut self, operands: &str, terminated: bool) -> Result<(), Failed> {
        for operand in split(operands) {
            let mut bytes = string_of(operand.trim())?;
            if terminated {
                bytes.push(0);
            }
            self.emit(Item::Bytes(bytes))?;
        }
        Ok(())
    }

    /// `.zero`, `.space` and `.skip`, which are that many bytes of the fill, and the fill is zero.
    fn space(&mut self, operands: &str) -> Result<(), Failed> {
        let parts = split(operands);
        let [count] = parts.as_slice() else {
            return Err(unsupported("a run of a fill that is not zero".to_owned()));
        };
        let value = self.value(count)?;
        let Ok(bytes) = u64::try_from(value.offset) else {
            return Err(unsupported(format!("a run of {} bytes", value.offset)));
        };
        self.zeros(bytes)
    }

    /// `.incbin`, which is the bytes of a file and is the whole reason this reader exists.
    fn incbin(&mut self, operands: &str) -> Result<(), Failed> {
        let parts = split(operands);
        let [name] = parts.as_slice() else {
            return Err(unsupported("an '.incbin' that skips or counts bytes".to_owned()));
        };
        let name = String::from_utf8(string_of(name.trim())?)
            .map_err(|_| unsupported("an '.incbin' of a name that is not text".to_owned()))?;
        match (self.read)(&name) {
            Ok(bytes) => self.emit(Item::Bytes(bytes)),
            Err(why) => Err(Failed::Missing(name, why)),
        }
    }

    /// That many zero bytes, said the way the section they go in can carry them.
    fn zeros(&mut self, bytes: u64) -> Result<(), Failed> {
        if bytes == 0 {
            return Ok(());
        }
        if self.sections[self.current].section == Section::Bss {
            return self.emit(Item::Zero(bytes));
        }
        self.emit(Item::Bytes(vec![0; usize::try_from(bytes).unwrap_or(usize::MAX)]))
    }

    /// Writes one piece of an image, after the padding any alignment directive in front of it
    /// asked for.
    fn emit(&mut self, item: Item) -> Result<(), Failed> {
        let pending = std::mem::replace(&mut self.pending, 1);
        if pending > 1 {
            let at = self.sections[self.current].at;
            let padding = round_up(at, u64::from(pending)) - at;
            if padding > 0 {
                self.put(Item::Bytes(vec![0; usize::try_from(padding).unwrap_or(usize::MAX)]))?;
            }
        }
        self.put(item)
    }

    /// Writes one piece of an image where the position already is.
    fn put(&mut self, item: Item) -> Result<(), Failed> {
        let section = &self.sections[self.current].section;
        if *section == Section::Text {
            return Err(unsupported("data in the text section at file scope".to_owned()));
        }
        if *section == Section::Bss && !matches!(item, Item::Zero(_)) {
            return Err(unsupported("data in a section that carries none".to_owned()));
        }
        // Bytes under no label, which are bytes all the same. They become a global of their own
        // with a name the walk mints, and they land in front of whatever label comes next because
        // the globals of one template are added to the module in the order it wrote them.
        let open = match self.open.as_mut() {
            Some(open) => open,
            // The alignment is one because whatever the last directive asked for has already been
            // written as the padding in front of these bytes, which is what an alignment in front
            // of data is here and is what it is in an assembler.
            None => self.open.insert(Open {
                name: None,
                section: self.current,
                start: self.sections[self.current].at,
                align: 1,
                items: Vec::new(),
                size: 0,
            }),
        };
        let size = item.size();
        open.size += size;
        open.items.push(item);
        self.sections[self.current].at += size;
        Ok(())
    }

    /// Finishes the global being written, if there is one.
    fn close(&mut self) {
        let Some(open) = self.open.take() else { return };
        self.starts.push(open.start);
        self.pieces.push(Piece {
            name: open.name,
            section: self.sections[open.section].section.clone(),
            align: open.align,
            items: open.items,
            size: open.size,
            linkage: Linkage::Internal,
            visibility: Visibility::Default,
        });
    }

    /// The globals the template defined, with what the directives said about the names put on.
    fn finish(mut self) -> Result<Assembled, Failed> {
        self.close();
        // A name in the text section is a function, and what is under it here is nothing, since
        // anything that would have been is an instruction and was refused where it was written.
        // Only a label gets here, since bytes written into the text section were refused where
        // they were written and a label is therefore the only thing that puts a piece in it.
        if let Some(name) = self
            .pieces
            .iter()
            .find(|piece| piece.section == Section::Text)
            .and_then(|piece| piece.name.as_deref())
        {
            return Err(unsupported(format!("the label '{name}' in the text section")));
        }
        for piece in &mut self.pieces {
            // Bytes under no label have no name for a directive to have said anything about, so
            // they stay internal and nothing can have claimed a size for them either.
            let Some(name) = piece.name.as_deref() else { continue };
            piece.linkage = self.linkage.get(name).copied().unwrap_or(Linkage::Internal);
            piece.visibility = self.visibility.get(name).copied().unwrap_or(Visibility::Default);
            if let Some(&said) = self.sizes.get(name) {
                if u64::try_from(said) != Ok(piece.size) {
                    return Err(unsupported(format!(
                        "a '.size' of '{name}' that is not what was written under it"
                    )));
                }
            }
        }
        // The offsets above are counted from the start of the block and the block starts wherever
        // the section had got to, so a label two hundred and fifty six bytes in is at a multiple
        // of two hundred and fifty six only if the block is. Giving the first global of a section
        // the largest alignment anything in that section asked for is what makes that true, and
        // it is the same thing an assembler does when it records a section's alignment.
        for index in 0..self.sections.len() {
            let section = self.sections[index].section.clone();
            let largest =
                self.pieces.iter().filter(|p| p.section == section).map(|p| p.align).max();
            let Some(largest) = largest else { continue };
            if let Some(first) = self.pieces.iter_mut().find(|p| p.section == section) {
                first.align = largest;
            }
        }
        // A name a `.set` gave takes what the directives said about it from the same two maps the
        // labels take theirs from, since `.weak name` says the same thing about a name whichever
        // of the two ways the name came to stand for something.
        let sets = self
            .sets
            .iter()
            .map(|(name, target)| Set {
                name: name.clone(),
                target: target.clone(),
                linkage: self.linkage.get(name).copied().unwrap_or(Linkage::Internal),
                visibility: self.visibility.get(name).copied().unwrap_or(Visibility::Default),
            })
            .collect();
        Ok(Assembled { pieces: self.pieces, sets })
    }

    /// What one expression works out to.
    fn value(&self, text: &str) -> Result<Value, Failed> {
        let mut chars: Vec<char> = text.chars().collect();
        chars.retain(|c| !c.is_whitespace());
        let mut at = 0;
        let value = self.bitwise(&chars, &mut at)?;
        if at != chars.len() {
            return Err(unsupported(format!("the expression '{}'", text.trim())));
        }
        Ok(value)
    }

    /// The loosest binding operators, which are the bitwise ones.
    fn bitwise(&self, text: &[char], at: &mut usize) -> Result<Value, Failed> {
        let mut left = self.shift(text, at)?;
        while let Some(&c) = text.get(*at) {
            if !matches!(c, '|' | '&' | '^') {
                break;
            }
            *at += 1;
            let right = self.shift(text, at)?;
            let (a, b) = (absolute(left, text)?, absolute(right, text)?);
            left = Value::number(match c {
                '|' => a | b,
                '&' => a & b,
                _ => a ^ b,
            });
        }
        Ok(left)
    }

    /// The shifts, which bind tighter than the bitwise operators and looser than addition.
    fn shift(&self, text: &[char], at: &mut usize) -> Result<Value, Failed> {
        let mut left = self.sum(text, at)?;
        while let (Some(&c), Some(&next)) = (text.get(*at), text.get(*at + 1)) {
            if c != next || !matches!(c, '<' | '>') {
                break;
            }
            *at += 2;
            let right = self.sum(text, at)?;
            let (a, b) = (absolute(left, text)?, absolute(right, text)?);
            let by = u32::try_from(b).unwrap_or(64);
            left = Value::number(if c == '<' {
                a.checked_shl(by).unwrap_or(0)
            } else {
                a.checked_shr(by).unwrap_or(0)
            });
        }
        Ok(left)
    }

    /// Addition and subtraction, which is where a label may still be one side.
    fn sum(&self, text: &[char], at: &mut usize) -> Result<Value, Failed> {
        let mut left = self.product(text, at)?;
        while let Some(&c) = text.get(*at) {
            if !matches!(c, '+' | '-') {
                break;
            }
            *at += 1;
            let right = self.product(text, at)?;
            left = if c == '+' { add(left, right, text)? } else { subtract(left, right, text)? };
        }
        Ok(left)
    }

    /// Multiplication and its two relatives.
    fn product(&self, text: &[char], at: &mut usize) -> Result<Value, Failed> {
        let mut left = self.unary(text, at)?;
        while let Some(&c) = text.get(*at) {
            if !matches!(c, '*' | '/' | '%') {
                break;
            }
            *at += 1;
            let right = self.unary(text, at)?;
            let (a, b) = (absolute(left, text)?, absolute(right, text)?);
            if b == 0 && c != '*' {
                return Err(unsupported(format!("a division by zero in '{}'", written(text))));
            }
            left = Value::number(match c {
                '*' => a.wrapping_mul(b),
                '/' => a.wrapping_div(b),
                _ => a.wrapping_rem(b),
            });
        }
        Ok(left)
    }

    /// A sign or a complement in front of something, and the something on its own.
    fn unary(&self, text: &[char], at: &mut usize) -> Result<Value, Failed> {
        match text.get(*at) {
            Some('-') => {
                *at += 1;
                let value = self.unary(text, at)?;
                Ok(Value::number(absolute(value, text)?.wrapping_neg()))
            }
            Some('+') => {
                *at += 1;
                self.unary(text, at)
            }
            Some('~') => {
                *at += 1;
                let value = self.unary(text, at)?;
                Ok(Value::number(!absolute(value, text)?))
            }
            _ => self.atom(text, at),
        }
    }

    /// Where a local label of that number is, looking the way the letter behind it said.
    ///
    /// Backwards only. A reference to one below where it is written is a thing an assembler
    /// answers by going round twice, and this reader goes round once, so it is refused by name
    /// rather than answered with the last one above, which would be a different label.
    fn local(&self, number: i64, which: char) -> Result<Value, Failed> {
        if which == 'f' {
            return Err(unsupported(format!("the local label '{number}f', which is below it")));
        }
        self.locals
            .get(&number)
            .copied()
            .ok_or_else(|| unsupported(format!("the local label '{number}b', which has no label")))
    }

    /// A number, a character, a label, the position, or a bracketed expression.
    fn atom(&self, text: &[char], at: &mut usize) -> Result<Value, Failed> {
        match text.get(*at) {
            Some('(') => {
                *at += 1;
                let value = self.bitwise(text, at)?;
                if text.get(*at) != Some(&')') {
                    return Err(unsupported(format!("the expression '{}'", written(text))));
                }
                *at += 1;
                Ok(value)
            }
            Some('\'') => {
                *at += 1;
                let Some(&c) = text.get(*at) else {
                    return Err(unsupported(format!("the expression '{}'", written(text))));
                };
                *at += 1;
                if text.get(*at) == Some(&'\'') {
                    *at += 1;
                }
                Ok(Value::number(i64::from(u32::from(c))))
            }
            Some(c) if c.is_ascii_digit() => {
                let value = number(text, at)?;
                // A number with a `b` or an `f` behind it is a reference to a local label rather
                // than a number, and the letter says which way to look for it.
                let behind = text.get(*at).copied();
                let joined = text.get(*at + 1).copied().is_some_and(is_name);
                match behind {
                    Some(which @ ('b' | 'f')) if !joined => {
                        *at += 1;
                        self.local(value.offset, which)
                    }
                    _ => Ok(value),
                }
            }
            Some(c) if is_name(*c) => {
                let start = *at;
                while text.get(*at).is_some_and(|&c| is_name(c)) {
                    *at += 1;
                }
                let name: String = text[start..*at].iter().collect();
                if name == "." {
                    return Ok(self.here());
                }
                self.labels
                    .get(&name)
                    .copied()
                    .ok_or_else(|| unsupported(format!("the name '{name}' in an expression")))
            }
            _ => Err(unsupported(format!("the expression '{}'", written(text)))),
        }
    }
}

/// Whether that character may be part of a name, which is more than C allows.
fn is_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$')
}

/// Whether that text is one name and nothing else.
///
/// A name that starts with a digit is a local label, which is a name that may be written again
/// further down and is therefore not a name anything outside the template can be given. This is
/// where one written in a `.set` is turned down, since the thing a `.set` names is a symbol.
fn is_a_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| is_name(c) && !c.is_ascii_digit()) && chars.all(is_name)
}

/// A number in one of the four bases an assembler writes them in.
fn number(text: &[char], at: &mut usize) -> Result<Value, Failed> {
    let start = *at;
    let (radix, from) = match (text.get(*at), text.get(*at + 1)) {
        (Some('0'), Some('x' | 'X')) => (16, start + 2),
        (Some('0'), Some('b' | 'B')) => (2, start + 2),
        (Some('0'), Some(c)) if c.is_ascii_digit() => (8, start + 1),
        _ => (10, start),
    };
    *at = from;
    while text.get(*at).is_some_and(|c| c.is_digit(radix)) {
        *at += 1;
    }
    let digits: String = text[from..*at].iter().collect();
    if digits.is_empty() {
        // A lone zero, whose one digit was read as the marker of an octal number.
        *at = start + 1;
        return Ok(Value::number(0));
    }
    i64::from_str_radix(&digits, radix)
        .map(Value::number)
        .map_err(|_| unsupported(format!("the number '{digits}'")))
}

/// A value that has to be a plain number, and the message for one that is not.
fn absolute(value: Value, text: &[char]) -> Result<i64, Failed> {
    match value.section {
        None => Ok(value.offset),
        Some(_) => Err(unsupported(format!("an address in the expression '{}'", written(text)))),
    }
}

/// The sum of two values, where at most one of them may count from a section.
fn add(left: Value, right: Value, text: &[char]) -> Result<Value, Failed> {
    let section = match (left.section, right.section) {
        (None, other) | (other, None) => other,
        (Some(_), Some(_)) => {
            return Err(unsupported(format!("two addresses added in '{}'", written(text))));
        }
    };
    let offset = left.offset.wrapping_add(right.offset);
    // A number added to a distance moves the place the distance is measured to, since that is
    // what the offset of one holds, so the sum is a distance to the place that far further on.
    Ok(Value { section, offset, here: false, away: left.away || right.away })
}

/// The difference of two values, which is a plain number when both count from one section.
///
/// One that counts from another section is refused, with the one exception that is the reason the
/// subtraction is written at all: how far a place is from where the answer itself is written. That
/// is a relocation rather than a number, so the target is carried and the position is dropped, and
/// the linker works out the difference once it has put both sections somewhere.
fn subtract(left: Value, right: Value, text: &[char]) -> Result<Value, Failed> {
    let section = match (left.section, right.section) {
        (held, None) => held,
        (Some(a), Some(b)) if a == b => None,
        (Some(_), Some(_)) if right.here && !left.away => {
            return Ok(Value { away: true, here: false, ..left });
        }
        _ => {
            return Err(unsupported(format!(
                "a difference of addresses in two sections in '{}'",
                written(text)
            )));
        }
    };
    if section.is_none() && (left.away || right.away) {
        return Err(unsupported(format!("a distance taken away from in '{}'", written(text))));
    }
    let offset = left.offset.wrapping_sub(right.offset);
    Ok(Value { section, offset, here: false, away: left.away })
}

/// The characters of an expression, for a message about it.
fn written(text: &[char]) -> String {
    text.iter().collect()
}

/// The first word of a statement, which is a label's name or a directive's.
fn word_of(text: &str) -> &str {
    let end = text.find(|c: char| c.is_whitespace() || c == ':' || c == ',').unwrap_or(text.len());
    &text[..end]
}

/// Which section a name stands for, with the four an object writer has answers of its own for
/// told apart from the rest.
fn section_named(name: &str) -> Section {
    match name {
        ".text" => Section::Text,
        ".data" => Section::Data,
        ".rodata" | ".const_data" | ".const" => Section::ReadOnly,
        ".bss" => Section::Bss,
        _ => Section::Named(name.to_owned()),
    }
}

/// The bytes of a quoted string, with the escapes an assembler reads.
fn string_of(text: &str) -> Result<Vec<u8>, Failed> {
    let inner = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or_else(|| unsupported(format!("the operand '{text}'")))?;
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buffer = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => bytes.push(b'\n'),
            Some('t') => bytes.push(b'\t'),
            Some('r') => bytes.push(b'\r'),
            Some('0') => bytes.push(0),
            Some('\\') => bytes.push(b'\\'),
            Some('"') => bytes.push(b'"'),
            Some(other) => {
                return Err(unsupported(format!("the escape '\\{other}' in a string")));
            }
            None => return Err(unsupported(format!("the operand '{text}'"))),
        }
    }
    Ok(bytes)
}

/// Splits a directive's operands on the commas between them, leaving the ones inside a string or
/// a bracket where they are.
fn split(operands: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut held = String::new();
    let mut quoted = false;
    let mut depth = 0u32;
    for c in operands.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                held.push(c);
            }
            '(' if !quoted => {
                depth += 1;
                held.push(c);
            }
            ')' if !quoted => {
                depth = depth.saturating_sub(1);
                held.push(c);
            }
            ',' if !quoted && depth == 0 => parts.push(std::mem::take(&mut held)),
            _ => held.push(c),
        }
    }
    if !held.trim().is_empty() || !parts.is_empty() {
        parts.push(held);
    }
    parts.into_iter().filter(|part| !part.trim().is_empty()).collect()
}

/// Cuts a template into the statements it is made of, with the comments taken out.
///
/// A newline and a semicolon both end one, which is what an assembler does on every target this
/// compiles for. A `#` runs to the end of the line and a `/*` runs to its closing pair, and
/// neither of them means anything inside a string, which is why this is a walk over characters
/// rather than a split.
fn statements(template: &str) -> Result<Vec<String>, Failed> {
    let mut out = Vec::new();
    let mut held = String::new();
    let mut chars = template.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        if quoted {
            held.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    held.push(next);
                }
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                held.push(c);
            }
            '#' => {
                while chars.peek().is_some_and(|&next| next != '\n') {
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut closed = false;
                while let Some(next) = chars.next() {
                    if next == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    return Err(unsupported("a comment with no end in it".to_owned()));
                }
            }
            '\n' | ';' => out.push(std::mem::take(&mut held)),
            _ => held.push(c),
        }
    }
    out.push(held);
    Ok(out)
}

/// The refusal for something this does not read.
fn unsupported(what: String) -> Failed {
    Failed::Unsupported(what)
}

/// That number rounded up to a multiple of the alignment.
fn round_up(at: u64, align: u64) -> u64 {
    match align {
        0 | 1 => at,
        _ => at.div_ceil(align) * align,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a template, with no file for `.incbin` to find, and keeps the globals.
    fn read(template: &str) -> Result<Vec<Piece>, Failed> {
        all_of(template).map(|read| read.pieces)
    }

    /// The same, keeping what it said rather than only the globals.
    fn all_of(template: &str) -> Result<Assembled, Failed> {
        assemble(template, &mut |name| Err(format!("no file '{name}'")))
    }

    /// Reads a template, with one file `.incbin` may name.
    fn with_file(template: &str, bytes: &[u8]) -> Result<Vec<Piece>, Failed> {
        let held = bytes.to_vec();
        assemble(template, &mut |_| Ok(held.clone())).map(|read| read.pieces)
    }

    /// What each piece is called, with an empty name for the bytes written under no label.
    fn named(pieces: &[Piece]) -> Vec<&str> {
        pieces.iter().map(|piece| piece.name.as_deref().unwrap_or_default()).collect()
    }

    /// The bytes a piece's image adds up to, with a zero run written out.
    fn image(piece: &Piece) -> Vec<u8> {
        let mut bytes = Vec::new();
        for item in &piece.items {
            match item {
                Item::Bytes(held) => bytes.extend_from_slice(held),
                Item::Int { width, value } => {
                    bytes.extend_from_slice(&value.to_le_bytes()[..usize::from(*width)]);
                }
                Item::Zero(count) => {
                    let want = bytes.len() + usize::try_from(*count).expect("a run that fits");
                    bytes.resize(want, 0);
                }
                // A hole the linker fills in, which is zeros until it does. The tests about one
                // read the item rather than these bytes, since what it says is the whole of it.
                Item::Away { .. } => bytes.extend_from_slice(&[0; 4]),
            }
        }
        bytes
    }

    /// What the incbin header writes, which is the whole reason this reader exists.
    ///
    /// Three names, the middle one at the end of the first one's bytes and the third holding the
    /// distance between them. The test is that the three land where the header's own arithmetic
    /// says they do, since a program that reads the size and walks to the end symbol is reading
    /// that and nothing else.
    #[test]
    fn a_file_pulled_in_by_incbin_becomes_the_three_names_the_header_promises() {
        let template = "\
.section .rodata
.global gData
.type gData, @object
.balign 16
gData:
.incbin \"file.bin\"
.global gEnd
.type gEnd, @object
.balign 1
gEnd:
.byte 1
.global gSize
.type gSize, @object
.balign 16
gSize:
.int gEnd - gData
.balign 16
.text
";
        let pieces = with_file(template, &[7u8; 962]).expect("the template is read");
        let names = named(&pieces);
        assert_eq!(names, ["gData", "gEnd", "gSize"]);
        assert!(pieces.iter().all(|piece| piece.section == Section::ReadOnly));
        assert!(pieces.iter().all(|piece| piece.linkage == Linkage::External));

        assert_eq!(pieces[0].size, 962);
        assert_eq!(pieces[0].align, 16, "the first of a section carries the largest alignment");
        assert_eq!(pieces[1].size, 1, "the end marker is the one byte written under it");
        assert_eq!(pieces[1].align, 1, "and is where the file's bytes stopped");
        assert_eq!(pieces[2].items, [Item::Int { width: 4, value: 962 }], "the size is a distance");
        assert_eq!(pieces[2].align, 16);
    }

    /// An alignment in front of a label is the label's, and one in front of data is padding.
    #[test]
    fn an_alignment_moves_the_next_label_and_pads_in_front_of_the_next_byte() {
        let pieces =
            read(".data\nx:\n.byte 1\n.balign 4\n.byte 2\n").expect("the template is read");
        assert_eq!(pieces.len(), 1);
        assert_eq!(image(&pieces[0]), [1, 0, 0, 0, 2]);

        let pieces = read(".data\nx:\n.byte 1\n.balign 4\ny:\n.byte 2\n").expect("read");
        assert_eq!(pieces.len(), 2);
        assert_eq!(image(&pieces[0]), [1]);
        assert_eq!(pieces[1].align, 4, "the label took the alignment rather than the bytes");
        assert_eq!(pieces[0].align, 4, "and the first of the section carries the largest");
    }

    /// The bytes a template writes before it writes any label, which tcc's test file starts with.
    ///
    /// They are a run of their own with no name, in the same section as the label that follows
    /// them and written before it, which is what makes the first byte under that label the one
    /// written under it rather than the one written above.
    #[test]
    fn bytes_written_before_any_label_are_a_run_of_their_own_in_front_of_it() {
        let pieces = read(".data\n.byte 41\nstuff:\n.byte 42\n").expect("the template is read");
        assert_eq!(named(&pieces), ["", "stuff"]);
        assert_eq!(image(&pieces[0]), [41]);
        assert_eq!(image(&pieces[1]), [42]);
        assert_eq!(pieces[0].section, Section::Data);
        assert_eq!(pieces[0].linkage, Linkage::Internal, "nothing can have said otherwise");

        // The same where a section directive is what closed the run above, since moving to a
        // section finishes what was being written the way a label does.
        let pieces = read(".data\nx:\n.byte 1\n.bss\n.zero 4\n").expect("read");
        assert_eq!(named(&pieces), ["x", ""]);
        assert_eq!(pieces[1].items, [Item::Zero(4)]);
    }

    /// A label written as a number, which is a position to measure from and not a symbol.
    #[test]
    fn a_local_label_is_a_place_the_bytes_around_it_can_measure_from() {
        let template = ".data\nstuff:\n661:\n.byte 42\n662:\n.byte 662b - 661b\n";
        let pieces = read(template).expect("the template is read");
        assert_eq!(named(&pieces), ["stuff"], "a number defines nothing and closes nothing");
        assert_eq!(image(&pieces[0]), [42, 1]);

        // Written again further down, where what a reference means is the last one above it.
        let template = ".data\nx:\n1:\n.byte 7\n.byte 1b - x\n1:\n.byte 1b - x\n";
        let pieces = read(template).expect("the template is read");
        assert_eq!(image(&pieces[0]), [7, 0, 2]);
    }

    /// `.pushsection` and `.popsection`, which is how a template writes into another section
    /// without having to know which one it was standing in.
    #[test]
    fn a_pushed_section_is_gone_back_to_where_it_was_left_when_the_template_pops() {
        let template = ".data\nx:\n.byte 1\n.pushsection .init_array\ny:\n.quad 0\n\
                        .popsection\n.byte 2\n";
        let pieces = read(template).expect("the template is read");
        assert_eq!(named(&pieces), ["x", "y", ""]);
        assert_eq!(pieces[0].section, Section::Data);
        assert_eq!(pieces[1].section, Section::Named(".init_array".to_owned()));
        assert_eq!(pieces[2].section, Section::Data, "which is where the pop went back to");
        assert_eq!(image(&pieces[2]), [2]);
    }

    /// How far a place is from the bytes that say so, which is the line tcc's test file ends its
    /// second block with and is the shape every alternative instruction table in a kernel header
    /// writes: a record in one section holding the distance to the code in another.
    #[test]
    fn a_distance_from_here_is_a_relocation_to_the_global_the_place_is_inside() {
        let template = ".data\n.byte 41\nstuff:\n661:\n.byte 42\n\
                        .pushsection .data.ignore\n.long 661b - .\n.popsection\n";
        let pieces = read(template).expect("the template is read");
        assert_eq!(named(&pieces), ["", "stuff", ""]);
        // The place is named as the global it stands inside and how far into it, because it was
        // written as a local label and no symbol is held for one of those.
        assert_eq!(pieces[2].items, [Item::Away { piece: 1, addend: 0 }]);
        assert_eq!(pieces[2].size, 4, "which is four bytes of image like any other long");

        // A place further into the global, which is the same global and a distance into it.
        let template = ".data\nstuff:\n.byte 42\n661:\n.byte 43\n\
                        .pushsection .data.ignore\n.long 661b - .\n.popsection\n";
        let pieces = read(template).expect("the template is read");
        assert_eq!(pieces[1].items, [Item::Away { piece: 0, addend: 1 }]);
    }

    /// The same subtraction inside one section, which is a number and stays one.
    #[test]
    fn a_distance_to_a_place_in_the_same_section_is_worked_out_here() {
        let pieces = read(".data\nx:\n.byte 7\n.long x - .\n").expect("read");
        assert_eq!(
            pieces[0].items,
            [Item::Int { width: 1, value: 7 }, Item::Int { width: 4, value: -1 }]
        );
    }

    /// What a distance is measured from is where the bytes holding it land, which the linker
    /// knows and this does not, so an alignment in front of them changes nothing here.
    #[test]
    fn an_alignment_in_front_of_a_distance_leaves_the_distance_alone() {
        let template = ".data\nstuff:\n661:\n.byte 42\n\
                        .pushsection .data.ignore\n.byte 1\n.balign 4\n.long 661b - .\n\
                        .popsection\n";
        let pieces = read(template).expect("the template is read");
        assert_eq!(
            pieces[1].items,
            [
                Item::Int { width: 1, value: 1 },
                Item::Bytes(vec![0, 0, 0]),
                Item::Away { piece: 0, addend: 0 },
            ]
        );
    }

    /// A name is internal unless something exported it, which is what an assembler does.
    #[test]
    fn a_label_nothing_exported_is_one_the_linker_does_not_see() {
        let pieces = read(".data\nx:\n.byte 1\n.globl y\ny:\n.byte 2\n").expect("read");
        assert_eq!(pieces[0].linkage, Linkage::Internal);
        assert_eq!(pieces[1].linkage, Linkage::External);

        let pieces = read(".data\n.weak w\n.hidden w\nw:\n.byte 1\n").expect("read");
        assert_eq!(pieces[0].linkage, Linkage::Weak);
        assert_eq!(pieces[0].visibility, Visibility::Hidden);
    }

    /// The sections the four bare directives name, and one the template named itself.
    #[test]
    fn the_section_a_label_is_in_is_the_one_the_directive_before_it_named() {
        let template = ".section .rodata\na:\n.byte 1\n.data\nb:\n.byte 2\n\
                        .bss\nc:\n.zero 8\n.section .init_array\nd:\n.quad 0\n";
        let pieces = read(template).expect("the template is read");
        let sections: Vec<&Section> = pieces.iter().map(|piece| &piece.section).collect();
        assert_eq!(
            sections,
            [
                &Section::ReadOnly,
                &Section::Data,
                &Section::Bss,
                &Section::Named(".init_array".to_owned()),
            ]
        );
        assert_eq!(pieces[2].items, [Item::Zero(8)], "a section with no image says how much");
    }

    /// The arithmetic an assembler does, which is more than the one subtraction incbin needs.
    #[test]
    fn an_expression_is_worked_out_the_way_an_assembler_works_one_out() {
        let cases = [
            (".byte 1 + 2 * 3", vec![7u8]),
            (".byte (1 + 2) * 3", vec![9]),
            (".byte 0x10", vec![16]),
            (".byte 0b101", vec![5]),
            (".byte 010", vec![8]),
            (".byte 0", vec![0]),
            (".byte 'A'", vec![65]),
            (".byte ~0 & 0xff", vec![255]),
            (".byte 1 << 3", vec![8]),
            (".byte -1 & 3", vec![3]),
            (".short 0x1234", vec![0x34, 0x12]),
        ];
        for (directive, want) in cases {
            let pieces = read(&format!(".data\nx:\n{directive}\n")).expect(directive);
            assert_eq!(image(&pieces[0]), want, "{directive}");
        }
    }

    /// A string directive is bytes, and `.asciz` is the same bytes with a terminator.
    #[test]
    fn a_string_directive_is_the_bytes_of_the_string_it_was_given() {
        let pieces = read(".data\nx:\n.ascii \"hi\"\n.asciz \"yo\"\n").expect("read");
        assert_eq!(image(&pieces[0]), b"hiyo\0");

        let pieces = read(".data\nx:\n.string \"a\\nb\"\n").expect("read");
        assert_eq!(image(&pieces[0]), b"a\nb\0");
    }

    /// Comments and semicolons, since a template written in C is one long string either way.
    #[test]
    fn a_comment_and_a_semicolon_end_what_an_assembler_says_they_end() {
        let pieces =
            read(".data; x: ; .byte 1 # and the rest of the line\n.byte 2\n").expect("read");
        assert_eq!(image(&pieces[0]), [1, 2]);

        let pieces = read(".data\nx:\n/* nothing */ .byte 3\n").expect("read");
        assert_eq!(image(&pieces[0]), [3]);
    }

    /// `.set` is a second name, and what the directives around it said about that name.
    ///
    /// The template is the one tcc's test file writes, which is a weak name equated to a function
    /// the file defines. The name is weak because `.weak` said so and the one below it is local
    /// because nothing said anything, which is an assembler's answer and not a default chosen
    /// here: a name is only in the symbol table for other objects to see when a directive asked.
    #[test]
    fn a_set_is_one_name_standing_for_another_with_what_was_said_about_it() {
        let read = all_of(".weak one\n.set one, base\n.set two, base\n").expect("read");
        assert!(read.pieces.is_empty(), "a name for something else defines nothing of its own");
        assert_eq!(
            read.sets,
            [
                Set {
                    name: "one".to_owned(),
                    target: "base".to_owned(),
                    linkage: Linkage::Weak,
                    visibility: Visibility::Default,
                },
                Set {
                    name: "two".to_owned(),
                    target: "base".to_owned(),
                    linkage: Linkage::Internal,
                    visibility: Visibility::Default,
                },
            ]
        );

        // `.equ` is the same directive under its other name, and a label of this template is a
        // name like any other to stand for.
        let read = all_of(".data\nx:\n.byte 1\n.globl y\n.equ y, x\n").expect("read");
        assert_eq!(read.pieces.len(), 1);
        assert_eq!(read.sets[0].target, "x");
        assert_eq!(read.sets[0].linkage, Linkage::External);
    }

    /// The two other shapes of `.set`, which are refused with what was written in the message.
    #[test]
    fn a_set_of_anything_but_a_name_is_refused_by_what_it_was_set_to() {
        let cases = [
            (".set n, 42\n", "a '.set' of 'n' to '42'"),
            (".set n, base + 8\n", "a '.set' of 'n' to 'base + 8'"),
            (".set 661, base\n", "a '.set' of '661'"),
            (".set n\n", "a '.set' that is not a name and a thing"),
        ];
        for (template, want) in cases {
            let failed = read(template).expect_err(template);
            assert_eq!(failed, Failed::Unsupported(want.to_owned()), "{template}");
        }
    }

    /// An instruction is the thing this does not do, and the message says which one it was.
    #[test]
    fn a_template_with_an_instruction_in_it_is_refused_by_name() {
        let failed = read(".text\nf:\n movq %rsp, %rbp\n ret\n").expect_err("it is refused");
        assert_eq!(failed, Failed::Unsupported("the instruction 'movq'".to_owned()));
    }

    /// The rest of what is refused, each with a message that names what was written.
    #[test]
    fn what_a_reader_of_directives_does_not_do_is_refused_rather_than_guessed_at() {
        let cases = [
            (".data\nx:\n.byte 1\n.byte 1\nx:\n", "the label 'x' written twice"),
            (".text\nx:\n.byte 1\n", "data in the text section at file scope"),
            (".text\nx:\n", "the label 'x' in the text section"),
            (".bss\nx:\n.byte 1\n", "data in a section that carries none"),
            (".data\nx:\n.quad elsewhere\n", "the name 'elsewhere' in an expression"),
            (".data\nx:\n.cfi_startproc\n", "the '.cfi_startproc' directive at file scope"),
            (".data\n.balign 3\nx:\n.byte 1\n", "an alignment of 3"),
            (".data\n.balign 4\n661:\n.byte 1\n", "an alignment in front of the label '661'"),
            (".data\nx:\n.byte 661f\n", "the local label '661f', which is below it"),
            (".data\nx:\n.byte 661b\n", "the local label '661b', which has no label"),
            (".data\nx:\n.popsection\n", "a '.popsection' with nothing pushed"),
            (
                ".data\nx:\n.byte 1\n.pushsection s\n.quad x - .\n",
                "a distance from here 8 bytes wide",
            ),
            (
                ".data\nx:\n.byte 1\n.pushsection s\ny:\n.long x - y\n",
                "a difference of addresses in two sections in 'x-y'",
            ),
            (
                ".data\nx:\n.byte 1\n.pushsection s\n.long x - (x - .)\n",
                "a distance taken away from in 'x-(x-.)'",
            ),
        ];
        for (template, want) in cases {
            let failed = read(template).expect_err(template);
            assert_eq!(failed, Failed::Unsupported(want.to_owned()), "{template}");
        }
    }

    /// A file that is not there is its own answer, because the program is right and the build
    /// is wrong, and the message a reader needs names the file rather than the directive.
    #[test]
    fn a_file_incbin_names_and_cannot_read_is_reported_as_the_file_it_is() {
        let failed = read(".data\nx:\n.incbin \"missing.bin\"\n").expect_err("it is refused");
        assert_eq!(
            failed,
            Failed::Missing("missing.bin".to_owned(), "no file 'missing.bin'".to_owned())
        );
    }

    /// `.size` is held against what was written under the name rather than believed.
    #[test]
    fn a_size_directive_that_disagrees_with_the_bytes_under_it_is_refused() {
        let pieces = read(".data\nx:\n.byte 1\n.byte 2\n.size x, .-x\n").expect("read");
        assert_eq!(pieces[0].size, 2);

        let failed = read(".data\nx:\n.byte 1\n.size x, 4\n").expect_err("it is refused");
        assert_eq!(
            failed,
            Failed::Unsupported(
                "a '.size' of 'x' that is not what was written under it".to_owned()
            )
        );
    }

    /// An empty template is no globals rather than a refusal, since a program writes one.
    #[test]
    fn a_template_with_nothing_in_it_defines_nothing() {
        for template in ["", "\n\t\n", ".text\n"] {
            assert!(read(template).expect("read").is_empty(), "{template:?}");
        }
    }
}
